//! Serveri RPC i Ziles (JSON-RPC 2.0 mbi HTTP).
//!
//! Metodat:
//! - `getAccountProof [pubkey_base58, {"slot": n}?]` → dëftesa në slotin e fundit të rrënjosur,
//!   ose në gjendjen e një sloti të kaluar brenda kujtesës (vetëm me pemën në disk). `slot` në
//!   përgjigje është sloti i rrënjosur që u përdor vërtet (≤ i kërkuari, L-05).
//! - `getMultipleAccountProofs [[pubkey, ...], {slot}?]` → `{slot, root, complete, accounts, proof}` (R5-2)
//! - `getZiljaRoot [slot?]` → `{slot, root, complete, provableFrom, accounts?}`; me `slot`, root-i i atij
//!   sloti nga unaza e sloteve të fundit (Simpler-5). A19: `accounts` (i pemës aktuale) jepet vetëm për
//!   slotin aktual; mbi majë `-32008`.
//!
//! `lamports` dhe `dataLen` dalin si tekst dhjetor (F-08): JSON-i s'mund t'i mbajë saktë mbi 2^53.
//! - `getZiljaAttestation [slot?]` → `{slot, root, validator, signer, authorization, genesis, signature}`;
//!   `authorization` = `{fromEpoch, untilEpoch, signature}` (v2) ose `null` kur firmos vetë identiteti
//!   (kur zilja firmos; vetëm për pemë të plota, F-10).
//!
//! Rrugë GET: `/health` (200 vetëm kur jep dëftesa, përndryshe 503), `/metrics` (Prometheus,
//! zakonisht në një port privat). Trupi i kërkesës ≤ 64 KB, me kohë maksimale leximi.

use crate::service::{Shared, State, PHASE_HALTED, PHASE_RECOVERED, PHASE_SERVING};
use zilja_tree::ProveError;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use zilja_verifier::{AccountMeta, Key};

/// Gabim RPC: (kodi, mesazhi).
pub type RpcError = (i64, String);

/// P-09: kodet e gabimeve. -32602 vetëm për parametra të pavlefshëm.
pub const CODE_NOT_READY: i64 = -32002;
pub const CODE_HALTED: i64 = -32003;
pub const CODE_OUT_OF_HISTORY: i64 = -32007;
/// A19: sloti i kërkuar është pas slotit të fundit të rrënjosur: përgjigja s'do të ishte e qëndrueshme.
pub const CODE_NOT_YET_ROOTED: i64 = -32008;
/// Gabim I/O ose dëmtim i ruajtjes: zilja ndalon së shërbyeri (s'garanton më korrektësinë).
pub const CODE_STORAGE: i64 = -32010;

/// Ndërton përgjigjen e `getAccountProof` nga gjendja (`at = None` = sloti i fundit).
/// L-05 / A19: sloti i kërkuar → sloti i rrënjosur që i përgjigjet (ose gabimi).
fn resolve_at(st: &State, at: Option<u64>) -> Result<Option<u64>, RpcError> {
    Ok(match at {
        None => None,
        Some(v) if v == st.slot => Some(st.slot),
        // A19: mbi majë s'ka përgjigje të qëndrueshme ("i fundit ≤ v" do të ndryshonte me rrënjosjet e reja).
        Some(v) if v > st.slot => return Err((CODE_NOT_YET_ROOTED, format!("sloti {v} s'është rrënjosur ende (i fundit: {})", st.slot))),
        Some(v) => match st.roots.range(..=v).next_back() {
            Some((s, _)) => Some(*s),
            None => return Err((CODE_OUT_OF_HISTORY, format!("sloti {v} është jashtë kujtesës"))),
        },
    })
}

fn prove_err(e: ProveError) -> RpcError {
    match e {
        ProveError::NotReady(m) => (CODE_NOT_READY, m),
        ProveError::OutOfHistory(m) => (CODE_OUT_OF_HISTORY, m),
        ProveError::Storage(m) => (CODE_STORAGE, format!("gabim i ruajtjes: {m}")),
    }
}

fn meta_json(m: &AccountMeta) -> Value {
    json!({
        "lamports": m.lamports.to_string(),
        "owner": bs58::encode(m.owner).into_string(),
        "executable": m.executable,
        "dataLen": m.data_len.to_string(),
        "dataHash": hex::encode(m.data_hash),
    })
}

