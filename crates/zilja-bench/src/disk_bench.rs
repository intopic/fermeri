//! Matjet e pemës në disk, për të dy motorët.
//!
//! `cargo run -p zilja-bench --release --bin zilja-disk-bench -- [llogari] [slote] [ndryshime_per_slot] [dir] [history]`
//!
//! Motori: skedari i faqeve (pages), i vetmi që nga R6.
//! Çelësat nuk mbahen në RAM: çelësi `i` rillogaritet nga numri `i`, ndaj bench-i shkon deri në
//! miliarda llogari me pak memorie.

use rand::{rngs::StdRng, Rng, SeedableRng};
use serde_json::json;
use std::sync::atomic::Ordering;
use std::time::Instant;
use zilja_store::{PageOptions, PageTree};
use zilja_tree::StateTree;
use zilja_verifier::{verify_tree_key, AccountMeta, Key, Verdict};

fn rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("VmRSS:")).map(|l| l.to_string()))
        .and_then(|l| l.split_whitespace().nth(1).and_then(|v| v.parse::<f64>().ok()))
        .map(|kb| kb / 1024.0)
        .unwrap_or(0.0)
}

fn dir_mb(p: &std::path::Path) -> f64 {
    fn walk(p: &std::path::Path) -> u64 {
        std::fs::read_dir(p)
            .map(|rd| {
                rd.flatten()
                    .map(|e| {
                        let m = e.metadata().unwrap();
                        if m.is_dir() {
                            walk(&e.path())
                        } else {
                            m.len()
                        }
                    })
                    .sum()
            })
            .unwrap_or(0)
    }
    walk(p) as f64 / (1 << 20) as f64
}

/// Bajtet që procesi ka dërguar realisht në disk / lexuar prej tij (`/proc/self/io`).
fn proc_io() -> (u64, u64) {
    let s = std::fs::read_to_string("/proc/self/io").unwrap_or_default();
    let get = |k: &str| s.lines().find(|l| l.starts_with(k)).and_then(|l| l.split_whitespace().nth(1)?.parse().ok()).unwrap_or(0);
    (get("read_bytes:"), get("write_bytes:"))
}

/// Çelësi i llogarisë `i`: i rastësishëm në pamje, por i rillogaritshëm (splitmix64).
fn key_of(i: u64) -> Key {
    let mut k = [0u8; 32];
    let mut x = i.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x5EED;
    for c in k.chunks_mut(8) {
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        c.copy_from_slice(&(z ^ (z >> 31)).to_le_bytes());
    }
    k
}

enum Engine {
    Pages(Box<PageTree>),
}

impl Engine {
    fn tree(&mut self) -> &mut dyn StateTree {
        match self {
            Engine::Pages(x) => x.as_mut(),
        }
    }
    fn view(&self) -> &dyn StateTree {
        match self {
            Engine::Pages(x) => x.as_ref(),
        }
    }
    /// Leximet nga disku në slotin e fundit.
    fn reads(&self) -> u64 {
        match self {
            Engine::Pages(x) => x.last_disk_reads.load(Ordering::Relaxed),
        }
    }
}

fn meta(rng: &mut StdRng) -> AccountMeta {
    let mut owner = [0u8; 32];
    owner[0] = rng.gen_range(0..16);
    AccountMeta { lamports: rng.gen_range(890_880..10_000_000_000), owner, executable: false, data_len: 165, data_hash: rng.gen() }
}

/// A22: dosja e krijuar nga ky ekzekutim; fshihet vetëm ajo, në fund (edhe pas panic-u).
struct OwnedDir(std::path::PathBuf);

