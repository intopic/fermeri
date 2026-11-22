//! Faza 5 e `zilja-snapshot-bench` (R9): dëftesa nga shumë klientë njëkohësisht.
//!
//! Pema e ndërtuar nga snapshot-i i jepet serverit të vërtetë HTTP të zilës (`zilja_node::rpc`,
//! me 8 punëtorët, afatin 5 s dhe `Connection: close`, siç punon në prodhim). Ndërkohë një thread
//! zbaton slote çdo `slot_ms` me `slot_updates` ndryshime, me të njëjtën rrugë si plugin-i
//! (`prepare_slot` nën kyçin e leximit, `commit_slot` nën kyçin e shkrimit), që të matet edhe
//! ndikimi i kërkesave te slotet dhe anasjelltas.
//!
//! Për çdo numër klientësh: kërkesa/s, dëftesa/s, vonesa sipas llojit, gabimet (503 "busy",
//! afati, gabime RPC), dhe slotet nën ngarkesë. Një nga `verify_every` përgjigje verifikohet
//! plotësisht (dëftesa kundër root-it dhe root-i kundër atij që zilja llogariti për atë slot); një
//! përgjigje e pavlefshme e bën bench-in të dështojë.
//!
//! Përzierja e kërkesave (fikse, që rezultatet të krahasohen): 60% `getAccountProof` për llogari
//! ekzistuese, 10% për llogari që s'ekzistojnë, 15% dëftesa historike (një nga 16 slotet e fundit),
//! 15% `getMultipleAccountProofs` me `multi_keys` llogari.
//!
//! `one_ip`: një matje shtesë me kufirin e prodhimit për një IP (2 lidhje njëkohësisht), si një
//! klient i madh pas një adrese të vetme. Klientët dhe serveri ndajnë të njëjtën makinë (CPU).

use rand::{rngs::StdRng, Rng, SeedableRng};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zilja_node::rpc::{self, HttpLimits, Routes};
use zilja_node::Shared;
use zilja_tree::{StateTree, Update};
use zilja_verifier::multi::{verify_accounts, MultiProof};
use zilja_verifier::{tree_key, verify_tree_key, AccountMeta, Hash, Key, Proof, Verdict};

use super::{cpu_secs, drop_os_cache, proc_io, rand_meta, stats, Cold};

pub struct RpcOpts {
    pub clients: Vec<usize>,
    pub seconds: u64,
    pub slot_ms: u64,
    pub slot_updates: usize,
    pub verify_every: u64,
    pub multi_keys: usize,
    pub one_ip: bool,
    /// Grupimi i sloteve (SPEC §3): pema përditësohet vetëm te slotet `slot % K == 0`, me
    /// ndryshimet e K sloteve të bashkuara (versioni i fundit i çdo llogarie). 1 = çdo slot.
    pub root_every: u64,
}

const KINDS: [&str; 4] = ["current", "missing", "historic", "multi"];
/// Sa slote prapa pyesin dëftesat historike (pema e bench-it mban `history: 32`). Gjysma e
/// historikut dhe 2 slote larg skajit: një slot i ri mund ta shtyjë skajin ndërsa kërkesa është
/// në rrugë, dhe atëherë "jashtë historikut" është përgjigja e saktë, jo ajo që matim këtu.
const HIST: u64 = 16;
const EDGE: u64 = 2;

#[derive(Default)]
struct ClientOut {
    lat: [Vec<f64>; 4],
    bytes: Vec<f64>,
    ok: [u64; 4],
    busy: u64,
    timeouts: u64,
    io_errors: u64,
    no_ports: u64,
    /// Lidhje që serveri i mbylli pa përgjigje (p.sh. kufiri për IP): reset/pipe.
    refused: u64,
    rpc_errors: BTreeMap<i64, u64>,
    verified: u64,
    invalid: Vec<String>,
}

/// Një kërkesë HTTP, si klientët e zakonshëm: lidhje e re, `Connection: close`.
fn post(addr: &str, body: &str) -> std::io::Result<(u16, Vec<u8>)> {
    let mut s = std::net::TcpStream::connect(addr)?;
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    s.set_write_timeout(Some(Duration::from_secs(10)))?;
    let req = format!(
        "POST / HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes())?;
    let mut out = Vec::new();
    s.read_to_end(&mut out)?;
    let head_end = out.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| std::io::Error::other("përgjigje pa koka"))?;
    let code = std::str::from_utf8(&out[..head_end])
        .ok()
        .and_then(|h| h.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| std::io::Error::other("rresht statusi i pavlefshëm"))?;
    Ok((code, out[head_end + 4..].to_vec()))
}

