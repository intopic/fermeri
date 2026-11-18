//! Firmosja e root-eve (F-10, I-02).
//!
//! - Mesazhi v2 përfshin hash-in e genesis-it.
//! - Firmos një çelës i veçantë, i autorizuar një herë nga identiteti, ose vetë identiteti.
//! - Firmoset vetëm kur pema është e plotë dhe zinxhiri është konfirmuar (thirrësi e garanton).
//! - Asnjëherë dy root-e të ndryshme për të njëjtin slot, as pas rinisjes: çdo (slot, root) i
//!   firmosur shkruhet në një skedar (me `sync`) para se firma të dalë. Po ky skedar i rikthen
//!   firmat e fundit pas rinisjes (I-02).
//! - P-04: skedari mbrohet me një lock ekskluziv (`<log>.lock`) gjatë gjithë jetës së instancës, ndaj
//!   dy procese në të njëjtin host s'firmosin përmes të njëjtit log. Një log që përmban dy root-e për
//!   të njëjtin slot refuzohet në hapje: është provë firmosjeje të dyfishtë dhe s'zgjidhet në heshtje.
//!
//! Kufizim: lock-u është lokal. Dy host-e me të njëjtin çelës dhe log-e të ndryshme NUK mbrohen;
//! një çelës firmosjeje duhet të jetë në një host të vetëm (shih docs/OPERIMI.md).

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
    _lock: Option<File>,
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
        // P-04: lock-u para leximit ose shkrimit të log-ut.
        let lock = match &opts.log {
            Some(p) => Some(lock_exclusive(&lock_path(p))?),
            None => None,
        };
        let mut signed: BTreeMap<u64, Hash> = BTreeMap::new();
        let mut lines = 0;
        if let Some(p) = &opts.log {
            if let Ok(f) = File::open(p) {
                for line in BufReader::new(f).lines() {
                    let line = line.map_err(|e| e.to_string())?;
                    let mut it = line.split_whitespace();
                    let (Some(s), Some(r)) = (it.next(), it.next()) else { continue };
                    let slot: u64 = s.parse().map_err(|_| format!("rresht i dëmtuar në {}: {line}", p.display()))?;
                    let root: Hash = hex::decode(r).ok().and_then(|b| b.try_into().ok()).ok_or_else(|| format!("rresht i dëmtuar: {line}"))?;
                    if let Some(prev) = signed.get(&slot) {
                        if *prev != root {
                            return Err(format!(
                                "{} përmban dy root-e të ndryshme për slotin {slot} ({} dhe {}): firmosje e dyfishtë; hapja refuzohet, skedari s'u prek",
                                p.display(),
                                hex::encode(prev),
                                hex::encode(root)
                            ));
                        }
                    }
                    signed.insert(slot, root);
                    lines += 1;
                }
            }
        }
        let file = match &opts.log {
            Some(p) => Some(OpenOptions::new().create(true).append(true).open(p).map_err(|e| e.to_string())?),
            None => None,
        };
        Ok(Attester { opts, _lock: lock, signed, file, lines, refused: 0 })
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

fn lock_path(log: &std::path::Path) -> PathBuf {
    let mut s = log.as_os_str().to_owned();
    s.push(".lock");
    PathBuf::from(s)
}

/// Lock ekskluziv (flock) pa pritur, i mbajtur derisa mbyllet skedari.
fn lock_exclusive(path: &std::path::Path) -> Result<File, String> {
    let f = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).map_err(|e| format!("hapja e {}: {e}", path.display()))?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(std::fs::TryLockError::WouldBlock) => Err(format!("log-u i firmave është në përdorim nga një instancë tjetër ({} është i zënë)", path.display())),
        Err(std::fs::TryLockError::Error(e)) => Err(format!("lock-u {}: {e}", path.display())),
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

    /// P-04: dy instanca mbi të njëjtin log nuk mund të firmosin dy root-e për një slot.
    #[test]
    fn second_attester_on_same_log_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("attest.log");
        let mut a = Attester::open(opts(Some(p.clone()))).unwrap();
        let b = Attester::open(opts(Some(p.clone())));
        let Ok(mut b) = b else {
            assert!(a.sign(10, &[5; 32]).is_ok());
            return;
        };
        let ra = a.sign(10, &[5; 32]);
        let rb = b.sign(10, &[6; 32]);
        panic!("dy instanca u hapën mbi të njëjtin log; firmosën: {} dhe {}", ra.is_ok(), rb.is_ok());
    }

    /// P-04: një log me dy root-e për të njëjtin slot është provë firmosjeje të dyfishtë: hapja
    /// refuzohet dhe asnjë nga të dy nuk "fiton" në heshtje.
    #[test]
    fn conflicting_roots_in_log_refuse_open() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("attest.log");
        std::fs::write(&p, format!("10 {}\n11 {}\n10 {}\n", hex::encode([5u8; 32]), hex::encode([7u8; 32]), hex::encode([6u8; 32]))).unwrap();
        let before = std::fs::read(&p).unwrap();
        match Attester::open(opts(Some(p.clone()))) {
            Ok(mut a) => {
                let r5 = a.sign(10, &[5; 32]).is_ok();
                let r6 = a.sign(10, &[6; 32]).is_ok();
                panic!("log me konflikt u pranua (firmos [5]: {r5}, firmos [6]: {r6})");
            }
            Err(e) => assert!(e.contains("10"), "mesazhi duhet të tregojë slotin: {e}"),
        }
        assert_eq!(std::fs::read(&p).unwrap(), before, "log-u me konflikt u ndryshua");
        // E njëjta hyrje e përsëritur nuk është konflikt.
        std::fs::write(&p, format!("10 {}\n10 {}\n", hex::encode([5u8; 32]), hex::encode([5u8; 32]))).unwrap();
        assert!(Attester::open(opts(Some(p))).is_ok());
    }
}

