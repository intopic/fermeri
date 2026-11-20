//! Motori me faqe (`PageTree`): i njëjti sjellje si pema në RAM, në çdo rast.
//!
//! P-03: dy lloje testesh qëndrueshmërie, me emra të ndarë:
//! - `crash_*`: rrëzim i **procesit** (SIGKILL, panic). Çdo shkrim i kryer është në disk; humbet
//!   vetëm puna që s'u mbyll. Rihapja duhet të japë slotin e fundit të konfirmuar.
//! - `unsynced_loss_*`: simulim i **ikjes së rrymës**: humbin bajtet që s'u bënë fsync (bishti i
//!   log-ut, rekordet e fundit). Rihapja duhet të japë një slot të plotë para tyre.
//! Dëmtimi i qëllimshëm (CRC, format, rend) testohet në `recovery.rs`.

use rand::{rngs::StdRng, Rng, SeedableRng};
use std::collections::BTreeMap;
use zilja_store::{PageOptions, PageTree};
use zilja_tree::{StateTree, Tree, Update};
use zilja_verifier::{verify_tree_key, AccountMeta, Key, Verdict, EMPTY};

fn meta(rng: &mut StdRng) -> AccountMeta {
    let mut owner = [0u8; 32];
    owner[0] = rng.gen_range(0..4);
    AccountMeta {
        lamports: rng.gen_range(1..1 << 40),
        owner,
        executable: rng.gen_bool(0.05),
        data_len: rng.gen_range(0..500),
        data_hash: rng.gen(),
    }
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
            b.push((rng.gen(), None));
        }
    }
    b
}

fn opts(history: u64, cache_depth: u16) -> PageOptions {
    PageOptions { history, cache_depth, durable: false, ..Default::default() }
}

fn check_proof(t: &PageTree, mem: &Tree, key: &Key, at: Option<u64>) {
    let (_, root, proof, m) = t.prove_at(key, at).unwrap();
    assert_eq!(root, mem.root());
    assert_eq!(m.as_ref(), mem.get(key));
    let v = verify_tree_key(&root, key, m.as_ref(), &proof);
    assert_eq!(v, if m.is_some() { Verdict::Exists } else { Verdict::DoesNotExist });
    assert_eq!(proof, mem.prove(key).0);
}

fn mem_accounts(mem: &Tree, keys: &[Key]) -> Vec<Update> {
    keys.iter().filter_map(|k| mem.get(k).map(|m| (*k, Some(m.clone())))).collect()
}

#[test]
fn pages_equal_memory_with_history() {
    for (cd, ckpt) in [(0u16, 1000u64), (4, 7), (8, 1), (20, 256)] {
        let dir = tempfile::tempdir().unwrap();
        let mut o = opts(8, cd);
        o.ckpt_every = ckpt;
        let mut t = PageTree::open(dir.path(), o).unwrap();
        let mut rng = StdRng::seed_from_u64(11 + cd as u64);
        let mut keys = Vec::new();
        let snap: Vec<Update> = random_batch(&mut rng, &mut keys, 3000).into_iter().filter(|(_, m)| m.is_some()).collect();
        t.stage_snapshot(100, snap.clone()).unwrap();
        t.finish_snapshot(100, true).unwrap();
        let mut mem = Tree::from_accounts(snap);
        assert_eq!(t.root(), mem.root());
        assert_eq!(t.len(), mem.len());
        t.verify_integrity().unwrap();
        let mut past: BTreeMap<u64, Tree> = BTreeMap::new();
        past.insert(100, mem.clone());
        let mut slot = 100;
        for round in 0..60 {
            slot += 1 + (round % 3);
            let n = rng.gen_range(1..300);
            let batch = random_batch(&mut rng, &mut keys, n);
            t.apply_slot(slot, batch.clone(), Some((slot, [slot as u8; 32]))).unwrap();
            mem.apply_batch(batch);
            assert_eq!(t.root(), mem.root(), "cd {cd} slot {slot}");
            assert_eq!(t.len(), mem.len(), "cd {cd} slot {slot}");
            past.insert(slot, mem.clone());
            let (lo, hi) = t.provable_range().unwrap();
            assert_eq!(hi, slot);
            for _ in 0..20 {
                let k = if rng.gen_bool(0.8) { keys[rng.gen_range(0..keys.len())] } else { rng.gen() };
                check_proof(&t, &mem, &k, None);
                let v = rng.gen_range(lo..=hi);
                let (_, old) = past.range(..=v).next_back().unwrap();
                check_proof(&t, old, &k, Some(v));
            }
            if lo > 100 {
                assert!(t.prove_at(&keys[0], Some(lo - 1)).is_err());
            }
        }
        t.verify_integrity().unwrap();
    }
}