/// R5-2: një multi-proof për deri në `MAX_MULTI_KEYS` llogari, të gjitha nga i njëjti root (i
/// njëjti kyç leximi, i njëjti slot). `accounts` del në radhën e kërkesës.
pub fn multi_proof_json(st: &State, pubkeys: &[Key], at: Option<u64>) -> Result<Value, RpcError> {
    use zilja_verifier::multi::{MultiProof, MAX_MULTI_KEYS};
    if pubkeys.is_empty() || pubkeys.len() > MAX_MULTI_KEYS {
        return Err((-32602, format!("duhen 1..={MAX_MULTI_KEYS} pubkey")));
    }
    let at = resolve_at(st, at)?;
    let mut entries = Vec::with_capacity(pubkeys.len());
    let mut metas = Vec::with_capacity(pubkeys.len());
    let mut slot_root = None;
    for pk in pubkeys {
        let tk = zilja_verifier::tree_key(pk);
        let (slot, root, proof, meta) = st.tree.prove_at(&tk, at).map_err(prove_err)?;
        match slot_root {
            None => slot_root = Some((slot, root)),
            Some(x) if x != (slot, root) => return Err((CODE_STORAGE, "gabim i ruajtjes: dëftesat dolën nga root-e të ndryshme".into())),
            _ => {}
        }
        if meta.is_none() && !st.complete {
            return Err((-32006, "pema e kësaj zileje nuk mbulon çdo llogari (nisi pa snapshot): s'jep dëftesa mospërfshirjeje".into()));
        }
        metas.push(meta);
        entries.push((tk, proof));
    }
    let (slot, root) = slot_root.expect("të paktën një pubkey");
    let (_, mp) = MultiProof::from_proofs(&entries).map_err(|e| (CODE_STORAGE, format!("gabim i ruajtjes: {e}")))?;
    let bytes = mp.to_bytes();
    let accounts: Vec<Value> = pubkeys
        .iter()
        .zip(&metas)
        .map(|(pk, m)| json!({"pubkey": bs58::encode(pk).into_string(), "account": m.as_ref().map(meta_json)}))
        .collect();
    Ok(json!({
        "slot": slot,
        "root": hex::encode(root),
        "complete": st.complete,
        "accounts": accounts,
        "proof": hex::encode(&bytes),
        "proofBytes": bytes.len(),
    }))
}

pub fn account_proof_json(st: &State, pubkey: &Key, at: Option<u64>) -> Result<Value, RpcError> {
    // L-05: gjendja në slotin `v` është ajo e slotit më të fundit të rrënjosur ≤ v; ai etiketohet.
    let at = resolve_at(st, at)?;
    let (slot, root, proof, meta) = st.tree.prove_at(&zilja_verifier::tree_key(pubkey), at).map_err(prove_err)?;
    // L-02: një pemë që s'mbulon çdo llogari nuk jep dëftesa "nuk ekziston".
    if meta.is_none() && !st.complete {
        return Err((-32006, "pema e kësaj zileje nuk mbulon çdo llogari (nisi pa snapshot): s'jep dëftesa mospërfshirjeje".into()));
    }
    let bytes = proof.to_bytes();
    let account = meta.as_ref().map(meta_json);
    Ok(json!({
        "slot": slot,
        "root": hex::encode(root),
        "complete": st.complete,
        "pubkey": bs58::encode(pubkey).into_string(),
        "account": account,
        "proof": hex::encode(&bytes),
        "proofBytes": bytes.len(),
    }))
}

fn parse_pubkey(s: &str) -> Result<Key, String> {
    let v = bs58::decode(s).into_vec().map_err(|e| format!("pubkey jo base58: {e}"))?;
    v.try_into().map_err(|_| "pubkey duhet të ketë 32 bajt".to_string())
}

