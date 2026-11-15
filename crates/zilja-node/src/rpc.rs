//! Serveri RPC i Ziles (JSON-RPC 2.0 mbi HTTP).
//!
//! Metodat:
//! - `getAccountProof [pubkey_base58, {"slot": n}?]` → dëftesa në slotin e fundit të rrënjosur,
//!   ose në gjendjen e një sloti të kaluar brenda kujtesës (vetëm me pemën në disk). `slot` në
//!   përgjigje është sloti i rrënjosur që u përdor vërtet (≤ i kërkuari, L-05).
//! - `getZiljaRoot [slot?]` → `{slot, root, accounts, complete}`; me `slot`, root-i i atij sloti
//!   nga unaza e sloteve të fundit (Simpler-5), në të dyja mënyrat.
//!
//! `lamports` dhe `dataLen` dalin si tekst dhjetor (F-08): JSON-i s'mund t'i mbajë saktë mbi 2^53.
//! - `getZiljaAttestation [slot?]` → `{slot, root, validator, signature}` (kur zilja firmos).
//!
//! Rrugë GET: `/health`, `/metrics` (Prometheus).

use crate::service::{Shared, State};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use tiny_http::{Header, Method, Response, Server};
use zilja_verifier::Key;

/// Gabim RPC: (kodi, mesazhi).
pub type RpcError = (i64, String);

/// Ndërton përgjigjen e `getAccountProof` nga gjendja (`at = None` = sloti i fundit).
pub fn account_proof_json(st: &State, pubkey: &Key, at: Option<u64>) -> Result<Value, RpcError> {
    // L-05: gjendja në slotin `v` është ajo e slotit më të fundit të rrënjosur ≤ v; ai etiketohet.
    let at = match at {
        None => None,
        Some(v) if v >= st.slot => Some(st.slot),
        Some(v) => match st.roots.range(..=v).next_back() {
            Some((s, _)) => Some(*s),
            None => return Err((-32602, format!("sloti {v} është jashtë kujtesës"))),
        },
    };
    let (slot, root, proof, meta) = st.tree.prove_at(pubkey, at).map_err(|e| (-32602, e))?;
    // L-02: një pemë që s'mbulon çdo llogari nuk jep dëftesa "nuk ekziston".
    if meta.is_none() && !st.complete {
        return Err((-32006, "pema e kësaj zileje nuk mbulon çdo llogari (nisi pa snapshot): s'jep dëftesa mospërfshirjeje".into()));
    }
    let bytes = proof.to_bytes();
    let account = meta.map(|m| {
        json!({
            "lamports": m.lamports.to_string(),
            "owner": bs58::encode(m.owner).into_string(),
            "executable": m.executable,
            "dataLen": m.data_len.to_string(),
            "dataHash": hex::encode(m.data_hash),
        })
    });
    Ok(json!({
        "slot": slot,
        "root": hex::encode(root),
        "complete": st.complete,
        "pubkey": bs58::encode(pubkey).into_string(),
        "account": account,
        "proof": hex::encode(&bytes),
        "proofBytes": bytes.len(),
    }))
}

fn parse_pubkey(s: &str) -> Result<Key, String> {
    let v = bs58::decode(s).into_vec().map_err(|e| format!("pubkey jo base58: {e}"))?;
    v.try_into().map_err(|_| "pubkey duhet të ketë 32 bajt".to_string())
}

