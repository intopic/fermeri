//! Safer-3: fuzz pa nightly. Hyrje të rastësishme dhe mutacione të hyrjeve të vlefshme për:
//!
//! - `Proof::from_bytes` (s'bie kurrë; çdo dëftesë e lexuar rikodohet njësoj);
//! - dekoduesit e diskut: faqe, gjendje, staging (`zilja_store::fuzz_decode`);
//! - dekoduesit e motorit me faqe: faqe, rekorde log-u, checkpoint (`zilja_store::fuzz_decode_pages`);
//! - fuzz diferencial i `PageTree` kundër pemës në RAM: slote të rastësishme, çelësa me prefikse të
//!   gjata, rrëzime mes faqeve dhe rekordit, log i prerë, rihapje, dëftesa historike;
//! - `handle_jsonrpc` me JSON të çfarëdoshëm (kthen gjithmonë `result` ose `error`);
//! - verifikimi i përgjigjeve: shkruan një korpus për testin diferencial me JS
//!   (`sdk/js/test/fuzz-diff.mjs`), ku vendimi i JS duhet të jetë ai i Rust-it.
//!
//! `cargo run --release -p zilja-bench --bin zilja-fuzz -- <sekonda> [korpus.json]`

use rand::{rngs::StdRng, seq::SliceRandom, Rng, SeedableRng};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use zilja_node::rpc::handle_jsonrpc;
use zilja_node::{Event, Service};
use zilja_verifier::wire::{parse_u64_dec, MAX_SAFE_JSON_INT};
use zilja_verifier::{verify_account, AccountMeta, Key, Proof, Verdict};

fn mutate(rng: &mut StdRng, b: &mut Vec<u8>) {
    for _ in 0..rng.gen_range(1..4) {
        match rng.gen_range(0..5) {
            0 if !b.is_empty() => {
                let i = rng.gen_range(0..b.len());
                b[i] ^= 1 << rng.gen_range(0..8);
            }
            1 if !b.is_empty() => {
                let i = rng.gen_range(0..b.len());
                b[i] = rng.gen();
            }
            2 => {
                let i = rng.gen_range(0..=b.len());
                b.insert(i, rng.gen());
            }
            3 if !b.is_empty() => {
                let i = rng.gen_range(0..b.len());
                b.remove(i);
            }
            _ => {
                let n = rng.gen_range(0..=b.len());
                b.truncate(n);
            }
        }
    }
}

/// Rregulli kanonik i Rust-it për një përgjigje (si te zilja-wire-vectors).
fn rust_verdict(resp: &Value) -> &'static str {
    let run = || -> Option<Verdict> {
        let h32 = |v: &Value| -> Option<[u8; 32]> {
            let s = v.as_str()?;
            if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            hex::decode(s).ok()?.try_into().ok()
        };
        let u = |v: &Value| match v {
            Value::String(s) => parse_u64_dec(s),
            Value::Number(n) => n.as_u64().filter(|n| *n <= MAX_SAFE_JSON_INT),
            _ => None,
        };
        let root = h32(&resp["root"])?;
        let pk_s = resp["pubkey"].as_str()?;
        if pk_s.is_empty() || pk_s.len() > 64 {
            return None;
        }
        let pk: Key = bs58::decode(pk_s).into_vec().ok()?.try_into().ok()?;
        let ph = resp["proof"].as_str()?;
        if ph.len() % 2 != 0 || !ph.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let proof = Proof::from_bytes(&hex::decode(ph).ok()?).ok()?;
        let a = &resp["account"];
        let meta = if a.is_null() {
            None
        } else {
            let owner_s = a["owner"].as_str()?;
            if owner_s.is_empty() || owner_s.len() > 64 {
                return None;
            }
            Some(AccountMeta {
                lamports: u(&a["lamports"])?,
                owner: bs58::decode(owner_s).into_vec().ok()?.try_into().ok()?,
                executable: a["executable"].as_bool()?,
                data_len: u(&a["dataLen"])?,
                data_hash: h32(&a["dataHash"])?,
            })
        };
        Some(verify_account(&root, &pk, meta.as_ref(), &proof))
    };
    match run() {
        Some(Verdict::Exists) => "exists",
        Some(Verdict::DoesNotExist) => "does_not_exist",
        _ => "invalid",
    }
}

