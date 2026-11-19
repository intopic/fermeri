//! Audit proof-of-concept tests for the disk store (2026-11-15, commit f59be06).
#![cfg(feature = "rocksdb")] // A21: motori i vjetër, vetëm me feature-in `rocksdb`.

use rand::{rngs::StdRng, Rng, SeedableRng};
use std::sync::atomic::Ordering;
use zilja_store::{DiskTree, StoreOptions};
use zilja_tree::{StateTree, Update};
use zilja_verifier::{verify_tree_key, AccountMeta, Key, Verdict};

fn meta(rng: &mut StdRng) -> AccountMeta {
    AccountMeta { lamports: rng.gen_range(1..1 << 40), data_len: 0, data_hash: rng.gen(), ..Default::default() }
}

/// F-04. A page that cannot be read or decoded is treated as "this subtree is empty".
/// The next slot that touches it silently drops every other account under that page
/// and commits a new root; the dropped accounts then get *valid exclusion proofs*.
/// Expected: apply_slot refuses (error / halt). This test FAILS on f59be06.
#[test]
fn f04_unreadable_page_must_not_become_empty_subtree() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = StdRng::seed_from_u64(1);
    let accs: Vec<(Key, AccountMeta)> = (0..3000).map(|_| (rng.gen(), meta(&mut rng))).collect();
    {
        let mut t = DiskTree::open_opts(dir.path(), StoreOptions { cache_depth: 0, history: 0, ..Default::default() }).unwrap();
        t.stage_snapshot(1, accs.iter().map(|(k, m)| (*k, Some(m.clone()))).collect()).unwrap();
        t.finish_snapshot(1, true).unwrap();
        t.verify_integrity().unwrap();
    }
    // Damage one page below the top one (bit rot, a bad block, a short write...).
    let victim_prefix: Vec<u8>;
    {
        let cfs = ["nodes", "pages", "nodes_hist", "staging", "meta"];
        let mut o = rocksdb::Options::default();
        o.set_atomic_flush(true);
        let db = rocksdb::DB::open_cf(&o, dir.path(), cfs).unwrap();
        let cf = db.cf_handle("pages").unwrap();
        let (k, _) = db
            .iterator_cf(cf, rocksdb::IteratorMode::Start)
            .map(|r| r.unwrap())
            .find(|(k, _)| u16::from_be_bytes([k[0], k[1]]) == 4)
            .unwrap();
        victim_prefix = k[2..3].to_vec(); // first 4 bits of every key under this page
        db.put_cf(cf, &k, b"garbage").unwrap();
        db.flush().unwrap();
    }
    let under = |k: &Key| k[0] >> 4 == victim_prefix[0] >> 4;
    let mut t = DiskTree::open_opts(dir.path(), StoreOptions { cache_depth: 0, history: 0, ..Default::default() }).unwrap();
    assert_eq!(t.len(), 3000, "open did not notice (expected: only the top levels are checked)");
    let mut victims = accs.iter().filter(|(k, _)| under(k));
    let (touched, _) = victims.next().unwrap().clone();
    let (bystander, bystander_meta) = victims.next().unwrap().clone();
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let ups: Vec<Update> = vec![(touched, Some(meta(&mut rng)))];
        t.apply_slot(2, ups, None)
    }));
    // Faza 3: apply_slot kthen Result; gabimi (ose panic) është sjellja e pritur.
    if let Ok(Ok(root)) = res {
        let (_, _, proof, m) = t.prove_at(&bystander, None).unwrap();
        let excluded = m.is_none() && verify_tree_key(&root, &bystander, None, &proof) == Verdict::DoesNotExist;
        assert!(
            !excluded,
            "account with {} lamports was never deleted, yet the node now serves a VALID exclusion proof for it (len {} -> {})",
            bystander_meta.lamports,
            3000,
            t.len()
        );
    }
}

