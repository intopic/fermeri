//! # zilja-tree
//!
//! Sparse Merkle tree kompakt mbi të gjitha llogaritë. Një nënpemë me vetëm një
//! llogari përfaqësohet nga vetë gjethja, kështu që thellësia mesatare është
//! ~log2(N), jo 256.
//!
//! Përditësimet bëhen me grupe (një grup për slot): ndryshimet renditen sipas
//! çelësit, ndahen sipas bit-it në çdo nivel dhe degët e pavarura përpunohen
//! paralelisht me rayon. Hash-et rillogariten vetëm në rrugët që ndryshuan.

use zilja_verifier::{bit, internal_hash, leaf_hash, AccountMeta, Hash, Key, Proof, EMPTY};

/// Një ndryshim: `Some(meta)` = vendos/përditëso, `None` = fshi.
pub type Update = (Key, Option<AccountMeta>);

/// Mbi këtë numër ndryshimesh, degët përpunohen paralelisht.
const PAR_THRESHOLD: usize = 2048;

#[derive(Clone, Debug)]
pub struct LeafNode {
    pub key: Key,
    pub meta: AccountMeta,
    hash: Hash,
}

impl LeafNode {
    fn new(key: Key, meta: AccountMeta) -> Self {
        let hash = leaf_hash(&key, &meta.value_hash());
        LeafNode { key, meta, hash }
    }
}

#[derive(Clone, Debug)]
pub struct InternalNode {
    left: Node,
    right: Node,
    hash: Hash,
}

#[derive(Clone, Debug, Default)]
pub enum Node {
    #[default]
    Empty,
    Leaf(Box<LeafNode>),
    Internal(Box<InternalNode>),
}

impl Node {
    #[inline]
    pub fn hash(&self) -> Hash {
        match self {
            Node::Empty => EMPTY,
            Node::Leaf(l) => l.hash,
            Node::Internal(i) => i.hash,
        }
    }
}

/// Bashkon dy fëmijë në formën kanonike.
fn join(left: Node, right: Node) -> Node {
    match (left, right) {
        (Node::Empty, Node::Empty) => Node::Empty,
        // Një gjethe e vetme ngjitet lart: forma kompakte.
        (l @ Node::Leaf(_), Node::Empty) => l,
        (Node::Empty, r @ Node::Leaf(_)) => r,
        (left, right) => {
            let hash = internal_hash(&left.hash(), &right.hash());
            Node::Internal(Box::new(InternalNode { left, right, hash }))
        }
    }
}

/// Ndan një listë të renditur sipas bit-it `depth`.
#[inline]
fn split(ups: &[Update], depth: usize) -> (&[Update], &[Update]) {
    let i = ups.partition_point(|(k, _)| bit(k, depth) == 0);
    ups.split_at(i)
}

fn par_or_seq<A, B, RA, RB>(par: bool, a: A, b: B) -> (RA, RB)
where
    A: FnOnce() -> RA + Send,
    B: FnOnce() -> RB + Send,
    RA: Send,
    RB: Send,
{
    if par {
        rayon::join(a, b)
    } else {
        (a(), b())
    }
}

/// Ndërton një nënpemë nga e para (lista e renditur, pa dyfishime).
/// Llogaritë e gjalla (pa varre), të renditura.
fn live(ups: &[Update]) -> Vec<Update> {
    ups.iter().filter(|(_, m)| m.is_some()).cloned().collect()
}

/// Ndërton një nënpemë nga llogari të gjalla (pa varre: ato hiqen një herë, para rekursionit).
fn build(depth: usize, ups: &[Update]) -> Node {
    match ups.len() {
        0 => Node::Empty,
        1 => {
            let (k, m) = &ups[0];
            Node::Leaf(Box::new(LeafNode::new(*k, m.clone().expect("build merr vetëm llogari të gjalla"))))
        }
        _ => {
            let (l, r) = split(ups, depth);
            let (ln, rn) = par_or_seq(
                ups.len() > PAR_THRESHOLD,
                || build(depth + 1, l),
                || build(depth + 1, r),
            );
            join(ln, rn)
        }
    }
}