/// Përpunon një kërkesë JSON-RPC (ndarë nga HTTP, që të testohet lehtë).
pub fn handle_jsonrpc(shared: &Shared, body: &str) -> Value {
    let req: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return err(Value::Null, -32700, &format!("JSON i pavlefshëm: {e}")),
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!([]));
    // L-04: params, nëse jepen, duhet të jenë listë.
    let Some(params) = params.as_array().cloned() else {
        return err(id, -32602, "params duhet të jenë listë");
    };
    let m = &shared.metrics;
    // P-10: kyçi mund të jetë "i helmuar" nëse punëtori ra duke e mbajtur; leximi mbetet i sigurt.
    let st = shared.state.read().unwrap_or_else(|e| e.into_inner());
    if m.halted.load(Ordering::Relaxed) || m.phase.load(Ordering::Relaxed) == PHASE_HALTED {
        let why = st.broken.clone().unwrap_or_else(|| "punëtori s'punon".into());
        return err(id, CODE_HALTED, &format!("zilja ndaloi: {why}"));
    }
    if !st.ready {
        return match &st.broken {
            Some(why) => err(id, CODE_HALTED, &format!("zilja ndaloi: {why}")),
            None if m.phase.load(Ordering::Relaxed) == PHASE_RECOVERED => {
                err(id, CODE_NOT_READY, "gjendja u hap nga disku por zinxhiri s'e ka konfirmuar ende (P-08)")
            }
            None => err(id, CODE_NOT_READY, "zilja ende po ndërton pemën fillestare"),
        };
    }
    match method {
        "getMultipleAccountProofs" => {
            let (Some(list), true) = (params.first().and_then(|p| p.as_array()), params.len() <= 2) else {
                return err(id, -32602, "duhet params: [[pubkey, ...], {slot}?]");
            };
            let at = match parse_slot_opt(params.get(1)) {
                Ok(a) => a,
                Err(e) => return err(id, -32602, &e),
            };
            let mut pks = Vec::with_capacity(list.len());
            for p in list {
                match p.as_str().ok_or_else(|| "pubkey duhet tekst base58".to_string()).and_then(parse_pubkey) {
                    Ok(k) => pks.push(k),
                    Err(e) => return err(id, -32602, &e),
                }
            }
            match multi_proof_json(&st, &pks, at) {
                Ok(res) => {
                    shared.metrics.proofs_served.fetch_add(pks.len() as u64, Ordering::Relaxed);
                    json!({"jsonrpc": "2.0", "id": id, "result": res})
                }
                Err((code, e)) if code == CODE_STORAGE => {
                    drop(st);
                    crate::service::stop_serving(shared, e.clone());
                    err(id, code, &e)
                }
                Err((code, e)) => err(id, code, &e),
            }
        }
        "getAccountProof" => {
            let (Some(pk), true) = (params.first().and_then(|p| p.as_str()), params.len() <= 2) else {
                return err(id, -32602, "duhet params: [pubkey, {slot}?]");
            };
            let at = match params.get(1) {
                None => None,
                Some(o) => match o.as_object() {
                    Some(m) if m.keys().all(|k| k == "slot") => match m.get("slot") {
                        None => None,
                        Some(s) => match s.as_u64() {
                            Some(s) => Some(s),
                            None => return err(id, -32602, "slot duhet numër i plotë jo negativ"),
                        },
                    },
                    _ => return err(id, -32602, "opsionet e njohura: {slot}"),
                },
            };
            match parse_pubkey(pk).map_err(|e| (-32602, e)).and_then(|k| account_proof_json(&st, &k, at)) {
                Ok(res) => {
                    shared.metrics.proofs_served.fetch_add(1, Ordering::Relaxed);
                    json!({"jsonrpc": "2.0", "id": id, "result": res})
                }
                Err((code, e)) if code == CODE_STORAGE => {
                    // P-09/P-10: ruajtja s'lexohet saktë: asnjë përgjigje tjetër s'garantohet më.
                    drop(st);
                    crate::service::stop_serving(shared, e.clone());
                    err(id, code, &e)
                }
                Err((code, e)) => err(id, code, &e),
            }
        }
        "getZiljaRoot" => {
            if params.len() > 1 {
                return err(id, -32602, "duhet params: [slot?]");
            }
            let (slot, root) = match params.first() {
                None => (st.slot, st.tree.root()),
                Some(v) => match v.as_u64() {
                    Some(s) if s == st.slot => (s, st.tree.root()),
                    Some(s) if s > st.slot => return err(id, CODE_NOT_YET_ROOTED, &format!("sloti {s} s'është rrënjosur ende (i fundit: {})", st.slot)),
                    Some(s) => match st.roots.get(&s) {
                        Some(r) => (s, *r),
                        None => return err(id, -32005, &format!("s'ka root për slotin {s}")),
                    },
                    None => return err(id, -32602, "slot duhet numër i plotë jo negativ"),
                },
            };
            // A19: `accounts` është i pemës aktuale: jepet vetëm kur përgjigja është për slotin aktual.
            let mut res = json!({
                "slot": slot, "root": hex::encode(root), "complete": st.complete,
                "provableFrom": st.tree.provable_range().map(|r| r.0),
            });
            if slot == st.slot {
                res["accounts"] = json!(st.tree.len());
            }
            json!({"jsonrpc": "2.0", "id": id, "result": res})
        }
        "getZiljaAttestation" => {
            if params.len() > 1 {
                return err(id, -32602, "duhet params: [slot?]");
            }
            let Some(attester) = st.attester.clone() else {
                return err(id, -32004, "kjo zile nuk firmos (mungon attest_keypair)");
            };
            if !st.complete {
                return err(id, -32004, "kjo zile s'firmos: pema nuk mbulon çdo llogari (nisi pa snapshot)");
            }
            let wanted = match params.first() {
                None => None,
                Some(v) => match v.as_u64() {
                    Some(s) => Some(s),
                    None => return err(id, -32602, "slot duhet numër i plotë jo negativ"),
                },
            };
            let found = match wanted {
                Some(s) => st.attestations.get(&s).map(|a| (s, a)),
                None => st.attestations.iter().next_back().map(|(s, a)| (*s, a)),
            };
            match found {
                Some((slot, a)) => json!({"jsonrpc": "2.0", "id": id, "result": {
                    "slot": slot,
                    "root": hex::encode(a.root),
                    "validator": bs58::encode(attester.identity).into_string(),
                    "signer": (attester.signer != attester.identity).then(|| bs58::encode(attester.signer).into_string()),
                    "authorization": attester.authorization.map(|a| json!({
                        "fromEpoch": a.from_epoch, "untilEpoch": a.until_epoch, "signature": hex::encode(a.signature),
                    })),
                    "genesis": bs58::encode(attester.genesis).into_string(),
                    "signature": hex::encode(a.signature),
                }}),
                None => err(id, -32005, "s'ka firmë për këtë slot"),
            }
        }
        _ => err(id, -32601, "metodë e panjohur"),
    }
}

