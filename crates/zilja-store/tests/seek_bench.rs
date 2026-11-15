//! Simpler-4 (V7): matje para vendimit. Sa kushton një lexim me `seek_for_prev` mbi çelësa me
//! version, kundrejt një `get` të thjeshtë? Nisja:
//!   cargo test --release -p zilja-store --test seek_bench -- --ignored --nocapture
use rand::{rngs::StdRng, Rng, SeedableRng};
use rocksdb::{BlockBasedOptions, Cache, Options, DB};
use std::time::Instant;

#[test]
#[ignore]
fn get_vs_seek_for_prev() {
    let n: usize = std::env::var("N").ok().and_then(|v| v.parse().ok()).unwrap_or(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let mut o = Options::default();
    o.create_if_missing(true);
    let mut bo = BlockBasedOptions::default();
    bo.set_block_cache(&Cache::new_lru_cache(256 << 20));
    bo.set_bloom_filter(10.0, false);
    o.set_block_based_table_factory(&bo);
    let db = DB::open(&o, dir.path()).unwrap();
    let mut rng = StdRng::seed_from_u64(1);
    let keys: Vec<[u8; 34]> = (0..n).map(|_| std::array::from_fn(|_| rng.gen())).collect();
    let val = vec![7u8; 600];
    for k in &keys {
        db.put(k, &val).unwrap();
        // Versionet: 3 për çelës (si historia e 32 sloteve me ~3 ndryshime).
        for v in [10u64, 20, 30] {
            let mut vk = [0u8; 42];
            vk[..1].copy_from_slice(b"v");
            vk[1..35].copy_from_slice(k);
            vk[35..42].copy_from_slice(&v.to_be_bytes()[1..]);
            db.put(vk, &val).unwrap();
        }
    }
    db.flush().unwrap();
    db.compact_range(None::<&[u8]>, None::<&[u8]>);
    let probes: Vec<&[u8; 34]> = (0..200_000).map(|_| &keys[rng.gen_range(0..n)]).collect();
    let t = Instant::now();
    for k in &probes {
        assert!(db.get_pinned(k).unwrap().is_some());
    }
    let get_ns = t.elapsed().as_nanos() as f64 / probes.len() as f64;
    let t = Instant::now();
    let mut it = db.raw_iterator();
    for k in &probes {
        let mut vk = [0u8; 42];
        vk[..1].copy_from_slice(b"v");
        vk[1..35].copy_from_slice(*k);
        vk[35..42].copy_from_slice(&25u64.to_be_bytes()[1..]);
        it.seek_for_prev(vk);
        assert!(it.valid());
    }
    let seek_ns = t.elapsed().as_nanos() as f64 / probes.len() as f64;
    println!("Simpler-4: N={n}: get {get_ns:.0} ns, seek_for_prev {seek_ns:.0} ns ({:.1}×)", seek_ns / get_ns);
}
