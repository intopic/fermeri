//! Pjesët e përbashkëta të `zilja-check` dhe `zilja-quorum`: politika e vendimit (P-11), thirrjet
//! JSON-RPC dhe mbledhja e firmave të grupuara sipas root-it.

pub mod policy;

use serde_json::{json, Value};
use std::collections::HashMap;
use zilja_verifier::attest::{verify_quorum, Attestation, QuorumReport, StakeTable};

/// JSON-RPC me kohë maksimale dhe kufi për madhësinë e përgjigjes (L-09).
pub fn rpc(url: &str, method: &str, params: Value) -> Result<Value, String> {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(30)).build();
    let r = agent.post(url).send_json(body).map_err(|e| format!("{url} {method}: {e}"))?;
    let mut text = String::new();
    std::io::Read::read_to_string(&mut std::io::Read::take(r.into_reader(), 64 << 20), &mut text).map_err(|e| e.to_string())?;
    if text.len() >= 64 << 20 {
        return Err("përgjigje shumë e madhe (> 64 MB)".into());
    }
    let resp: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if let Some(e) = resp.get("error") {
        return Err(format!("{url} {method}: {e}"));
    }
    Ok(resp["result"].clone())
}

pub fn h32(s: &str) -> Result<[u8; 32], String> {
    zilja_verifier::wire::hex_lower::<32>(s).ok_or_else(|| "duhen 32 bajt hex i vogël (A20)".into())
}

pub fn b58_32(s: &str) -> Result<[u8; 32], String> {
    bs58::decode(s).into_vec().map_err(|e| e.to_string())?.try_into().map_err(|_| "duhen 32 bajt".into())
}

/// Root i shkruar si 64 shifra hex të vogla (formati i ziles).
pub fn is_root_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// u64 nga JSON sipas `zilja_verifier::wire`: tekst dhjetor, ose numër ≤ 2^53 - 1 (F-08).
pub fn u64_field(v: &Value) -> Option<u64> {
    match v {
        Value::String(s) => zilja_verifier::wire::parse_u64_dec(s),
        Value::Number(n) => n.as_u64().filter(|n| *n <= zilja_verifier::wire::MAX_SAFE_JSON_INT),
        _ => None,
    }
}

/// Tabela e stake-ut nga `getVoteAccounts` (current + delinquent), sipas identitetit (nodePubkey).
///
/// A10: kjo është pamja e RPC-së (`bank.vote_accounts()`), jo tabela e konsensusit e epokës; i
/// besohet RPC-së. Merren vetëm llogaritë me `epochVoteAccount`. A11: kërkohen të dy grupet, çelësat
/// 32 bajt, `activatedStake` u64 i saktë; shuma kontrollohet për tejmbushje. Çdo shkelje = gabim.
pub fn stake_table_from(va: &Value) -> Result<StakeTable, String> {
    let mut stakes: HashMap<[u8; 32], u64> = HashMap::new();
    for group in ["current", "delinquent"] {
        let list = va[group].as_array().ok_or(format!("getVoteAccounts pa grupin \"{group}\""))?;
        for v in list {
            let node = b58_32(v["nodePubkey"].as_str().ok_or("getVoteAccounts: nodePubkey")?).map_err(|e| format!("getVoteAccounts: nodePubkey {e}"))?;
            b58_32(v["votePubkey"].as_str().ok_or("getVoteAccounts: votePubkey")?).map_err(|e| format!("getVoteAccounts: votePubkey {e}"))?;
            let stake = v["activatedStake"].as_u64().ok_or(format!("getVoteAccounts: activatedStake i pavlefshëm: {}", v["activatedStake"]))?;
            if v["epochVoteAccount"].as_bool() != Some(true) || stake == 0 {
                continue;
            }
            let e = stakes.entry(node).or_default();
            *e = e.checked_add(stake).ok_or("getVoteAccounts: stake jashtë u64")?;
        }
    }
    Ok(StakeTable { entries: stakes.into_iter().collect() })
}

/// A10: pamja e stake-ut nga RPC-ja, e lexuar në mënyrë koherente (epoka para dhe pas
/// `getVoteAccounts` duhet të jetë e njëjtë), bashkë me kufijtë e epokës së saj.
pub struct StakeView {
    pub table: StakeTable,
    pub epoch: u64,
    pub first_slot: u64,
    pub last_slot: u64,
}

