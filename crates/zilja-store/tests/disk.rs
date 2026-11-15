use rand::{rngs::StdRng, Rng, SeedableRng};
use std::collections::BTreeMap;
use zilja_store::{DiskTree, StoreOptions};
use zilja_tree::{StateTree, Tree, Update};
use zilja_verifier::{verify_account, AccountMeta, Key, Verdict};

fn meta(rng: &mut StdRng) -> AccountMeta {
    let mut owner = [0u8; 32];
    owner[0] = rng.gen_range(0..4);
    AccountMeta { lamports: rng.gen_range(1..1 << 40), owner, executable: rng.gen_bool(0.05), data_len: rng.gen_range(0..500), data_hash: rng.gen() }
}

fn random_batch(rng: &mut StdRng, keys: &mut Vec<Key>, n: usize) -> Vec<Update> {
    let mut b = Vec::new();
    for _ in 0..n {
        let r: f64 = rng.gen();
        if r < 0.4 || keys.is_empty() {
            let k: Key = rng.gen();
            keys.push(k);
            b.push((k, Some(meta(rng))));
        } else if r < 0.8 {
            b.push((keys[rng.gen_range(0..keys.len())], Some(meta(rng))));
        } else if r < 0.95 {
            b.push((keys[rng.gen_range(0..keys.len())], None));
        } else {
            // Fshirje e një llogarie që s'ka ekzistuar kurrë.
            b.push((rng.gen(), None));
        }
    }
    b
}

/// Kontrollon një dëftesë nga disku kundër pemës referencë në RAM.
fn check_proof(disk: &DiskTree, mem: &Tree, key: &Key, at: Option<u64>) {
    let (_, root, proof, m) = disk.prove_at(key, at).unwrap();
    assert_eq!(root, mem.root());
    assert_eq!(m.as_ref(), mem.get(key));
    let v = verify_account(&root, key, m.as_ref(), &proof);
    assert_eq!(v, if m.is_some() { Verdict::Exists } else { Verdict::DoesNotExist });
    // E njëjta dëftesë si ajo e pemës në RAM.
    assert_eq!(proof, mem.prove(key).0);
}

#[test]
fn disk_equals_memory_with_history_and_pruning() {
    // 0 = çdo nyje lexohet nga disku; 4 = përzierje; 20 = gjithçka në cache.
    for cache_depth in [0u16, 4, 20] {
        equivalence(cache_depth);
    }
}

fn equivalence(cache_depth: u16) {
    let dir = tempfile::tempdir().unwrap();
    let history = 8;
    let mut disk = DiskTree::open_opts(dir.path(), StoreOptions { history, cache_depth, ..Default::default() }).unwrap();
    let mut rng = StdRng::seed_from_u64(11 + cache_depth as u64);
    let mut keys = Vec::new();

    // Snapshot-i fillestar.
    let snap = random_batch(&mut rng, &mut keys, 3000);
    let snap: Vec<Update> = snap.into_iter().filter(|(_, m)| m.is_some()).collect();
    disk.stage_snapshot(100, snap.clone()).unwrap();
    disk.finish_snapshot(100, true).unwrap();
    let mut mem = Tree::from_accounts(snap);
    assert_eq!(disk.root(), mem.root());
    assert_eq!(disk.len(), mem.len());

    let mut past: BTreeMap<u64, Tree> = BTreeMap::new();
    past.insert(100, mem.clone());
    let mut slot = 100;
    for round in 0..60 {
        // Slote që kapërcehen ndonjëherë (si Rooted që kapërcen).
        slot += 1 + (round % 3);
        let n = rng.gen_range(1..300);
        let batch = random_batch(&mut rng, &mut keys, n);
        disk.apply_slot(slot, batch.clone(), Some((slot, [slot as u8; 32]))).unwrap();
        mem.apply_batch(batch);
        assert_eq!(disk.root(), mem.root(), "slot {slot}");
        assert_eq!(disk.len(), mem.len(), "slot {slot}");
        past.insert(slot, mem.clone());

        // Dëftesa në slotin e fundit dhe në slote të kaluara brenda kujtesës.
        let (lo, hi) = disk.provable_range().unwrap();
        assert_eq!(hi, slot);
        for _ in 0..20 {
            let k = if rng.gen_bool(0.8) { keys[rng.gen_range(0..keys.len())] } else { rng.gen() };
            check_proof(&disk, &mem, &k, None);
            let v = rng.gen_range(lo..=hi);
            // Gjendja në v = gjendja pas slotit të fundit të shkruar ≤ v.
            let (_, t) = past.range(..=v).next_back().unwrap();
            check_proof(&disk, t, &k, Some(v));
        }
        if lo > 100 {
            assert!(disk.prove_at(&keys[0], Some(lo - 1)).is_err(), "sloti jashtë kujtesës duhet refuzuar");
        }
    }
    disk.verify_integrity().unwrap();
}

