//! Matjet e pemës në disk.
//!
//! `cargo run -p zilja-bench --release --bin zilja-disk-bench -- [llogari] [slote] [ndryshime_per_slot] [dir]`

use rand::{rngs::StdRng, Rng, SeedableRng};
use serde_json::json;
use std::sync::atomic::Ordering;
use std::time::Instant;
use zilja_store::{DiskTree, StoreOptions};
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
                        if m.is_dir() { walk(&e.path()) } else { m.len() }
                    })
                    .sum()
            })
            .unwrap_or(0)
    }
    walk(p) as f64 / (1 << 20) as f64
}

fn meta(rng: &mut StdRng) -> AccountMeta {
    let mut owner = [0u8; 32];
    owner[0] = rng.gen_range(0..16);
    AccountMeta { lamports: rng.gen_range(890_880..10_000_000_000), owner, executable: false, data_len: 165, data_hash: rng.gen() }
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
    let dir = std::path::PathBuf::from(a.get(3).cloned().unwrap_or_else(|| "/tmp/zilja-disk-bench".into()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut rng = StdRng::seed_from_u64(42);
    eprintln!("zilja-disk-bench: {n} llogari, {slots} slote × {per_slot}, në {}", dir.display());

    let history: u64 = a.get(4).and_then(|x| x.parse().ok()).unwrap_or(zilja_store::DEFAULT_HISTORY);
    let durable = std::env::var_os("ZILJA_NOWAL").is_none();
    let cache_depth: u16 = std::env::var("ZILJA_CACHE_DEPTH").ok().and_then(|v| v.parse().ok()).unwrap_or(zilja_store::CACHE_DEPTH);
    let mut t = DiskTree::open_opts(&dir, StoreOptions { history, cache_depth, block_cache_mb: std::env::var("ZILJA_BLOCK_CACHE_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(256), durable, ..Default::default() }).unwrap();
    let rss0 = rss_mb();
    let t0 = Instant::now();
    let mut keys: Vec<Key> = Vec::with_capacity(n);
    let chunk = 1 << 18;
    let mut staged = 0;
    while staged < n {
        let m = chunk.min(n - staged);
        let ups: Vec<_> = (0..m)
            .map(|_| {
                let k: Key = rng.gen();
                keys.push(k);
                (k, Some(meta(&mut rng)))
            })
            .collect();
        t.stage_snapshot(0, ups).unwrap();
        staged += m;
    }
    let stage_s = t0.elapsed().as_secs_f64();
    let t1 = Instant::now();
    t.finish_snapshot(0, true).unwrap();
    let build_s = t1.elapsed().as_secs_f64();
    eprintln!("snapshot: shkrimi {stage_s:.1} s, ndërtimi {build_s:.1} s, disk {:.0} MB", dir_mb(&dir));

    let mut apply_ms = Vec::new();
    let mut reads = Vec::new();
    for slot in 1..=slots {
        let ups: Vec<_> = (0..per_slot)
            .map(|_| {
                let r: f64 = rng.gen();
                if r < 0.9 {
                    (keys[rng.gen_range(0..keys.len())], Some(meta(&mut rng)))
                } else if r < 0.98 {
                    let k: Key = rng.gen();
                    keys.push(k);
                    (k, Some(meta(&mut rng)))
                } else {
                    (keys[rng.gen_range(0..keys.len())], None)
                }
            })
            .collect();
        let s = Instant::now();
        t.apply_slot(slot, ups, None).unwrap();
        apply_ms.push(s.elapsed().as_secs_f64() * 1000.0);
        reads.push(t.last_disk_reads.load(Ordering::Relaxed) as f64);
    }

    let root = t.root();
    let (lo, hi) = t.provable_range().unwrap();
    let mut prove_ms = Vec::new();
    let mut hist_ms = Vec::new();
    let mut ok = 0;
    for i in 0..2000 {
        let k = keys[rng.gen_range(0..keys.len())];
        let s = Instant::now();
        let (_, r, p, m) = t.prove_at(&k, None).unwrap();
        prove_ms.push(s.elapsed().as_secs_f64() * 1000.0);
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
        "durable_wal": durable,
        "cached_nodes": t.cached_nodes(),
        "cache_depth": cache_depth,
        "snapshot_stage_seconds": stage_s,
        "snapshot_build_seconds": build_s,
        "apply_ms_p50": pct(&apply_ms, 0.5),
        "apply_ms_p99": pct(&apply_ms, 0.99),
        "apply_ms_max": pct(&apply_ms, 1.0),
        "disk_reads_per_slot_p50": pct(&reads, 0.5),
        "prove_ms_p50": pct(&prove_ms, 0.5),
        "prove_ms_p99": pct(&prove_ms, 0.99),
        "historic_prove_ms_p50": pct(&hist_ms, 0.5),
        "proofs_verified": format!("{ok}/2000"),
        "process_rss_mb": rss_mb(),
        "rss_growth_mb": rss_mb() - rss0,
        "disk_mb": dir_mb(&dir),
        "disk_bytes_per_account": dir_mb(&dir) * (1 << 20) as f64 / t.len() as f64,
        "note": "process_rss përfshin edhe listën e çelësave të bench-it (32 B/llogari) dhe cache-në e RocksDB (256 MB)",
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
