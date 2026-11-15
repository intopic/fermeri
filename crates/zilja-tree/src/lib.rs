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
fn build(depth: usize, ups: &[Update]) -> Node {
    let live = ups.iter().filter(|(_, m)| m.is_some()).count();
    match live {
        0 => Node::Empty,
        1 => {
            let (k, m) = ups.iter().find(|(_, m)| m.is_some()).unwrap();
            Node::Leaf(Box::new(LeafNode::new(*k, m.clone().unwrap())))
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

fn apply(node: Node, depth: usize, ups: &[Update]) -> Node {
    if ups.is_empty() {
        return node;
    }
    match node {
        Node::Empty => build(depth, ups),
        Node::Leaf(leaf) => {
            // Gjethja ekzistuese bashkohet me ndryshimet (nëse nuk mbishkruhet prej tyre).
            match ups.binary_search_by(|(k, _)| k.cmp(&leaf.key)) {
                Ok(_) => build(depth, ups),
                Err(pos) => {
                    let mut merged = Vec::with_capacity(ups.len() + 1);
                    merged.extend_from_slice(&ups[..pos]);
                    merged.push((leaf.key, Some(leaf.meta)));
                    merged.extend_from_slice(&ups[pos..]);
                    build(depth, &merged)
                }
            }
        }
        Node::Internal(mut inner) => {
            let (l, r) = split(ups, depth);
            let left = std::mem::take(&mut inner.left);
            let right = std::mem::take(&mut inner.right);
            let (ln, rn) = par_or_seq(
                ups.len() > PAR_THRESHOLD,
                || apply(left, depth + 1, l),
                || apply(right, depth + 1, r),
            );
            join(ln, rn)
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
        let len = ups.iter().filter(|(_, m)| m.is_some()).count();
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
        // Numërimi i llogarive: kontrollo cilat ekzistonin para ndryshimit.
        let mut delta: isize = 0;
        for (k, m) in &ups {
            let existed = self.get(k).is_some();
            match (existed, m.is_some()) {
                (false, true) => delta += 1,
                (true, false) => delta -= 1,
                _ => {}
            }
        }
        let root = std::mem::take(&mut self.root);
        self.root = apply(root, 0, &ups);
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
    use zilja_verifier::{verify_account, Verdict};

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
        assert_eq!(verify_account(&t.root(), &k, None, &p), Verdict::DoesNotExist);
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
            assert_eq!(verify_account(&root, k, Some(m), &p2), Verdict::Exists);
        }
        assert!(max <= 10 * 1024, "dëftesa më e madhe: {max} bajt");
        // Mospërfshirje për çelësa që s'ekzistojnë.
        for _ in 0..1000 {
            let k = rand_key(&mut rng);
            let (p, got) = t.prove(&k);
            assert!(got.is_none());
            assert_eq!(verify_account(&root, &k, None, &p), Verdict::DoesNotExist);
            // E njëjta dëftesë nuk mund të "provojë" një llogari të shpikur.
            let fake = rand_meta(&mut rng);
            assert_eq!(verify_account(&root, &k, Some(&fake), &p), Verdict::Invalid);
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
        assert_eq!(verify_account(&t.root(), &b, Some(&mb), &p), Verdict::Exists);
    }
}
