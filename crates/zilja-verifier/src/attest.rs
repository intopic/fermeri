//! "Pleqtë firmosin ziles" (version jashtë protokollit), formati v2 (F-10).
//!
//! Mesazhi: `"zilja/attest/v2" ‖ genesis_hash ‖ slot LE ‖ root`. Hash-i i genesis-it e lidh
//! firmën me një zinxhir (mainnet, testnet, një klaster lokal): një firmë nga një klaster
//! tjetër nuk vlen këtu, edhe për të njëjtin slot.
//!
//! Firmos një çelës i veçantë (`signer`), jo identiteti i validatorit. Identiteti e autorizon për
//! një periudhë të kufizuar epokash (v2):
//! `"zilja/authorize/v2" ‖ genesis_hash ‖ signer ‖ from_epoch LE ‖ until_epoch LE`, e vlefshme për
//! `from_epoch ≤ epoka < until_epoch`, me `until_epoch - from_epoch ≤ MAX_AUTH_EPOCHS` (15 ≈ 30
//! ditë në mainnet). Një çelës i vjedhur vlen kështu vetëm deri në fund të periudhës; identiteti
//! thjesht s'e rinovon. Autorizimi v1 (pa afat) s'pranohet më. Kur `signer == validator`, s'ka
//! nevojë për autorizim (identiteti firmos vetë).
//!
//! Light client-i pranon root-in vetëm nëse validatorët që e firmosën kanë **më shumë se 2/3**
//! e stake-ut total. Bashkimi i firmave (BLS ose një set firmash në një objekt) është planifikuar
//! për versionin e protokollit (SIMD); logjika e kuorumit mbetet e njëjtë.

use crate::Hash;
use ed25519_dalek::{Signature, VerifyingKey};
use std::collections::HashSet;

pub const ATTEST_DOMAIN: &[u8] = b"zilja/attest/v2";
pub const AUTHORIZE_DOMAIN: &[u8] = b"zilja/authorize/v2";
/// Gjatësia maksimale e një autorizimi, në epoka (15 ≈ 30 ditë në mainnet).
pub const MAX_AUTH_EPOCHS: u64 = 15;

/// Mesazhi që firmos një validator.
pub fn attest_message(genesis: &Hash, slot: u64, root: &Hash) -> Vec<u8> {
    let mut m = Vec::with_capacity(ATTEST_DOMAIN.len() + 32 + 8 + 32);
    m.extend_from_slice(ATTEST_DOMAIN);
    m.extend_from_slice(genesis);
    m.extend_from_slice(&slot.to_le_bytes());
    m.extend_from_slice(root);
    m
}

/// Mesazhi me të cilin identiteti autorizon çelësin që firmos, për epokat `[from_epoch, until_epoch)`.
pub fn authorize_message(genesis: &Hash, signer: &[u8; 32], from_epoch: u64, until_epoch: u64) -> Vec<u8> {
    let mut m = Vec::with_capacity(AUTHORIZE_DOMAIN.len() + 64 + 16);
    m.extend_from_slice(AUTHORIZE_DOMAIN);
    m.extend_from_slice(genesis);
    m.extend_from_slice(signer);
    m.extend_from_slice(&from_epoch.to_le_bytes());
    m.extend_from_slice(&until_epoch.to_le_bytes());
    m
}

/// Autorizimi i një çelësi të veçantë nga identiteti, me afat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Authorization {
    pub from_epoch: u64,
    /// Epoka e parë kur autorizimi s'vlen më.
    pub until_epoch: u64,
    /// Firma e identitetit mbi `authorize_message(genesis, signer, from_epoch, until_epoch)`.
    pub signature: [u8; 64],
}

impl Authorization {
    /// Periudha është e mirëformuar: jo bosh dhe jo më e gjatë se `MAX_AUTH_EPOCHS`.
    pub fn well_formed(&self) -> bool {
        self.until_epoch > self.from_epoch && self.until_epoch - self.from_epoch <= MAX_AUTH_EPOCHS
    }
    /// Vlen në `epoch`?
    pub fn covers(&self, epoch: u64) -> bool {
        self.well_formed() && self.from_epoch <= epoch && epoch < self.until_epoch
    }
    /// Firma e identitetit `validator` mbi këtë autorizim për `signer` (pa kontrollin e epokës).
    pub fn signed_by(&self, validator: &[u8; 32], genesis: &Hash, signer: &[u8; 32]) -> bool {
        self.well_formed() && ed_ok(validator, &authorize_message(genesis, signer, self.from_epoch, self.until_epoch), &self.signature)
    }
}