fn mutate_json(rng: &mut StdRng, v: &mut Value) {
    let fields = ["root", "pubkey", "proof", "account"];
    let acc = ["lamports", "owner", "executable", "dataLen", "dataHash"];
    let pick_val = |rng: &mut StdRng| -> Value {
        match rng.gen_range(0..10) {
            0 => json!(null),
            1 => json!(true),
            2 => json!(rng.gen::<u64>() >> rng.gen_range(0..64)),
            3 => json!(-(rng.gen::<i32>() as i64).abs()),
            4 => json!(format!("{}", rng.gen::<u64>())),
            5 => json!(format!("0{}", rng.gen::<u32>())),
            6 => json!("18446744073709551616"),
            7 => json!(1.5),
            8 => json!(""),
            _ => json!({}),
        }
    };
    match rng.gen_range(0..6) {
        0 => {
            let f = *acc.choose(rng).unwrap();
            if v["account"].is_object() {
                v["account"][f] = pick_val(rng);
            }
        }
        1 => {
            let f = *fields.choose(rng).unwrap();
            if let Some(s) = v[f].as_str().map(str::to_string) {
                let mut b = s.into_bytes();
                mutate(rng, &mut b);
                v[f] = json!(String::from_utf8_lossy(&b).to_string());
            }
        }
        2 => {
            let f = *acc.choose(rng).unwrap();
            if let Some(s) = v["account"][f].as_str().map(str::to_string) {
                let mut b = s.into_bytes();
                mutate(rng, &mut b);
                v["account"][f] = json!(String::from_utf8_lossy(&b).to_string());
            }
        }
        3 => v["account"] = json!(null),
        4 => {
            // Dëftesa bajt pas bajti.
            if let Some(h) = v["proof"].as_str().and_then(|h| hex::decode(h).ok()) {
                let mut b = h;
                mutate(rng, &mut b);
                v["proof"] = json!(hex::encode(b));
            }
        }
        _ => {
            let f = *fields.choose(rng).unwrap();
            v[f] = pick_val(rng);
        }
    }
}

/// Çelës i ri: i rastësishëm, ose fqinj i afërt i një ekzistuesi (prefiks i gjatë, rrugë e thellë).
fn fuzz_key(rng: &mut StdRng, keys: &[Key]) -> Key {
    if keys.is_empty() || rng.gen_bool(0.5) {
        return rng.gen();
    }
    let mut k = *keys.choose(rng).unwrap();
    let b = if rng.gen_bool(0.5) { rng.gen_range(200..256) } else { rng.gen_range(0..256) };
    k[b / 8] ^= 1 << (7 - b % 8);
    k
}

