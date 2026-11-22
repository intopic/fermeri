//! # Motori me faqe: pema në një skedar blloqesh, pa bazë të dhënash
//!
//! Pema ruhet në faqe me lartësi 4 (deri në 15 nyje), si te `DiskTree`, por pa RocksDB:
//!
//! - **Adresë direkte.** Çdo faqe ka një numër (`PageId`) = blloku, vendi në bllok dhe gjatësia.
//!   Faqja prind mban numrat e faqeve fëmijë (deri në 16). Leximi i një faqeje është një `pread`
//!   i vetëm, pa indeks, pa kërkim dhe pa kompaktim.
//! - **Blloqe të plota 4 KB.** Faqet e reja të një sloti paketohen njëra pas tjetrës në blloqe të
//!   lira të tëra, dhe çdo bllok shkruhet një herë, i plotë. Disku (dhe sistemi i skedarëve) punon me
//!   blloqe 4 KB: kështu një faqe e vogël nuk kushton një bllok të tërë shkrim.
//! - **Kopjim në shkrim (copy-on-write).** Një faqe që ndryshon shkruhet në vend të ri, kurrë mbi të
//!   vjetrën; prindi merr numrin e ri, deri te root-i. Faqja e vjetër mbetet e paprekur për `history`
//!   slote, kështu dëftesat historike lexohen thjesht nga root-i i vjetër, pa arkivë.
//! - **Mbledhja e hapësirës.** Një bllok ripërdoret kur s'ka më asnjë faqe të gjallë. Blloqet gjysmë
//!   bosh "zbrazen" duke rishkruar faqet e tyre të gjalla bashkë me faqet e slotit (çdo faqe mban
//!   rrugën e saj, ndaj gjendet prindi). Kështu skedari s'rritet pa fund.
//! - **Log + checkpoint.** Çdo slot shton një rekord të vogël në `meta.log`; pas disa slotesh e gjithë
//!   gjendja e alokuesit shkon në `meta.ckpt` dhe log-u zbrazet. Rinisja = checkpoint + rekordet pas tij.
//!
//! Siguria: çdo nyje që lexohet krahasohet me hash-in që mban prindi (F-04), dhe çdo faqe me rrugën
//! ku pritet. Një faqe e dëmtuar, e vjetër ose e zhvendosur nuk prodhon kurrë dëftesë të gabuar:
//! prodhon gabim dhe zilja ndalon.
//!
//! Faqet me rrënjë mbi `cache_depth` mbahen gjithmonë në RAM (vetëm versioni i fundit).

use super::*;
use sha2::Digest;
use std::collections::{BTreeSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, RwLock};

/// Madhësia e bllokut në skedar (= faqja e sistemit të skedarëve dhe e NVMe-së).
pub const BLOCK: usize = 4096;
/// Koka e faqes: crc32 (4) ‖ gjatësia e trupit (2).
const PAGE_HDR: usize = 6;
const KIDS: usize = 1 << PAGE_H;
/// Blloqet me më pak se kaq bajte të gjalla janë kandidatë për t'u zbrazur (60%).
const GC_LIVE_MAX: u16 = (BLOCK * 6 / 10) as u16;
/// Kovat e snapshot-it: 2^8, sipas bajtit të parë të çelësit (thellësia 8 = kufi faqeje).
const BUCKET_BITS: u16 = 8;
/// Ndërtimi nga snapshot-i mban në RAM më së shumti kaq blloqe 4 KB (~16 MB) para se t'i shkruajë.
pub const SNAPSHOT_FLUSH_BLOCKS: usize = 4096;
/// Rekordi i staging-ut: çelësi ‖ sloti ‖ radha ‖ flamuri ‖ meta.
const STAGE_REC: usize = 32 + 8 + 8 + 1 + META_LEN;
const LOG_MAGIC: &[u8; 4] = b"ZLR3";
const CKPT_MAGIC: &[u8; 4] = b"ZCK3";
const NO_PAGE: u64 = u64::MAX;

/// `blloku (41 bit) ‖ vendi në bllok (12) ‖ gjatësia (11)`.
pub type PageId = u64;

fn pid(block: u64, off: usize, len: usize) -> PageId {
    debug_assert!(off < BLOCK && len < 2048 && block < 1 << 41);
    (block << 23) | ((off as u64) << 11) | len as u64
}
fn pid_block(id: PageId) -> u64 {
    id >> 23
}
fn pid_off(id: PageId) -> usize {
    ((id >> 11) & 0xfff) as usize
}
fn pid_len(id: PageId) -> usize {
    (id & 0x7ff) as usize
}

// ---------------------------------------------------------------- faqja në skedar

/// Faqja: rruga e rrënjës, nyjet dhe numrat e faqeve fëmijë në thellësinë
/// `rrënja + 4`. Fëmija `j` është nënpema me bit-ët `rrënja..rrënja+4` = `j`.
#[derive(Clone, PartialEq, Eq, Debug)]
struct FPage {
    path: NPath,
    nodes: Page,
    kids: [Option<PageId>; KIDS],
}

impl FPage {
    fn new(path: NPath) -> Self {
        FPage { path, nodes: Page::new(), kids: [None; KIDS] }
    }

    /// `thellësia u16 ‖ bit-ët e rrugës (⌈thellësia/8⌉) ‖ gjatësia e nyjeve u16 ‖ nyjet ‖
    /// bitmap-i i fëmijëve u16 ‖ numrat u64`
    fn body(&self) -> Vec<u8> {
        let nodes = self.nodes.enc();
        let pb = self.path.depth.div_ceil(8) as usize;
        let mut v = Vec::with_capacity(6 + pb + nodes.len() + 8 * KIDS);
        v.extend_from_slice(&self.path.depth.to_le_bytes());
        v.extend_from_slice(&self.path.bits[..pb]);
        v.extend_from_slice(&(nodes.len() as u16).to_le_bytes());
        v.extend_from_slice(&nodes);
        let mut bm = 0u16;
        for (j, k) in self.kids.iter().enumerate() {
            if k.is_some() {
                bm |= 1 << j;
            }
        }
        v.extend_from_slice(&bm.to_le_bytes());
        for k in self.kids.iter().flatten() {
            v.extend_from_slice(&k.to_le_bytes());
        }
        v
    }

    fn dec_body(b: &[u8]) -> Option<FPage> {
        let depth = u16::from_le_bytes(b.get(..2)?.try_into().ok()?);
        if depth > 256 || depth % PAGE_H != 0 {
            return None;
        }
        let pb = depth.div_ceil(8) as usize;
        let mut bits = [0u8; 32];
        bits[..pb].copy_from_slice(b.get(2..2 + pb)?);
        let path = NPath { depth, bits };
        if path.truncate(depth) != path {
            return None; // bit-e përtej thellësisë: kodim jo kanonik
        }
        let mut pos = 2 + pb;
        let nlen = u16::from_le_bytes(b.get(pos..pos + 2)?.try_into().ok()?) as usize;
        pos += 2;
        let nodes = Page::dec(b.get(pos..pos + nlen)?)?;
        pos += nlen;
        let bm = u16::from_le_bytes(b.get(pos..pos + 2)?.try_into().ok()?);
        pos += 2;
        let mut kids = [None; KIDS];
        for (j, k) in kids.iter_mut().enumerate() {
            if bm >> j & 1 == 1 {
                *k = Some(u64::from_le_bytes(b.get(pos..pos + 8)?.try_into().ok()?));
                pos += 8;
            }
        }
        (pos == b.len()).then_some(FPage { path, nodes, kids })
    }

    /// `crc32 ‖ gjatësia ‖ trupi`, i gatshëm për t'u vendosur në një bllok.
    fn enc(&self) -> Vec<u8> {
        let body = self.body();
        let mut v = Vec::with_capacity(PAGE_HDR + body.len());
        v.extend_from_slice(&crc32fast::hash(&body).to_le_bytes());
        v.extend_from_slice(&(body.len() as u16).to_le_bytes());
        v.extend_from_slice(&body);
        v
    }

    fn dec(b: &[u8]) -> Option<FPage> {
        let crc = u32::from_le_bytes(b.get(..4)?.try_into().ok()?);
        let len = u16::from_le_bytes(b.get(4..6)?.try_into().ok()?) as usize;
        if b.len() != PAGE_HDR + len {
            return None;
        }
        let body = &b[PAGE_HDR..];
        if crc32fast::hash(body) != crc {
            return None;
        }
        FPage::dec_body(body)
    }

    /// Fëmijët duhet të përputhen me nyjet e rreshtit të fundit: fëmijë jo-bosh ⇔ faqe fëmijë.
    fn check_kids(&self) -> Result<(), String> {
        for i in 7..PAGE_SLOTS {
            let refs = match &self.nodes.slots[i] {
                Some(Node::Internal { l, r }) => [l.kind != Kind::Empty, r.kind != Kind::Empty],
                _ => [false, false],
            };
            for (b, want) in refs.into_iter().enumerate() {
                if self.kids[2 * (i - 7) + b].is_some() != want {
                    return Err("faqja nuk përputhet me faqet fëmijë".into());
                }
            }
        }
        Ok(())
    }
}

/// Rruga e fëmijës `j` të faqes me rrënjë `root`.
fn kid_path(root: &NPath, j: usize) -> NPath {
    let mut p = *root;
    for i in 0..PAGE_H {
        p = p.child(((j >> (PAGE_H - 1 - i)) & 1) as u8);
    }
    p
}

/// Indeksi `j` i faqes me rrënjë `p` brenda prindit të saj.
fn kid_index(p: &NPath) -> usize {
    let mut j = 0usize;
    for i in p.depth - PAGE_H..p.depth {
        j = (j << 1) | bit(&p.bits, i as usize) as usize;
    }
    j
}

fn page_root_of(p: &NPath) -> NPath {
    p.truncate(p.depth - p.depth % PAGE_H)
}

// ---------------------------------------------------------------- alokuesi

/// Blloqet: të lira, të përdorura dhe sa bajte të gjalla ka secili. Determinist: i njëjti varg
/// rekordesh jep të njëjtën gjendje (rinisja e përsërit).
#[derive(Clone, Default, Debug)]
struct Alloc {
    /// Blloqe të përdorura ndonjëherë (skedari ka `next × 4 KB`).
    next: u64,
    /// Blloqe pa asnjë faqe të gjallë, gati për t'u rishkruar të tëra.
    free: Vec<u64>,
    /// Bajte të faqeve të gjalla ose në pritje, për çdo bllok.
    live: Vec<u16>,
    /// (sloti kur u liruan, rekordi që i liroi, faqet): lirohen vetëm kur s'i lexon më asnjë slot i
    /// kujtesës dhe kur kanë kaluar `slack` rekorde (mbrojtje ndaj humbjes së rekordeve të fundit).
    pending: VecDeque<(u64, u64, Vec<PageId>)>,
    /// Blloqe gjysmë bosh (0 < të gjalla ≤ 60%), sipas bajteve të gjalla: kandidatë për zbrazje.
    cand: BTreeSet<(u16, u64)>,
}

impl PartialEq for Alloc {
    fn eq(&self, o: &Self) -> bool {
        // `cand` rrjedh nga `live`.
        self.next == o.next && self.free == o.free && self.live == o.live && self.pending == o.pending
    }
}
impl Eq for Alloc {}

fn is_cand(v: u16) -> bool {
    v > 0 && v <= GC_LIVE_MAX
}

impl Alloc {
    fn set_live(&mut self, b: u64, v: u16) {
        let old = self.live[b as usize];
        if is_cand(old) {
            self.cand.remove(&(old, b));
        }
        self.live[b as usize] = v;
        if is_cand(v) {
            self.cand.insert((v, b));
        }
    }

    /// Faqet e zëvendësuara në slotin `w` ishin pjesë e versionit `w-1`. Ai s'lexohet më kur
    /// `w + history ≤ prev` (kujtesa e versionit `prev` nis te `prev - history`).
    ///
    /// Kushti i dytë: rekordi `q` që i liroi duhet të jetë të paktën `slack + 1` rekorde para atij që
    /// po përgatitet (`next_q`). Kështu, nëse pas një ikjeje rryme humbin deri në `slack + 1`
    /// rekordet e fundit, versioni para tyre është ende i paprekur në disk.
    fn release(&mut self, prev: u64, history: u64, next_q: u64, slack: u64) {
        while self.pending.front().is_some_and(|(w, q, _)| w.saturating_add(history) <= prev && q.saturating_add(slack) < next_q) {
            for id in self.pending.pop_front().unwrap().2 {
                let b = pid_block(id);
                let v = self.live[b as usize].saturating_sub(pid_len(id) as u16);
                self.set_live(b, v);
                if v == 0 {
                    self.free.push(b);
                }
            }
        }
    }

    /// Një bllok i lirë: së pari nga të lirët (nga fundi), pastaj i ri në fund të skedarit.
    fn take_block(&mut self) -> u64 {
        match self.free.pop() {
            Some(b) => b,
            None => {
                self.live.push(0);
                self.next += 1;
                self.next - 1
            }
        }
    }

    /// Kthen blloqet e marra që s'u përdorën (gabim gjatë përgatitjes), saktësisht si ishin.
    fn untake(&mut self, taken: &[u64], next0: u64) {
        for &b in taken.iter().rev() {
            self.set_live(b, 0);
            if b < next0 {
                self.free.push(b);
            }
        }
        self.live.truncate(next0 as usize);
        self.next = next0;
    }

