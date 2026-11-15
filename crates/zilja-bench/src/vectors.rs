//! Gjeneron vektorë testi (JSON) që çdo verifikues tjetër (JS, Swift, Kotlin…)
//! duhet t'i kalojë njësoj si verifikuesi Rust.
//!
//! `cargo run -p zilja-bench --bin zilja-vectors > web/vectors.json`

use rand::{rngs::StdRng, Rng, SeedableRng};
use serde_json::{json, Value};
use zilja_tree::Tree;
use zilja_verifier::{verify_account, AccountMeta, Key, Proof, Verdict};

fn acc_json(m: &AccountMeta) -> Value {
    json!({
        "lamports": m.lamports.to_string(),
        "owner": bs58::encode(m.owner).into_string(),
        "executable": m.executable,
        "dataLen": m.data_len.to_string(),
        "dataHash": hex::encode(m.data_hash),
    })
}

fn case(name: &str, root: &[u8; 32], k: &Key, m: Option<&AccountMeta>, p: &Proof) -> Value {
    let expect = match verify_account(root, k, m, p) {
        Verdict::Exists => "exists",
        Verdict::DoesNotExist => "does_not_exist",
        Verdict::Invalid => "invalid",
    };
    json!({
        "name": name,
        "root": hex::encode(root),
        "pubkey": bs58::encode(k).into_string(),
        "account": m.map(acc_json),
        "proof": hex::encode(p.to_bytes()),
        "expect": expect,
    })
}

fn main() {
    let mut rng = StdRng::seed_from_u64(2026);
    let mut accs = Vec::new();
    for i in 0..300 {
        let k: Key = rng.gen();
        let mut owner = [0u8; 32];
        owner[0] = rng.gen_range(0..4);
        let data: Vec<u8> = (0..rng.gen_range(0..200)).map(|_| rng.gen()).collect();
        // Disa vlera të mëdha lamports për të testuar u64 të plotë.
        let lamports = if i % 50 == 0 { u64::MAX - i } else { rng.gen_range(1..1u64 << 50) };
        accs.push((k, AccountMeta::from_account(lamports, &owner, i % 7 == 0, &data)));
    }
    let tree = Tree::from_accounts(accs.iter().map(|(k, m)| (*k, Some(m.clone()))).collect());
    let root = tree.root();
    let mut cases = Vec::new();

    // Përfshirje e vlefshme.
    for (k, m) in accs.iter().take(40) {
        let (p, _) = tree.prove(k);
        cases.push(case("inclusion", &root, k, Some(m), &p));
    }
    // Mospërfshirje e vlefshme.
    for _ in 0..20 {
        let k: Key = rng.gen();
        let (p, _) = tree.prove(&k);
        cases.push(case("exclusion", &root, &k, None, &p));
    }
    // Sulmet: duhet të dalin "invalid".
    for (k, m) in accs.iter().skip(40).take(10) {
        let (p, _) = tree.prove(k);
        let mut fake = m.clone();
        fake.lamports = fake.lamports.wrapping_add(1);
        cases.push(case("attack_wrong_lamports", &root, k, Some(&fake), &p));
        let mut fake = m.clone();
        fake.owner[5] ^= 1;
        cases.push(case("attack_wrong_owner", &root, k, Some(&fake), &p));
        let mut bad = p.clone();
        if let Some(s) = bad.siblings.first_mut() {
            s[0] ^= 1;
        }
        cases.push(case("attack_tampered_sibling", &root, k, Some(m), &bad));
        cases.push(case("attack_claim_missing", &root, k, None, &p));
        let mut other_root = root;
        other_root[31] ^= 1;
        cases.push(case("attack_wrong_root", &other_root, k, Some(m), &p));
    }
    for c in &cases {
        assert!(
            (c["name"].as_str().unwrap().starts_with("attack")) == (c["expect"] == "invalid"),
            "rast i papritur: {c}"
        );
    }
    println!("{}", serde_json::to_string_pretty(&json!({"version": 1, "cases": cases})).unwrap());
}
