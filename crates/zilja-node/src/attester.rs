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
//! - P-05: rreshti i parë i log-ut është konteksti: `# zilja-attest-log v1 genesis=… identity=… signer=…`.
//!   Hapja me genesis, identitet ose çelës tjetër refuzohet, ndaj root-et e vjetra s'rifirmosen kurrë
//!   në një kontekst tjetër. Një log pa kontekst (version i vjetër) pranohet vetëm me
//!   `adopt_legacy` (vendim eksplicit i operatorit), që i shton kokën pa humbur historinë.
//!
//! Kufizim: lock-u është lokal. Dy host-e me të njëjtin çelës dhe log-e të ndryshme NUK mbrohen;
//! një çelës firmosjeje duhet të jetë në një host të vetëm (shih docs/OPERIMI.md).

use ed25519_dalek::{Signer, SigningKey};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use zilja_verifier::attest::{attest_message, authorize_message};
use zilja_verifier::Hash;

/// Sa (slot, root) të fundit mbahen në skedar.
#[cfg(not(test))]
const KEEP: usize = 4096;
#[cfg(test)]
const KEEP: usize = 64;
const HEADER: &str = "# zilja-attest-log v1";

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
    /// P-05: operatori pohon që një log pa kontekst (version i vjetër) është i këtij genesis-i,
    /// identiteti dhe çelësi. Përdoret një herë; pas saj log-u ka kokë.
    pub adopt_legacy: bool,
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
        let header = header_line(&opts);
        let mut signed: BTreeMap<u64, Hash> = BTreeMap::new();
        let mut lines = 0;
        let mut need_header = true;
        if let Some(p) = &opts.log {
            let text = match std::fs::read_to_string(p) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(e) => return Err(format!("leximi i {}: {e}", p.display())),
            };
            let mut it = text.lines().peekable();
            match it.peek() {
                None => {}
                Some(first) if first.starts_with('#') => {
                    check_header(first, &header).map_err(|e| format!("{}: {e}; hapja refuzohet, skedari s'u prek", p.display()))?;
                    it.next();
                    need_header = false;
                }
                Some(_) if opts.adopt_legacy => log::warn!("zilja: log-u i firmave {} s'ka kontekst; po adoptohet me kontekstin aktual (attest_adopt_legacy)", p.display()),
                Some(_) => {
                    return Err(format!(
                        "{} s'ka kontekst (genesis/identity/signer; version i vjetër). Nëse je i sigurt që u krijua nga ky çelës për këtë rrjet, \
                         vendos attest_adopt_legacy = true një herë; përndryshe përdor një attest_state të ri vetëm me një çelës të ri (docs/OPERIMI.md)",
                        p.display()
                    ))
                }
            }
            for line in it {
                let mut f = line.split_whitespace();
                let (Some(s), Some(r)) = (f.next(), f.next()) else { continue };
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
        let mut file = match &opts.log {
            Some(p) => Some(OpenOptions::new().create(true).append(true).open(p).map_err(|e| e.to_string())?),
            None => None,
        };
        if let (Some(p), true) = (&opts.log, need_header) {
            if signed.is_empty() {
                // Log i ri: koka para çdo firme.
                let f = file.as_mut().unwrap();
                writeln!(f, "{header}").and_then(|_| f.sync_all()).map_err(|e| format!("{}: {e}", p.display()))?;
                sync_parent(p)?;
            } else {
                // Adoptimi: rishkrim atomik me kokën dhe hyrjet ekzistuese.
                write_log(p, &header, &signed)?;
                file = Some(OpenOptions::new().append(true).open(p).map_err(|e| e.to_string())?);
            }
        }
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

    /// Rishkruan skedarin vetëm me hyrjet që mbahen (shkrim i ri + rename atomik), me kokën.
    fn compact(&mut self) -> Result<(), String> {
        let Some(p) = &self.opts.log else { return Ok(()) };
        write_log(p, &header_line(&self.opts), &self.signed)?;
        self.file = Some(OpenOptions::new().append(true).open(p).map_err(|e| e.to_string())?);
        self.lines = self.signed.len();
        Ok(())
    }
}

fn header_line(o: &AttestOptions) -> String {
    let signer = o.key.verifying_key().to_bytes();
    let b = |h: &[u8; 32]| bs58::encode(h).into_string();
    format!("{HEADER} genesis={} identity={} signer={}", b(&o.genesis), b(&o.identity.unwrap_or(signer)), b(&signer))
}

/// Krahason kokën e ruajtur me atë të konfigurimit; gabimi emërton çdo fushë që ndryshon.
fn check_header(stored: &str, want: &str) -> Result<(), String> {
    let kv = |l: &str| -> BTreeMap<String, String> { l.split_whitespace().filter_map(|t| t.split_once('=')).map(|(k, v)| (k.into(), v.into())).collect() };
    if !stored.starts_with(HEADER) {
        return Err(format!("format i panjohur i log-ut të firmave: {stored:?} (pritej {HEADER:?})"));
    }
    let (s, w) = (kv(stored), kv(want));
    let diff: Vec<String> =
        ["genesis", "identity", "signer"].iter().filter(|k| s.get(**k) != w.get(**k)).map(|k| format!("{k} {} ≠ {}", s.get(*k).map_or("?", |v| v), w[*k])).collect();
    if diff.is_empty() {
        Ok(())
    } else {
        Err(format!("log-u u krijua për një kontekst tjetër ({}): root-et e tij s'firmosen në këtë kontekst", diff.join(", ")))
    }
}

