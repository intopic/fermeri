//! Audit proof-of-concept tests (2026-11-15, commit f59be06).
//!
//! Testet me `#[ignore]` dështojnë me qëllim derisa të rregullohet gjetja e tyre;
//! faza që e rregullon heq `#[ignore]`. `cargo test -- --ignored` i nis të gjitha.
//!
//! Each test asserts the behaviour the brief's invariants promise. They FAIL on
//! f59be06; they are meant to become regression tests once the findings are fixed.
//! The event sequences mirror what Agave v4.3.2 actually sends (see the audit report).

use zilja_node::tracker::{Output, Tracker};
use zilja_node::{Event, Service, Status};
use zilja_tree::Update;
use zilja_verifier::{AccountMeta, Key};

fn meta(l: u64) -> Option<AccountMeta> {
    Some(AccountMeta { lamports: l, ..Default::default() })
}
fn key(b: u8) -> Key {
    [b; 32]
}
fn acc(slot: u64, bank_id: u64, k: u8, wv: u64, l: u64) -> Event {
    Event::Account { slot, bank_id, pubkey: key(k), write_version: wv, meta: meta(l) }
}
fn st(slot: u64, parent: u64, bank_id: u64, s: Status) -> Event {
    Event::Status { slot, parent: Some(parent), bank_id: Some(bank_id), status: s }
}
fn rooted(out: &[Output]) -> Option<(u64, usize, Vec<Update>)> {
    out.iter().find_map(|o| match o {
        Output::Rooted { slot, slots_in_batch, updates, .. } => {
            let mut u = updates.clone();
            u.sort_by_key(|a| a.0);
            Some((*slot, *slots_in_batch, u))
        }
        _ => None,
    })
}

/// F-01. Validator boots from a snapshot at slot 10 and replays its local ledger
/// (slots 11..=13) during startup. Agave's blockstore_processor sends the account
/// writes of those banks but NO CreatedBank / Rooted statuses for them. The first
/// status the plugin sees for that range is `Rooted(13, parent = 12)`.
/// Expected: slots 11, 12, 13 are all applied (or the node halts). Actual: 11 is dropped.
#[test]
fn f01_startup_ledger_replay_must_not_drop_slots() {
    let mut t = Tracker::new();
    t.handle(Event::Snapshot { slot: 10, pubkey: key(1), meta: meta(1) });
    t.handle(Event::EndOfStartup);
    // Startup replay: account writes only, each bank known only by its bank_id.
    t.handle(acc(11, 101, 0xA1, 1, 111)); // last touched in slot 11
    t.handle(acc(12, 102, 0xA2, 2, 222));
    t.handle(acc(13, 103, 0xA3, 3, 333));
    // Replay stage takes over: first new bank, then the first rooted chain [13, ...].
    t.handle(st(14, 13, 104, Status::CreatedBank));
    let out = t.handle(st(13, 12, 103, Status::Rooted));
    let broken = out.iter().any(|o| matches!(o, Output::Broken(_)));
    let got = rooted(&out);
    let applied_11 = got.as_ref().is_some_and(|(_, _, u)| u.iter().any(|(k, _)| *k == key(0xA1)));
    assert!(
        broken || applied_11,
        "slot 11 was silently skipped: tracker emitted {got:?} and did not halt"
    );
}

/// F-01 (resume variant, breaks invariant 4 "gap safety"). Disk tree is at slot 50.
/// The validator restarts from a snapshot at slot 60 and replays 61..=70 at startup.
/// Slots 51..=60 can never be delivered, so the node must halt. Actual: it applies
/// slots 69 and 70 on top of the slot-50 tree and keeps serving and signing.
#[test]
fn f01_resume_gap_hidden_by_startup_replayed_banks() {
    let mut t = Tracker::new();
    t.resume(50, None, true);
    for s in 61..=70u64 {
        t.handle(acc(s, 1000 + s, s as u8, s, s));
    }
    t.handle(st(71, 70, 1071, Status::CreatedBank));
    let out = t.handle(st(70, 69, 1070, Status::Rooted));
    assert!(
        out.iter().any(|o| matches!(o, Output::Broken(_))),
        "gap 51..=68 not detected; tracker emitted {:?}",
        rooted(&out).map(|(s, n, u)| (s, n, u.len()))
    );
}