#[test]
fn reopen_keeps_state_cache_and_history() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = StdRng::seed_from_u64(12);
    let mut keys = Vec::new();
    let mut mem = Tree::new();
    let mut past = BTreeMap::new();
    {
        let mut disk = DiskTree::open_opts(dir.path(), StoreOptions { history: 16, cache_depth: 3, ..Default::default() }).unwrap();
        disk.finish_snapshot(1, false).unwrap(); // nisje nga genesis, pa llogari
        for slot in 2..30 {
            let b = random_batch(&mut rng, &mut keys, 200);
            disk.apply_slot(slot, b.clone(), Some((slot, [slot as u8; 32]))).unwrap();
            mem.apply_batch(b);
            past.insert(slot, mem.clone());
        }
        assert_eq!(disk.root(), mem.root());
    }
    // Thellësia e cache-së lexohet nga disku (3), jo nga opsionet.
    let disk = DiskTree::open_opts(dir.path(), StoreOptions { history: 16, cache_depth: 9, ..Default::default() }).unwrap();
    assert_eq!(disk.cache_depth(), 3);
    assert!(disk.cached_nodes() > 0, "cache duhet rillogaritur nga disku");
    let r = disk.resume_point().unwrap();
    assert_eq!(r.slot, 29);
    assert_eq!(r.root, mem.root());
    assert_eq!(r.fingerprint, Some((29, [29u8; 32])));
    assert!(!r.complete);
    assert_eq!(disk.root(), mem.root());
    // Pas rihapjes, kujtesa nis nga sloti i rihapjes (nivelet e sipërme s'kanë histori në disk).
    assert_eq!(disk.provable_range(), Some((29, 29)));
    assert!(disk.prove_at(&keys[0], Some(20)).is_err());
    for k in keys.iter().take(200) {
        check_proof(&disk, &mem, k, None);
    }
    disk.verify_integrity().unwrap();
    // Vazhdon të punojë dhe historia rritet sërish.
    let mut disk = disk;
    for slot in 30..40 {
        let b = random_batch(&mut rng, &mut keys, 200);
        disk.apply_slot(slot, b.clone(), None).unwrap();
        mem.apply_batch(b);
        past.insert(slot, mem.clone());
    }
    assert_eq!(disk.root(), mem.root());
    for k in keys.iter().take(200) {
        check_proof(&disk, &past[&33], k, Some(33));
    }
    disk.verify_integrity().unwrap();
}

#[test]
fn snapshot_with_buckets_duplicates_and_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = StdRng::seed_from_u64(13);
    let mut disk = DiskTree::open_opts(dir.path(), StoreOptions { history: 4, cache_depth: 6, ..Default::default() }).unwrap();
    disk.bucket_target = 64; // shumë kova edhe me pak llogari
    let accs: Vec<Update> = (0..20_000).map(|_| (rng.gen(), Some(meta(&mut rng)))).collect();
    // Copa, me një llogari që del dy herë (e fundit fiton) dhe një fshirje.
    let mut dup = accs[5].clone();
    dup.1 = Some(meta(&mut rng));
    disk.stage_snapshot(50, accs[..10_000].to_vec()).unwrap();
    disk.stage_snapshot(50, accs[10_000..].to_vec()).unwrap();
    disk.stage_snapshot(50, vec![dup.clone(), (accs[7].0, None)]).unwrap();
    disk.finish_snapshot(50, true).unwrap();
    let mut expect = accs.clone();
    expect.push(dup);
    expect.push((accs[7].0, None));
    let mem = Tree::from_accounts(expect);
    assert_eq!(disk.root(), mem.root());
    assert_eq!(disk.len(), mem.len());
    disk.verify_integrity().unwrap();

    // Një snapshot i ri fshin gjendjen e vjetër.
    let accs2: Vec<Update> = (0..500).map(|_| (rng.gen(), Some(meta(&mut rng)))).collect();
    disk.stage_snapshot(90, accs2.clone()).unwrap();
    disk.finish_snapshot(90, true).unwrap();
    assert_eq!(disk.root(), Tree::from_accounts(accs2).root());
    assert_eq!(disk.len(), 500);
    disk.verify_integrity().unwrap();
}

