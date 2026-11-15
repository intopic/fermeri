//! `zilja-authorize`: identiteti i validatorit autorizon një çelës të veçantë që firmos root-et (F-10).
//!
//! ```text
//! zilja-authorize <identity-keypair.json> <signing-keypair.json> <genesis_hash base58> > zilja-authorization.json
//! ```
//!
//! Çelësi i identitetit përdoret vetëm një herë, këtu; zilja firmos pastaj vetëm me çelësin e
//! veçantë. Skedari që del vendoset te `attest_authorization` në konfigurimin e plugin-it.

use ed25519_dalek::{Signer, SigningKey};
use serde_json::json;
use zilja_verifier::attest::authorize_message;

fn keypair(path: &str) -> Result<SigningKey, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let bytes: Vec<u8> = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
    let secret: [u8; 32] = bytes.get(..32).and_then(|b| b.try_into().ok()).ok_or("keypair i pavlefshëm")?;
    let key = SigningKey::from_bytes(&secret);
    if bytes.len() == 64 && bytes[32..] != key.verifying_key().to_bytes() {
        return Err(format!("{path}: çelësi publik nuk përputhet me sekretin"));
    }
    Ok(key)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let run = || -> Result<(), String> {
        let [id, signer, genesis] = args.as_slice() else {
            return Err("përdorimi: zilja-authorize <identity-keypair.json> <signing-keypair.json> <genesis_hash>".into());
        };
        let id = keypair(id)?;
        let signer = keypair(signer)?.verifying_key().to_bytes();
        let g: [u8; 32] = bs58::decode(genesis).into_vec().map_err(|e| e.to_string())?.try_into().map_err(|_| "genesis duhet 32 bajt")?;
        let auth = id.sign(&authorize_message(&g, &signer)).to_bytes();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "identity": bs58::encode(id.verifying_key().to_bytes()).into_string(),
                "signer": bs58::encode(signer).into_string(),
                "genesis": genesis,
                "authorization": hex::encode(auth),
            }))
            .unwrap()
        );
        Ok(())
    };
    if let Err(e) = run() {
        eprintln!("gabim: {e}");
        std::process::exit(2);
    }
}
