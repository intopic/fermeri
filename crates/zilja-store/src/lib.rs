//! # zilja-store: pema e Ziles në disk
//!
//! **Tavolina dhe rafti.** Nivelet e sipërme të pemës (`cache_depth`) jetojnë vetëm
//! në RAM. Në RocksDB shkruhen **gjethet** (me meta-n e llogarisë brenda) dhe
//! **nyjet e brendshme poshtë cache-së**. Kur hapet disku, nivelet e sipërme
//! rillogariten nga shtresa e poshtme dhe krahasohen me root-in e ruajtur.
//!
//! **Shkrim atomik.** Gjithë ndryshimet e një sloti dhe gjendja e re shkruhen me
//! një `WriteBatch`. Pas një ndërprerjeje mbetet gjendja e plotë e slotit të
//! mëparshëm, kurrë gjysmë-gjendje.
//!
//! **Kujtesa e sloteve.** Kur një nyje e poshtme zëvendësohet, versioni i vjetër
//! kalon në arkivë (`nodes_hist`, çelësi `rruga ‖ versioni`, vlera
//! `skadon_në ‖ nyja`). Kompaktimi i RocksDB fshin vetë hyrjet që kanë skaduar.
//! Për nivelet e sipërme, vlerat e vjetra mbahen në RAM për `history` slotet e fundit.
//!
//! | CF | çelësi | vlera |
//! |---|---|---|
//! | `nodes` | rruga (34) | versioni ‖ nyja |
//! | `nodes_hist` | rruga ‖ versioni | skadon_në ‖ nyja |
//! | `staging` | pubkey | meta (vetëm gjatë ndërtimit nga snapshot-i) |
//! | `meta` | `state` | gjendja e fundit e shkruar |

use rocksdb::{
    compaction_filter::Decision, BlockBasedOptions, Cache, ColumnFamilyDescriptor, IteratorMode, Options, WriteBatch,
    WriteOptions, DB,
};
use std::collections::VecDeque;
/// Tabelë hash e shpejtë dhe e mbrojtur nga sulmet me përplasje (çelësa të rastësishëm).
type HashMap<K, V> = ahash::AHashMap<K, V>;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use zilja_tree::{normalize_updates, ProofAt, Resume, StateTree, Update};
use zilja_verifier::{bit, internal_hash, leaf_hash, AccountMeta, Hash, Key, Proof, EMPTY};

/// Nivelet e pemës që mbahen në RAM (deri në ~2^20 nyje).
pub const CACHE_DEPTH: u16 = 20;
/// Kujtesa e paracaktuar: sa slote pas mund të jepen dëftesa.
pub const DEFAULT_HISTORY: u64 = 32;
const PAR_THRESHOLD: usize = 512;
/// Llogari për kovë gjatë ndërtimit nga snapshot-i.
const BUCKET_TARGET: u64 = 1 << 20;

const CF_NODES: &str = "nodes";
const CF_HIST: &str = "nodes_hist";
const CF_STAGING: &str = "staging";
const CF_META: &str = "meta";
const STATE_KEY: &[u8] = b"state";

// ---------------------------------------------------------------- rruga

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct NPath {
    depth: u16,
    bits: Key,
}

type PKey = [u8; 34];

impl NPath {
    fn root() -> Self {
        NPath { depth: 0, bits: [0; 32] }
    }
    fn child(&self, b: u8) -> Self {
        let mut bits = self.bits;
        if b == 1 {
            let d = self.depth as usize;
            bits[d / 8] |= 1 << (7 - d % 8);
        }
        NPath { depth: self.depth + 1, bits }
    }
    fn enc(&self) -> PKey {
        let mut k = [0u8; 34];
        k[..2].copy_from_slice(&self.depth.to_be_bytes());
        k[2..].copy_from_slice(&self.bits);
        k
    }
    fn dec(k: &[u8]) -> Option<Self> {
        (k.len() == 34).then(|| NPath { depth: u16::from_be_bytes([k[0], k[1]]), bits: k[2..34].try_into().unwrap() })
    }
    fn enc_ver(&self, v: u64) -> [u8; 42] {
        let mut k = [0u8; 42];
        k[..34].copy_from_slice(&self.enc());
        k[34..].copy_from_slice(&v.to_be_bytes());
        k
    }
    /// A ndodhet çelësi brenda kësaj rruge?
    fn contains(&self, key: &Key) -> bool {
        let d = self.depth as usize;
        let full = d / 8;
        if key[..full] != self.bits[..full] {
            return false;
        }
        let rem = d % 8;
        rem == 0 || (key[full] ^ self.bits[full]) & (0xffu8 << (8 - rem)) == 0
    }
}

