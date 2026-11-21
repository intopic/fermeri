//! P-11: `zilja-check` dhe `zilja-quorum` kundër një serveri JSON-RPC të simuluar. Kodi i daljes duhet
//! të jetë dështim kur dështon një kontroll që kërkoi përdoruesi, dhe vendimi i njëjtë me SDK-në.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use zilja_tree::Tree;
use zilja_verifier::{tree_key, AccountMeta};

/// Server që i përgjigjet çdo metode me vlerën e dhënë (ose gabim kur s'ka).
fn serve(answers: HashMap<&'static str, Value>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let body = loop {
                let n = s.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break None;
                }
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(i) = text.find("\r\n\r\n") {
                    let len: usize = text[..i]
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap()))
                        .unwrap_or(0);
                    if buf.len() >= i + 4 + len {
                        break Some(text[i + 4..i + 4 + len].to_string());
                    }
                }
            };
            let Some(body) = body else { continue };
            let req: Value = serde_json::from_str(&body).unwrap();
            let resp = match answers.get(req["method"].as_str().unwrap()) {
                Some(v) => json!({"jsonrpc": "2.0", "id": 1, "result": v}),
                None => json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32601, "message": "s'ka"}}),
            };
            let out = resp.to_string();
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}",
                out.len()
            );
        }
    });
    url
}

const PK: [u8; 32] = [7; 32];

fn meta() -> AccountMeta {
    AccountMeta { lamports: 5_000, owner: [1; 32], executable: false, data_len: 3, data_hash: zilja_verifier::sha256(b"abc") }
}

/// Përgjigja e getAccountProof: `present` = llogaria ekziston; `complete` = pema e plotë.
fn proof_answer(present: bool, complete: bool, pubkey: [u8; 32]) -> Value {
    let mut accs = vec![(tree_key(&[9; 32]), Some(AccountMeta::default()))];
    if present {
        accs.push((tree_key(&PK), Some(meta())));
    }
    let t = Tree::from_accounts(accs);
    let (p, m) = t.prove(&tree_key(&PK));
    let b = p.to_bytes();
    json!({
        "slot": 100, "root": hex::encode(t.root()), "complete": complete,
        "pubkey": bs58::encode(pubkey).into_string(),
        "account": m.map(|m| json!({"lamports": m.lamports.to_string(), "owner": bs58::encode(m.owner).into_string(),
            "executable": m.executable, "dataLen": m.data_len.to_string(), "dataHash": hex::encode(m.data_hash)})),
        "proof": hex::encode(&b), "proofBytes": b.len(),
    })
}

fn check(args: &[&str]) -> i32 {
    let o = Command::new(env!("CARGO_BIN_EXE_zilja-check")).args(args).output().unwrap();
    eprintln!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    o.status.code().unwrap_or(-1)
}

fn zile(present: bool, complete: bool, pubkey: [u8; 32]) -> String {
    let p = proof_answer(present, complete, pubkey);
    let root = json!({"slot": 100, "root": p["root"]});
    serve(HashMap::from([("getAccountProof", p), ("getZiljaRoot", root)]))
}

fn pk() -> String {
    bs58::encode(PK).into_string()
}

#[test]
fn single_endpoint_is_not_enough_by_default() {
    // Si SDK-ja: parazgjedhja është "agreement" (të paktën 2 zile me të njëjtin root).
    assert_eq!(check(&[&zile(true, true, PK), &pk()]), 1, "një zile e vetme kaloi me parazgjedhjen");
    assert_eq!(check(&[&zile(true, true, PK), &pk(), "--min-trust", "single"]), 0);
}

#[test]
fn absence_from_incomplete_tree_fails() {
    assert_eq!(check(&[&zile(false, false, PK), &pk(), "--min-trust", "single"]), 1, "'nuk ekziston' nga pemë jo e plotë u pranua");
    assert_eq!(check(&[&zile(false, true, PK), &pk(), "--min-trust", "single"]), 0);
}

#[test]
fn answer_for_another_account_fails() {
    assert_eq!(check(&[&zile(true, true, [8; 32]), &pk(), "--min-trust", "single"]), 1, "dëftesë për llogari tjetër u pranua");
}

#[test]
fn requested_data_check_failure_is_an_exit_failure() {
    let sol = serve(HashMap::from([
        ("getAccountInfo", json!({"context": {"slot": 100}, "value": {"lamports": 1, "data": ["AAAA", "base64"]}})),
        ("getSlot", json!(100)),
    ]));
    assert_eq!(check(&[&zile(true, true, PK), &pk(), &sol, "--min-trust", "single"]), 1, "mospërputhja e të dhënave doli me sukses");
}

#[test]
fn agreement_of_two_endpoints_passes() {
    let a = zile(true, true, PK);
    // E njëjta pemë, pra i njëjti root.
    let b = zile(true, true, PK);
    assert_eq!(check(&[&a, &pk(), "--also", &b]), 0);
}

#[test]
fn quorum_tool_rejects_malformed_root_without_panicking() {
    let sol = serve(HashMap::from([
        ("getVoteAccounts", json!({"current": [], "delinquent": []})),
        ("getGenesisHash", json!(bs58::encode([1u8; 32]).into_string())),
    ]));
    let z = serve(HashMap::from([(
        "getZiljaAttestation",
        json!({"slot": 5, "root": "ab", "validator": bs58::encode([2u8; 32]).into_string(), "signature": "00".repeat(64)}),
    )]));
    let o = Command::new(env!("CARGO_BIN_EXE_zilja-quorum")).args([&sol, &z]).output().unwrap();
    let code = o.status.code().unwrap_or(-1);
    assert!(code == 1 || code == 2, "zilja-quorum ra (kodi {code}): {}", String::from_utf8_lossy(&o.stderr));
}