/// F-02. Agave notifies EVERY stored version of an account at startup (one per
/// storage/slot, storages scanned in parallel, arbitrary order). The newest slot
/// must win. Here the old version (slot 10) happens to arrive after the new one (slot 20).
#[test]
fn f02_snapshot_duplicates_newest_slot_must_win() {
    let mut svc = Service::start_opts(Default::default()).unwrap();
    let k = key(7);
    svc.send(Event::Snapshot { slot: 20, pubkey: k, meta: meta(200) }); // current version
    svc.send(Event::Snapshot { slot: 10, pubkey: k, meta: meta(100) }); // stale version, arrives later
    // An account closed in slot 20 whose stale slot-10 copy arrives later must stay absent.
    svc.send(Event::Snapshot { slot: 20, pubkey: key(8), meta: None });
    svc.send(Event::Snapshot { slot: 10, pubkey: key(8), meta: meta(5) });
    svc.send(Event::EndOfStartup);
    let sh = svc.shared.clone();
    svc.shutdown();
    let s = sh.state.read().unwrap();
    // (Faza 4: pema renditet sipas tree_key(pubkey).)
    let (_, _, _, m) = s.tree.prove_at(&zilja_verifier::tree_key(&k), None).unwrap();
    let (_, _, _, closed) = s.tree.prove_at(&zilja_verifier::tree_key(&key(8)), None).unwrap();
    assert_eq!(m.map(|m| m.lamports), Some(200), "stale snapshot version won");
    assert!(closed.is_none(), "closed account resurrected from a stale snapshot version");
}

/// F-05. After `Broken`, the tracker keeps buffering every account write forever
/// (Rooted is ignored, so nothing is ever pruned) inside the validator process.
#[test]
fn f05_no_unbounded_buffering_after_broken() {
    let mut t = Tracker::new();
    // (Faza 1: Event::Block u hoq; zinxhiri i gabuar tani zbulohet nga SlotHashes.)
    t.resume(50, Some((50, [0x50; 32])), true);
    let out = t.handle(Event::SlotHashes { slot: 51, bank_id: 51, entries: vec![(50, [0xEE; 32]), (49, [0xEE; 32])] });
    assert!(matches!(out[0], Output::Broken(_)));
    for s in 51..2051u64 {
        t.handle(st(s, s - 1, s, Status::CreatedBank));
        t.handle(acc(s, s, 1, s, 1));
        t.handle(st(s, s - 1, s, Status::Rooted));
    }
    assert!(t.pending_banks() < 100, "{} banks buffered after Broken", t.pending_banks());
}

/// F-06. The RPC loop handles one request at a time and reads the body with no size
/// limit and no deadline. One client that announces a body and never sends it blocks
/// every other request, including /health and /metrics.
#[test]
fn f06_one_stalled_client_must_not_block_health() {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;
    let svc = Service::start_opts(Default::default()).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let bind = format!("127.0.0.1:{port}");
    let _h = zilja_node::rpc::serve(&bind, svc.shared.clone()).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let get_health = || -> bool {
        let mut s = TcpStream::connect(&bind).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        s.write_all(b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
        let mut buf = [0u8; 64];
        matches!(s.read(&mut buf), Ok(n) if n > 0)
    };
    assert!(get_health(), "sanity: /health answers before the attack");
    let mut slow = TcpStream::connect(&bind).unwrap();
    slow.write_all(b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 1000000000\r\n\r\n{").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(get_health(), "/health did not answer within 3 s while one idle POST was open");
}