fn h32(v: &Value) -> Result<[u8; 32], String> {
    hex::decode(v.as_str().ok_or("pritej hex")?).map_err(|e| e.to_string())?.try_into().map_err(|_| "pritej 32 bajt".to_string())
}

fn meta_of(v: &Value) -> Result<Option<AccountMeta>, String> {
    if v.is_null() {
        return Ok(None);
    }
    let num = |k: &str| v[k].as_str().and_then(|s| s.parse::<u64>().ok()).ok_or(format!("{k} mungon"));
    let owner: [u8; 32] = bs58::decode(v["owner"].as_str().ok_or("owner mungon")?)
        .into_vec()
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|_| "owner duhet 32 bajt".to_string())?;
    Ok(Some(AccountMeta {
        lamports: num("lamports")?,
        owner,
        executable: v["executable"].as_bool().ok_or("executable mungon")?,
        data_len: num("dataLen")?,
        data_hash: h32(&v["dataHash"])?,
    }))
}

/// Verifikon përgjigjen si një klient i kujdesshëm: dëftesa kundër root-it të përgjigjes, dhe
/// root-i kundër atij që zilja llogariti për atë slot.
fn verify(res: &Value, pks: &[Key], multi: bool, shared: &Shared) -> Result<(), String> {
    let slot = res["slot"].as_u64().ok_or("slot mungon")?;
    let root: Hash = h32(&res["root"])?;
    let bytes = hex::decode(res["proof"].as_str().ok_or("proof mungon")?).map_err(|e| e.to_string())?;
    if multi {
        let mp = MultiProof::from_bytes(&bytes).ok_or("multi-proof i pavlefshëm")?;
        let accs = res["accounts"].as_array().ok_or("accounts mungon")?;
        if accs.len() != pks.len() {
            return Err("numër i gabuar llogarish".into());
        }
        let metas: Vec<Option<AccountMeta>> = accs.iter().map(|a| meta_of(&a["account"])).collect::<Result<_, _>>()?;
        // `verify_accounts` merr pubkey-t dhe llogarit vetë `tree_key`.
        let claims: Vec<(Key, Option<&AccountMeta>)> = pks.iter().zip(&metas).map(|(pk, m)| (*pk, m.as_ref())).collect();
        if verify_accounts(&root, &claims, &mp).contains(&Verdict::Invalid) {
            return Err(format!("multi-proof NUK verifikohet në slotin {slot}"));
        }
    } else {
        let proof = Proof::from_bytes(&bytes).map_err(|e| format!("{e:?}"))?;
        let meta = meta_of(&res["account"])?;
        if verify_tree_key(&root, &tree_key(&pks[0]), meta.as_ref(), &proof) == Verdict::Invalid {
            return Err(format!("dëftesa NUK verifikohet në slotin {slot}"));
        }
    }
    match shared.state.read().unwrap().roots.get(&slot) {
        Some(r) if *r != root => Err(format!("root-i i përgjigjes ≠ root-i i zilës për slotin {slot}")),
        _ => Ok(()),
    }
}

