//! P-11: politika në Rust jep saktësisht të njëjtin vendim si `sdk/js/src/policy.js` për çdo vektor
//! të përbashkët. Kur ndryshon njëra, ky test (ose `sdk/js/test/policy.test.mjs`) bie.

use serde_json::Value;
use zilja_cli::policy::{decide, Input, Other, Policy, Primary, QuorumGroup};

fn u(v: &Value) -> Option<u64> {
    v.as_u64()
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn input(i: &Value) -> Input {
    let p = &i["policy"];
    let r = &i["primary"];
    Input {
        policy: Policy {
            min_trust: s(&p["minTrust"]),
            min_agree: p["minAgree"].as_i64().unwrap_or(0),
            max_slot_lag: u(&p["maxSlotLag"]),
            min_slot: u(&p["minSlot"]),
        },
        request_pubkey: s(&i["request"]["pubkey"]),
        request_slot: u(&i["request"]["slot"]),
        endpoints: i["endpoints"].as_u64().unwrap() as usize,
        quorum_enabled: i["quorumEnabled"].as_bool().unwrap(),
        root_override: i["rootOverride"].as_bool().unwrap(),
        primary: r.is_object().then(|| Primary {
            pubkey: s(&r["pubkey"]),
            slot: u(&r["slot"]),
            root: s(&r["root"]),
            complete: r["complete"].as_bool() == Some(true),
            verdict: s(&r["verdict"]),
        }),
        others: i["others"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| Other { ok: o["ok"].as_bool() == Some(true), slot: u(&o["slot"]), root: o["root"].as_str().map(str::to_string) })
            .collect(),
        quorum: i["quorum"]
            .as_array()
            .map(|g| g.iter().map(|g| QuorumGroup { root: s(&g["root"]), reached: g["reached"].as_bool() == Some(true) }).collect()),
        quorum_epoch_ok: i["quorumEpochOk"].as_bool(),
        finalized_slot: u(&i["finalizedSlot"]),
        highest_seen: u(&i["highestSeen"]),
        data: i["data"]["matches"].as_bool(),
    }
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(s).collect()
}

#[test]
fn rust_policy_matches_shared_vectors() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../sdk/js/test/policy-vectors.json");
    let vectors: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert!(vectors.len() >= 20, "shumë pak vektorë");
    let mut bad = Vec::new();
    for v in &vectors {
        let d = decide(&input(&v["input"]));
        let e = &v["expect"];
        let c = &e["checks"];
        let same = d.ok == e["ok"].as_bool().unwrap()
            && d.verdict == s(&e["verdict"])
            && d.trust == s(&e["trust"])
            && d.agreeing == e["agreeing"].as_u64().unwrap()
            && d.checks.proof == s(&c["proof"])
            && d.checks.completeness == s(&c["completeness"])
            && d.checks.root == s(&c["root"])
            && d.checks.freshness == s(&c["freshness"])
            && d.checks.data == s(&c["data"])
            && d.errors.iter().map(|x| x.to_string()).collect::<Vec<_>>() == strs(&e["errors"])
            && d.warnings.iter().map(|x| x.to_string()).collect::<Vec<_>>() == strs(&e["warnings"]);
        if !same {
            bad.push(format!("{}: pritej {e}, u mor {d:?}", v["name"]));
        }
    }
    assert!(bad.is_empty(), "Rust ≠ JS:\n{}", bad.join("\n"));
}
