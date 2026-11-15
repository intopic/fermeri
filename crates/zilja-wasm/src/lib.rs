//! Verifikuesi në WebAssembly: e njëjta logjikë si `zilja-verifier`, për kuletat në web.
//!
//! Ndërtimi: `cargo build -p zilja-wasm --target wasm32-unknown-unknown --profile release-wasm`
//! (CI e bën këtë dhe mat madhësinë e `.wasm`).

use wasm_bindgen::prelude::*;
use zilja_verifier::{verify_account, AccountMeta, Proof, Verdict};

fn h32(s: &str) -> Result<[u8; 32], String> {
    hex::decode(s).map_err(|e| e.to_string())?.try_into().map_err(|_| "duhen 32 bajt".to_string())
}

fn b58_32(s: &str) -> Result<[u8; 32], String> {
    bs58::decode(s).into_vec().map_err(|e| e.to_string())?.try_into().map_err(|_| "duhen 32 bajt".to_string())
}

/// u64 i rreptë nga tekst dhjetor (F-03), sipas `zilja_verifier::wire`.
pub fn parse_u64(s: &str) -> Result<u64, String> {
    zilja_verifier::wire::parse_u64_dec(s).ok_or_else(|| format!("u64 i pavlefshëm: {s:?}"))
}

fn parse_bool(s: &str) -> Result<bool, String> {
    match s {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("executable duhet \"true\" ose \"false\", jo {s:?}")),
    }
}

/// Kthen "exists", "does_not_exist", "invalid" ose "error: ...".
///
/// Numrat jepen si tekst dhjetor dhe `executable` si "true"/"false": kufiri JS→WASM
/// përndryshe i mbështjell numrat modulo 2^64 dhe i kthen vlerat në boolean sipas "truthiness".
/// Për llogari që pretendohet se nuk ekziston, jep `owner = ""` (fushat e tjera injorohen).
#[allow(clippy::too_many_arguments)]
pub fn verify_str(
    root_hex: &str,
    pubkey_b58: &str,
    lamports: &str,
    owner_b58: &str,
    executable: &str,
    data_len: &str,
    data_hash_hex: &str,
    proof_hex: &str,
) -> String {
    let run = || -> Result<Verdict, String> {
        let root = h32(root_hex)?;
        let pubkey = b58_32(pubkey_b58)?;
        let proof = Proof::from_bytes(&hex::decode(proof_hex).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let meta = if owner_b58.is_empty() {
            None
        } else {
            Some(AccountMeta {
                lamports: parse_u64(lamports)?,
                owner: b58_32(owner_b58)?,
                executable: parse_bool(executable)?,
                data_len: parse_u64(data_len)?,
                data_hash: h32(data_hash_hex)?,
            })
        };
        Ok(verify_account(&root, &pubkey, meta.as_ref(), &proof))
    };
    match run() {
        Ok(Verdict::Exists) => "exists".into(),
        Ok(Verdict::DoesNotExist) => "does_not_exist".into(),
        Ok(Verdict::Invalid) => "invalid".into(),
        Err(e) => format!("error: {e}"),
    }
}

#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn verify(
    root_hex: &str,
    pubkey_b58: &str,
    lamports: &str,
    owner_b58: &str,
    executable: &str,
    data_len: &str,
    data_hash_hex: &str,
    proof_hex: &str,
) -> String {
    verify_str(root_hex, pubkey_b58, lamports, owner_b58, executable, data_len, data_hash_hex, proof_hex)
}

/// SHA-256 e të dhënave të llogarisë, për t'i krahasuar me `dataHash`.
#[wasm_bindgen]
pub fn data_hash(data: &[u8]) -> String {
    hex::encode(zilja_verifier::sha256(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// I njëjti skedar vektorësh që kalon edhe verifikuesi JS.
    #[test]
    fn passes_shared_vectors() {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../sdk/js/test/vectors.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let cases = v["cases"].as_array().unwrap();
        for c in cases {
            let a = &c["account"];
            let s = |x: &serde_json::Value| x.as_str().unwrap_or("").to_string();
            let got = verify_str(
                c["root"].as_str().unwrap(),
                c["pubkey"].as_str().unwrap(),
                &s(&a["lamports"]),
                &s(&a["owner"]),
                if a["executable"].as_bool().unwrap_or(false) { "true" } else { "false" },
                &s(&a["dataLen"]),
                &s(&a["dataHash"]),
                c["proof"].as_str().unwrap(),
            );
            assert_eq!(got, c["expect"].as_str().unwrap(), "{}", c["name"]);
        }
        assert!(cases.len() >= 100);
    }

    /// Vektorët e formatit në rrjet: e njëjta përgjigje JSON, e kaluar te WASM si në JS.
    #[test]
    fn passes_wire_vectors() {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../sdk/js/test/wire-vectors.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        for c in v["cases"].as_array().unwrap() {
            let r = &c["response"];
            let a = &r["account"];
            let s = |x: &serde_json::Value| x.as_str().unwrap_or("").to_string();
            // Kufiri JS→WASM: numrat si tekst; një numër JSON vetëm kur JS e mban saktë.
            let num = |x: &serde_json::Value| match x {
                serde_json::Value::String(t) => t.clone(),
                serde_json::Value::Number(n) => match n.as_u64().filter(|n| *n <= zilja_verifier::wire::MAX_SAFE_JSON_INT) {
                    Some(n) => n.to_string(),
                    None => "x".into(),
                },
                _ => "x".into(),
            };
            let exe = match &a["executable"] {
                serde_json::Value::Bool(b) => b.to_string(),
                _ => "x".into(),
            };
            let got = verify_str(&s(&r["root"]), &s(&r["pubkey"]), &num(&a["lamports"]), &s(&a["owner"]), &exe, &num(&a["dataLen"]), &s(&a["dataHash"]), &s(&r["proof"]));
            let got = if got.starts_with("error") { "invalid".to_string() } else { got };
            assert_eq!(got, c["expect"].as_str().unwrap(), "{}", c["name"]);
        }
    }

    #[test]
    fn strict_inputs() {
        assert_eq!(parse_u64("18446744073709551615"), Ok(u64::MAX));
        for bad in ["18446744073709551616", "-1", " 1", "01", "", "1e3", "0x10"] {
            assert!(parse_u64(bad).is_err(), "{bad}");
        }
        assert!(parse_bool("1").is_err() && parse_bool("False").is_err() && parse_bool("true") == Ok(true));
    }
}