/// Përpunon një kërkesë JSON-RPC (ndarë nga HTTP, që të testohet lehtë).
pub fn handle_jsonrpc(shared: &Shared, body: &str) -> Value {
    let req: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return err(Value::Null, -32700, &format!("JSON i pavlefshëm: {e}")),
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!([]));
    // L-04: params, nëse jepen, duhet të jenë listë.
    let Some(params) = params.as_array().cloned() else {
        return err(id, -32602, "params duhet të jenë listë");
    };
    let st = shared.state.read().unwrap();
    if !st.ready {
        return match &st.broken {
            Some(why) => err(id, -32003, &format!("zilja ndaloi: {why}")),
            None => err(id, -32002, "zilja ende po ndërton pemën fillestare"),
        };
    }
    match method {
        "getAccountProof" => {
            let (Some(pk), true) = (params.first().and_then(|p| p.as_str()), params.len() <= 2) else {
                return err(id, -32602, "duhet params: [pubkey, {slot}?]");
            };
            let at = match params.get(1) {
                None => None,
                Some(o) => match o.as_object() {
                    Some(m) if m.keys().all(|k| k == "slot") => match m.get("slot") {
                        None => None,
                        Some(s) => match s.as_u64() {
                            Some(s) => Some(s),
                            None => return err(id, -32602, "slot duhet numër i plotë jo negativ"),
                        },
                    },
                    _ => return err(id, -32602, "opsionet e njohura: {slot}"),
                },
            };
            match parse_pubkey(pk).map_err(|e| (-32602, e)).and_then(|k| account_proof_json(&st, &k, at)) {
                Ok(res) => {
                    shared.metrics.proofs_served.fetch_add(1, Ordering::Relaxed);
                    json!({"jsonrpc": "2.0", "id": id, "result": res})
                }
                Err((code, e)) => err(id, code, &e),
            }
        }
        "getZiljaRoot" => {
            let (slot, root) = match params.first() {
                None => (st.slot, st.tree.root()),
                Some(v) => match v.as_u64() {
                    Some(s) if s == st.slot => (s, st.tree.root()),
                    Some(s) => match st.roots.get(&s) {
                        Some(r) => (s, *r),
                        None => return err(id, -32005, &format!("s'ka root për slotin {s}")),
                    },
                    None => return err(id, -32602, "slot duhet numër i plotë jo negativ"),
                },
            };
            json!({
                "jsonrpc": "2.0", "id": id,
                "result": {
                    "slot": slot, "root": hex::encode(root), "accounts": st.tree.len(), "complete": st.complete,
                    "provableFrom": st.tree.provable_range().map(|r| r.0),
                }
            })
        }
        "getZiljaAttestation" => {
            let Some(attester) = st.attester else {
                return err(id, -32004, "kjo zile nuk firmos (mungon attest_keypair)");
            };
            let wanted = match params.first() {
                None => None,
                Some(v) => match v.as_u64() {
                    Some(s) => Some(s),
                    None => return err(id, -32602, "slot duhet numër i plotë jo negativ"),
                },
            };
            let found = match wanted {
                Some(s) => st.attestations.get(&s).map(|a| (s, a)),
                None => st.attestations.iter().next_back().map(|(s, a)| (*s, a)),
            };
            match found {
                Some((slot, a)) => json!({"jsonrpc": "2.0", "id": id, "result": {
                    "slot": slot,
                    "root": hex::encode(a.root),
                    "validator": bs58::encode(attester).into_string(),
                    "signature": hex::encode(a.signature),
                }}),
                None => err(id, -32005, "s'ka firmë për këtë slot"),
            }
        }
        _ => err(id, -32601, "metodë e panjohur"),
    }
}

fn err(id: Value, code: i64, msg: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}})
}

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
}

