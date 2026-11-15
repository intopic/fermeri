//! Faza 4 (version jashtë protokollit): "pleqtë firmosin ziles".
//!
//! Çdo validator firmos mesazhin `"zilja/attest/v1" ‖ slot LE ‖ root` me çelësin e vet
//! (Ed25519). Light client-i pranon root-in vetëm nëse validatorët që e firmosën
//! kanë **më shumë se 2/3** e stake-ut total.
//!
//! Në versionin e protokollit (pas SIMD), root-i do të futet në bank hash dhe
//! votat ekzistuese (ose Alpenglow/BLS) do ta firmosin drejtpërdrejt. Logjika e
//! kuorumit mbetet e njëjtë.

use crate::Hash;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use std::collections::HashSet;

pub const ATTEST_DOMAIN: &[u8] = b"zilja/attest/v1";

/// Mesazhi që firmos një validator.
pub fn attest_message(slot: u64, root: &Hash) -> Vec<u8> {
    let mut m = Vec::with_capacity(ATTEST_DOMAIN.len() + 8 + 32);
    m.extend_from_slice(ATTEST_DOMAIN);
    m.extend_from_slice(&slot.to_le_bytes());
    m.extend_from_slice(root);
    m
}

#[derive(Clone, Debug)]
pub struct Attestation {
    pub validator: [u8; 32],
    pub signature: [u8; 64],
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

/// Kontrollon firmat dhe kuorumin > 2/3 të stake-ut.
pub fn verify_quorum(slot: u64, root: &Hash, atts: &[Attestation], table: &StakeTable) -> QuorumReport {
    let msg = attest_message(slot, root);
    let mut seen = HashSet::new();
    let mut attested: u128 = 0;
    let mut valid = 0;
    let mut rejected = 0;
    for a in atts {
        // Një validator numërohet vetëm një herë.
        if !seen.insert(a.validator) {
            rejected += 1;
            continue;
        }
        let Some(stake) = table.stake_of(&a.validator) else {
            rejected += 1;
            continue;
        };
        let ok = VerifyingKey::from_bytes(&a.validator)
            .map(|vk| vk.verify(&msg, &Signature::from_bytes(&a.signature)).is_ok())
            .unwrap_or(false);
        if ok {
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

    fn setup(n: usize) -> (Vec<SigningKey>, StakeTable) {
        let keys: Vec<SigningKey> = (0..n).map(|_| SigningKey::generate(&mut OsRng)).collect();
        let table = StakeTable {
            entries: keys.iter().map(|k| (k.verifying_key().to_bytes(), 100)).collect(),
        };
        (keys, table)
    }

    fn sign(k: &SigningKey, slot: u64, root: &Hash) -> Attestation {
        Attestation {
            validator: k.verifying_key().to_bytes(),
            signature: k.sign(&attest_message(slot, root)).to_bytes(),
        }
    }

    #[test]
    fn quorum_needs_more_than_two_thirds() {
        let (keys, table) = setup(10);
        let root = [5u8; 32];
        let atts: Vec<_> = keys[..7].iter().map(|k| sign(k, 42, &root)).collect();
        assert!(verify_quorum(42, &root, &atts, &table).reached);
        // 6/10 = 60% nuk mjafton.
        assert!(!verify_quorum(42, &root, &atts[..6], &table).reached);
    }

    #[test]
    fn rejects_wrong_root_slot_duplicates_and_strangers() {
        let (keys, table) = setup(10);
        let root = [5u8; 32];
        let atts: Vec<_> = keys[..8].iter().map(|k| sign(k, 42, &root)).collect();
        // Root i ndryshuar ose slot tjetër: asnjë firmë s'vlen.
        assert_eq!(verify_quorum(42, &[6u8; 32], &atts, &table).valid_signatures, 0);
        assert_eq!(verify_quorum(43, &root, &atts, &table).valid_signatures, 0);
        // I njëjti validator 7 herë nuk e kalon kuorumin.
        let dup: Vec<_> = (0..7).map(|_| sign(&keys[0], 42, &root)).collect();
        let r = verify_quorum(42, &root, &dup, &table);
        assert!(!r.reached);
        assert_eq!(r.rejected, 6);
        // Validator jashtë tabelës nuk numërohet.
        let (strangers, _) = setup(8);
        let s: Vec<_> = strangers.iter().map(|k| sign(k, 42, &root)).collect();
        assert!(!verify_quorum(42, &root, &s, &table).reached);
    }
}
