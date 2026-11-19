//! `zilja-check`: banori që kontrollon hambarin e vet.
//!
//! ```text
//! zilja-check <ZILJA_URL> <pubkey> [SOLANA_RPC_URL]
//!             [--also <ZILJA_URL>]... [--min-trust single|agreement|quorum] [--min-agree <n>]
//!             [--quorum] [--genesis <hash>] [--max-lag <n>|none] [--min-slot <n>]
//!             [--root <hex>] [--slot <n>]
//! ```
//!
//! Vendimi merret nga e njëjta politikë si SDK-ja dhe faqja (P-11, `policy.rs`):
//! 1. dëftesa nga zilja e parë, e verifikuar kundër root-it (i serverit, ose `--root` yti);
//! 2. besimi te root-i: `single` (një zile), `agreement` (≥ min-agree zile me të njëjtin root,
//!    parazgjedhja), `quorum` (> 2/3 e stake-ut firmosi), `user` (`--root`);
//! 3. freskia: me `SOLANA_RPC_URL`, sloti nuk duhet të jetë më shumë se `--max-lag` (150) pas të
//!    finalizuarit; `--slot` është kërkesë historike;
//! 4. të dhënat: me `SOLANA_RPC_URL`, bajtet e RPC-së duhet të përputhen me vulën.
//!
//! Kodi i daljes: 0 = çdo kontroll kaloi, 1 = një kontroll dështoi, 2 = gabim (rrjet, argumente).

use base64::Engine;
use serde_json::{json, Value};
use std::process::exit;
use zilja_cli::policy::{self, decide, explain, Input, Other, Policy, Primary, QuorumGroup};
use zilja_cli::{b58_32, genesis, h32, is_root_hex, quorum_by_root, rpc, stake_view, u64_field};
use zilja_verifier::{data_matches, verify_account, AccountMeta, Proof, Verdict};

const USAGE: &str = "përdorimi: zilja-check <ZILJA_URL> <pubkey> [SOLANA_RPC_URL] [--also <ZILJA_URL>]... \
[--min-trust single|agreement|quorum] [--min-agree <n>] [--quorum] [--genesis <hash>] [--max-lag <n>|none] \
[--min-slot <n>] [--root <hex>] [--slot <n>]";

struct Args {
    endpoints: Vec<String>,
    pubkey: String,
    sol: Option<String>,
    root_override: Option<String>,
    at_slot: Option<u64>,
    policy: Policy,
    quorum: bool,
    genesis: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut pos = Vec::new();
    let mut also = Vec::new();
    let (mut root_override, mut at_slot, mut genesis) = (None, None, None);
    let mut min_trust = policy::DEFAULT_MIN_TRUST.to_string();
    let mut min_agree = policy::DEFAULT_MIN_AGREE as i64;
    let (mut max_lag, mut max_lag_set, mut min_slot, mut quorum) = (None, false, None, false);
    let mut i = 0;
    let num =
        |v: Option<&String>, f: &str| -> Result<u64, String> { v.and_then(|v| v.parse().ok()).ok_or(format!("{f} kërkon një numër")) };
    while i < args.len() {
        let v = args.get(i + 1);
        let mut step = 2;
        match args[i].as_str() {
            "--slot" => at_slot = Some(num(v, "--slot")?),
            "--root" => root_override = Some(v.ok_or("--root kërkon një vlerë")?.clone()),
            "--also" => also.push(v.ok_or("--also kërkon një URL")?.clone()),
            "--min-trust" => min_trust = v.ok_or("--min-trust kërkon një vlerë")?.clone(),
            "--min-agree" => min_agree = num(v, "--min-agree")? as i64,
            "--min-slot" => min_slot = Some(num(v, "--min-slot")?),
            "--genesis" => genesis = Some(v.ok_or("--genesis kërkon një vlerë")?.clone()),
            "--max-lag" => {
                max_lag_set = true;
                max_lag = match v.map(String::as_str) {
                    Some("none") => None,
                    _ => Some(num(v, "--max-lag")?),
                };
            }
            "--quorum" => {
                quorum = true;
                step = 1;
            }
            a if a.starts_with("--") => return Err(format!("flamur i panjohur {a}\n{USAGE}")),
            _ => {
                pos.push(args[i].clone());
                step = 1;
            }
        }
        i += step;
    }
    if pos.len() < 2 || pos.len() > 3 {
        return Err(USAGE.into());
    }
    let sol = pos.get(2).cloned();
    if !max_lag_set && sol.is_some() {
        max_lag = Some(policy::DEFAULT_MAX_SLOT_LAG_WITH_RPC);
    }
    if quorum && sol.is_none() {
        return Err("--quorum kërkon SOLANA_RPC_URL".into());
    }
    // E njëjta adresë dy herë s'është dy zile (L-04).
    let mut endpoints: Vec<String> = Vec::new();
    for e in std::iter::once(pos[0].clone()).chain(also) {
        let n = e.trim_end_matches('/').to_string();
        if !endpoints.contains(&n) {
            endpoints.push(n);
        }
    }
    if let Some(r) = &root_override {
        if !is_root_hex(r) {
            return Err("--root duhet 64 shifra hex të vogla".into());
        }
    }
    Ok(Args {
        endpoints,
        pubkey: pos[1].clone(),
        sol,
        root_override,
        at_slot,
        policy: Policy { min_trust, min_agree, max_slot_lag: max_lag, min_slot },
        quorum,
        genesis,
    })
}

