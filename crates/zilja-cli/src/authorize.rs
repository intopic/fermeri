//! `zilja-authorize`: identiteti i validatorit autorizon një çelës të veçantë që firmos root-et, për
//! një periudhë epokash (F-10, v2).
//!
//! ```text
//! zilja-authorize <identity-keypair.json> <signing-keypair.json> <genesis_hash base58> <from_epoch> [epoka=15] > zilja-authorization.json
//! ```
//!
//! Autorizimi vlen për epokat `[from_epoch, from_epoch + epoka)`, me `epoka` 1..=15 (15 ≈ 30 ditë në
//! mainnet). Epokën aktuale e jep `solana epoch`. Rinovoje para se të skadojë (p.sh. kur mbeten 2
//! epoka), me `from_epoch` = epoka aktuale: plugin-i e rilexon skedarin vetë, pa rinisje. Shkruaje në
//! një skedar të përkohshëm dhe zhvendose (`mv`) mbi të vjetrin, që plugin-i të mos lexojë gjysmë
//! skedari (edhe atëherë, një skedar i pavlefshëm injorohet dhe mbetet autorizimi i mëparshëm).
//!
//! Revokimi (R5-1), kur çelësi i firmosjes vidhet ose del nga përdorimi:
//!
//! ```text
//! zilja-authorize --revoke <identity-keypair.json> <signer_pubkey base58> <genesis_hash> > revocation.json
//! ```
//!
//! Revokimi e vërteton veten (firmë e identitetit) dhe është i përhershëm. Vendose te
//! `attest_revocations` (një objekt ose listë) në çdo zile që mund ta shërbejë; klientët e marrin
//! nga cilado zile dhe s'e numërojnë më atë çelës, edhe brenda periudhës së autorizimit.
//!
//! Çelësi i identitetit përdoret vetëm këtu; zilja firmos pastaj vetëm me çelësin e veçantë. Një
//! çelës i vjedhur vlen kështu vetëm deri në fund të periudhës.

use ed25519_dalek::{Signer, SigningKey};
use serde_json::json;
use zilja_verifier::attest::{authorize_message, revoke_message, MAX_AUTH_EPOCHS};

fn keypair(path: &str) -> Result<SigningKey, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let bytes: Vec<u8> = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
    // G22: ekzaktësisht 64 bajt (sekreti + çelësi publik), si `solana-keygen`; pjesa e dytë kontrollohet gjithmonë.
    if bytes.len() != 64 {
        return Err(format!("{path}: keypair i pavlefshëm: {} bajt (duhen 64)", bytes.len()));
    }
    let secret: [u8; 32] = bytes[..32].try_into().unwrap();
    let key = SigningKey::from_bytes(&secret);
    if bytes[32..] != key.verifying_key().to_bytes() {
        return Err(format!("{path}: çelësi publik nuk përputhet me sekretin"));
    }
    Ok(key)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let run = || -> Result<(), String> {
        if args.first().map(String::as_str) == Some("--revoke") {
            let [_, id, signer, genesis] = args.as_slice() else {
                return Err("përdorimi: zilja-authorize --revoke <identity-keypair.json> <signer_pubkey> <genesis_hash>".into());
            };
            let id = keypair(id)?;
            let signer: [u8; 32] = bs58::decode(signer).into_vec().map_err(|e| e.to_string())?.try_into().map_err(|_| "signer duhet 32 bajt")?;
            if signer == id.verifying_key().to_bytes() {
                return Err("identiteti s'mund të revokojë veten; revokohet vetëm një çelës i veçantë".into());
            }
            let g: [u8; 32] = bs58::decode(genesis).into_vec().map_err(|e| e.to_string())?.try_into().map_err(|_| "genesis duhet 32 bajt")?;
            let sig = id.sign(&revoke_message(&g, &signer)).to_bytes();
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "validator": bs58::encode(id.verifying_key().to_bytes()).into_string(),
                    "signer": bs58::encode(signer).into_string(),
                    "genesis": genesis,
                    "signature": hex::encode(sig),
                }))
                .unwrap()
            );
            eprintln!("revokimi është i përhershëm: shtoje te `attest_revocations` në zilet tuaja");
            return Ok(());
        }
        let usage = || format!("përdorimi: zilja-authorize <identity-keypair.json> <signing-keypair.json> <genesis_hash> <from_epoch> [epoka 1..={MAX_AUTH_EPOCHS}, parazgjedhje {MAX_AUTH_EPOCHS}]");
        let (id, signer, genesis, from, n) = match args.as_slice() {
            [a, b, c, d] => (a, b, c, d, None),
            [a, b, c, d, e] => (a, b, c, d, Some(e)),
            _ => return Err(usage()),
        };
        let num = |s: &str, what: &str| s.parse::<u64>().ok().filter(|v| v.to_string() == s).ok_or(format!("{what} duhet numër i plotë: {s:?}"));
        let from = num(from, "from_epoch")?;
        let n = match n {
            Some(n) => num(n, "epoka")?,
            None => MAX_AUTH_EPOCHS,
        };
        if !(1..=MAX_AUTH_EPOCHS).contains(&n) {
            return Err(format!("epoka duhet 1..={MAX_AUTH_EPOCHS}, jo {n}"));
        }
        let until = from.checked_add(n).ok_or("from_epoch shumë i madh")?;
        let id = keypair(id)?;
        let signer = keypair(signer)?.verifying_key().to_bytes();
        let g: [u8; 32] = bs58::decode(genesis).into_vec().map_err(|e| e.to_string())?.try_into().map_err(|_| "genesis duhet 32 bajt")?;
        let auth = id.sign(&authorize_message(&g, &signer, from, until)).to_bytes();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "version": 2,
                "identity": bs58::encode(id.verifying_key().to_bytes()).into_string(),
                "signer": bs58::encode(signer).into_string(),
                "genesis": genesis,
                "fromEpoch": from,
                "untilEpoch": until,
                "authorization": hex::encode(auth),
            }))
            .unwrap()
        );
        eprintln!("autorizimi vlen për epokat {from}..{} (përfshirë); rinovoje para epokës {until}", until - 1);
        Ok(())
    };
    if let Err(e) = run() {
        eprintln!("gabim: {e}");
        std::process::exit(2);
    }
}