#[allow(clippy::too_many_arguments)]
fn client(i: usize, addr: String, end: Instant, pubs: Arc<Vec<Key>>, cur: Arc<AtomicU64>, lo: Arc<AtomicU64>, o: (u64, usize, u64), shared: Shared) -> ClientOut {
    let (verify_every, multi_keys, seed) = o;
    let mut rng = StdRng::seed_from_u64(seed ^ (0x9e37_79b9_7f4a_7c15u64.wrapping_mul(i as u64 + 1)));
    let mut out = ClientOut::default();
    let mut n = 0u64;
    let pick = |rng: &mut StdRng| -> Key { if pubs.is_empty() { rng.gen() } else { pubs[rng.gen_range(0..pubs.len())] } };
    while Instant::now() < end {
        n += 1;
        let r: f64 = rng.gen();
        let kind = if r < 0.60 { 0 } else if r < 0.70 { 1 } else if r < 0.85 { 2 } else { 3 };
        let (body, pks) = match kind {
            0 | 1 => {
                let pk: Key = if kind == 0 { pick(&mut rng) } else { rng.gen() };
                (json!({"jsonrpc":"2.0","id":n,"method":"getAccountProof","params":[bs58::encode(pk).into_string()]}), vec![pk])
            }
            2 => {
                let c = cur.load(Ordering::Relaxed);
                let s = rng.gen_range((lo.load(Ordering::Relaxed) + EDGE).max(c.saturating_sub(HIST)).min(c)..=c);
                let pk = pick(&mut rng);
                (json!({"jsonrpc":"2.0","id":n,"method":"getAccountProof","params":[bs58::encode(pk).into_string(), {"slot": s}]}), vec![pk])
            }
            _ => {
                let pks: Vec<Key> = (0..multi_keys).map(|_| pick(&mut rng)).collect();
                let list: Vec<String> = pks.iter().map(|k| bs58::encode(k).into_string()).collect();
                (json!({"jsonrpc":"2.0","id":n,"method":"getMultipleAccountProofs","params":[list]}), pks)
            }
        };
        let body = body.to_string();
        let t = Instant::now();
        match post(&addr, &body) {
            Ok((200, b)) => {
                let ms = t.elapsed().as_secs_f64() * 1e3;
                let v: Value = match serde_json::from_slice(&b) {
                    Ok(v) => v,
                    Err(_) => {
                        out.io_errors += 1;
                        continue;
                    }
                };
                if let Some(code) = v["error"]["code"].as_i64() {
                    *out.rpc_errors.entry(code).or_default() += 1;
                    continue;
                }
                out.lat[kind].push(ms);
                out.bytes.push(b.len() as f64);
                out.ok[kind] += 1;
                if verify_every > 0 && n.is_multiple_of(verify_every) {
                    out.verified += 1;
                    if let Err(e) = verify(&v["result"], &pks, kind == 3, &shared) {
                        out.invalid.push(e);
                    }
                }
            }
            Ok((503, b)) => {
                if b.starts_with(b"timeout") {
                    out.timeouts += 1
                } else {
                    out.busy += 1
                }
            }
            Ok(_) => out.io_errors += 1,
            Err(e) if e.kind() == std::io::ErrorKind::AddrNotAvailable => {
                // Klienti mbaroi portat e përkohshme (TIME_WAIT): problem i makinës së testit, jo i zilës.
                out.no_ports += 1;
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => out.timeouts += 1,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionAborted) => out.refused += 1,
            Err(_) => out.io_errors += 1,
        }
    }
    out
}

/// Slotet gjatë ngarkesës: (total, pritja për kyçin e shkrimit, mbajtja e kyçit, prepare) ms.
type WriterLog = Arc<Mutex<(Vec<[f64; 4]>, u64)>>;

