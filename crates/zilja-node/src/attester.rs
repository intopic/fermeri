//! Firmosja e root-eve (F-10, I-02).
//!
//! - Mesazhi v2 përfshin hash-in e genesis-it.
//! - Firmos një çelës i veçantë, i autorizuar nga identiteti për një periudhë epokash (v2, maksimumi
//!   15), ose vetë identiteti. Autorizimi rinovohet pa rinisje: kur skedari i tij ndryshon, lexohet,
//!   verifikohet dhe zëvendësohet; një skedar i pavlefshëm s'e prek autorizimin e mëparshëm.
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
use std::path::{Path, PathBuf};
use zilja_verifier::attest::{attest_message, Authorization, Revocation, MAX_AUTH_EPOCHS};
use zilja_verifier::Hash;

/// Sa (slot, root) të fundit mbahen në skedar.
#[cfg(not(test))]
const KEEP: usize = 4096;
#[cfg(test)]
const KEEP: usize = 64;
/// A03: ditari v2 ruan edhe firmën origjinale dhe kufirin e historisë (`floor`).
const HEADER: &str = "# zilja-attest-log v2";
const HEADER_V1: &str = "# zilja-attest-log v1";

pub struct AttestOptions {
    /// Çelësi që firmos.
    pub key: SigningKey,
    /// Identiteti i validatorit, kur firmos një çelës i veçantë.
    pub identity: Option<[u8; 32]>,
    /// Autorizimi i `key` nga identiteti (v2, me afat).
    pub authorization: Option<Authorization>,
    /// Skedari nga i cili erdhi autorizimi (`zilja-authorize`); rilexohet kur ndryshon.
    pub authorization_file: Option<PathBuf>,
    /// R5-1: skedari i revokimeve (`zilja-authorize --revoke`), i çdo validatori; rilexohet kur
    /// ndryshon dhe shërbehet te `getZiljaRevocations`.
    pub revocations_file: Option<PathBuf>,
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
    pub authorization: Option<Authorization>,
    pub genesis: Hash,
    /// R5-1: revokimet e verifikuara për këtë genesis (të çdo validatori).
    pub revocations: Vec<Revocation>,
}

/// R5-1: lexon skedarin e revokimeve: një objekt `{validator, signer, signature}` ose një listë
/// e tyre. Kthen vetëm ato të vlefshme për `genesis` dhe numrin e të hedhurave.
pub fn read_revocations_file(path: &Path, genesis: &Hash) -> Result<(Vec<Revocation>, usize), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse_revocations(path, &bytes, genesis)
}

/// G08: revokimet nga bajtet që u lexuan një herë (të njëjtat që ruhen për krahasim).
fn parse_revocations(path: &Path, bytes: &[u8], genesis: &Hash) -> Result<(Vec<Revocation>, usize), String> {
    let at = |e: String| format!("{}: {e}", path.display());
    let text = std::str::from_utf8(bytes).map_err(|e| at(e.to_string()))?;
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| at(e.to_string()))?;
    let list = match v {
        serde_json::Value::Array(a) => a,
        o @ serde_json::Value::Object(_) => vec![o],
        _ => return Err(at("pritej një objekt ose listë revokimesh".into())),
    };
    let b58 = |x: &serde_json::Value| -> Option<[u8; 32]> { bs58::decode(x.as_str()?).into_vec().ok()?.try_into().ok() };
    let mut out = Vec::new();
    let mut bad = 0;
    for r in &list {
        let parsed = (|| Some(Revocation { validator: b58(&r["validator"])?, signer: b58(&r["signer"])?, signature: lower_hex(r["signature"].as_str()?)? }))();
        match parsed {
            Some(rv) if rv.valid(genesis) => {
                if !out.contains(&rv) {
                    out.push(rv);
                }
            }
            _ => bad += 1,
        }
    }
    Ok((out, bad))
}

/// Lexon skedarin që nxjerr `zilja-authorize` (v2):
/// `{"version": 2, "identity", "signer", "genesis", "fromEpoch", "untilEpoch", "authorization"}`.
/// Kthen identitetin dhe autorizimin; firma kontrollohet nga `Attester`. Një skedar v1 (pa afat)
/// refuzohet me udhëzimin për ta rikrijuar.
pub fn read_authorization_file(path: &Path) -> Result<([u8; 32], Authorization), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse_authorization(path, &bytes)
}

/// G08: autorizimi nga bajtet që u lexuan një herë.
fn parse_authorization(path: &Path, bytes: &[u8]) -> Result<([u8; 32], Authorization), String> {
    let at = |e: String| format!("{}: {e}", path.display());
    let text = std::str::from_utf8(bytes).map_err(|e| at(e.to_string()))?;
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| at(e.to_string()))?;
    if v.get("version").and_then(|x| x.as_u64()) != Some(2) {
        return Err(at("autorizim i vjetër (v1, pa afat) ose i panjohur: s'pranohet më. Rikrijoje me `zilja-authorize ... <from_epoch>` (v2, maksimumi 15 epoka)".into()));
    }
    let id: [u8; 32] = bs58::decode(v["identity"].as_str().ok_or_else(|| at("mungon identity".into()))?)
        .into_vec()
        .map_err(|e| at(e.to_string()))?
        .try_into()
        .map_err(|_| at("identity duhet 32 bajt".into()))?;
    let epoch = |k: &str| v[k].as_u64().ok_or_else(|| at(format!("{k} duhet numër i plotë")));
    let signature: [u8; 64] = lower_hex(v["authorization"].as_str().ok_or_else(|| at("mungon authorization".into()))?)
        .ok_or_else(|| at("authorization duhet 64 bajt hex i vogël".into()))?;
    let a = Authorization { from_epoch: epoch("fromEpoch")?, until_epoch: epoch("untilEpoch")?, signature };
    if !a.well_formed() {
        return Err(at(format!("periudha {}..{} s'vlen (duhet 1..={MAX_AUTH_EPOCHS} epoka)", a.from_epoch, a.until_epoch)));
    }
    Ok((id, a))
}

