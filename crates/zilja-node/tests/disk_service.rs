//! Shërbimi me pemën në disk: rinisja pa snapshot, dëftesat historike, zinxhiri i gabuar.

use serde_json::Value;
use zilja_node::{rpc::handle_jsonrpc, Event, Options, Service, Shared, Status};
use zilja_store::DiskTree;
use zilja_verifier::{verify_account, AccountMeta, Proof, Verdict};

fn m(l: u64) -> Option<AccountMeta> {
    Some(AccountMeta { lamports: l, ..Default::default() })
}

fn start(dir: &std::path::Path) -> Service {
    let tree = DiskTree::open(dir, 8).unwrap();
    Service::start_opts(Options { tree: Some(Box::new(tree)), ..Default::default() }).unwrap()
}

/// Dërgon slotin `s` (prindi `s-1`) me një ndryshim dhe e rrënjos.
fn slot(svc: &Service, s: u64, key: u8, lamports: u64) {
    let entries = (s.saturating_sub(20)..s).rev().map(|p| (p, [p as u8; 32])).collect();
    svc.send(Event::SlotHashes { slot: s, bank_id: s, entries });
    svc.send(Event::Status { slot: s, parent: Some(s - 1), bank_id: Some(s), status: Status::CreatedBank });
    svc.send(Event::Account { slot: s, bank_id: s, pubkey: [key; 32], write_version: s, meta: m(lamports) });
    svc.send(Event::Status { slot: s, parent: Some(s - 1), bank_id: Some(s), status: Status::Rooted });
}

fn rpc(sh: &Shared, body: &str) -> Value {
    handle_jsonrpc(sh, body)
}