/// Një skenar diferencial: `PageTree` duhet të japë kudo root-in, numrin dhe dëftesat e pemës në RAM.
fn page_scenario(seed: u64) -> u64 {
    use std::collections::BTreeMap;
    use zilja_store::{PageOptions, PageTree};
    use zilja_tree::{StateTree, Tree, Update};
    use zilja_verifier::verify_tree_key;
    let mut rng = StdRng::seed_from_u64(seed);
    let dir = std::env::temp_dir().join(format!("zilja-fuzz-pages-{}-{seed}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let o = PageOptions {
        history: *[0u64, 1, 4, 8].choose(&mut rng).unwrap(),
        cache_depth: *[0u16, 4, 8, 12].choose(&mut rng).unwrap(),
        durable: false,
        sync_every: 0,
        ckpt_every: rng.gen_range(1..20),
        ckpt_log_mb: 64,
        reuse_slack: rng.gen_range(0..4),
        genesis: None,
    };
    let mut t = PageTree::open(&dir, o.clone()).unwrap();
    // Rekordet që i kanë mbijetuar një rihapjeje janë në disk: një ikje rryme e mëvonshme s'i humb.
    let log_seqs = |dir: &std::path::Path| -> Vec<(usize, u64)> {
        let log = std::fs::read(dir.join("meta.log")).unwrap_or_default();
        let mut v = Vec::new();
        let mut at = 0usize;
        while at + 40 <= log.len() {
            v.push((at, u64::from_le_bytes(log[at + 32..at + 40].try_into().unwrap())));
            at += 16 + u64::from_le_bytes(log[at + 4..at + 12].try_into().unwrap()) as usize;
        }
        v
    };
    let mut durable_seq = 0u64;
    let mut keys: Vec<Key> = Vec::new();
    let meta = |rng: &mut StdRng| AccountMeta {
        lamports: rng.gen(),
        owner: rng.gen(),
        executable: rng.gen(),
        data_len: rng.gen_range(0..1 << 20),
        data_hash: rng.gen(),
    };
    let n0 = rng.gen_range(0..600);
    let mut snap: Vec<Update> = Vec::new();
    for _ in 0..n0 {
        let k = fuzz_key(&mut rng, &keys);
        keys.push(k);
        snap.push((k, if rng.gen_bool(0.05) { None } else { Some(meta(&mut rng)) }));
    }
    let base = rng.gen_range(0..1000u64);
    for c in snap.chunks(rng.gen_range(1..200)) {
        t.stage_snapshot(base, c.to_vec()).unwrap();
    }
    t.finish_snapshot(base, true).unwrap();
    durable_seq = durable_seq.max(1);
    let mut mem = Tree::from_accounts(snap);
    assert_eq!(t.root(), mem.root(), "seed {seed}: snapshot");
    let mut past: BTreeMap<u64, Tree> = BTreeMap::from([(base, mem.clone())]);
    let mut slot = base;
    let mut ops = 0;
    for _ in 0..rng.gen_range(5..60) {
        slot += rng.gen_range(1..4);
        let mut b: Vec<Update> = Vec::new();
        // Ndonjëherë grup i madh: kalon pragun ku pema punon paralelisht.
        let n = if rng.gen_bool(0.08) { rng.gen_range(600..2500) } else { rng.gen_range(0..120) };
        for _ in 0..n {
            let k = if keys.is_empty() || rng.gen_bool(0.4) { fuzz_key(&mut rng, &keys) } else { *keys.choose(&mut rng).unwrap() };
            keys.push(k);
            b.push((k, if rng.gen_bool(0.2) { None } else { Some(meta(&mut rng)) }));
        }
        let op = rng.gen_range(0..10);
        if std::env::var_os("ZILJA_PAGES_DEBUG").is_some() {
            eprintln!("op {op} slot {slot} n {} ver {:?} {:?}", b.len(), t.version(), o);
        }
        match op {
            // Rrëzim: faqet shkruhen, rekordi jo. Pas rihapjes, sloti i mëparshëm.
            0 => {
                drop(t.prepare_slot(slot, b, None).unwrap());
                drop(t);
                t = PageTree::open(&dir, o.clone()).unwrap();
                durable_seq = log_seqs(&dir).last().map_or(durable_seq, |x| x.1.max(durable_seq));
            }
            // Ikje rryme: humbin k rekordet e fundit (k ≤ slack + 1, kufiri i garantuar),
            // i fundit ndoshta vetëm pjesërisht. Rihapja duhet të japë slotin e plotë para tyre.
            1 => {
                t.apply_slot(slot, b.clone(), None).unwrap();
                mem.apply_batch(b);
                past.insert(slot, mem.clone());
                drop(t);
                let lp = dir.join("meta.log");
                let log_len = std::fs::metadata(&lp).unwrap().len() as usize;
                let recs: Vec<usize> = log_seqs(&dir).into_iter().filter(|(_, q)| *q > durable_seq).map(|(at, _)| at).collect();
                if !recs.is_empty() {
                    let k = rng.gen_range(1..=recs.len().min((o.reuse_slack + 1) as usize));
                    let start = recs[recs.len() - k];
                    let end = recs.get(recs.len() - k + 1).copied().unwrap_or(log_len);
                    let cut = rng.gen_range(start..end);
                    if std::env::var_os("ZILJA_PAGES_DEBUG").is_some() {
                        eprintln!("  cut: {} rekorde të reja, humbin {k}, cut {cut}/{log_len}", recs.len());
                    }
                    std::fs::OpenOptions::new().write(true).open(&lp).unwrap().set_len(cut as u64).unwrap();
                }
                t = PageTree::open(&dir, o.clone()).unwrap();
                durable_seq = log_seqs(&dir).last().map_or(durable_seq, |x| x.1.max(durable_seq));
                let v = t.version().unwrap_or_else(|| panic!("seed {seed}: gjendja humbi pas prerjes së log-ut"));
                mem = past.range(..=v).next_back().unwrap().1.clone();
                past.retain(|s, _| *s <= v);
                assert_eq!(t.root(), mem.root(), "seed {seed}: prerja e log-ut");
                t.verify_integrity().unwrap();
            }
            2 => {
                drop(t);
                t = PageTree::open(&dir, o.clone()).unwrap();
                durable_seq = log_seqs(&dir).last().map_or(durable_seq, |x| x.1.max(durable_seq));
            }
            // Snapshot i ri në mes: gjendja e vjetër hidhet krejt.
            3 if rng.gen_bool(0.3) => {
                let snap: Vec<Update> = b.into_iter().filter(|(_, m)| m.is_some()).collect();
                for c in snap.chunks(rng.gen_range(1..100)) {
                    t.stage_snapshot(slot, c.to_vec()).unwrap();
                }
                t.finish_snapshot(slot, true).unwrap();
                mem = Tree::from_accounts(snap);
                past = BTreeMap::from([(slot, mem.clone())]);
                durable_seq = 1;
            }
            _ => {
                t.apply_slot(slot, b.clone(), Some((slot, rng.gen()))).unwrap();
                mem.apply_batch(b);
                past.insert(slot, mem.clone());
            }
        }
        ops += 1;
        assert_eq!(t.root(), mem.root(), "seed {seed} slot {slot}");
        assert_eq!(t.len(), mem.len(), "seed {seed} slot {slot}");
        let (lo, hi) = t.provable_range().unwrap();
        for _ in 0..8 {
            let k = if keys.is_empty() || rng.gen_bool(0.2) { rng.gen() } else { *keys.choose(&mut rng).unwrap() };
            let v = rng.gen_range(lo..=hi);
            let (_, want) = past.range(..=v).next_back().unwrap();
            let (_, root, p, m) = t.prove_at(&k, Some(v)).unwrap();
            assert_eq!(root, want.root(), "seed {seed}: root historik");
            assert_eq!(m.as_ref(), want.get(&k), "seed {seed}: meta");
            assert_eq!(p, want.prove(&k).0, "seed {seed}: dëftesa");
            assert_ne!(verify_tree_key(&root, &k, m.as_ref(), &p), Verdict::Invalid);
        }
        if rng.gen_bool(0.1) {
            t.verify_integrity().unwrap();
        }
    }
    t.verify_integrity().unwrap();
    drop(t);
    let _ = std::fs::remove_dir_all(&dir);
    ops
}

fn main() {
    if let Some(seed) = std::env::var("ZILJA_PAGE_SEED").ok().and_then(|s| s.parse().ok()) {
        // Riprodhon një skenar të vetëm të PageTree (për gjetjet e fuzz-it).
        let ops = std::thread::Builder::new().stack_size(64 << 20).spawn(move || page_scenario(seed)).unwrap().join().unwrap();
        println!("skenari {seed}: {ops} slote, pa gjetje");
        return;
    }
    let args: Vec<String> = std::env::args().collect();
    let secs: u64 = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(60);
    let corpus_path = args.get(2).cloned();
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut rng = StdRng::seed_from_u64(std::env::var("ZILJA_FUZZ_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(1));

    // Një shërbim me llogari të vërteta, për përgjigje të vlefshme dhe për handle_jsonrpc.
    let mut svc = Service::start_opts(Default::default()).unwrap();
    let mut keys = Vec::new();
    for i in 0..400u64 {
        let k: Key = rng.gen();
        let lamports = if i % 3 == 0 { u64::MAX - i } else { rng.gen_range(1..1 << 45) };
        let data: Vec<u8> = (0..rng.gen_range(0..40)).map(|_| rng.gen()).collect();
        svc.send(Event::Snapshot {
            slot: 9,
            pubkey: k,
            meta: Some(AccountMeta::from_account(lamports, &[i as u8; 32], i % 4 == 0, &data)),
        });
        keys.push(k);
    }
    svc.send(Event::EndOfStartup);
    let shared = svc.shared.clone();
    svc.shutdown();
    let proof_of = |k: &Key| -> Value {
        let pk = bs58::encode(k).into_string();
        handle_jsonrpc(&shared, &format!(r#"{{"id":1,"method":"getAccountProof","params":["{pk}"]}}"#))["result"].clone()
    };
    let seeds: Vec<Vec<u8>> = zilja_store::fuzz_seeds(&(0..256).map(|_| rng.gen()).collect::<Vec<u8>>());
    let page_seeds: Vec<Vec<u8>> = zilja_store::fuzz_seeds_pages(&(0..256).map(|_| rng.gen()).collect::<Vec<u8>>());
    // Skenarët diferencialë në një thread më vete, me stack të madh (rrugë deri në 256 nivele).
    let page_deadline = deadline;
    let page_seed0: u64 = rng.gen();
    let pages = std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let (mut runs, mut ops) = (0u64, 0u64);
            while Instant::now() < page_deadline {
                ops += page_scenario(page_seed0.wrapping_add(runs));
                runs += 1;
            }
            (runs, ops)
        })
        .unwrap();
    let rpc_seeds = [
        r#"{"jsonrpc":"2.0","id":1,"method":"getAccountProof","params":["11111111111111111111111111111111"]}"#,
        r#"{"jsonrpc":"2.0","id":1,"method":"getAccountProof","params":["11111111111111111111111111111111",{"slot":9}]}"#,
        r#"{"jsonrpc":"2.0","id":1,"method":"getZiljaRoot","params":[9]}"#,
        r#"{"jsonrpc":"2.0","id":1,"method":"getZiljaAttestation","params":[]}"#,
    ];

    let (mut n_proof, mut n_store, mut n_rpc, mut n_diff) = (0u64, 0u64, 0u64, 0u64);
    let mut corpus = Vec::new();
    while Instant::now() < deadline {
        for _ in 0..200 {
            // 1) Proof::from_bytes
            let k = keys.choose(&mut rng).unwrap();
            let mut b = hex::decode(proof_of(k)["proof"].as_str().unwrap()).unwrap();
            if rng.gen_bool(0.1) {
                b = (0..rng.gen_range(0..200)).map(|_| rng.gen()).collect();
            } else {
                mutate(&mut rng, &mut b);
            }
            if let Ok(p) = Proof::from_bytes(&b) {
                assert_eq!(p.to_bytes(), b, "dëftesa nuk rikodohet njësoj: {}", hex::encode(&b));
            }
            n_proof += 1;

            // 2) dekoduesit e diskut
            let mut d = seeds.choose(&mut rng).unwrap().clone();
            mutate(&mut rng, &mut d);
            zilja_store::fuzz_decode(&d);
            let mut d = page_seeds.choose(&mut rng).unwrap().clone();
            if rng.gen_bool(0.05) {
                d = (0..rng.gen_range(0..300)).map(|_| rng.gen()).collect();
            } else {
                mutate(&mut rng, &mut d);
            }
            zilja_store::fuzz_decode_pages(&d);
            n_store += 1;

            // 3) handle_jsonrpc
            let mut r = rpc_seeds.choose(&mut rng).unwrap().as_bytes().to_vec();
            mutate(&mut rng, &mut r);
            let out = handle_jsonrpc(&shared, &String::from_utf8_lossy(&r));
            assert!(out.get("result").is_some() || out.get("error").is_some(), "përgjigje pa result/error: {out}");
            n_rpc += 1;

            // 4) verifikimi i përgjigjeve (dhe korpusi për JS)
            let mut resp = proof_of(k);
            for _ in 0..rng.gen_range(0..3) {
                mutate_json(&mut rng, &mut resp);
            }
            let v = rust_verdict(&resp);
            if corpus_path.is_some() && corpus.len() < 200_000 {
                corpus.push(json!({"response": resp, "expect": v}));
            }
            n_diff += 1;
        }
    }
    let (runs, ops) = pages.join().expect("fuzz-i diferencial i faqeve gjeti një gabim");
    println!("✅ fuzz {secs} s pa gjetje: {n_proof} dëftesa, {n_store} dekodime disku (×2 motorë), {n_rpc} kërkesa RPC, {n_diff} përgjigje, {runs} skenarë PageTree ({ops} slote)");
    if let Some(p) = corpus_path {
        let n = corpus.len();
        std::fs::write(&p, serde_json::to_string(&json!({"cases": corpus})).unwrap()).unwrap();
        println!("korpusi diferencial: {n} raste → {p}");
    }
}
