//! Motori i vjetër RocksDB (`DiskTree`). A21: vetëm me feature-in `rocksdb` (bench, krahasime,
//! migrim). Release-i i plugin-it ndërtohet pa të: as RocksDB, as libclang/bindgen.

use super::*;
use rocksdb::{
    compaction_filter::Decision, BlockBasedOptions, Cache, ColumnFamilyDescriptor, IteratorMode, Options, WriteBatch,
    WriteOptions, DB,
};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Llogari për kovë gjatë ndërtimit nga snapshot-i.
const BUCKET_TARGET: u64 = 1 << 20;
const CF_NODES: &str = "nodes";
const CF_PAGES: &str = "pages";
const CF_HIST: &str = "nodes_hist";
const CF_STAGING: &str = "staging";
/// Çelësi në `meta` që shënon gjendjen si të pavlefshme (rindërtohet në nisjen e ardhshme).
const INVALID_KEY: &[u8] = b"invalid";
const CF_META: &str = "meta";
const STATE_KEY: &[u8] = b"state";

/// Rezultati i `prepare_slot`: nyjet e reja dhe gjendja, gati për t'u shkruar.
struct DiskPrepared {
    slot: u64,
    out: Vec<NodeW>,
    st: State,
    t_pre: std::time::Duration,
    t_tree: std::time::Duration,
}

impl Source for DiskTree {
    fn read_expected(&self, p: &NPath, rf: Ref) -> Result<Option<(u64, Node)>, String> {
        DiskTree::read_expected(self, p, rf)
    }
}

// ---------------------------------------------------------------- DiskTree

pub struct DiskTree {
    db: DB,
    path: PathBuf,
    history: u64,
    cache_depth: u16,
    /// Nyjet me thellësi < cache_depth, gjithmonë të plota (mungesa = bosh).
    cache: HashMap<PKey, (u64, Node)>,
    /// Vlerat e vjetra të niveleve të sipërme për slotet e fundit.
    ring: VecDeque<CacheDiff>,
    /// Sloti nga i cili cache-ja dhe unaza janë të plota (hapja ose snapshot-i i fundit).
    cache_since: u64,
    state: Option<State>,
    staged: u64,
    /// Hyrjet në arkivë me `skadon_në ≤ cutoff` fshihen gjatë kompaktimit.
    cutoff: Arc<AtomicU64>,
    /// Llogari për kovë gjatë ndërtimit nga snapshot-i.
    pub bucket_target: u64,
    /// Matje: sa nyje u lexuan nga disku në slotin e fundit.
    pub last_disk_reads: AtomicU64,
    durable: bool,
    sync_every: u64,
    commits: u64,
    /// `ZILJA_PROFILE` lexohet një herë, në hapje (Simpler-6).
    profile: bool,
    /// Faqet e lexuara paraprakisht për slotin që po zbatohet.
    /// Faster-1: pas një `RwLock`, që `prepare_slot` të punojë me `&self` (nën kyçin e leximit të
    /// shërbimit, pa bllokuar dëftesat). Faqet aty janë faqe të gjendjes së fundit të shkruar.
    prefetched: std::sync::RwLock<HashMap<PKey, Option<(u64, Page)>>>,
    /// Testet (Safer-6): kur > 0, çdo lexim i n-të i një faqeje "dështon".
    fault_every: AtomicU64,
    fault_count: AtomicU64,
}

fn cf_names() -> [&'static str; 5] {
    [CF_NODES, CF_PAGES, CF_HIST, CF_STAGING, CF_META]
}

/// Opsionet e hapjes.
#[derive(Clone, Debug)]
pub struct StoreOptions {
    pub history: u64,
    /// Nivelet në RAM. Vlen vetëm kur krijohet disku; më pas lexohet nga gjendja.
    pub cache_depth: u16,
    pub block_cache_mb: usize,
    /// `true` = me WAL: çdo slot i shkruar i mbijeton vrasjes së procesit (SIGKILL).
    /// `false` = pa WAL: më shpejt, por pas një rrëzimi mund të humbasin slotet e fundit
    /// (gjendja mbetet e rregullt sepse `atomic_flush` ruan të gjitha CF-të bashkë).
    pub durable: bool,
    /// I-01: WAL-i shkruhet, por sistemi operativ e mban në RAM derisa ta çojë në disk. Pa `sync`,
    /// ikja e rrymës mund të humbasë slotet e fundit (jo konsistencën: zilja vazhdon nga një slot
    /// më i hershëm dhe validatori i ripërsërit). `sync_every = N` bën fsync çdo N slote; 0 = kurrë.
    pub sync_every: u64,
}

impl Default for StoreOptions {
    fn default() -> Self {
        StoreOptions { history: DEFAULT_HISTORY, cache_depth: CACHE_DEPTH, block_cache_mb: 512, durable: true, sync_every: 0 }
    }
}