impl StakeView {
    /// A10: tabela vlen vetëm për slotet e epokës së saj (të dy kufijtë).
    pub fn covers(&self, slot: u64) -> bool {
        (self.first_slot..=self.last_slot).contains(&slot)
    }
}

pub fn stake_view(sol: &str) -> Result<StakeView, String> {
    let fin = json!([{"commitment": "finalized"}]);
    let e1 = rpc(sol, "getEpochInfo", fin.clone())?;
    let va = rpc(sol, "getVoteAccounts", fin.clone())?;
    let e2 = rpc(sol, "getEpochInfo", fin)?;
    let f = |e: &Value, k: &str| e[k].as_u64().ok_or(format!("getEpochInfo: {k}"));
    let epoch = f(&e1, "epoch")?;
    if f(&e2, "epoch")? != epoch {
        return Err(format!("epoka ndryshoi gjatë leximit të tabelës së stake-ut ({epoch} → {})", e2["epoch"]));
    }
    let first_slot = f(&e1, "absoluteSlot")?.checked_sub(f(&e1, "slotIndex")?).ok_or("getEpochInfo: slotIndex > absoluteSlot")?;
    let last_slot = first_slot.checked_add(f(&e1, "slotsInEpoch")?.checked_sub(1).ok_or("getEpochInfo: slotsInEpoch 0")?).ok_or("getEpochInfo: tejmbushje")?;
    Ok(StakeView { table: stake_table_from(&va)?, epoch, first_slot, last_slot })
}

pub fn genesis(sol: &str, arg: Option<&str>) -> Result<[u8; 32], String> {
    let g = match arg {
        Some(g) => g.to_string(),
        None => rpc(sol, "getGenesisHash", json!([]))?.as_str().ok_or("getGenesisHash")?.to_string(),
    };
    if g.len() == 64 && g.bytes().all(|b| b.is_ascii_hexdigit()) {
        h32(&g)
    } else {
        b58_32(&g)
    }
}

/// Një firmë e lexuar nga një zile; `Err` = përgjigje e pavlefshme (s'numërohet, s'rrëzon veglën).
pub fn parse_attestation(a: &Value, slot: u64) -> Result<(String, Attestation), String> {
    if a["slot"].as_u64() != Some(slot) {
        return Err(format!("firma është për slot tjetër ({})", a["slot"]));
    }
    let root = a["root"].as_str().ok_or("mungon root")?;
    if !is_root_hex(root) {
        return Err(format!("root i pavlefshëm: {root:?}"));
    }
    let validator = b58_32(a["validator"].as_str().ok_or("mungon validator")?)?;
    let signature: [u8; 64] = zilja_verifier::wire::hex_lower(a["signature"].as_str().ok_or("mungon signature")?).ok_or("firma duhet 64 bajt hex i vogël")?;
    let signer = match a["signer"].as_str() {
        Some(s) => Some(b58_32(s)?),
        None => None,
    };
    let authorization = match a["authorization"].as_str() {
        Some(h) => Some(zilja_verifier::wire::hex_lower(h).ok_or("autorizimi duhet 64 bajt hex i vogël")?),
        None => None,
    };
    Ok((root.to_string(), Attestation { validator, signer, authorization, signature }))
}

/// Firmat e zileve për `slot`, të grupuara sipas root-it, me raportin e kuorumit për secilin.
/// Zilet që s'përgjigjen ose japin të dhëna të pavlefshme raportohen në `skipped`.
pub struct QuorumResult {
    pub groups: Vec<(String, QuorumReport)>,
    pub skipped: Vec<(String, String)>,
}

pub fn quorum_by_root(ziljas: &[String], slot: u64, genesis: &[u8; 32], table: &StakeTable) -> QuorumResult {
    let mut by_root: Vec<(String, Vec<Attestation>)> = Vec::new();
    let mut skipped = Vec::new();
    for z in ziljas {
        match rpc(z, "getZiljaAttestation", json!([slot])).and_then(|a| parse_attestation(&a, slot)) {
            Ok((root, att)) => match by_root.iter_mut().find(|(r, _)| *r == root) {
                Some((_, v)) => v.push(att),
                None => by_root.push((root, vec![att])),
            },
            Err(e) => skipped.push((z.clone(), e)),
        }
    }
    let groups = by_root
        .into_iter()
        .map(|(root_hex, atts)| {
            let root = h32(&root_hex).expect("i kontrolluar nga is_root_hex");
            let rep = verify_quorum(genesis, slot, &root, &atts, table);
            (root_hex, rep)
        })
        .collect();
    QuorumResult { groups, skipped }
}