pub struct Attester {
    opts: AttestOptions,
    _lock: Option<File>,
    /// slot → (root, firma origjinale).
    signed: BTreeMap<u64, (Hash, [u8; 64])>,
    /// A03: asnjë slot nën këtë s'firmoset (historia e kompaktuar ose e migruar s'dihet plotësisht).
    floor: u64,
    file: Option<File>,
    lines: usize,
    pub refused: u64,
    /// Përmbajtja e skedarit të autorizimit kur u lexua së fundi.
    auth_seen: Option<Vec<u8>>,
    auth_checked: Option<std::time::Instant>,
    /// R5-1: revokimet e verifikuara dhe gjendja e rilexuar e skedarit të tyre.
    revocations: Vec<Revocation>,
    rev_seen: Option<Vec<u8>>,
    rev_checked: Option<std::time::Instant>,
    /// Ky çelës u revokua gjatë punës: s'firmos më asgjë.
    self_revoked: bool,
}

fn rev_bytes(o: &AttestOptions) -> Option<Vec<u8>> {
    o.revocations_file.as_ref().and_then(|p| std::fs::read(p).ok())
}

fn auth_bytes(o: &AttestOptions) -> Option<Vec<u8>> {
    o.authorization_file.as_ref().and_then(|p| std::fs::read(p).ok())
}

/// Një rresht v2: `<slot> <root 64 hex> <firma 128 hex>`, kanonik (pa hapësira të tepërta, hex i vogël).
fn parse_v2(line: &str) -> Option<(u64, Hash, [u8; 64])> {
    let f: Vec<&str> = line.split(' ').collect();
    let [s, r, g] = f.as_slice() else { return None };
    Some((canon_u64(s)?, lower_hex(r)?, lower_hex(g)?))
}

/// Një rresht v1 / pa kokë: `<slot> <root 64 hex>`, kanonik.
fn parse_v1(line: &str) -> Option<(u64, Hash)> {
    let f: Vec<&str> = line.split(' ').collect();
    let [s, r] = f.as_slice() else { return None };
    Some((canon_u64(s)?, lower_hex(r)?))
}

fn canon_u64(s: &str) -> Option<u64> {
    let v: u64 = s.parse().ok()?;
    (v.to_string() == s).then_some(v)
}

fn lower_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != 2 * N || !s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return None;
    }
    hex::decode(s).ok()?.try_into().ok()
}

