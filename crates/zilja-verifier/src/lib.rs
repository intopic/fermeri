//! # zilja-verifier
//!
//! Verifikuesi i lehtë i "Ziles së fshatit": merr një root (vulën globale të
//! gjendjes), një llogari dhe një dëftesë Merkle, dhe përgjigjet po ose jo.
//!
//! Formati i plotë është te `docs/SPEC.md`. Shkurt:
//!
//! - `data_hash  = SHA256(data)`
//! - `value_hash = SHA256("zilja/account/v1" ‖ lamports LE ‖ owner ‖ executable ‖ data_len LE ‖ data_hash)`
//! - `tree_key   = SHA256("zilja/key/v2" ‖ pubkey)` (F-09: vendi në pemë)
//! - `leaf       = SHA256(0x00 ‖ tree_key ‖ value_hash)`
//! - `internal   = SHA256(0x01 ‖ left ‖ right)`
//! - `empty      = [0; 32]`
//!
//! Pema është një sparse Merkle tree kompakt mbi `tree_key` (256 bit): një nënpemë me
//! vetëm një llogari përfaqësohet nga vetë gjethja. Meqë vendi është hash i pubkey-it,
//! askush s'mund të krijojë me qëllim llogari me prefiks të gjatë të përbashkët.

#![forbid(unsafe_code)]

use sha2::{Digest, Sha256};

#[cfg(feature = "attest")]
pub mod attest;
pub mod wire;
pub mod multi;

pub type Hash = [u8; 32];
pub type Key = [u8; 32];

/// Hash-i i nyjes bosh.
pub const EMPTY: Hash = [0u8; 32];
/// Prefiksi i domenit për hash-in e vlerës së llogarisë.
pub const ACCOUNT_DOMAIN: &[u8] = b"zilja/account/v1";
/// Prefiksi i domenit për vendin e llogarisë në pemë.
pub const KEY_DOMAIN: &[u8] = b"zilja/key/v2";
/// Versioni i formatit binar të dëftesës (2 = pema mbi `tree_key`).
pub const PROOF_VERSION: u8 = 2;
/// Thellësia maksimale e pemës (bit-ët e çelësit).
pub const MAX_DEPTH: usize = 256;

/// Të dhënat e llogarisë që hyjnë në vulë.
///
/// Vetë `data` nuk ruhet: ruhet `data_hash`, që klienti ta krahasojë me të
/// dhënat që merr nga çdo RPC (edhe i pabesueshëm).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AccountMeta {
    pub lamports: u64,
    pub owner: [u8; 32],
    pub executable: bool,
    pub data_len: u64,
    pub data_hash: Hash,
}

impl AccountMeta {
    /// Ndërton meta-n nga të dhënat e plota të llogarisë.
    pub fn from_account(lamports: u64, owner: &[u8; 32], executable: bool, data: &[u8]) -> Self {
        AccountMeta {
            lamports,
            owner: *owner,
            executable,
            data_len: data.len() as u64,
            data_hash: sha256(data),
        }
    }

    /// `value_hash` sipas SPEC.md.
    pub fn value_hash(&self) -> Hash {
        let mut h = Sha256::new();
        h.update(ACCOUNT_DOMAIN);
        h.update(self.lamports.to_le_bytes());
        h.update(self.owner);
        h.update([self.executable as u8]);
        h.update(self.data_len.to_le_bytes());
        h.update(self.data_hash);
        h.finalize().into()
    }
}

pub fn sha256(data: &[u8]) -> Hash {
    Sha256::digest(data).into()
}

/// Vendi i një llogarie në pemë: `SHA256("zilja/key/v2" ‖ pubkey)`.
pub fn tree_key(pubkey: &Key) -> Key {
    let mut h = Sha256::new();
    h.update(KEY_DOMAIN);
    h.update(pubkey);
    h.finalize().into()
}

/// Hash-i i një gjetheje.
pub fn leaf_hash(key: &Key, value_hash: &Hash) -> Hash {
    let mut h = Sha256::new();
    h.update([0x00]);
    h.update(key);
    h.update(value_hash);
    h.finalize().into()
}

