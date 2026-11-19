//! # zilja-store: pema e Ziles në disk
//!
//! Një motor: `PageTree` (skedar blloqesh 4 KB pa bazë të dhënash, në `pagefile.rs`). Motori i vjetër
//! RocksDB (`DiskTree`) u hoq; një dosje e vjetër RocksDB refuzohet në hapje dhe s'preket.
//!
//! **Tavolina dhe rafti.** Nivelet e sipërme të pemës (`cache_depth`) jetojnë vetëm
//! në RAM. Poshtë tyre, pema ruhet në disk me **faqe**: çdo faqe mban 4 nivele
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

/// Lartësia e një faqeje: 4 nivele, deri në 15 nyje.
pub const PAGE_H: u16 = 4;
const PAGE_SLOTS: usize = (1 << PAGE_H) - 1;


mod pagefile;
pub use pagefile::{fuzz_decode_pages, fuzz_seeds_pages, PageOptions, PageStats, PageTree, SNAPSHOT_FLUSH_BLOCKS, BLOCK as PAGE_BLOCK, FORMAT_VERSION as PAGE_FORMAT_VERSION};

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

/// Një ndryshim nyjeje: vendos `new` në `path` (None = hiq).
struct NodeW {
    path: NPath,
    new: Option<Node>,
}

fn emit(out: &mut Vec<NodeW>, path: NPath, new: Option<Node>, old: Option<(u64, Node)>) {
    let same = match (&new, &old) {
        (None, None) => true,
        (Some(n), Some((_, o))) => n == o,
        _ => false,
    };
    if !same {
        out.push(NodeW { path, new });
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

fn hx(h: &Hash) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
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