/// F-09 (measurement). Run with: cargo test --release -p zilja-store --test audit_poc -- --ignored --nocapture
/// (a debug build overflows the 2 MiB test-thread stack on the depth-256 paths).
/// Keys are raw pubkeys, and anyone can fund an
/// arbitrary 32-byte address, so an attacker can create "twins" that share 255 bits
/// with an existing key. Each twin pair costs one page per 4 levels down to depth 256.
///
/// Faza 4: pema renditet sipas `tree_key(pubkey)`, si në shërbim. Binjakët e pubkey-it bien
/// në vende të rastësishme, ndaj puna duhet të jetë si për ndryshimet e zakonshme.
#[test]
fn f09_deep_twin_keys_amplify_disk_work() {
    let tk = |k: &Key| zilja_verifier::tree_key(k);
    let dir = tempfile::tempdir().unwrap();
    let mut rng = StdRng::seed_from_u64(2);
    let keys: Vec<Key> = (0..50_000).map(|_| rng.gen()).collect();
    let mut t = DiskTree::open_opts(dir.path(), StoreOptions { cache_depth: 8, history: 32, ..Default::default() }).unwrap();
    t.stage_snapshot(1, keys.iter().map(|k| (tk(k), Some(meta(&mut rng)))).collect()).unwrap();
    t.finish_snapshot(1, true).unwrap();
    let n = 500;
    let run = |t: &mut DiskTree, slot: u64, ks: &[Key], rng: &mut StdRng| {
        let ups: Vec<Update> = ks.iter().map(|k| (tk(k), Some(meta(rng)))).collect();
        let t0 = std::time::Instant::now();
        t.apply_slot(slot, ups, None).unwrap();
        (t0.elapsed(), t.last_disk_reads.load(Ordering::Relaxed))
    };
    let (normal_t, normal_reads) = run(&mut t, 2, &keys[..n], &mut rng);
    let twins: Vec<Key> = keys[n..2 * n].iter().map(|k| { let mut x = *k; x[31] ^= 1; x }).collect();
    let (create_t, create_reads) = run(&mut t, 3, &twins, &mut rng);
    let (upd_t, upd_reads) = run(&mut t, 4, &twins, &mut rng);
    let (_, _, proof, _) = t.prove_at(&tk(&twins[0]), None).unwrap();
    println!("F-09 {n} ordinary updates : {normal_t:?}, {normal_reads} page reads");
    println!("F-09 {n} twin creations   : {create_t:?}, {create_reads} page reads");
    println!("F-09 {n} twin updates     : {upd_t:?}, {upd_reads} page reads, proof depth {}", proof.siblings.len());
    t.verify_integrity().unwrap();
    assert!(create_reads <= normal_reads * 3 && upd_reads <= normal_reads * 3, "binjakët ende shtojnë punë");
    assert!(proof.siblings.len() < 64, "rrugë e thellë: {}", proof.siblings.len());
}

/// F-09 (stack probe). Depth-256 paths recurse 256 frames in `apply`/`build`. Runs on a
/// 2 MiB thread, the default for the `zilja-worker` thread. A stack overflow aborts the
/// process, so the probe only runs when asked:
///   ZILJA_TWIN_PROBE=apply|snapshot [ZILJA_N=1] [ZILJA_CD=20] cargo test -p zilja-store --test audit_poc f09_twin_probe
/// On f59be06: release build passes; debug build overflows with `apply`, N=1.
#[test]
fn f09_twin_probe() {
    let Ok(mode) = std::env::var("ZILJA_TWIN_PROBE") else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut rng = StdRng::seed_from_u64(3);
    let cd: u16 = std::env::var("ZILJA_CD").ok().and_then(|v| v.parse().ok()).unwrap_or(20);
    let n: usize = std::env::var("ZILJA_N").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    let h = std::thread::Builder::new()
        .name("zilja-worker".into())
        .spawn(move || {
            let mut t = DiskTree::open_opts(dir.path(), StoreOptions { cache_depth: cd, history: 32, ..Default::default() }).unwrap();
            let keys: Vec<Key> = (0..1000.max(n)).map(|_| rng.gen()).collect();
            let twins: Vec<Key> = keys[..n].iter().map(|k| { let mut x = *k; x[31] ^= 1; x }).collect();
            let mut snap: Vec<Update> = keys.iter().map(|k| (*k, Some(meta(&mut rng)))).collect();
            if mode == "snapshot" {
                snap.extend(twins.iter().map(|k| (*k, Some(meta(&mut rng)))));
            }
            t.stage_snapshot(1, snap).unwrap();
            t.finish_snapshot(1, true).unwrap();
            if mode == "apply" {
                t.apply_slot(2, twins.iter().map(|k| (*k, Some(meta(&mut rng)))).collect(), None).unwrap();
                t.apply_slot(3, twins.iter().map(|k| (*k, Some(meta(&mut rng)))).collect(), None).unwrap();
                t.apply_slot(4, twins.iter().map(|k| (*k, None)).collect(), None).unwrap();
            }
            t.verify_integrity().unwrap();
            println!("probe {mode} cd={cd} n={n}: ok");
        })
        .unwrap();
    h.join().unwrap();
}
