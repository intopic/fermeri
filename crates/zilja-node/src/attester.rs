//! Firmosja e root-eve (F-10, I-02).
//!
//! - Mesazhi v2 përfshin hash-in e genesis-it.
//! - Firmos një çelës i veçantë, i autorizuar një herë nga identiteti, ose vetë identiteti.
//! - Firmoset vetëm kur pema është e plotë dhe zinxhiri është konfirmuar (thirrësi e garanton).
//! - Asnjëherë dy root-e të ndryshme për të njëjtin slot, as pas rinisjes: çdo (slot, root) i
//!   firmosur shkruhet në një skedar (me `sync`) para se firma të dalë. Po ky skedar i rikthen
//!   firmat e fundit pas rinisjes (I-02).

use ed25519_dalek::{Signer, SigningKey};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use zilja_verifier::attest::{attest_message, authorize_message};
use zilja_verifier::Hash;

/// Sa (slot, root) të fundit mbahen në skedar.
const KEEP: usize = 4096;

pub struct AttestOptions {
    /// Çelësi që firmos.
    pub key: SigningKey,
    /// Identiteti i validatorit, kur firmos një çelës i veçantë.
    pub identity: Option<[u8; 32]>,
    /// Firma e identitetit mbi `authorize_message(genesis, key)`.
    pub authorization: Option<[u8; 64]>,
    pub genesis: Hash,
    /// Skedari i firmave (kundër firmosjes së dyfishtë). `None` vetëm në teste.
    pub log: Option<PathBuf>,
}

/// Çfarë i tregon RPC-ja klientit për firmuesin.
#[derive(Clone, Debug)]
pub struct AttesterInfo {
    pub identity: [u8; 32],
    pub signer: [u8; 32],
    pub authorization: Option<[u8; 64]>,
    pub genesis: Hash,
}

pub struct Attester {
    opts: AttestOptions,
    signed: BTreeMap<u64, Hash>,
    file: Option<File>,
    lines: usize,
    pub refused: u64,
}

impl Attester {
    pub fn open(opts: AttestOptions) -> Result<Self, String> {
        let signer = opts.key.verifying_key().to_bytes();
        if let (Some(id), Some(auth)) = (opts.identity, opts.authorization) {
            let ok = ed25519_dalek::VerifyingKey::from_bytes(&id)
                .map(|vk| vk.verify_strict(&authorize_message(&opts.genesis, &signer), &ed25519_dalek::Signature::from_bytes(&auth)).is_ok())
                .unwrap_or(false);
            if !ok {
                return Err("autorizimi i çelësit të firmosjes nuk vlen për këtë identitet dhe genesis".into());
            }
        } else if opts.identity.is_some_and(|id| id != signer) {
            return Err("një çelës i veçantë firmosjeje kërkon autorizim nga identiteti".into());
        }
        let mut signed = BTreeMap::new();
        let mut lines = 0;
        if let Some(p) = &opts.log {
            if let Ok(f) = File::open(p) {
                for line in BufReader::new(f).lines() {
                    let line = line.map_err(|e| e.to_string())?;
                    let mut it = line.split_whitespace();
                    let (Some(s), Some(r)) = (it.next(), it.next()) else { continue };
                    let slot: u64 = s.parse().map_err(|_| format!("rresht i dëmtuar në {}: {line}", p.display()))?;
                    let root: Hash = hex::decode(r).ok().and_then(|b| b.try_into().ok()).ok_or_else(|| format!("rresht i dëmtuar: {line}"))?;
                    signed.insert(slot, root);
                    lines += 1;
                }
            }
        }
        let file = match &opts.log {
            Some(p) => Some(OpenOptions::new().create(true).append(true).open(p).map_err(|e| e.to_string())?),
            None => None,
        };
        Ok(Attester { opts, signed, file, lines, refused: 0 })
    }

    pub fn info(&self) -> AttesterInfo {
        let signer = self.opts.key.verifying_key().to_bytes();
        AttesterInfo {
            identity: self.opts.identity.unwrap_or(signer),
            signer,
            authorization: self.opts.authorization,
            genesis: self.opts.genesis,
        }
    }

    fn sig(&self, slot: u64, root: &Hash) -> [u8; 64] {
        self.opts.key.sign(&attest_message(&self.opts.genesis, slot, root)).to_bytes()
    }