    fn retire(&mut self, slot: u64, q: u64, ids: Vec<PageId>) {
        if !ids.is_empty() {
            self.pending.push_back((slot, q, ids));
        }
    }

    fn enc(&self, v: &mut Vec<u8>) {
        v.extend_from_slice(&self.next.to_le_bytes());
        v.extend_from_slice(&(self.free.len() as u64).to_le_bytes());
        for x in &self.free {
            v.extend_from_slice(&x.to_le_bytes());
        }
        for l in &self.live {
            v.extend_from_slice(&l.to_le_bytes());
        }
        v.extend_from_slice(&(self.pending.len() as u64).to_le_bytes());
        for (w, q, ids) in &self.pending {
            v.extend_from_slice(&w.to_le_bytes());
            v.extend_from_slice(&q.to_le_bytes());
            v.extend_from_slice(&(ids.len() as u64).to_le_bytes());
            for x in ids {
                v.extend_from_slice(&x.to_le_bytes());
            }
        }
    }

    fn dec(r: &mut Cur) -> Option<Alloc> {
        let mut a = Alloc { next: r.u64()?, ..Default::default() };
        let nf = r.len()?;
        a.free = (0..nf).map(|_| r.u64()).collect::<Option<_>>()?;
        if a.next > r.0.len() as u64 / 2 {
            return None;
        }
        a.live = (0..a.next).map(|_| r.u16()).collect::<Option<_>>()?;
        let mut seen = std::collections::HashSet::new();
        for &b in &a.free {
            // Një bllok i lirë s'ka bajte të gjalla dhe s'del dy herë.
            if b >= a.next || a.live[b as usize] != 0 || !seen.insert(b) {
                return None;
            }
        }
        for _ in 0..r.len()? {
            let w = r.u64()?;
            let q = r.u64()?;
            let n = r.len()?;
            a.pending.push_back((w, q, (0..n).map(|_| r.u64()).collect::<Option<_>>()?));
        }
        a.cand = a.live.iter().enumerate().filter(|(_, v)| is_cand(**v)).map(|(b, v)| (*v, b as u64)).collect();
        Some(a)
    }
}

/// Lexues i thjeshtë bajtesh për rekordet.
struct Cur<'a>(&'a [u8]);

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_le_bytes(b.try_into().unwrap()))
    }
    fn u64(&mut self) -> Option<u64> {
        self.take(8).map(|b| u64::from_le_bytes(b.try_into().unwrap()))
    }
    /// Një numër elementesh, i kufizuar nga bajtet që mbeten (s'lejon alokime të mëdha nga të dhëna të dëmtuara).
    fn len(&mut self) -> Option<usize> {
        let n = self.u64()?;
        (n <= self.0.len() as u64).then_some(n as usize)
    }
    fn hash(&mut self) -> Option<Hash> {
        self.take(32).map(|b| b.try_into().unwrap())
    }
}

fn opt_id(id: Option<PageId>) -> u64 {
    id.unwrap_or(NO_PAGE)
}
fn id_opt(x: u64) -> Option<PageId> {
    (x != NO_PAGE).then_some(x)
}

// ---------------------------------------------------------------- rekordet

/// Rekordi i një sloti në log.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LogRec {
    history: u64,
    slack: u64,
    /// Numri rendor i rekordit (1 = snapshot-i).
    seq: u64,
    st: State,
    root_id: Option<PageId>,
    /// Bajtet e gjalla në çdo bllok të marrë, sipas radhës së marrjes.
    blocks: Vec<u16>,
    freed: Vec<PageId>,
}

impl LogRec {
    fn payload(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(192 + 2 * self.blocks.len() + 8 * self.freed.len());
        v.extend_from_slice(&self.history.to_le_bytes());
        v.extend_from_slice(&self.slack.to_le_bytes());
        v.extend_from_slice(&self.seq.to_le_bytes());
        let st = self.st.enc();
        v.push(st.len() as u8);
        v.extend_from_slice(&st);
        v.extend_from_slice(&opt_id(self.root_id).to_le_bytes());
        v.extend_from_slice(&(self.blocks.len() as u64).to_le_bytes());
        for b in &self.blocks {
            v.extend_from_slice(&b.to_le_bytes());
        }
        v.extend_from_slice(&(self.freed.len() as u64).to_le_bytes());
        for x in &self.freed {
            v.extend_from_slice(&x.to_le_bytes());
        }
        v
    }

    fn dec_payload(b: &[u8]) -> Option<LogRec> {
        let mut r = Cur(b);
        let history = r.u64()?;
        let slack = r.u64()?;
        let seq = r.u64()?;
        let sl = r.u8()? as usize;
        let st = State::dec(r.take(sl)?)?;
        let root_id = id_opt(r.u64()?);
        let nb = r.len()?;
        let blocks = (0..nb).map(|_| r.u16()).collect::<Option<_>>()?;
        let n = r.len()?;
        let freed = (0..n).map(|_| r.u64()).collect::<Option<_>>()?;
        r.0.is_empty().then_some(LogRec { history, slack, seq, st, root_id, blocks, freed })
    }

    /// G18: një rekord mbi `MAX_RECORD` s'shkruhet: hapja do ta refuzonte si të dëmtuar, ndaj sloti s'do
    /// të rikthehej dot pas rinisjes. Gabimi del para shkrimit, pra para se sloti të konfirmohet.
    fn framed(&self) -> Result<Vec<u8>, String> {
        let p = self.payload();
        if p.len() as u64 > MAX_RECORD {
            return Err(format!("rekordi i log-ut për slotin {} ka {} B, mbi kufirin {MAX_RECORD} B", self.st.version, p.len()));
        }
        Ok(frame(LOG_MAGIC, &p))
    }
}

fn frame(magic: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(16 + payload.len());
    v.extend_from_slice(magic);
    v.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    v.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
    v.extend_from_slice(payload);
    v
}

/// P-02: buxheti i deklaruar. Një rekord log-u s'kalon 64 MB; log-u s'kalon 256 MB (ose 2× pragun e
/// checkpoint-it, nëse është më i madh); checkpoint-i s'kalon 8 GB (≈ 2 B për bllok 4 KB + lista).
const MAX_RECORD: u64 = 64 << 20;
const MIN_LOG_BUDGET: u64 = 256 << 20;
const MAX_CKPT: u64 = 8 << 30;

/// A05: shënuesi i ndërtimit nga snapshot-i. Shkruhet (me fsync) para hapit të parë shkatërrues të
/// ndërtimit dhe fshihet vetëm pasi checkpoint-i i parë është në disk. Pa të, një dosje me faqe por
/// pa checkpoint s'fshihet kurrë automatikisht.
const BUILDING: &str = "BUILDING";

/// Rezultati i leximit të një kornize në fillim të `buf`.
#[derive(Debug, PartialEq, Eq)]
enum Frame<'a> {
    /// Korniza e plotë dhe e saktë: trupi dhe gjatësia gjithsej.
    Ok(&'a [u8], usize),
    /// Bisht i papërfunduar që shtrihet deri në fund të skedarit: kokë e prerë, trup i prerë ose vetëm
    /// zero. Një kornizë e plotë me CRC të gabuar s'është kurrë bisht (A04).
    Torn(&'static str),
    /// Dëmtim ose format i panjohur.
    Bad(String),
}

fn parse_frame<'a>(buf: &'a [u8], magic: &[u8; 4], max: u64) -> Frame<'a> {
    if buf.iter().all(|b| *b == 0) {
        return Frame::Torn("vetëm zero");
    }
    if buf.len() < 16 {
        let n = buf.len().min(4);
        return if buf[..n] == magic[..n] { Frame::Torn("kokë e prerë") } else { Frame::Bad("magji e panjohur".into()) };
    }
    if &buf[..4] != magic {
        return if buf[..3] == magic[..3] {
            Frame::Bad(format!("format i panjohur {:?} (pritej {:?})", String::from_utf8_lossy(&buf[..4]), String::from_utf8_lossy(magic)))
        } else {
            Frame::Bad("magji e panjohur (jo një rekord i Ziles)".into())
        };
    }
    let len = u64::from_le_bytes(buf[4..12].try_into().unwrap());
    if len > max {
        return Frame::Bad(format!("gjatësi e pamundshme {len} B (kufiri {max} B)"));
    }
    let end = 16 + len as usize;
    if end > buf.len() {
        return Frame::Torn("trup i prerë");
    }
    let crc = u32::from_le_bytes(buf[12..16].try_into().unwrap());
    let body = &buf[16..end];
    if crc32fast::hash(body) != crc {
        // A04: një kornizë e plotë (kokë + gjithë trupi) me CRC të gabuar mund të jetë dëmtim i një
        // sloti tashmë të konfirmuar; pas faktit s'dallohet nga një shkrim i papërfunduar. Refuzohet.
        return Frame::Bad(if end == buf.len() { "CRC e gabuar në kornizën e fundit (e plotë)".into() } else { "CRC e gabuar".into() });
    }
    Frame::Ok(body, end)
}

/// Lexon një skedar të tërë brenda buxhetit; mungesa = `None`.
fn read_budget(path: &Path, budget: u64, what: &str) -> Result<Option<Vec<u8>>, String> {
    let f = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{what}: hapja e {}: {e}", path.display())),
    };
    let len = f.metadata().map_err(|e| format!("{what}: {e}"))?.len();
    if len > budget {
        return Err(format!("{what} {} ka {len} B, mbi buxhetin {budget} B; skedarët u ruajtën", path.display()));
    }
    let mut b = Vec::with_capacity(len as usize);
    (&f).read_to_end(&mut b).map_err(|e| format!("{what}: leximi i {}: {e}", path.display()))?;
    Ok(Some(b))
}

/// Kujtesa e versioneve: (sloti, faqja rrënjë, ref-i i root-it).
type Ring = VecDeque<(u64, Option<PageId>, Ref)>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Ckpt {
    history: u64,
    slack: u64,
    seq: u64,
    st: State,
    root_id: Option<PageId>,
    alloc: Alloc,
    ring: Ring,
}

impl Ckpt {
    fn payload(&self) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&self.history.to_le_bytes());
        v.extend_from_slice(&self.slack.to_le_bytes());
        v.extend_from_slice(&self.seq.to_le_bytes());
        let st = self.st.enc();
        v.push(st.len() as u8);
        v.extend_from_slice(&st);
        v.extend_from_slice(&opt_id(self.root_id).to_le_bytes());
        self.alloc.enc(&mut v);
        v.extend_from_slice(&(self.ring.len() as u64).to_le_bytes());
        for (s, id, rf) in &self.ring {
            v.extend_from_slice(&s.to_le_bytes());
            v.extend_from_slice(&opt_id(*id).to_le_bytes());
            v.push(rf.kind as u8);
            v.extend_from_slice(&rf.hash);
        }
        v
    }

    fn dec_payload(b: &[u8]) -> Option<Ckpt> {
        let mut r = Cur(b);
        let history = r.u64()?;
        let slack = r.u64()?;
        let seq = r.u64()?;
        let sl = r.u8()? as usize;
        let st = State::dec(r.take(sl)?)?;
        let root_id = id_opt(r.u64()?);
        let alloc = Alloc::dec(&mut r)?;
        let mut ring = VecDeque::new();
        for _ in 0..r.len()? {
            let s = r.u64()?;
            let id = id_opt(r.u64()?);
            let kind = kind_of(r.u8()?)?;
            ring.push_back((s, id, Ref { kind, hash: r.hash()? }));
        }
        r.0.is_empty().then_some(Ckpt { history, slack, seq, st, root_id, alloc, ring })
    }
}

/// Safer-3 (fuzz): dekodon bajte arbitrare si trup faqeje, faqe me kokë, rekord log-u dhe
/// checkpoint. S'duhet të bjerë kurrë; çdo gjë që dekodohet duhet të rikodohet njësoj.
#[doc(hidden)]
pub fn fuzz_decode_pages(b: &[u8]) -> usize {
    let mut n = 0;
    if let Some(pg) = FPage::dec_body(b) {
        assert_eq!(pg.body(), b, "trupi i faqes nuk rikodohet njësoj");
        let _ = pg.check_kids();
        assert_eq!(FPage::dec(&pg.enc()), Some(pg));
        n += 1;
    }
    if let Some(pg) = FPage::dec(b) {
        assert_eq!(pg.enc(), b, "faqja nuk rikodohet njësoj");
        n += 1;
    }
    if let Some(r) = LogRec::dec_payload(b) {
        assert_eq!(r.payload(), b, "rekordi i log-ut nuk rikodohet njësoj");
        n += 1;
    }
    if let Some(c) = Ckpt::dec_payload(b) {
        assert_eq!(c.payload(), b, "checkpoint-i nuk rikodohet njësoj");
        n += 1;
    }
    n
}