// ---------------------------------------------------------------- nyjet

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Empty = 0,
    Leaf = 1,
    Internal = 2,
}

fn kind_of(b: u8) -> Kind {
    match b {
        0 => Kind::Empty,
        1 => Kind::Leaf,
        _ => Kind::Internal,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Ref {
    kind: Kind,
    hash: Hash,
}

const EMPTY_REF: Ref = Ref { kind: Kind::Empty, hash: EMPTY };

#[derive(Clone, PartialEq, Eq, Debug)]
struct LeafData {
    key: Key,
    meta: AccountMeta,
    /// `meta.value_hash()`, i ruajtur që të mos rillogaritet.
    vh: Hash,
}

/// Gjethja mbahet në kuti, që nyja e brendshme (shumica në cache) të zërë pak RAM.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Node {
    Leaf(Box<LeafData>),
    Internal { l: Ref, r: Ref },
}

impl Node {
    fn leaf(key: Key, meta: AccountMeta) -> Self {
        let vh = meta.value_hash();
        Node::Leaf(Box::new(LeafData { key, meta, vh }))
    }
    fn hash(&self) -> Hash {
        match self {
            Node::Leaf(d) => leaf_hash(&d.key, &d.vh),
            Node::Internal { l, r } => internal_hash(&l.hash, &r.hash),
        }
    }
    fn is_leaf(&self) -> bool {
        matches!(self, Node::Leaf(_))
    }
    fn to_ref(&self) -> Ref {
        let kind = if self.is_leaf() { Kind::Leaf } else { Kind::Internal };
        Ref { kind, hash: self.hash() }
    }
    fn enc(&self) -> Vec<u8> {
        match self {
            Node::Leaf(d) => [&[1u8][..], &d.key, &enc_meta(&d.meta)].concat(),
            Node::Internal { l, r } => [&[2u8][..], &[l.kind as u8], &l.hash, &[r.kind as u8], &r.hash].concat(),
        }
    }
    fn dec(b: &[u8]) -> Option<Node> {
        let h = |s: &[u8]| -> Hash { s.try_into().unwrap() };
        match b.first()? {
            1 if b.len() == 33 + META_LEN => Some(Node::leaf(h(&b[1..33]), dec_meta(&b[33..])?)),
            2 if b.len() == 67 => Some(Node::Internal {
                l: Ref { kind: kind_of(b[1]), hash: h(&b[2..34]) },
                r: Ref { kind: kind_of(b[34]), hash: h(&b[35..67]) },
            }),
            _ => None,
        }
    }
}

fn ref_of(n: &Option<Node>) -> Ref {
    n.as_ref().map_or(EMPTY_REF, Node::to_ref)
}

const META_LEN: usize = 81;

fn enc_meta(m: &AccountMeta) -> [u8; META_LEN] {
    let mut v = [0u8; META_LEN];
    v[..8].copy_from_slice(&m.lamports.to_le_bytes());
    v[8..40].copy_from_slice(&m.owner);
    v[40] = m.executable as u8;
    v[41..49].copy_from_slice(&m.data_len.to_le_bytes());
    v[49..81].copy_from_slice(&m.data_hash);
    v
}

fn dec_meta(b: &[u8]) -> Option<AccountMeta> {
    if b.len() != META_LEN {
        return None;
    }
    Some(AccountMeta {
        lamports: u64::from_le_bytes(b[..8].try_into().unwrap()),
        owner: b[8..40].try_into().unwrap(),
        executable: b[40] == 1,
        data_len: u64::from_le_bytes(b[41..49].try_into().unwrap()),
        data_hash: b[49..81].try_into().unwrap(),
    })
}

fn with_u64(v: u64, body: &[u8]) -> Vec<u8> {
    [&v.to_be_bytes()[..], body].concat()
}

fn split_u64(b: &[u8]) -> Option<(u64, &[u8])> {
    (b.len() >= 8).then(|| (u64::from_be_bytes(b[..8].try_into().unwrap()), &b[8..]))
}

/// Një ndryshim nyjeje: vendos `new` në `path` (None = hiq), `old` = ç'ishte aty.
struct NodeW {
    path: NPath,
    new: Option<Node>,
    old: Option<(u64, Node)>,
}

fn emit(out: &mut Vec<NodeW>, path: NPath, new: Option<Node>, old: Option<(u64, Node)>) {
    let same = match (&new, &old) {
        (None, None) => true,
        (Some(n), Some((_, o))) => n == o,
        _ => false,
    };
    if !same {
        out.push(NodeW { path, new, old });
    }
}

// ---------------------------------------------------------------- gjendja

#[derive(Clone, Debug, PartialEq, Eq)]
struct State {
    version: u64,
    root: Ref,
    len: u64,
    complete: bool,
    /// Sloti i ndërtimit të fundit nga snapshot-i.
    base: u64,
    cache_depth: u16,
    blockhash: Option<String>,
}

impl State {
    fn enc(&self) -> Vec<u8> {
        let mut v = vec![2u8];
        v.extend_from_slice(&self.version.to_le_bytes());
        v.push(self.root.kind as u8);
        v.extend_from_slice(&self.root.hash);
        v.extend_from_slice(&self.len.to_le_bytes());
        v.push(self.complete as u8);
        v.extend_from_slice(&self.base.to_le_bytes());
        v.extend_from_slice(&self.cache_depth.to_le_bytes());
        if let Some(b) = &self.blockhash {
            v.extend_from_slice(b.as_bytes());
        }
        v
    }
    fn dec(b: &[u8]) -> Option<State> {
        if b.len() < 61 || b[0] != 2 {
            return None;
        }
        Some(State {
            version: u64::from_le_bytes(b[1..9].try_into().ok()?),
            root: Ref { kind: kind_of(b[9]), hash: b[10..42].try_into().ok()? },
            len: u64::from_le_bytes(b[42..50].try_into().ok()?),
            complete: b[50] == 1,
            base: u64::from_le_bytes(b[51..59].try_into().ok()?),
            cache_depth: u16::from_le_bytes(b[59..61].try_into().ok()?),
            blockhash: String::from_utf8(b[61..].to_vec()).ok().filter(|s| !s.is_empty()),
        })
    }
}

/// Ndryshimet e niveleve të sipërme në një slot: vlerat e vjetra, për dëftesat historike.
type CacheDiff = (u64, HashMap<PKey, Option<(u64, Node)>>);

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
    /// Nyjet e poshtme të lexuara paraprakisht për slotin që po zbatohet.
    prefetched: HashMap<PKey, Option<(u64, Node)>>,
}