impl Attester {
    pub fn open(opts: AttestOptions) -> Result<Self, String> {
        let signer = opts.key.verifying_key().to_bytes();
        if let (Some(id), Some(auth)) = (opts.identity, opts.authorization) {
            if !auth.signed_by(&id, &opts.genesis, &signer) {
                return Err("autorizimi i çelësit të firmosjes nuk vlen për këtë identitet, çelës dhe genesis (ose periudha s'është 1..=15 epoka)".into());
            }
        } else if opts.identity.is_some_and(|id| id != signer) {
            return Err("një çelës i veçantë firmosjeje kërkon autorizim nga identiteti".into());
        }
        // R5-1: revokimet; një çelës i revokuar nga identiteti i vet s'ngarkohet.
        // G08: skedari lexohet një herë; po këto bajte ruhen për krahasimin e rilexuar.
        let mut rev_seen = None;
        let revocations = match &opts.revocations_file {
            None => Vec::new(),
            Some(p) => {
                let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
                let (r, bad) = parse_revocations(p, &bytes, &opts.genesis)?;
                rev_seen = Some(bytes);
                if bad > 0 {
                    log::warn!("zilja: {}: {bad} revokime të pavlefshme u injoruan", p.display());
                }
                r
            }
        };
        let identity = opts.identity.unwrap_or(signer);
        if revocations.iter().any(|r| r.validator == identity && r.signer == signer) {
            return Err("ky çelës firmosjeje është revokuar nga identiteti (attest_revocations): përdor një çelës të ri".into());
        }
        // P-04: lock-u para leximit ose shkrimit të log-ut.
        let lock = match &opts.log {
            Some(p) => Some(lock_exclusive(&lock_path(p))?),
            None => None,
        };
        // G08: autorizimi erdhi nga thirrësi (i lexuar më herët). `auth_seen = None` e detyron rilexuarjen e
        // parë ta krahasojë me skedarin e tanishëm, ndaj një ndryshim mes dy leximeve s'humbet.
        let auth_seen = None;
        let Some(p) = opts.log.clone() else {
            return Ok(Attester { opts, _lock: lock, signed: BTreeMap::new(), floor: 0, file: None, lines: 0, refused: 0, auth_seen, auth_checked: None, revocations, rev_seen, rev_checked: None, self_revoked: false });
        };
        let ctx = context_line(&opts);
        let text = match std::fs::read(&p) {
            Ok(t) => String::from_utf8(t).map_err(|_| format!("{}: s'është tekst UTF-8; hapja refuzohet, skedari s'u prek", p.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("leximi i {}: {e}", p.display())),
        };
        let bad = |what: String| format!("{}: {what}; hapja refuzohet, skedari s'u prek", p.display());
        let first = text.lines().next();
        let (signed, floor, lines) = match first {
            None => {
                write_log(&p, &ctx, 0, &BTreeMap::new())?;
                (BTreeMap::new(), 0, 0)
            }
            Some(h) if h.starts_with(HEADER) => {
                check_header(h, &ctx).map_err(bad)?;
                let floor = header_floor(h).ok_or_else(|| bad("koka s'ka floor të vlefshëm".into()))?;
                let vk = opts.key.verifying_key();
                let body = &text[h.len() + 1.min(text.len() - h.len())..];
                // Rreshti i fundit pa '\n' = shkrim i papërfunduar (firma s'u kthye kurrë, sepse kthehet
                // vetëm pas fsync-ut të rreshtit të plotë). Ruhet për diagnozë dhe hiqet.
                let (complete, torn) = match body.rfind('\n') {
                    Some(i) => (&body[..=i], &body[i + 1..]),
                    None => ("", body),
                };
                let mut signed = BTreeMap::new();
                let mut last: Option<u64> = None;
                for (n, line) in complete.lines().enumerate() {
                    let (slot, root, sig) = parse_v2(line).ok_or_else(|| bad(format!("rreshti {} i dëmtuar ose jo kanonik: {line:?}", n + 2)))?;
                    if slot < floor {
                        return Err(bad(format!("sloti {slot} nën kufirin e historisë {floor}")));
                    }
                    if last.is_some_and(|l| slot <= l) {
                        return Err(bad(format!("sloti {slot} pas slotit {} (rend i gabuar ose dy root-e për një slot)", last.unwrap())));
                    }
                    // A03: çdo hyrje vërtetohet me firmën e saj origjinale; asgjë s'rifirmoset.
                    if vk.verify_strict(&attest_message(&opts.genesis, slot, &root), &ed25519_dalek::Signature::from_bytes(&sig)).is_err() {
                        return Err(bad(format!("firma e slotit {slot} s'vërtetohet: rekordi u ndryshua")));
                    }
                    last = Some(slot);
                    signed.insert(slot, (root, sig));
                }
                if !torn.is_empty() {
                    let off = text.len() - torn.len();
                    let keep = PathBuf::from(format!("{}.tail-{off}", p.display()));
                    std::fs::write(&keep, torn).map_err(|e| format!("{}: {e}", keep.display()))?;
                    let f = OpenOptions::new().write(true).open(&p).map_err(|e| e.to_string())?;
                    f.set_len(off as u64).and_then(|_| f.sync_all()).map_err(|e| format!("{}: {e}", p.display()))?;
                    log::warn!("zilja: u hoq rreshti i papërfunduar i log-ut të firmave ({} B); u ruajt në {}", torn.len(), keep.display());
                }
                let n = signed.len();
                (signed, floor, n)
            }
            Some(h) if h.starts_with(HEADER_V1) || !h.starts_with('#') => {
                if h.starts_with(HEADER_V1) {
                    check_header(&h.replacen(HEADER_V1, HEADER, 1), &ctx).map_err(bad)?;
                } else if !opts.adopt_legacy {
                    return Err(format!(
                        "{} s'ka kontekst (genesis/identity/signer; version i vjetër). Nëse je i sigurt që u krijua nga ky çelës për këtë rrjet, \
                         vendos attest_adopt_legacy = true një herë; përndryshe përdor një attest_state të ri vetëm me një çelës të ri (docs/OPERIMI.md)",
                        p.display()
                    ));
                }
                // A03: v1 s'ka firma: historia s'mund të vërtetohet, ndaj s'shërbehet dhe s'rifirmoset.
                // Ruhet vetëm si mbrojtje: asnjë slot deri te më i madhi i saj s'firmoset më.
                let skip = usize::from(h.starts_with('#'));
                let mut max: Option<u64> = None;
                let mut seen: BTreeMap<u64, Hash> = BTreeMap::new();
                for (n, line) in text.lines().skip(skip).enumerate() {
                    let (slot, root) = parse_v1(line).ok_or_else(|| bad(format!("rreshti {} i dëmtuar ose jo kanonik: {line:?}", n + 1 + skip)))?;
                    if seen.get(&slot).is_some_and(|r| *r != root) {
                        return Err(bad(format!("dy root-e të ndryshme për slotin {slot}: firmosje e dyfishtë")));
                    }
                    seen.insert(slot, root);
                    max = Some(max.map_or(slot, |m| m.max(slot)));
                }
                let floor = max.map_or(0, |m| m.saturating_add(1));
                let old = PathBuf::from(format!("{}.v1-migrated", p.display()));
                if old.exists() {
                    return Err(bad(format!("{} ekziston tashmë", old.display())));
                }
                std::fs::copy(&p, &old).map_err(|e| format!("{}: {e}", old.display()))?;
                File::open(&old).and_then(|f| f.sync_all()).map_err(|e| e.to_string())?;
                write_log(&p, &ctx, floor, &BTreeMap::new())?;
                log::warn!(
                    "zilja: log-u i firmave {} u migrua në v2: {} hyrje pa firmë s'shërbehen; asnjë slot nën {floor} s'firmoset (origjinali: {})",
                    p.display(),
                    seen.len(),
                    old.display()
                );
                (BTreeMap::new(), floor, 0)
            }
            Some(h) => return Err(bad(format!("format i panjohur i log-ut të firmave: {h:?} (pritej {HEADER:?})"))),
        };
        let file = Some(OpenOptions::new().append(true).open(&p).map_err(|e| e.to_string())?);
        Ok(Attester { opts, _lock: lock, signed, floor, file, lines, refused: 0, auth_seen, auth_checked: None, revocations, rev_seen, rev_checked: None, self_revoked: false })
    }

    /// Rinovimi pa rinisje: kur përmbajtja e skedarit të autorizimit ndryshon (kontrollohet më së
    /// shumti një herë në sekondë), lexohet dhe verifikohet; vetëm një autorizim i vlefshëm për të njëjtin identitet,
    /// çelës dhe genesis e zëvendëson të vjetrin. Kthen `true` kur autorizimi ndryshoi.
    /// R5-1: rilexon revokimet kur skedari ndryshon (më së shumti një herë në sekondë). Nëse ky
    /// çelës revokohet, ndalon firmosjen menjëherë. Kthen `true` kur lista ndryshoi.
    pub fn refresh_revocations(&mut self) -> bool {
        let Some(path) = self.opts.revocations_file.clone() else { return false };
        let now = std::time::Instant::now();
        if self.rev_checked.is_some_and(|t| now.duration_since(t) < std::time::Duration::from_secs(1)) {
            return false;
        }
        self.rev_checked = Some(now);
        let m = rev_bytes(&self.opts);
        if m == self.rev_seen {
            return false;
        }
        let Some(bytes) = m.clone() else {
            self.rev_seen = None;
            log::error!("zilja: {} s'u lexua; mbeten revokimet e mëparshme", path.display());
            return false;
        };
        self.rev_seen = m;
        // G08: nga po këto bajte, jo nga një lexim i dytë i skedarit.
        match parse_revocations(&path, &bytes, &self.opts.genesis) {
            Ok((r, bad)) => {
                if bad > 0 {
                    log::warn!("zilja: {}: {bad} revokime të pavlefshme u injoruan", path.display());
                }
                // Revokimet s'hiqen kurrë gjatë punës: një skedar më i shkurtër s'e "çrevokon" një çelës.
                let mut changed = false;
                for x in r {
                    if !self.revocations.contains(&x) {
                        self.revocations.push(x);
                        changed = true;
                    }
                }
                let signer = self.opts.key.verifying_key().to_bytes();
                let identity = self.opts.identity.unwrap_or(signer);
                if !self.self_revoked && self.revocations.iter().any(|r| r.validator == identity && r.signer == signer) {
                    self.self_revoked = true;
                    log::error!("zilja: ky çelës firmosjeje u revokua nga identiteti: firmosja ndaloi (vendos një çelës të ri)");
                }
                changed
            }
            Err(e) => {
                log::error!("zilja: revokimet s'u lexuan ({e}); mbeten të mëparshmet");
                false
            }
        }
    }

    pub fn refresh_authorization(&mut self) -> bool {
        let Some(path) = self.opts.authorization_file.clone() else { return false };
        let now = std::time::Instant::now();
        if self.auth_checked.is_some_and(|t| now.duration_since(t) < std::time::Duration::from_secs(1)) {
            return false;
        }
        self.auth_checked = Some(now);
        // Krahasohet përmbajtja, jo mtime-i: dy shkrime brenda të njëjtës "tik" të orës së sistemit të
        // skedarëve kanë të njëjtin mtime (u pa në e2e me `cp` + `mv`). Skedari është ~400 B.
        let m = auth_bytes(&self.opts);
        if m == self.auth_seen {
            return false;
        }
        let Some(bytes) = m.clone() else {
            self.auth_seen = None;
            log::error!("zilja: {} s'u lexua; mbetet autorizimi i vjetër", path.display());
            return false;
        };
        self.auth_seen = m;
        let signer = self.opts.key.verifying_key().to_bytes();
        // G08: nga po këto bajte, jo nga një lexim i dytë i skedarit.
        match parse_authorization(&path, &bytes) {
            Ok((id, a)) if Some(id) == self.opts.identity && a.signed_by(&id, &self.opts.genesis, &signer) => {
                if self.opts.authorization == Some(a) {
                    return false;
                }
                log::info!("zilja: autorizimi i çelësit u rinovua: epokat {}..{}", a.from_epoch, a.until_epoch);
                self.opts.authorization = Some(a);
                true
            }
            Ok(_) => {
                log::error!("zilja: {}: autorizimi i ri s'vlen për këtë identitet, çelës dhe genesis; mbetet i vjetri", path.display());
                false
            }
            Err(e) => {
                log::error!("zilja: autorizimi i ri s'u lexua ({e}); mbetet i vjetri");
                false
            }
        }
    }

    /// Autorizimi aktual i çelësit (nëse ka).
    pub fn authorization(&self) -> Option<Authorization> {
        self.opts.authorization
    }

    pub fn info(&self) -> AttesterInfo {
        let signer = self.opts.key.verifying_key().to_bytes();
        AttesterInfo {
            identity: self.opts.identity.unwrap_or(signer),
            signer,
            authorization: self.opts.authorization,
            genesis: self.opts.genesis,
            revocations: self.revocations.clone(),
        }
    }

    /// A03: firmat e fundit, ato origjinale nga skedari (të vërtetuara në hapje), për t'i shërbyer
    /// menjëherë pas rinisjes. Asgjë s'rifirmoset.
    pub fn recent(&self, n: usize) -> Vec<(u64, Hash, [u8; 64])> {
        self.signed.iter().rev().take(n).map(|(s, (r, g))| (*s, *r, *g)).collect()
    }

    /// Firmos (slot, root), ose refuzon kur kjo do të ishte firmosje e dyfishtë ose kur sloti s'mund
    /// të dihet që s'është firmosur (nën kufirin, ose para firmës së fundit).
    pub fn sign(&mut self, slot: u64, root: &Hash) -> Result<[u8; 64], String> {
        if self.self_revoked {
            self.refused += 1;
            return Err("çelësi i firmosjes është revokuar: s'firmos".into());
        }
        if let Some((prev, sig)) = self.signed.get(&slot) {
            if prev != root {
                self.refused += 1;
                return Err(format!("sloti {slot} është firmosur tashmë me një root tjetër: s'firmos sërish"));
            }
            return Ok(*sig);
        }
        if slot < self.floor {
            self.refused += 1;
            return Err(format!("sloti {slot} është nën kufirin e historisë së firmave ({})", self.floor));
        }
        if self.signed.keys().next_back().is_some_and(|last| slot < *last) {
            self.refused += 1;
            return Err(format!("sloti {slot} është para firmës së fundit: s'firmoset jashtë radhe"));
        }
        let sig = self.opts.key.sign(&attest_message(&self.opts.genesis, slot, root)).to_bytes();
        if let Some(f) = &mut self.file {
            writeln!(f, "{slot} {} {}", hex::encode(root), hex::encode(sig)).and_then(|_| f.sync_data()).map_err(|e| format!("skedari i firmave: {e}"))?;
            self.lines += 1;
        }
        self.signed.insert(slot, (*root, sig));
        while self.signed.len() > KEEP {
            self.signed.pop_first();
            self.floor = *self.signed.keys().next().unwrap();
        }
        if self.lines > KEEP * 4 {
            self.compact()?;
        }
        Ok(sig)
    }

    /// Rishkruan skedarin vetëm me hyrjet që mbahen (shkrim i ri + rename atomik), me kokën dhe kufirin.
    fn compact(&mut self) -> Result<(), String> {
        let Some(p) = &self.opts.log else { return Ok(()) };
        write_log(p, &context_line(&self.opts), self.floor, &self.signed)?;
        self.file = Some(OpenOptions::new().append(true).open(p).map_err(|e| e.to_string())?);
        self.lines = self.signed.len();
        Ok(())
    }
}

/// Koka pa `floor`: konteksti (genesis, identiteti, çelësi).
fn context_line(o: &AttestOptions) -> String {
    let signer = o.key.verifying_key().to_bytes();
    let b = |h: &[u8; 32]| bs58::encode(h).into_string();
    format!("{HEADER} genesis={} identity={} signer={}", b(&o.genesis), b(&o.identity.unwrap_or(signer)), b(&signer))
}

fn header_floor(h: &str) -> Option<u64> {
    let v: Vec<&str> = h.split_whitespace().filter_map(|t| t.strip_prefix("floor=")).collect();
    match v.as_slice() {
        [f] => canon_u64(f),
        _ => None,
    }
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

fn write_log(p: &Path, ctx: &str, floor: u64, signed: &BTreeMap<u64, (Hash, [u8; 64])>) -> Result<(), String> {
    let tmp = p.with_extension("tmp");
    {
        let mut f = File::create(&tmp).map_err(|e| e.to_string())?;
        writeln!(f, "{ctx} floor={floor}").map_err(|e| e.to_string())?;
        for (s, (r, g)) in signed {
            writeln!(f, "{s} {} {}", hex::encode(r), hex::encode(g)).map_err(|e| e.to_string())?;
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
        AttestOptions { key: SigningKey::from_bytes(&[3; 32]), identity: None, authorization: None, authorization_file: None, revocations_file: None, genesis: [1; 32], log, adopt_legacy: false }
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

    fn auth(id: &SigningKey, g: &Hash, key: &SigningKey, from: u64, until: u64) -> Authorization {
        let m = zilja_verifier::attest::authorize_message(g, &key.verifying_key().to_bytes(), from, until);
        Authorization { from_epoch: from, until_epoch: until, signature: id.sign(&m).to_bytes() }
    }

    fn dedicated(id: &SigningKey, key: &SigningKey, g: Hash, a: Option<Authorization>) -> AttestOptions {
        AttestOptions { key: key.clone(), identity: Some(id.verifying_key().to_bytes()), authorization: a, authorization_file: None, revocations_file: None, genesis: g, log: None, adopt_legacy: false }
    }

    #[test]
    fn dedicated_key_needs_valid_authorization() {
        let id = SigningKey::from_bytes(&[9; 32]);
        let key = SigningKey::from_bytes(&[3; 32]);
        let g = [1u8; 32];
        let a = auth(&id, &g, &key, 800, 815);
        assert!(Attester::open(dedicated(&id, &key, g, Some(a))).is_ok());
        assert!(Attester::open(dedicated(&id, &key, [2; 32], Some(a))).is_err());
        assert!(Attester::open(dedicated(&id, &key, g, None)).is_err());
        // Periudhë më e gjatë se 15 epoka: refuzohet edhe e firmosur.
        assert!(Attester::open(dedicated(&id, &key, g, Some(auth(&id, &g, &key, 800, 816)))).is_err());
    }

    /// Rinovimi: skedari i ri zëvendëson autorizimin pa rinisje; një skedar i prishur, i një
    /// identiteti tjetër ose v1 s'e prek të vjetrin.
    #[test]
    fn authorization_file_is_reloaded_without_restart() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("zilja-authorization.json");
        let id = SigningKey::from_bytes(&[9; 32]);
        let key = SigningKey::from_bytes(&[3; 32]);
        let g = [1u8; 32];
        let write = |id: &SigningKey, from: u64, until: u64, extra: Option<serde_json::Value>| {
            let a = auth(id, &g, &key, from, until);
            let mut v = serde_json::json!({
                "version": 2,
                "identity": bs58::encode(id.verifying_key().to_bytes()).into_string(),
                "signer": bs58::encode(key.verifying_key().to_bytes()).into_string(),
                "genesis": bs58::encode(g).into_string(),
                "fromEpoch": from, "untilEpoch": until,
                "authorization": hex::encode(a.signature),
            });
            if let Some(x) = extra {
                v = x;
            }
            std::fs::write(&f, v.to_string()).unwrap();
            // mtime-i duhet të lëvizë edhe në sisteme skedarësh me rezolucion të ulët.
            let t = std::time::SystemTime::now() + std::time::Duration::from_secs(from);
            File::options().write(true).open(&f).unwrap().set_modified(t).unwrap();
        };
        write(&id, 800, 815, None);
        let (fid, fa) = read_authorization_file(&f).unwrap();
        let mut o = dedicated(&id, &key, g, Some(fa));
        assert_eq!(Some(fid), o.identity);
        o.authorization_file = Some(f.clone());
        let mut at = Attester::open(o).unwrap();
        let now = |at: &Attester| at.info().authorization.map(|a| (a.from_epoch, a.until_epoch));
        assert_eq!(now(&at), Some((800, 815)));
        let refresh = |at: &mut Attester| {
            at.auth_checked = None;
            at.refresh_authorization()
        };
        assert!(!refresh(&mut at), "pa ndryshim");
        write(&id, 813, 828, None);
        assert!(refresh(&mut at));
        assert_eq!(now(&at), Some((813, 828)));
        // Identitet tjetër, periudhë e gjatë, skedar v1, JSON i prishur: mbetet 813..828.
        write(&SigningKey::from_bytes(&[8; 32]), 820, 830, None);
        assert!(!refresh(&mut at));
        write(&id, 820, 840, None);
        assert!(!refresh(&mut at));
        write(&id, 821, 830, Some(serde_json::json!({"identity": "x", "authorization": "00"})));
        assert!(!refresh(&mut at));
        std::fs::write(&f, "{").unwrap();
        File::options().write(true).open(&f).unwrap().set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(9999)).unwrap();
        assert!(!refresh(&mut at));
        assert_eq!(now(&at), Some((813, 828)));
    }

    /// G08: skedari ndryshoi mes leximit nga thirrësi dhe hapjes: rilexuarja e parë e merr të riun,
    /// nuk e ngatërron me atë që u lexua tashmë.
    #[test]
    fn authorization_changed_before_open_is_picked_up() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("auth.json");
        let id = SigningKey::from_bytes(&[9; 32]);
        let key = SigningKey::from_bytes(&[3; 32]);
        let g = [1u8; 32];
        let body = |from: u64, until: u64| {
            serde_json::json!({
                "version": 2,
                "identity": bs58::encode(id.verifying_key().to_bytes()).into_string(),
                "signer": bs58::encode(key.verifying_key().to_bytes()).into_string(),
                "genesis": bs58::encode(g).into_string(),
                "fromEpoch": from, "untilEpoch": until,
                "authorization": hex::encode(auth(&id, &g, &key, from, until).signature),
            })
            .to_string()
        };
        std::fs::write(&f, body(0, 15)).unwrap();
        let (_, a0) = read_authorization_file(&f).unwrap();
        std::fs::write(&f, body(10, 25)).unwrap();
        let mut o = dedicated(&id, &key, g, Some(a0));
        o.authorization_file = Some(f.clone());
        let mut at = Attester::open(o).unwrap();
        assert!(at.refresh_authorization());
        assert_eq!(at.info().authorization.unwrap().from_epoch, 10);
    }

    /// G08: revokimet lexohen një herë në hapje; një rilexim pa ndryshim s'raporton ndryshim.
    #[test]
    fn revocations_read_once_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("rev.json");
        std::fs::write(&f, "[]").unwrap();
        let mut o = opts(None);
        o.revocations_file = Some(f.clone());
        let mut at = Attester::open(o).unwrap();
        assert_eq!(at.rev_seen.as_deref(), Some(&b"[]"[..]));
        assert!(!at.refresh_revocations());
    }

