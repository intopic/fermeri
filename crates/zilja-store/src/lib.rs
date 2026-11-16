//! # zilja-store: pema e Ziles në disk
//!
//! Dy motorë me të njëjtin algoritëm dhe të njëjtat root-e/dëftesa: `DiskTree` (RocksDB, më poshtë)
//! dhe `PageTree` (skedar blloqesh pa bazë të dhënash, në `pagefile.rs`).
//!
//! **Tavolina dhe rafti.** Nivelet e sipërme të pemës (`cache_depth`) jetojnë vetëm
//! në RAM. Poshtë tyre, pema ruhet në RocksDB me **faqe**: çdo faqe mban 4 nivele
//! (deri në 15 nyje, gjethe ose të brendshme) dhe lexohet/shkruhet si një hyrje e
//! vetme. Kështu një ndryshim kërkon ~1–2 lexime nga disku në vend të një leximi për
//! çdo nivel. Gjethet mbi `cache_depth` (të rralla) ruhen më vete. Kur hapet disku,
//! nivelet e sipërme rillogariten nga rrënjët e faqeve dhe krahasohen me root-in e ruajtur.
//!
//! **Shkrim atomik.** Gjithë ndryshimet e një sloti dhe gjendja e re shkruhen me
//! një `WriteBatch`. Pas një ndërprerjeje mbetet gjendja e plotë e slotit të
//! mëparshëm, kurrë gjysmë-gjendje.
//!
//! **Kujtesa e sloteve.** Kur një faqe ndryshon, versioni i vjetër kalon në arkivë
//! (`nodes_hist`, çelësi `rrënja_e_faqes ‖ versioni`, vlera `skadon_në ‖ faqja`).
//! Kompaktimi i RocksDB fshin vetë hyrjet që kanë skaduar.
//! Për nivelet e sipërme, vlerat e vjetra mbahen në RAM për `history` slotet e fundit.
//!
//! | CF | çelësi | vlera |
//! |---|---|---|
//! | `nodes` | rruga (34) | versioni ‖ nyja (vetëm gjethe mbi `cache_depth`) |
//! | `pages` | rrënja e faqes (34) | versioni ‖ faqja |
//! | `nodes_hist` | rrënja e faqes ‖ versioni | skadon_në ‖ faqja |
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
use zilja_tree::{normalize_updates, Fingerprint, ProofAt, Resume, StateTree, Update};
use zilja_verifier::{bit, internal_hash, leaf_hash, AccountMeta, Hash, Key, Proof, EMPTY};

/// Nivelet e pemës që mbahen në RAM (deri në ~2^20 nyje).
pub const CACHE_DEPTH: u16 = 20;
/// Kujtesa e paracaktuar: sa slote pas mund të jepen dëftesa.
pub const DEFAULT_HISTORY: u64 = 32;
const PAR_THRESHOLD: usize = 512;
/// Llogari për kovë gjatë ndërtimit nga snapshot-i.
const BUCKET_TARGET: u64 = 1 << 20;

/// Lartësia e një faqeje: 4 nivele, deri në 15 nyje.
pub const PAGE_H: u16 = 4;
const PAGE_SLOTS: usize = (1 << PAGE_H) - 1;

const CF_NODES: &str = "nodes";
const CF_PAGES: &str = "pages";
const CF_HIST: &str = "nodes_hist";
const CF_STAGING: &str = "staging";
/// Çelësi në `meta` që shënon gjendjen si të pavlefshme (rindërtohet në nisjen e ardhshme).
const INVALID_KEY: &[u8] = b"invalid";
const CF_META: &str = "meta";

