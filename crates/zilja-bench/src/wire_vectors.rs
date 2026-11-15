//! Vektorë të formatit në rrjet (Safer-4 i auditimit): përgjigje të vërteta të
//! `getAccountProof` nga shërbimi, plus versione të prishura të tyre. Çdo verifikues
//! (Rust/CLI, WASM, JS) duhet të japë vendimin `expect` për secilin.
//!
//! `cargo run -p zilja-bench --bin zilja-wire-vectors > sdk/js/test/wire-vectors.json`

use rand::{rngs::StdRng, Rng, SeedableRng};
use serde_json::{json, Value};
use zilja_node::rpc::handle_jsonrpc;
use zilja_node::{Event, Service};
use zilja_verifier::wire::{parse_u64_dec, MAX_SAFE_JSON_INT};
use zilja_verifier::{verify_account, AccountMeta, Key, Proof, Verdict};

/// Rregulli kanonik: lexon `account` sipas `zilja_verifier::wire`; çdo shkelje = "invalid".
fn expect(resp: &Value) -> &'static str {
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
        let pk: Key = bs58::decode(resp["pubkey"].as_str()?).into_vec().ok()?.try_into().ok()?;
        let proof_hex = resp["proof"].as_str()?;
        if proof_hex.len() % 2 != 0 || !proof_hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let proof = Proof::from_bytes(&hex::decode(proof_hex).ok()?).ok()?;
        let a = &resp["account"];
        let meta = if a.is_null() {
            None
        } else {
            Some(AccountMeta {
                lamports: u(&a["lamports"])?,
                owner: bs58::decode(a["owner"].as_str()?).into_vec().ok()?.try_into().ok()?,
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

fn main() {
    let mut rng = StdRng::seed_from_u64(1115);
    let mut svc = Service::start_opts(Default::default()).unwrap();
    let mut keys = Vec::new();
    for i in 0..60u64 {
        let k: Key = rng.gen();
        let owner = [rng.gen_range(0..3u8); 32];
        let data: Vec<u8> = (0..rng.gen_range(0..80)).map(|_| rng.gen()).collect();
        // Vlera të mëdha (mbi 2^53 dhe afër 2^64) që JSON-i i rrumbullakon si numër.
        let lamports = match i % 4 {
            0 => u64::MAX - i,
            1 => (1 << 53) + 1 + i,
            _ => rng.gen_range(1..1 << 40),
        };
        let m = AccountMeta::from_account(lamports, &owner, i % 5 == 0, &data);
        svc.send(Event::Snapshot { slot: 10, pubkey: k, meta: Some(m) });
        keys.push(k);
    }
    svc.send(Event::EndOfStartup);
    let shared = svc.shared.clone();
    svc.shutdown();

    let get = |k: &Key| -> Value {
        let pk = bs58::encode(k).into_string();
        let r = handle_jsonrpc(&shared, &format!(r#"{{"id":1,"method":"getAccountProof","params":["{pk}"]}}"#));
        r["result"].clone()
    };
    let mut cases = Vec::new();
    let mut push = |name: &str, resp: Value| {
        let e = expect(&resp);
        cases.push(json!({"name": name, "response": resp, "expect": e}));
    };

    for k in keys.iter().take(12) {
        push("honest_inclusion", get(k));
    }
    for _ in 0..4 {
        push("honest_exclusion", get(&rng.gen()));
    }
    let two64 = 1u128 << 64;
    for (i, k) in keys.iter().take(20).enumerate() {
        let r = get(k);
        let a = &r["account"];
        let lam: u128 = a["lamports"].as_str().unwrap().parse().unwrap();
        let dl: u128 = a["dataLen"].as_str().unwrap().parse().unwrap();
        let with = |f: &str, v: Value| {
            let mut r2 = r.clone();
            r2["account"][f] = v;
            r2
        };
        let (name, resp) = match i % 10 {
            0 => ("lamports_plus_2^64", with("lamports", json!((lam + two64).to_string()))),
            1 => ("lamports_negative", with("lamports", json!(format!("-{}", two64 - lam)))),
            2 => ("dataLen_plus_2^64", with("dataLen", json!((dl + two64).to_string()))),
            3 => ("executable_as_string", with("executable", json!(r["account"]["executable"].as_bool().unwrap().to_string()))),
            4 => ("lamports_json_number", with("lamports", serde_json::from_str(a["lamports"].as_str().unwrap()).unwrap())),
            5 => ("lamports_leading_zero", with("lamports", json!(format!("0{lam}")))),
            6 => ("dataHash_junk", {
                let mut h = a["dataHash"].as_str().unwrap().to_string();
                h.replace_range(0..1, "g");
                with("dataHash", json!(h))
            }),
            7 => ("dataLen_json_number", with("dataLen", serde_json::from_str(a["dataLen"].as_str().unwrap()).unwrap())),
            8 => ("root_junk", {
                let mut r2 = r.clone();
                let mut h = r["root"].as_str().unwrap().to_string();
                h.replace_range(1..2, "g");
                r2["root"] = json!(h);
                r2
            }),
            _ => ("lamports_off_by_one", with("lamports", json!((lam ^ 1).to_string()))),
        };
        push(name, resp);
    }
    let n = cases.len();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "about": "Përgjigje të vërteta të getAccountProof dhe versione të prishura; rregullat: zilja_verifier::wire",
            "cases": cases,
        }))
        .unwrap()
    );
    eprintln!("{n} raste");
}