/// Hyrje fillestare për fuzz-in, nga koduesit e vërtetë.
#[doc(hidden)]
pub fn fuzz_seeds_pages(rng_bytes: &[u8]) -> Vec<Vec<u8>> {
    let take =
        |i: usize| -> [u8; 32] { std::array::from_fn(|j| rng_bytes.get((i * 32 + j) % rng_bytes.len().max(1)).copied().unwrap_or(0)) };
    let m = AccountMeta { lamports: 5, owner: take(1), executable: false, data_len: 9, data_hash: take(2) };
    let mut pg = FPage::new(NPath { depth: 12, bits: take(8) }.truncate(12));
    pg.nodes.slots[0] = Some(Node::Internal { l: Ref { kind: Kind::Leaf, hash: take(3) }, r: Ref { kind: Kind::Internal, hash: take(4) } });
    pg.nodes.slots[1] = Some(Node::leaf(take(5), m));
    pg.nodes.slots[14] = Some(Node::Internal { l: EMPTY_REF, r: Ref { kind: Kind::Internal, hash: take(6) } });
    pg.kids[15] = Some(pid(77, 100, 300));
    let st = State {
        version: 9,
        root: Ref { kind: Kind::Internal, hash: take(7) },
        len: 3,
        complete: true,
        base: 1,
        cache_depth: 20,
        fingerprint: None,
    };
    let rec = LogRec {
        history: 32,
        slack: 8,
        seq: 4,
        st: st.clone(),
        root_id: Some(pid(1, 3, 200)),
        blocks: vec![4000, 120],
        freed: vec![pid(0, 1, 140), pid(7, 2, 900)],
    };
    let mut alloc = Alloc { next: 4, live: vec![0, 100, 0, 3000], free: vec![2, 0], ..Default::default() };
    alloc.pending.push_back((8, 3, vec![pid(1, 0, 100)]));
    let ck =
        Ckpt { history: 32, slack: 8, seq: 4, st, root_id: None, alloc, ring: VecDeque::from([(9, Some(pid(3, 0, 1000)), EMPTY_REF)]) };
    vec![pg.body(), pg.enc(), rec.payload(), ck.payload()]
}

// ---------------------------------------------------------------- paketuesi

/// Vendos faqet njëra pas tjetrës në blloqe të lira të plota.
struct Packer {
    blocks: Vec<(u64, Vec<u8>)>,
    /// Blloqet e marra nga alokuesi, sipas radhës (për rekordin dhe për kthimin pas një gabimi).
    taken: Vec<u64>,
    next0: u64,
    /// Sa blloqe u mbajtën njëherësh në RAM (matje: ndërtimi nga snapshot-i duhet të mbetet i kufizuar).
    peak: usize,
}

impl Packer {
    fn new(a: &Alloc) -> Self {
        Packer { blocks: Vec::new(), taken: Vec::new(), next0: a.next, peak: 0 }
    }

    fn place(&mut self, a: &Mutex<Alloc>, bytes: &[u8]) -> PageId {
        let need_new = self.blocks.last().is_none_or(|(_, buf)| buf.len() + bytes.len() > BLOCK);
        if need_new {
            let b = a.lock().unwrap().take_block();
            self.taken.push(b);
            self.blocks.push((b, Vec::with_capacity(BLOCK)));
        }
        self.peak = self.peak.max(self.blocks.len());
        let (b, buf) = self.blocks.last_mut().unwrap();
        let off = buf.len();
        buf.extend_from_slice(bytes);
        pid(*b, off, bytes.len())
    }

    /// Shënon bajtet e gjalla te alokuesi dhe kthen listën për rekordin.
    fn live_list(&self, a: &mut Alloc) -> Vec<u16> {
        self.blocks
            .iter()
            .map(|(b, buf)| {
                a.set_live(*b, buf.len() as u16);
                buf.len() as u16
            })
            .collect()
    }
}

// ---------------------------------------------------------------- pema në RAM për snapshot-in

/// Nënpemë në RAM gjatë ndërtimit nga snapshot-i. `Stub` = nënpemë e shkruar tashmë në disk.
enum MNode {
    Leaf(Node),
    Int(Option<Box<MNode>>, Option<Box<MNode>>, Ref, Ref),
    Stub(Ref, PageId),
}

impl MNode {
    fn rf(&self) -> Ref {
        match self {
            MNode::Leaf(n) => n.to_ref(),
            MNode::Int(_, _, l, r) => Ref { kind: Kind::Internal, hash: internal_hash(&l.hash, &r.hash) },
            MNode::Stub(rf, _) => *rf,
        }
    }
}

fn mref(n: &Option<Box<MNode>>) -> Ref {
    n.as_ref().map_or(EMPTY_REF, |n| n.rf())
}

/// Bashkon dy fëmijë në formën kanonike (gjethja e vetme ngjitet lart).
fn mjoin(l: Option<Box<MNode>>, r: Option<Box<MNode>>) -> Option<Box<MNode>> {
    match (l, r) {
        (None, None) => None,
        (Some(n), None) | (None, Some(n)) if matches!(*n, MNode::Leaf(_)) => Some(n),
        (l, r) => {
            let (lr, rr) = (mref(&l), mref(&r));
            Some(Box::new(MNode::Int(l, r, lr, rr)))
        }
    }
}

fn mbuild(depth: usize, ups: &[(Key, AccountMeta)]) -> Option<Box<MNode>> {
    match ups.len() {
        0 => None,
        1 => Some(Box::new(MNode::Leaf(Node::leaf(ups[0].0, ups[0].1.clone())))),
        n => {
            let i = ups.partition_point(|(k, _)| bit(k, depth) == 0);
            let (a, b) = ups.split_at(i);
            let (l, r) = if n > 4096 {
                rayon::join(|| mbuild(depth + 1, a), || mbuild(depth + 1, b))
            } else {
                (mbuild(depth + 1, a), mbuild(depth + 1, b))
            };
            mjoin(l, r)
        }
    }
}

// ---------------------------------------------------------------- PageTree

/// Opsionet e motorit me faqe.
#[derive(Clone, Debug)]
pub struct PageOptions {
    pub history: u64,
    /// Faqet me rrënjë mbi këtë thellësi mbahen gjithmonë në RAM.
    pub cache_depth: u16,
    /// `false` = asnjë fsync (testet, matjet). `true` = fsync sipas `sync_every`.
    pub durable: bool,
    /// fsync çdo N slote (0 = vetëm në snapshot dhe checkpoint). Me 1 (parazgjedhja), faqet shkojnë në
    /// disk gjatë përgatitjes dhe rekordi pas tyre: një ikje rryme humb maksimumi slotin në shkrim.
    pub sync_every: u64,
    /// Checkpoint pas kaq slotesh, ose kur log-u kalon `ckpt_log_mb`.
    pub ckpt_every: u64,
    pub ckpt_log_mb: u64,
    /// Rekorde shtesë para se një vend i liruar të ripërdoret. Kështu, edhe nëse humbasin deri në
    /// `reuse_slack + 1` rekordet e fundit pa fsync (ikje e rrymës), versioni para tyre është i plotë në disk.
    pub reuse_slack: u64,
    /// P-05: genesis-i i rrjetit. Ruhet në `MANIFEST` kur krijohet dosja, dhe çdo hapje pas saj duhet
    /// të japë të njëjtin; ndryshe refuzohet. `None` vetëm për teste dhe matje.
    pub genesis: Option<Hash>,
}

impl Default for PageOptions {
    fn default() -> Self {
        PageOptions {
            history: DEFAULT_HISTORY,
            cache_depth: CACHE_DEPTH,
            durable: true,
            sync_every: 1,
            ckpt_every: 256,
            ckpt_log_mb: 64,
            reuse_slack: 2,
            genesis: None,
        }
    }
}

/// Gjendja e hapësirës në skedar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageStats {
    /// Blloqe 4 KB në skedar.
    pub blocks: u64,
    pub free_blocks: u64,
    /// Bajte të faqeve të gjalla ose në pritje.
    pub live_bytes: u64,
    /// Blloqe gjysmë bosh që presin zbrazjen.
    pub sparse_blocks: u64,
}

/// Matjet e slotit të fundit sipas fazave (për `zilja-snapshot-bench`). Kohët janë mure-ore.
#[derive(Clone, Copy, Debug, Default)]
pub struct SlotTiming {
    /// Leximi paraprak i faqeve (nivel pas niveli, paralelisht) dhe i blloqeve për zhvendosje.
    pub read: std::time::Duration,
    /// Llogaritja e nyjeve dhe e hash-eve në RAM.
    pub tree: std::time::Duration,
    /// Ndërtimi dhe kodimi i faqeve të reja (pa `pwrite`/fsync).
    pub build: std::time::Duration,
    /// `pwrite` i blloqeve.
    pub pwrite: std::time::Duration,
    /// fsync i skedarit të faqeve.
    pub fsync: std::time::Duration,
    /// `commit_slot`: rekordi në log (me fsync-un e tij) dhe checkpoint-i, kur ka.
    pub commit: std::time::Duration,
    /// Faqe/blloqe të kërkuara nga disku (`pread`); nëse vijnë nga cache-i i sistemit s'dallohet këtu.
    pub disk_reads: u64,
    /// Faqe të shkruara.
    pub pages: usize,
    /// U shkrua `meta.ckpt` në këtë slot.
    pub checkpoint: bool,
}

/// Rezultati i `prepare_slot`: faqet janë shkruar tashmë në blloqe të lira; mbetet rekordi.
struct PagePrepared {
    slot: u64,
    rec: LogRec,
    pinned_new: Vec<(PageId, Arc<FPage>)>,
    t_pre: std::time::Duration,
    t_tree: std::time::Duration,
    t_write: std::time::Duration,
    /// Brenda shkrimit: ndërtimi + kodimi i faqeve, `pwrite`, fsync (vetëm për `ZILJA_PROFILE`).
    t_split: [std::time::Duration; 5],
    pages: usize,
    moved: usize,
}

/// Një faqe që rishkruhet në slot: rruga e rrënjës dhe nyjet që ndryshojnë (vendi → nyja e re).
type DirtyPage = (NPath, Vec<(usize, Option<Node>)>);

/// Faqja e versionit të fundit në një rrugë: `None` = s'ka faqe aty.
type WorkPage = Option<(PageId, Arc<FPage>)>;
/// Faqja e ndërtuar në `prepare_slot`: numri i vjetër (për t'u liruar) dhe faqja e re me bajtet e
/// koduara (`None` = faqja u zbraz).
type BuiltPage = (Option<PageId>, Option<(FPage, Vec<u8>)>);

pub struct PageTree {
    dir: PathBuf,
    /// P-01: lock ekskluziv mbi `LOCK`, i marrë para çdo recovery, wipe ose shkrimi dhe i mbajtur
    /// gjatë gjithë jetës së instancës. Rindërtimi s'e fshin kurrë këtë skedar.
    _lock: File,
    data: File,
    log: File,
    /// Atomik, që checkpoint-i të bëhet edhe me `&self` (`maintenance`, nën kyçin e leximit).
    log_len: AtomicU64,
    o: PageOptions,
    state: Option<State>,
    root_id: Option<PageId>,
    ring: Ring,
    /// Sloti nga i cili kujtesa është e plotë (hapja ose snapshot-i i fundit).
    cache_since: u64,
    alloc: Mutex<Alloc>,
    /// Faqet e sipërme të versionit të fundit, sipas numrit.
    pinned: HashMap<PageId, Arc<FPage>>,
    /// Maja e blloqeve në RAM gjatë ndërtimit të fundit nga snapshot-i (matje, `snapshot_peak_blocks`).
    snapshot_peak_blocks: usize,
    /// Faqet e lexuara për slotin që po zbatohet, sipas rrugës së rrënjës.
    work: RwLock<HashMap<PKey, WorkPage>>,
    records_since_ckpt: AtomicU64,
    /// Checkpoint-i shtyhet nga `commit_slot` te `maintenance` (jashtë kyçit të shkrimit).
    defer_ckpt: bool,
    /// Një checkpoint është i nevojshëm dhe pret `maintenance`.
    ckpt_due: AtomicBool,
    /// Numri rendor i rekordit të fundit të shkruar.
    rec_seq: u64,
    commits: u64,
    /// Blloqe të shkruara në slotin e fundit (masë për sa duhet zbrazur).
    last_blocks: AtomicU64,
    /// Blloqe të zbrazura së fundi → rekordi: faqet e tyre të vjetra lirohen vetëm pas kujtesës, ndaj
    /// s'ka kuptim t'i lexojmë sërish deri atëherë.
    drained: Mutex<HashMap<u64, u64>>,
    seq: u64,
    stage_buf: HashMap<u16, Vec<u8>>,
    /// A26: për çdo kovë të staging-ut, numri i rekordeve dhe SHA-256 i bajteve të shkruara.
    stage_sums: HashMap<u16, (u64, sha2::Sha256)>,
    staged: u64,
    profile: bool,
    /// Matje: faqe të lexuara nga disku në slotin e fundit.
    pub last_disk_reads: AtomicU64,
    /// Matje: bajte të shkruara në skedarin e faqeve gjithsej.
    pub page_bytes_written: AtomicU64,
    last_timing: Option<SlotTiming>,
    fault_every: AtomicU64,
    fault_count: AtomicU64,
}

fn io<E: std::fmt::Display>(what: &str) -> impl Fn(E) -> String + '_ {
    move |e| format!("{what}: {e}")
}