#[test]
fn reopen_replays_log_and_checkpoint() {
    for ckpt in [1u64, 5, 1000] {
        let dir = tempfile::tempdir().unwrap();
        let mut rng = StdRng::seed_from_u64(12);
        let mut keys = Vec::new();
        let mut mem = Tree::new();
        let mut o = opts(16, 4);
        o.ckpt_every = ckpt;
        {
            let mut t = PageTree::open(dir.path(), o.clone()).unwrap();
            t.finish_snapshot(1, false).unwrap();
            for slot in 2..30 {
                let b = random_batch(&mut rng, &mut keys, 200);
                t.apply_slot(slot, b.clone(), Some((slot, [slot as u8; 32]))).unwrap();
                mem.apply_batch(b);
            }
            assert_eq!(t.root(), mem.root());
        }
        // Thellësia e cache-së lexohet nga disku (4), jo nga opsionet.
        let mut o2 = o.clone();
        o2.cache_depth = 12;
        let mut t = PageTree::open(dir.path(), o2.clone()).unwrap();
        assert_eq!(t.cache_depth(), 4);
        let r = t.resume_point().unwrap();
        assert_eq!((r.slot, r.root, r.fingerprint, r.complete), (29, mem.root(), Some((29, [29u8; 32])), false));
        assert_eq!(t.provable_range(), Some((29, 29)));
        for k in keys.iter().take(200) {
            check_proof(&t, &mem, k, None);
        }
        t.verify_integrity().unwrap();
        // Vazhdon saktë: alokuesi i rindërtuar s'mbishkruan asnjë faqe të gjallë.
        let mut past = BTreeMap::new();
        for slot in 30..60 {
            let b = random_batch(&mut rng, &mut keys, 200);
            t.apply_slot(slot, b.clone(), None).unwrap();
            mem.apply_batch(b);
            past.insert(slot, mem.clone());
            assert_eq!(t.root(), mem.root());
        }
        for k in keys.iter().take(200) {
            check_proof(&t, &past[&50], k, Some(50));
        }
        t.verify_integrity().unwrap();
        drop(t);
        let t = PageTree::open(dir.path(), o2).unwrap();
        assert_eq!(t.root(), mem.root());
        t.verify_integrity().unwrap();
    }
}

/// Rrëzim pas shkrimit të faqeve, para rekordit: rihapja jep slotin e mëparshëm dhe vazhdon.
#[test]
fn crash_between_pages_and_log_record() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = StdRng::seed_from_u64(21);
    let keys: Vec<Key> = (0..3000).map(|_| rng.gen()).collect();
    let mut mem = Tree::from_accounts(keys.iter().map(|k| (*k, Some(meta(&mut rng)))).collect());
    let mut t = PageTree::open(dir.path(), opts(4, 4)).unwrap();
    t.stage_snapshot(1, mem_accounts(&mem, &keys)).unwrap();
    t.finish_snapshot(1, true).unwrap();
    for slot in 2..30u64 {
        let ups: Vec<Update> = (0..150).map(|_| (keys[rng.gen_range(0..keys.len())], Some(meta(&mut rng)))).collect();
        if slot % 5 == 0 {
            // Faqet shkruhen, rekordi jo: "rrëzim".
            let _p = t.prepare_slot(slot, ups, None).unwrap();
            drop(_p);
            drop(t);
            t = PageTree::open(dir.path(), opts(4, 4)).unwrap();
            assert_eq!(t.root(), mem.root(), "slot {slot}");
            t.verify_integrity().unwrap();
        } else {
            t.apply_slot(slot, ups.clone(), None).unwrap();
            mem.apply_batch(ups);
        }
        assert_eq!(t.root(), mem.root());
    }
    t.verify_integrity().unwrap();
}

/// Bishti i log-ut i prerë në çdo bajt: rihapja jep gjithmonë një slot të plotë dhe të saktë.
#[test]
fn unsynced_loss_torn_log_tail_at_every_offset() {
    let src = tempfile::tempdir().unwrap();
    let mut rng = StdRng::seed_from_u64(31);
    let keys: Vec<Key> = (0..800).map(|_| rng.gen()).collect();
    let mut mem = Tree::from_accounts(keys.iter().map(|k| (*k, Some(meta(&mut rng)))).collect());
    let mut o = opts(4, 4);
    o.ckpt_every = 1_000_000;
    // Rezervë më e madhe se numri i rekordeve: çdo prerje është brenda garancisë.
    o.reuse_slack = 8;
    let mut roots = vec![];
    {
        let mut t = PageTree::open(src.path(), o.clone()).unwrap();
        t.stage_snapshot(1, mem_accounts(&mem, &keys)).unwrap();
        t.finish_snapshot(1, true).unwrap();
        roots.push((1u64, mem.root()));
        for slot in 2..8u64 {
            let ups: Vec<Update> = (0..40).map(|_| (keys[rng.gen_range(0..keys.len())], Some(meta(&mut rng)))).collect();
            t.apply_slot(slot, ups.clone(), None).unwrap();
            mem.apply_batch(ups);
            roots.push((slot, mem.root()));
        }
    }
    let log = std::fs::read(src.path().join("meta.log")).unwrap();
    assert!(!log.is_empty());
    for cut in (0..=log.len()).step_by(7).chain([log.len()]) {
        let d = tempfile::tempdir().unwrap();
        for f in std::fs::read_dir(src.path()).unwrap() {
            let f = f.unwrap();
            std::fs::copy(f.path(), d.path().join(f.file_name())).unwrap();
        }
        std::fs::write(d.path().join("meta.log"), &log[..cut]).unwrap();
        let t = PageTree::open(d.path(), o.clone()).unwrap();
        // Humbja e rekordeve më pak se `history + slack` jep gjithmonë një slot të plotë dhe të saktë.
        let v = t.version().unwrap_or_else(|| panic!("cut {cut}: gjendja humbi"));
        let want = roots.iter().find(|(s, _)| *s == v).unwrap().1;
        assert_eq!(t.root(), want, "cut {cut}");
        t.verify_integrity().unwrap();
        if cut == log.len() {
            assert_eq!(v, 7);
        }
    }
}