/// Hash-i i një nyjeje të brendshme.
pub fn internal_hash(left: &Hash, right: &Hash) -> Hash {
    let mut h = Sha256::new();
    h.update([0x01]);
    h.update(left);
    h.update(right);
    h.finalize().into()
}

/// Bit-i `i` i çelësit (0 = majtas), duke nisur nga bit-i më i lartë i bajtit të parë.
#[inline]
pub fn bit(key: &Key, i: usize) -> u8 {
    (key[i / 8] >> (7 - (i % 8))) & 1
}

/// Sa bit-ë të parë kanë të përbashkët dy çelësa.
pub fn common_prefix_len(a: &Key, b: &Key) -> usize {
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        let d = x ^ y;
        if d != 0 {
            return i * 8 + d.leading_zeros() as usize;
        }
    }
    MAX_DEPTH
}

/// Dëftesa Merkle për një çelës.
///
/// `siblings[i]` është hash-i i vëllait në thellësinë `i` (nga root-i poshtë).
/// `leaf` është gjethja ku përfundon rruga, nëse ka: kur `leaf.0 == key`
/// dëftesa provon përfshirjen, përndryshe provon mospërfshirjen.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Proof {
    pub siblings: Vec<Hash>,
    pub leaf: Option<(Key, Hash)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProofError {
    Truncated,
    BadVersion(u8),
    TooDeep,
    TrailingBytes,
    BadLeafFlag(u8),
    /// Bit-e mbushëse në bitmap, ose një vëlla "jo-bosh" që është bosh: e njëjta dëftesë do të
    /// kishte dy kodime (u gjet nga fuzz-i, Safer-3).
    NonCanonical,
}

impl core::fmt::Display for ProofError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ProofError::Truncated => write!(f, "dëftesa është e cunguar"),
            ProofError::BadVersion(v) => write!(f, "version i panjohur i dëftesës: {v}"),
            ProofError::TooDeep => write!(f, "dëftesa është më e thellë se 256"),
            ProofError::TrailingBytes => write!(f, "bajte të tepërta në fund të dëftesës"),
            ProofError::BadLeafFlag(v) => write!(f, "flamur i pavlefshëm gjetheje: {v}"),
            ProofError::NonCanonical => write!(f, "kodim jo kanonik i dëftesës"),
        }
    }
}

impl std::error::Error for ProofError {}

impl Proof {
    /// Llogarit root-in që del nga kjo dëftesë për çelësin `key`.
    fn compute_root(&self, key: &Key) -> Option<Hash> {
        let depth = self.siblings.len();
        if depth > MAX_DEPTH {
            return None;
        }
        // Një pemë kanonike s'ka kurrë vëlla bosh në fund të rrugës: një gjethe me vëlla bosh
        // do të ishte ngjitur lart, dhe dy fëmijë bosh do të ishin një nyje bosh. Kështu çdo
        // çelës ka një dëftesë të vetme.
        if depth > 0 && self.siblings[depth - 1] == EMPTY {
            return None;
        }
        let mut cur = match &self.leaf {
            Some((k, vh)) => {
                // Gjethja duhet të jetë në rrugën e çelësit tonë.
                if common_prefix_len(k, key) < depth {
                    return None;
                }
                leaf_hash(k, vh)
            }
            None => EMPTY,
        };
        for i in (0..depth).rev() {
            let sib = &self.siblings[i];
            cur = if bit(key, i) == 0 {
                internal_hash(&cur, sib)
            } else {
                internal_hash(sib, &cur)
            };
        }
        Some(cur)
    }

    /// Provon që `key` ka vlerën `value_hash` nën `root`.
    pub fn verify_inclusion(&self, root: &Hash, key: &Key, value_hash: &Hash) -> bool {
        match &self.leaf {
            Some((k, vh)) if k == key && vh == value_hash => {
                self.compute_root(key).as_ref() == Some(root)
            }
            _ => false,
        }
    }

    /// Provon që `key` NUK ekziston nën `root`.
    pub fn verify_exclusion(&self, root: &Hash, key: &Key) -> bool {
        if let Some((k, _)) = &self.leaf {
            if k == key {
                return false;
            }
        }
        self.compute_root(key).as_ref() == Some(root)
    }