impl PageTree {
    pub fn open(dir: impl AsRef<Path>, o: PageOptions) -> Result<Self, String> {
        let dir = dir.as_ref().to_path_buf();
        if o.durable && o.sync_every != 1 {
            // P-03: release-i fillestar mbështet vetëm sync_every = 1. Ripërdorimi i blloqeve supozon
            // që çdo rekord i shkruar është në disk para se të shkruhet tjetri.
            return Err(format!("motori me faqe kërkon sync_every = 1 (u dha {}); mënyra asinkrone s'mbështetet ende", o.sync_every));
        }
        std::fs::create_dir_all(&dir).map_err(io("krijimi i dosjes"))?;
        // A21: një dosje e motorit të vjetër RocksDB s'hapet si "pages" (edhe skedari LOCK ka të
        // njëjtin emër): asgjë s'krijohet dhe asgjë s'ndryshohet.
        if let Some(m) = rocksdb_marker(&dir) {
            return Err(format!(
                "{} është dosje e motorit të vjetër RocksDB (u gjet {m}); \"pages\" s'e hap dhe s'e prek. \
                 Zhvendose dhe lëre zilen ta ndërtojë nga snapshot-i (docs/OPERIMI.md, A21)",
                dir.display()
            ));
        }
        let lock = lock_exclusive(&dir.join("LOCK"))?;
        check_manifest(&dir, &o)?;
        match std::fs::read(dir.join("INVALID")) {
            Ok(why) => {
                // F-13: gjendja u shënua e pavlefshme në nisjen e kaluar: rindërtim i pastër.
                log::warn!("zilja-pages: pema u shënua e pavlefshme ({}); po rindërtohet nga e para", String::from_utf8_lossy(&why));
                wipe(&dir)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            // A06: shënuesi është vendimi i instancës së kaluar që pema s'përdoret. Kur s'dihet nëse
            // ekziston (leje, I/O), pema s'hapet dhe asgjë s'ndryshohet.
            Err(e) => return Err(corrupt(&dir, &format!("shënuesi INVALID s'lexohet ({e}): s'dihet nëse pema është e vlefshme"))),
        }
        let open = |name: &str| OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join(name));
        let data = open("pages.dat").map_err(io("hapja e faqeve"))?;
        let log = open("meta.log").map_err(io("hapja e log-ut"))?;
        if o.durable {
            // Skedarët e sapokrijuar duhet të mbijetojnë një ikje rryme.
            sync_dir(&dir)?;
        }
        let mut t = PageTree {
            dir,
            _lock: lock,
            data,
            log,
            log_len: AtomicU64::new(0),
            o: o.clone(),
            state: None,
            root_id: None,
            ring: VecDeque::new(),
            cache_since: 0,
            alloc: Mutex::new(Alloc::default()),
            pinned: HashMap::default(),
            snapshot_peak_blocks: 0,
            work: RwLock::new(HashMap::default()),
            records_since_ckpt: AtomicU64::new(0),
            defer_ckpt: false,
            ckpt_due: AtomicBool::new(false),
            rec_seq: 0,
            commits: 0,
            last_blocks: AtomicU64::new(0),
            drained: Mutex::new(HashMap::default()),
            seq: 0,
            stage_buf: HashMap::default(),
            stage_sums: HashMap::default(),
            staged: 0,
            profile: std::env::var_os("ZILJA_PROFILE").is_some(),
            last_disk_reads: AtomicU64::new(0),
            page_bytes_written: AtomicU64::new(0),
            last_timing: None,
            fault_every: AtomicU64::new(0),
            fault_count: AtomicU64::new(0),
        };
        t.recover()?;
        match t.state.clone() {
            // Pa checkpoint: asnjë snapshot s'u mbyll. A05: kjo lejohet vetëm për një dosje bosh, ose
            // kur shënuesi BUILDING tregon që një ndërtim nga snapshot-i u ndërpre. Faqe ose staging pa
            // shënues = gjendje me prejardhje të paqartë (p.sh. checkpoint-i humbi): s'fshihet asgjë.
            None => {
                let building = t.dir.join(BUILDING).exists();
                let pages = t.data.metadata().map_err(io("faqet"))?.len();
                let staging = t.dir.join("staging").exists();
                if !building && (pages > 0 || staging) {
                    return Err(corrupt(
                        &t.dir,
                        &format!("mungon meta.ckpt, por ka {} (pa shënuesin {BUILDING} të ndërtimit të parë)", if pages > 0 { format!("{pages} B faqe") } else { "staging".into() }),
                    ));
                }
                t.clear()?;
            }
            Some(st) => {
                // Thellësia e cache-së është e diskut, jo e opsioneve.
                t.o.cache_depth = st.cache_depth;
                // P-02: faqe që s'përputhen me checkpoint-in/log-un = dëmtim. Asgjë s'fshihet.
                t.load_pinned().map_err(|e| corrupt(&t.dir, &format!("faqet nuk përputhen me gjendjen e ruajtur: {e}")))?;
                t.cache_since = st.version;
            }
        }
        Ok(t)
    }

    /// Checkpoint-i + rekordet pas tij (P-02).
    ///
    /// - Çdo rekord duhet të jetë i plotë, me CRC të saktë, i formatit të njohur, dhe në rend:
    ///   numrat rendorë rriten rreptësisht; i pari pas checkpoint-it është `seq + 1`, pastaj pa
    ///   boshllëqe; sloti rritet rreptësisht.
    /// - I vetmi rast i toleruar është bishti pas rekordit të fundit të vlefshëm që shtrihet deri në
    ///   fund të skedarit (shih [`Frame::Torn`]). Me `sync_every = 1` ai është gjithmonë një slot që s'u
    ///   konfirmua kurrë (commit-i kthehet vetëm pas fsync-ut të rekordit). Bishti ruhet në
    ///   `meta.log.tail-<offset>` dhe hiqet nga log-u.
    /// - Çdo gjë tjetër ndalon hapjen me gabim, pa ndryshuar asnjë skedar.
    fn recover(&mut self) -> Result<(), String> {
        let mut alloc = Alloc::default();
        let mut have_ckpt = false;
        if let Some(b) = read_budget(&self.dir.join("meta.ckpt"), MAX_CKPT, "checkpoint-i")? {
            let c = match parse_frame(&b, CKPT_MAGIC, MAX_CKPT) {
                Frame::Ok(p, n) if n == b.len() => Ckpt::dec_payload(p).ok_or_else(|| corrupt(&self.dir, "checkpoint-i nuk dekodohet"))?,
                Frame::Ok(..) => return Err(corrupt(&self.dir, "checkpoint-i ka bajte pas kornizës")),
                Frame::Torn(w) => return Err(corrupt(&self.dir, &format!("checkpoint-i është i papërfunduar ({w})"))),
                Frame::Bad(w) => return Err(corrupt(&self.dir, &format!("checkpoint-i: {w}"))),
            };
            self.state = Some(c.st);
            self.root_id = c.root_id;
            self.ring = c.ring;
            self.rec_seq = c.seq;
            alloc = c.alloc;
            have_ckpt = true;
        }
        let budget = MIN_LOG_BUDGET.max(2 * (self.o.ckpt_log_mb << 20));
        let lp = self.dir.join("meta.log");
        let log = read_budget(&lp, budget, "log-u")?.unwrap_or_default();
        let mut pos = 0usize;
        let mut last_seq: Option<u64> = None;
        let mut tail: Option<&'static str> = None;
        while pos < log.len() {
            let at = |w: &str| corrupt(&self.dir, &format!("meta.log, bajti {pos}: {w}"));
            let (body, n) = match parse_frame(&log[pos..], LOG_MAGIC, MAX_RECORD) {
                Frame::Ok(b, n) => (b, n),
                Frame::Torn(w) => {
                    tail = Some(w);
                    break;
                }
                Frame::Bad(w) => return Err(at(&w)),
            };
            let rec = LogRec::dec_payload(body).ok_or_else(|| at("rekord që nuk dekodohet"))?;
            if last_seq.is_some_and(|l| rec.seq <= l) {
                return Err(at(&format!("rend i gabuar: rekordi {} pas {}", rec.seq, last_seq.unwrap())));
            }
            last_seq = Some(rec.seq);
            // Rekordet deri te checkpoint-i janë përfshirë tashmë në të (rrëzim mes rename-it të
            // checkpoint-it dhe zbrazjes së log-ut).
            if rec.seq > self.rec_seq {
                if !have_ckpt {
                    return Err(at("rekord log-u pa checkpoint"));
                }
                if rec.seq != self.rec_seq + 1 {
                    return Err(at(&format!("rend i gabuar: pritej rekordi {}, u gjet {}", self.rec_seq + 1, rec.seq)));
                }
                let prev = self.state.as_ref().map(|s| s.version);
                if prev.is_some_and(|v| rec.st.version <= v) {
                    return Err(at(&format!("rend i gabuar: sloti {} pas slotit {}", rec.st.version, prev.unwrap())));
                }
                apply_rec(&mut alloc, &mut self.ring, prev, &rec);
                self.rec_seq = rec.seq;
                self.root_id = rec.root_id;
                self.state = Some(rec.st);
                *self.records_since_ckpt.get_mut() += 1;
            }
            pos += n;
        }
        if let Some(w) = tail {
            // Bishti ruhet për diagnozë, pastaj hiqet, që rekordet e reja të vijnë pas një rekordi të plotë.
            let keep = self.dir.join(format!("meta.log.tail-{pos}"));
            std::fs::write(&keep, &log[pos..]).map_err(io("ruajtja e bishtit të log-ut"))?;
            log::warn!("zilja-pages: u hoq bishti i papërfunduar i log-ut ({w}, {} B nga bajti {pos}); u ruajt në {}", log.len() - pos, keep.display());
            self.log.set_len(pos as u64).map_err(io("shkurtimi i log-ut"))?;
            if self.o.durable {
                self.log.sync_all().map_err(io("fsync i log-ut"))?;
                sync_dir(&self.dir)?;
            }
        }
        *self.log_len.get_mut() = pos as u64;
        self.alloc = Mutex::new(alloc);
        Ok(())
    }

    /// Ngarkon faqet e sipërme të versionit të fundit në RAM dhe kontrollon hash-et nga root-i.
    fn load_pinned(&mut self) -> Result<(), String> {
        self.pinned.clear();
        let st = self.state.clone().ok_or("pa gjendje")?;
        let Some(rid) = self.root_id else {
            return if st.root.kind == Kind::Empty { Ok(()) } else { Err("mungon faqja rrënjë".into()) };
        };
        let cd = self.o.cache_depth;
        let mut level = vec![(rid, st.root, NPath::root())];
        while !level.is_empty() {
            let mut next = Vec::new();
            for (id, rf, path) in level {
                let pg = self.read_page_disk(id)?;
                check_page(&pg, rf, &path)?;
                if path.depth + PAGE_H < cd {
                    for (j, k) in pg.kids.iter().enumerate() {
                        if let Some(kid) = k {
                            next.push((*kid, kid_ref(&pg, j)?, kid_path(&path, j)));
                        }
                    }
                }
                self.pinned.insert(id, Arc::new(pg));
            }
            level = next;
        }
        Ok(())
    }

    /// Vetëm për teste: çdo lexim i `n`-të i një faqeje nga disku dështon (0 = fikur).
    #[doc(hidden)]
    pub fn inject_read_faults(&self, n: u64) {
        self.fault_every.store(n, Ordering::Relaxed);
        self.fault_count.store(0, Ordering::Relaxed);
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }
    pub fn version(&self) -> Option<u64> {
        self.state.as_ref().map(|s| s.version)
    }
    pub fn cache_depth(&self) -> u16 {
        self.o.cache_depth
    }
    pub fn cached_pages(&self) -> usize {
        self.pinned.len()
    }

    /// Sa blloqe 4 KB u mbajtën njëherësh në RAM gjatë ndërtimit të fundit nga snapshot-i
    /// (≤ `SNAPSHOT_FLUSH_BLOCKS` + 1, pavarësisht madhësisë së pemës).
    pub fn snapshot_peak_blocks(&self) -> usize {
        self.snapshot_peak_blocks
    }

    /// Matjet e slotit të fundit të bërë commit (`None` para slotit të parë).
    pub fn last_timing(&self) -> Option<SlotTiming> {
        self.last_timing
    }

    pub fn page_stats(&self) -> PageStats {
        let a = self.alloc.lock().unwrap();
        PageStats {
            blocks: a.next,
            free_blocks: a.free.len() as u64,
            live_bytes: a.live.iter().map(|v| *v as u64).sum(),
            sparse_blocks: a.cand.len() as u64,
        }
    }

    fn fault(&self) -> bool {
        let n = self.fault_every.load(Ordering::Relaxed);
        n > 0 && (self.fault_count.fetch_add(1, Ordering::Relaxed) + 1).is_multiple_of(n)
    }

    fn read_page_disk(&self, id: PageId) -> Result<FPage, String> {
        self.last_disk_reads.fetch_add(1, Ordering::Relaxed);
        if self.fault() {
            return Err("faqja s'lexohet nga disku (gabim i injektuar)".into());
        }
        let len = pid_len(id);
        if len < PAGE_HDR || pid_off(id) + len > BLOCK {
            return Err(format!("numër faqeje i pavlefshëm {id:#x}"));
        }
        let mut buf = vec![0u8; len];
        self.data
            .read_exact_at(&mut buf, pid_block(id) * BLOCK as u64 + pid_off(id) as u64)
            .map_err(|e| format!("leximi i faqes {id:#x}: {e}"))?;
        FPage::dec(&buf).ok_or_else(|| format!("faqja {id:#x} është e dëmtuar"))
    }

    fn load(&self, id: PageId) -> Result<Arc<FPage>, String> {
        if let Some(p) = self.pinned.get(&id) {
            return Ok(p.clone());
        }
        Ok(Arc::new(self.read_page_disk(id)?))
    }