/// Rrëzim pas checkpoint-it, para zbrazjes së log-ut: rekordet e vjetra kapërcehen.
#[test]
fn crash_between_checkpoint_and_log_truncate() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = StdRng::seed_from_u64(41);
    let mut keys = Vec::new();
    let mut mem = Tree::new();
    let mut o = opts(4, 4);
    o.ckpt_every = 10;
    let mut t = PageTree::open(dir.path(), o.clone()).unwrap();
    t.finish_snapshot(0, true).unwrap();
    let mut saved = Vec::new();
    for slot in 1..=25u64 {
        let b = random_batch(&mut rng, &mut keys, 100);
        t.apply_slot(slot, b.clone(), None).unwrap();
        mem.apply_batch(b);
        if slot == 9 {
            saved = std::fs::read(dir.path().join("meta.log")).unwrap();
        }
    }
    drop(t);
    // Log-u i vjetër (para checkpoint-it të slotit 10) kthehet, si të mos ishte zbrazur kurrë.
    let mut cur = std::fs::read(dir.path().join("meta.log")).unwrap();
    let mut stale = saved.clone();
    stale.append(&mut cur);
    std::fs::write(dir.path().join("meta.log"), stale).unwrap();
    let t = PageTree::open(dir.path(), o).unwrap();
    assert_eq!(t.version(), Some(25));
    assert_eq!(t.root(), mem.root());
    t.verify_integrity().unwrap();
}

#[test]
fn snapshot_newest_wins_buckets_and_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = StdRng::seed_from_u64(13);
    let mut t = PageTree::open(dir.path(), opts(4, 8)).unwrap();
    let accs: Vec<Update> = (0..20_000).map(|_| (rng.gen(), Some(meta(&mut rng)))).collect();
    let mut dup = accs[5].clone();
    dup.1 = Some(meta(&mut rng));
    t.stage_snapshot(50, accs[..10_000].to_vec()).unwrap();
    t.stage_snapshot(50, accs[10_000..].to_vec()).unwrap();
    t.stage_snapshot(50, vec![dup.clone(), (accs[7].0, None)]).unwrap();
    // F-02: një version më i vjetër që vjen më vonë nuk fiton.
    t.stage_snapshot(40, vec![(accs[9].0, Some(meta(&mut rng))), (accs[7].0, Some(meta(&mut rng)))]).unwrap();
    t.finish_snapshot(50, true).unwrap();
    let mut expect = accs.clone();
    expect.push(dup);
    expect.push((accs[7].0, None));
    let mem = Tree::from_accounts(expect);
    assert_eq!(t.root(), mem.root());
    assert_eq!(t.len(), mem.len());
    t.verify_integrity().unwrap();
    let accs2: Vec<Update> = (0..500).map(|_| (rng.gen(), Some(meta(&mut rng)))).collect();
    t.stage_snapshot(90, accs2.clone()).unwrap();
    t.finish_snapshot(90, true).unwrap();
    assert_eq!(t.root(), Tree::from_accounts(accs2).root());
    assert_eq!(t.len(), 500);
    t.verify_integrity().unwrap();
}

#[test]
fn tiny_trees_and_deep_prefixes() {
    // Rrugët 256 nivele kërkojnë stack të madh në debug (si thread-i i ziles në prodhim, 64 MB).
    std::thread::Builder::new().stack_size(64 << 20).spawn(tiny_trees_and_deep_prefixes_body).unwrap().join().unwrap();
}

fn tiny_trees_and_deep_prefixes_body() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = PageTree::open(dir.path(), opts(2, 8)).unwrap();
    t.finish_snapshot(1, true).unwrap();
    assert_eq!(t.root(), EMPTY);
    let m = |l: u64| Some(AccountMeta { lamports: l, ..Default::default() });
    let mut mem = Tree::new();
    // Çelësa që ndryshojnë vetëm në bit-in e fundit: rrugë 255 nivele, ~64 faqe.
    let a = [0u8; 32];
    let mut b = [0u8; 32];
    b[31] = 1;
    let mut c = [0u8; 32];
    c[0] = 0x80;
    let steps: Vec<Vec<Update>> = vec![
        vec![(a, m(1))],
        vec![(b, m(2))],
        vec![(c, m(3))],
        vec![(a, None)],
        vec![(b, None), (a, m(4))],
        vec![(a, None), (c, None)],
        vec![(b, m(5)), (a, m(6))],
    ];
    for (i, s) in steps.into_iter().enumerate() {
        let slot = 2 + i as u64;
        t.apply_slot(slot, s.clone(), None).unwrap();
        mem.apply_batch(s);
        assert_eq!(t.root(), mem.root(), "step {i}");
        for k in [a, b, c] {
            check_proof(&t, &mem, &k, None);
        }
        t.verify_integrity().unwrap();
    }
}