/// Nis serverin në një thread më vete.
pub fn serve(bind: &str, shared: Shared) -> std::io::Result<JoinHandle<()>> {
    let server = Server::http(bind).map_err(|e| std::io::Error::other(e.to_string()))?;
    log::info!("zilja: RPC po dëgjon në http://{bind}");
    std::thread::Builder::new().name("zilja-rpc".into()).spawn(move || {
        for mut req in server.incoming_requests() {
            let cors = [
                header("Access-Control-Allow-Origin", "*"),
                header("Access-Control-Allow-Headers", "content-type"),
                header("Access-Control-Allow-Methods", "GET, POST, OPTIONS"),
            ];
            let resp = match (req.method(), req.url()) {
                (Method::Options, _) => Response::from_string(""),
                (Method::Get, "/health") => Response::from_string("ok"),
                (Method::Get, "/metrics") => Response::from_string(shared.metrics.prometheus())
                    .with_header(header("Content-Type", "text/plain; version=0.0.4")),
                (Method::Post, _) => {
                    let mut body = String::new();
                    let _ = std::io::Read::read_to_string(&mut req.as_reader(), &mut body);
                    let v = handle_jsonrpc(&shared, &body);
                    Response::from_string(v.to_string()).with_header(header("Content-Type", "application/json"))
                }
                _ => Response::from_string("not found").with_status_code(404),
            };
            let mut resp = resp;
            for h in cors {
                resp.add_header(h);
            }
            let _ = req.respond(resp);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::Service;
    use crate::tracker::{Event, Status};
    use zilja_verifier::{verify_account, AccountMeta, Proof, Verdict};

    fn decode_meta(v: &Value) -> AccountMeta {
        AccountMeta {
            lamports: v["lamports"].as_str().unwrap().parse().unwrap(),
            owner: bs58::decode(v["owner"].as_str().unwrap()).into_vec().unwrap().try_into().unwrap(),
            executable: v["executable"].as_bool().unwrap(),
            data_len: v["dataLen"].as_str().unwrap().parse().unwrap(),
            data_hash: hex::decode(v["dataHash"].as_str().unwrap()).unwrap().try_into().unwrap(),
        }
    }

    #[test]
    fn end_to_end_service_and_jsonrpc() {
        let mut svc = Service::start(None).unwrap();
        let a = [1u8; 32];
        let b = [2u8; 32];
        let owner = [9u8; 32];
        svc.send(Event::Snapshot { slot: 5, pubkey: a, meta: Some(AccountMeta::from_account(10, &owner, false, b"a")) });
        svc.send(Event::EndOfStartup);
        svc.send(Event::Status { slot: 6, parent: Some(5), bank_id: Some(1), status: Status::CreatedBank });
        svc.send(Event::Account {
            slot: 6,
            bank_id: 1,
            pubkey: b,
            write_version: 1,
            meta: Some(AccountMeta::from_account(20, &owner, false, b"bb")),
        });
        svc.send(Event::Status { slot: 6, parent: Some(5), bank_id: Some(1), status: Status::Rooted });
        let shared = svc.shared.clone();
        svc.shutdown();

        let root_resp = handle_jsonrpc(&shared, r#"{"jsonrpc":"2.0","id":1,"method":"getZiljaRoot"}"#);
        assert_eq!(root_resp["result"]["slot"], 6);
        assert_eq!(root_resp["result"]["accounts"], 2);
        let root: [u8; 32] = hex::decode(root_resp["result"]["root"].as_str().unwrap()).unwrap().try_into().unwrap();

        let pk = bs58::encode(b).into_string();
        let r = handle_jsonrpc(&shared, &format!(r#"{{"jsonrpc":"2.0","id":2,"method":"getAccountProof","params":["{pk}"]}}"#));
        let res = &r["result"];
        let proof = Proof::from_bytes(&hex::decode(res["proof"].as_str().unwrap()).unwrap()).unwrap();
        let meta = decode_meta(&res["account"]);
        assert_eq!(meta.lamports, 20);
        assert_eq!(verify_account(&root, &b, Some(&meta), &proof), Verdict::Exists);

        // Llogari që s'ekziston.
        let none = bs58::encode([3u8; 32]).into_string();
        let r = handle_jsonrpc(&shared, &format!(r#"{{"jsonrpc":"2.0","id":3,"method":"getAccountProof","params":["{none}"]}}"#));
        assert!(r["result"]["account"].is_null());
        let proof = Proof::from_bytes(&hex::decode(r["result"]["proof"].as_str().unwrap()).unwrap()).unwrap();
        assert_eq!(verify_account(&root, &[3u8; 32], None, &proof), Verdict::DoesNotExist);

        // Gabimet.
        // Kjo zile nuk firmos.
        assert_eq!(handle_jsonrpc(&shared, r#"{"id":1,"method":"getZiljaAttestation"}"#)["error"]["code"], -32004);

        assert_eq!(handle_jsonrpc(&shared, "jo json")["error"]["code"], -32700);
        // L-04: parametrat e gabuar refuzohen.
        let pk = bs58::encode(b).into_string();
        for bad in [
            format!(r#"{{"id":1,"method":"getAccountProof","params":["{pk}",{{"slot":-1}}]}}"#),
            format!(r#"{{"id":1,"method":"getAccountProof","params":["{pk}",{{"slot":"5"}}]}}"#),
            format!(r#"{{"id":1,"method":"getAccountProof","params":["{pk}",{{"slott":5}}]}}"#),
            format!(r#"{{"id":1,"method":"getAccountProof","params":{{"pubkey":"{pk}"}}}}"#),
            r#"{"id":1,"method":"getZiljaRoot","params":["x"]}"#.to_string(),
        ] {
            assert_eq!(handle_jsonrpc(&shared, &bad)["error"]["code"], -32602, "{bad}");
        }
        // Simpler-5: root-i i një sloti të kaluar nga unaza.
        let r5 = handle_jsonrpc(&shared, r#"{"id":1,"method":"getZiljaRoot","params":[5]}"#);
        assert_eq!(r5["result"]["slot"], 5, "{r5}");
        assert_ne!(r5["result"]["root"], root_resp["result"]["root"]);
        assert_eq!(handle_jsonrpc(&shared, r#"{"id":1,"method":"getZiljaRoot","params":[4]}"#)["error"]["code"], -32005);
        assert_eq!(handle_jsonrpc(&shared, r#"{"id":1,"method":"x"}"#)["error"]["code"], -32601);
        assert_eq!(
            handle_jsonrpc(&shared, r#"{"id":1,"method":"getAccountProof","params":["0OIl"]}"#)["error"]["code"],
            -32602
        );
    }
}

#[cfg(test)]
mod attest_tests {
    use super::*;
    use crate::service::Service;
    use crate::tracker::{Event, Status};
    use ed25519_dalek::SigningKey;
    use zilja_verifier::attest::{verify_quorum, Attestation, StakeTable};
    use zilja_verifier::AccountMeta;

    #[test]
    fn signed_roots_reach_quorum() {
        let keys: Vec<SigningKey> = (1u8..=4).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
        let mut shareds = Vec::new();
        for k in &keys {
            let mut svc = Service::start_with(None, Some(k.clone())).unwrap();
            // Të gjitha zilet shohin të njëjtin zinxhir: duhet të dalë i njëjti root.
            svc.send(Event::Snapshot { slot: 1, pubkey: [7; 32], meta: Some(AccountMeta { lamports: 5, ..Default::default() }) });
            svc.send(Event::EndOfStartup);
            svc.send(Event::Status { slot: 2, parent: Some(1), bank_id: Some(9), status: Status::CreatedBank });
            svc.send(Event::Account { slot: 2, bank_id: 9, pubkey: [8; 32], write_version: 1, meta: Some(AccountMeta { lamports: 6, ..Default::default() }) });
            svc.send(Event::Status { slot: 2, parent: Some(1), bank_id: Some(9), status: Status::Rooted });
            let sh = svc.shared.clone();
            svc.shutdown();
            shareds.push(sh);
        }
        let mut atts = Vec::new();
        let mut root = None;
        for sh in &shareds {
            let r = handle_jsonrpc(sh, r#"{"id":1,"method":"getZiljaAttestation","params":[2]}"#)["result"].clone();
            let rt: [u8; 32] = hex::decode(r["root"].as_str().unwrap()).unwrap().try_into().unwrap();
            assert!(root.is_none() || root == Some(rt));
            root = Some(rt);
            atts.push(Attestation {
                validator: bs58::decode(r["validator"].as_str().unwrap()).into_vec().unwrap().try_into().unwrap(),
                signature: hex::decode(r["signature"].as_str().unwrap()).unwrap().try_into().unwrap(),
            });
        }
        let table = StakeTable { entries: keys.iter().map(|k| (k.verifying_key().to_bytes(), 10)).collect() };
        let root = root.unwrap();
        assert!(verify_quorum(2, &root, &atts, &table).reached);
        assert!(!verify_quorum(2, &root, &atts[..2], &table).reached);
    }
}