/// Zbaton ndryshimet; kthen nyjen e re dhe ndryshimin në numrin e llogarive.
fn apply(node: Node, depth: usize, ups: &[Update]) -> (Node, isize) {
    if ups.is_empty() {
        return (node, 0);
    }
    match node {
        Node::Empty => {
            let l = live(ups);
            let n = l.len() as isize;
            (build(depth, &l), n)
        }
        Node::Leaf(leaf) => {
            // Gjethja ekzistuese bashkohet me ndryshimet (nëse nuk mbishkruhet prej tyre).
            match ups.binary_search_by(|(k, _)| k.cmp(&leaf.key)) {
                Ok(_) => {
                    let l = live(ups);
                    let n = l.len() as isize - 1;
                    (build(depth, &l), n)
                }
                Err(pos) => {
                    let mut merged = Vec::with_capacity(ups.len() + 1);
                    merged.extend(ups[..pos].iter().filter(|(_, m)| m.is_some()).cloned());
                    let added = merged.len() as isize;
                    merged.push((leaf.key, Some(leaf.meta)));
                    let before = merged.len();
                    merged.extend(ups[pos..].iter().filter(|(_, m)| m.is_some()).cloned());
                    let added = added + (merged.len() - before) as isize;
                    (build(depth, &merged), added)
                }
            }
        }
        Node::Internal(mut inner) => {
            let (l, r) = split(ups, depth);
            let left = std::mem::take(&mut inner.left);
            let right = std::mem::take(&mut inner.right);
            let ((ln, d0), (rn, d1)) = par_or_seq(
                ups.len() > PAR_THRESHOLD,
                || apply(left, depth + 1, l),
                || apply(right, depth + 1, r),
            );
            (join(ln, rn), d0 + d1)
        }
    }
}

/// Renditet sipas çelësit; për çelësa të dyfishtë mbetet i fundit (sipas renditjes fillestare).
pub fn normalize_updates(ups: &mut Vec<Update>) {
    // Renditje e qëndrueshme: dyfishimet ruajnë radhën e mbërritjes.
    ups.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out: Vec<Update> = Vec::with_capacity(ups.len());
    for u in ups.drain(..) {
        match out.last_mut() {
            Some(last) if last.0 == u.0 => *last = u,
            _ => out.push(u),
        }
    }
    *ups = out;
}

#[derive(Default, Clone)]
pub struct Tree {
    root: Node,
    len: usize,
}

impl Tree {
    pub fn new() -> Self {
        Tree::default()
    }

    /// Ndërton pemën nga një listë llogarish (p.sh. snapshot-i).
    pub fn from_accounts(mut ups: Vec<Update>) -> Self {
        normalize_updates(&mut ups);
        ups.retain(|(_, m)| m.is_some());
        let len = ups.len();
        Tree { root: build(0, &ups), len }
    }

    /// Vula globale (root-i).
    pub fn root(&self) -> Hash {
        self.root.hash()
    }

    /// Numri i llogarive në pemë.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Zbaton një grup ndryshimesh dhe kthen root-in e ri.
    pub fn apply_batch(&mut self, mut ups: Vec<Update>) -> Hash {
        normalize_updates(&mut ups);
        // Faster-2: numërimi del nga vetë zbatimi, pa një `get` për çdo ndryshim.
        let root = std::mem::take(&mut self.root);
        let (root, delta) = apply(root, 0, &ups);
        self.root = root;
        self.len = (self.len as isize + delta) as usize;
        self.root()
    }

    pub fn get(&self, key: &Key) -> Option<&AccountMeta> {
        let mut node = &self.root;
        let mut depth = 0;
        loop {
            match node {
                Node::Empty => return None,
                Node::Leaf(l) => return (l.key == *key).then_some(&l.meta),
                Node::Internal(i) => {
                    node = if bit(key, depth) == 0 { &i.left } else { &i.right };
                    depth += 1;
                }
            }
        }
    }

    /// Dëftesa për një çelës (përfshirje ose mospërfshirje).
    pub fn prove(&self, key: &Key) -> (Proof, Option<&AccountMeta>) {
        let mut node = &self.root;
        let mut depth = 0;
        let mut siblings = Vec::new();
        loop {
            match node {
                Node::Empty => return (Proof { siblings, leaf: None }, None),
                Node::Leaf(l) => {
                    let meta = (l.key == *key).then_some(&l.meta);
                    let proof = Proof { siblings, leaf: Some((l.key, l.meta.value_hash())) };
                    return (proof, meta);
                }
                Node::Internal(i) => {
                    if bit(key, depth) == 0 {
                        siblings.push(i.right.hash());
                        node = &i.left;
                    } else {
                        siblings.push(i.left.hash());
                        node = &i.right;
                    }
                    depth += 1;
                }
            }
        }
    }
}

/// Puna e përgatitur nga `StateTree::prepare_slot`, e çdo peme sipas llojit të saj.
pub type Prepared = Box<dyn std::any::Any + Send>;