#[allow(clippy::too_many_arguments)]
pub fn run(
    mut tree: Box<dyn StateTree>,
    complete: bool,
    slot: u64,
    root: Hash,
    pubs: Vec<Key>,
    mut sample: Vec<Key>,
    o: &RpcOpts,
    seed: u64,
    cold: Cold,
    pages: &Path,
) -> Result<Value, String> {
    let shared = Shared::default();
    // Si në shërbim: checkpoint-i pas commit-it, nën kyçin e leximit.
    tree.set_background_maintenance(true);
    {
        let mut st = shared.state.write().unwrap();
        st.tree = tree;
        st.slot = slot;
        st.ready = true;
        st.complete = complete;
        st.roots.insert(slot, root);
    }
    let cur = Arc::new(AtomicU64::new(slot));
    // Slotet historike që zilja i njeh: ka root (`st.roots`) dhe pema i mban ende.
    let lo = Arc::new(AtomicU64::new(slot));
    let stop = Arc::new(AtomicBool::new(false));
    let wlog: WriterLog = Arc::default();
    let werr: Arc<Mutex<Option<String>>> = Arc::default();

    // Shkruesi: slote me ritmin e Mainnet-it, me rrugën e plugin-it (Faster-1).
    let writer = {
        let (shared, cur, lo, stop, wlog, werr) = (shared.clone(), cur.clone(), lo.clone(), stop.clone(), wlog.clone(), werr.clone());
        let k = o.root_every.max(1);
        let (n, every, pages) = (o.slot_updates, Duration::from_millis(o.slot_ms.max(1) * k), pages.to_path_buf());
        let mut rng = StdRng::seed_from_u64(seed ^ 0x5107);
        std::thread::spawn(move || {
            while n > 0 && !stop.load(Ordering::Relaxed) {
                let t0 = Instant::now();
                // Kufiri i ardhshëm i dritares: K slote me nga `n` ndryshime, një commit.
                let s = (cur.load(Ordering::Relaxed) / k + 1) * k;
                let mut ups: Vec<Update> = Vec::with_capacity(n * k as usize);
                for _ in 0..n * k as usize {
                    let r: f64 = rng.gen();
                    if r < 0.75 && !sample.is_empty() {
                        ups.push((sample[rng.gen_range(0..sample.len())], Some(rand_meta(&mut rng))));
                    } else if r < 0.90 || sample.len() < 2 {
                        let k: Key = rng.gen();
                        sample.push(k);
                        ups.push((k, Some(rand_meta(&mut rng))));
                    } else {
                        let i = rng.gen_range(0..sample.len());
                        ups.push((sample.swap_remove(i), None));
                    }
                }
                if k > 1 {
                    // Si tracker-i: për çdo llogari mbetet versioni i fundit i dritares.
                    let mut last: HashMap<Key, usize> = HashMap::with_capacity(ups.len());
                    for (i, u) in ups.iter().enumerate() {
                        last.insert(u.0, i);
                    }
                    ups = ups.into_iter().enumerate().filter(|(i, u)| last[&u.0] == *i).map(|(_, u)| u).collect();
                }
                if cold == Cold::Slot {
                    if let Err(e) = drop_os_cache(&pages) {
                        *werr.lock().unwrap() = Some(e);
                        break;
                    }
                }
                let tp = Instant::now();
                let p = shared.state.read().unwrap().tree.prepare_slot(s, ups, None);
                let prep = tp.elapsed();
                let tw = Instant::now();
                let mut st = shared.state.write().unwrap();
                let wait = tw.elapsed();
                let th = Instant::now();
                match p.and_then(|p| st.tree.commit_slot(s, p)) {
                    Ok(r) => {
                        st.slot = s;
                        st.roots.insert(s, r);
                        while st.roots.len() > zilja_node::service::ROOT_HISTORY {
                            st.roots.pop_first();
                        }
                        let first_root = st.roots.keys().next().copied().unwrap_or(s);
                        let l = st.tree.provable_range().map(|x| x.0).unwrap_or(s).max(first_root);
                        drop(st);
                        lo.store(l, Ordering::Relaxed);
                        cur.store(s, Ordering::Relaxed);
                        let mres = shared.state.read().unwrap().tree.maintenance();
                        if let Err(e) = mres {
                            *werr.lock().unwrap() = Some(format!("checkpoint pas slotit {s}: {e}"));
                            break;
                        }
                    }
                    Err(e) => {
                        *werr.lock().unwrap() = Some(format!("slot {s}: {e}"));
                        break;
                    }
                }
                let hold = th.elapsed();
                let el = t0.elapsed();
                let ms = |d: Duration| d.as_secs_f64() * 1e3;
                let mut g = wlog.lock().unwrap();
                g.0.push([ms(el), ms(wait), ms(hold), ms(prep)]);
                if el < every {
                    drop(g);
                    std::thread::sleep(every - el);
                } else {
                    g.1 += 1; // sloti zgjati më shumë se ritmi: zilja mbetet prapa
                }
            }
        })
    };

    let pubs = Arc::new(pubs);
    let mut levels = Vec::new();
    let mut plan: Vec<(usize, bool)> = o.clients.iter().map(|&c| (c, false)).collect();
    if o.one_ip {
        if let Some(&m) = o.clients.iter().max() {
            plan.push((m, true));
        }
    }
    let mut invalid_all = Vec::new();
    for (clients, one_ip) in plan {
        if let Some(e) = werr.lock().unwrap().clone() {
            return Err(format!("shkruesi i sloteve: {e}"));
        }
        let limits = if one_ip { HttpLimits { exempt_loopback: false, ..Default::default() } } else { HttpLimits::default() };
        let server = rpc::serve_routes_with("127.0.0.1:0", shared.clone(), Routes { jsonrpc: true, metrics: false, cors: false }, limits).map_err(|e| format!("serveri: {e}"))?;
        let addr = server.addr().to_string();
        if cold != Cold::None {
            drop_os_cache(pages)?;
        }
        *wlog.lock().unwrap() = Default::default();
        let (c0, io0, t0) = (cpu_secs(), proc_io(), Instant::now());
        let end = t0 + Duration::from_secs(o.seconds);
        let hs: Vec<_> = (0..clients)
            .map(|i| {
                let (addr, pubs, cur, lo, shared) = (addr.clone(), pubs.clone(), cur.clone(), lo.clone(), shared.clone());
                let oo = (o.verify_every, o.multi_keys, seed ^ clients as u64);
                std::thread::spawn(move || client(i, addr, end, pubs, cur, lo, oo, shared))
            })
            .collect();
        let mut tot = ClientOut::default();
        for h in hs {
            let mut c = h.join().map_err(|_| "klienti u ndal papritur")?;
            for k in 0..4 {
                tot.lat[k].append(&mut c.lat[k]);
                tot.ok[k] += c.ok[k];
            }
            tot.bytes.extend(c.bytes);
            tot.busy += c.busy;
            tot.timeouts += c.timeouts;
            tot.io_errors += c.io_errors;
            tot.no_ports += c.no_ports;
            tot.refused += c.refused;
            for (k, v) in c.rpc_errors {
                *tot.rpc_errors.entry(k).or_default() += v;
            }
            tot.verified += c.verified;
            tot.invalid.extend(c.invalid);
        }
        let dt = t0.elapsed().as_secs_f64().max(1e-9);
        let (c1, io1) = (cpu_secs(), proc_io());
        drop(server);
        let (wl, late) = std::mem::take(&mut *wlog.lock().unwrap());
        let col = |j: usize| wl.iter().map(|r| r[j]).collect::<Vec<f64>>();
        let ok: u64 = tot.ok.iter().sum();
        let errs = tot.busy + tot.timeouts + tot.io_errors + tot.no_ports + tot.refused + tot.rpc_errors.values().sum::<u64>();
        let proofs = tot.ok[0] + tot.ok[1] + tot.ok[2] + tot.ok[3] * o.multi_keys as u64;
        let all: Vec<f64> = tot.lat.iter().flatten().copied().collect();
        let mut by_kind = serde_json::Map::new();
        for (k, name) in KINDS.iter().enumerate() {
            by_kind.insert(name.to_string(), stats(std::mem::take(&mut tot.lat[k])));
        }
        let lat = stats(all);
        let w_total = stats(col(0));
        eprintln!(
            "rpc {clients} klientë{}: {:.0} kërkesa/s, {:.0} dëftesa/s, p50 {:.2} ms, p99 {:.2} ms | gabime {:.2}% (busy {}, të refuzuara {}, afati {}, io {}, porta {}, RPC {:?}) | slote {} (vonë {late}), p99 {:.1} ms | verifikuar {} (të pavlefshme {})",
            if one_ip { " nga NJË IP" } else { "" },
            ok as f64 / dt,
            proofs as f64 / dt,
            lat["p50"].as_f64().unwrap_or(0.0),
            lat["p99"].as_f64().unwrap_or(0.0),
            100.0 * errs as f64 / (ok + errs).max(1) as f64,
            tot.busy,
            tot.refused,
            tot.timeouts,
            tot.io_errors,
            tot.no_ports,
            tot.rpc_errors,
            wl.len(),
            w_total["p99"].as_f64().unwrap_or(0.0),
            tot.verified,
            tot.invalid.len()
        );
        levels.push(json!({
            "clients": clients, "one_ip": one_ip, "seconds": dt,
            "requests_ok": ok, "requests_per_second": ok as f64 / dt, "proofs_per_second": proofs as f64 / dt,
            "error_pct": 100.0 * errs as f64 / (ok + errs).max(1) as f64,
            "busy_503": tot.busy, "refused": tot.refused, "timeouts": tot.timeouts, "io_errors": tot.io_errors, "client_out_of_ports": tot.no_ports,
            "rpc_errors": tot.rpc_errors.iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<serde_json::Map<_, _>>(),
            "latency_ms": lat, "by_kind": Value::Object(by_kind), "response_bytes": stats(tot.bytes),
            "verified": tot.verified, "invalid": tot.invalid.len(),
            "writer": {"slots": wl.len(), "late": late, "total_ms": w_total, "lock_wait_ms": stats(col(1)), "write_hold_ms": stats(col(2)), "prepare_ms": stats(col(3))},
            "cpu_pct": 100.0 * (c1 - c0) / dt, "device_read_mb_s": io1.0.saturating_sub(io0.0) as f64 / 1e6 / dt,
        }));
        invalid_all.extend(tot.invalid);
    }
    stop.store(true, Ordering::Relaxed);
    let _ = writer.join();
    if let Some(e) = werr.lock().unwrap().clone() {
        return Err(format!("shkruesi i sloteve: {e}"));
    }
    let out = json!({
        "mix": {"current": 0.60, "missing": 0.10, "historic": 0.15, "multi": 0.15, "multi_keys": o.multi_keys},
        "slot_ms": o.slot_ms, "slot_updates": o.slot_updates, "root_every": o.root_every, "server_workers": 8, "keep_alive": false,
        "levels": levels,
    });
    if !invalid_all.is_empty() {
        return Err(format!("{} përgjigje RPC NUK u verifikuan, p.sh.: {}", invalid_all.len(), invalid_all[0]));
    }
    Ok(out)
}