impl Drop for OwnedDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn pct(v: &[f64], p: f64) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let n: usize = a.first().and_then(|x| x.parse().ok()).unwrap_or(1_000_000);
    let slots: u64 = a.get(1).and_then(|x| x.parse().ok()).unwrap_or(200);
    let per_slot: usize = a.get(2).and_then(|x| x.parse().ok()).unwrap_or(5_000);
    // A22: argumenti është dosja PRIND. Bench-i krijon brenda saj një dosje të re vetëm për këtë
    // ekzekutim (dështon nëse ekziston) dhe fshin vetëm atë. Asgjë tjetër te prindi s'preket.
    let parent = std::path::PathBuf::from(a.get(3).cloned().unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned()));
    std::fs::create_dir_all(&parent).expect("dosja prind");
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let dir = parent.join(format!("zilja-disk-bench-{}-{stamp}", std::process::id()));
    std::fs::create_dir(&dir).expect("dosja e ekzekutimit (s'duhet të ekzistojë)");
    let _own = OwnedDir(dir.clone());
    let mut rng = StdRng::seed_from_u64(42);
    eprintln!("zilja-disk-bench: {n} llogari, {slots} slote × {per_slot}, në {}", dir.display());

    let history: u64 = a.get(4).and_then(|x| x.parse().ok()).unwrap_or(zilja_store::DEFAULT_HISTORY);
    let durable = std::env::var_os("ZILJA_NOWAL").is_none();
    let cache_depth: u16 = std::env::var("ZILJA_CACHE_DEPTH").ok().and_then(|v| v.parse().ok()).unwrap_or(zilja_store::CACHE_DEPTH);
    let engine = std::env::var("ZILJA_ENGINE").unwrap_or_else(|_| "pages".into());
    let mut eng = match engine.as_str() {
        "pages" => Engine::Pages(Box::new(
            PageTree::open(
                &dir,
                PageOptions {
                    history,
                    cache_depth,
                    durable,
                    sync_every: std::env::var("ZILJA_SYNC_EVERY").ok().and_then(|v| v.parse().ok()).unwrap_or(1),
                    ..Default::default()
                },
            )
            .unwrap(),
        )),
        e => panic!("motor i panjohur: {e} (vetëm pages; RocksDB u hoq)"),
    };
    eprintln!("motori: {engine}, cache_depth {cache_depth}, history {history}, durable {durable}");
    let rss0 = rss_mb();
    let t0 = Instant::now();
    let mut nkeys: u64 = 0;
    let chunk = 1 << 18;
    while nkeys < n as u64 {
        let m = (chunk as u64).min(n as u64 - nkeys);
        let ups: Vec<_> = (nkeys..nkeys + m).map(|i| (key_of(i), Some(meta(&mut rng)))).collect();
        eng.tree().stage_snapshot(0, ups).unwrap();
        nkeys += m;
        if nkeys.is_multiple_of(1 << 24) {
            eprintln!("  staging {nkeys}/{n}");
        }
    }
    let stage_s = t0.elapsed().as_secs_f64();
    let t1 = Instant::now();
    eng.tree().finish_snapshot(0, true).unwrap();
    let build_s = t1.elapsed().as_secs_f64();
    let disk_after_build = dir_mb(&dir);
    eprintln!("snapshot: shkrimi {stage_s:.1} s, ndërtimi {build_s:.1} s, disk {disk_after_build:.0} MB");

    let mut apply_ms = Vec::new();
    let mut reads = Vec::new();
    let (r0, w0) = proc_io();
    let ta = Instant::now();
    for slot in 1..=slots {
        let ups: Vec<_> = (0..per_slot)
            .map(|_| {
                let r: f64 = rng.gen();
                if r < 0.9 {
                    (key_of(rng.gen_range(0..nkeys)), Some(meta(&mut rng)))
                } else if r < 0.98 {
                    nkeys += 1;
                    (key_of(nkeys - 1), Some(meta(&mut rng)))
                } else {
                    (key_of(rng.gen_range(0..nkeys)), None)
                }
            })
            .collect();
        let s = Instant::now();
        eng.tree().apply_slot(slot, ups, None).unwrap();
        apply_ms.push(s.elapsed().as_secs_f64() * 1000.0);
        reads.push(eng.reads() as f64);
        if slot % 100 == 0 {
            eprintln!("  slot {slot}/{slots}: p50 {:.1} ms", pct(&apply_ms, 0.5));
        }
    }
    let apply_wall = ta.elapsed().as_secs_f64();
    let (r1, w1) = proc_io();

    let t = eng.view();
    let root = t.root();
    let (lo, hi) = t.provable_range().unwrap();
    let mut prove_ms = Vec::new();
    let mut hist_ms = Vec::new();
    let mut ok = 0;
    let mut siblings = Vec::new();
    for i in 0..2000 {
        let k = key_of(rng.gen_range(0..nkeys));
        let s = Instant::now();
        let (_, r, p, m) = t.prove_at(&k, None).unwrap();
        prove_ms.push(s.elapsed().as_secs_f64() * 1000.0);
        siblings.push(p.siblings.len() as f64);
        assert_eq!(r, root);
        if verify_tree_key(&r, &k, m.as_ref(), &p) != Verdict::Invalid {
            ok += 1;
        }
        if i % 4 == 0 {
            let v = rng.gen_range(lo..=hi);
            let s = Instant::now();
            let (_, r, p, m) = t.prove_at(&k, Some(v)).unwrap();
            hist_ms.push(s.elapsed().as_secs_f64() * 1000.0);
            assert_ne!(verify_tree_key(&r, &k, m.as_ref(), &p), Verdict::Invalid);
        }
    }
    let out = json!({
        "accounts_initial": n,
        "accounts_final": t.len(),
        "slots": slots,
        "updates_per_slot": per_slot,
        "history_slots": history,
        "engine": engine,
        "durable": durable,
        "cache_depth": cache_depth,
        "snapshot_stage_seconds": stage_s,
        "snapshot_build_seconds": build_s,
        "apply_ms_p50": pct(&apply_ms, 0.5),
        "apply_ms_p99": pct(&apply_ms, 0.99),
        "apply_ms_max": pct(&apply_ms, 1.0),
        "disk_reads_per_slot_p50": pct(&reads, 0.5),
        "apply_wall_seconds": apply_wall,
        "slots_per_second": slots as f64 / apply_wall,
        "updates_per_second": (slots as usize * per_slot) as f64 / apply_wall,
        "device_write_mb_per_slot": (w1 - w0) as f64 / (1 << 20) as f64 / slots as f64,
        "device_write_bytes_per_update": (w1 - w0) as f64 / (slots as usize * per_slot) as f64,
        "device_read_mb_per_slot": (r1 - r0) as f64 / (1 << 20) as f64 / slots as f64,
        "disk_mb_after_build": disk_after_build,
        "prove_ms_p50": pct(&prove_ms, 0.5),
        "prove_ms_p99": pct(&prove_ms, 0.99),
        "historic_prove_ms_p50": pct(&hist_ms, 0.5),
        "proofs_verified": format!("{ok}/2000"),
        "proof_siblings_avg": siblings.iter().sum::<f64>() / siblings.len() as f64,
        "proof_siblings_max": pct(&siblings, 1.0),
        // Dëftesa në tel: një hash për çdo fqinj jo-bosh + gjethja + koka (vlerësim i sipërm).
        "proof_bytes_approx": 32.0 * siblings.iter().sum::<f64>() / siblings.len() as f64 + 32.0 + 64.0 + 40.0,
        "process_rss_mb": rss_mb(),
        "rss_growth_mb": rss_mb() - rss0,
        "disk_mb": dir_mb(&dir),
        "disk_bytes_per_account": dir_mb(&dir) * (1 << 20) as f64 / t.len() as f64,
        "note": "device_* nga /proc/self/io (bajte të dërguara realisht në disk)",
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