fn cf_options(name: &str, cache: &Cache, cutoff: Arc<AtomicU64>) -> Options {
    let mut bo = BlockBasedOptions::default();
    bo.set_block_cache(cache);
    bo.set_bloom_filter(10.0, false);
    bo.set_cache_index_and_filter_blocks(true);
    let mut o = Options::default();
    o.set_block_based_table_factory(&bo);
    // LZ4: hash-et nuk ngjeshen, por meta-t dhe versionet po (~40% më pak disk në matje).
    o.set_compression_type(rocksdb::DBCompressionType::Lz4);
    o.set_write_buffer_size(64 << 20);
    if name == CF_HIST {
        o.set_compaction_filter("zilja-expire", move |_lvl, _k, v| {
            let expired = v.len() >= 8 && u64::from_be_bytes(v[..8].try_into().unwrap()) <= cutoff.load(Ordering::Relaxed);
            if expired {
                Decision::Remove
            } else {
                Decision::Keep
            }
        });
    }
    o
}

fn db_options(cache_mb: usize, cutoff: Arc<AtomicU64>) -> (Options, Vec<ColumnFamilyDescriptor>, Cache) {
    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);
    opts.set_atomic_flush(true);
    opts.increase_parallelism(std::thread::available_parallelism().map(|n| n.get() as i32).unwrap_or(2));
    let cache = Cache::new_lru_cache(cache_mb << 20);
    let cfs = cf_names().iter().map(|name| ColumnFamilyDescriptor::new(*name, cf_options(name, &cache, cutoff.clone()))).collect();
    (opts, cfs, cache)
}

impl DiskTree {
    pub fn open(path: impl AsRef<Path>, history: u64) -> Result<Self, String> {
        Self::open_opts(path, StoreOptions { history, ..Default::default() })
    }

    pub fn open_opts(path: impl AsRef<Path>, o: StoreOptions) -> Result<Self, String> {
        let path = path.as_ref().to_path_buf();
        let cutoff = Arc::new(AtomicU64::new(0));
        let (opts, cfs, cache) = db_options(o.block_cache_mb, cutoff.clone());
        let mut db = DB::open_cf_descriptors(&opts, &path, cfs).map_err(|e| e.to_string())?;
        let meta = db.cf_handle(CF_META).ok_or("mungon CF meta")?;
        if let Some(why) = db.get_cf(meta, INVALID_KEY).map_err(|e| e.to_string())? {
            // F-13 + Simpler-7: gjendja u shënua e pavlefshme në nisjen e kaluar: rindërtim i pastër.
            log::warn!("zilja-store: pema u shënua e pavlefshme ({}); po rindërtohet nga e para", String::from_utf8_lossy(&why));
            for name in cf_names() {
                db.drop_cf(name).map_err(|e| e.to_string())?;
                db.create_cf(name, &cf_options(name, &cache, cutoff.clone())).map_err(|e| e.to_string())?;
            }
        }
        let raw = db.get_cf(db.cf_handle(CF_META).ok_or("mungon CF meta")?, STATE_KEY).map_err(|e| e.to_string())?;
        let state = match raw {
            None => None,
            Some(b) => match State::dec(&b) {
                Some(st) => Some(st),
                // F-13: kurrë mos fshi diçka që s'e kuptojmë.
                None => {
                    return Err(format!(
                        "gjendja në {} ka version të panjohur ({}): kjo zile s'e njeh; fshije dosjen me dorë për ta rindërtuar",
                        path.display(),
                        b.first().copied().unwrap_or(0)
                    ))
                }
            },
        };
        let mut t = DiskTree {
            db,
            path,
            history: o.history,
            cache_depth: state.as_ref().map_or(o.cache_depth, |s| s.cache_depth),
            cache: HashMap::default(),
            ring: VecDeque::new(),
            cache_since: 0,
            state,
            staged: 0,
            cutoff,
            bucket_target: BUCKET_TARGET,
            last_disk_reads: AtomicU64::new(0),
            durable: o.durable,
            sync_every: o.sync_every,
            commits: 0,
            profile: std::env::var_os("ZILJA_PROFILE").is_some(),
            prefetched: std::sync::RwLock::new(HashMap::default()),
            fault_every: AtomicU64::new(0),
            fault_count: AtomicU64::new(0),
        };
        match &t.state {
            // Ndërtim i mbetur në mes, ose disk i ri: nis nga e para.
            None => t.clear()?,
            Some(st) => {
                t.cutoff.store(st.version.saturating_sub(t.history), Ordering::Relaxed);
                if let Err(e) = t.rebuild_cache() {
                    log::error!("zilja-store: disku nuk përputhet me gjendjen ({e}); do të rindërtohet");
                    t.clear()?;
                }
            }
        }
        Ok(t)
    }

    /// Vetëm për teste: çdo lexim i `n`-të i një faqeje nga disku kthehet si i palexueshëm (0 = fikur).
    #[doc(hidden)]
    pub fn inject_read_faults(&self, n: u64) {
        self.fault_every.store(n, Ordering::Relaxed);
        self.fault_count.store(0, Ordering::Relaxed);
    }

    fn fault(&self) -> bool {
        let n = self.fault_every.load(Ordering::Relaxed);
        n > 0 && (self.fault_count.fetch_add(1, Ordering::Relaxed) + 1) % n == 0
    }