fn account_meta(a: &Value) -> Result<AccountMeta, String> {
    Ok(AccountMeta {
        lamports: u64_field(&a["lamports"]).ok_or("lamports")?,
        owner: b58_32(a["owner"].as_str().ok_or("owner")?)?,
        executable: a["executable"].as_bool().ok_or("executable")?,
        data_len: u64_field(&a["dataLen"]).ok_or("dataLen")?,
        data_hash: h32(a["dataHash"].as_str().ok_or("dataHash")?)?,
    })
}

/// Verifikon dëftesën; çdo gjë e keqformuar është "invalid", jo gabim i veglës.
fn check_proof(r: &Value, root_hex: &str, pubkey: &[u8; 32]) -> (&'static str, Option<AccountMeta>, String) {
    let res = (|| -> Result<(Verdict, Option<AccountMeta>), String> {
        let root = h32(root_hex)?;
        let proof = Proof::from_bytes(&zilja_verifier::wire::decode_hex_lower(r["proof"].as_str().ok_or("mungon proof")?).ok_or("proof: hex i pavlefshëm (vetëm [0-9a-f])")?)
            .map_err(|e| e.to_string())?;
        let meta = if r["account"].is_null() { None } else { Some(account_meta(&r["account"])?) };
        Ok((verify_account(&root, pubkey, meta.as_ref(), &proof), meta))
    })();
    match res {
        Ok((Verdict::Exists, Some(m))) => ("exists", Some(m), String::new()),
        Ok((Verdict::DoesNotExist, None)) => ("does_not_exist", None, String::new()),
        Ok((Verdict::DoesNotExist, Some(_))) => ("invalid", None, "dëftesë mungese me të dhëna llogarie".into()),
        Ok(_) => ("invalid", None, String::new()),
        Err(e) => ("invalid", None, e),
    }
}