#[test]
fn space_is_reused_under_churn() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = StdRng::seed_from_u64(14);
    let mut t = PageTree::open(dir.path(), opts(4, 4)).unwrap();
    let keys: Vec<Key> = (0..2000).map(|_| rng.gen()).collect();
    t.stage_snapshot(0, keys.iter().map(|k| (*k, Some(meta(&mut rng)))).collect()).unwrap();
    t.finish_snapshot(0, true).unwrap();
    let used = |t: &PageTree| t.page_stats().blocks;
    let mut sizes = Vec::new();
    for slot in 1..=300u64 {
        let b: Vec<Update> = keys.iter().take(300).map(|k| (*k, Some(meta(&mut rng)))).collect();
        t.apply_slot(slot, b, None).unwrap();
        sizes.push(used(&t));
    }
    let (early, late) = (sizes[60], *sizes.last().unwrap());
    assert!(late <= early + early / 5, "skedari rritet pa fund: {early} → {late}");
    // Mbledhja e hapësirës mban blloqet e përdorura të mbushura mirë.
    let s = t.page_stats();
    let fill = s.live_bytes as f64 / ((s.blocks - s.free_blocks) * zilja_store::PAGE_BLOCK as u64) as f64;
    assert!(fill > 0.5, "blloqet janë shumë bosh: {fill:.2}");
    t.verify_integrity().unwrap();
}

#[test]
fn injected_read_faults_never_produce_a_wrong_root() {
    let mut rng = StdRng::seed_from_u64(66);
    let mut total = 0;
    for round in 0..6u64 {
        let dir = tempfile::tempdir().unwrap();
        let keys: Vec<Key> = (0..4000).map(|_| rng.gen()).collect();
        let mut mem = Tree::from_accounts(keys.iter().map(|k| (*k, Some(meta(&mut rng)))).collect());
        let mut t = PageTree::open(dir.path(), opts(0, 4)).unwrap();
        t.stage_snapshot(1, mem_accounts(&mem, &keys)).unwrap();
        t.finish_snapshot(1, true).unwrap();
        t.inject_read_faults(2 + round * 3);
        let mut errors = 0;
        for slot in 2..40u64 {
            let ups: Vec<Update> = (0..rng.gen_range(1..60))
                .map(|_| (keys[rng.gen_range(0..keys.len())], if rng.gen_bool(0.1) { None } else { Some(meta(&mut rng)) }))
                .collect();
            match t.apply_slot(slot, ups.clone(), None) {
                Ok(root) => {
                    mem.apply_batch(ups);
                    assert_eq!(root, mem.root(), "round {round} slot {slot}");
                }
                Err(_) => {
                    errors += 1;
                    drop(t);
                    t = PageTree::open(dir.path(), opts(0, 4)).unwrap();
                    assert_eq!(t.root(), mem.root());
                    t.verify_integrity().unwrap();
                    t.inject_read_faults(2 + round * 3);
                }
            }
        }
        total += errors;
    }
    // Leximet që dështojnë te leximi paraprak riprovohen; ato që dështojnë sërish ndalojnë slotin.
    assert!(total > 0, "asnjë gabim i injektuar s'u pa");
}

/// Një faqe e dëmtuar në disk jep gabim, kurrë dëftesë të gabuar.
#[test]
fn corrupted_page_is_an_error_not_a_wrong_proof() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = StdRng::seed_from_u64(71);
    let keys: Vec<Key> = (0..5000).map(|_| rng.gen()).collect();
    let mem = Tree::from_accounts(keys.iter().map(|k| (*k, Some(meta(&mut rng)))).collect());
    {
        let mut t = PageTree::open(dir.path(), opts(4, 0)).unwrap();
        t.stage_snapshot(1, mem_accounts(&mem, &keys)).unwrap();
        t.finish_snapshot(1, true).unwrap();
    }
    {
        let p = dir.path().join("pages.dat");
        let mut b = std::fs::read(&p).unwrap();
        for _ in 0..b.len() / 4000 + 1 {
            if b.is_empty() {
                break;
            }
            let i = rng.gen_range(0..b.len());
            b[i] ^= 1 << rng.gen_range(0..8);
        }
        std::fs::write(&p, b).unwrap();
    }
    let t = match PageTree::open(dir.path(), opts(4, 0)) {
        Ok(t) => t,
        Err(_) => return,
    };
    let mut errors = 0;
    for k in &keys {
        match t.prove_at(k, None) {
            Ok((_, root, p, m)) => {
                // Nëse jep përgjigje, ajo duhet të jetë e saktë.
                if root == mem.root() {
                    assert_eq!(m.as_ref(), mem.get(k));
                    assert_ne!(verify_tree_key(&root, k, m.as_ref(), &p), Verdict::Invalid);
                }
            }
            Err(_) => errors += 1,
        }
    }
    assert!(errors > 0 || t.root() != mem.root(), "dëmtimi s'u vu re askund");
}