    /// Format binar kompakt (shih SPEC.md):
    /// `version u8 ‖ depth u16 LE ‖ bitmap ‖ vëllezërit jo-bosh ‖ leaf_flag u8 ‖ [key ‖ value_hash]`
    pub fn to_bytes(&self) -> Vec<u8> {
        let depth = self.siblings.len();
        let bitmap_len = depth.div_ceil(8);
        let mut out = Vec::with_capacity(3 + bitmap_len + depth * 32 + 65);
        out.push(PROOF_VERSION);
        out.extend_from_slice(&(depth as u16).to_le_bytes());
        let mut bitmap = vec![0u8; bitmap_len];
        for (i, s) in self.siblings.iter().enumerate() {
            if s != &EMPTY {
                bitmap[i / 8] |= 1 << (7 - (i % 8));
            }
        }
        out.extend_from_slice(&bitmap);
        for s in &self.siblings {
            if s != &EMPTY {
                out.extend_from_slice(s);
            }
        }
        match &self.leaf {
            Some((k, vh)) => {
                out.push(1);
                out.extend_from_slice(k);
                out.extend_from_slice(vh);
            }
            None => out.push(0),
        }
        out
    }

    pub fn from_bytes(b: &[u8]) -> Result<Self, ProofError> {
        let mut r = Reader { b, pos: 0 };
        let v = r.u8()?;
        if v != PROOF_VERSION {
            return Err(ProofError::BadVersion(v));
        }
        let depth = u16::from_le_bytes([r.u8()?, r.u8()?]) as usize;
        if depth > MAX_DEPTH {
            return Err(ProofError::TooDeep);
        }
        let bitmap = r.take(depth.div_ceil(8))?.to_vec();
        if !depth.is_multiple_of(8) && bitmap[depth / 8] & (0xFF >> (depth % 8)) != 0 {
            return Err(ProofError::NonCanonical);
        }
        let mut siblings = Vec::with_capacity(depth);
        for i in 0..depth {
            if (bitmap[i / 8] >> (7 - (i % 8))) & 1 == 1 {
                let h = r.hash()?;
                if h == EMPTY {
                    return Err(ProofError::NonCanonical);
                }
                siblings.push(h);
            } else {
                siblings.push(EMPTY);
            }
        }
        let leaf = match r.u8()? {
            0 => None,
            1 => Some((r.hash()?, r.hash()?)),
            x => return Err(ProofError::BadLeafFlag(x)),
        };
        if r.pos != b.len() {
            return Err(ProofError::TrailingBytes);
        }
        Ok(Proof { siblings, leaf })
    }
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ProofError> {
        let end = self.pos.checked_add(n).ok_or(ProofError::Truncated)?;
        let s = self.b.get(self.pos..end).ok_or(ProofError::Truncated)?;
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, ProofError> {
        Ok(self.take(1)?[0])
    }
    fn hash(&mut self) -> Result<Hash, ProofError> {
        let mut h = [0u8; 32];
        h.copy_from_slice(self.take(32)?);
        Ok(h)
    }
}

/// Rezultati i verifikimit të një llogarie.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Llogaria ekziston me pikërisht këto të dhëna.
    Exists,
    /// Llogaria nuk ekziston në këtë root.
    DoesNotExist,
    /// Dëftesa nuk përputhet me root-in: dikush po gënjen.
    Invalid,
}

/// Verifikimi kryesor që përdor kuleta / telefoni.
///
/// - `account = Some(meta)`: pretendimi është "llogaria ekziston me këtë meta".
/// - `account = None`: pretendimi është "llogaria nuk ekziston".
pub fn verify_account(root: &Hash, pubkey: &Key, account: Option<&AccountMeta>, proof: &Proof) -> Verdict {
    verify_tree_key(root, &tree_key(pubkey), account, proof)
}

/// Si [`verify_account`], por me vendin në pemë (`tree_key`) të dhënë drejtpërdrejt.
/// Për pemët që ndërtohen pa pubkey (teste, matje).
pub fn verify_tree_key(root: &Hash, key: &Key, account: Option<&AccountMeta>, proof: &Proof) -> Verdict {
    let key = *key;
    match account {
        Some(meta) => {
            if proof.verify_inclusion(root, &key, &meta.value_hash()) {
                Verdict::Exists
            } else {
                Verdict::Invalid
            }
        }
        None => {
            if proof.verify_exclusion(root, &key) {
                Verdict::DoesNotExist
            } else {
                Verdict::Invalid
            }
        }
    }
}