#[test]
fn archive_stays_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = StdRng::seed_from_u64(14);
    let mut disk = DiskTree::open_opts(dir.path(), StoreOptions { history: 4, cache_depth: 2, ..Default::default() }).unwrap();
    let keys: Vec<Key> = (0..200).map(|_| rng.gen()).collect();
    disk.stage_snapshot(0, keys.iter().map(|k| (*k, Some(meta(&mut rng)))).collect()).unwrap();
    disk.finish_snapshot(0, true).unwrap();
    let mut sizes = Vec::new();
    for slot in 1..=200u64 {
        // Të njëjtat llogari ndryshojnë pa pushim: arkiva s'duhet të rritet pa fund.
        let b: Vec<Update> = keys.iter().take(50).map(|k| (*k, Some(meta(&mut rng)))).collect();
        disk.apply_slot(slot, b, None).unwrap();
        disk.compact_history();
        sizes.push(disk.debug_hist_entries());
    }
    let early = sizes[20];
    let late = *sizes.last().unwrap();
    assert!(late <= early * 2, "arkiva rritet: {early} → {late}");
    disk.verify_integrity().unwrap();
}

/// F-02: versionet e snapshot-it vijnë në radhë të çfarëdoshme; fiton sloti më i ri,
/// dhe një llogari e mbyllur nuk ringjallet nga një kopje e vjetër.
#[test]
fn snapshot_newest_version_wins_on_disk_and_in_ram() {
    let dir = tempfile::tempdir().unwrap();
    let mut disk = zilja_store::DiskTree::open(dir.path(), 8).unwrap();
    let mut ram = zilja_tree::MemTree::new();
    let m = |l: u64| Some(AccountMeta { lamports: l, ..Default::default() });
    for t in [&mut disk as &mut dyn StateTree, &mut ram] {
        t.stage_snapshot(20, vec![([7; 32], m(200)), ([8; 32], None)]).unwrap();
        t.stage_snapshot(10, vec![([7; 32], m(100)), ([8; 32], m(5)), ([9; 32], m(9))]).unwrap();
        t.stage_snapshot(20, vec![([9; 32], m(90))]).unwrap();
        t.finish_snapshot(20, true).unwrap();
    }
    let want = Tree::from_accounts(vec![([7; 32], m(200)), ([9; 32], m(90))]);
    assert_eq!(disk.root(), want.root());
    assert_eq!(ram.root(), want.root());
    assert_eq!(disk.len(), 2);
}

/// Safer-6: lexime që dështojnë rastësisht. Çdo `apply_slot` ose kthen gabim, ose jep
/// saktësisht root-in e pemës referencë; pas një gabimi, disku i rihapur është i paprekur
/// (në slotin e fundit të suksesshëm) dhe vazhdon saktë.
#[test]
fn injected_read_faults_never_produce_a_wrong_root() {
    let mut rng = StdRng::seed_from_u64(66);
    for round in 0..6u64 {
        let dir = tempfile::tempdir().unwrap();
        let keys: Vec<Key> = (0..4000).map(|_| rng.gen()).collect();
        let mut mem = Tree::from_accounts(keys.iter().map(|k| (*k, Some(meta(&mut rng)))).collect());
        let mut disk = DiskTree::open_opts(dir.path(), StoreOptions { history: 0, cache_depth: 4, ..Default::default() }).unwrap();
        disk.stage_snapshot(1, mem_accounts(&mem, &keys)).unwrap();
        disk.finish_snapshot(1, true).unwrap();
        assert_eq!(disk.root(), mem.root());
        disk.inject_read_faults(7 + round * 13);
        let mut errors = 0;
        for slot in 2..40u64 {
            let ups: Vec<Update> = (0..rng.gen_range(1..60))
                .map(|_| (keys[rng.gen_range(0..keys.len())], if rng.gen_bool(0.1) { None } else { Some(meta(&mut rng)) }))
                .collect();
            match disk.apply_slot(slot, ups.clone(), None) {
                Ok(root) => {
                    mem.apply_batch(ups);
                    assert_eq!(root, mem.root(), "round {round} slot {slot}: root i gabuar pa gabim");
                }
                Err(_) => {
                    errors += 1;
                    // Zilja do të ndalonte këtu; rihap diskun pa gabime dhe vazhdo nga ai.
                    drop(disk);
                    disk = DiskTree::open_opts(dir.path(), StoreOptions { history: 0, cache_depth: 4, ..Default::default() }).unwrap();
                    assert_eq!(disk.root(), mem.root(), "round {round}: disku ndryshoi nga një slot i dështuar");
                    disk.verify_integrity().unwrap();
                    disk.inject_read_faults(7 + round * 13);
                }
            }
        }
        assert!(errors > 0, "round {round}: asnjë gabim i injektuar s'u pa");
    }
}

fn mem_accounts(mem: &Tree, keys: &[Key]) -> Vec<Update> {
    keys.iter().filter_map(|k| mem.get(k).map(|m| (*k, Some(m.clone())))).collect()
}