#[test]
fn invalidate_rebuilds_on_next_open() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = PageTree::open(dir.path(), opts(4, 4)).unwrap();
    t.stage_snapshot(1, vec![([1; 32], Some(AccountMeta::default()))]).unwrap();
    t.finish_snapshot(1, true).unwrap();
    t.invalidate("test").unwrap();
    drop(t);
    let t = PageTree::open(dir.path(), opts(4, 4)).unwrap();
    assert!(t.resume_point().is_none());
}

#[test]
fn proofs_during_prepare_see_the_committed_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = StdRng::seed_from_u64(77);
    let keys: Vec<Key> = (0..3000).map(|_| rng.gen()).collect();
    let mut mem = Tree::from_accounts(keys.iter().map(|k| (*k, Some(meta(&mut rng)))).collect());
    let mut t = PageTree::open(dir.path(), opts(0, 4)).unwrap();
    t.stage_snapshot(1, mem_accounts(&mem, &keys)).unwrap();
    t.finish_snapshot(1, true).unwrap();
    for slot in 2..40u64 {
        let ups: Vec<Update> = (0..200).map(|_| (keys[rng.gen_range(0..keys.len())], Some(meta(&mut rng)))).collect();
        let p = t.prepare_slot(slot, ups.clone(), None).unwrap();
        for k in keys.iter().take(100) {
            check_proof(&t, &mem, k, None);
        }
        t.commit_slot(slot, p).unwrap();
        mem.apply_batch(ups);
        for k in keys.iter().take(100) {
            check_proof(&t, &mem, k, None);
        }
    }
}

// ---------------------------------------------------------------- testet e rënda

/// Dëftesat nga 4 thread-e njëherësh, ndërsa slotet zbatohen si në shërbim (prepare nën kyçin e
/// leximit, commit nën kyçin e shkrimit). Çdo dëftesë duhet të jetë e saktë për root-in që jep.
#[test]
fn concurrent_readers_during_slots() {
    use std::collections::HashMap;
    use std::sync::{atomic::AtomicBool, Arc, Mutex, RwLock};
    let dir = tempfile::tempdir().unwrap();
    let mut rng = StdRng::seed_from_u64(91);
    let keys: Vec<Key> = (0..6000).map(|_| rng.gen()).collect();
    let mut mem = Tree::from_accounts(keys.iter().map(|k| (*k, Some(meta(&mut rng)))).collect());
    let mut t = PageTree::open(dir.path(), opts(4, 4)).unwrap();
    t.stage_snapshot(1, mem_accounts(&mem, &keys)).unwrap();
    t.finish_snapshot(1, true).unwrap();
    let tree = Arc::new(RwLock::new(t));
    let models: Arc<Mutex<HashMap<[u8; 32], Tree>>> = Arc::new(Mutex::new(HashMap::from([(mem.root(), mem.clone())])));
    let stop = Arc::new(AtomicBool::new(false));
    let keys = Arc::new(keys);
    let readers: Vec<_> = (0..4)
        .map(|r| {
            let (tree, models, stop, keys) = (tree.clone(), models.clone(), stop.clone(), keys.clone());
            std::thread::spawn(move || {
                let mut rng = StdRng::seed_from_u64(1000 + r);
                let mut n = 0u64;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let k = if rng.gen_bool(0.9) { keys[rng.gen_range(0..keys.len())] } else { rng.gen() };
                    let (_, root, p, m) = tree.read().unwrap().prove_at(&k, None).unwrap();
                    let want = models.lock().unwrap().get(&root).cloned().expect("root i panjohur");
                    assert_eq!(m.as_ref(), want.get(&k));
                    assert_eq!(p, want.prove(&k).0);
                    n += 1;
                }
                n
            })
        })
        .collect();
    for slot in 2..120u64 {
        let ups: Vec<Update> = (0..rng.gen_range(1..800))
            .map(|_| (keys[rng.gen_range(0..keys.len())], if rng.gen_bool(0.05) { None } else { Some(meta(&mut rng)) }))
            .collect();
        mem.apply_batch(ups.clone());
        models.lock().unwrap().insert(mem.root(), mem.clone());
        let p = tree.read().unwrap().prepare_slot(slot, ups, None).unwrap();
        tree.write().unwrap().commit_slot(slot, p).unwrap();
        assert_eq!(tree.read().unwrap().root(), mem.root());
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let total: u64 = readers.into_iter().map(|h| h.join().unwrap()).sum();
    assert!(total > 1000, "pak dëftesa u kontrolluan: {total}");
    tree.read().unwrap().verify_integrity().unwrap();
}