    /// Faqja e versionit të fundit me rrënjë në rrugën `root` (thellësi shumëfish i 4).
    fn page_at_path(&self, root: &NPath) -> Result<WorkPage, String> {
        let key = root.enc();
        if let Some(hit) = self.work.read().unwrap().get(&key) {
            return Ok(hit.clone());
        }
        let id = if root.depth == 0 {
            self.root_id
        } else {
            match self.page_at_path(&root.truncate(root.depth - PAGE_H))? {
                None => None,
                Some((_, parent)) => parent.kids[kid_index(root)],
            }
        };
        let res = match id {
            None => None,
            Some(id) => {
                let pg = self.load(id)?;
                if pg.path != *root {
                    return Err(format!("faqja {id:#x} nuk është ajo e rrugës së pritur"));
                }
                Some((id, pg))
            }
        };
        self.work.write().unwrap().insert(key, res.clone());
        Ok(res)
    }

    /// Lexon paraprakisht, rresht pas rreshti dhe paralelisht, faqet që do t'i duhen zbatimit.
    fn prefetch(&self, ups: &[Update]) {
        let mut frontier: Vec<(NPath, PageId)> = self.root_id.map(|id| (NPath::root(), id)).into_iter().collect();
        while !frontier.is_empty() {
            use rayon::prelude::*;
            let loaded: Vec<(NPath, PageId, Result<Arc<FPage>, String>)> =
                frontier.into_par_iter().map(|(p, id)| (p, id, self.load(id))).collect();
            let mut next = Vec::new();
            let mut w = self.work.write().unwrap();
            for (root, id, pg) in loaded {
                // `page_at_path` e riprovon dhe e kthen gabimin.
                let Ok(pg) = pg else { continue };
                if pg.path != root {
                    continue;
                }
                let lo = ups.partition_point(|(k, _)| k < &root.bits);
                for (key, _) in ups[lo..].iter().take_while(|(k, _)| root.contains(k)) {
                    let (mut idx, mut d) = (0usize, root.depth);
                    while let Some(Node::Internal { l, r }) = &pg.nodes.slots[idx] {
                        let b = bit(key, d as usize);
                        if (if b == 0 { l } else { r }).kind == Kind::Empty {
                            break;
                        }
                        if d + 1 == root.depth + PAGE_H {
                            let j = 2 * (idx - 7) + b as usize;
                            if let Some(kid) = pg.kids[j] {
                                let cp = kid_path(&root, j);
                                if next.last().map(|x: &(NPath, PageId)| x.0) != Some(cp) {
                                    next.push((cp, kid));
                                }
                            }
                            break;
                        }
                        idx = 2 * idx + 1 + b as usize;
                        d += 1;
                    }
                }
                w.insert(root.enc(), Some((id, pg)));
            }
            frontier = next;
        }
    }

    /// Mbledhja e hapësirës: faqet e gjalla në blloqet më bosh, që të rishkruhen në këtë slot.
    /// Një faqe është e gjallë kur rruga e saj, në versionin e fundit, të çon pikërisht te ajo.
    fn pick_relocations(&self) -> Vec<NPath> {
        let budget = {
            let a = self.alloc.lock().unwrap();
            let used = a.next.saturating_sub(a.free.len() as u64);
            if used == 0 || a.cand.is_empty() {
                return Vec::new();
            }
            let live: u64 = a.live.iter().map(|v| *v as u64).sum();
            // Sa më shumë hapësirë e humbur, aq më shumë zbrazje, në raport me blloqet që shkruajnë
            // vetë ndryshimet e slotit (jo zhvendosjet: përndryshe zbrazja ushqen veten dhe vjen me shpërthime).
            let fill = live as f64 / (used * BLOCK as u64) as f64;
            let base = self.last_blocks.load(Ordering::Relaxed).max(8) as usize;
            let n = if fill < 0.5 {
                base / 2
            } else if fill < 0.65 {
                base / 4
            } else if fill < 0.75 {
                base / 16
            } else {
                0
            };
            if n == 0 {
                return Vec::new();
            }
            let q = self.rec_seq + 1;
            let window = self.o.history + self.o.reuse_slack + 2;
            let mut d = self.drained.lock().unwrap();
            d.retain(|_, at| *at + window > q);
            let picked: Vec<u64> = a.cand.iter().map(|(_, b)| *b).filter(|b| !d.contains_key(b)).take(n.max(1)).collect();
            for b in &picked {
                d.insert(*b, q);
            }
            picked
        };
        let mut out = Vec::new();
        let mut buf = vec![0u8; BLOCK];
        for b in budget {
            self.last_disk_reads.fetch_add(1, Ordering::Relaxed);
            if self.data.read_exact_at(&mut buf, b * BLOCK as u64).is_err() {
                continue;
            }
            let mut off = 0usize;
            while off + PAGE_HDR <= BLOCK {
                let len = u16::from_le_bytes([buf[off + 4], buf[off + 5]]) as usize;
                if len == 0 || off + PAGE_HDR + len > BLOCK {
                    break;
                }
                let n = PAGE_HDR + len;
                if let Some(pg) = FPage::dec(&buf[off..off + n]) {
                    let id = pid(b, off, n);
                    if let Ok(Some((cur, _))) = self.page_at_path(&pg.path) {
                        if cur == id {
                            out.push(pg.path);
                        }
                    }
                }
                off += n;
            }
        }
        out
    }

    fn sync_data(&self) -> Result<(), String> {
        self.data.sync_data().map_err(io("fsync i faqeve"))
    }

    fn append_log(&mut self, rec: &LogRec, sync: bool) -> Result<(), String> {
        let b = rec.framed()?;
        self.log.seek(SeekFrom::Start(*self.log_len.get_mut())).map_err(|e| e.to_string())?;
        self.log.write_all(&b).map_err(io("shkrimi i log-ut"))?;
        if sync {
            self.log.sync_data().map_err(io("fsync i log-ut"))?;
        }
        *self.log_len.get_mut() += b.len() as u64;
        Ok(())
    }

    /// Gjendja e plotë në `meta.ckpt` (atomike: skedar i përkohshëm + rename), pastaj log-u zbrazet.
    fn checkpoint(&self) -> Result<(), String> {
        let Some(st) = self.state.clone() else {
            return Ok(());
        };
        let c = Ckpt {
            history: self.o.history,
            slack: self.o.reuse_slack,
            seq: self.rec_seq,
            st,
            root_id: self.root_id,
            alloc: self.alloc.lock().unwrap().clone(),
            ring: self.ring.clone(),
        };
        if self.o.durable {
            self.sync_data()?;
            self.log.sync_data().map_err(io("fsync i log-ut"))?;
        }
        let tmp = self.dir.join("meta.ckpt.tmp");
        {
            let mut f = File::create(&tmp).map_err(io("krijimi i checkpoint-it"))?;
            // G18: si te log-u, një checkpoint që hapja s'do ta pranonte s'shkruhet.
            let payload = c.payload();
            if payload.len() as u64 > MAX_CKPT {
                return Err(format!("checkpoint-i ka {} B, mbi kufirin {MAX_CKPT} B", payload.len()));
            }
            f.write_all(&frame(CKPT_MAGIC, &payload)).map_err(io("shkrimi i checkpoint-it"))?;
            if self.o.durable {
                f.sync_all().map_err(io("fsync i checkpoint-it"))?;
            }
        }
        std::fs::rename(&tmp, self.dir.join("meta.ckpt")).map_err(io("rename i checkpoint-it"))?;
        if self.o.durable {
            File::open(&self.dir).and_then(|d| d.sync_all()).map_err(io("fsync i dosjes"))?;
        }
        // Rekordet e vjetra kapërcehen nga numri rendor edhe nëse kjo ndërpritet këtu.
        self.log.set_len(0).map_err(io("zbrazja e log-ut"))?;
        self.log_len.store(0, Ordering::Relaxed);
        self.records_since_ckpt.store(0, Ordering::Relaxed);
        self.ckpt_due.store(false, Ordering::Relaxed);
        Ok(())
    }

    /// Fshin gjithçka.
    pub fn clear(&mut self) -> Result<(), String> {
        self.data.set_len(0).map_err(io("fshirja e faqeve"))?;
        self.log.set_len(0).map_err(io("fshirja e log-ut"))?;
        *self.log_len.get_mut() = 0;
        remove_file(&self.dir.join("meta.ckpt"))?;
        remove_dir_all(&self.dir.join("staging"))?;
        *self.alloc.get_mut().unwrap() = Alloc::default();
        self.pinned.clear();
        self.work.get_mut().unwrap().clear();
        self.ring.clear();
        self.state = None;
        self.root_id = None;
        self.staged = 0;
        self.stage_buf.clear();
        self.stage_sums.clear();
        *self.records_since_ckpt.get_mut() = 0;
        *self.ckpt_due.get_mut() = false;
        self.rec_seq = 0;
        Ok(())
    }

