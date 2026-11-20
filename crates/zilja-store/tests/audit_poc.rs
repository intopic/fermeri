//! Audit proof-of-concept tests for the disk store (2026-11-15, commit f59be06).

use rand::{rngs::StdRng, Rng, SeedableRng};
use std::sync::atomic::Ordering;
use zilja_store::{PageOptions, PageTree};
use zilja_tree::{StateTree, Update};
use zilja_verifier::{AccountMeta, Key};

fn meta(rng: &mut StdRng) -> AccountMeta {
    AccountMeta { lamports: rng.gen_range(1..1 << 40), data_len: 0, data_hash: rng.gen(), ..Default::default() }
}

// F-04. A page that cannot be read or decoded is treated as "this subtree is empty".
// The next slot that touches it silently drops every other account under that page
// and commits a new root; the dropped accounts then get *valid exclusion proofs*.
// Expected: apply_slot refuses (error / halt). This test FAILS on f59be06.
// F-04 (faqe e palexueshme ≠ nënpemë bosh) testohet për motorin me faqe te `pages.rs`:
// `injected_read_faults_never_produce_a_wrong_root` dhe `corrupted_page_is_an_error_not_a_wrong_proof`.

/// Faza 4: pema renditet sipas `tree_key(pubkey)`, si në shërbim. Binjakët e pubkey-it bien
/// në vende të rastësishme, ndaj puna duhet të jetë si për ndryshimet e zakonshme.
#[test]
fn f09_deep_twin_keys_amplify_disk_work() {
    let tk = |k: &Key| zilja_verifier::tree_key(k);
    let dir = tempfile::tempdir().unwrap();
    let mut rng = StdRng::seed_from_u64(2);
    let keys: Vec<Key> = (0..50_000).map(|_| rng.gen()).collect();
    let mut t = PageTree::open(dir.path(), PageOptions { cache_depth: 8, history: 32, durable: false, ..Default::default() }).unwrap();
    t.stage_snapshot(1, keys.iter().map(|k| (tk(k), Some(meta(&mut rng)))).collect()).unwrap();
    t.finish_snapshot(1, true).unwrap();
    let n = 500;
    let run = |t: &mut PageTree, slot: u64, ks: &[Key], rng: &mut StdRng| {
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
            let mut t = PageTree::open(dir.path(), PageOptions { cache_depth: cd, history: 32, durable: false, ..Default::default() }).unwrap();
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
