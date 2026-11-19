//! R5-3: mbledhës i **pabesuar** i firmave (Faza A).
//!
//! Sot klienti pyet çdo zile për `getZiljaAttestation`. Me qindra validatorë kjo s'bëhet nga një
//! telefon. Aggregator-i i pyet ai zilet, i mban në kujtesë për slot, dhe i jep klientit të gjitha
//! me një thirrje: `getZiljaQuorumBundle [slot]`.
//!
//! Aggregator-it s'i besohet asgjë: ai vetëm përcjell firma Ed25519 dhe revokime, që klienti i
//! verifikon vetë (genesis, epoka, autorizimi, revokimet, stake-u). Më e keqja që mund të bëjë është
//! të fshehë firma (klienti s'arrin kuorum dhe bie te zilet e veta), jo të krijojë një kuorum të rremë.
//! Nesër, me BLS, i njëjti shërbim mund t'i bashkojë në një firmë të vetme.
//!
//! Kujdes ndaj kostos: çdo slot i ri kërkon një pyetje te çdo zile. Prandaj sloti duhet të jetë afër
//! majës që njeh aggregator-i, dhe numri i sloteve të reja në sekondë është i kufizuar.

use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zilja_node::rpc::HttpHandler;

/// Sa slote mbahen në kujtesë.
pub const CACHE_SLOTS: usize = 512;
/// Sa slote të reja (fan-out te të gjitha zilet) lejohen në sekondë.
pub const NEW_SLOTS_PER_SEC: u32 = 4;
/// Pas sa kohe pyeten sërish zilet që s'u përgjigjën (p.sh. s'e kishin firmosur ende slotin).
pub const RETRY_AFTER: Duration = Duration::from_secs(2);
/// Sa shpesh rifreskohen revokimet.
pub const REVOCATIONS_TTL: Duration = Duration::from_secs(30);
/// Sa thirrje njëkohësisht drejt zileve.
pub const FANOUT: usize = 32;
/// Sa larg nga sloti më i lartë i njohur mund të jetë një kërkesë (poshtë; lart lejohen 4).
pub const MAX_BEHIND: u64 = 4096;

/// Si i pyet zilet (e zëvendësueshme në teste).
pub trait Fetch: Send + Sync + 'static {
    fn call(&self, url: &str, method: &str, params: Value) -> Result<Value, String>;
}

/// HTTP i vërtetë, me afat të shkurtër (një zile e ngadaltë s'e mban bundle-in).
pub struct HttpFetch;

impl Fetch for HttpFetch {
    fn call(&self, url: &str, method: &str, params: Value) -> Result<Value, String> {
        zilja_cli::rpc_timeout(url, method, params, Duration::from_secs(3))
    }
}

struct Entry {
    /// zile → firma (objekti JSON siç e dha zilja, pas kontrollit të formës).
    got: HashMap<String, Value>,
    missing: Vec<String>,
    fetched: Instant,
}

struct Inner {
    cache: BTreeMap<u64, Arc<Mutex<Entry>>>,
    highest: u64,
    window_start: Instant,
    new_in_window: u32,
    revocations: Vec<Value>,
    revocations_at: Option<Instant>,
}

pub struct Aggregator<F: Fetch = HttpFetch> {
    ziljas: Vec<String>,
    fetch: F,
    inner: Mutex<Inner>,
    pub served: std::sync::atomic::AtomicU64,
}

impl<F: Fetch> Aggregator<F> {
    pub fn new(ziljas: Vec<String>, fetch: F) -> Self {
        Aggregator {
            ziljas,
            fetch,
            inner: Mutex::new(Inner {
                cache: BTreeMap::new(),
                highest: 0,
                window_start: Instant::now(),
                new_in_window: 0,
                revocations: Vec::new(),
                revocations_at: None,
            }),
            served: Default::default(),
        }
    }

    /// Numri i zileve (për log-un e nisjes).
    pub fn metrics_line(&self) -> String {
        self.ziljas.len().to_string()
    }

    /// Pyet `urls` paralelisht (më së shumti `FANOUT` njëkohësisht).
    fn fan_out(&self, urls: &[String], method: &str, params: &Value) -> Vec<(String, Result<Value, String>)> {
        let mut out = Vec::with_capacity(urls.len());
        for chunk in urls.chunks(FANOUT) {
            std::thread::scope(|s| {
                let hs: Vec<_> = chunk.iter().map(|u| s.spawn(move || (u.clone(), self.fetch.call(u, method, params.clone())))).collect();
                for h in hs {
                    if let Ok(r) = h.join() {
                        out.push(r);
                    }
                }
            });
        }
        out
    }

    /// Firma e një zileje, e kontrolluar në formë (jo në vlefshmëri: këtë e bën klienti).
    fn shape_ok(v: &Value, slot: u64) -> bool {
        zilja_cli::parse_attestation(v, slot).is_ok()
    }