#[derive(Clone, Debug)]
pub struct Attestation {
    /// Identiteti i validatorit (çelësi në tabelën e stake-ut).
    pub validator: [u8; 32],
    /// Çelësi që firmosi; `None` = vetë identiteti.
    pub signer: Option<[u8; 32]>,
    /// Autorizimi i `signer`-it nga identiteti (kur `signer` ≠ identiteti).
    pub authorization: Option<Authorization>,
    pub signature: [u8; 64],
}

fn ed_ok(pk: &[u8; 32], msg: &[u8], sig: &[u8; 64]) -> bool {
    VerifyingKey::from_bytes(pk).map(|vk| vk.verify_strict(msg, &Signature::from_bytes(sig)).is_ok()).unwrap_or(false)
}

impl Attestation {
    /// A është kjo firmë e vlefshme për (genesis, slot, root)? `epoch` është epoka e `slot`-it (sipas
    /// orarit të epokave të zinxhirit); një autorizim që s'e mbulon s'vlen.
    pub fn verify(&self, genesis: &Hash, epoch: u64, slot: u64, root: &Hash) -> bool {
        let signer = match (self.signer, self.authorization) {
            (None, _) => self.validator,
            (Some(s), _) if s == self.validator => s,
            (Some(s), Some(auth)) => {
                if !auth.covers(epoch) || !auth.signed_by(&self.validator, genesis, &s) {
                    return false;
                }
                s
            }
            (Some(_), None) => return false,
        };
        ed_ok(&signer, &attest_message(genesis, slot, root), &self.signature)
    }
}

/// Tabela e stake-ut për epokën (e marrë nga një burim i besuar ose e provuar më vete).
#[derive(Clone, Debug, Default)]
pub struct StakeTable {
    pub entries: Vec<([u8; 32], u64)>,
}

