//! R5-2: dëftesë e përbashkët për shumë llogari (multi-proof) mbi të njëjtin root.
//!
//! Rrugët e disa çelësave ndajnë nyjet e sipërme të pemës. Një dëftesë e veçantë për secilin
//! përsërit vëllezërit e përbashkët dhe përfshin si "vëlla" nyje që verifikuesi mund t'i llogarisë
//! vetë nga çelësat e tjerë. Multi-proof-i mban vetëm vëllezërit që s'llogariten dot.
//!
//! Formati (SPEC.md §4b), kanonik:
//! ```text
//! version  u8      = 0x11
//! count    u16 LE  = numri i çelësave (1..=MAX_MULTI_KEYS), të renditur rritës sipas tree_key, pa dyfishe
//! count ×  { depth u16 LE (0..=256) ‖ leaf_flag u8 (0|1) ‖ [leaf_key[32] ‖ value_hash[32]] }
//! nsib     u32 LE  = numri i vëllezërve
//! bitmap   ceil(nsib/8): bit 1 (MSB i pari) = vëlla jo-bosh; bitët mbushës 0
//! vëllezërit jo-bosh, 32 bajte secili
//! ```
//! Çelësat vetë s'kodohen: verifikuesi i di (i llogarit nga pubkey-t që pyeti).
//!
//! Verifikimi ndjek pemën nga root-i: në thellësinë `i`, bashkësia e çelësave ndahet sipas bitit
//! `i`; një anë pa çelës merr vëllain e radhës nga lista. Një çelës mbaron në thellësinë e tij
//! (`depth`): aty gjithë çelësat e nënpemës duhet të kenë të njëjtin përfundim. Rregulli kanonik i
//! dëftesës së vetme (vëllai i fundit s'është bosh) përgjithësohet: një nyje me një fëmijë bosh dhe
//! tjetrin bosh ose gjethe të njohur është e pavlefshme.

use crate::{bit, common_prefix_len, internal_hash, leaf_hash, tree_key, AccountMeta, Hash, Key, Proof, Verdict, EMPTY, MAX_DEPTH};

pub const MULTI_VERSION: u8 = 0x11;
/// Sa llogari më së shumti në një multi-proof (dhe në një thirrje RPC).
pub const MAX_MULTI_KEYS: usize = 256;

/// Përfundimi i rrugës së një çelësi: thellësia dhe gjethja (ose bosh).
pub type Terminal = (u16, Option<(Key, Hash)>);

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct MultiProof {
    /// Një për çdo çelës, në radhën rritëse të `tree_key`.
    pub terminals: Vec<Terminal>,
    pub siblings: Vec<Hash>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Empty,
    Leaf,
    /// Nyje e brendshme e llogaritur, ose vëlla jo-bosh (lloji i panjohur).
    Other,
}

fn sorted_unique(keys: &[Key]) -> bool {
    keys.windows(2).all(|w| w[0] < w[1])
}

/// Indeksi i parë në `keys[lo..hi]` me bit-in `i` = 1 (çelësat janë të renditur dhe ndajnë prefiksin `i`).
fn split(keys: &[Key], lo: usize, hi: usize, i: usize) -> usize {
    lo + keys[lo..hi].partition_point(|k| bit(k, i) == 0)
}

impl MultiProof {
    /// Root-i që del për çelësat `keys` (rritës, pa dyfishe, sa `terminals`), ose `None` kur dëftesa
    /// s'është e mirëformuar.
    pub fn compute_root(&self, keys: &[Key]) -> Option<Hash> {
        if keys.is_empty() || keys.len() != self.terminals.len() || keys.len() > MAX_MULTI_KEYS || !sorted_unique(keys) {
            return None;
        }
        let mut next = 0usize;
        let (h, _) = self.rec(keys, 0, keys.len(), 0, &mut next)?;
        (next == self.siblings.len()).then_some(h)
    }