fn run() -> Result<bool, String> {
    let a = parse_args()?;
    let ce = policy::config_errors(&a.policy, a.endpoints.len(), a.quorum, a.root_override.is_some());
    if !ce.is_empty() {
        for c in &ce {
            println!("❌ {c}: {} ({} zile)", explain(c), a.endpoints.len());
        }
        return Ok(false);
    }
    let pubkey = b58_32(&a.pubkey)?;
    let primary_url = &a.endpoints[0];
    let params = match a.at_slot {
        Some(s) => json!([a.pubkey, {"slot": s}]),
        None => json!([a.pubkey]),
    };
    let r = rpc(primary_url, "getAccountProof", params)?;
    let server_root = r["root"].as_str().unwrap_or_default().to_string();
    let root_hex = a.root_override.clone().unwrap_or(server_root.clone());
    let slot = r["slot"].as_u64();
    let (verdict, meta, why) = check_proof(&r, &root_hex, &pubkey);
    let mut notes: Vec<String> = Vec::new();
    if !why.is_empty() {
        notes.push(format!("{primary_url}: përgjigje e pavlefshme ({why})"));
    }

    // Zilet e tjera: root-i në të njëjtin slot.
    let mut others = Vec::new();
    if let (Some(slot), None) = (slot, &a.root_override) {
        for ep in &a.endpoints[1..] {
            match rpc(ep, "getZiljaRoot", json!([slot])) {
                Ok(v) => {
                    let root = v["root"].as_str().map(str::to_string);
                    if root.as_deref() != Some(server_root.as_str()) {
                        notes.push(format!("{ep} jep root tjetër për slotin {slot}"));
                    }
                    others.push(Other { ok: true, slot: v["slot"].as_u64(), root });
                }
                Err(e) => {
                    notes.push(e);
                    others.push(Other { ok: false, slot: None, root: None });
                }
            }
        }
    }

    // Kuorumi, grupuar sipas root-it; tabela e stake-ut nga RPC vlen vetëm për epokën aktuale.
    let (mut quorum, mut quorum_epoch_ok) = (None, None);
    if let (true, Some(slot), None, Some(sol)) = (a.quorum, slot, &a.root_override, &a.sol) {
        let res = (|| -> Result<_, String> {
            let view = stake_view(sol)?;
            let g = genesis(sol, a.genesis.as_deref())?;
            Ok((view.covers(slot), quorum_by_root(&a.endpoints, slot, &g, &view.table)))
        })();
        match res {
            Ok((epoch_ok, q)) => {
                quorum_epoch_ok = Some(epoch_ok);
                for (ep, e) in &q.skipped {
                    notes.push(format!("{ep}: firma s'u numërua ({e})"));
                }
                for (root, rep) in &q.groups {
                    let pct = if rep.total_stake > 0 { rep.attested_stake as f64 * 100.0 / rep.total_stake as f64 } else { 0.0 };
                    println!("kuorumi:       root {}… {pct:.2}% e stake-ut{}", &root[..16], if rep.reached { " ✅" } else { "" });
                }
                notes.push("pesha e kuorumit është pamja e SOLANA_RPC_URL (getVoteAccounts), jo tabela e konsensusit e epokës; i besohet atij".into());
                quorum = Some(q.groups.iter().map(|(root, rep)| QuorumGroup { root: root.clone(), reached: rep.reached }).collect());
            }
            Err(e) => notes.push(format!("kuorumi s'u kontrollua: {e}")),
        }
    }

    // Freskia.
    let mut finalized = None;
    if let (Some(sol), None, Some(_)) = (&a.sol, a.at_slot, a.policy.max_slot_lag) {
        match rpc(sol, "getSlot", json!([{"commitment": "finalized"}])) {
            Ok(v) => finalized = v.as_u64(),
            Err(e) => notes.push(format!("sloti i finalizuar s'u mor: {e}")),
        }
    }

    // Të dhënat nga RPC-ja.
    let mut data = None;
    if let (Some(sol), Some(m), None) = (&a.sol, &meta, a.at_slot) {
        match rpc(sol, "getAccountInfo", json!([a.pubkey, {"encoding": "base64", "commitment": "finalized"}])) {
            Ok(info) => {
                let ctx = info["context"]["slot"].as_u64().unwrap_or(0);
                let same = match info["value"]["data"][0].as_str() {
                    Some(b64) => base64::engine::general_purpose::STANDARD
                        .decode(b64)
                        .is_ok_and(|d| data_matches(m, &d) && info["value"]["lamports"].as_u64() == Some(m.lamports)),
                    None => false,
                };
                if !same {
                    notes.push(format!("RPC-ja (slot {ctx}) jep të dhëna të tjera: ose llogaria ndryshoi pas slotit, ose RPC-ja gënjen"));
                }
                data = Some(same);
            }
            Err(e) => notes.push(format!("të dhënat s'u krahasuan: {e}")),
        }
    }

    let d = decide(&Input {
        policy: a.policy.clone(),
        request_pubkey: a.pubkey.clone(),
        request_slot: a.at_slot,
        endpoints: a.endpoints.len(),
        quorum_enabled: a.quorum,
        root_override: a.root_override.is_some(),
        primary: r.is_object().then(|| Primary {
            pubkey: r["pubkey"].as_str().unwrap_or_default().to_string(),
            slot,
            root: root_hex.clone(),
            complete: r["complete"].as_bool() == Some(true),
            verdict: verdict.to_string(),
        }),
        others,
        quorum,
        quorum_epoch_ok,
        finalized_slot: finalized,
        highest_seen: None,
        data,
    });

    println!("slot:          {}", r["slot"]);
    println!("root:          {root_hex}{}", if a.root_override.is_some() { " (i dhënë nga ti)" } else { " (nga serveri)" });
    println!("dëftesa:       {}", d.checks.proof);
    println!("pema:          {}", d.checks.completeness);
    println!("besimi:        {} ({} zile pajtohen)", d.trust, d.agreeing);
    println!("freskia:       {}", d.checks.freshness);
    println!("të dhënat:     {}", d.checks.data);
    println!("vendimi:       {}", d.verdict);
    for n in &notes {
        println!("  · {n}");
    }
    for w in &d.warnings {
        println!("⚠️  {w}: {}", explain(w));
    }
    for e in &d.errors {
        println!("❌ {e}: {}", explain(e));
    }
    if d.ok {
        match (&d.verdict[..], &meta) {
            ("exists", Some(m)) => println!("✅ llogaria ekziston: {} lamports, owner {}", m.lamports, bs58::encode(m.owner).into_string()),
            _ => println!("✅ llogaria NUK ekziston në këtë root"),
        }
        if d.checks.freshness == "unchecked" {
            println!("   (freskia s'u kontrollua: jep SOLANA_RPC_URL që kjo të jetë gjendja aktuale)");
        }
    } else {
        println!("❌ Mos i beso si gjendje e verifikuar");
    }
    Ok(d.ok)
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
