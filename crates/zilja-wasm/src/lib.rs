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

/// Kthen "exists", "does_not_exist", "invalid" ose "error: ...".
///
/// Për llogari që pretendohet se nuk ekziston, jep `lamports = 0` dhe `owner = ""`.
#[allow(clippy::too_many_arguments)]
pub fn verify_str(
    root_hex: &str,
    pubkey_b58: &str,
    lamports: u64,
    owner_b58: &str,
    executable: bool,
    data_len: u64,
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
            Some(AccountMeta { lamports, owner: b58_32(owner_b58)?, executable, data_len, data_hash: h32(data_hash_hex)? })
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
    lamports: u64,
    owner_b58: &str,
    executable: bool,
    data_len: u64,
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