/// `{slot}?` si te `getAccountProof`.
fn parse_slot_opt(o: Option<&Value>) -> Result<Option<u64>, String> {
    match o {
        None => Ok(None),
        Some(o) => match o.as_object() {
            Some(m) if m.keys().all(|k| k == "slot") => match m.get("slot") {
                None => Ok(None),
                Some(s) => s.as_u64().map(Some).ok_or_else(|| "slot duhet numër i plotë jo negativ".to_string()),
            },
            _ => Err("opsionet e njohura: {slot}".into()),
        },
    }
}

fn err(id: Value, code: i64, msg: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}})
}

/// Kufijtë e serverit HTTP (F-06).
pub const MAX_BODY: usize = 64 << 10;
const MAX_HEADERS: usize = 16 << 10;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// A17: afati i plotë i një kërkese, monoton, që nga pranimi i lidhjes: përfshin kohën në radhë,
/// leximin e kokave dhe të trupit, dhe shkrimin e përgjigjes. `IO_TIMEOUT` kufizon vetëm një
/// operacion; pa këtë, një klient që dërgon një bajt në çdo 4 s e mbante punëtorin pa fund.
pub const REQUEST_DEADLINE: Duration = Duration::from_secs(5);
const WORKERS: usize = 8;
/// R4-3: sa lidhje njëkohësisht (në radhë + në shërbim) lejohen nga një IP në një port. Me 8
/// punëtorë, një sulmues duhet ≥ 4 IP për t'i zënë të gjithë (secila lidhje zgjat ≤ afatin e
/// kërkesës). Loopback-u përjashtohet si parazgjedhje: një reverse proxy në të njëjtin host
/// (OPERIMI.md) i sjell të gjitha kërkesat nga 127.0.0.1 dhe i buferon vetë kërkesat e ngadalta.
pub const MAX_CONN_PER_IP: usize = 2;

/// Kufijtë e lidhjeve të një porti.
#[derive(Clone, Copy, Debug)]
pub struct HttpLimits {
    /// Afati i plotë i një kërkese (A17).
    pub deadline: Duration,
    /// Lidhje njëkohësisht për IP (R4-3).
    pub per_ip: usize,
    /// Loopback-u s'kufizohet për IP.
    pub exempt_loopback: bool,
}

impl Default for HttpLimits {
    fn default() -> Self {
        HttpLimits { deadline: REQUEST_DEADLINE, per_ip: MAX_CONN_PER_IP, exempt_loopback: true }
    }
}

type PerIp = Arc<std::sync::Mutex<std::collections::HashMap<std::net::IpAddr, usize>>>;

/// Një vend i zënë për IP-në e lidhjes; lirohet kur lidhja mbaron (edhe në gabim ose panic).
struct IpSlot {
    map: PerIp,
    ip: Option<std::net::IpAddr>,
}