    fn rec(&self, keys: &[Key], lo: usize, hi: usize, i: usize, next: &mut usize) -> Option<(Hash, Kind)> {
        let ends_here = self.terminals[lo..hi].iter().any(|(d, _)| *d as usize == i);
        if ends_here {
            // Gjithë çelësat e kësaj nënpeme mbarojnë këtu, në të njëjtin përfundim.
            let t = &self.terminals[lo];
            if self.terminals[lo..hi].iter().any(|x| x != t) {
                return None;
            }
            return Some(match &t.1 {
                Some((lk, vh)) => {
                    if keys[lo..hi].iter().any(|k| common_prefix_len(lk, k) < i) {
                        return None;
                    }
                    (leaf_hash(lk, vh), Kind::Leaf)
                }
                None => (EMPTY, Kind::Empty),
            });
        }
        if i >= MAX_DEPTH || self.terminals[lo..hi].iter().any(|(d, _)| (*d as usize) < i) {
            return None;
        }
        let mid = split(keys, lo, hi, i);
        let side = |a: usize, b: usize, next: &mut usize| -> Option<(Hash, Kind)> {
            if a < b {
                self.rec(keys, a, b, i + 1, next)
            } else {
                let s = *self.siblings.get(*next)?;
                *next += 1;
                Some((s, if s == EMPTY { Kind::Empty } else { Kind::Other }))
            }
        };
        let (l, lk) = side(lo, mid, next)?;
        let (r, rk) = side(mid, hi, next)?;
        let bad = |x: Kind, y: Kind| x == Kind::Empty && (y == Kind::Empty || y == Kind::Leaf);
        if bad(lk, rk) || bad(rk, lk) {
            return None;
        }
        Some((internal_hash(&l, &r), Kind::Other))
    }

    /// Ndërton multi-proof-in nga dëftesat e veçanta të çelësave (të gjitha nga i njëjti root).
    /// `entries` mund të jenë në çdo radhë; dyfishet hiqen. Gabim kur dëftesat s'përputhen.
    pub fn from_proofs(entries: &[(Key, Proof)]) -> Result<(Vec<Key>, MultiProof), String> {
        let mut e: Vec<&(Key, Proof)> = entries.iter().collect();
        e.sort_by_key(|a| a.0);
        e.dedup_by(|a, b| a.0 == b.0);
        if e.is_empty() || e.len() > MAX_MULTI_KEYS {
            return Err(format!("duhen 1..={MAX_MULTI_KEYS} çelësa"));
        }
        let keys: Vec<Key> = e.iter().map(|x| x.0).collect();
        let proofs: Vec<&Proof> = e.iter().map(|x| &x.1).collect();
        let terminals: Vec<Terminal> = proofs.iter().map(|p| (p.siblings.len() as u16, p.leaf)).collect();
        let mut siblings = Vec::new();
        fn gen(keys: &[Key], proofs: &[&Proof], terminals: &[Terminal], lo: usize, hi: usize, i: usize, out: &mut Vec<Hash>) -> Result<(), String> {
            if terminals[lo..hi].iter().any(|(d, _)| *d as usize == i) {
                return if terminals[lo..hi].iter().all(|t| *t == terminals[lo]) { Ok(()) } else { Err("dëftesat s'përputhen (përfundime të ndryshme)".into()) };
            }
            let mid = split(keys, lo, hi, i);
            // Vëllai i një ane pa çelës: i njëjtë në dëftesat e gjithë çelësave të anës tjetër.
            let sib = |a: usize, b: usize| -> Result<Hash, String> {
                let s = proofs[a].siblings[i];
                if proofs[a..b].iter().any(|p| p.siblings[i] != s) {
                    return Err("dëftesat s'përputhen (vëllezër të ndryshëm)".into());
                }
                Ok(s)
            };
            if lo == mid {
                out.push(sib(mid, hi)?);
            } else {
                gen(keys, proofs, terminals, lo, mid, i + 1, out)?;
            }
            if mid == hi {
                out.push(sib(lo, mid)?);
            } else {
                gen(keys, proofs, terminals, mid, hi, i + 1, out)?;
            }
            Ok(())
        }
        if terminals.iter().any(|(d, _)| *d as usize > MAX_DEPTH) {
            return Err("dëftesë më e thellë se 256".into());
        }
        gen(&keys, &proofs, &terminals, 0, keys.len(), 0, &mut siblings)?;
        Ok((keys, MultiProof { terminals, siblings }))
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let nonempty = self.siblings.iter().filter(|s| **s != EMPTY).count();
        let mut out = Vec::with_capacity(3 + self.terminals.len() * 67 + 4 + self.siblings.len().div_ceil(8) + nonempty * 32);
        out.push(MULTI_VERSION);
        out.extend_from_slice(&(self.terminals.len() as u16).to_le_bytes());
        for (d, leaf) in &self.terminals {
            out.extend_from_slice(&d.to_le_bytes());
            match leaf {
                Some((k, vh)) => {
                    out.push(1);
                    out.extend_from_slice(k);
                    out.extend_from_slice(vh);
                }
                None => out.push(0),
            }
        }
        out.extend_from_slice(&(self.siblings.len() as u32).to_le_bytes());
        let mut bitmap = vec![0u8; self.siblings.len().div_ceil(8)];
        for (i, s) in self.siblings.iter().enumerate() {
            if *s != EMPTY {
                bitmap[i / 8] |= 1 << (7 - (i % 8));
            }
        }
        out.extend_from_slice(&bitmap);
        for s in self.siblings.iter().filter(|s| **s != EMPTY) {
            out.extend_from_slice(s);
        }
        out
    }