fn cf_names() -> [&'static str; 4] {
    [CF_NODES, CF_HIST, CF_STAGING, CF_META]
}

/// Opsionet e hapjes.
#[derive(Clone, Debug)]
pub struct StoreOptions {
    pub history: u64,
    /// Nivelet në RAM. Vlen vetëm kur krijohet disku; më pas lexohet nga gjendja.
    pub cache_depth: u16,
    pub block_cache_mb: usize,
    /// `false` = pa WAL: më shpejt, por pas një rrëzimi mund të humbasin slotet e fundit
    /// (gjendja mbetet e rregullt sepse `atomic_flush` ruan të gjitha CF-të bashkë).
    pub durable: bool,
}

impl Default for StoreOptions {
    fn default() -> Self {
        StoreOptions { history: DEFAULT_HISTORY, cache_depth: CACHE_DEPTH, block_cache_mb: 512, durable: true }
    }
}

fn db_options(cache_mb: usize, cutoff: Arc<AtomicU64>) -> (Options, Vec<ColumnFamilyDescriptor>) {
    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);
    opts.set_atomic_flush(true);
    opts.increase_parallelism(std::thread::available_parallelism().map(|n| n.get() as i32).unwrap_or(2));
    let cache = Cache::new_lru_cache(cache_mb << 20);
    let cfs = cf_names()
        .iter()
        .map(|name| {
            let mut bo = BlockBasedOptions::default();
            bo.set_block_cache(&cache);
            bo.set_bloom_filter(10.0, false);
            bo.set_cache_index_and_filter_blocks(true);
            let mut o = Options::default();
            o.set_block_based_table_factory(&bo);
            // LZ4: hash-et nuk ngjeshen, por meta-t dhe versionet po (~40% më pak disk në matje).
            o.set_compression_type(rocksdb::DBCompressionType::Lz4);
            o.set_write_buffer_size(64 << 20);
            if *name == CF_HIST {
                let c = cutoff.clone();
                o.set_compaction_filter("zilja-expire", move |_lvl, _k, v| {
                    let expired = v.len() >= 8 && u64::from_be_bytes(v[..8].try_into().unwrap()) <= c.load(Ordering::Relaxed);
                    if expired {
                        Decision::Remove
                    } else {
                        Decision::Keep
                    }
                });
            }
            ColumnFamilyDescriptor::new(*name, o)
        })
        .collect();
    (opts, cfs)
}