impl IpSlot {
    /// `None` kur IP-ja ka arritur kufirin.
    fn take(map: &PerIp, ip: std::net::IpAddr, l: &HttpLimits) -> Option<IpSlot> {
        if l.exempt_loopback && ip.is_loopback() {
            return Some(IpSlot { map: map.clone(), ip: None });
        }
        let mut m = map.lock().unwrap();
        let n = m.entry(ip).or_insert(0);
        if *n >= l.per_ip {
            return None;
        }
        *n += 1;
        Some(IpSlot { map: map.clone(), ip: Some(ip) })
    }
}

impl Drop for IpSlot {
    fn drop(&mut self) {
        if let Some(ip) = self.ip {
            let mut m = self.map.lock().unwrap();
            if let Some(n) = m.get_mut(&ip) {
                *n -= 1;
                if *n == 0 {
                    m.remove(&ip);
                }
            }
        }
    }
}

/// Çfarë shërben një port.
#[derive(Clone, Copy, Debug)]
pub struct Routes {
    /// JSON-RPC me POST.
    pub jsonrpc: bool,
    /// `GET /metrics`. Parazgjedhja: në një port privat më vete.
    pub metrics: bool,
    /// Kokat CORS (`*`). Parazgjedhja: fikur; ndize vetëm kur faqja demo flet direkt me ziles.
    pub cors: bool,
}

/// Një server HTTP që mund të ndalet dhe të pritet (F-07).
pub struct HttpServer {
    stop: Arc<AtomicBool>,
    addr: SocketAddr,
    threads: Vec<JoinHandle<()>>,
}