    /// Kontroll i plotë: çdo faqe nga root-i, çdo hash kundër prindit, çdo rrugë, root-i nga gjethet,
    /// numri i llogarive, dhe bajtet e gjalla të alokuesit (asnjë faqe e gjallë në një bllok "të lirë").
    pub fn verify_integrity(&self) -> Result<(), String> {
        let st = self.state.as_ref().ok_or("pa gjendje")?;
        let mut accs = Vec::new();
        let mut per_block: HashMap<u64, u64> = HashMap::default();
        if let Some(rid) = self.root_id {
            let mut stack = vec![(rid, st.root, NPath::root())];
            while let Some((id, rf, path)) = stack.pop() {
                let pg = self.read_page_disk(id)?;
                check_page(&pg, rf, &path)?;
                *per_block.entry(pid_block(id)).or_default() += pid_len(id) as u64;
                for n in pg.nodes.slots.iter().flatten() {
                    if let Node::Leaf(d) = n {
                        accs.push((d.key, d.meta.clone()));
                    }
                }
                for (j, k) in pg.kids.iter().enumerate() {
                    if let Some(kid) = k {
                        stack.push((*kid, kid_ref(&pg, j)?, kid_path(&path, j)));
                    }
                }
            }
        } else if st.root.kind != Kind::Empty {
            return Err("mungon faqja rrënjë".into());
        }
        let expect = zilja_tree::reference_root(&accs);
        if expect != st.root.hash {
            return Err(format!("root-i i ruajtur {} != root-i nga gjethet {}", hx(&st.root.hash), hx(&expect)));
        }
        if accs.len() as u64 != st.len {
            return Err(format!("numri i llogarive {} != {}", accs.len(), st.len));
        }
        let a = self.alloc.lock().unwrap();
        for (b, n) in per_block {
            let live = *a.live.get(b as usize).ok_or("faqe përtej skedarit")? as u64;
            if live < n {
                return Err(format!("blloku {b}: {n} bajte të gjalla, alokuesi di vetëm {live}"));
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ shkrimi

    /// Shkruan blloqe të plota (mbushur me zero deri në 4 KB), paralelisht.
    fn write_blocks(&self, blocks: &[(u64, Vec<u8>)]) -> Result<(), String> {
        use rayon::prelude::*;
        blocks.par_iter().try_for_each(|(b, buf)| {
            let mut full = buf.clone();
            full.resize(BLOCK, 0);
            self.data.write_all_at(&full, b * BLOCK as u64).map_err(|e| format!("shkrimi i bllokut {b}: {e}"))
        })?;
        self.page_bytes_written.fetch_add((blocks.len() * BLOCK) as u64, Ordering::Relaxed);
        Ok(())
    }

    /// Snapshot: shkruan nënpemën `n` me rrënjë në `path` (kufi faqeje) dhe faqet poshtë saj.
    fn write_mtree(&self, n: &MNode, path: NPath, pk: &Mutex<Packer>) -> Result<PageId, String> {
        if let MNode::Stub(_, id) = n {
            return Ok(*id);
        }
        let mut pg = FPage::new(path);
        let mut kids: Vec<(usize, &MNode)> = Vec::new();
        fn fill<'a>(n: &'a MNode, idx: usize, pg: &mut FPage, kids: &mut Vec<(usize, &'a MNode)>) -> Result<(), String> {
            match n {
                MNode::Leaf(node) => pg.nodes.slots[idx] = Some(node.clone()),
                MNode::Stub(..) => return Err("nënpemë e shkruar brenda një faqeje".into()),
                MNode::Int(l, r, lr, rr) => {
                    pg.nodes.slots[idx] = Some(Node::Internal { l: *lr, r: *rr });
                    for (b, c) in [(0usize, l), (1, r)] {
                        if let Some(c) = c {
                            if idx >= 7 {
                                kids.push((2 * (idx - 7) + b, c));
                            } else {
                                fill(c, 2 * idx + 1 + b, pg, kids)?;
                            }
                        }
                    }
                }
            }
            Ok(())
        }
        fill(n, 0, &mut pg, &mut kids)?;
        let ids: Result<Vec<(usize, PageId)>, String> = if kids.len() > 1 {
            use rayon::prelude::*;
            kids.par_iter().map(|(j, c)| Ok((*j, self.write_mtree(c, kid_path(&path, *j), pk)?))).collect()
        } else {
            kids.iter().map(|(j, c)| Ok((*j, self.write_mtree(c, kid_path(&path, *j), pk)?))).collect()
        };
        for (j, id) in ids? {
            pg.kids[j] = Some(id);
        }
        let bytes = pg.enc();
        let mut p = pk.lock().unwrap();
        let id = p.place(&self.alloc, &bytes);
        // Blloqet e mbushura shkruhen herë pas here, që RAM-i të mos rritet me snapshot-in.
        if p.blocks.len() > SNAPSHOT_FLUSH_BLOCKS {
            drop(p);
            self.flush_full_blocks(pk)?;
        }
        Ok(id)
    }

    /// Ndërtimi nga snapshot-i: shkruan në disk të gjitha blloqet e mbyllura të `pk` (mbetet vetëm i
    /// fundit, që mund të mos jetë plot), shënon bajtet e gjalla te alokuesi dhe i liron nga RAM-i.
    /// Edhe lista `taken` zbrazet: në këtë rrugë s'ka kthim mbrapsht blloqesh (një gabim e rindërton
    /// gjithë skedarin), dhe për Mainnet ajo do të rritej me ~8 B për çdo bllok (~0.5 GB).
    fn flush_full_blocks(&self, pk: &Mutex<Packer>) -> Result<(), String> {
        let full = {
            let mut p = pk.lock().unwrap();
            if p.blocks.len() <= 1 {
                return Ok(());
            }
            let keep = p.blocks.pop().unwrap();
            let full = std::mem::take(&mut p.blocks);
            p.blocks.push(keep);
            p.taken.clear();
            full
        };
        {
            let mut a = self.alloc.lock().unwrap();
            for (b, buf) in &full {
                a.set_live(*b, buf.len() as u16);
            }
        }
        self.write_blocks(&full)
    }

    /// A05: shënon që po ndërtohet nga snapshot-i (para çdo hapi shkatërrues).
    fn mark_building(&mut self) -> Result<(), String> {
        let p = self.dir.join(BUILDING);
        if p.exists() {
            return Ok(());
        }
        let mut f = File::create(&p).map_err(io("krijimi i BUILDING"))?;
        f.write_all(b"ndertim nga snapshot-i ne vazhdim\n").map_err(io("shkrimi i BUILDING"))?;
        if self.o.durable {
            f.sync_all().map_err(io("fsync i BUILDING"))?;
            sync_dir(&self.dir)?;
        }
        Ok(())
    }

    /// Vetëm për teste: shkruan në disk llogaritë e snapshot-it që janë ende në RAM.
    #[doc(hidden)]
    pub fn flush_staging_for_test(&mut self) -> Result<(), String> {
        self.flush_staging()
    }

    fn flush_staging(&mut self) -> Result<(), String> {
        let dir = self.dir.join("staging");
        std::fs::create_dir_all(&dir).map_err(io("dosja e staging-ut"))?;
        for (b, buf) in self.stage_buf.drain() {
            let mut f = OpenOptions::new().create(true).append(true).open(dir.join(format!("b{b:03x}"))).map_err(io("staging"))?;
            f.write_all(&buf).map_err(io("shkrimi i snapshot-it"))?;
        }
        Ok(())
    }

    /// Ndërton një kovë (thellësia 8) nga skedari i saj: fiton sloti më i ri, pastaj i fundit (F-02).
    fn build_bucket(&self, b: u16, pk: &Mutex<Packer>) -> Result<(Option<Box<MNode>>, u64), String> {
        let path = self.dir.join("staging").join(format!("b{b:03x}"));
        // A26: kova duhet të ketë saktësisht rekordet që u shkruan (numri dhe SHA-256), ndryshe pema
        // do të ndërtohej (dhe firmosej si e plotë) nga një input tjetër.
        let (want_n, want_sum) = match self.stage_sums.get(&b) {
            Some((n, h)) => (*n, Some(h.clone().finalize())),
            None => (0, None),
        };
        let raw = match std::fs::read(&path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && want_n == 0 => return Ok((None, 0)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(format!("kova {b:03x} e staging-ut mungon (priteshin {want_n} rekorde)")),
            Err(e) => return Err(format!("leximi i staging-ut: {e}")),
        };
        if raw.len() % STAGE_REC != 0 {
            return Err("staging i dëmtuar".into());
        }
        let got_n = (raw.len() / STAGE_REC) as u64;
        if got_n != want_n || want_sum.is_some_and(|w| sha2::Sha256::digest(&raw) != w) {
            return Err(format!("kova {b:03x} e staging-ut nuk përputhet me atë që u shkrua ({got_n} rekorde, priteshin {want_n}, ose përmbajtje tjetër)"));
        }
        let mut recs: Vec<(Key, u64, u64, Option<AccountMeta>)> = raw
            .chunks_exact(STAGE_REC)
            .map(|r| {
                let key: Key = r[..32].try_into().unwrap();
                let slot = u64::from_le_bytes(r[32..40].try_into().unwrap());
                let seq = u64::from_le_bytes(r[40..48].try_into().unwrap());
                let m = match r[48] {
                    0 => None,
                    _ => Some(dec_meta(&r[49..]).ok_or("meta e dëmtuar")?),
                };
                Ok((key, slot, seq, m))
            })
            .collect::<Result<_, String>>()?;
        drop(raw);
        recs.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        let mut live: Vec<(Key, AccountMeta)> = Vec::with_capacity(recs.len());
        for (i, (k, _, _, m)) in recs.iter().enumerate() {
            if recs.get(i + 1).is_none_or(|n| n.0 != *k) {
                if let Some(m) = m {
                    live.push((*k, m.clone()));
                }
            }
        }
        drop(recs);
        let n = live.len() as u64;
        let sub = mbuild(BUCKET_BITS as usize, &live);
        drop(live);
        remove_file(&path)?;
        let mut bits = [0u8; 32];
        bits[0] = (b << (8 - BUCKET_BITS)) as u8;
        let bpath = NPath { depth: BUCKET_BITS, bits };
        Ok((
            match sub {
                // Nënpema shkruhet tani; gjethja e vetme mund të ngjitet lart, ndaj mbetet në RAM.
                Some(s) if !matches!(*s, MNode::Leaf(_)) => {
                    let rf = s.rf();
                    Some(Box::new(MNode::Stub(rf, self.write_mtree(&s, bpath, pk)?)))
                }
                other => other,
            },
            n,
        ))
    }
}

/// Zbaton një rekord mbi alokuesin dhe kujtesën (rinisja dhe `prepare/commit` e bëjnë njësoj).
fn apply_rec(alloc: &mut Alloc, ring: &mut Ring, prev: Option<u64>, rec: &LogRec) {
    if let Some(p) = prev {
        alloc.release(p, rec.history, rec.seq, rec.slack);
    }
    for &lb in &rec.blocks {
        let b = alloc.take_block();
        alloc.set_live(b, lb);
    }
    alloc.retire(rec.st.version, rec.seq, rec.freed.clone());
    push_ring(ring, rec.st.version, rec.root_id, rec.st.root, rec.history);
}

/// Mban versionet e kujtesës, plus më të voglin që duhet ende për slotin `version - history`.
fn push_ring(ring: &mut Ring, version: u64, id: Option<PageId>, rf: Ref, history: u64) {
    ring.push_back((version, id, rf));
    let lo = version.saturating_sub(history);
    while ring.len() > 1 && ring[1].0 <= lo {
        ring.pop_front();
    }
}

/// Faqja duhet të jetë ajo e rrugës së pritur, nyja rrënjë me ref-in e prindit, fëmijët të përputhen.
fn check_page(pg: &FPage, rf: Ref, path: &NPath) -> Result<(), String> {
    if pg.path != *path {
        return Err("faqja nuk është ajo e rrugës së pritur".into());
    }
    if ref_of(&pg.nodes.slots[0]) != rf {
        return Err("faqja nuk përputhet me hash-in e prindit".into());
    }
    pg.check_kids()
}

/// Ref-i që faqja mban për fëmijën `j`.
fn kid_ref(pg: &FPage, j: usize) -> Result<Ref, String> {
    match &pg.nodes.slots[7 + j / 2] {
        Some(Node::Internal { l, r }) => Ok(if j.is_multiple_of(2) { *l } else { *r }),
        _ => Err("fëmijë pa nyje prind".into()),
    }
}

/// P-01: lock ekskluziv (flock) mbi `path`, pa pritur. Lirohet vetëm kur mbyllet skedari (fundi i
/// instancës ose i procesit). Mbron vetëm brenda një host-i: një sistem skedarësh i ndarë mes
/// makinave (NFS etj.) nuk garantohet.
pub(crate) fn lock_exclusive(path: &Path) -> Result<File, String> {
    let f = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).map_err(|e| format!("hapja e {}: {e}", path.display()))?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(std::fs::TryLockError::WouldBlock) => {
            Err(format!("{} është në përdorim nga një instancë tjetër (lock-u {} është i zënë); asgjë s'u ndryshua", path.parent().unwrap_or(path).display(), path.display()))
        }
        Err(std::fs::TryLockError::Error(e)) => Err(format!("lock-u {}: {e}", path.display())),
    }
}

/// P-05: versioni i formatit të dosjes (blloqe 4 KB, `ZLR3`/`ZCK3`). Ndryshon kur ndryshon formati.
pub const FORMAT_VERSION: u64 = 1;
const FORMAT_NAME: &str = "zilja-pages";

/// P-05: `MANIFEST` lidh dosjen me formatin dhe me genesis-in.
///
/// - Dosje RocksDB → refuzohet (s'hapet kurrë si instalim i ri pages).
/// - `MANIFEST` ekziston → formati, versioni dhe genesis-i duhet të përputhen.
/// - S'ekziston, por ka të dhëna pages → refuzohet (s'dihet për cilin rrjet janë).
/// - Dosje bosh → krijohet.
fn check_manifest(dir: &Path, o: &PageOptions) -> Result<(), String> {
    let path = dir.join("MANIFEST");
    let rocks = std::fs::read_dir(dir).map_err(io("leximi i dosjes"))?.filter_map(Result::ok).any(|e| {
        let n = e.file_name().to_string_lossy().into_owned();
        n == "CURRENT" || n == "IDENTITY" || n.starts_with("OPTIONS-") || n.ends_with(".sst")
    });
    if rocks {
        return Err(format!(
            "{} është një dosje RocksDB (motori i vjetër), jo e motorit me faqe; s'hapet si instalim i ri. \
             Vendos një db_path tjetër, ose zhvendose dosjen që zilja të ndërtohet nga snapshot-i",
            dir.display()
        ));
    }
    let g = |h: &Option<Hash>| h.map_or("asnjë".to_string(), |h| bs58::encode(h).into_string());
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{} s'lexohet: {e}", path.display()))?;
            if v["format"] != FORMAT_NAME {
                return Err(format!("{}: format i panjohur {}", path.display(), v["format"]));
            }
            if v["format_version"].as_u64() != Some(FORMAT_VERSION) {
                return Err(format!(
                    "{}: format_version {} s'mbështetet nga ky version (pritej {FORMAT_VERSION}); shih docs/OPERIMI.md për migrimin",
                    path.display(),
                    v["format_version"]
                ));
            }
            let stored = match &v["genesis"] {
                serde_json::Value::Null => None,
                serde_json::Value::String(s) => Some(
                    bs58::decode(s).into_vec().ok().and_then(|b| Hash::try_from(b).ok()).ok_or_else(|| format!("{}: genesis i pavlefshëm", path.display()))?,
                ),
                _ => return Err(format!("{}: genesis i pavlefshëm", path.display())),
            };
            if stored != o.genesis {
                return Err(format!(
                    "pema në {} është për genesis {}, por konfigurimi jep genesis {}; s'hapet (rrjet tjetër)",
                    dir.display(),
                    g(&stored),
                    g(&o.genesis)
                ));
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let has_data = ["pages.dat", "meta.log", "meta.ckpt"].iter().any(|n| std::fs::metadata(dir.join(n)).is_ok_and(|m| m.len() > 0));
            if has_data {
                return Err(format!(
                    "{} ka të dhëna pages por s'ka MANIFEST (version i vjetër): s'dihet për cilin rrjet janë. Zhvendose dosjen që zilja të ndërtohet nga snapshot-i",
                    dir.display()
                ));
            }
            let body = serde_json::json!({ "format": FORMAT_NAME, "format_version": FORMAT_VERSION, "genesis": o.genesis.map(|h| bs58::encode(h).into_string()) });
            let tmp = dir.join("MANIFEST.tmp");
            let mut f = File::create(&tmp).map_err(io("krijimi i MANIFEST"))?;
            f.write_all(serde_json::to_string_pretty(&body).unwrap().as_bytes()).map_err(io("shkrimi i MANIFEST"))?;
            if o.durable {
                f.sync_all().map_err(io("fsync i MANIFEST"))?;
            }
            std::fs::rename(&tmp, &path).map_err(io("rename i MANIFEST"))?;
            if o.durable {
                sync_dir(dir)?;
            }
            Ok(())
        }
        Err(e) => Err(format!("leximi i {}: {e}", path.display())),
    }
}

/// A21: shenjat e një dosjeje RocksDB.
fn rocksdb_marker(dir: &Path) -> Option<String> {
    let rd = std::fs::read_dir(dir).ok()?;
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n == "CURRENT" || n == "IDENTITY" || n.starts_with("OPTIONS-") || n.ends_with(".sst") || (n.starts_with("MANIFEST-") && n != "MANIFEST") {
            return Some(n);
        }
    }
    None
}