/// Kontrollon që të dhënat e llogarisë (marrë nga çdo RPC) përputhen me `data_hash` të vulosur.
pub fn data_matches(meta: &AccountMeta, data: &[u8]) -> bool {
    meta.data_len == data.len() as u64 && meta.data_hash == sha256(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_and_prefix() {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        assert_eq!(common_prefix_len(&a, &b), 256);
        a[0] = 0b1000_0000;
        assert_eq!(bit(&a, 0), 1);
        assert_eq!(common_prefix_len(&a, &b), 0);
        b[0] = 0b1000_0000;
        b[1] = 0b0001_0000;
        assert_eq!(common_prefix_len(&a, &b), 11);
    }

    #[test]
    fn single_leaf_tree() {
        let key = [7u8; 32];
        let meta = AccountMeta::from_account(5, &[1u8; 32], false, b"miell");
        let vh = meta.value_hash();
        let tk = tree_key(&key);
        let root = leaf_hash(&tk, &vh);
        let proof = Proof { siblings: vec![], leaf: Some((tk, vh)) };
        assert_eq!(verify_account(&root, &key, Some(&meta), &proof), Verdict::Exists);
        // Mospërfshirje për një çelës tjetër, me të njëjtën dëftesë.
        let other = [9u8; 32];
        assert_eq!(verify_account(&root, &other, None, &proof), Verdict::DoesNotExist);
        // Pretendim i rremë: balancë tjetër.
        let mut fake = meta.clone();
        fake.lamports = 1_000_000;
        assert_eq!(verify_account(&root, &key, Some(&fake), &proof), Verdict::Invalid);
    }

    #[test]
    fn proof_roundtrip_and_errors() {
        let p = Proof {
            siblings: vec![[1u8; 32], EMPTY, EMPTY, [2u8; 32]],
            leaf: Some(([3u8; 32], [4u8; 32])),
        };
        let b = p.to_bytes();
        // 1 + 2 + 1 bitmap + 2*32 + 1 + 64
        assert_eq!(b.len(), 133);
        assert_eq!(Proof::from_bytes(&b).unwrap(), p);
        assert_eq!(Proof::from_bytes(&b[..b.len() - 1]), Err(ProofError::Truncated));
        let mut extra = b.clone();
        extra.push(0);
        assert_eq!(Proof::from_bytes(&extra), Err(ProofError::TrailingBytes));
        let mut badv = b.clone();
        badv[0] = 9;
        assert_eq!(Proof::from_bytes(&badv), Err(ProofError::BadVersion(9)));
    }

    #[test]
    fn non_canonical_proofs_are_rejected() {
        // Gjethe me vëlla bosh në fund: një pemë kanonike do ta kishte ngjitur lart.
        let key = [7u8; 32];
        let meta = AccountMeta::from_account(5, &[1u8; 32], false, b"x");
        let (tk, vh) = (tree_key(&key), meta.value_hash());
        let leaf = leaf_hash(&tk, &vh);
        let root = if bit(&tk, 0) == 0 { internal_hash(&leaf, &EMPTY) } else { internal_hash(&EMPTY, &leaf) };
        let p = Proof { siblings: vec![EMPTY], leaf: Some((tk, vh)) };
        assert_eq!(verify_account(&root, &key, Some(&meta), &p), Verdict::Invalid);
        // Pozicion bosh me vëlla bosh.
        let p = Proof { siblings: vec![EMPTY], leaf: None };
        assert_eq!(verify_account(&internal_hash(&EMPTY, &EMPTY), &[1u8; 32], None, &p), Verdict::Invalid);
    }

    #[test]
    fn data_hash_check() {
        let meta = AccountMeta::from_account(1, &[0u8; 32], false, b"grure");
        assert!(data_matches(&meta, b"grure"));
        assert!(!data_matches(&meta, b"elb"));
    }
}