fn proof_ok(sh: &Shared, key: u8, at: Option<u64>) -> (u64, u64) {
    let pk = bs58::encode([key; 32]).into_string();
    let params = match at {
        Some(s) => format!(r#"["{pk}",{{"slot":{s}}}]"#),
        None => format!(r#"["{pk}"]"#),
    };
    let r = rpc(sh, &format!(r#"{{"id":1,"method":"getAccountProof","params":{params}}}"#));
    let res = &r["result"];
    assert!(res.is_object(), "{r}");
    let root: [u8; 32] = hex::decode(res["root"].as_str().unwrap()).unwrap().try_into().unwrap();
    let proof = Proof::from_bytes(&hex::decode(res["proof"].as_str().unwrap()).unwrap()).unwrap();
    let meta = AccountMeta { lamports: res["account"]["lamports"].as_str().unwrap().parse().unwrap(), ..Default::default() };
    assert_eq!(verify_account(&root, &[key; 32], Some(&meta), &proof), Verdict::Exists);
    (res["slot"].as_u64().unwrap(), meta.lamports)
}

#[test]
fn resume_history_and_wrong_chain() {
    let dir = tempfile::tempdir().unwrap();
    let (root_a, root_mem);
    {
        let mut svc = start(dir.path());
        assert!(!svc.resumed);
        svc.send(Event::Snapshot { slot: 10, pubkey: [1; 32], meta: m(100) });
        svc.send(Event::EndOfStartup);
        for s in 11..=20 {
            slot(&svc, s, 2, s * 10);
        }
        let sh = svc.shared.clone();
        svc.shutdown();
        root_a = sh.state.read().unwrap().tree.root();
        // Dëftesë historike: llogaria 2 kishte 150 lamports në slotin 15.
        assert_eq!(proof_ok(&sh, 2, Some(15)), (15, 150));
        assert_eq!(proof_ok(&sh, 2, None), (20, 200));
        // Jashtë kujtesës (8 slote).
        let pk = bs58::encode([2u8; 32]).into_string();
        let r = rpc(&sh, &format!(r#"{{"id":1,"method":"getAccountProof","params":["{pk}",{{"slot":5}}]}}"#));
        assert_eq!(r["error"]["code"], -32602);
    }
    {
        // E njëjta histori në RAM, për krahasim.
        let mut svc = Service::start(None).unwrap();
        svc.send(Event::Snapshot { slot: 10, pubkey: [1; 32], meta: m(100) });
        svc.send(Event::EndOfStartup);
        for s in 11..=25 {
            slot(&svc, s, 2, s * 10);
        }
        let sh = svc.shared.clone();
        svc.shutdown();
        root_mem = sh.state.read().unwrap().tree.root();
    }
    {
        // Rinisja: vazhdon nga disku, pa snapshot; ripërsëritja 15..20 injorohet.
        let mut svc = start(dir.path());
        assert!(svc.resumed);
        assert_eq!(svc.current().1, root_a);
        svc.send(Event::Snapshot { slot: 5, pubkey: [9; 32], meta: m(1) }); // më i vjetër se disku: injorohet
        for s in 15..=25 {
            slot(&svc, s, 2, s * 10);
        }
        let sh = svc.shared.clone();
        svc.shutdown();
        let st = sh.state.read().unwrap();
        assert_eq!(st.slot, 25);
        assert_eq!(st.tree.root(), root_mem, "rinisja nga disku = historia e plotë në RAM");
        assert!(st.broken.is_none());
        drop(st);
        // Pas rinisjes, kujtesa nis nga sloti i rinisjes (20): sloti 22 është brenda.
        assert_eq!(proof_ok(&sh, 2, Some(22)), (22, 220));
    }
    {
        // Zinxhir tjetër: SlotHashes i bankës pas slotit 25 ka hash-e të tjera → ndalon dhe e shënon diskun të pavlefshëm.
        let mut svc = start(dir.path());
        assert!(svc.resumed);
        let entries = (5..26u64).rev().map(|p| (p, [0xEE; 32])).collect();
        svc.send(Event::SlotHashes { slot: 26, bank_id: 26, entries });
        let sh = svc.shared.clone();
        svc.shutdown();
        let r = rpc(&sh, r#"{"id":1,"method":"getZiljaRoot"}"#);
        assert_eq!(r["error"]["code"], -32003, "{r}");
        assert_eq!(sh.metrics.broken.load(std::sync::atomic::Ordering::Relaxed), 1);
        // F-13: asgjë s'u fshi ende; vetëm u shënua.
        assert!(dir.path().join("CURRENT").exists());
    }
    // Nisja tjetër e sheh shënimin dhe e rindërton nga e para (do të vijë snapshot-i).
    let svc = start(dir.path());
    assert!(!svc.resumed);
    drop(svc);
    // Dhe një snapshot më i ri se pema në disk e ndalon vazhdimin.
    {
        let mut svc = start(dir.path());
        svc.send(Event::Snapshot { slot: 10, pubkey: [1; 32], meta: m(100) });
        svc.send(Event::EndOfStartup);
        for s in 11..=12 {
            slot(&svc, s, 2, s * 10);
        }
        svc.shutdown();
    }
    let mut svc = start(dir.path());
    assert!(svc.resumed);
    svc.send(Event::Snapshot { slot: 40, pubkey: [1; 32], meta: m(100) });
    let sh = svc.shared.clone();
    svc.shutdown();
    assert_eq!(sh.metrics.broken.load(std::sync::atomic::Ordering::Relaxed), 1);
}

/// F-13: një gjendje me version të panjohur refuzohet, nuk fshihet.
#[test]
fn unknown_state_version_is_refused_not_wiped() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut t = DiskTree::open(dir.path(), 8).unwrap();
        use zilja_tree::StateTree;
        t.stage_snapshot(1, vec![([1; 32], m(5))]).unwrap();
        t.finish_snapshot(1, true).unwrap();
    }
    {
        let cfs = ["nodes", "pages", "nodes_hist", "staging", "meta"];
        let mut o = rocksdb::Options::default();
        o.set_atomic_flush(true);
        let db = rocksdb::DB::open_cf(&o, dir.path(), cfs).unwrap();
        let cf = db.cf_handle("meta").unwrap();
        let mut v = db.get_cf(cf, b"state").unwrap().unwrap();
        v[0] = 99;
        db.put_cf(cf, b"state", &v).unwrap();
        db.flush().unwrap();
    }
    assert!(DiskTree::open(dir.path(), 8).is_err());
    assert!(DiskTree::open(dir.path(), 8).is_err(), "gjendja duhet të jetë ende aty");
}