/// Çelësa që ndajnë 240 bit-ët e parë: pema shkon në fund të çelësit (faqe deri në thellësinë 256).
#[test]
fn clustered_keys_deep_pages() {
    std::thread::Builder::new().stack_size(64 << 20).spawn(clustered_body).unwrap().join().unwrap();
}

fn clustered_body() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = StdRng::seed_from_u64(92);
    let prefix: [u8; 32] = rng.gen();
    let near = |rng: &mut StdRng| {
        let mut k = prefix;
        k[30] = rng.gen();
        k[31] = rng.gen();
        k
    };
    let keys: Vec<Key> = (0..3000).map(|_| near(&mut rng)).collect();
    let mut mem = Tree::from_accounts(keys.iter().map(|k| (*k, Some(meta(&mut rng)))).collect());
    let mut t = PageTree::open(dir.path(), opts(4, 8)).unwrap();
    t.stage_snapshot(1, mem_accounts(&mem, &keys)).unwrap();
    t.finish_snapshot(1, true).unwrap();
    assert_eq!(t.root(), mem.root());
    for slot in 2..60u64 {
        let ups: Vec<Update> =
            (0..rng.gen_range(1..400)).map(|_| (near(&mut rng), if rng.gen_bool(0.3) { None } else { Some(meta(&mut rng)) })).collect();
        t.apply_slot(slot, ups.clone(), None).unwrap();
        mem.apply_batch(ups);
        assert_eq!(t.root(), mem.root(), "slot {slot}");
        if slot % 15 == 0 {
            drop(t);
            t = PageTree::open(dir.path(), opts(4, 8)).unwrap();
        }
        for _ in 0..30 {
            let k = near(&mut rng);
            check_proof(&t, &mem, &k, None);
        }
    }
    t.verify_integrity().unwrap();
}

/// 12 farë të ndryshme: PageTree dhe pema në RAM japin të njëjtat root-e, dëftesa (edhe historike,
/// të verifikuara) dhe numra, me snapshot-e të rinj në mes dhe fshirje të gjithçkaje. (Deri te R6
/// krahasohej edhe me motorin RocksDB, që u hoq.)
#[test]
fn differential_many_seeds() {
    for seed in 0..12u64 {
        let da = tempfile::tempdir().unwrap();
        let mut rng = StdRng::seed_from_u64(500 + seed);
        let h = [0u64, 2, 8][seed as usize % 3];
        let cd = [0u16, 4, 8, 12][seed as usize % 4];
        let mut o = opts(h, cd);
        o.ckpt_every = rng.gen_range(1..30);
        o.reuse_slack = seed % 3;
        let mut a = PageTree::open(da.path(), o).unwrap();
        let mut keys = Vec::new();
        let mut mem = Tree::new();
        let mut past = BTreeMap::new();
        let mut slot = 0u64;
        for round in 0..80 {
            slot += rng.gen_range(1..3);
            if round % 37 == 0 {
                // Snapshot i ri: gjendja e vjetër hidhet.
                let n = rng.gen_range(0..2000);
                let snap: Vec<Update> = random_batch(&mut rng, &mut keys, n).into_iter().filter(|(_, m)| m.is_some()).collect();
                a.stage_snapshot(slot, snap.clone()).unwrap();
                a.finish_snapshot(slot, true).unwrap();
                mem = Tree::from_accounts(snap);
                past.clear();
            } else if round % 23 == 0 {
                // Fshi gjithçka.
                let all: Vec<Update> = keys.iter().map(|k| (*k, None)).collect();
                a.apply_slot(slot, all.clone(), None).unwrap();
                mem.apply_batch(all);
                assert_eq!(a.root(), EMPTY);
            } else {
                let n = if rng.gen_bool(0.1) { rng.gen_range(600..3000) } else { rng.gen_range(0..200) };
                let batch = random_batch(&mut rng, &mut keys, n);
                a.apply_slot(slot, batch.clone(), None).unwrap();
                mem.apply_batch(batch);
            }
            past.insert(slot, mem.clone());
            assert_eq!(a.root(), mem.root(), "seed {seed} round {round}");
            assert_eq!(a.len(), mem.len());
            let (lo, hi) = a.provable_range().unwrap();
            for _ in 0..10 {
                let k = if keys.is_empty() || rng.gen_bool(0.2) { rng.gen() } else { keys[rng.gen_range(0..keys.len())] };
                let v = rng.gen_range(lo..=hi);
                let (_, want) = past.range(..=v).next_back().unwrap();
                check_proof(&a, want, &k, Some(v));
            }
        }
        a.verify_integrity().unwrap();
    }
}