fn write_log(p: &std::path::Path, header: &str, signed: &BTreeMap<u64, Hash>) -> Result<(), String> {
    let tmp = p.with_extension("tmp");
    {
        let mut f = File::create(&tmp).map_err(|e| e.to_string())?;
        writeln!(f, "{header}").map_err(|e| e.to_string())?;
        for (s, r) in signed {
            writeln!(f, "{s} {}", hex::encode(r)).map_err(|e| e.to_string())?;
        }
        f.sync_all().map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, p).map_err(|e| e.to_string())?;
    sync_parent(p)
}

fn sync_parent(p: &std::path::Path) -> Result<(), String> {
    let dir = p.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(std::path::Path::new("."));
    File::open(dir).and_then(|d| d.sync_all()).map_err(|e| format!("fsync i {}: {e}", dir.display()))
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
        AttestOptions { key: SigningKey::from_bytes(&[3; 32]), identity: None, authorization: None, genesis: [1; 32], log, adopt_legacy: false }
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
        let ok = AttestOptions { key: key.clone(), identity: Some(id.verifying_key().to_bytes()), authorization: Some(auth), genesis: g, log: None, adopt_legacy: false };
        assert!(Attester::open(ok).is_ok());
        let bad = AttestOptions { key: key.clone(), identity: Some(id.verifying_key().to_bytes()), authorization: Some(auth), genesis: [2; 32], log: None, adopt_legacy: false };
        assert!(Attester::open(bad).is_err());
        let none = AttestOptions { key, identity: Some(id.verifying_key().to_bytes()), authorization: None, genesis: g, log: None, adopt_legacy: false };
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
        let h = header_line(&opts(None));
        std::fs::write(&p, format!("{h}\n10 {}\n11 {}\n10 {}\n", hex::encode([5u8; 32]), hex::encode([7u8; 32]), hex::encode([6u8; 32]))).unwrap();
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
        std::fs::write(&p, format!("{h}\n10 {}\n10 {}\n", hex::encode([5u8; 32]), hex::encode([5u8; 32]))).unwrap();
        assert!(Attester::open(opts(Some(p))).is_ok());
    }

    /// P-05: një log i krijuar për genesis A s'mund të përdoret për të firmosur root-et e tij si
    /// pjesë e genesis B (as me identitet ose çelës tjetër).
    #[test]
    fn log_is_bound_to_genesis_identity_and_signer() {
        use ed25519_dalek::Verifier;
        type Change = fn(&mut AttestOptions);
        let variants: [(&str, Change); 3] = [
            ("genesis", |o| o.genesis = [2; 32]),
            ("signer", |o| o.key = SigningKey::from_bytes(&[4; 32])),
            ("identity", |o| {
                let id = SigningKey::from_bytes(&[9; 32]);
                o.authorization = Some(id.sign(&authorize_message(&o.genesis, &o.key.verifying_key().to_bytes())).to_bytes());
                o.identity = Some(id.verifying_key().to_bytes());
            }),
        ];
        for (what, change) in variants {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("attest.log");
            {
                let mut a = Attester::open(opts(Some(p.clone()))).unwrap();
                a.sign(10, &[5; 32]).unwrap();
                a.sign(11, &[6; 32]).unwrap();
            }
            let before = std::fs::read(&p).unwrap();
            let mut o = opts(Some(p.clone()));
            change(&mut o);
            let (g, vk) = (o.genesis, o.key.verifying_key());
            match Attester::open(o) {
                Ok(a) => {
                    let resigned = a.recent(10).iter().filter(|(s, r, sig)| vk.verify(&attest_message(&g, *s, r), &ed25519_dalek::Signature::from_bytes(sig)).is_ok()).count();
                    panic!("{what} i ndryshuar: log-u u pranua dhe {resigned} root-e të vjetra u rifirmosën në kontekstin e ri");
                }
                Err(e) => assert!(e.contains(what), "mesazhi duhet të tregojë '{what}': {e}"),
            }
            assert_eq!(std::fs::read(&p).unwrap(), before, "{what}: log-u u ndryshua");
        }
    }

    /// P-05: një log pa kontekst (version i vjetër) s'pranohet në heshtje.
    #[test]
    fn legacy_log_without_context_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("attest.log");
        std::fs::write(&p, format!("10 {}\n", hex::encode([5u8; 32]))).unwrap();
        let e = Attester::open(opts(Some(p.clone()))).err().expect("log pa kontekst duhet të refuzohet");
        assert!(e.contains("kontekst"), "{e}");
        // Adoptimi eksplicit: koka shtohet, historia ruhet (sloti 10 s'firmoset me root tjetër).
        let mut o = opts(Some(p.clone()));
        o.adopt_legacy = true;
        let mut a = Attester::open(o).unwrap();
        assert!(a.sign(10, &[6; 32]).is_err());
        drop(a);
        assert!(std::fs::read_to_string(&p).unwrap().starts_with(HEADER));
        let mut o = opts(Some(p));
        o.genesis = [2; 32];
        assert!(Attester::open(o).is_err(), "pas adoptimit log-u është i lidhur me kontekstin");
    }

    /// Kompaktimi ruan kontekstin: pas tij log-u hapet vetëm me të njëjtin kontekst.
    #[test]
    fn compaction_keeps_context() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("attest.log");
        {
            let mut a = Attester::open(opts(Some(p.clone()))).unwrap();
            for s in 0..(KEEP * 4 + 10) as u64 {
                a.sign(s, &[(s % 251) as u8; 32]).unwrap();
            }
        }
        assert!(Attester::open(opts(Some(p.clone()))).is_ok());
        let mut o = opts(Some(p));
        o.genesis = [2; 32];
        assert!(Attester::open(o).is_err());
    }
}