impl StakeTable {
    pub fn total(&self) -> u128 {
        self.entries.iter().map(|(_, s)| *s as u128).sum()
    }
    fn stake_of(&self, v: &[u8; 32]) -> Option<u64> {
        self.entries.iter().find(|(k, _)| k == v).map(|(_, s)| *s)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct QuorumReport {
    pub attested_stake: u128,
    pub total_stake: u128,
    pub valid_signatures: usize,
    pub rejected: usize,
    pub reached: bool,
}

/// Kontrollon firmat dhe kuorumin > 2/3 të stake-ut. `epoch` është epoka e `slot`-it (e njëjta me
/// atë të tabelës së stake-ut; thirrësi e kontrollon).
pub fn verify_quorum(genesis: &Hash, epoch: u64, slot: u64, root: &Hash, atts: &[Attestation], table: &StakeTable) -> QuorumReport {
    let mut seen = HashSet::new();
    let mut attested: u128 = 0;
    let mut valid = 0;
    let mut rejected = 0;
    for a in atts {
        // Një validator numërohet vetëm një herë. A15: shënohet si "i parë" vetëm pas një firme të
        // vlefshme, ndaj një firmë e pavlefshme s'e përjashton një të vlefshme të të njëjtit identitet,
        // dhe rezultati s'varet nga radha.
        if seen.contains(&a.validator) {
            rejected += 1;
            continue;
        }
        let Some(stake) = table.stake_of(&a.validator) else {
            rejected += 1;
            continue;
        };
        if a.verify(genesis, epoch, slot, root) {
            seen.insert(a.validator);
            valid += 1;
            attested += stake as u128;
        } else {
            rejected += 1;
        }
    }
    let total = table.total();
    QuorumReport {
        attested_stake: attested,
        total_stake: total,
        valid_signatures: valid,
        rejected,
        reached: total > 0 && attested * 3 > total * 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use rand::rngs::OsRng;

    const G: Hash = [0x6e; 32];
    /// Epoka e sloteve në teste.
    const E: u64 = 800;

    fn setup(n: usize) -> (Vec<SigningKey>, StakeTable) {
        let keys: Vec<SigningKey> = (0..n).map(|_| SigningKey::generate(&mut OsRng)).collect();
        let table = StakeTable { entries: keys.iter().map(|k| (k.verifying_key().to_bytes(), 100)).collect() };
        (keys, table)
    }

    fn sign(k: &SigningKey, slot: u64, root: &Hash) -> Attestation {
        Attestation {
            validator: k.verifying_key().to_bytes(),
            signer: None,
            authorization: None,
            signature: k.sign(&attest_message(&G, slot, root)).to_bytes(),
        }
    }

    #[test]
    fn quorum_needs_more_than_two_thirds() {
        let (keys, table) = setup(10);
        let root = [5u8; 32];
        let atts: Vec<_> = keys[..7].iter().map(|k| sign(k, 42, &root)).collect();
        assert!(verify_quorum(&G, E, 42, &root, &atts, &table).reached);
        // 6/10 = 60% nuk mjafton.
        assert!(!verify_quorum(&G, E, 42, &root, &atts[..6], &table).reached);
    }

    #[test]
    fn rejects_wrong_root_slot_genesis_duplicates_and_strangers() {
        let (keys, table) = setup(10);
        let root = [5u8; 32];
        let atts: Vec<_> = keys[..8].iter().map(|k| sign(k, 42, &root)).collect();
        assert_eq!(verify_quorum(&G, E, 42, &[6u8; 32], &atts, &table).valid_signatures, 0);
        assert_eq!(verify_quorum(&G, E, 43, &root, &atts, &table).valid_signatures, 0);
        // F-10: e njëjta firmë në një zinxhir tjetër nuk vlen.
        assert_eq!(verify_quorum(&[1u8; 32], E, 42, &root, &atts, &table).valid_signatures, 0);
        let dup: Vec<_> = (0..7).map(|_| sign(&keys[0], 42, &root)).collect();
        let r = verify_quorum(&G, E, 42, &root, &dup, &table);
        assert!(!r.reached);
        assert_eq!(r.rejected, 6);
        let (strangers, _) = setup(8);
        let s: Vec<_> = strangers.iter().map(|k| sign(k, 42, &root)).collect();
        assert!(!verify_quorum(&G, E, 42, &root, &s, &table).reached);
    }

    fn auth(id: &SigningKey, g: &Hash, s_pk: &[u8; 32], from: u64, until: u64) -> Authorization {
        Authorization { from_epoch: from, until_epoch: until, signature: id.sign(&authorize_message(g, s_pk, from, until)).to_bytes() }
    }

    #[test]
    fn dedicated_signer_needs_identity_authorization() {
        let id = SigningKey::generate(&mut OsRng);
        let signer = SigningKey::generate(&mut OsRng);
        let (s_pk, id_pk) = (signer.verifying_key().to_bytes(), id.verifying_key().to_bytes());
        let root = [9u8; 32];
        let sig = signer.sign(&attest_message(&G, 7, &root)).to_bytes();
        let a = Attestation { validator: id_pk, signer: Some(s_pk), authorization: Some(auth(&id, &G, &s_pk, E, E + 15)), signature: sig };
        assert!(a.verify(&G, E, 7, &root));
        // Pa autorizim, ose me autorizim për një zinxhir tjetër: s'vlen.
        assert!(!Attestation { authorization: None, ..a.clone() }.verify(&G, E, 7, &root));
        assert!(!Attestation { authorization: Some(auth(&id, &[1u8; 32], &s_pk, E, E + 15)), ..a.clone() }.verify(&G, E, 7, &root));
        // Një tjetër s'mund të pretendojë identitetin.
        let thief = SigningKey::generate(&mut OsRng);
        assert!(!Attestation { authorization: Some(auth(&thief, &G, &s_pk, E, E + 15)), ..a }.verify(&G, E, 7, &root));
    }

    /// Autorizimi v2 vlen vetëm për epokat `[from, until)`, me gjatësi 1..=15. Periudha është pjesë e
    /// mesazhit të firmosur: s'mund të zgjatet pa identitetin.
    #[test]
    fn authorization_expires_and_cannot_be_extended() {
        let id = SigningKey::generate(&mut OsRng);
        let signer = SigningKey::generate(&mut OsRng);
        let (s_pk, id_pk) = (signer.verifying_key().to_bytes(), id.verifying_key().to_bytes());
        let root = [9u8; 32];
        let sig = signer.sign(&attest_message(&G, 7, &root)).to_bytes();
        let with = |au: Authorization| Attestation { validator: id_pk, signer: Some(s_pk), authorization: Some(au), signature: sig };
        let a = with(auth(&id, &G, &s_pk, 800, 815));
        assert!(!a.verify(&G, 799, 7, &root), "para fillimit");
        assert!(a.verify(&G, 800, 7, &root));
        assert!(a.verify(&G, 814, 7, &root));
        assert!(!a.verify(&G, 815, 7, &root), "epoka e mbarimit s'përfshihet");
        // Zgjatja e periudhës pa firmë të re prish firmën.
        let mut longer = auth(&id, &G, &s_pk, 800, 815);
        longer.until_epoch = 816;
        assert!(!with(longer).verify(&G, 815, 7, &root));
        // Periudha më e gjatë se 15 epoka, ose bosh, refuzohet edhe kur është e firmosur.
        assert!(!with(auth(&id, &G, &s_pk, 800, 816)).verify(&G, 805, 7, &root));
        assert!(!with(auth(&id, &G, &s_pk, 800, 800)).verify(&G, 800, 7, &root));
        assert!(!with(auth(&id, &G, &s_pk, u64::MAX, 3)).verify(&G, 0, 7, &root));
        // Identiteti që firmos vetë s'ka nevojë për autorizim dhe s'skadon.
        let own = Attestation { validator: id_pk, signer: None, authorization: None, signature: id.sign(&attest_message(&G, 7, &root)).to_bytes() };
        assert!(own.verify(&G, 99_999, 7, &root));
    }

    /// Opsioni A: një autorizim v1 (pa afat) s'pranohet, edhe i firmosur saktë.
    #[test]
    fn v1_authorization_is_rejected() {
        let id = SigningKey::generate(&mut OsRng);
        let signer = SigningKey::generate(&mut OsRng);
        let (s_pk, id_pk) = (signer.verifying_key().to_bytes(), id.verifying_key().to_bytes());
        let mut v1 = b"zilja/authorize/v1".to_vec();
        v1.extend_from_slice(&G);
        v1.extend_from_slice(&s_pk);
        let sig_v1 = id.sign(&v1).to_bytes();
        let root = [9u8; 32];
        let sig = signer.sign(&attest_message(&G, 7, &root)).to_bytes();
        for (from, until) in [(0, 0), (0, 15), (E, E + 1)] {
            let a = Attestation { validator: id_pk, signer: Some(s_pk), authorization: Some(Authorization { from_epoch: from, until_epoch: until, signature: sig_v1 }), signature: sig };
            assert!(!a.verify(&G, E, 7, &root));
        }
    }

    /// A15: një firmë e pavlefshme e një identiteti, para një të vlefshme të të njëjtit identitet, s'e
    /// fshin të vlefshmen; rezultati është i njëjtë në çdo radhë.
    #[test]
    fn invalid_signature_does_not_shadow_valid_one() {
        let (keys, table) = setup(3);
        let root = [7u8; 32];
        let good = |k: &SigningKey| Attestation {
            validator: k.verifying_key().to_bytes(),
            signer: None,
            authorization: None,
            signature: k.sign(&attest_message(&G, 5, &root)).to_bytes(),
        };
        let bad = |k: &SigningKey| Attestation { signature: [1u8; 64], ..good(k) };
        let mut atts = vec![bad(&keys[0]), good(&keys[0]), good(&keys[1]), good(&keys[2])];
        let a = verify_quorum(&G, E, 5, &root, &atts, &table);
        atts.reverse();
        let b = verify_quorum(&G, E, 5, &root, &atts, &table);
        assert!(a.reached && b.reached, "{a:?} / {b:?}");
        assert_eq!((a.attested_stake, a.valid_signatures), (b.attested_stake, b.valid_signatures));
    }
}
