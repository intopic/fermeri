//! Gjeneron vektorë testi (JSON) që çdo verifikues tjetër (JS, Swift, Kotlin…)
//! duhet t'i kalojë njësoj si verifikuesi Rust.
//!
//! `cargo run -p zilja-bench --bin zilja-vectors > web/vectors.json`

use rand::{rngs::StdRng, Rng, SeedableRng};
use serde_json::{json, Value};
use zilja_tree::Tree;
use zilja_verifier::{tree_key, verify_account, AccountMeta, Key, Proof, Verdict};

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
    // Pema mbi tree_key (F-09); vektorët mbajnë pubkey-in, dhe verifikuesit e hashojnë vetë.
    let tree = Tree::from_accounts(accs.iter().map(|(k, m)| (tree_key(k), Some(m.clone()))).collect());
    let root = tree.root();
    let mut cases = Vec::new();

    // Përfshirje e vlefshme.
    for (k, m) in accs.iter().take(40) {
        let (p, _) = tree.prove(&tree_key(k));
        cases.push(case("inclusion", &root, k, Some(m), &p));
    }
    // Mospërfshirje e vlefshme.
    for _ in 0..20 {
        let k: Key = rng.gen();
        let (p, _) = tree.prove(&tree_key(&k));
        cases.push(case("exclusion", &root, &k, None, &p));
    }
    // Sulmet: duhet të dalin "invalid".
    for (k, m) in accs.iter().skip(40).take(10) {
        let (p, _) = tree.prove(&tree_key(k));
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
    println!("{}", serde_json::to_string_pretty(&json!({"version": 2, "cases": cases, "attest": attest_vectors()})).unwrap());
}