impl HttpServer {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Ndalon pranimin, pret lidhjet që po shërbehen dhe të gjithë thread-et.
    pub fn stop(&mut self) {
        if self.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        // Zgjo `accept()` me një lidhje te vetja.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1));
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// JSON-RPC + /health (+ /metrics) në të njëjtin port, pa CORS.
pub fn serve(bind: &str, shared: Shared) -> std::io::Result<HttpServer> {
    serve_routes(bind, shared, Routes { jsonrpc: true, metrics: true, cors: false })
}

/// Nis serverin: një thread pranon lidhjet dhe disa punëtorë i shërbejnë. Çdo lidhje ka
/// kohë maksimale leximi/shkrimi dhe kufi për trupin, ndaj një klient i ngadaltë zë
/// vetëm një punëtor për pak sekonda dhe s'bllokon kurrë /health.
pub fn serve_routes(bind: &str, shared: Shared, routes: Routes) -> std::io::Result<HttpServer> {
    serve_routes_with(bind, shared, routes, HttpLimits::default())
}

/// Si `serve_routes`, me kufijtë e dhënë (testet).
pub fn serve_routes_with(bind: &str, shared: Shared, routes: Routes, limits: HttpLimits) -> std::io::Result<HttpServer> {
    let listener = TcpListener::bind(bind)?;
    let addr = listener.local_addr()?;
    let stop = Arc::new(AtomicBool::new(false));
    let deadline = limits.deadline;
    let per_ip: PerIp = Arc::default();
    let (tx, rx) = crossbeam_channel::bounded::<(TcpStream, Instant, IpSlot)>(WORKERS * 8);
    let mut threads = Vec::new();
    for i in 0..WORKERS {
        let rx = rx.clone();
        let sh = shared.clone();
        threads.push(std::thread::Builder::new().name(format!("zilja-http-{i}")).spawn(move || {
            // `_slot` lirohet në fund të çdo iterimi, pasi lidhja mbyllet.
            for (mut stream, accepted, _slot) in rx.iter() {
                let until = accepted + deadline;
                // A17: e skaduar në radhë: përgjigje e shkurtër dhe mbyllje, pa lexuar asgjë.
                if Instant::now() >= until {
                    let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
                    let _ = write_resp(&mut stream, 503, "text/plain", b"timeout", false);
                    continue;
                }
                let _ = handle_conn(stream, &sh, routes, until);
            }
        })?);
    }
    let st = stop.clone();
    threads.push(std::thread::Builder::new().name("zilja-http-accept".into()).spawn(move || {
        for conn in listener.incoming() {
            if st.load(Ordering::SeqCst) {
                break;
            }
            let Ok(mut s) = conn else { continue };
            let accepted = Instant::now();
            let _ = s.set_read_timeout(Some(IO_TIMEOUT));
            let _ = s.set_write_timeout(Some(IO_TIMEOUT));
            // R4-3: kufi lidhjesh për IP, para radhës: një IP s'mund t'i zërë të gjithë punëtorët.
            let slot = match s.peer_addr().map(|a| IpSlot::take(&per_ip, a.ip(), &limits)) {
                Ok(Some(slot)) => slot,
                _ => {
                    let _ = s.set_write_timeout(Some(Duration::from_millis(200)));
                    let _ = write_resp(&mut s, 503, "text/plain", b"too many connections from this address", false);
                    continue;
                }
            };
            if let Err(crossbeam_channel::TrySendError::Full((mut s, _, _))) = tx.try_send((s, accepted, slot)) {
                let _ = s.set_write_timeout(Some(Duration::from_millis(200)));
                let _ = write_resp(&mut s, 503, "text/plain", b"busy", false);
            }
        }
        // `tx` bie këtu: punëtorët mbarojnë lidhjet në radhë dhe dalin.
    })?);
    log::info!("zilja: HTTP po dëgjon në http://{addr} ({routes:?})");
    Ok(HttpServer { stop, addr, threads })
}

fn write_resp(s: &mut TcpStream, code: u16, ctype: &str, body: &[u8], cors: bool) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        411 => "Length Required",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let mut h = format!("HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    if cors {
        h.push_str("Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: content-type\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\n");
    }
    h.push_str("\r\n");
    s.write_all(h.as_bytes())?;
    s.write_all(body)?;
    s.flush()
}

/// `/health`: 200 vetëm kur zilja shërben, s'ka ndaluar, punëtori jeton dhe root-i është i freskët
/// (P-08, P-10). Lexon vetëm atomike, ndaj s'bllokohet nga punëtori ose nga kërkesat.
fn health(shared: &Shared) -> (u16, &'static str) {
    let m = &shared.metrics;
    if m.broken.load(Ordering::Relaxed) == 1 || m.halted.load(Ordering::Relaxed) || !m.worker_alive.load(Ordering::Relaxed) {
        return (503, "halted");
    }
    match m.phase.load(Ordering::Relaxed) {
        PHASE_SERVING if m.ready.load(Ordering::Relaxed) == 1 => {
            let max = m.max_root_age_secs.load(Ordering::Relaxed);
            let last = m.last_root_unix.load(Ordering::Relaxed);
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            if max > 0 && now.saturating_sub(last) > max {
                (503, "stale")
            } else {
                (200, "ok")
            }
        }
        PHASE_RECOVERED => (503, "recovered"),
        PHASE_HALTED => (503, "halted"),
        _ => (503, "starting"),
    }
}

/// A17: koha që i mbetet kërkesës; çdo lexim/shkrim merr vetëm aq (dhe kurrë më shumë se IO_TIMEOUT).
fn budget(s: &TcpStream, until: Instant) -> std::io::Result<()> {
    let left = until.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "afati i kërkesës kaloi"));
    }
    let t = Some(left.min(IO_TIMEOUT));
    s.set_read_timeout(t)?;
    s.set_write_timeout(t)
}