/// Ngarkesë e gjatë: 50 000 llogari, 400 slote, rihapje të shpeshta dhe rrëzime; hapësira mbetet e kufizuar.
#[test]
fn crash_long_churn_with_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = StdRng::seed_from_u64(93);
    let mut keys: Vec<Key> = (0..50_000).map(|_| rng.gen()).collect();
    let mut mem = Tree::from_accounts(keys.iter().map(|k| (*k, Some(meta(&mut rng)))).collect());
    let mut o = opts(8, 8);
    o.ckpt_every = 40;
    let mut t = PageTree::open(dir.path(), o.clone()).unwrap();
    t.stage_snapshot(1, mem_accounts(&mem, &keys)).unwrap();
    t.finish_snapshot(1, true).unwrap();
    let total = |t: &PageTree| t.page_stats().blocks;
    let mut sizes = Vec::new();
    for slot in 2..400u64 {
        let n = rng.gen_range(100..1500);
        let ups = random_batch(&mut rng, &mut keys, n);
        match slot % 50 {
            7 => {
                drop(t.prepare_slot(slot, ups, None).unwrap());
                drop(t);
                t = PageTree::open(dir.path(), o.clone()).unwrap();
            }
            21 => {
                drop(t);
                t = PageTree::open(dir.path(), o.clone()).unwrap();
            }
            _ => {
                t.apply_slot(slot, ups.clone(), None).unwrap();
                mem.apply_batch(ups);
            }
        }
        assert_eq!(t.root(), mem.root(), "slot {slot}");
        sizes.push(total(&t) as f64 / mem.len().max(1) as f64);
    }
    for k in keys.iter().step_by(101) {
        check_proof(&t, &mem, k, None);
    }
    t.verify_integrity().unwrap();
    let (early, late) = (sizes[60], *sizes.last().unwrap());
    assert!(late < early * 1.3, "faqe për llogari rriten: {early:.3} → {late:.3}");
}

/// Ikje rryme: humbin `slack + 1` rekordet e fundit (edhe me boshllëqe mes sloteve dhe pa
/// histori). Versioni para tyre duhet të jetë i plotë, dhe pema vazhdon saktë prej tij.
#[test]
fn unsynced_loss_of_last_records_within_slack() {
    for slack in [0u64, 1, 3] {
        let src = tempfile::tempdir().unwrap();
        let mut rng = StdRng::seed_from_u64(95 + slack);
        let keys: Vec<Key> = (0..2000).map(|_| rng.gen()).collect();
        let mut mem = Tree::from_accounts(keys.iter().map(|k| (*k, Some(meta(&mut rng)))).collect());
        let mut o = opts(0, 4);
        o.reuse_slack = slack;
        o.ckpt_every = 1_000_000;
        let mut past = BTreeMap::new();
        let mut slot = 1u64;
        {
            let mut t = PageTree::open(src.path(), o.clone()).unwrap();
            t.stage_snapshot(1, mem_accounts(&mem, &keys)).unwrap();
            t.finish_snapshot(1, true).unwrap();
            past.insert(1, mem.clone());
            for _ in 0..30 {
                slot += rng.gen_range(1..5);
                let ups: Vec<Update> = (0..300).map(|_| (keys[rng.gen_range(0..keys.len())], Some(meta(&mut rng)))).collect();
                t.apply_slot(slot, ups.clone(), None).unwrap();
                mem.apply_batch(ups);
                past.insert(slot, mem.clone());
            }
        }
        let log = std::fs::read(src.path().join("meta.log")).unwrap();
        let mut offs = Vec::new();
        let mut at = 0usize;
        while at < log.len() {
            offs.push(at);
            at += 16 + u64::from_le_bytes(log[at + 4..at + 12].try_into().unwrap()) as usize;
        }
        for k in 1..=(slack as usize + 1) {
            let d = tempfile::tempdir().unwrap();
            for f in std::fs::read_dir(src.path()).unwrap() {
                let f = f.unwrap();
                std::fs::copy(f.path(), d.path().join(f.file_name())).unwrap();
            }
            std::fs::write(d.path().join("meta.log"), &log[..offs[offs.len() - k]]).unwrap();
            let mut t = PageTree::open(d.path(), o.clone()).unwrap();
            let v = t.version().unwrap_or_else(|| panic!("slack {slack}, k {k}: gjendja humbi"));
            let mut m = past.range(..=v).next_back().unwrap().1.clone();
            assert_eq!(t.root(), m.root());
            t.verify_integrity().unwrap();
            for s in v + 1..v + 10 {
                let ups: Vec<Update> = (0..300).map(|_| (keys[rng.gen_range(0..keys.len())], Some(meta(&mut rng)))).collect();
                t.apply_slot(s, ups.clone(), None).unwrap();
                m.apply_batch(ups);
                assert_eq!(t.root(), m.root());
            }
            t.verify_integrity().unwrap();
        }
    }
}

