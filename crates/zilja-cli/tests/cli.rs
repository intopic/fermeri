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