/// Vektorë për firmat e validatorëve: 10 validatorë me çelësa të njohur dhe stake të ndryshëm.
fn attest_vectors() -> Value {
    use ed25519_dalek::{Signer, SigningKey};
    use zilja_verifier::attest::{attest_message, authorize_message, verify_quorum, Attestation, Authorization, StakeTable};
    let genesis = zilja_verifier::sha256(b"zilja-vektoret-genesis");
    let keys: Vec<SigningKey> = (1u8..=10).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
    let stakes: Vec<u64> = (1..=10).map(|i| i * 1_000_000_000).collect(); // 1..10 SOL, gjithsej 55
    let table = StakeTable { entries: keys.iter().zip(&stakes).map(|(k, s)| (k.verifying_key().to_bytes(), *s)).collect() };
    let sign = |k: &SigningKey, slot: u64, root: &[u8; 32]| Attestation {
        validator: k.verifying_key().to_bytes(),
        signer: None,
        authorization: None,
        signature: k.sign(&attest_message(&genesis, slot, root)).to_bytes(),
    };
    // Çelës i veçantë firmosjeje, i autorizuar nga identiteti (F-10).
    // v2: autorizimi vlen për epokat [800, 815).
    let sign_dedicated = |id: &SigningKey, slot: u64, root: &[u8; 32], g_auth: &[u8; 32]| {
        let signer = SigningKey::from_bytes(&zilja_verifier::sha256(&id.to_bytes()));
        let s_pk = signer.verifying_key().to_bytes();
        let (from, until) = (800, 815);
        Attestation {
            validator: id.verifying_key().to_bytes(),
            signer: Some(s_pk),
            authorization: Some(Authorization { from_epoch: from, until_epoch: until, signature: id.sign(&authorize_message(g_auth, &s_pk, from, until)).to_bytes() }),
            signature: signer.sign(&attest_message(&genesis, slot, root)).to_bytes(),
        }
    };
    let enc = |atts: &[Attestation]| -> Value {
        atts.iter()
            .map(|a| {
                json!({
                    "validator": bs58::encode(a.validator).into_string(),
                    "signer": a.signer.map(|s| bs58::encode(s).into_string()),
                    "authorization": a.authorization.map(|au| json!({
                        "fromEpoch": au.from_epoch, "untilEpoch": au.until_epoch, "signature": hex::encode(au.signature),
                    })),
                    "signature": hex::encode(a.signature),
                })
            })
            .collect()
    };
    let root = [7u8; 32];
    let slot = 123_456u64;
    let mut cases = Vec::new();
    let mut add_at = |name: &str, epoch: u64, s: u64, r: &[u8; 32], atts: Vec<Attestation>| {
        let rep = verify_quorum(&genesis, epoch, s, r, &atts, &table);
        cases.push(json!({
            "name": name, "epoch": epoch, "slot": s, "root": hex::encode(r), "atts": enc(&atts),
            "expectReached": rep.reached, "expectAttested": rep.attested_stake.to_string(),
        }));
    };
    let mut add = |name: &str, s: u64, r: &[u8; 32], atts: Vec<Attestation>| add_at(name, 805, s, r, atts);
    // 7 më të mëdhenjtë: 4+..+10 = 49/55 → po.
    add("quorum_reached", slot, &root, keys[3..].iter().map(|k| sign(k, slot, &root)).collect());
    // 5 më të mëdhenjtë: 6+..+10 = 40/55 = 72.7% → po. 4 më të mëdhenjtë: 34/55 = 61.8% → jo.
    add("quorum_reached_by_stake_not_count", slot, &root, keys[5..].iter().map(|k| sign(k, slot, &root)).collect());
    add("not_enough_stake", slot, &root, keys[6..].iter().map(|k| sign(k, slot, &root)).collect());
    // Firma për root tjetër / slot tjetër.
    add("wrong_root", slot, &[8u8; 32], keys[3..].iter().map(|k| sign(k, slot, &root)).collect());
    add("wrong_slot", slot + 1, &root, keys[3..].iter().map(|k| sign(k, slot, &root)).collect());
    // I njëjti validator shumë herë.
    add("duplicates", slot, &root, (0..10).map(|_| sign(&keys[9], slot, &root)).collect());
    // Validatorë jashtë tabelës.
    let strangers: Vec<SigningKey> = (50u8..60).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
    add("strangers", slot, &root, strangers.iter().map(|k| sign(k, slot, &root)).collect());
    // Firmë e prishur.
    let mut bad: Vec<Attestation> = keys[3..].iter().map(|k| sign(k, slot, &root)).collect();
    for a in bad.iter_mut().take(3) {
        a.signature[0] ^= 1;
    }
    add("corrupted_signatures", slot, &root, bad);
    add("dedicated_signers", slot, &root, keys[3..].iter().map(|k| sign_dedicated(k, slot, &root, &genesis)).collect());
    add("dedicated_signers_other_chain_auth", slot, &root, keys[3..].iter().map(|k| sign_dedicated(k, slot, &root, &[1u8; 32])).collect());
    // Autorizimi [800, 815): vlen në 800 dhe 814, jo në 799 dhe 815.
    let ded = || -> Vec<Attestation> { keys[3..].iter().map(|k| sign_dedicated(k, slot, &root, &genesis)).collect() };
    add_at("dedicated_signers_first_epoch", 800, slot, &root, ded());
    add_at("dedicated_signers_last_epoch", 814, slot, &root, ded());
    add_at("dedicated_signers_before_start", 799, slot, &root, ded());
    add_at("dedicated_signers_expired", 815, slot, &root, ded());
    // Periudha e zgjatur pa firmë të re.
    let mut extended = ded();
    for a in extended.iter_mut() {
        a.authorization.as_mut().unwrap().until_epoch = 816;
    }
    add_at("dedicated_signers_extended_period", 815, slot, &root, extended);
    json!({
        "genesis": hex::encode(genesis),
        "table": table.entries.iter().map(|(k, s)| json!([bs58::encode(k).into_string(), s.to_string()])).collect::<Vec<_>>(),
        "cases": cases,
    })
}