/// Hapi 8: faqet e një thellësie ndërtohen paralelisht, por vendosja mbetet sekuenciale. Nga e
/// njëjta gjendje në disk, i njëjti varg slotesh duhet të japë skedarë bajt për bajt të njëjtë me 1
/// ose me shumë fije, dhe root-e të njëjta me pemën në RAM. (Paketimi i ndërtimit nga snapshot-i
/// varet nga radha e fijeve dhe s'është pjesë e këtij testi; root-i i tij është gjithsesi i njëjtë.)
#[test]
fn parallel_page_build_is_byte_identical() {
    fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_file() && e.file_name() != "LOCK" {
                std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
            }
        }
    }
    let base = tempfile::tempdir().unwrap();
    let mut rng = StdRng::seed_from_u64(808);
    let mut keys = Vec::new();
    let snap: Vec<Update> = random_batch(&mut rng, &mut keys, 30_000).into_iter().filter(|(_, m)| m.is_some()).collect();
    {
        let mut t = PageTree::open(base.path(), opts(4, 12)).unwrap();
        t.stage_snapshot(1, snap.clone()).unwrap();
        t.finish_snapshot(1, true).unwrap();
    }
    let batches: Vec<Vec<Update>> = (0..12).map(|_| random_batch(&mut rng, &mut keys, 3000)).collect();
    let run = |threads: usize| -> (Vec<u8>, Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        copy_dir(base.path(), dir.path());
        let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
        pool.install(|| {
            let mut t = PageTree::open(dir.path(), opts(4, 12)).unwrap();
            let mut mem = Tree::from_accounts(snap.clone());
            assert_eq!(t.root(), mem.root());
            for (i, batch) in batches.iter().enumerate() {
                let slot = 2 + i as u64;
                t.apply_slot(slot, batch.clone(), None).unwrap();
                mem.apply_batch(batch.clone());
                assert_eq!(t.root(), mem.root(), "{threads} fije, sloti {slot}");
            }
            t.verify_integrity().unwrap();
            for k in keys.iter().step_by(53) {
                check_proof(&t, &mem, k, None);
            }
        });
        let pages = std::fs::read(dir.path().join("pages.dat")).unwrap();
        let log = std::fs::read(dir.path().join("meta.log")).unwrap();
        (pages, log)
    };
    let one = run(1);
    let many = run(4);
    assert!(one.0 == many.0, "pages.dat ndryshon sipas numrit të fijeve");
    assert!(one.1 == many.1, "meta.log ndryshon sipas numrit të fijeve");
}

/// Ndërtimi nga snapshot-i mban në RAM një numër të kufizuar blloqesh, pavarësisht madhësisë së
/// pemës (Mainnet: ~70M blloqe), dhe pema del e njëjtë me atë në RAM.
#[test]
fn snapshot_build_keeps_a_bounded_number_of_blocks_in_ram() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = StdRng::seed_from_u64(1717);
    let n = 250_000;
    let accounts: Vec<Update> = (0..n).map(|_| (rng.gen(), Some(meta(&mut rng)))).collect();
    let mut t = PageTree::open(dir.path(), opts(4, 20)).unwrap();
    for chunk in accounts.chunks(50_000) {
        t.stage_snapshot(1, chunk.to_vec()).unwrap();
    }
    let root = t.finish_snapshot(1, true).unwrap();
    assert_eq!(root, Tree::from_accounts(accounts.clone()).root());
    let blocks = t.page_stats().blocks as usize;
    let peak = t.snapshot_peak_blocks();
    assert!(blocks > 3 * zilja_store::SNAPSHOT_FLUSH_BLOCKS, "pema duhet të jetë shumë më e madhe se kufiri ({blocks} blloqe)");
    assert!(peak <= zilja_store::SNAPSHOT_FLUSH_BLOCKS + 1, "{peak} blloqe në RAM njëherësh (nga {blocks})");
    t.verify_integrity().unwrap();
    // Pas rihapjes: e njëjta pemë, dëftesat e sakta.
    drop(t);
    let t = PageTree::open(dir.path(), opts(4, 20)).unwrap();
    assert_eq!(t.root(), root);
    let mem = Tree::from_accounts(accounts.clone());
    for (k, _) in accounts.iter().step_by(997) {
        check_proof(&t, &mem, k, None);
    }
}

/// R8: `last_timing` jep fazat e slotit dhe shënon checkpoint-in (çdo `ckpt_every` slote).
#[test]
fn last_timing_reports_phases_and_checkpoints() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = PageTree::open(dir.path(), PageOptions { history: 4, cache_depth: 8, durable: true, ckpt_every: 16, ..Default::default() }).unwrap();
    assert!(t.last_timing().is_none());
    let mut rng = StdRng::seed_from_u64(7);
    let mut keys = Vec::new();
    let (mut ckpts, mut reads) = (Vec::new(), 0u64);
    for slot in 1..=40u64 {
        let ups = random_batch(&mut rng, &mut keys, 200);
        let t0 = std::time::Instant::now();
        let p = t.prepare_slot(slot, ups, None).unwrap();
        t.commit_slot(slot, p).unwrap();
        let wall = t0.elapsed();
        let s = t.last_timing().expect("matjet pas commit-it");
        assert!(s.read + s.tree + s.build + s.pwrite + s.fsync + s.commit <= wall, "fazat s'kalojnë kohën e slotit");
        assert!(s.pages > 0);
        reads += s.disk_reads;
        if s.checkpoint {
            ckpts.push(slot);
            assert!(dir.path().join("meta.ckpt").exists());
        }
    }
    assert_eq!(ckpts, vec![16, 32], "checkpoint çdo 16 slote");
    assert!(reads > 0, "faqet e thella lexohen nga disku (cache_depth 8)");
}