mod pagefile;
pub use pagefile::{fuzz_decode_pages, fuzz_seeds_pages, PageOptions, PageStats, PageTree, BLOCK as PAGE_BLOCK};
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
    /// E njëjta rrugë, e shkurtuar në thellësinë `d`.
    fn truncate(&self, d: u16) -> Self {
        let mut bits = [0u8; 32];
        let full = (d / 8) as usize;
        bits[..full].copy_from_slice(&self.bits[..full]);
        let rem = d % 8;
        if rem > 0 {
            bits[full] = self.bits[full] & (0xffu8 << (8 - rem));
        }
        NPath { depth: d, bits }
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

/// Vetëm 0, 1, 2: çdo vlerë tjetër është e dëmtuar (Safer-3: dekodim kanonik).
fn kind_of(b: u8) -> Option<Kind> {
    match b {
        0 => Some(Kind::Empty),
        1 => Some(Kind::Leaf),
        2 => Some(Kind::Internal),
        _ => None,
    }
}

fn bool_of(b: u8) -> Option<bool> {
    match b {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Ref {
    kind: Kind,
    hash: Hash,
}

const EMPTY_REF: Ref = Ref { kind: Kind::Empty, hash: EMPTY };

#[derive(Clone, Debug)]
struct LeafData {
    key: Key,
    meta: AccountMeta,
    /// `meta.value_hash()`, i llogaritur vetëm kur duhet (shumica e gjetheve të lexuara
    /// nga disku s'kanë nevojë për të).
    vh: std::sync::OnceLock<Hash>,
}

impl LeafData {
    fn vh(&self) -> Hash {
        *self.vh.get_or_init(|| self.meta.value_hash())
    }
}

impl PartialEq for LeafData {
    fn eq(&self, o: &Self) -> bool {
        self.key == o.key && self.meta == o.meta
    }
}
impl Eq for LeafData {}

/// Gjethja mbahet në kuti, që nyja e brendshme (shumica në cache) të zërë pak RAM.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Node {
    Leaf(Box<LeafData>),
    Internal { l: Ref, r: Ref },
}

impl Node {
    fn leaf(key: Key, meta: AccountMeta) -> Self {
        Node::Leaf(Box::new(LeafData { key, meta, vh: std::sync::OnceLock::new() }))
    }
    fn hash(&self) -> Hash {
        match self {
            Node::Leaf(d) => leaf_hash(&d.key, &d.vh()),
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
        let mut v = Vec::with_capacity(114);
        self.enc_into(&mut v);
        v
    }
    fn enc_into(&self, v: &mut Vec<u8>) {
        match self {
            Node::Leaf(d) => {
                v.push(1);
                v.extend_from_slice(&d.key);
                v.extend_from_slice(&enc_meta(&d.meta));
            }
            Node::Internal { l, r } => {
                v.push(2);
                v.push(l.kind as u8);
                v.extend_from_slice(&l.hash);
                v.push(r.kind as u8);
                v.extend_from_slice(&r.hash);
            }
        }
    }
    fn dec(b: &[u8]) -> Option<Node> {
        let h = |s: &[u8]| -> Hash { s.try_into().unwrap() };
        match b.first()? {
            1 if b.len() == 33 + META_LEN => Some(Node::leaf(h(&b[1..33]), dec_meta(&b[33..])?)),
            2 if b.len() == 67 => Some(Node::Internal {
                l: Ref { kind: kind_of(b[1])?, hash: h(&b[2..34]) },
                r: Ref { kind: kind_of(b[34])?, hash: h(&b[35..67]) },
            }),
            _ => None,
        }
    }
}

/// Një faqe: nënpema me lartësi `PAGE_H` nën rrënjën e saj, në renditje "heap":
/// pozicioni 0 = rrënja, fëmijët e `i` janë `2i+1` (majtas) dhe `2i+2` (djathtas).
#[derive(Clone, PartialEq, Eq, Debug)]
struct Page {
    slots: [Option<Node>; PAGE_SLOTS],
}

impl Page {
    fn new() -> Self {
        Page { slots: std::array::from_fn(|_| None) }
    }
    fn is_empty(&self) -> bool {
        self.slots.iter().all(Option::is_none)
    }
    /// `bitmap u16 LE ‖ nyjet e pranishme njëra pas tjetrës`
    fn enc(&self) -> Vec<u8> {
        self.enc_with_ver(None)
    }
    /// `[versioni ‖] bitmap ‖ nyjet`, në një buffer të vetëm.
    fn enc_with_ver(&self, ver: Option<u64>) -> Vec<u8> {
        let mut out = Vec::with_capacity(10 + PAGE_SLOTS * 80);
        if let Some(v) = ver {
            out.extend_from_slice(&v.to_be_bytes());
        }
        let at = out.len();
        out.extend_from_slice(&[0, 0]);
        let mut bm: u16 = 0;
        for (i, s) in self.slots.iter().enumerate() {
            if let Some(n) = s {
                bm |= 1 << i;
                n.enc_into(&mut out);
            }
        }
        out[at..at + 2].copy_from_slice(&bm.to_le_bytes());
        out
    }
    fn dec(b: &[u8]) -> Option<Page> {
        let bm = u16::from_le_bytes(b.get(..2)?.try_into().ok()?);
        if bm >> PAGE_SLOTS != 0 {
            return None; // bit-e përtej vendeve të faqes: kodim jo kanonik (Safer-3)
        }
        let mut pos = 2;
        let mut pg = Page::new();
        for i in 0..PAGE_SLOTS {
            if bm >> i & 1 == 1 {
                let len = match b.get(pos)? {
                    1 => 33 + META_LEN,
                    2 => 67,
                    _ => return None,
                };
                pg.slots[i] = Some(Node::dec(b.get(pos..pos + len)?)?);
                pos += len;
            }
        }
        (pos == b.len()).then_some(pg)
    }
}

/// Safer-3 (fuzz): dekodon bajte arbitrare si faqe, gjendje dhe vlerë staging-u. S'duhet të bjerë
/// kurrë, dhe çdo gjë që dekodohet duhet të rikodohet njësoj. Kthen sa nga të tria u dekoduan.
#[doc(hidden)]
pub fn fuzz_decode(b: &[u8]) -> usize {
    let mut n = 0;
    if let Some(pg) = Page::dec(b) {
        assert_eq!(pg.enc(), b, "faqja nuk rikodohet njësoj");
        n += 1;
    }
    if let Some(st) = State::dec(b) {
        assert_eq!(st.enc(), b, "gjendja nuk rikodohet njësoj");
        n += 1;
    }
    if let Some((slot, m)) = dec_staged(b) {
        assert_eq!(enc_staged(slot, &m), b, "staging nuk rikodohet njësoj");
        n += 1;
    }
    n
}

/// Safer-3: koduesit e vërtetë, për të krijuar hyrje fillestare (seeds) për fuzz-in.
#[doc(hidden)]
pub fn fuzz_seeds(rng_bytes: &[u8]) -> Vec<Vec<u8>> {
    let take = |i: usize| -> [u8; 32] { std::array::from_fn(|j| rng_bytes.get((i * 32 + j) % rng_bytes.len().max(1)).copied().unwrap_or(0)) };
    let m = AccountMeta { lamports: 5, owner: take(1), executable: true, data_len: 9, data_hash: take(2) };
    let mut pg = Page::new();
    pg.slots[0] = Some(Node::Internal { l: Ref { kind: Kind::Leaf, hash: take(3) }, r: Ref { kind: Kind::Internal, hash: take(4) } });
    pg.slots[1] = Some(Node::leaf(take(5), m.clone()));
    let st = State { version: 7, root: Ref { kind: Kind::Internal, hash: take(6) }, len: 3, complete: true, base: 1, cache_depth: 20, fingerprint: Some((6, take(7))) };
    vec![pg.enc(), st.enc(), enc_staged(3, &Some(m)), enc_staged(4, &None)]
}

fn dec_page_val(v: &[u8]) -> Option<(u64, Page)> {
    let (ver, body) = split_u64(v)?;
    Some((ver, Page::dec(body)?))
}

/// Rrënja e faqes (në thellësinë `pd`) dhe pozicioni i `p` brenda saj.
fn page_pos(p: &NPath, pd: u16) -> (NPath, usize) {
    let mut x = 0usize;
    for i in pd..p.depth {
        x = (x << 1) | bit(&p.bits, i as usize) as usize;
    }
    (p.truncate(pd), (1usize << (p.depth - pd)) - 1 + x)
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

/// Vlera e staging-ut: `slot u64 ‖ 0` (llogari e mbyllur) ose `slot u64 ‖ 1 ‖ meta`.
fn enc_staged(slot: u64, m: &Option<AccountMeta>) -> Vec<u8> {
    let mut v = slot.to_le_bytes().to_vec();
    match m {
        None => v.push(0),
        Some(m) => {
            v.push(1);
            v.extend_from_slice(&enc_meta(m));
        }
    }
    v
}

fn dec_staged(b: &[u8]) -> Option<(u64, Option<AccountMeta>)> {
    let slot = u64::from_le_bytes(b.get(..8)?.try_into().ok()?);
    match b.get(8)? {
        0 if b.len() == 9 => Some((slot, None)),
        1 => Some((slot, Some(dec_meta(&b[9..])?))),
        _ => None,
    }
}

fn dec_meta(b: &[u8]) -> Option<AccountMeta> {
    if b.len() != META_LEN {
        return None;
    }
    Some(AccountMeta {
        lamports: u64::from_le_bytes(b[..8].try_into().unwrap()),
        owner: b[8..40].try_into().unwrap(),
        executable: bool_of(b[40])?,
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
    fingerprint: Option<Fingerprint>,
}

impl State {
    fn enc(&self) -> Vec<u8> {
        let mut v = vec![4u8];
        v.extend_from_slice(&self.version.to_le_bytes());
        v.push(self.root.kind as u8);
        v.extend_from_slice(&self.root.hash);
        v.extend_from_slice(&self.len.to_le_bytes());
        v.push(self.complete as u8);
        v.extend_from_slice(&self.base.to_le_bytes());
        v.extend_from_slice(&self.cache_depth.to_le_bytes());
        if let Some((slot, h)) = &self.fingerprint {
            v.extend_from_slice(&slot.to_le_bytes());
            v.extend_from_slice(h);
        }
        v
    }
    fn dec(b: &[u8]) -> Option<State> {
        if b.len() < 61 || b[0] != 4 || !(b.len() == 61 || b.len() == 101) {
            return None;
        }
        Some(State {
            version: u64::from_le_bytes(b[1..9].try_into().ok()?),
            root: Ref { kind: kind_of(b[9])?, hash: b[10..42].try_into().ok()? },
            len: u64::from_le_bytes(b[42..50].try_into().ok()?),
            complete: bool_of(b[50])?,
            base: u64::from_le_bytes(b[51..59].try_into().ok()?),
            cache_depth: u16::from_le_bytes(b[59..61].try_into().ok()?),
            fingerprint: (b.len() == 101).then(|| (u64::from_le_bytes(b[61..69].try_into().unwrap()), b[69..101].try_into().unwrap())),
        })
    }
}

/// Rezultati i `prepare_slot`: nyjet e reja dhe gjendja, gati për t'u shkruar.
struct DiskPrepared {
    slot: u64,
    out: Vec<NodeW>,
    st: State,
    t_pre: std::time::Duration,
    t_tree: std::time::Duration,
}

/// Ndryshimet e niveleve të sipërme në një slot: vlerat e vjetra, për dëftesat historike.
type CacheDiff = (u64, HashMap<PKey, Option<(u64, Node)>>);

// ---------------------------------------------------------------- algoritmi

/// Burimi i nyjeve të gjendjes së fundit. Algoritmi i zbatimit (`build`/`apply`) është i
/// përbashkët për çdo mënyrë ruajtjeje (RocksDB ose skedari i faqeve): ndryshon vetëm leximi.
trait Source: Sync {
    /// F-04: nyja në `p` që prindi e shënon si `rf`; mungesa ose hash i ndryshëm = gabim.
    fn read_expected(&self, p: &NPath, rf: Ref) -> Result<Option<(u64, Node)>, String>;


    /// Ndërton një nënpemë të re në `p` (poshtë `p` nuk ka asgjë). Nuk shkruan vetë `p`.
    /// `ups` përmban vetëm llogari të gjalla (Faster-2: varret hiqen një herë, nga thirrësi).
    fn build(&self, p: NPath, ups: &[(Key, Option<AccountMeta>)], out: &mut Vec<NodeW>) -> Option<Node> {
        match ups.len() {
            0 => None,
            1 => {
                let (k, v) = &ups[0];
                Some(Node::leaf(*k, v.clone()?))
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
    ) -> Result<(Option<Node>, i64), String> {
        Ok(match old.map(|o| o.1) {
            None => {
                let live: Vec<_> = ups.iter().filter(|(_, m)| m.is_some()).cloned().collect();
                let delta = live.len() as i64;
                (self.build(p, &live, out), delta)
            }
            Some(Node::Leaf(d)) => {
                let LeafData { key, meta, .. } = *d;
                match ups.binary_search_by(|(k, _)| k.cmp(&key)) {
                    Ok(_) => {
                        let live: Vec<_> = ups.iter().filter(|(_, m)| m.is_some()).cloned().collect();
                        let delta = live.len() as i64 - 1;
                        (self.build(p, &live, out), delta)
                    }
                    Err(pos) => {
                        let mut merged = Vec::with_capacity(ups.len() + 1);
                        merged.extend(ups[..pos].iter().filter(|(_, m)| m.is_some()).cloned());
                        merged.push((key, Some(meta)));
                        merged.extend(ups[pos..].iter().filter(|(_, m)| m.is_some()).cloned());
                        let delta = merged.len() as i64 - 1;
                        (self.build(p, &merged, out), delta)
                    }
                }
            }
            Some(Node::Internal { l, r }) => {
                let i = ups.partition_point(|(k, _)| bit(k, p.depth as usize) == 0);
                let (u0, u1) = ups.split_at(i);
                type Side = (bool, Option<Node>, Option<(u64, Node)>, Vec<NodeW>, i64);
                let side = |b: u8, u: &[(Key, Option<AccountMeta>)], rf: Ref| -> Result<Side, String> {
                    let mut o = Vec::new();
                    if u.is_empty() {
                        return Ok((false, None, None, o, 0));
                    }
                    let cp = p.child(b);
                    let old_c = self.read_expected(&cp, rf)?;
                    let (res, d) = self.apply(cp, old_c.clone(), u, &mut o)?;
                    Ok((true, res, old_c, o, d))
                };
                let (s0, s1) = if ups.len() > PAR_THRESHOLD {
                    rayon::join(|| side(0, u0, l), || side(1, u1, r))
                } else {
                    (side(0, u0, l), side(1, u1, r))
                };
                let (s0, s1) = (s0?, s1?);
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
                            (n.ok_or("gjethja e re mungon")?, old)
                        } else {
                            let rf = if li == 0 { l } else { r };
                            let o = self.read_expected(&cp, rf)?;
                            (o.as_ref().ok_or("gjethja e pandryshuar mungon")?.1.clone(), o)
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
        })
    }

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

    fn invalidate(&mut self, why: &str) {
        // F-13: asgjë nuk fshihet tani. Shënimi në `meta` bën që nisja e ardhshme ta rindërtojë.
        let mut b = WriteBatch::default();
        b.put_cf(self.cf(CF_META), INVALID_KEY, why.as_bytes());
        if let Err(e) = self.write(b) {
            log::error!("zilja-store: shënimi 'i pavlefshëm' dështoi: {e}");
        }
        let _ = self.db.flush_cf(self.cf(CF_META));
    }
}

#[cfg(test)]
mod page_tests {
    use super::*;

    #[test]
    fn page_positions_are_heap_ordered() {
        let mut key = [0u8; 32];
        key[0] = 0b1011_0000; // bit-ët 1,0,1,1
        let pd = 0;
        let at = |d: u16| page_pos(&NPath { depth: d, bits: key }.truncate(d), pd).1;
        assert_eq!(at(0), 0);
        assert_eq!(at(1), 2); // djathtas
        assert_eq!(at(2), 2 * 2 + 1); // pastaj majtas
        assert_eq!(at(3), 2 * 5 + 2); // pastaj djathtas
        assert_eq!(page_pos(&NPath { depth: 3, bits: key }.truncate(3), 0).0, NPath::root());
        // Faqja tjetër nis në thellësinë 4, me rrënjë = rruga e shkurtuar.
        let p4 = NPath { depth: 5, bits: key }.truncate(5);
        let (root, idx) = page_pos(&p4, 4);
        assert_eq!(root, NPath { depth: 4, bits: key }.truncate(4));
        assert_eq!(idx, 1); // bit-i 4 i çelësit = 0 → majtas
    }

    #[test]
    fn page_roundtrip() {
        let mut pg = Page::new();
        assert!(pg.is_empty());
        pg.slots[0] = Some(Node::Internal { l: Ref { kind: Kind::Leaf, hash: [1; 32] }, r: EMPTY_REF });
        pg.slots[1] = Some(Node::leaf([9; 32], AccountMeta { lamports: 7, ..Default::default() }));
        pg.slots[14] = Some(Node::Internal { l: EMPTY_REF, r: Ref { kind: Kind::Internal, hash: [2; 32] } });
        let b = pg.enc();
        assert_eq!(b.len(), 2 + 67 + 114 + 67);
        assert_eq!(Page::dec(&b), Some(pg));
        assert!(Page::dec(&b[..b.len() - 1]).is_none());
        assert!(Page::dec(&[b.clone(), vec![0]].concat()).is_none());
    }
}