    pub fn from_bytes(b: &[u8]) -> Option<MultiProof> {
        let mut pos = 0usize;
        let mut take = |n: usize| -> Option<&[u8]> {
            let s = b.get(pos..pos.checked_add(n)?)?;
            pos += n;
            Some(s)
        };
        if take(1)?[0] != MULTI_VERSION {
            return None;
        }
        let count = u16::from_le_bytes(take(2)?.try_into().ok()?) as usize;
        if count == 0 || count > MAX_MULTI_KEYS {
            return None;
        }
        let mut terminals = Vec::with_capacity(count);
        for _ in 0..count {
            let d = u16::from_le_bytes(take(2)?.try_into().ok()?);
            if d as usize > MAX_DEPTH {
                return None;
            }
            let leaf = match take(1)?[0] {
                0 => None,
                1 => {
                    let k: Key = take(32)?.try_into().ok()?;
                    let vh: Hash = take(32)?.try_into().ok()?;
                    Some((k, vh))
                }
                _ => return None,
            };
            terminals.push((d, leaf));
        }
        let nsib = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
        // Çdo çelës ka ≤ 256 vëllezër: një numër më i madh s'mund të jetë kanonik.
        if nsib > count * MAX_DEPTH {
            return None;
        }
        let bitmap = take(nsib.div_ceil(8))?.to_vec();
        if !nsib.is_multiple_of(8) && bitmap[nsib / 8] & (0xFF >> (nsib % 8)) != 0 {
            return None;
        }
        let mut siblings = Vec::with_capacity(nsib);
        for i in 0..nsib {
            if (bitmap[i / 8] >> (7 - (i % 8))) & 1 == 1 {
                let h: Hash = take(32)?.try_into().ok()?;
                if h == EMPTY {
                    return None;
                }
                siblings.push(h);
            } else {
                siblings.push(EMPTY);
            }
        }
        (pos == b.len()).then_some(MultiProof { terminals, siblings })
    }
}

/// Verifikon pretendimet për shumë llogari me një multi-proof. Radha e `claims` s'ka rëndësi; një
/// pubkey mund të përsëritet. Kur dëftesa s'është e vlefshme për root-in, çdo pretendim del `Invalid`.
pub fn verify_accounts(root: &Hash, claims: &[(Key, Option<&AccountMeta>)], proof: &MultiProof) -> Vec<Verdict> {
    let tks: Vec<Key> = claims.iter().map(|(pk, _)| tree_key(pk)).collect();
    let mut keys = tks.clone();
    keys.sort();
    keys.dedup();
    if proof.compute_root(&keys).as_ref() != Some(root) {
        return vec![Verdict::Invalid; claims.len()];
    }
    claims
        .iter()
        .zip(&tks)
        .map(|((_, meta), tk)| {
            let idx = keys.binary_search(tk).expect("çelësi është në listë");
            let leaf = &proof.terminals[idx].1;
            match (meta, leaf) {
                (Some(m), Some((lk, vh))) if lk == tk && *vh == m.value_hash() => Verdict::Exists,
                (None, Some((lk, _))) if lk != tk => Verdict::DoesNotExist,
                (None, None) => Verdict::DoesNotExist,
                _ => Verdict::Invalid,
            }
        })
        .collect()
}