/// Fshin skedarët e pemës (jo `LOCK` dhe jo `MANIFEST`). Thirret vetëm nga mbajtësi i lock-ut.
fn wipe(dir: &Path) -> Result<(), String> {
    // `INVALID` fshihet i fundit: nëse ndërpritet, nisja e ardhshme e provon sërish.
    for name in ["pages.dat", "meta.log", "meta.ckpt", "meta.ckpt.tmp", BUILDING, "INVALID"] {
        remove_file(&dir.join(name))?;
    }
    remove_dir_all(&dir.join("staging"))?;
    sync_dir(dir)
}

/// Fshin një skedar; mungesa s'është gabim, çdo gjë tjetër po (P-03).
fn remove_file(p: &Path) -> Result<(), String> {
    match std::fs::remove_file(p) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("fshirja e {}: {e}", p.display())),
        _ => Ok(()),
    }
}

fn remove_dir_all(p: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(p) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("fshirja e {}: {e}", p.display())),
        _ => Ok(()),
    }
}

/// fsync i dosjes: krijimet, fshirjet dhe rename-t e skedarëve në të bëhen të qëndrueshme.
fn sync_dir(dir: &Path) -> Result<(), String> {
    File::open(dir).and_then(|d| d.sync_all()).map_err(|e| format!("fsync i dosjes {}: {e}", dir.display()))
}

/// Gabimi i dëmtimit: skedarët ruhen, dhe operatori vendos.
fn corrupt(dir: &Path, what: &str) -> String {
    format!(
        "zilja-pages: ruajtja në {} është e dëmtuar ose e paqëndrueshme: {what}. Asnjë skedar s'u ndryshua (u ruajtën për diagnozë). \
         Për rindërtim nga snapshot-i: ndalo validatorin, zhvendose dosjen dhe rinise (shih docs/OPERIMI.md)",
        dir.display()
    )
}

impl Source for PageTree {
    fn read_expected(&self, p: &NPath, rf: Ref) -> Result<Option<(u64, Node)>, String> {
        if rf.kind == Kind::Empty {
            return Ok(None);
        }
        let root = page_root_of(p);
        let (_, idx) = page_pos(p, root.depth);
        // Hapi 8: faqet e fiksuara (të krijuara nga ky proces ose të verifikuara me `check_page`
        // gjatë ngarkimit) janë të besuara, ndaj nyjet e tyre s'rihash-ohen. Faqet e lexuara nga
        // disku kontrollohen nyje për nyje si më parë (F-04).
        let (node, trusted) = match self.page_at_path(&root)? {
            None => (None, false),
            Some((id, pg)) => {
                let trusted = self.pinned.get(&id).is_some_and(|q| Arc::ptr_eq(q, &pg));
                (pg.nodes.slots[idx].clone(), trusted)
            }
        };
        match node {
            None => Err(format!("nyja në thellësinë {} mungon ose s'lexohet nga disku", p.depth)),
            Some(n) if (!trusted || cfg!(debug_assertions)) && n.to_ref() != rf => Err(format!("nyja në thellësinë {} nuk përputhet me hash-in e prindit", p.depth)),
            Some(n) => Ok(Some((0, n))),
        }
    }
}

impl StateTree for PageTree {
    fn set_background_maintenance(&mut self, on: bool) {
        self.defer_ckpt = on;
    }

    fn maintenance(&self) -> Result<bool, String> {
        if !self.ckpt_due.load(Ordering::Relaxed) {
            return Ok(false);
        }
        self.checkpoint()?;
        Ok(true)
    }

    fn root(&self) -> Hash {
        self.state.as_ref().map_or(EMPTY, |s| s.root.hash)
    }

    fn len(&self) -> usize {
        self.state.as_ref().map_or(0, |s| s.len as usize)
    }

    fn stage_snapshot(&mut self, slot: u64, ups: Vec<Update>) -> Result<(), String> {
        if self.staged == 0 {
            self.mark_building()?;
            if self.state.is_some() {
                self.clear()?;
            }
        }
        for (k, m) in ups {
            let b = (k[0] as u16) >> (8 - BUCKET_BITS);
            let buf = self.stage_buf.entry(b).or_default();
            let at = buf.len();
            buf.extend_from_slice(&k);
            buf.extend_from_slice(&slot.to_le_bytes());
            buf.extend_from_slice(&self.seq.to_le_bytes());
            self.seq += 1;
            match &m {
                None => buf.extend_from_slice(&[0u8; 1 + META_LEN]),
                Some(m) => {
                    buf.push(1);
                    buf.extend_from_slice(&enc_meta(m));
                }
            }
            let sum = self.stage_sums.entry(b).or_insert_with(|| (0, sha2::Sha256::new()));
            sum.0 += 1;
            sum.1.update(&buf[at..]);
            self.staged += 1;
        }
        if self.stage_buf.values().map(Vec::len).sum::<usize>() > 64 << 20 {
            self.flush_staging().map_err(|e| format!("shkrimi i snapshot-it dështoi: {e}"))?;
        }
        Ok(())
    }

    fn finish_snapshot(&mut self, slot: u64, complete: bool) -> Result<Hash, String> {
        if self.staged == 0 {
            self.mark_building()?;
            if self.state.is_some() {
                self.clear()?;
            }
        }
        self.flush_staging()?;
        self.data.set_len(0).map_err(io("fshirja e faqeve"))?;
        *self.alloc.get_mut().unwrap() = Alloc::default();
        let pk = Mutex::new(Packer::new(&Alloc::default()));
        let mut buckets: Vec<Option<Box<MNode>>> = Vec::with_capacity(1 << BUCKET_BITS);
        let mut len = 0u64;
        for b in 0..(1u16 << BUCKET_BITS) {
            let (sub, n) = self.build_bucket(b, &pk).map_err(|e| format!("ndërtimi nga snapshot-i dështoi: {e}"))?;
            buckets.push(sub);
            len += n;
            // Pas çdo kove: blloqet e mbyllura në disk, që RAM-i i Packer-it të mos varet nga madhësia e pemës.
            self.flush_full_blocks(&pk)?;
        }
        fn top(depth: u16, v: &mut [Option<Box<MNode>>]) -> Option<Box<MNode>> {
            if depth == BUCKET_BITS {
                return v[0].take();
            }
            let (a, b) = v.split_at_mut(v.len() / 2);
            let l = top(depth + 1, a);
            let r = top(depth + 1, b);
            mjoin(l, r)
        }
        let root = top(0, &mut buckets);
        let root_id = match &root {
            None => None,
            Some(n) => Some(self.write_mtree(n, NPath::root(), &pk)?),
        };
        let pk = pk.into_inner().unwrap();
        {
            let a = self.alloc.get_mut().unwrap();
            pk.live_list(a);
        }
        self.write_blocks(&pk.blocks)?;
        self.snapshot_peak_blocks = pk.peak;
        let st = State { version: slot, root: mref(&root), len, complete, base: slot, cache_depth: self.o.cache_depth, fingerprint: None };
        self.ring.clear();
        let delay = self.o.history;
        push_ring(&mut self.ring, slot, root_id, st.root, delay);
        self.state = Some(st);
        self.root_id = root_id;
        self.staged = 0;
        self.seq = 0;
        self.rec_seq = 1;
        remove_dir_all(&self.dir.join("staging"))?;
        self.load_pinned()?;
        self.cache_since = slot;
        // Snapshot-i ruhet drejtpërdrejt si checkpoint (me fsync të faqeve para tij).
        self.checkpoint()?;
        // A05: ndërtimi mbaroi; vetëm tani hiqet shënuesi.
        self.stage_sums.clear();
        remove_file(&self.dir.join(BUILDING))?;
        if self.o.durable {
            sync_dir(&self.dir)?;
        }
        Ok(self.root())
    }

    fn apply_slot(&mut self, slot: u64, ups: Vec<Update>, fingerprint: Option<Fingerprint>) -> Result<Hash, String> {
        let p = self.prepare_slot(slot, ups, fingerprint)?;
        self.commit_slot(slot, p)
    }

