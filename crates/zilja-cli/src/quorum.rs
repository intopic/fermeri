//! `zilja-quorum`: a e kanë firmosur pleqtë ziles?
//!
//! ```text
//! zilja-quorum <SOLANA_RPC_URL> <ZILJA_URL> [ZILJA_URL ...]
//! ```
//!
//! 1. Merr tabelën e stake-ut nga `getVoteAccounts` (sot nga RPC; në versionin e
//!    protokollit, nga vetë zinxhiri).
//! 2. Pyet çdo zile për root-in e firmosur në të njëjtin slot.
//! 3. Kontrollon firmat dhe që validatorët që firmosën kanë > 2/3 e stake-ut.
//!
//! Kodi i daljes: 0 = kuorumi u arrit, 1 = nuk u arrit, 2 = gabim.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::exit;
use zilja_verifier::attest::{verify_quorum, Attestation, StakeTable};

fn rpc(url: &str, method: &str, params: Value) -> Result<Value, String> {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let resp: Value = ureq::post(url).send_json(body).map_err(|e| e.to_string())?.into_json().map_err(|e| e.to_string())?;
    if let Some(e) = resp.get("error") {
        return Err(format!("{url} {method}: {e}"));
    }
    Ok(resp["result"].clone())
}

fn b58_32(s: &str) -> Result<[u8; 32], String> {
    bs58::decode(s).into_vec().map_err(|e| e.to_string())?.try_into().map_err(|_| "duhen 32 bajt".into())
}

fn run() -> Result<bool, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        return Err("përdorimi: zilja-quorum <SOLANA_RPC_URL> <ZILJA_URL> [ZILJA_URL ...]".into());
    }
    let (sol, ziljas) = (&args[0], &args[1..]);

    let va = rpc(sol, "getVoteAccounts", json!([{"commitment": "finalized"}]))?;
    let mut stakes: HashMap<[u8; 32], u64> = HashMap::new();
    for group in ["current", "delinquent"] {
        for v in va[group].as_array().into_iter().flatten() {
            let node = b58_32(v["nodePubkey"].as_str().unwrap_or_default())?;
            *stakes.entry(node).or_default() += v["activatedStake"].as_u64().unwrap_or(0);
        }
    }
    let table = StakeTable { entries: stakes.into_iter().collect() };
    println!("stake total: {} lamports, {} validatorë", table.total(), table.entries.len());

    // Sloti i përbashkët: më i vogli nga slotet e fundit të firmosura.
    let mut latest = Vec::new();
    for z in ziljas {
        latest.push(rpc(z, "getZiljaAttestation", json!([]))?["slot"].as_u64().ok_or("slot")?);
    }
    let slot = *latest.iter().min().unwrap();
    println!("sloti i krahasimit: {slot}");

    let mut by_root: HashMap<String, Vec<Attestation>> = HashMap::new();
    for z in ziljas {
        let a = rpc(z, "getZiljaAttestation", json!([slot]))?;
        let validator = b58_32(a["validator"].as_str().ok_or("validator")?)?;
        let signature: [u8; 64] = hex::decode(a["signature"].as_str().ok_or("signature")?)
            .map_err(|e| e.to_string())?
            .try_into()
            .map_err(|_| "firma duhet 64 bajt")?;
        let root = a["root"].as_str().ok_or("root")?.to_string();
        println!("  {z}: validator {} → root {}", a["validator"].as_str().unwrap(), &root[..16]);
        by_root.entry(root).or_default().push(Attestation { validator, signature });
    }
    if by_root.len() > 1 {
        println!("⚠️  zilet japin {} root-e të ndryshme për slotin {slot}", by_root.len());
    }
    let mut best: Option<(String, zilja_verifier::attest::QuorumReport)> = None;
    for (root_hex, atts) in &by_root {
        let root: [u8; 32] = hex::decode(root_hex).map_err(|e| e.to_string())?.try_into().map_err(|_| "root")?;
        let rep = verify_quorum(slot, &root, atts, &table);
        if best.as_ref().is_none_or(|(_, b)| rep.attested_stake > b.attested_stake) {
            best = Some((root_hex.clone(), rep));
        }
    }
    let (root, rep) = best.ok_or("asnjë firmë")?;
    let pct = if rep.total_stake > 0 { rep.attested_stake as f64 * 100.0 / rep.total_stake as f64 } else { 0.0 };
    println!("root {root}");
    println!("firma të vlefshme: {}, të refuzuara: {}, stake që firmosi: {pct:.2}%", rep.valid_signatures, rep.rejected);
    if rep.reached {
        println!("✅ kuorumi > 2/3 u arrit: pleqtë e firmosën ziles");
    } else {
        println!("❌ kuorumi nuk u arrit");
    }
    Ok(rep.reached)
}

fn main() {
    match run() {
        Ok(true) => exit(0),
        Ok(false) => exit(1),
        Err(e) => {
            eprintln!("gabim: {e}");
            exit(2)
        }
    }
}