fn handle_conn(mut s: TcpStream, shared: &Shared, routes: Routes, until: Instant) -> std::io::Result<()> {
    // Kokat: lexo deri te rreshti bosh, jo më shumë se MAX_HEADERS.
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
        if buf.len() > MAX_HEADERS {
            return write_resp(&mut s, 431, "text/plain", b"headers too large", routes.cors);
        }
        budget(&s, until)?;
        let n = s.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (method, path) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
    let mut len: Option<usize> = None;
    for l in lines {
        let Some((k, v)) = l.split_once(':') else { continue };
        let k = k.trim().to_ascii_lowercase();
        if k == "content-length" {
            match v.trim().parse::<usize>() {
                Ok(n) => len = Some(n),
                Err(_) => return write_resp(&mut s, 400, "text/plain", b"bad content-length", routes.cors),
            }
        } else if k == "transfer-encoding" {
            return write_resp(&mut s, 411, "text/plain", b"content-length required", routes.cors);
        }
    }
    let cors = routes.cors;
    budget(&s, until)?;
    match (method, path) {
        ("OPTIONS", _) if cors => write_resp(&mut s, 204, "text/plain", b"", cors),
        ("GET", "/health") => {
            let (code, body) = health(shared);
            write_resp(&mut s, code, "text/plain", body.as_bytes(), cors)
        }
        ("GET", "/metrics") if routes.metrics => {
            write_resp(&mut s, 200, "text/plain; version=0.0.4", shared.metrics.prometheus().as_bytes(), cors)
        }
        ("POST", _) if routes.jsonrpc => {
            let Some(n) = len else {
                return write_resp(&mut s, 411, "text/plain", b"content-length required", cors);
            };
            if n > MAX_BODY {
                return write_resp(&mut s, 413, "text/plain", b"body too large", cors);
            }
            let mut body = buf[head_end..].to_vec();
            body.truncate(n);
            while body.len() < n {
                budget(&s, until)?;
                let k = s.read(&mut chunk)?;
                if k == 0 {
                    return Ok(());
                }
                body.extend_from_slice(&chunk[..k.min(n - body.len())]);
            }
            let v = handle_jsonrpc(shared, &String::from_utf8_lossy(&body));
            budget(&s, until)?;
            write_resp(&mut s, 200, "application/json", v.to_string().as_bytes(), cors)
        }
        _ => write_resp(&mut s, 404, "text/plain", b"not found", cors),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::Service;
    use crate::tracker::{Event, Status};
    use zilja_verifier::{verify_account, AccountMeta, Proof, Verdict};

    fn decode_meta(v: &Value) -> AccountMeta {
        AccountMeta {
            lamports: v["lamports"].as_str().unwrap().parse().unwrap(),
            owner: bs58::decode(v["owner"].as_str().unwrap()).into_vec().unwrap().try_into().unwrap(),
            executable: v["executable"].as_bool().unwrap(),
            data_len: v["dataLen"].as_str().unwrap().parse().unwrap(),
            data_hash: hex::decode(v["dataHash"].as_str().unwrap()).unwrap().try_into().unwrap(),
        }
    }

    #[test]
    fn end_to_end_service_and_jsonrpc() {
        let mut svc = Service::start_opts(Default::default()).unwrap();
        let a = [1u8; 32];
        let b = [2u8; 32];
        let owner = [9u8; 32];
        svc.send(Event::Snapshot { slot: 5, pubkey: a, meta: Some(AccountMeta::from_account(10, &owner, false, b"a")) });
        svc.send(Event::EndOfStartup);
        svc.send(Event::Status { slot: 6, parent: Some(5), bank_id: Some(1), status: Status::CreatedBank });
        svc.send(Event::Account {
            slot: 6,
            bank_id: 1,
            pubkey: b,
            write_version: 1,
            meta: Some(AccountMeta::from_account(20, &owner, false, b"bb")),
        });
        svc.send(Event::Status { slot: 6, parent: Some(5), bank_id: Some(1), status: Status::Rooted });
        let shared = svc.shared.clone();
        svc.shutdown();

        let root_resp = handle_jsonrpc(&shared, r#"{"jsonrpc":"2.0","id":1,"method":"getZiljaRoot"}"#);
        assert_eq!(root_resp["result"]["slot"], 6);
        assert_eq!(root_resp["result"]["accounts"], 2);
        let root: [u8; 32] = hex::decode(root_resp["result"]["root"].as_str().unwrap()).unwrap().try_into().unwrap();

        let pk = bs58::encode(b).into_string();
        let r = handle_jsonrpc(&shared, &format!(r#"{{"jsonrpc":"2.0","id":2,"method":"getAccountProof","params":["{pk}"]}}"#));
        let res = &r["result"];
        let proof = Proof::from_bytes(&hex::decode(res["proof"].as_str().unwrap()).unwrap()).unwrap();
        let meta = decode_meta(&res["account"]);
        assert_eq!(meta.lamports, 20);
        assert_eq!(verify_account(&root, &b, Some(&meta), &proof), Verdict::Exists);

        // Llogari që s'ekziston.
        let none = bs58::encode([3u8; 32]).into_string();
        let r = handle_jsonrpc(&shared, &format!(r#"{{"jsonrpc":"2.0","id":3,"method":"getAccountProof","params":["{none}"]}}"#));
        assert!(r["result"]["account"].is_null());
        let proof = Proof::from_bytes(&hex::decode(r["result"]["proof"].as_str().unwrap()).unwrap()).unwrap();
        assert_eq!(verify_account(&root, &[3u8; 32], None, &proof), Verdict::DoesNotExist);

        // Gabimet.
        // Kjo zile nuk firmos.
        assert_eq!(handle_jsonrpc(&shared, r#"{"id":1,"method":"getZiljaAttestation"}"#)["error"]["code"], -32004);

        assert_eq!(handle_jsonrpc(&shared, "jo json")["error"]["code"], -32700);
        // L-04: parametrat e gabuar refuzohen.
        let pk = bs58::encode(b).into_string();
        for bad in [
            format!(r#"{{"id":1,"method":"getAccountProof","params":["{pk}",{{"slot":-1}}]}}"#),
            format!(r#"{{"id":1,"method":"getAccountProof","params":["{pk}",{{"slot":"5"}}]}}"#),
            format!(r#"{{"id":1,"method":"getAccountProof","params":["{pk}",{{"slott":5}}]}}"#),
            format!(r#"{{"id":1,"method":"getAccountProof","params":{{"pubkey":"{pk}"}}}}"#),
            r#"{"id":1,"method":"getZiljaRoot","params":["x"]}"#.to_string(),
        ] {
            assert_eq!(handle_jsonrpc(&shared, &bad)["error"]["code"], -32602, "{bad}");
        }
        // Simpler-5: root-i i një sloti të kaluar nga unaza.
        let r5 = handle_jsonrpc(&shared, r#"{"id":1,"method":"getZiljaRoot","params":[5]}"#);
        assert_eq!(r5["result"]["slot"], 5, "{r5}");
        assert_ne!(r5["result"]["root"], root_resp["result"]["root"]);
        assert_eq!(handle_jsonrpc(&shared, r#"{"id":1,"method":"getZiljaRoot","params":[4]}"#)["error"]["code"], -32005);
        assert_eq!(handle_jsonrpc(&shared, r#"{"id":1,"method":"x"}"#)["error"]["code"], -32601);
        assert_eq!(
            handle_jsonrpc(&shared, r#"{"id":1,"method":"getAccountProof","params":["0OIl"]}"#)["error"]["code"],
            -32602
        );
    }
}

#[cfg(test)]
mod attest_tests {
    use super::*;
    use crate::service::Service;
    use crate::tracker::{Event, Status};
    use ed25519_dalek::SigningKey;
    use zilja_verifier::attest::{verify_quorum, Attestation, StakeTable};
    use zilja_verifier::AccountMeta;

    #[test]
    fn signed_roots_reach_quorum() {
        let keys: Vec<SigningKey> = (1u8..=4).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
        let mut shareds = Vec::new();
        for k in &keys {
            let attest = crate::AttestOptions { key: k.clone(), identity: None, authorization: None, authorization_file: None, genesis: [0x33; 32], log: None, adopt_legacy: false };
            let mut svc = Service::start_opts(crate::Options { attest: Some(attest), ..Default::default() }).unwrap();
            // Të gjitha zilet shohin të njëjtin zinxhir: duhet të dalë i njëjti root.
            svc.send(Event::Snapshot { slot: 1, pubkey: [7; 32], meta: Some(AccountMeta { lamports: 5, ..Default::default() }) });
            svc.send(Event::EndOfStartup);
            svc.send(Event::Status { slot: 2, parent: Some(1), bank_id: Some(9), status: Status::CreatedBank });
            svc.send(Event::Account { slot: 2, bank_id: 9, pubkey: [8; 32], write_version: 1, meta: Some(AccountMeta { lamports: 6, ..Default::default() }) });
            svc.send(Event::Status { slot: 2, parent: Some(1), bank_id: Some(9), status: Status::Rooted });
            let sh = svc.shared.clone();
            svc.shutdown();
            shareds.push(sh);
        }
        let mut atts = Vec::new();
        let mut root = None;
        for sh in &shareds {
            let r = handle_jsonrpc(sh, r#"{"id":1,"method":"getZiljaAttestation","params":[2]}"#)["result"].clone();
            let rt: [u8; 32] = hex::decode(r["root"].as_str().unwrap()).unwrap().try_into().unwrap();
            assert!(root.is_none() || root == Some(rt));
            root = Some(rt);
            atts.push(Attestation {
                signer: None,
                authorization: None,
                validator: bs58::decode(r["validator"].as_str().unwrap()).into_vec().unwrap().try_into().unwrap(),
                signature: hex::decode(r["signature"].as_str().unwrap()).unwrap().try_into().unwrap(),
            });
        }
        let table = StakeTable { entries: keys.iter().map(|k| (k.verifying_key().to_bytes(), 10)).collect() };
        let root = root.unwrap();
        assert!(verify_quorum(&[0x33; 32], 0, 2, &root, &atts, &table).reached);
        assert!(!verify_quorum(&[0x33; 32], 0, 2, &root, &atts[..2], &table).reached);
    }
}