    /// Leximet, llogaritja dhe shkrimi i faqeve të reja në blloqe të lira. Asgjë që lexojnë
    /// dëftesat nuk preket: faqet e reja s'janë pjesë e asnjë versioni deri te `commit_slot`.
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
            cache_depth: self.o.cache_depth,
            fingerprint: None,
        });
        if self.state.is_some() {
            self.alloc.lock().unwrap().release(prev.version, self.o.history, self.rec_seq + 1, self.o.reuse_slack);
        }
        self.work.write().unwrap().clear();
        self.prefetch(&ups);
        let moved_paths = self.pick_relocations();
        let t_pre = t0.elapsed();
        let res = (|| -> Result<PagePrepared, String> {
            let mut out = Vec::new();
            let root_path = NPath::root();
            let old_root = self.read_expected(&root_path, prev.root)?;
            let (new_root, delta) = if ups.is_empty() {
                (old_root.clone().map(|o| o.1), 0)
            } else {
                self.apply(root_path, old_root.clone(), &ups, &mut out).map_err(|e| format!("sloti {slot}: {e}"))?
            };
            emit(&mut out, root_path, new_root.clone(), old_root);
            let t_tree = t0.elapsed();

            // Faqet që ndryshojnë, faqet që zhvendosen (mbledhja e hapësirës), plus të gjithë
            // paraardhësit e tyre (marrin numra të rinj fëmijësh).
            let mut dirty: HashMap<PKey, DirtyPage> = HashMap::default();
            for w in out {
                let root = page_root_of(&w.path);
                let (_, idx) = page_pos(&w.path, root.depth);
                dirty.entry(root.enc()).or_insert_with(|| (root, Vec::new())).1.push((idx, w.new));
            }
            let moved = moved_paths.len();
            for p in moved_paths {
                dirty.entry(p.enc()).or_insert_with(|| (p, Vec::new()));
            }
            let mut roots: Vec<NPath> = dirty.values().map(|(p, _)| *p).collect();
            while let Some(p) = roots.pop() {
                if p.depth >= PAGE_H {
                    let parent = p.truncate(p.depth - PAGE_H);
                    if !dirty.contains_key(&parent.enc()) {
                        dirty.insert(parent.enc(), (parent, Vec::new()));
                        roots.push(parent);
                    }
                }
            }
            let mut order: Vec<DirtyPage> = dirty.into_values().collect();
            // Thellësia zbret (fëmijët para prindërve); brenda thellësisë sipas rrugës, që paketimi
            // të vendosë fqinjët në të njëjtin bllok.
            order.sort_unstable_by(|a, b| b.0.depth.cmp(&a.0.depth).then(a.0.bits.cmp(&b.0.bits)));
            let pos: HashMap<PKey, usize> = order.iter().enumerate().map(|(i, (p, _))| (p.enc(), i)).collect();

            let mut news: Vec<Option<Arc<FPage>>> = Vec::with_capacity(order.len());
            let mut ids: Vec<Option<PageId>> = Vec::with_capacity(order.len());
            let mut freed: Vec<PageId> = Vec::new();
            let mut pk = Packer::new(&self.alloc.lock().unwrap());
            let mut t_split = [std::time::Duration::ZERO; 5];
            t_split[3] = t0.elapsed() - t_tree;
            let r = (|| -> Result<(), String> {
                use rayon::prelude::*;
                let tb = std::time::Instant::now();
                // Hapi 8: faqet e një thellësie janë të pavarura nga njëra-tjetra (fëmijët e tyre janë
                // më thellë dhe kanë marrë numrat), ndaj ndërtohen dhe kodohen paralelisht. Vendosja
                // në blloqe mbetet sekuenciale dhe në të njëjtën radhë, që rezultati të jetë
                // përcaktues dhe bajt për bajt i njëjtë me më parë.
                let mut i = 0;
                while i < order.len() {
                    let depth = order[i].0.depth;
                    let j = i + order[i..].iter().take_while(|(p, _)| p.depth == depth).count();
                    let ids_ro = &ids;
                    let pos_ro = &pos;
                    let build = |(root, changes): &DirtyPage| -> Result<BuiltPage, String> {
                        let old = self.page_at_path(root)?;
                        let mut pg = old.as_ref().map_or_else(|| FPage::new(*root), |(_, p)| (**p).clone());
                        for (idx, n) in changes {
                            pg.nodes.slots[*idx] = n.clone();
                        }
                        // Në thellësinë 256 (gjethe në fund të çelësit) s'ka më fëmijë.
                        for j in (0..KIDS).filter(|_| root.depth + PAGE_H <= 256) {
                            if let Some(&ci) = pos_ro.get(&kid_path(root, j).enc()) {
                                // Fëmija u përpunua më parë (thellësia zbret).
                                pg.kids[j] = ids_ro[ci];
                            }
                        }
                        let old_id = old.map(|(id, _)| id);
                        if pg.nodes.is_empty() {
                            if pg.kids.iter().any(Option::is_some) {
                                return Err("faqe bosh me faqe fëmijë".into());
                            }
                            return Ok((old_id, None));
                        }
                        pg.check_kids().map_err(|e| format!("sloti {slot}: {e}"))?;
                        let bytes = pg.enc();
                        Ok((old_id, Some((pg, bytes))))
                    };
                    let built: Vec<_> = if j - i >= 64 {
                        order[i..j].par_iter().map(build).collect::<Result<_, _>>()?
                    } else {
                        order[i..j].iter().map(build).collect::<Result<_, _>>()?
                    };
                    for (old_id, pg) in built {
                        freed.extend(old_id);
                        match pg {
                            None => {
                                news.push(None);
                                ids.push(None);
                            }
                            Some((pg, bytes)) => {
                                ids.push(Some(pk.place(&self.alloc, &bytes)));
                                // `FPage` s'ka alokime të brendshme përveç gjetheve, ndaj `Arc`-u
                                // krijohet këtu, në fillin që bën commit: faqet jetëgjata të
                                // fiksuara s'shpërndahen nëpër arenat e malloc-ut të fijeve rayon
                                // (pa këtë RSS rritej ~50 MB nga fragmentimi).
                                news.push(Some(Arc::new(pg)));
                            }
                        }
                    }
                    i = j;
                }
                // `sync_every = 1`: faqet në disk para rekordit, këtu, jashtë kyçit të shkrimit. Kështu
                // `commit_slot` bën vetëm fsync-un e vogël të log-ut, dhe një ikje rryme humb maksimumi
                // slotin që po shkruhej.
                t_split[0] = tb.elapsed();
                let tw = std::time::Instant::now();
                self.write_blocks(&pk.blocks)?;
                t_split[1] = tw.elapsed();
                let ts = std::time::Instant::now();
                if self.o.durable && self.o.sync_every == 1 && !pk.blocks.is_empty() {
                    self.sync_data()?;
                }
                t_split[2] = ts.elapsed();
                Ok(())
            })();
            if let Err(e) = r {
                self.alloc.lock().unwrap().untake(&pk.taken, pk.next0);
                return Err(e);
            }
            let ta = std::time::Instant::now();
            let blocks = pk.live_list(&mut self.alloc.lock().unwrap());
            let pages = ids.iter().flatten().count().max(1);
            self.last_blocks.store((blocks.len() * pages.saturating_sub(moved) / pages) as u64, Ordering::Relaxed);
            let root_id = if order.is_empty() { self.root_id } else { ids[pos[&root_path.enc()]] };
            let cd = self.o.cache_depth;
            let pinned_new = order
                .iter()
                .zip(news.iter().zip(&ids))
                .filter(|((p, _), _)| p.depth < cd)
                .filter_map(|(_, (pg, id))| Some(((*id)?, pg.clone()?)))
                .collect();
            let st = State {
                version: slot,
                root: ref_of(&new_root),
                len: (prev.len as i64 + delta).max(0) as u64,
                complete: prev.complete,
                base: prev.base,
                cache_depth: self.o.cache_depth,
                fingerprint,
            };
            t_split[4] = ta.elapsed();
            Ok(PagePrepared {
                slot,
                rec: LogRec { history: self.o.history, slack: self.o.reuse_slack, seq: self.rec_seq + 1, st, root_id, blocks, freed },
                pinned_new,
                t_pre,
                t_tree,
                t_write: t0.elapsed(),
                t_split,
                pages: ids.iter().flatten().count(),
                moved,
            })
        })();
        self.work.write().unwrap().clear();
        Ok(Box::new(res?))
    }

    /// Rekordi në log (pika pa kthim) dhe ndërrimi i versionit në RAM. I shkurtër.
    fn commit_slot(&mut self, slot: u64, p: zilja_tree::Prepared) -> Result<Hash, String> {
        let p = *p.downcast::<PagePrepared>().map_err(|_| "commit_slot: përgatitje e një peme tjetër")?;
        if p.slot != slot {
            return Err(format!("commit_slot: përgatitja ishte për slotin {}", p.slot));
        }
        if p.rec.seq != self.rec_seq + 1 {
            return Err("commit_slot: përgatitje e vjetëruar".into());
        }
        let t0 = std::time::Instant::now();
        self.commits += 1;
        let sync = self.o.durable && self.o.sync_every > 0 && self.commits.is_multiple_of(self.o.sync_every);
        if sync && self.o.sync_every > 1 {
            self.sync_data()?;
        }
        self.append_log(&p.rec, sync).map_err(|e| format!("shkrimi i slotit {slot} dështoi: {e}"))?;
        let rec = p.rec;
        self.alloc.get_mut().unwrap().retire(slot, rec.seq, rec.freed.clone());
        self.rec_seq = rec.seq;
        for id in &rec.freed {
            self.pinned.remove(id);
        }
        self.pinned.extend(p.pinned_new);
        let delay = self.o.history;
        push_ring(&mut self.ring, slot, rec.root_id, rec.st.root, delay);
        if self.state.is_none() {
            self.cache_since = slot;
        }
        self.root_id = rec.root_id;
        self.state = Some(rec.st);
        let recs = *self.records_since_ckpt.get_mut() + 1;
        *self.records_since_ckpt.get_mut() = recs;
        let log_len = *self.log_len.get_mut();
        let due = recs >= self.o.ckpt_every || log_len > self.o.ckpt_log_mb << 20;
        // Me `maintenance` checkpoint-i bëhet jashtë kyçit të shkrimit; këtu vetëm nëse
        // `maintenance` mungon aq gjatë sa log-u kalon dyfishin e kufirit (log-u s'rritet pa fund).
        let forced = recs >= 2 * self.o.ckpt_every || log_len > 2 * (self.o.ckpt_log_mb << 20);
        let ckpt = due && (!self.defer_ckpt || forced);
        if ckpt {
            self.checkpoint()?;
        } else if due {
            *self.ckpt_due.get_mut() = true;
        }
        let write = p.t_write.saturating_sub(p.t_tree);
        self.last_timing = Some(SlotTiming {
            read: p.t_pre,
            tree: p.t_tree.saturating_sub(p.t_pre),
            build: write.saturating_sub(p.t_split[1] + p.t_split[2]),
            pwrite: p.t_split[1],
            fsync: p.t_split[2],
            commit: t0.elapsed(),
            disk_reads: self.last_disk_reads.load(Ordering::Relaxed),
            pages: p.pages,
            checkpoint: ckpt,
        });
        if self.profile {
            let s = self.page_stats();
            eprintln!(
                "slot {slot}: leximi {:?}, pema {:?}, shkrimi {:?} (ndotura {:?}, faqet {:?}, pwrite {:?}, fsync {:?}, pas {:?}), log {:?} | {} faqe ({} të zhvendosura), {} blloqe, {} lexime | skedari {} blloqe, {:.0}% plot",
                p.t_pre,
                p.t_tree - p.t_pre,
                p.t_write - p.t_tree,
                p.t_split[3],
                p.t_split[0],
                p.t_split[1],
                p.t_split[2],
                p.t_split[4],
                t0.elapsed(),
                p.pages,
                p.moved,
                rec.blocks.len(),
                self.last_disk_reads.load(Ordering::Relaxed),
                s.blocks,
                100.0 * s.live_bytes as f64 / ((s.blocks - s.free_blocks).max(1) * BLOCK as u64) as f64
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
        // Versioni = i fundit me slot ≤ v.
        let &(_, rid, rref) = self.ring.iter().rev().find(|(s, _, _)| *s <= v).ok_or("versioni s'është në kujtesë")?;
        let root = rref.hash;
        let mut siblings = Vec::new();
        let Some(mut id) = rid else {
            return Ok((v, root, Proof { siblings, leaf: None }, None));
        };
        let mut pg = self.load(id)?;
        let (mut idx, mut depth, mut want) = (0usize, 0usize, rref);
        loop {
            if idx == 0 && pg.path != (NPath { depth: depth as u16, bits: *key }).truncate(depth as u16) {
                return Err("faqe në disk që nuk është ajo e rrugës së pritur".into());
            }
            // F-04: çdo nyje duhet të jetë aty dhe të përputhet me hash-in e prindit.
            let node = pg.nodes.slots[idx].as_ref().ok_or("nyje që mungon në disk")?;
            if node.to_ref() != want {
                return Err("nyje në disk që nuk përputhet me hash-in e prindit".into());
            }
            match node {
                Node::Leaf(d) => {
                    let m = (d.key == *key).then(|| d.meta.clone());
                    return Ok((v, root, Proof { siblings, leaf: Some((d.key, d.vh())) }, m));
                }
                Node::Internal { l, r } => {
                    let b = bit(key, depth);
                    let (next, sib) = if b == 0 { (*l, *r) } else { (*r, *l) };
                    siblings.push(sib.hash);
                    if next.kind == Kind::Empty {
                        return Ok((v, root, Proof { siblings, leaf: None }, None));
                    }
                    want = next;
                    depth += 1;
                    if depth % PAGE_H as usize == 0 {
                        id = pg.kids[2 * (idx - 7) + b as usize].ok_or("faqe fëmijë që mungon")?;
                        pg = self.load(id)?;
                        idx = 0;
                    } else {
                        idx = 2 * idx + 1 + b as usize;
                    }
                }
            }
        }
    };
        inner().map_err(zilja_tree::ProveError::Storage)
    }

    fn provable_range(&self) -> Option<(u64, u64)> {
        self.state.as_ref().map(|s| {
            let lo = if self.o.history == 0 { s.version } else { s.version.saturating_sub(self.o.history) };
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
        // F-13: asgjë nuk fshihet tani; nisja e ardhshme e rindërton. P-03: gabimi raportohet.
        let p = self.dir.join("INVALID");
        File::create(&p)
            .and_then(|mut f| {
                f.write_all(why.as_bytes())?;
                f.sync_all()
            })
            .map_err(|e| format!("shënimi 'i pavlefshëm' {}: {e}", p.display()))?;
        sync_dir(&self.dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// G18: një rekord log-u mbi 64 MB refuzohet para shkrimit (hapja do ta quante të dëmtuar).
    #[test]
    fn oversized_log_record_is_refused_before_write() {
        let st = State { version: 9, root: EMPTY_REF, len: 0, complete: true, base: 1, cache_depth: 20, fingerprint: None };
        let small = LogRec { history: 32, slack: 8, seq: 2, st: st.clone(), root_id: None, blocks: vec![], freed: vec![pid(0, 1, 10)] };
        assert!(small.framed().is_ok());
        let n = (MAX_RECORD / 8) as usize + 1;
        let big = LogRec { history: 32, slack: 8, seq: 2, st, root_id: None, blocks: vec![], freed: vec![pid(0, 1, 10); n] };
        let e = big.framed().unwrap_err();
        assert!(e.contains("mbi kufirin"), "{e}");
    }

    #[test]
    fn kid_paths_and_indices_agree() {
        let mut key = [0u8; 32];
        key[0] = 0b1011_0110;
        let root = NPath { depth: 4, bits: key }.truncate(4);
        for j in 0..KIDS {
            let cp = kid_path(&root, j);
            assert_eq!(cp.depth, 8);
            assert_eq!(kid_index(&cp), j);
            assert_eq!(cp.truncate(4), root);
        }
    }

    #[test]
    fn page_roundtrip_and_ids() {
        let path = NPath { depth: 12, bits: [0xab; 32] }.truncate(12);
        let mut pg = FPage::new(path);
        pg.nodes.slots[0] = Some(Node::Internal { l: Ref { kind: Kind::Leaf, hash: [1; 32] }, r: EMPTY_REF });
        pg.nodes.slots[1] = Some(Node::leaf([9; 32], AccountMeta { lamports: 7, ..Default::default() }));
        let b = pg.enc();
        assert_eq!(FPage::dec(&b), Some(pg.clone()));
        let mut bad = b.clone();
        bad[20] ^= 1;
        assert!(FPage::dec(&bad).is_none(), "crc duhet ta kapë");
        // Faqja më e madhe e mundshme (në thellësinë 252) futet në një bllok.
        let mut big = FPage::new(NPath { depth: 252, bits: [0xff; 32] }.truncate(252));
        for i in 0..PAGE_SLOTS {
            big.nodes.slots[i] = Some(Node::leaf([i as u8; 32], AccountMeta::default()));
        }
        big.kids = [Some(pid((1 << 41) - 1, 4095, 2047)); KIDS];
        let e = big.enc();
        assert!(e.len() < 2048 && e.len() <= BLOCK);
        assert_eq!(FPage::dec(&e), Some(big));
        let id = pid(123_456_789, 4000, 96);
        assert_eq!((pid_block(id), pid_off(id), pid_len(id)), (123_456_789, 4000, 96));
        // Bit-e përtej thellësisë: jo kanonike.
        let mut body = FPage::new(path).body();
        body[3] |= 1;
        assert!(FPage::dec_body(&body).is_none());
    }

    #[test]
    fn alloc_take_untake_restores_exactly() {
        let mut a = Alloc::default();
        for _ in 0..5 {
            let b = a.take_block();
            a.set_live(b, 1000);
        }
        a.retire(5, 1, vec![pid(1, 0, 1000), pid(3, 0, 1000)]);
        a.release(5, 0, 2, 0);
        assert_eq!(a.free, vec![1, 3]);
        let before = a.clone();
        let next0 = a.next;
        let t: Vec<u64> = (0..4).map(|_| a.take_block()).collect();
        assert_eq!(t, vec![3, 1, 5, 6]);
        for b in &t {
            a.set_live(*b, 500);
        }
        a.untake(&t, next0);
        assert_eq!(a, before);
        assert_eq!(a.cand.len(), before.cand.len());
    }

    #[test]
    fn records_roundtrip() {
        for s in fuzz_seeds_pages(&[7u8; 64]) {
            assert!(fuzz_decode_pages(&s) >= 1);
        }
    }
}
