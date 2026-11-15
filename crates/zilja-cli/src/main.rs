//! `zilja-check`: banori që kontrollon hambarin e vet.
//!
//! ```text
//! zilja-check <ZILJA_URL> <pubkey> [SOLANA_RPC_URL] [--root <hex>] [--slot <n>]
//! ```
//!
//! 1. Merr dëftesën nga Zilja (`getAccountProof`).
//! 2. E verifikon kundër root-it (ai i serverit, ose ai që jep ti me `--root`,
//!    p.sh. i marrë nga një burim tjetër ose i firmosur nga validatorët).
//! 3. Nëse jep `SOLANA_RPC_URL`, merr të dhënat e llogarisë nga çdo RPC (edhe i
//!    pabesueshëm) dhe kontrollon që përputhen me `dataHash` të vulosur.
//!
//! Kodi i daljes: 0 = e verifikuar, 1 = dëftesë e pavlefshme, 2 = gabim.

use base64::Engine;
use serde_json::{json, Value};
use std::process::exit;
use zilja_verifier::{data_matches, verify_account, AccountMeta, Proof, Verdict};

fn rpc(url: &str, method: &str, params: Value) -> Result<Value, String> {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let resp: Value = ureq::post(url).send_json(body).map_err(|e| e.to_string())?.into_json().map_err(|e| e.to_string())?;
    if let Some(e) = resp.get("error") {
        return Err(e.to_string());
    }
    Ok(resp["result"].clone())
}

fn h32(s: &str) -> Result<[u8; 32], String> {
    hex::decode(s).map_err(|e| e.to_string())?.try_into().map_err(|_| "duhen 32 bajt".into())
}

fn b58_32(s: &str) -> Result<[u8; 32], String> {
    bs58::decode(s).into_vec().map_err(|e| e.to_string())?.try_into().map_err(|_| "duhen 32 bajt".into())
}

fn run() -> Result<bool, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut pos = Vec::new();
    let mut root_override = None;
    let mut at_slot: Option<u64> = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--slot" {
            at_slot = Some(args.get(i + 1).and_then(|v| v.parse().ok()).ok_or("--slot kërkon një numër")?);
            i += 2;
        } else if args[i] == "--root" {
            root_override = Some(args.get(i + 1).ok_or("--root kërkon një vlerë")?.clone());
            i += 2;
        } else {
            pos.push(args[i].clone());
            i += 1;
        }
    }
    if pos.len() < 2 {
        return Err("përdorimi: zilja-check <ZILJA_URL> <pubkey> [SOLANA_RPC_URL] [--root <hex>]".into());
    }
    let (zilja, pubkey_s) = (&pos[0], &pos[1]);
    let pubkey = b58_32(pubkey_s)?;

    let params = match at_slot {
        Some(s) => json!([pubkey_s, {"slot": s}]),
        None => json!([pubkey_s]),
    };
    let r = rpc(zilja, "getAccountProof", params)?;
    let server_root = r["root"].as_str().ok_or("mungon root")?;
    let root_hex = root_override.as_deref().unwrap_or(server_root);
    let root = h32(root_hex)?;
    let proof = Proof::from_bytes(&hex::decode(r["proof"].as_str().ok_or("mungon proof")?).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let meta = if r["account"].is_null() {
        None
    } else {
        let a = &r["account"];
        Some(AccountMeta {
            lamports: a["lamports"].as_u64().ok_or("lamports")?,
            owner: b58_32(a["owner"].as_str().ok_or("owner")?)?,
            executable: a["executable"].as_bool().ok_or("executable")?,
            data_len: a["dataLen"].as_u64().ok_or("dataLen")?,
            data_hash: h32(a["dataHash"].as_str().ok_or("dataHash")?)?,
        })
    };

    println!("slot:          {}", r["slot"]);
    println!("root:          {root_hex}{}", if root_override.is_some() { " (i dhënë nga ti)" } else { " (nga serveri)" });
    println!("dëftesa:       {} bajt, thellësia {}", r["proofBytes"], proof.siblings.len());
    let verdict = verify_account(&root, &pubkey, meta.as_ref(), &proof);
    let ok = match (&verdict, &meta) {
        (Verdict::Exists, Some(m)) => {
            println!("✅ llogaria ekziston: {} lamports, owner {}", m.lamports, bs58::encode(m.owner).into_string());
            true
        }
        (Verdict::DoesNotExist, _) => {
            println!("✅ llogaria NUK ekziston në këtë root");
            true
        }
        _ => {
            println!("❌ dëftesa NUK përputhet me root-in: dikush po gënjen");
            false
        }
    };

    if let (true, Some(m), Some(sol), None) = (ok, &meta, pos.get(2), at_slot) {
        let info = rpc(sol, "getAccountInfo", json!([pubkey_s, {"encoding": "base64", "commitment": "finalized"}]))?;
        let ctx_slot = info["context"]["slot"].as_u64().unwrap_or(0);
        match info["value"]["data"][0].as_str() {
            Some(b64) => {
                let data = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| e.to_string())?;
                let lamports = info["value"]["lamports"].as_u64().unwrap_or(0);
                if data_matches(m, &data) && lamports == m.lamports {
                    println!("✅ të dhënat nga RPC-ja (slot {ctx_slot}) përputhen me vulën");
                } else {
                    println!(
                        "⚠️  RPC-ja (slot {ctx_slot}) jep të dhëna të tjera: ose llogaria ndryshoi pas slotit {}, ose RPC-ja gënjen",
                        r["slot"]
                    );
                }
            }
            None => println!("⚠️  RPC-ja nuk e gjeti llogarinë (slot {ctx_slot})"),
        }
    }
    Ok(ok)
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