impl DiskTree {
    pub fn open(path: impl AsRef<Path>, history: u64) -> Result<Self, String> {
        Self::open_opts(path, StoreOptions { history, ..Default::default() })
    }

    pub fn open_opts(path: impl AsRef<Path>, o: StoreOptions) -> Result<Self, String> {
        let path = path.as_ref().to_path_buf();
        let cutoff = Arc::new(AtomicU64::new(0));
        let (opts, cfs) = db_options(o.block_cache_mb, cutoff.clone());
        let db = DB::open_cf_descriptors(&opts, &path, cfs).map_err(|e| e.to_string())?;
        let state = db
            .get_cf(db.cf_handle(CF_META).unwrap(), STATE_KEY)
            .map_err(|e| e.to_string())?
            .and_then(|b| State::dec(&b));
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
            prefetched: HashMap::default(),
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
        for item in self.db.iterator_cf(self.cf(CF_NODES), IteratorMode::Start) {
            let (k, v) = item.map_err(|e| e.to_string())?;
            let p = NPath::dec(&k).ok_or("çelës i dëmtuar")?;
            if p.depth > cd {
                break;
            }
            let (_, body) = split_u64(&v).ok_or("vlerë e dëmtuar")?;
            let n = Node::dec(body).ok_or("nyje e dëmtuar")?;
            if p.depth < cd && !n.is_leaf() {
                continue; // nivelet e sipërme të brendshme s'ruhen në disk
            }
            layer.insert(p.enc(), n);
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
        let k = p.enc();
        if let Some(hit) = self.prefetched.get(&k) {
            return hit.clone();
        }
        self.last_disk_reads.fetch_add(1, Ordering::Relaxed);
        let v = self.db.get_pinned_cf(self.cf(CF_NODES), k).ok()??;
        let (ver, body) = split_u64(&v)?;
        Some((ver, Node::dec(body)?))
    }

    /// Lexon paraprakisht, nivel pas niveli dhe me një kërkesë të grupuar për nivel,
    /// nyjet e poshtme që do t'i duhen zbatimit të `ups` (të renditura).
    fn prefetch(&mut self, ups: &[(Key, Option<AccountMeta>)]) {
        self.prefetched.clear();
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
            let res: Vec<Option<(u64, Node)>> = {
                use rayon::prelude::*;
                keys.par_chunks(512)
                    .flat_map_iter(|chunk| {
                        let cf = db.cf_handle(CF_NODES).unwrap();
                        db.batched_multi_get_cf(cf, chunk.iter(), true)
                            .into_iter()
                            .map(|r| {
                                r.ok().flatten().and_then(|v| {
                                    let (ver, body) = split_u64(&v)?;
                                    Some((ver, Node::dec(body)?))
                                })
                            })
                            .collect::<Vec<_>>()
                    })
                    .collect()
            };
            let mut next = Vec::new();
            for ((p, k), val) in frontier.iter().zip(keys.iter()).zip(res) {
                if let Some((_, Node::Internal { l, r })) = &val {
                    // Fëmijët që kanë ndryshime: çelësat në këtë rrugë, sipas bit-it të radhës.
                    let lo = ups.partition_point(|(key, _)| key < &p.bits);
                    let mut hi = lo;
                    while hi < ups.len() && p.contains(&ups[hi].0) {
                        hi += 1;
                    }
                    let mid = lo + ups[lo..hi].partition_point(|(key, _)| bit(key, p.depth as usize) == 0);
                    if mid > lo && l.kind != Kind::Empty {
                        next.push(p.child(0));
                    }
                    if hi > mid && r.kind != Kind::Empty {
                        next.push(p.child(1));
                    }
                }
                self.prefetched.insert(*k, val);
            }
            frontier = next;
        }
    }