    /// Firmat e fundit nga skedari, për t'i shërbyer menjëherë pas rinisjes.
    pub fn recent(&self, n: usize) -> Vec<(u64, Hash, [u8; 64])> {
        self.signed.iter().rev().take(n).map(|(s, r)| (*s, *r, self.sig(*s, r))).collect()
    }

    /// Firmos (slot, root), ose refuzon kur kjo do të ishte firmosje e dyfishtë.
    pub fn sign(&mut self, slot: u64, root: &Hash) -> Result<[u8; 64], String> {
        if let Some(prev) = self.signed.get(&slot) {
            if prev != root {
                self.refused += 1;
                return Err(format!("sloti {slot} është firmosur tashmë me një root tjetër: s'firmos sërish"));
            }
            return Ok(self.sig(slot, root));
        }
        // Më i vjetër se çfarë mbajmë: s'mund ta dimë që s'është firmosur. Refuzo.
        if self.signed.len() >= KEEP && self.signed.keys().next().is_some_and(|first| slot < *first) {
            self.refused += 1;
            return Err(format!("sloti {slot} është më i vjetër se historia e firmave"));
        }
        if let Some(f) = &mut self.file {
            writeln!(f, "{slot} {}", hex::encode(root)).and_then(|_| f.sync_data()).map_err(|e| format!("skedari i firmave: {e}"))?;
            self.lines += 1;
        }
        self.signed.insert(slot, *root);
        while self.signed.len() > KEEP {
            self.signed.pop_first();
        }
        if self.lines > KEEP * 4 {
            self.compact()?;
        }
        Ok(self.sig(slot, root))
    }

    /// Rishkruan skedarin vetëm me hyrjet që mbahen (shkrim i ri + rename atomik).
    fn compact(&mut self) -> Result<(), String> {
        let Some(p) = &self.opts.log else { return Ok(()) };
        let tmp = p.with_extension("tmp");
        {
            let mut f = File::create(&tmp).map_err(|e| e.to_string())?;
            for (s, r) in &self.signed {
                writeln!(f, "{s} {}", hex::encode(r)).map_err(|e| e.to_string())?;
            }
            f.sync_all().map_err(|e| e.to_string())?;
        }
        std::fs::rename(&tmp, p).map_err(|e| e.to_string())?;
        self.file = Some(OpenOptions::new().append(true).open(p).map_err(|e| e.to_string())?);
        self.lines = self.signed.len();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(log: Option<PathBuf>) -> AttestOptions {
        AttestOptions { key: SigningKey::from_bytes(&[3; 32]), identity: None, authorization: None, genesis: [1; 32], log }
    }

    #[test]
    fn never_signs_two_roots_for_one_slot_even_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("attest.log");
        {
            let mut a = Attester::open(opts(Some(p.clone()))).unwrap();
            assert!(a.sign(10, &[5; 32]).is_ok());
            assert!(a.sign(10, &[5; 32]).is_ok(), "i njëjti root: lejohet");
            assert!(a.sign(10, &[6; 32]).is_err());
        }
        let mut a = Attester::open(opts(Some(p))).unwrap();
        assert!(a.sign(10, &[6; 32]).is_err(), "pas rinisjes kujtohet");
        assert_eq!(a.recent(5).len(), 1);
        assert_eq!(a.refused, 1);
    }

    #[test]
    fn dedicated_key_needs_valid_authorization() {
        let id = SigningKey::from_bytes(&[9; 32]);
        let key = SigningKey::from_bytes(&[3; 32]);
        let g = [1u8; 32];
        let auth = id.sign(&authorize_message(&g, &key.verifying_key().to_bytes())).to_bytes();
        let ok = AttestOptions { key: key.clone(), identity: Some(id.verifying_key().to_bytes()), authorization: Some(auth), genesis: g, log: None };
        assert!(Attester::open(ok).is_ok());
        let bad = AttestOptions { key: key.clone(), identity: Some(id.verifying_key().to_bytes()), authorization: Some(auth), genesis: [2; 32], log: None };
        assert!(Attester::open(bad).is_err());
        let none = AttestOptions { key, identity: Some(id.verifying_key().to_bytes()), authorization: None, genesis: g, log: None };
        assert!(Attester::open(none).is_err());
    }
}