/// Një validator (stake 100) që firmos root-in `[4; 32]` në slotin 5, dhe një zile që e shërben.
fn signed_setup(epoch_info: Value, stake: Value) -> (String, String, [u8; 32]) {
    use ed25519_dalek::{Signer, SigningKey};
    let key = SigningKey::from_bytes(&[5u8; 32]);
    let genesis = [1u8; 32];
    let root = [4u8; 32];
    let sig = key.sign(&zilja_verifier::attest::attest_message(&genesis, 5, &root)).to_bytes();
    let id = bs58::encode(key.verifying_key().to_bytes()).into_string();
    let sol = serve(HashMap::from([
        ("getVoteAccounts", json!({"current": [{"nodePubkey": id, "votePubkey": id, "activatedStake": stake, "epochVoteAccount": true}], "delinquent": []})),
        ("getEpochInfo", epoch_info),
        ("getGenesisHash", json!(bs58::encode(genesis).into_string())),
    ]));
    let z = serve(HashMap::from([("getZiljaAttestation", json!({"slot": 5, "root": hex::encode(root), "validator": id, "signature": hex::encode(sig)}))]));
    (sol, z, root)
}

fn quorum(sol: &str, z: &str) -> i32 {
    let o = Command::new(env!("CARGO_BIN_EXE_zilja-quorum")).args([sol, z]).output().unwrap();
    eprintln!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    o.status.code().unwrap_or(-1)
}

#[test]
fn quorum_counts_only_inside_the_stake_tables_epoch() {
    // A10: epoka e tabelës (slotet 0–99) përmban slotin 5 → kuorumi arrihet.
    let (sol, z, _) = signed_setup(json!({"epoch": 0, "absoluteSlot": 50, "slotIndex": 50, "slotsInEpoch": 100}), json!(100));
    assert_eq!(quorum(&sol, &z), 0, "rasti i rregullt duhet të kalojë");
    // Tabela është e epokës 3 (slotet 990–1089): sloti 5 s'mbulohet, kuorumi s'numërohet.
    let (sol, z, _) = signed_setup(json!({"epoch": 3, "absoluteSlot": 1000, "slotIndex": 10, "slotsInEpoch": 100}), json!(100));
    assert_eq!(quorum(&sol, &z), 1, "kuorumi u numërua me tabelën e një epoke tjetër");
}

#[test]
fn quorum_rejects_inexact_stake_numbers() {
    // A11: activatedStake jo-u64 (thyesë, tekst) s'bëhet 0 në heshtje: gabim.
    let ei = json!({"epoch": 0, "absoluteSlot": 50, "slotIndex": 50, "slotsInEpoch": 100});
    for bad in [json!(1.5), json!("100"), json!(-1)] {
        let (sol, z, _) = signed_setup(ei.clone(), bad.clone());
        assert_eq!(quorum(&sol, &z), 2, "stake {bad} u pranua");
    }
}

#[test]
fn requested_data_check_that_did_not_run_fails() {
    // A13: RPC-ja Solana jep slotin, por s'i përgjigjet getAccountInfo: kontrolli i kërkuar s'u krye.
    let sol = serve(HashMap::from([("getSlot", json!(100))]));
    assert_eq!(check(&[&zile(true, true, PK), &pk(), &sol, "--min-trust", "single"]), 1, "krahasimi që s'u krye doli si sukses");
}

#[test]
fn slot_ahead_of_finalized_reference_is_not_fresh() {
    // A12: zilja në slotin 100, RPC-ja e finalizuar në 99. Të dhënat përputhen (abc, 5000 lamports).
    let info = json!({"context": {"slot": 99}, "value": {"lamports": 5000, "owner": bs58::encode([1u8; 32]).into_string(), "executable": false, "data": ["YWJj", "base64"]}});
    let ok_sol = serve(HashMap::from([("getSlot", json!(100)), ("getAccountInfo", info.clone())]));
    assert_eq!(check(&[&zile(true, true, PK), &pk(), &ok_sol, "--min-trust", "single"]), 0, "rasti i rregullt duhet të kalojë");
    let sol = serve(HashMap::from([("getSlot", json!(99)), ("getAccountInfo", info)]));
    assert_eq!(check(&[&zile(true, true, PK), &pk(), &sol, "--min-trust", "single"]), 1, "slot mbi referencën u shpall i freskët");
}

/// G22: zilja-authorize pranon vetëm keypair 64 bajt me çelësin publik të saktë.
#[test]
fn authorize_refuses_short_or_mismatched_keypairs() {
    let dir = tempfile::tempdir().unwrap();
    let id = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
    let signer = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
    let write = |name: &str, bytes: &[u8]| {
        let p = dir.path().join(name);
        std::fs::write(&p, serde_json::to_string(bytes).unwrap()).unwrap();
        p
    };
    let full = |k: &ed25519_dalek::SigningKey| [k.to_bytes().to_vec(), k.verifying_key().to_bytes().to_vec()].concat();
    let s = write("signer.json", &full(&signer));
    let g = bs58::encode([0x33u8; 32]).into_string();
    let run = |idp: &std::path::Path| Command::new(env!("CARGO_BIN_EXE_zilja-authorize")).args([idp.to_str().unwrap(), s.to_str().unwrap(), &g, "10"]).output().unwrap();
    assert!(run(&write("id.json", &full(&id))).status.success());
    let short = run(&write("id32.json", &id.to_bytes()));
    assert!(!short.status.success() && String::from_utf8_lossy(&short.stderr).contains("duhen 64"), "{short:?}");
    let mut bad = full(&id);
    bad[40] ^= 1;
    assert!(!run(&write("idbad.json", &bad)).status.success());
}