    /// Rinovimi me `cp` + `mv` (si në OPERIMI.md): edhe një skedar me mtime më të vjetër se i
    /// fundit i lexuari lexohet, sepse ndryshimi s'është "më i ri", por "tjetër".
    #[test]
    fn authorization_swap_back_with_older_mtime_is_reloaded() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("auth.json");
        let id = SigningKey::from_bytes(&[9; 32]);
        let key = SigningKey::from_bytes(&[3; 32]);
        let g = [1u8; 32];
        let body = |from: u64, until: u64| {
            serde_json::json!({
                "version": 2,
                "identity": bs58::encode(id.verifying_key().to_bytes()).into_string(),
                "signer": bs58::encode(key.verifying_key().to_bytes()).into_string(),
                "genesis": bs58::encode(g).into_string(),
                "fromEpoch": from, "untilEpoch": until,
                "authorization": hex::encode(auth(&id, &g, &key, from, until).signature),
            })
            .to_string()
        };
        std::fs::write(&f, body(0, 15)).unwrap();
        let (_, a0) = read_authorization_file(&f).unwrap();
        let mut o = dedicated(&id, &key, g, Some(a0));
        o.authorization_file = Some(f.clone());
        let mut at = Attester::open(o).unwrap();
        let ok = dir.path().join("ok.json");
        std::fs::copy(&f, &ok).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let tmp = dir.path().join("tmp.json");
        std::fs::write(&tmp, body(20, 35)).unwrap();
        std::fs::rename(&tmp, &f).unwrap();
        at.auth_checked = None;
        assert!(at.refresh_authorization());
        assert_eq!(at.info().authorization.unwrap().from_epoch, 20);
        std::fs::rename(&ok, &f).unwrap();
        at.auth_checked = None;
        assert!(at.refresh_authorization(), "kthimi te skedari i vjetër duhet lexuar");
        assert_eq!(at.info().authorization.unwrap().from_epoch, 0);
        // I njëjti mtime (dy shkrime brenda një "tik" të orës): ndryshimi duhet parë gjithsesi.
        let t = std::fs::metadata(&f).unwrap().modified().unwrap();
        std::fs::write(&f, body(20, 35)).unwrap();
        File::options().write(true).open(&f).unwrap().set_modified(t).unwrap();
        at.auth_checked = None;
        assert!(at.refresh_authorization(), "mtime i njëjtë, përmbajtje tjetër");
        assert_eq!(at.info().authorization.unwrap().from_epoch, 20);
    }

    /// R5-1: një çelës i revokuar s'ngarkohet; një revokim që vjen gjatë punës e ndalon firmosjen
    /// menjëherë; një revokim i falsifikuar injorohet; një skedar më i shkurtër s'e "çrevokon".
    #[test]
    fn revocation_stops_signing_and_cannot_be_forged_or_undone() {
        use zilja_verifier::attest::revoke_message;
        let dir = tempfile::tempdir().unwrap();
        let rf = dir.path().join("revocations.json");
        let id = SigningKey::from_bytes(&[9; 32]);
        let key = SigningKey::from_bytes(&[3; 32]);
        let g = [1u8; 32];
        let rev = |by: &SigningKey, signer: &SigningKey| {
            serde_json::json!({
                "validator": bs58::encode(by.verifying_key().to_bytes()).into_string(),
                "signer": bs58::encode(signer.verifying_key().to_bytes()).into_string(),
                "signature": hex::encode(by.sign(&revoke_message(&g, &signer.verifying_key().to_bytes())).to_bytes()),
            })
        };
        let other_id = SigningKey::from_bytes(&[7; 32]);
        let other_key = SigningKey::from_bytes(&[6; 32]);
        let thief = SigningKey::from_bytes(&[5; 32]);
        // Fillimisht: revokim i një validatori tjetër (shërbehet) + një i falsifikuar për ne (injorohet).
        let mut forged = rev(&thief, &key);
        forged["validator"] = serde_json::json!(bs58::encode(id.verifying_key().to_bytes()).into_string());
        std::fs::write(&rf, serde_json::json!([rev(&other_id, &other_key), forged]).to_string()).unwrap();
        let mk = || {
            let mut o = dedicated(&id, &key, g, Some(auth(&id, &g, &key, 800, 815)));
            o.revocations_file = Some(rf.clone());
            o
        };
        let mut at = Attester::open(mk()).unwrap();
        assert_eq!(at.info().revocations.len(), 1, "vetëm revokimi i vlefshëm shërbehet");
        assert!(at.sign(10, &[1; 32]).is_ok());
        // Gjatë punës identiteti ynë revokon çelësin: firmosja ndalon menjëherë.
        std::fs::write(&rf, serde_json::json!([rev(&other_id, &other_key), rev(&id, &key)]).to_string()).unwrap();
        at.rev_checked = None;
        assert!(at.refresh_revocations());
        assert!(at.sign(11, &[1; 32]).is_err());
        assert_eq!(at.info().revocations.len(), 2);
        // Një skedar pa revokimin s'e rikthen çelësin.
        std::fs::write(&rf, "[]").unwrap();
        at.rev_checked = None;
        at.refresh_revocations();
        assert!(at.sign(12, &[1; 32]).is_err());
        assert_eq!(at.info().revocations.len(), 2);
        drop(at);
        // Pas rinisjes me revokimin në skedar: s'ngarkohet.
        std::fs::write(&rf, serde_json::json!(rev(&id, &key)).to_string()).unwrap();
        let e = Attester::open(mk()).err().unwrap();
        assert!(e.contains("revokuar"), "{e}");
    }

    #[test]
    fn v1_authorization_file_is_refused_with_instructions() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.json");
        std::fs::write(&f, r#"{"identity":"11111111111111111111111111111111","signer":"x","genesis":"y","authorization":"00"}"#).unwrap();
        let e = read_authorization_file(&f).unwrap_err();
        assert!(e.contains("v1") && e.contains("zilja-authorize"), "{e}");
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

    /// Rresht v2 i firmosur saktë me çelësin e `opts`.
    fn line(slot: u64, root: [u8; 32]) -> String {
        let o = opts(None);
        let sig = o.key.sign(&attest_message(&o.genesis, slot, &root)).to_bytes();
        format!("{slot} {} {}", hex::encode(root), hex::encode(sig))
    }

    fn head(floor: u64) -> String {
        format!("{} floor={floor}", context_line(&opts(None)))
    }

    /// P-04: një log me dy root-e për të njëjtin slot është provë firmosjeje të dyfishtë: hapja
    /// refuzohet dhe asnjë nga të dy nuk "fiton" në heshtje. (A03: edhe e njëjta hyrje e përsëritur
    /// s'del kurrë nga shkruesi, ndaj refuzohet si rend i gabuar.)
    #[test]
    fn conflicting_roots_in_log_refuse_open() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("attest.log");
        for body in [
            format!("{}\n{}\n{}\n", line(10, [5; 32]), line(11, [7; 32]), line(10, [6; 32])),
            format!("{}\n{}\n", line(10, [5; 32]), line(10, [5; 32])),
        ] {
            std::fs::write(&p, format!("{}\n{body}", head(0))).unwrap();
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
        }
    }

    /// A03: një rekord i ndryshuar (root që dekodohet ende, slot, firmë) s'pranohet dhe s'rifirmoset.
    #[test]
    fn tampered_or_incomplete_records_are_refused() {
        let good = format!("{}\n{}\n{}\n", head(0), line(10, [0xab; 32]), line(11, [7; 32]));
        let mut root_changed = good.clone();
        root_changed = root_changed.replacen(&hex::encode([7u8; 32]), &hex::encode([8u8; 32]), 1);
        let cases: [(&str, String); 5] = [
            ("root i ndryshuar", root_changed),
            ("slot i ndryshuar", good.replacen("\n11 ", "\n12 ", 1)),
            ("pa firmë", format!("{}\n10 {}\n", head(0), hex::encode([5u8; 32]))),
            ("vetëm një fushë", format!("{}\n{}\n12\n", head(0), line(10, [5; 32]))),
            ("hex i madh", good.replacen(&hex::encode([0xabu8; 32]), &hex::encode([0xabu8; 32]).to_uppercase(), 1)),
        ];
        for (what, text) in cases {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("attest.log");
            std::fs::write(&p, &text).unwrap();
            match Attester::open(opts(Some(p.clone()))) {
                Ok(a) => panic!("{what}: u pranua; firmat e kthyera: {:?}", a.recent(5).iter().map(|x| x.0).collect::<Vec<_>>()),
                Err(e) => assert!(e.contains("refuzohet"), "{what}: {e}"),
            }
            assert_eq!(std::fs::read_to_string(&p).unwrap(), text, "{what}: skedari u ndryshua");
        }
        // I pandryshuari hapet dhe kthen firmat origjinale.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("attest.log");
        std::fs::write(&p, &good).unwrap();
        let a = Attester::open(opts(Some(p))).unwrap();
        let o = opts(None);
        for (s, r, g) in a.recent(5) {
            assert_eq!(g, o.key.sign(&attest_message(&o.genesis, s, &r)).to_bytes());
        }
    }

    /// A03: rreshti i fundit pa '\n' (shkrim i ndërprerë; firma s'u kthye kurrë) ruhet dhe hiqet.
    #[test]
    fn torn_last_line_is_dropped_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("attest.log");
        let full = format!("{}\n{}\n", head(0), line(10, [5; 32]));
        std::fs::write(&p, format!("{full}11 {}", &hex::encode([7u8; 32])[..20])).unwrap();
        let mut a = Attester::open(opts(Some(p.clone()))).unwrap();
        assert_eq!(a.recent(5).len(), 1);
        assert!(std::fs::read_dir(dir.path()).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().contains(".tail-")));
        a.sign(11, &[7; 32]).unwrap();
        drop(a);
        assert_eq!(Attester::open(opts(Some(p))).unwrap().recent(5).len(), 2);
    }

    /// A03: v1 s'ka firma. Migrimi s'i jep vlefshmëri historisë: s'shërbehet, s'rifirmoset, dhe asnjë
    /// slot deri te më i madhi i saj s'firmoset më. Origjinali ruhet.
    #[test]
    fn v1_log_migrates_without_trusting_history() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("attest.log");
        let v1 = format!("{}\n10 {}\n20 {}\n", head(0).replacen(HEADER, HEADER_V1, 1).replace(" floor=0", ""), hex::encode([5u8; 32]), hex::encode([6u8; 32]));
        std::fs::write(&p, &v1).unwrap();
        let mut a = Attester::open(opts(Some(p.clone()))).unwrap();
        assert!(a.recent(10).is_empty(), "historia pa firmë u shërbye");
        assert!(a.sign(15, &[1; 32]).is_err(), "slot brenda historisë së vjetër u firmos");
        assert!(a.sign(20, &[6; 32]).is_err());
        assert!(a.sign(21, &[1; 32]).is_ok());
        drop(a);
        assert_eq!(std::fs::read_to_string(dir.path().join("attest.log.v1-migrated")).unwrap(), v1);
        assert!(std::fs::read_to_string(&p).unwrap().starts_with(HEADER));
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
                o.authorization = Some(auth(&id, &o.genesis, &o.key, 1, 2));
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
        // A03: kufiri i historisë së kompaktuar mbijeton rinisjen.
        let mut a = Attester::open(opts(Some(p.clone()))).unwrap();
        assert!(a.sign(0, &[0; 32]).is_err(), "slot nën kufirin e kompaktuar u firmos pas rinisjes");
        drop(a);
        let mut o = opts(Some(p));
        o.genesis = [2; 32];
        assert!(Attester::open(o).is_err());
    }
}

