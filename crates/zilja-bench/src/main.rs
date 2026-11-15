//! Matjet e Ziles me llogari sintetike.
//!
//! ```text
//! cargo run -p zilja-bench --release -- [llogari] [slote] [ndryshime_per_slot]
//! cargo run -p zilja-bench --release -- 1000000 200 5000
//! ```
//!
//! Ndryshimet për slot ndahen: 90% llogari ekzistuese, 8% të reja, 2% të fshira.

use rand::{rngs::StdRng, Rng, SeedableRng};
use serde_json::json;
use std::time::Instant;
use zilja_tree::{Tree, Update};
use zilja_verifier::{verify_account, AccountMeta, Key, Proof, Verdict};

fn rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("VmRSS:")).map(|l| l.to_string()))
        .and_then(|l| l.split_whitespace().nth(1).and_then(|v| v.parse::<f64>().ok()))
        .map(|kb| kb / 1024.0)
        .unwrap_or(0.0)
}

fn meta(rng: &mut StdRng) -> AccountMeta {
    let mut owner = [0u8; 32];
    owner[0] = rng.gen_range(0..16); // pak programe pronare, si në realitet
    let mut data_hash = [0u8; 32];
    rng.fill(&mut data_hash);
    AccountMeta { lamports: rng.gen_range(890_880..10_000_000_000), owner, executable: false, data_len: 165, data_hash }
}

fn key(rng: &mut StdRng) -> Key {
    let mut k = [0u8; 32];
    rng.fill(&mut k);
    k
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if v.is_empty() {
        return 0.0;
    }
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn main() {
    let args: Vec<u64> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let n = *args.first().unwrap_or(&1_000_000) as usize;
    let slots = *args.get(1).unwrap_or(&200) as usize;
    let per_slot = *args.get(2).unwrap_or(&5_000) as usize;
    let threads = rayon_threads();
    eprintln!("zilja-bench: {n} llogari, {slots} slote × {per_slot} ndryshime, {threads} thread-e");

    let mut rng = StdRng::seed_from_u64(42);
    let rss0 = rss_mb();
    let mut keys: Vec<Key> = (0..n).map(|_| key(&mut rng)).collect();
    let initial: Vec<Update> = keys.iter().map(|k| (*k, Some(meta(&mut rng)))).collect();
    let t0 = Instant::now();
    let mut tree = Tree::from_accounts(initial);
    let build_s = t0.elapsed().as_secs_f64();
    let rss_tree = rss_mb() - rss0;
    eprintln!("ndërtimi: {build_s:.2} s, memorie ~{rss_tree:.0} MB");

    let mut apply_ms = Vec::with_capacity(slots);
    for _ in 0..slots {
        let mut batch = Vec::with_capacity(per_slot);
        for _ in 0..per_slot {
            let r: f64 = rng.gen();
            if r < 0.90 {
                let k = keys[rng.gen_range(0..keys.len())];
                batch.push((k, Some(meta(&mut rng))));
            } else if r < 0.98 {
                let k = key(&mut rng);
                keys.push(k);
                batch.push((k, Some(meta(&mut rng))));
            } else {
                let k = keys[rng.gen_range(0..keys.len())];
                batch.push((k, None));
            }
        }
        let t = Instant::now();
        tree.apply_batch(batch);
        apply_ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }

    let root = tree.root();
    let samples = 10_000.min(keys.len());
    let mut sizes = Vec::with_capacity(samples);
    let mut depths = Vec::with_capacity(samples);
    let mut verify_us = Vec::with_capacity(samples);
    let mut ok = 0usize;
    for _ in 0..samples {
        let k = keys[rng.gen_range(0..keys.len())];
        let (p, m) = tree.prove(&k);
        let bytes = p.to_bytes();
        sizes.push(bytes.len() as f64);
        depths.push(p.siblings.len() as f64);
        let m = m.cloned();
        let t = Instant::now();
        let p2 = Proof::from_bytes(&bytes).unwrap();
        let v = verify_account(&root, &k, m.as_ref(), &p2);
        verify_us.push(t.elapsed().as_secs_f64() * 1e6);
        if matches!(v, Verdict::Exists | Verdict::DoesNotExist) {
            ok += 1;
        }
    }

    let total_apply: f64 = apply_ms.iter().sum();
    let out = json!({
        "accounts_initial": n,
        "accounts_final": tree.len(),
        "threads": threads,
        "slots": slots,
        "updates_per_slot": per_slot,
        "build_seconds": build_s,
        "tree_memory_mb": rss_tree,
        "bytes_per_account": rss_tree * 1024.0 * 1024.0 / n as f64,
        "apply_ms_p50": pct(&mut apply_ms.clone(), 0.5),
        "apply_ms_p99": pct(&mut apply_ms.clone(), 0.99),
        "apply_ms_max": pct(&mut apply_ms.clone(), 1.0),
        "apply_ms_mean": total_apply / slots as f64,
        "updates_per_second": (slots * per_slot) as f64 / (total_apply / 1000.0),
        "proof_bytes_p50": pct(&mut sizes.clone(), 0.5),
        "proof_bytes_max": pct(&mut sizes.clone(), 1.0),
        "proof_depth_p50": pct(&mut depths.clone(), 0.5),
        "proof_depth_max": pct(&mut depths.clone(), 1.0),
        "verify_us_p50": pct(&mut verify_us.clone(), 0.5),
        "verify_us_max": pct(&mut verify_us.clone(), 1.0),
        "proofs_verified": format!("{ok}/{samples}"),
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

fn rayon_threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}