    /// Sloti më i lartë që njohin zilet (nga firmat e fundit), për kufirin e kërkesave.
    pub fn learn_tip(&self) {
        let got = self.fan_out(&self.ziljas, "getZiljaRoot", &json!([]));
        let tip = got.iter().filter_map(|(_, r)| r.as_ref().ok()?.get("slot")?.as_u64()).max();
        if let Some(t) = tip {
            let mut g = self.inner.lock().unwrap();
            g.highest = g.highest.max(t);
        }
    }

    fn revocations(&self) -> Vec<Value> {
        {
            let g = self.inner.lock().unwrap();
            if g.revocations_at.is_some_and(|t| t.elapsed() < REVOCATIONS_TTL) {
                return g.revocations.clone();
            }
        }
        let mut all: Vec<Value> = Vec::new();
        for (_, r) in self.fan_out(&self.ziljas, "getZiljaRevocations", &json!([])) {
            if let Ok(Value::Array(list)) = r {
                for x in list {
                    if zilja_cli::parse_revocation(&x).is_ok() && !all.contains(&x) {
                        all.push(x);
                    }
                }
            }
        }
        let mut g = self.inner.lock().unwrap();
        g.revocations = all.clone();
        g.revocations_at = Some(Instant::now());
        all
    }

    /// `getZiljaQuorumBundle [slot]`.
    pub fn bundle(&self, slot: u64) -> Result<Value, (i64, String)> {
        let entry = {
            let mut g = self.inner.lock().unwrap();
            if g.highest > 0 && (slot + MAX_BEHIND < g.highest || slot > g.highest + 4) {
                return Err((-32007, format!("sloti {slot} është larg majës së njohur ({})", g.highest)));
            }
            match g.cache.get(&slot) {
                Some(e) => e.clone(),
                None => {
                    if g.window_start.elapsed() >= Duration::from_secs(1) {
                        g.window_start = Instant::now();
                        g.new_in_window = 0;
                    }
                    if g.new_in_window >= NEW_SLOTS_PER_SEC {
                        return Err((-32005, "shumë slote të reja në sekondë; provo pas pak".into()));
                    }
                    g.new_in_window += 1;
                    let e = Arc::new(Mutex::new(Entry { got: HashMap::new(), missing: self.ziljas.clone(), fetched: Instant::now() - RETRY_AFTER * 2 }));
                    g.cache.insert(slot, e.clone());
                    while g.cache.len() > CACHE_SLOTS {
                        g.cache.pop_first();
                    }
                    e
                }
            }
        };
        // Një kërkesë njëkohësisht për slot (të tjerat presin dhe marrin të njëjtën përgjigje).
        let mut e = entry.lock().unwrap();
        if !e.missing.is_empty() && e.fetched.elapsed() >= RETRY_AFTER {
            let ask = std::mem::take(&mut e.missing);
            for (url, r) in self.fan_out(&ask, "getZiljaAttestation", &json!([slot])) {
                match r {
                    Ok(v) if Self::shape_ok(&v, slot) => {
                        e.got.insert(url, v);
                    }
                    _ => e.missing.push(url),
                }
            }
            e.fetched = Instant::now();
            let mut g = self.inner.lock().unwrap();
            g.highest = g.highest.max(slot);
        }
        let mut atts: Vec<&Value> = e.got.values().collect();
        // Radhë e qëndrueshme (s'ka rëndësi për klientin, por e bën përgjigjen të krahasueshme).
        atts.sort_by_key(|v| v.to_string());
        let res = json!({
            "slot": slot,
            "attestations": atts,
            "revocations": self.revocations(),
            "ziljas": self.ziljas.len(),
            "answered": e.got.len(),
        });
        self.served.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(res)
    }
}

fn err(id: Value, code: i64, msg: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}})
}

impl<F: Fetch> HttpHandler for Aggregator<F> {
    fn jsonrpc(&self, body: &str) -> Value {
        let Ok(req) = serde_json::from_str::<Value>(body) else {
            return err(Value::Null, -32700, "JSON i pavlefshëm");
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let params = req.get("params").cloned().unwrap_or(json!([]));
        match req.get("method").and_then(Value::as_str).unwrap_or("") {
            "getZiljaQuorumBundle" => match params.as_array().map(|a| a.as_slice()) {
                Some([s]) => match s.as_u64() {
                    Some(slot) => match self.bundle(slot) {
                        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
                        Err((c, m)) => err(id, c, &m),
                    },
                    None => err(id, -32602, "slot duhet numër i plotë jo negativ"),
                },
                _ => err(id, -32602, "duhet params: [slot]"),
            },
            _ => err(id, -32601, "metodë e panjohur"),
        }
    }
    fn health(&self) -> (u16, &'static str) {
        (200, "ok")
    }
    fn metrics(&self) -> String {
        let g = self.inner.lock().unwrap();
        format!(
            "zilja_aggregator_ziljas {}\nzilja_aggregator_cached_slots {}\nzilja_aggregator_highest_slot {}\nzilja_aggregator_bundles_served_total {}\n",
            self.ziljas.len(),
            g.cache.len(),
            g.highest,
            self.served.load(std::sync::atomic::Ordering::Relaxed)
        )
    }
}