    /// Nyja në `p` siç ishte në versionin `v`.
    fn read_at(&self, p: &NPath, v: u64) -> Result<Option<Node>, String> {
        let latest = self.read_latest(p);
        if let Some((ver, n)) = &latest {
            if *ver <= v {
                return Ok(Some(n.clone()));
            }
        }
        if p.depth < self.cache_depth {
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
        let mut it = self.db.raw_iterator_cf(self.cf(CF_HIST));
        it.seek_for_prev(p.enc_ver(v));
        if it.valid() {
            if let (Some(k), Some(val)) = (it.key(), it.value()) {
                if k.starts_with(&p.enc()) {
                    let (drop_at, body) = split_u64(val).ok_or("arkivë e dëmtuar")?;
                    // E vlefshme vetëm nëse ishte ende aty në v.
                    if drop_at > v {
                        return Ok(Some(Node::dec(body).ok_or("arkivë e dëmtuar")?));
                    }
                }
            }
        }
        Ok(None)
    }

    // ------------------------------------------------------------ algoritmi

    /// Ndërton një nënpemë të re në `p` (poshtë `p` nuk ka asgjë). Nuk shkruan vetë `p`.
    fn build(&self, p: NPath, ups: &[(Key, Option<AccountMeta>)], out: &mut Vec<NodeW>) -> Option<Node> {
        let live = ups.iter().filter(|(_, v)| v.is_some()).count();
        match live {
            0 => None,
            1 => {
                let (k, v) = ups.iter().find(|(_, v)| v.is_some()).unwrap();
                Some(Node::leaf(*k, v.clone().unwrap()))
            }
            _ => {
                let i = ups.partition_point(|(k, _)| bit(k, p.depth as usize) == 0);
                let (u0, u1) = ups.split_at(i);
                let (c0, c1) = if ups.len() > PAR_THRESHOLD {
                    let ((c0, o0), (c1, o1)) = rayon::join(
                        || {
                            let mut o = Vec::new();
                            (self.build(p.child(0), u0, &mut o), o)
                        },
                        || {
                            let mut o = Vec::new();
                            (self.build(p.child(1), u1, &mut o), o)
                        },
                    );
                    out.extend(o0);
                    out.extend(o1);
                    (c0, c1)
                } else {
                    (self.build(p.child(0), u0, out), self.build(p.child(1), u1, out))
                };
                let node = Node::Internal { l: ref_of(&c0), r: ref_of(&c1) };
                emit(out, p.child(0), c0, None);
                emit(out, p.child(1), c1, None);
                Some(node)
            }
        }
    }

    /// Zbaton ndryshimet në `p`, ku tani ndodhet `old`. Kthen nyjen e re për `p` (pa e
    /// shkruar) dhe ndryshimin në numrin e llogarive.
    fn apply(
        &self,
        p: NPath,
        old: Option<(u64, Node)>,
        ups: &[(Key, Option<AccountMeta>)],
        out: &mut Vec<NodeW>,
    ) -> (Option<Node>, i64) {
        match old.map(|o| o.1) {
            None => {
                let delta = ups.iter().filter(|(_, m)| m.is_some()).count() as i64;
                (self.build(p, ups, out), delta)
            }
            Some(Node::Leaf(d)) => {
                let LeafData { key, meta, .. } = *d;
                let others = ups.iter().filter(|(k, m)| *k != key && m.is_some()).count() as i64;
                match ups.binary_search_by(|(k, _)| k.cmp(&key)) {
                    Ok(i) => {
                        let removed = if ups[i].1.is_none() { 1 } else { 0 };
                        (self.build(p, ups, out), others - removed)
                    }
                    Err(pos) => {
                        let mut merged = Vec::with_capacity(ups.len() + 1);
                        merged.extend_from_slice(&ups[..pos]);
                        merged.push((key, Some(meta)));
                        merged.extend_from_slice(&ups[pos..]);
                        (self.build(p, &merged, out), others)
                    }
                }
            }
            Some(Node::Internal { l, r }) => {
                let i = ups.partition_point(|(k, _)| bit(k, p.depth as usize) == 0);
                let (u0, u1) = ups.split_at(i);
                type Side = (bool, Option<Node>, Option<(u64, Node)>, Vec<NodeW>, i64);
                let side = |b: u8, u: &[(Key, Option<AccountMeta>)], rf: Ref| -> Side {
                    let mut o = Vec::new();
                    if u.is_empty() {
                        return (false, None, None, o, 0);
                    }
                    let cp = p.child(b);
                    let old_c = if rf.kind == Kind::Empty { None } else { self.read_latest(&cp) };
                    let (res, d) = self.apply(cp, old_c.clone(), u, &mut o);
                    (true, res, old_c, o, d)
                };
                let (s0, s1) = if ups.len() > PAR_THRESHOLD {
                    rayon::join(|| side(0, u0, l), || side(1, u1, r))
                } else {
                    (side(0, u0, l), side(1, u1, r))
                };
                let (ch0, n0, old0, o0, d0) = s0;
                let (ch1, n1, old1, o1, d1) = s1;
                out.extend(o0);
                out.extend(o1);
                let k0 = if ch0 { ref_of(&n0).kind } else { l.kind };
                let k1 = if ch1 { ref_of(&n1).kind } else { r.kind };
                let (cp0, cp1) = (p.child(0), p.child(1));
                let node = match (k0, k1) {
                    (Kind::Empty, Kind::Empty) => {
                        if ch0 {
                            emit(out, cp0, None, old0);
                        }
                        if ch1 {
                            emit(out, cp1, None, old1);
                        }
                        None
                    }
                    (Kind::Leaf, Kind::Empty) | (Kind::Empty, Kind::Leaf) => {
                        // Gjethja e vetme ngjitet lart; vendi i saj dhe vendi bosh lirohen.
                        let mut sides = [(ch0, n0, old0, cp0), (ch1, n1, old1, cp1)];
                        let li = if k0 == Kind::Leaf { 0 } else { 1 };
                        let (ch, n, old, cp) = std::mem::replace(&mut sides[li], (false, None, None, cp0));
                        let (leaf, old_place) = if ch {
                            (n.unwrap(), old)
                        } else {
                            let o = self.read_latest(&cp);
                            (o.as_ref().expect("gjethja e pandryshuar mungon").1.clone(), o)
                        };
                        emit(out, cp, None, old_place);
                        let (che, _, olde, cpe) = std::mem::replace(&mut sides[1 - li], (false, None, None, cp0));
                        if che {
                            emit(out, cpe, None, olde);
                        }
                        Some(leaf)
                    }
                    _ => {
                        let l2 = if ch0 { ref_of(&n0) } else { l };
                        let r2 = if ch1 { ref_of(&n1) } else { r };
                        if ch0 {
                            emit(out, cp0, n0, old0);
                        }
                        if ch1 {
                            emit(out, cp1, n1, old1);
                        }
                        Some(Node::Internal { l: l2, r: r2 })
                    }
                };
                (node, d0 + d1)
            }
        }
    }

    /// Kalon ndryshimet në `batch` (disk) dhe në cache/unazë (RAM).
    fn write_nodes(&mut self, batch: &mut WriteBatch, v: u64, ws: Vec<NodeW>) {
        let hist = self.history > 0;
        let cd = self.cache_depth;
        let mut diff: HashMap<PKey, Option<(u64, Node)>> = HashMap::default();
        let nodes = self.db.cf_handle(CF_NODES).unwrap();
        let hcf = self.db.cf_handle(CF_HIST).unwrap();
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
                if hist {
                    diff.insert(key, w.old.clone());
                }
                match w.new {
                    Some(n) => {
                        self.cache.insert(key, (v, n));
                    }
                    None => {
                        self.cache.remove(&key);
                    }
                }
            } else {
                match &w.new {
                    Some(n) => batch.put_cf(nodes, key, with_u64(v, &n.enc())),
                    None => batch.delete_cf(nodes, key),
                }
                if hist {
                    if let Some((ov, on)) = &w.old {
                        batch.put_cf(hcf, w.path.enc_ver(*ov), with_u64(v, &on.enc()));
                    }
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
        self.write(std::mem::take(batch))?;
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

    fn build_bucketed(&self, p: NPath, bucket_depth: u16, slot: u64) -> Result<Option<Node>, String> {
        let persist = |batch: &mut WriteBatch, path: &NPath, n: &Node| {
            if path.depth >= self.cache_depth || n.is_leaf() {
                batch.put_cf(self.cf(CF_NODES), path.enc(), with_u64(slot, &n.enc()));
            }
        };
        if p.depth == bucket_depth {
            let mut ups = Vec::new();
            let mut it = self.db.raw_iterator_cf(self.cf(CF_STAGING));
            it.seek(p.bits);
            while it.valid() {
                let k: Key = it.key().unwrap().try_into().map_err(|_| "çelës i dëmtuar")?;
                if !p.contains(&k) {
                    break;
                }
                ups.push((k, Some(dec_meta(it.value().unwrap()).ok_or("meta e dëmtuar")?)));
                it.next();
            }
            let mut out = Vec::new();
            let res = self.build(p, &ups, &mut out);
            let mut batch = WriteBatch::default();
            for w in out {
                if let Some(n) = &w.new {
                    persist(&mut batch, &w.path, n);
                }
            }
            self.write(batch)?;
            return Ok(res);
        }
        let c0 = self.build_bucketed(p.child(0), bucket_depth, slot)?;
        let c1 = self.build_bucketed(p.child(1), bucket_depth, slot)?;
        let mut batch = WriteBatch::default();
        let res = match (&c0, &c1) {
            (None, None) => None,
            (Some(n @ Node::Leaf(_)), None) | (None, Some(n @ Node::Leaf(_))) => Some(n.clone()),
            _ => {
                for (b, c) in [(0u8, &c0), (1u8, &c1)] {
                    if let Some(n) = c {
                        persist(&mut batch, &p.child(b), n);
                    }
                }
                Some(Node::Internal { l: ref_of(&c0), r: ref_of(&c1) })
            }
        };
        self.write(batch)?;
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

fn hx(h: &Hash) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

impl StateTree for DiskTree {
    fn root(&self) -> Hash {
        self.state.as_ref().map_or(EMPTY, |s| s.root.hash)
    }

    fn len(&self) -> usize {
        self.state.as_ref().map_or(0, |s| s.len as usize)
    }

    fn stage_snapshot(&mut self, _slot: u64, ups: Vec<Update>) {
        if self.staged == 0 && self.state.is_some() {
            // Ndërtim i ri: hidh gjendjen e vjetër.
            let _ = self.clear();
        }
        let mut batch = WriteBatch::default();
        for (k, m) in ups {
            match m {
                Some(m) => batch.put_cf(self.cf(CF_STAGING), k, enc_meta(&m)),
                None => batch.delete_cf(self.cf(CF_STAGING), k),
            }
            self.staged += 1;
        }
        if let Err(e) = self.write(batch) {
            log::error!("zilja-store: shkrimi i snapshot-it dështoi: {e}");
        }
    }

    fn finish_snapshot(&mut self, slot: u64, complete: bool) -> Hash {
        if self.staged == 0 && self.state.is_some() {
            let _ = self.clear();
        }
        let len = self.db.iterator_cf(self.cf(CF_STAGING), IteratorMode::Start).count() as u64;
        let mut bucket_depth = 0u16;
        while (len >> bucket_depth) > self.bucket_target && bucket_depth < 16 {
            bucket_depth += 1;
        }
        let root = self.build_bucketed(NPath::root(), bucket_depth, slot).expect("ndërtimi nga snapshot-i dështoi");
        let mut batch = WriteBatch::default();
        if let Some(n) = &root {
            if self.cache_depth == 0 || n.is_leaf() {
                batch.put_cf(self.cf(CF_NODES), NPath::root().enc(), with_u64(slot, &n.enc()));
            }
        }
        batch.delete_range_cf(self.cf(CF_STAGING), [].as_slice(), [0xffu8; 64].as_slice());
        let st = State {
            version: slot,
            root: ref_of(&root),
            len,
            complete,
            base: slot,
            cache_depth: self.cache_depth,
            blockhash: None,
        };
        self.commit_state(&mut batch, st).expect("shkrimi i gjendjes dështoi");
        self.db.compact_range_cf(self.cf(CF_STAGING), None::<&[u8]>, None::<&[u8]>);
        self.staged = 0;
        self.rebuild_cache().expect("cache");
        self.trim_history(slot);
        self.root()
    }

    fn apply_slot(&mut self, slot: u64, mut ups: Vec<Update>, blockhash: Option<String>) -> Hash {
        let prof = std::env::var_os("ZILJA_PROFILE").is_some();
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
            blockhash: None,
        });
        if self.state.is_none() {
            self.cache_since = slot;
        }
        self.prefetch(&ups);
        let t_pre = t0.elapsed();
        let mut batch = WriteBatch::default();
        let mut out = Vec::new();
        let root_path = NPath::root();
        let old_root = self.read_latest(&root_path);
        let (new_root, delta) = if ups.is_empty() {
            (old_root.clone().map(|o| o.1), 0)
        } else {
            self.apply(root_path, old_root.clone(), &ups, &mut out)
        };
        emit(&mut out, root_path, new_root.clone(), old_root);
        let t_tree = t0.elapsed();
        let nw = out.len();
        self.write_nodes(&mut batch, slot, out);
        self.prefetched.clear();
        let t_batch = t0.elapsed();
        let ops = batch.len();
        let st = State {
            version: slot,
            root: ref_of(&new_root),
            len: (prev.len as i64 + delta).max(0) as u64,
            complete: prev.complete,
            base: prev.base,
            cache_depth: self.cache_depth,
            blockhash,
        };
        self.commit_state(&mut batch, st).expect("shkrimi i slotit dështoi");
        self.trim_history(slot);
        if prof {
            eprintln!(
                "slot {slot}: leximi {:?}, pema {:?}, batch {:?}, shkrimi {:?} | {nw} nyje, {ops} ops, {} lexime",
                t_pre,
                t_tree - t_pre,
                t_batch - t_tree,
                t0.elapsed() - t_batch,
                self.last_disk_reads.load(Ordering::Relaxed)
            );
        }
        self.root()
    }

    fn prove_at(&self, key: &Key, at: Option<u64>) -> Result<ProofAt, String> {
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
                    let m = (d.key == *key).then_some(d.meta);
                    return Ok((v, root, Proof { siblings, leaf: Some((d.key, d.vh)) }, m));
                }
                Some(Node::Internal { l, r }) => {
                    let b = bit(key, p.depth as usize);
                    let (next, sib) = if b == 0 { (l, r) } else { (r, l) };
                    siblings.push(sib.hash);
                    p = p.child(b);
                    node = if next.kind == Kind::Empty {
                        None
                    } else {
                        Some(self.read_at(&p, v)?.ok_or("nyje që mungon në disk")?)
                    };
                }
            }
        }
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
            blockhash: s.blockhash.clone(),
        })
    }

    fn invalidate(&mut self) {
        let _ = self.clear();
    }
}