/// Gjurma e zinxhirit: (slot, bank_hash) e slotit të fundit të rrënjosur, nga SlotHashes.
pub type Fingerprint = (u64, [u8; 32]);

/// Gjendja e ruajtur e një peme që mund të vazhdojë pas rinisjes (vetëm për pemën në disk).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resume {
    pub slot: u64,
    pub root: Hash,
    pub len: usize,
    pub complete: bool,
    /// Gjurma e zinxhirit të slotit të ruajtur; `None` kur ende s'dihet (p.sh. menjëherë pas snapshot-it).
    pub fingerprint: Option<Fingerprint>,
}

/// Përgjigja e një dëftese: sloti, root-i në atë slot, dëftesa dhe meta (nëse llogaria ekziston).
pub type ProofAt = (u64, Hash, Proof, Option<AccountMeta>);

/// Ndërfaqja e përbashkët për pemën në RAM dhe pemën në disk.
pub trait StateTree: Send + Sync {
    fn root(&self) -> Hash;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Llogari nga snapshot-i, të gjitha nga sloti `slot` (mund të thirret disa herë, me copa).
    /// Kur e njëjta llogari vjen disa herë, fiton sloti më i ri; në të njëjtin slot, e fundit (F-02).
    ///
    /// Çdo gabim (disk, të dhëna të dëmtuara) kthehet si `Err` dhe e ndalon ziles (F-12):
    /// pema nuk vazhdon kurrë me një gjendje që s'është e sigurt.
    fn stage_snapshot(&mut self, slot: u64, ups: Vec<Update>) -> Result<(), String>;
    /// Ndërton pemën nga llogaritë e snapshot-it.
    fn finish_snapshot(&mut self, slot: u64, complete: bool) -> Result<Hash, String>;
    /// Zbaton ndryshimet e një sloti të rrënjosur.
    fn apply_slot(&mut self, slot: u64, ups: Vec<Update>, fingerprint: Option<Fingerprint>) -> Result<Hash, String>;
    /// Faster-1: pjesa e rëndë e `apply_slot` (leximet dhe llogaritja), vetëm me `&self`, që
    /// dëftesat të shërbehen ndërkohë. Parazgjedhja s'bën gjë dhe e lë punën te `commit_slot`.
    fn prepare_slot(&self, _slot: u64, ups: Vec<Update>, fingerprint: Option<Fingerprint>) -> Result<Prepared, String> {
        Ok(Box::new((ups, fingerprint)))
    }
    /// Shkruan atë që përgatiti `prepare_slot`. Asgjë tjetër s'duhet të ndryshojë pemën mes tyre.
    fn commit_slot(&mut self, slot: u64, p: Prepared) -> Result<Hash, String> {
        let (ups, fp) = *p.downcast::<(Vec<Update>, Option<Fingerprint>)>().map_err(|_| "commit_slot: përgatitje e gabuar")?;
        self.apply_slot(slot, ups, fp)
    }
    /// Dëftesa në slotin e fundit (`at = None`) ose në një slot të kaluar.
    fn prove_at(&self, key: &Key, at: Option<u64>) -> Result<ProofAt, String>;
    /// Slotet për të cilat mund të jepen dëftesa: (më i vjetri, më i riu).
    fn provable_range(&self) -> Option<(u64, u64)>;
    /// Gjendja e ruajtur nga një nisje e mëparshme, nëse ka.
    fn resume_point(&self) -> Option<Resume> {
        None
    }
    /// Shënon gjendjen e ruajtur si të pavlefshme (p.sh. kur zbulohet zinxhir tjetër). Nuk fshin
    /// gjë tani (F-13): rindërtimi bëhet në nisjen e ardhshme. Gabimi i shkrimit raportohet (P-03).
    fn invalidate(&mut self, _why: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Pema në RAM, e mbështjellë për ndërfaqen [`StateTree`]. Pa histori.
#[derive(Default)]
pub struct MemTree {
    pub tree: Tree,
    slot: Option<u64>,
    /// pubkey → (sloti i versionit, meta); `None` = e mbyllur.
    staged: std::collections::HashMap<Key, (u64, Option<AccountMeta>)>,
}

impl MemTree {
    pub fn new() -> Self {
        Self::default()
    }
}

impl StateTree for MemTree {
    fn root(&self) -> Hash {
        self.tree.root()
    }
    fn len(&self) -> usize {
        self.tree.len()
    }
    fn stage_snapshot(&mut self, slot: u64, ups: Vec<Update>) -> Result<(), String> {
        for (k, m) in ups {
            match self.staged.get(&k) {
                Some((s, _)) if *s > slot => {}
                _ => {
                    self.staged.insert(k, (slot, m));
                }
            }
        }
        Ok(())
    }
    fn finish_snapshot(&mut self, slot: u64, _complete: bool) -> Result<Hash, String> {
        let accs = std::mem::take(&mut self.staged).into_iter().filter_map(|(k, (_, m))| m.map(|m| (k, Some(m)))).collect();
        self.tree = Tree::from_accounts(accs);
        self.slot = Some(slot);
        Ok(self.tree.root())
    }
    fn apply_slot(&mut self, slot: u64, ups: Vec<Update>, _fingerprint: Option<Fingerprint>) -> Result<Hash, String> {
        self.slot = Some(slot);
        Ok(self.tree.apply_batch(ups))
    }
    fn prove_at(&self, key: &Key, at: Option<u64>) -> Result<ProofAt, String> {
        let slot = self.slot.unwrap_or(0);
        if let Some(a) = at {
            if a != slot {
                return Err(format!("pema në RAM ka vetëm slotin e fundit ({slot})"));
            }
        }
        let (p, m) = self.tree.prove(key);
        Ok((slot, self.tree.root(), p, m.cloned()))
    }
    fn provable_range(&self) -> Option<(u64, u64)> {
        self.slot.map(|s| (s, s))
    }
}

/// Implementim referencë, i ngadaltë por i thjeshtë: llogarit root-in direkt nga
/// lista e plotë. Përdoret në teste për të kontrolluar pemën.
pub fn reference_root(accounts: &[(Key, AccountMeta)]) -> Hash {
    fn go(depth: usize, items: &[(Key, AccountMeta)]) -> Hash {
        match items.len() {
            0 => EMPTY,
            1 => leaf_hash(&items[0].0, &items[0].1.value_hash()),
            _ => {
                let i = items.partition_point(|(k, _)| bit(k, depth) == 0);
                let (l, r) = items.split_at(i);
                // Nëse të gjithë shkojnë në njërën anë, ana tjetër është bosh.
                internal_hash(&go(depth + 1, l), &go(depth + 1, r))
            }
        }
    }
    let mut v = accounts.to_vec();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    go(0, &v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{rngs::StdRng, Rng, SeedableRng};
    use std::collections::BTreeMap;
    use zilja_verifier::{verify_tree_key, Verdict};

    fn rand_meta(rng: &mut StdRng) -> AccountMeta {
        let mut owner = [0u8; 32];
        rng.fill(&mut owner);
        let data: Vec<u8> = (0..rng.gen_range(0..64)).map(|_| rng.gen()).collect();
        AccountMeta::from_account(rng.gen_range(1..u64::MAX / 2), &owner, rng.gen_bool(0.05), &data)
    }

    fn rand_key(rng: &mut StdRng) -> Key {
        let mut k = [0u8; 32];
        rng.fill(&mut k);
        k
    }

    #[test]
    fn empty_tree() {
        let t = Tree::new();
        assert_eq!(t.root(), EMPTY);
        let k = [1u8; 32];
        let (p, m) = t.prove(&k);
        assert!(m.is_none());
        assert_eq!(verify_tree_key(&t.root(), &k, None, &p), Verdict::DoesNotExist);
    }

    #[test]
    fn matches_reference_and_is_order_independent() {
        let mut rng = StdRng::seed_from_u64(1);
        let accs: Vec<(Key, AccountMeta)> = (0..5000).map(|_| (rand_key(&mut rng), rand_meta(&mut rng))).collect();
        let expected = reference_root(&accs);

        // 1) E gjitha njëherësh.
        let t1 = Tree::from_accounts(accs.iter().map(|(k, m)| (*k, Some(m.clone()))).collect());
        assert_eq!(t1.root(), expected);
        assert_eq!(t1.len(), 5000);

        // 2) Me grupe të rastësishme, në radhë tjetër.
        let mut shuffled = accs.clone();
        for i in (1..shuffled.len()).rev() {
            let j = rng.gen_range(0..=i);
            shuffled.swap(i, j);
        }
        let mut t2 = Tree::new();
        for chunk in shuffled.chunks(377) {
            t2.apply_batch(chunk.iter().map(|(k, m)| (*k, Some(m.clone()))).collect());
        }
        assert_eq!(t2.root(), expected);
        assert_eq!(t2.len(), 5000);
    }

    #[test]
    fn random_updates_and_deletes_match_reference() {
        let mut rng = StdRng::seed_from_u64(2);
        let mut model: BTreeMap<Key, AccountMeta> = BTreeMap::new();
        let mut tree = Tree::new();
        let mut keys: Vec<Key> = Vec::new();
        for _round in 0..60 {
            let mut batch = Vec::new();
            for _ in 0..rng.gen_range(1..400) {
                let r: f64 = rng.gen();
                if r < 0.5 || keys.is_empty() {
                    let k = rand_key(&mut rng);
                    keys.push(k);
                    let m = rand_meta(&mut rng);
                    batch.push((k, Some(m)));
                } else if r < 0.85 {
                    let k = keys[rng.gen_range(0..keys.len())];
                    batch.push((k, Some(rand_meta(&mut rng))));
                } else {
                    let k = keys[rng.gen_range(0..keys.len())];
                    batch.push((k, None));
                }
            }
            for (k, m) in &batch {
                match m {
                    Some(m) => {
                        model.insert(*k, m.clone());
                    }
                    None => {
                        model.remove(k);
                    }
                }
            }
            tree.apply_batch(batch);
            let accs: Vec<_> = model.iter().map(|(k, m)| (*k, m.clone())).collect();
            assert_eq!(tree.root(), reference_root(&accs));
            assert_eq!(tree.len(), model.len());
        }
        // Fshi gjithçka: pema kthehet bosh.
        tree.apply_batch(model.keys().map(|k| (*k, None)).collect());
        assert_eq!(tree.root(), EMPTY);
        assert_eq!(tree.len(), 0);
    }

    #[test]
    fn duplicate_keys_in_batch_last_wins() {
        let mut rng = StdRng::seed_from_u64(3);
        let k = rand_key(&mut rng);
        let a = rand_meta(&mut rng);
        let b = rand_meta(&mut rng);
        let mut t = Tree::new();
        t.apply_batch(vec![(k, Some(a)), (k, Some(b.clone()))]);
        assert_eq!(t.get(&k), Some(&b));
    }

    #[test]
    fn proofs_for_10k_random_accounts() {
        let mut rng = StdRng::seed_from_u64(4);
        let accs: Vec<(Key, AccountMeta)> = (0..10_000).map(|_| (rand_key(&mut rng), rand_meta(&mut rng))).collect();
        let t = Tree::from_accounts(accs.iter().map(|(k, m)| (*k, Some(m.clone()))).collect());
        let root = t.root();
        let mut max = 0;
        for (k, m) in &accs {
            let (p, got) = t.prove(k);
            assert_eq!(got, Some(m));
            let bytes = p.to_bytes();
            max = max.max(bytes.len());
            let p2 = Proof::from_bytes(&bytes).unwrap();
            assert_eq!(verify_tree_key(&root, k, Some(m), &p2), Verdict::Exists);
        }
        assert!(max <= 10 * 1024, "dëftesa më e madhe: {max} bajt");
        // Mospërfshirje për çelësa që s'ekzistojnë.
        for _ in 0..1000 {
            let k = rand_key(&mut rng);
            let (p, got) = t.prove(&k);
            assert!(got.is_none());
            assert_eq!(verify_tree_key(&root, &k, None, &p), Verdict::DoesNotExist);
            // E njëjta dëftesë nuk mund të "provojë" një llogari të shpikur.
            let fake = rand_meta(&mut rng);
            assert_eq!(verify_tree_key(&root, &k, Some(&fake), &p), Verdict::Invalid);
        }
    }

    #[test]
    fn deep_shared_prefix() {
        // Dy çelësa që ndryshojnë vetëm në bit-in e fundit: rrugë 255 nivele.
        let a = [0u8; 32];
        let mut b = [0u8; 32];
        b[31] = 1;
        let mut rng = StdRng::seed_from_u64(5);
        let (ma, mb) = (rand_meta(&mut rng), rand_meta(&mut rng));
        let t = Tree::from_accounts(vec![(a, Some(ma.clone())), (b, Some(mb.clone()))]);
        assert_eq!(t.root(), reference_root(&[(a, ma.clone()), (b, mb.clone())]));
        let (p, _) = t.prove(&b);
        assert_eq!(p.siblings.len(), 256);
        // Bitmap-i i ngjesh vëllezërit bosh: 3 + 32 + 1*32 + 1 + 64.
        assert_eq!(p.to_bytes().len(), 132);
        assert_eq!(verify_tree_key(&t.root(), &b, Some(&mb), &p), Verdict::Exists);
    }
}
