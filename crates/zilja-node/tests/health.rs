//! P-08, P-09, P-10: gjendja e shëndetit dhe ndalimi i shërbimit.
//!
//! Pranimi: pas një gabimi kritik ose ndalimit të punëtorit, endpoint-et normale s'vazhdojnë të
//! japin sukses sikur sistemi të ishte i shëndetshëm.

use serde_json::Value;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zilja_node::{rpc, Event, Options, Service, Shared, Status};
use zilja_store::{PageOptions, PageTree};
use zilja_tree::{MemTree, ProofAt, Resume, StateTree, Update};
use zilja_verifier::{AccountMeta, Hash, Key};

fn m(l: u64) -> Option<AccountMeta> {
    Some(AccountMeta { lamports: l, ..Default::default() })
}
fn hs(s: u64) -> [u8; 32] {
    [s as u8; 32]
}
/// Sloti `s` (prindi `s-1`) me një ndryshim, i rrënjosur pasi fëmija ekziston (si në validator).
fn slot(svc: &Service, s: u64, key: u8, lamports: u64) {
    let entries = |top: u64| (top.saturating_sub(19)..=top).rev().map(|p| (p, hs(p))).collect();
    svc.send(Event::SlotHashes { slot: s, bank_id: s, entries: entries(s - 1) });
    svc.send(Event::Status { slot: s, parent: Some(s - 1), bank_id: Some(s), status: Status::CreatedBank });
    svc.send(Event::Account { slot: s, bank_id: s, pubkey: [key; 32], write_version: s, meta: m(lamports) });
    svc.send(Event::SlotHashes { slot: s + 1, bank_id: s + 1, entries: entries(s) });
    svc.send(Event::Status { slot: s, parent: Some(s - 1), bank_id: Some(s), status: Status::Rooted });
}
fn rpc_call(sh: &Shared, body: &str) -> Value {
    rpc::handle_jsonrpc(sh, body)
}
fn proof(sh: &Shared, key: u8, at: Option<u64>) -> Value {
    let pk = bs58::encode([key; 32]).into_string();
    let p = match at {
        Some(s) => format!(r#"["{pk}",{{"slot":{s}}}]"#),
        None => format!(r#"["{pk}"]"#),
    };
    rpc_call(sh, &format!(r#"{{"id":1,"method":"getAccountProof","params":{p}}}"#))
}
fn code(v: &Value) -> Option<i64> {
    v["error"]["code"].as_i64()
}
/// GET /health përmes HTTP-së së vërtetë.
fn health(addr: std::net::SocketAddr) -> u16 {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.write_all(b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out.split_whitespace().nth(1).unwrap().parse().unwrap()
}
fn wait(mut f: impl FnMut() -> bool) -> bool {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(10) {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}
fn pages(dir: &std::path::Path) -> PageTree {
    PageTree::open(dir, PageOptions { durable: false, cache_depth: 0, history: 8, ..Default::default() }).unwrap()
}
/// Pemë në disk me slotet 1..=10 (nisur nga genesis, me gjurmë të saktë).
fn filled(dir: &std::path::Path) {
    let svc = Service::start_opts(Options { tree: Some(Box::new(pages(dir))), ..Default::default() }).unwrap();
    for s in 1..=10 {
        slot(&svc, s, s as u8, s * 10);
    }
    let sh = svc.shared.clone();
    drop(svc);
    assert_eq!(sh.state.read().unwrap().slot, 10);
}

/// P-08: gjendja e hapur nga disku s'shërbehet derisa zinxhiri ta konfirmojë.
#[test]
fn recovered_state_is_not_served_until_verified() {
    let d = tempfile::tempdir().unwrap();
    filled(d.path());
    let svc = Service::start_opts(Options { tree: Some(Box::new(pages(d.path()))), ..Default::default() }).unwrap();
    assert!(svc.resumed);
    let mut srv = rpc::serve_routes("127.0.0.1:0", svc.shared.clone(), rpc::Routes { jsonrpc: true, metrics: false, cors: false }).unwrap();
    let r = proof(&svc.shared, 3, None);
    assert_eq!(code(&r), Some(-32002), "gjendja e paverifikuar u shërbye: {r}");
    assert_eq!(health(srv.addr()), 503, "/health = 200 për gjendje të paverifikuar");
    // Një bankë e re që e përmban slotin 10 në SlotHashes e konfirmon.
    svc.send(Event::SlotHashes { slot: 11, bank_id: 11, entries: (0..=10).rev().map(|p| (p, hs(p))).collect() });
    assert!(wait(|| proof(&svc.shared, 3, None)["result"].is_object()), "pas verifikimit duhej të shërbente");
    assert_eq!(health(srv.addr()), 200);
    srv.stop();
}

/// P-09: gabimet e ruajtjes s'janë "parametra të pavlefshëm"; korrektësia s'garantohet më, ndaj
/// shërbimi ndalon.
#[test]
fn storage_error_is_classified_and_stops_serving() {
    let d = tempfile::tempdir().unwrap();
    filled(d.path());
    let t = pages(d.path());
    let svc = Service::start_opts(Options { tree: Some(Box::new(t)), ..Default::default() }).unwrap();
    let mut srv = rpc::serve_routes("127.0.0.1:0", svc.shared.clone(), rpc::Routes { jsonrpc: true, metrics: false, cors: false }).unwrap();
    svc.send(Event::SlotHashes { slot: 11, bank_id: 11, entries: (0..=10).rev().map(|p| (p, hs(p))).collect() });
    assert!(wait(|| proof(&svc.shared, 3, None)["result"].is_object()));
    // Disku "prishet": faqet ndryshojnë nën shërbimin.
    let p = d.path().join("pages.dat");
    let mut b = std::fs::read(&p).unwrap();
    for x in b.iter_mut() {
        *x ^= 0xff;
    }
    std::fs::write(&p, b).unwrap();
    let r = proof(&svc.shared, 3, None);
    assert!(r["error"].is_object(), "faqe e dëmtuar dha sukses: {r}");
    assert_ne!(code(&r), Some(-32602), "gabimi i ruajtjes u raportua si parametër i pavlefshëm: {r}");
    assert_eq!(health(srv.addr()), 503, "pas gabimit të ruajtjes /health mbeti 200");
    let r = rpc_call(&svc.shared, r#"{"id":1,"method":"getZiljaRoot"}"#);
    assert_eq!(code(&r), Some(-32003), "pas gabimit të ruajtjes getZiljaRoot vazhdoi: {r}");
    srv.stop();
}

/// P-09: "jashtë historisë" ka kodin e vet, jo "parametra të pavlefshëm".
#[test]
fn out_of_history_has_its_own_code() {
    let d = tempfile::tempdir().unwrap();
    let svc = Service::start_opts(Options { tree: Some(Box::new(pages(d.path()))), ..Default::default() }).unwrap();
    for s in 1..=30 {
        slot(&svc, s, 1, s);
    }
    assert!(wait(|| svc.shared.state.read().unwrap().slot == 30));
    let r = proof(&svc.shared, 1, Some(15));
    assert_eq!(code(&r), Some(-32007), "{r}");
    let r = rpc_call(&svc.shared, r#"{"id":1,"method":"getAccountProof","params":["xx"]}"#);
    assert_eq!(code(&r), Some(-32602), "{r}");
}

/// Pemë në RAM që bie (panic) në zbatimin e `n`-të: imiton një gabim të papritur në punëtor.
struct Panicky(MemTree, Arc<AtomicUsize>, usize);
impl StateTree for Panicky {
    fn root(&self) -> Hash {
        self.0.root()
    }
    fn len(&self) -> usize {
        self.0.len()
    }
    fn stage_snapshot(&mut self, s: u64, u: Vec<Update>) -> Result<(), String> {
        self.0.stage_snapshot(s, u)
    }
    fn finish_snapshot(&mut self, s: u64, c: bool) -> Result<Hash, String> {
        self.0.finish_snapshot(s, c)
    }
    fn apply_slot(&mut self, s: u64, u: Vec<Update>, f: Option<zilja_tree::Fingerprint>) -> Result<Hash, String> {
        if self.1.fetch_add(1, Ordering::SeqCst) + 1 == self.2 {
            panic!("gabim i papritur në pemë");
        }
        self.0.apply_slot(s, u, f)
    }
    fn prove_at(&self, k: &Key, at: Option<u64>) -> Result<ProofAt, zilja_tree::ProveError> {
        self.0.prove_at(k, at)
    }
    fn provable_range(&self) -> Option<(u64, u64)> {
        self.0.provable_range()
    }
    fn resume_point(&self) -> Option<Resume> {
        None
    }
}

/// P-10: kur punëtori bie, shërbimi s'vazhdon të shërbejë sukses dhe validatori s'bllokohet.
#[test]
fn worker_death_stops_serving() {
    let svc =
        Service::start_opts(Options { tree: Some(Box::new(Panicky(MemTree::new(), Arc::default(), 4))), ..Default::default() }).unwrap();
    let mut srv = rpc::serve_routes("127.0.0.1:0", svc.shared.clone(), rpc::Routes { jsonrpc: true, metrics: false, cors: false }).unwrap();
    for s in 1..=2 {
        slot(&svc, s, 1, s);
    }
    assert!(wait(|| health(srv.addr()) == 200));
    for s in 3..=6 {
        slot(&svc, s, 1, s);
    }
    assert!(wait(|| health(srv.addr()) == 503), "punëtori ra, /health mbeti 200");
    let r = rpc_call(&svc.shared, r#"{"id":1,"method":"getZiljaRoot"}"#);
    assert!(r["error"].is_object(), "punëtori ra, RPC-ja vazhdoi me sukses: {r}");
    // Validatori vazhdon të dërgojë: s'duhet të bllokohet as të bjerë.
    for s in 7..=2000 {
        slot(&svc, s, 1, s);
    }
    assert_eq!(svc.shared.metrics.halted.load(Ordering::Relaxed), true);
    srv.stop();
}

/// P-10: root-i i vjetër (asnjë rrënjosje përtej pragut) → /health 503.
#[test]
fn stale_root_makes_health_fail() {
    let svc = Service::start_opts(Options { max_root_age_secs: Some(1), ..Default::default() }).unwrap();
    let mut srv = rpc::serve_routes("127.0.0.1:0", svc.shared.clone(), rpc::Routes { jsonrpc: true, metrics: false, cors: false }).unwrap();
    slot(&svc, 1, 1, 1);
    assert!(wait(|| health(srv.addr()) == 200));
    std::thread::sleep(Duration::from_millis(2300));
    assert_eq!(health(srv.addr()), 503, "root-i 2 s i vjetër me prag 1 s");
    slot(&svc, 2, 1, 2);
    assert!(wait(|| health(srv.addr()) == 200));
    srv.stop();
}

/// Pemë në RAM që ndalet te `prepare_slot` i slotit `at` derisa testi ta lëshojë.
struct Gated(MemTree, u64, std::sync::Mutex<std::sync::mpsc::Receiver<()>>, Arc<std::sync::atomic::AtomicBool>);
impl StateTree for Gated {
    fn root(&self) -> Hash {
        self.0.root()
    }
    fn len(&self) -> usize {
        self.0.len()
    }
    fn stage_snapshot(&mut self, s: u64, u: Vec<Update>) -> Result<(), String> {
        self.0.stage_snapshot(s, u)
    }
    fn finish_snapshot(&mut self, s: u64, c: bool) -> Result<Hash, String> {
        self.0.finish_snapshot(s, c)
    }
    fn apply_slot(&mut self, s: u64, u: Vec<Update>, f: Option<zilja_tree::Fingerprint>) -> Result<Hash, String> {
        self.0.apply_slot(s, u, f)
    }
    fn prepare_slot(&self, s: u64, ups: Vec<Update>, f: Option<zilja_tree::Fingerprint>) -> Result<zilja_tree::Prepared, String> {
        if s == self.1 {
            self.3.store(true, Ordering::SeqCst);
            let _ = self.2.lock().unwrap().recv();
        }
        Ok(Box::new((ups, f)))
    }
    fn prove_at(&self, k: &Key, at: Option<u64>) -> Result<ProofAt, zilja_tree::ProveError> {
        self.0.prove_at(k, at)
    }
    fn provable_range(&self) -> Option<(u64, u64)> {
        self.0.provable_range()
    }
}

/// A07: ndalimi nga RPC-ja (një thread tjetër) ndalon edhe punën e pranuar në radhë. Pasi
/// `stop_serving` kthehet, sloti, root-et, firmat dhe faza s'ndryshojnë më.
#[test]
fn stop_from_rpc_freezes_queued_work() {
    let (go, gate) = std::sync::mpsc::channel();
    let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let tree = Gated(MemTree::new(), 3, std::sync::Mutex::new(gate), entered.clone());
    let svc = Service::start_opts(Options { tree: Some(Box::new(tree)), ..Default::default() }).unwrap();
    for s in 1..=2 {
        slot(&svc, s, 1, s);
    }
    assert!(wait(|| svc.shared.state.read().unwrap().slot == 2));
    // Sloti 3 ndalet te përgatitja; 4..=6 presin në radhë (të pranuara para ndalimit).
    for s in 3..=6 {
        slot(&svc, s, 1, s);
    }
    assert!(wait(|| entered.load(Ordering::SeqCst)));
    let sh = svc.shared.clone();
    let stopper = std::thread::spawn(move || zilja_node::service::stop_serving(&sh, "test: gabim ruajtjeje nga RPC".into()));
    // `stop_serving` pret kyçin; punëtori mban kyçin e leximit gjatë përgatitjes. E lëshojmë.
    std::thread::sleep(Duration::from_millis(50));
    go.send(()).unwrap();
    stopper.join().unwrap();
    let frozen = {
        let st = svc.shared.state.read().unwrap();
        (st.slot, st.roots.keys().copied().collect::<Vec<_>>())
    };
    // Punëtori mbaron radhën.
    let total = 2 * 5 + 4 * 5;
    assert!(wait(|| svc.shared.metrics.events_received.load(Ordering::Relaxed) >= total as u64 && svc.shared.metrics.queue_len.load(Ordering::Relaxed) == 0));
    std::thread::sleep(Duration::from_millis(100));
    let st = svc.shared.state.read().unwrap();
    assert_eq!((st.slot, st.roots.keys().copied().collect::<Vec<_>>()), frozen, "punë e pranuar u bë commit pas ndalimit");
    // Një commit para se `stop_serving` të kthehet është i ligjshëm (punëtori mund ta fitojë kyçin, edhe
    // për gjithë radhën): kontrata vlen nga kthimi i ndalimit, dhe vetëm ajo pohohet këtu.
    assert_eq!(svc.shared.metrics.phase.load(Ordering::Relaxed), zilja_node::service::PHASE_HALTED, "faza u rishkrua pas ndalimit");
    assert_eq!(svc.shared.metrics.ready.load(Ordering::Relaxed), 0, "gatishmëria u rishkrua pas ndalimit");
}

/// A17: një klient që dërgon një bajt kokash në çdo 300 ms s'kapërcen kurrë kohën e një operacioni
/// (5 s), por afati i plotë i kërkesës (5 s nga pranimi) e mbyll lidhjen. Pa të, punëtori zihej pa fund.
#[test]
fn slow_drip_client_is_cut_at_the_request_deadline() {
    let svc = Service::start_opts(Default::default()).unwrap();
    let mut srv = rpc::serve_routes("127.0.0.1:0", svc.shared.clone(), rpc::Routes { jsonrpc: true, metrics: false, cors: false }).unwrap();
    let mut s = std::net::TcpStream::connect(srv.addr()).unwrap();
    let t = Instant::now();
    let head = b"POST / HTTP/1.1\r\nHost: x\r\nX-Pad: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n";
    let mut closed_at = None;
    for b in head.iter().cycle().take(40) {
        if s.write_all(&[*b]).is_err() {
            closed_at = Some(t.elapsed());
            break;
        }
        // Serveri mbyll: leximi kthen 0 (ose 503) pa pritur.
        s.set_read_timeout(Some(Duration::from_millis(1))).unwrap();
        let mut x = [0u8; 64];
        if let Ok(n) = s.read(&mut x) {
            if n == 0 || std::str::from_utf8(&x[..n]).is_ok() {
                closed_at = Some(t.elapsed());
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    let at = closed_at.unwrap_or_else(|| panic!("lidhja e ngadaltë mbeti hapur {:?} (> afati {:?})", t.elapsed(), rpc::REQUEST_DEADLINE));
    assert!(at <= rpc::REQUEST_DEADLINE + Duration::from_millis(1500), "u mbyll vonë: {at:?}");
    srv.stop();
}

/// A19: kontrata e historisë: poshtë kujtesës -32007, mbi majë -32008 (jo sloti i fundit), `accounts`
/// vetëm për slotin aktual, dhe parametrat e tepërt -32602.
#[test]
fn history_contract_matches_the_spec() {
    let svc = Service::start_opts(Default::default()).unwrap();
    for s in 1..=5 {
        slot(&svc, s, 1, s);
    }
    assert!(wait(|| svc.shared.state.read().unwrap().slot == 5));
    let r = proof(&svc.shared, 1, Some(9));
    assert_eq!(code(&r), Some(-32008), "slot mbi majë u përgjigj si i fundit: {r}");
    let r = rpc_call(&svc.shared, r#"{"id":1,"method":"getZiljaRoot","params":[9]}"#);
    assert_eq!(code(&r), Some(-32008), "{r}");
    let cur = rpc_call(&svc.shared, r#"{"id":1,"method":"getZiljaRoot"}"#);
    assert!(cur["result"]["accounts"].is_u64(), "{cur}");
    let old = rpc_call(&svc.shared, r#"{"id":1,"method":"getZiljaRoot","params":[3]}"#);
    assert_eq!(old["result"]["slot"], 3, "{old}");
    assert!(old["result"].get("accounts").is_none(), "accounts i pemës aktuale te një root historik: {old}");
    for m in ["getZiljaRoot", "getZiljaAttestation"] {
        let r = rpc_call(&svc.shared, &format!(r#"{{"id":1,"method":"{m}","params":[3,4]}}"#));
        assert_eq!(code(&r), Some(-32602), "{m} pranoi parametra të tepërt: {r}");
    }
}