    /// F-04: nyja në `p` që prindi e shënon si `rf`. Kur prindi thotë "jo bosh", nyja duhet të
    /// lexohet dhe hash-i i saj duhet të jetë ai i prindit; përndryshe gabim, kurrë "bosh".
    fn read_expected(&self, p: &NPath, rf: Ref) -> Result<Option<(u64, Node)>, String> {
        if rf.kind == Kind::Empty {
            return Ok(None);
        }
        match self.read_latest(p) {
            None => Err(format!("nyja në thellësinë {} mungon ose s'lexohet nga disku", p.depth)),
            Some((v, n)) => {
                if n.to_ref() != rf {
                    return Err(format!("nyja në thellësinë {} nuk përputhet me hash-in e prindit", p.depth));
                }
                Ok(Some((v, n)))
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn version(&self) -> Option<u64> {
        self.state.as_ref().map(|s| s.version)
    }

    pub fn cache_depth(&self) -> u16 {
        self.cache_depth
    }

    /// Numri i nyjeve në cache-në e RAM-it.
    pub fn cached_nodes(&self) -> usize {
        self.cache.len()
    }

    fn cf(&self, name: &str) -> &rocksdb::ColumnFamily {
        self.db.cf_handle(name).unwrap()
    }

    fn write(&self, b: WriteBatch) -> Result<(), String> {
        let mut wo = WriteOptions::default();
        wo.disable_wal(!self.durable);
        self.db.write_opt(b, &wo).map_err(|e| e.to_string())
    }

    /// Fshin gjithçka (p.sh. para një ndërtimi të ri nga snapshot-i).
    pub fn clear(&mut self) -> Result<(), String> {
        for name in cf_names() {
            let mut b = WriteBatch::default();
            b.delete_range_cf(self.cf(name), [].as_slice(), [0xffu8; 64].as_slice());
            self.write(b)?;
            self.db.compact_range_cf(self.cf(name), None::<&[u8]>, None::<&[u8]>);
        }
        self.cache.clear();
        self.ring.clear();
        self.state = None;
        self.staged = 0;
        Ok(())
    }

    /// Rillogarit nivelet e sipërme nga gjethet e cekëta dhe shtresa `cache_depth` në disk,
    /// dhe kontrollon që root-i del i njëjtë me atë të ruajtur.
    fn rebuild_cache(&mut self) -> Result<(), String> {
        let st = self.state.clone().ok_or("pa gjendje")?;
        let cd = self.cache_depth;
        let mut layer: HashMap<PKey, Node> = HashMap::default();
        // Gjethet e cekëta.
        for item in self.db.iterator_cf(self.cf(CF_NODES), IteratorMode::Start) {
            let (k, v) = item.map_err(|e| e.to_string())?;
            let p = NPath::dec(&k).ok_or("çelës i dëmtuar")?;
            let (_, body) = split_u64(&v).ok_or("vlerë e dëmtuar")?;
            let n = Node::dec(body).ok_or("nyje e dëmtuar")?;
            if p.depth < cd && n.is_leaf() {
                layer.insert(p.enc(), n);
            }
        }
        // Rrënjët e faqeve në thellësinë `cd` (renditen të parat, sepse çelësi nis me thellësinë).
        for item in self.db.iterator_cf(self.cf(CF_PAGES), IteratorMode::Start) {
            let (k, v) = item.map_err(|e| e.to_string())?;
            let p = NPath::dec(&k).ok_or("çelës i dëmtuar")?;
            if p.depth != cd {
                break;
            }
            let (_, pg) = dec_page_val(&v).ok_or("faqe e dëmtuar")?;
            if let Some(n) = &pg.slots[0] {
                layer.insert(p.enc(), n.clone());
            }
        }
        self.cache.clear();
        let mut computed = Vec::new();
        let root = compute_top(NPath::root(), cd, &layer, &mut computed);
        if ref_of(&root) != st.root {
            return Err("root-i i rillogaritur ndryshon nga ai i ruajtur".into());
        }
        for (k, n) in computed {
            self.cache.insert(k, (st.version, n));
        }
        self.ring.clear();
        self.cache_since = st.version;
        Ok(())
    }

    // ------------------------------------------------------------ leximi

    fn read_latest(&self, p: &NPath) -> Option<(u64, Node)> {
        if p.depth < self.cache_depth {
            return self.cache.get(&p.enc()).cloned();
        }
        let (root, idx) = page_pos(p, self.page_depth(p.depth));
        if let Some(hit) = self.prefetched.read().unwrap().get(&root.enc()) {
            return hit.as_ref().and_then(|(ver, pg)| pg.slots[idx].clone().map(|n| (*ver, n)));
        }
        let (ver, pg) = self.page_latest(&root)?;
        pg.slots[idx].clone().map(|n| (ver, n))
    }

    /// Thellësia e rrënjës së faqes që përmban një nyje në thellësinë `depth` (≥ cache_depth).
    fn page_depth(&self, depth: u16) -> u16 {
        let cd = self.cache_depth;
        cd + ((depth - cd) / PAGE_H) * PAGE_H
    }

    /// Faqja e tanishme me rrënjë `root`.
    fn page_latest(&self, root: &NPath) -> Option<(u64, Page)> {
        if let Some(hit) = self.prefetched.read().unwrap().get(&root.enc()) {
            return hit.clone();
        }
        self.last_disk_reads.fetch_add(1, Ordering::Relaxed);
        if self.fault() {
            return None;
        }
        let v = self.db.get_pinned_cf(self.cf(CF_PAGES), root.enc()).ok()??;
        dec_page_val(&v)
    }

    /// Faqja me rrënjë `root` siç ishte në versionin `v`.
    fn page_at(&self, root: &NPath, v: u64) -> Result<Option<Page>, String> {
        if let Some((ver, pg)) = self.page_latest(root) {
            if ver <= v {
                return Ok(Some(pg));
            }
        }
        let mut it = self.db.raw_iterator_cf(self.cf(CF_HIST));
        it.seek_for_prev(root.enc_ver(v));
        if it.valid() {
            if let (Some(k), Some(val)) = (it.key(), it.value()) {
                if k.starts_with(&root.enc()) {
                    let (drop_at, body) = split_u64(val).ok_or("arkivë e dëmtuar")?;
                    // E vlefshme vetëm nëse ishte ende aty në v.
                    if drop_at > v {
                        return Ok(Some(Page::dec(body).ok_or("arkivë e dëmtuar")?));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Lexon paraprakisht, rresht pas rreshti faqesh dhe me një kërkesë të grupuar për
    /// rresht, faqet që do t'i duhen zbatimit të `ups` (të renditura).
    fn prefetch(&self, ups: &[(Key, Option<AccountMeta>)]) {
        self.prefetched.write().unwrap().clear();
        let cd = self.cache_depth;
        // Rrugët në nivelin `cd` që kanë ndonjë ndryshim, vetëm ku prindi në cache tregon një fëmijë jo-bosh.
        let mut frontier: Vec<NPath> = Vec::new();
        for (k, _) in ups {
            let mut p = NPath::root();
            let mut alive = true;
            while p.depth < cd {
                match self.cache.get(&p.enc()) {
                    Some((_, Node::Internal { l, r })) => {
                        let b = bit(k, p.depth as usize);
                        if (if b == 0 { l } else { r }).kind == Kind::Empty {
                            alive = false;
                            break;
                        }
                        p = p.child(b);
                    }
                    _ => {
                        alive = false;
                        break;
                    }
                }
            }
            if alive && frontier.last() != Some(&p) {
                frontier.push(p);
            }
        }
        if cd == 0 {
            frontier = vec![NPath::root()];
        }
        while !frontier.is_empty() {
            let keys: Vec<PKey> = frontier.iter().map(|p| p.enc()).collect();
            self.last_disk_reads.fetch_add(keys.len() as u64, Ordering::Relaxed);
            // Copa paralele: RocksDB lejon lexime nga disa thread-e njëherësh.
            let db = &self.db;
            let res: Vec<Option<(u64, Page)>> = {
                use rayon::prelude::*;
                keys.par_chunks(256)
                    .flat_map_iter(|chunk| {
                        let cf = db.cf_handle(CF_PAGES).unwrap();
                        db.batched_multi_get_cf(cf, chunk.iter(), true)
                            .into_iter()
                            .map(|r| if self.fault() { None } else { r.ok().flatten().and_then(|v| dec_page_val(&v)) })
                            .collect::<Vec<_>>()
                    })
                    .collect()
            };
            let mut next: Vec<NPath> = Vec::new();
            for ((root, k), val) in frontier.iter().zip(keys.iter()).zip(res) {
                if let Some((_, pg)) = &val {
                    // Për çdo çelës në këtë faqe: zbrit brenda faqes; nëse rruga del poshtë saj,
                    // faqja fëmijë duhet në rreshtin e ardhshëm.
                    let lo = ups.partition_point(|(key, _)| key < &root.bits);
                    let mut hi = lo;
                    while hi < ups.len() && root.contains(&ups[hi].0) {
                        hi += 1;
                    }
                    for (key, _) in &ups[lo..hi] {
                        let (mut idx, mut d) = (0usize, root.depth);
                        while let Some(Node::Internal { l, r }) = &pg.slots[idx] {
                            let b = bit(key, d as usize);
                            if (if b == 0 { l } else { r }).kind == Kind::Empty {
                                break;
                            }
                            if d + 1 == root.depth + PAGE_H {
                                let cp = NPath { depth: d + 1, bits: *key }.truncate(d + 1);
                                if next.last() != Some(&cp) {
                                    next.push(cp);
                                }
                                break;
                            }
                            idx = 2 * idx + 1 + b as usize;
                            d += 1;
                        }
                    }
                }
                self.prefetched.write().unwrap().insert(*k, val);
            }
            frontier = next;
        }
    }

    /// Nyja në `p` siç ishte në versionin `v`.
    fn read_at(&self, p: &NPath, v: u64) -> Result<Option<Node>, String> {
        if p.depth < self.cache_depth {
            let latest = self.read_latest(p);
            if let Some((ver, n)) = &latest {
                if *ver <= v {
                    return Ok(Some(n.clone()));
                }
            }
            // Vlera para ndryshimit të parë pas v.
            for (s, diff) in &self.ring {
                if *s > v {
                    if let Some(old) = diff.get(&p.enc()) {
                        return Ok(old.as_ref().map(|(_, n)| n.clone()));
                    }
                }
            }
            return match latest {
                None => Ok(None),
                Some(_) => Err(format!("niveli i sipërm i slotit {v} nuk është më në kujtesë")),
            };
        }
        let (root, idx) = page_pos(p, self.page_depth(p.depth));
        Ok(self.page_at(&root, v)?.and_then(|pg| pg.slots[idx].clone()))
    }

    /// Kalon ndryshimet në `batch` (disk) dhe në cache/unazë (RAM).
    fn write_nodes(&mut self, batch: &mut WriteBatch, v: u64, ws: Vec<NodeW>) {
        let hist = self.history > 0;
        let cd = self.cache_depth;
        let mut diff: HashMap<PKey, Option<(u64, Node)>> = HashMap::default();
        let nodes = self.db.cf_handle(CF_NODES).unwrap();
        let hcf = self.db.cf_handle(CF_HIST).unwrap();
        let pcf = self.db.cf_handle(CF_PAGES).unwrap();
        let mut pages: HashMap<PKey, (NPath, Vec<(usize, Option<Node>)>)> = HashMap::default();
        for w in ws {
            let key = w.path.enc();
            if w.path.depth < cd {
                // RAM; në disk shkon vetëm nëse është gjethe (që shtresa e poshtme të jetë e plotë).
                let was_leaf = w.old.as_ref().is_some_and(|(_, n)| n.is_leaf());
                match &w.new {
                    Some(n) if n.is_leaf() => batch.put_cf(nodes, key, with_u64(v, &n.enc())),
                    _ if was_leaf => batch.delete_cf(nodes, key),
                    _ => {}
                }
                let w = w;
                let NodeW { new, old, .. } = w;
                if hist {
                    diff.insert(key, old);
                }
                match new {
                    Some(n) => {
                        self.cache.insert(key, (v, n));
                    }
                    None => {
                        self.cache.remove(&key);
                    }
                }
            } else {
                let pd = cd + ((w.path.depth - cd) / PAGE_H) * PAGE_H;
                let (root, idx) = page_pos(&w.path, pd);
                pages.entry(root.enc()).or_insert_with(|| (root, Vec::new())).1.push((idx, w.new));
            }
        }
        // Një shkrim për çdo faqe që ndryshoi; faqja e vjetër kalon në arkivë.
        for (k, (root, changes)) in pages {
            // Faqja e lexuar paraprakisht nuk duhet më: merre pa e kopjuar.
            let pre = self.prefetched.get_mut().unwrap().remove(&k);
            let old = match pre {
                Some(o) => o,
                None => self.page_latest(&root),
            };
            let mut np = old.as_ref().map_or_else(Page::new, |o| o.1.clone());
            for (idx, n) in changes {
                np.slots[idx] = n;
            }
            if old.as_ref().map(|o| &o.1) == Some(&np) {
                continue;
            }
            if np.is_empty() {
                batch.delete_cf(pcf, k);
            } else {
                batch.put_cf(pcf, k, np.enc_with_ver(Some(v)));
            }
            if hist {
                if let Some((ov, op)) = &old {
                    batch.put_cf(hcf, root.enc_ver(*ov), op.enc_with_ver(Some(v)));
                }
            }
        }
        if hist {
            self.ring.push_back((v, diff));
        }
    }

    fn trim_history(&mut self, version: u64) {
        let lo = version.saturating_sub(self.history);
        self.cutoff.store(lo, Ordering::Relaxed);
        // Unaza: mbaj vetëm slotet > lo (ato duhen për pyetjet në v ≥ lo).
        while self.ring.front().is_some_and(|(s, _)| *s <= lo) {
            self.ring.pop_front();
        }
    }

    fn commit_state(&mut self, batch: &mut WriteBatch, st: State) -> Result<(), String> {
        batch.put_cf(self.cf(CF_META), STATE_KEY, st.enc());
        self.commits += 1;
        if self.sync_every > 0 && self.durable && self.commits % self.sync_every == 0 {
            let mut wo = WriteOptions::default();
            wo.set_sync(true);
            self.db.write_opt(std::mem::take(batch), &wo).map_err(|e| e.to_string())?;
        } else {
            self.write(std::mem::take(batch))?;
        }
        self.state = Some(st);
        Ok(())
    }

    /// Kontroll i plotë: root-i nga gjethet në disk = root-i i ruajtur; numri i llogarive përputhet.
    /// I ngadaltë; për teste dhe pas rrëzimeve.
    pub fn verify_integrity(&self) -> Result<(), String> {
        let mut accs = Vec::new();
        for item in self.db.iterator_cf(self.cf(CF_NODES), IteratorMode::Start) {
            let (_, v) = item.map_err(|e| e.to_string())?;
            let (_, body) = split_u64(&v).ok_or("vlerë e dëmtuar")?;
            if let Some(Node::Leaf(d)) = Node::dec(body) {
                accs.push((d.key, d.meta));
            }
        }
        for item in self.db.iterator_cf(self.cf(CF_PAGES), IteratorMode::Start) {
            let (_, v) = item.map_err(|e| e.to_string())?;
            let (_, pg) = dec_page_val(&v).ok_or("faqe e dëmtuar")?;
            for n in pg.slots.into_iter().flatten() {
                if let Node::Leaf(d) = n {
                    accs.push((d.key, d.meta));
                }
            }
        }
        let expect = zilja_tree::reference_root(&accs);
        if expect != self.root() {
            return Err(format!("root-i i ruajtur {} != root-i nga gjethet {}", hx(&self.root()), hx(&expect)));
        }
        if accs.len() as u64 != self.state.as_ref().map_or(0, |s| s.len) {
            return Err(format!("numri i llogarive {} != {}", accs.len(), self.len()));
        }
        Ok(())
    }

    /// Numri i hyrjeve në arkivë (për teste dhe metrika).
    pub fn debug_hist_entries(&self) -> usize {
        self.db.iterator_cf(self.cf(CF_HIST), IteratorMode::Start).count()
    }

    /// Detyron kompaktimin e arkivës (fshin hyrjet që kanë skaduar).
    pub fn compact_history(&self) {
        self.db.compact_range_cf(self.cf(CF_HIST), None::<&[u8]>, None::<&[u8]>);
    }

    // ------------------------------------------------------------ snapshot

    /// Shkruan nyjet përfundimtare të ndërtimit nga snapshot-i: nyjet poshtë cache-së në
    /// faqe (të bashkuara me atë që është tashmë në disk), gjethet e cekëta më vete.
    fn persist_snapshot(&self, items: Vec<(NPath, Node)>, slot: u64) -> Result<(), String> {
        let cd = self.cache_depth;
        let mut batch = WriteBatch::default();
        let mut pages: HashMap<PKey, (NPath, Vec<(usize, Node)>)> = HashMap::default();
        for (p, n) in items {
            if p.depth >= cd {
                let (root, idx) = page_pos(&p, self.page_depth(p.depth));
                pages.entry(root.enc()).or_insert_with(|| (root, Vec::new())).1.push((idx, n));
            } else if n.is_leaf() {
                batch.put_cf(self.cf(CF_NODES), p.enc(), with_u64(slot, &n.enc()));
            }
        }
        for (k, (_, items)) in pages {
            let mut pg = self
                .db
                .get_pinned_cf(self.cf(CF_PAGES), k)
                .map_err(|e| e.to_string())?
                .and_then(|v| dec_page_val(&v))
                .map_or_else(Page::new, |o| o.1);
            for (idx, n) in items {
                pg.slots[idx] = Some(n);
            }
            batch.put_cf(self.cf(CF_PAGES), k, with_u64(slot, &pg.enc()));
        }
        self.write(batch)
    }

    fn build_bucketed(&self, p: NPath, bucket_depth: u16, slot: u64) -> Result<Option<Node>, String> {
        if p.depth == bucket_depth {
            let mut ups = Vec::new();
            let mut it = self.db.raw_iterator_cf(self.cf(CF_STAGING));
            it.seek(p.bits);
            while it.valid() {
                let k: Key = it.key().unwrap().try_into().map_err(|_| "çelës i dëmtuar")?;
                if !p.contains(&k) {
                    break;
                }
                if let (_, Some(m)) = dec_staged(it.value().unwrap()).ok_or("meta e dëmtuar")? {
                    ups.push((k, Some(m)));
                }
                it.next();
            }
            let mut out = Vec::new();
            let res = self.build(p, &ups, &mut out);
            self.persist_snapshot(out.into_iter().filter_map(|w| w.new.map(|n| (w.path, n))).collect(), slot)?;
            return Ok(res);
        }
        let c0 = self.build_bucketed(p.child(0), bucket_depth, slot)?;
        let c1 = self.build_bucketed(p.child(1), bucket_depth, slot)?;
        let res = match (&c0, &c1) {
            (None, None) => None,
            (Some(n @ Node::Leaf(_)), None) | (None, Some(n @ Node::Leaf(_))) => Some(n.clone()),
            _ => {
                let kids = [(0u8, &c0), (1u8, &c1)]
                    .into_iter()
                    .filter_map(|(b, c)| c.clone().map(|n| (p.child(b), n)))
                    .collect();
                self.persist_snapshot(kids, slot)?;
                Some(Node::Internal { l: ref_of(&c0), r: ref_of(&c1) })
            }
        };
        Ok(res)
    }
}

/// Ndërton nivelet e sipërme nga shtresa: `layer` ka gjethet e cekëta dhe nyjet në thellësinë `cd`.
fn compute_top(p: NPath, cd: u16, layer: &HashMap<PKey, Node>, out: &mut Vec<(PKey, Node)>) -> Option<Node> {
    if let Some(n) = layer.get(&p.enc()) {
        if p.depth < cd {
            out.push((p.enc(), n.clone()));
        }
        return Some(n.clone());
    }
    if p.depth >= cd {
        return None;
    }
    let c0 = compute_top(p.child(0), cd, layer, out);
    let c1 = compute_top(p.child(1), cd, layer, out);
    if c0.is_none() && c1.is_none() {
        return None;
    }
    let n = Node::Internal { l: ref_of(&c0), r: ref_of(&c1) };
    out.push((p.enc(), n.clone()));
    Some(n)
}


impl StateTree for DiskTree {
    fn root(&self) -> Hash {
        self.state.as_ref().map_or(EMPTY, |s| s.root.hash)
    }

    fn len(&self) -> usize {
        self.state.as_ref().map_or(0, |s| s.len as usize)
    }

    fn stage_snapshot(&mut self, slot: u64, ups: Vec<Update>) -> Result<(), String> {
        if self.staged == 0 && self.state.is_some() {
            // Ndërtim i ri: hidh gjendjen e vjetër.
            self.clear()?;
        }
        // F-02: validatori dërgon çdo version të ruajtur, në radhë të çfarëdoshme. Fiton sloti më i ri;
        // llogaritë e mbyllura ruhen si varre deri në `finish_snapshot`, që një kopje e vjetër të mos ringjallet.
        let mut last: HashMap<Key, Option<AccountMeta>> = HashMap::with_capacity(ups.len());
        for (k, m) in ups {
            last.insert(k, m);
        }
        let keys: Vec<Key> = last.keys().copied().collect();
        let cf = self.cf(CF_STAGING);
        let existing = self.db.batched_multi_get_cf(cf, keys.iter(), false);
        let mut batch = WriteBatch::default();
        let mut n = 0;
        for (k, old) in keys.iter().zip(existing) {
            let old_slot = match old {
                Ok(Some(v)) => dec_staged(&v).map(|(s, _)| s),
                _ => None,
            };
            if old_slot.is_some_and(|s| s > slot) {
                continue;
            }
            batch.put_cf(cf, k, enc_staged(slot, &last[k]));
            n += 1;
        }
        self.staged += n;
        self.write(batch).map_err(|e| format!("shkrimi i snapshot-it dështoi: {e}"))
    }

    fn finish_snapshot(&mut self, slot: u64, complete: bool) -> Result<Hash, String> {
        if self.staged == 0 && self.state.is_some() {
            self.clear()?;
        }
        let mut len = 0u64;
        for item in self.db.iterator_cf(self.cf(CF_STAGING), IteratorMode::Start) {
            let (_, v) = item.map_err(|e| format!("leximi i staging-ut dështoi: {e}"))?;
            if v.get(8) == Some(&1) {
                len += 1;
            }
        }
        let mut bucket_depth = 0u16;
        // Kovat nuk shkojnë më thellë se cache-ja, që çdo faqe të jetë e tëra brenda një kove.
        while (len >> bucket_depth) > self.bucket_target && bucket_depth < 16.min(self.cache_depth) {
            bucket_depth += 1;
        }
        let root = self.build_bucketed(NPath::root(), bucket_depth, slot).map_err(|e| format!("ndërtimi nga snapshot-i dështoi: {e}"))?;
        if let Some(n) = &root {
            self.persist_snapshot(vec![(NPath::root(), n.clone())], slot)?;
        }
        let mut batch = WriteBatch::default();
        batch.delete_range_cf(self.cf(CF_STAGING), [].as_slice(), [0xffu8; 64].as_slice());
        let st = State {
            version: slot,
            root: ref_of(&root),
            len,
            complete,
            base: slot,
            cache_depth: self.cache_depth,
            fingerprint: None,
        };
        self.commit_state(&mut batch, st)?;
        self.db.compact_range_cf(self.cf(CF_STAGING), None::<&[u8]>, None::<&[u8]>);
        self.staged = 0;
        self.rebuild_cache()?;
        self.trim_history(slot);
        Ok(self.root())
    }

    fn apply_slot(&mut self, slot: u64, ups: Vec<Update>, fingerprint: Option<Fingerprint>) -> Result<Hash, String> {
        let p = self.prepare_slot(slot, ups, fingerprint)?;
        self.commit_slot(slot, p)
    }

    /// Faster-1: leximet dhe llogaritja e nyjeve të reja, pa ndryshuar asgjë (`&self`).
    fn prepare_slot(&self, slot: u64, mut ups: Vec<Update>, fingerprint: Option<Fingerprint>) -> Result<zilja_tree::Prepared, String> {
        let t0 = std::time::Instant::now();
        normalize_updates(&mut ups);
        self.last_disk_reads.store(0, Ordering::Relaxed);
        let prev = self.state.clone().unwrap_or(State {
            version: 0,
            root: EMPTY_REF,
            len: 0,
            complete: false,
            base: slot,
            cache_depth: self.cache_depth,
            fingerprint: None,
        });
        self.prefetch(&ups);
        let t_pre = t0.elapsed();
        let mut out = Vec::new();
        let root_path = NPath::root();
        let res = self.read_expected(&root_path, prev.root).and_then(|old_root| {
            let (new_root, delta) = if ups.is_empty() {
                (old_root.clone().map(|o| o.1), 0)
            } else {
                self.apply(root_path, old_root.clone(), &ups, &mut out).map_err(|e| format!("sloti {slot}: {e}"))?
            };
            emit(&mut out, root_path, new_root.clone(), old_root);
            Ok((new_root, delta))
        });
        let (new_root, delta) = match res {
            Ok(r) => r,
            Err(e) => {
                // Asgjë nuk u shkrua: gjendja në disk mbetet ajo e slotit të mëparshëm.
                self.prefetched.write().unwrap().clear();
                return Err(e);
            }
        };
        let st = State {
            version: slot,
            root: ref_of(&new_root),
            len: (prev.len as i64 + delta).max(0) as u64,
            complete: prev.complete,
            base: prev.base,
            cache_depth: self.cache_depth,
            fingerprint,
        };
        Ok(Box::new(DiskPrepared { slot, out, st, t_pre, t_tree: t0.elapsed() }))
    }

    /// Shkrimi: nyjet në një `WriteBatch` atomik, cache-ja, gjendja. I shkurtër.
    fn commit_slot(&mut self, slot: u64, p: zilja_tree::Prepared) -> Result<Hash, String> {
        let p = *p.downcast::<DiskPrepared>().map_err(|_| "commit_slot: përgatitje e një peme tjetër")?;
        if p.slot != slot {
            return Err(format!("commit_slot: përgatitja ishte për slotin {}", p.slot));
        }
        let t0 = std::time::Instant::now();
        if self.state.is_none() {
            self.cache_since = slot;
        }
        let mut batch = WriteBatch::default();
        let nw = p.out.len();
        self.write_nodes(&mut batch, slot, p.out);
        self.prefetched.get_mut().unwrap().clear();
        let t_batch = t0.elapsed();
        let ops = batch.len();
        self.commit_state(&mut batch, p.st).map_err(|e| format!("shkrimi i slotit {slot} dështoi: {e}"))?;
        self.trim_history(slot);
        if self.profile {
            eprintln!(
                "slot {slot}: leximi {:?}, pema {:?}, batch {:?}, shkrimi {:?} | {nw} nyje, {ops} ops, {} lexime",
                p.t_pre,
                p.t_tree - p.t_pre,
                t_batch,
                t0.elapsed() - t_batch,
                self.last_disk_reads.load(Ordering::Relaxed)
            );
        }
        Ok(self.root())
    }

    fn prove_at(&self, key: &Key, at: Option<u64>) -> Result<ProofAt, zilja_tree::ProveError> {
        // P-09: gjendje që mungon / jashtë historisë dallohen nga gabimet e ruajtjes.
        zilja_tree::check_provable(self.provable_range(), at)?;
        let inner = || -> Result<ProofAt, String> {
        let st = self.state.as_ref().ok_or("pema ende s'është ndërtuar")?;
        let v = at.unwrap_or(st.version);
        let (lo, hi) = self.provable_range().unwrap();
        if v < lo || v > hi {
            return Err(format!("sloti {v} është jashtë kujtesës ({lo}..={hi})"));
        }
        let mut siblings = Vec::new();
        let mut p = NPath::root();
        let mut node = self.read_at(&p, v)?;
        let root = ref_of(&node).hash;
        loop {
            match node {
                None => return Ok((v, root, Proof { siblings, leaf: None }, None)),
                Some(Node::Leaf(d)) => {
                    let vh = d.vh();
                    let m = (d.key == *key).then_some(d.meta);
                    return Ok((v, root, Proof { siblings, leaf: Some((d.key, vh)) }, m));
                }
                Some(Node::Internal { l, r }) => {
                    let b = bit(key, p.depth as usize);
                    let (next, sib) = if b == 0 { (l, r) } else { (r, l) };
                    siblings.push(sib.hash);
                    p = p.child(b);
                    node = if next.kind == Kind::Empty {
                        None
                    } else {
                        // F-04: një fëmijë që prindi e shënon jo-bosh duhet të jetë aty dhe të përputhet.
                        let n = self.read_at(&p, v)?.ok_or("nyje që mungon në disk")?;
                        if n.to_ref() != next {
                            return Err("nyje në disk që nuk përputhet me hash-in e prindit".into());
                        }
                        Some(n)
                    };
                }
            }
        }
    };
        inner().map_err(zilja_tree::ProveError::Storage)
    }

    fn provable_range(&self) -> Option<(u64, u64)> {
        self.state.as_ref().map(|s| {
            let lo = if self.history == 0 { s.version } else { s.version.saturating_sub(self.history) };
            (lo.max(s.base).max(self.cache_since), s.version)
        })
    }

    fn resume_point(&self) -> Option<Resume> {
        self.state.as_ref().map(|s| Resume {
            slot: s.version,
            root: s.root.hash,
            len: s.len as usize,
            complete: s.complete,
            fingerprint: s.fingerprint,
        })
    }

    fn invalidate(&mut self, why: &str) -> Result<(), String> {
        // F-13: asgjë nuk fshihet tani. Shënimi në `meta` bën që nisja e ardhshme ta rindërtojë.
        let mut b = WriteBatch::default();
        b.put_cf(self.cf(CF_META), INVALID_KEY, why.as_bytes());
        self.write(b).map_err(|e| format!("shënimi 'i pavlefshëm': {e}"))?;
        self.db.flush_cf(self.cf(CF_META)).map_err(|e| format!("shënimi 'i pavlefshëm': {e}"))
    }
}

