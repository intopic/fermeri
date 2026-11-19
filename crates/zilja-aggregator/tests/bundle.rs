//! R5-3: aggregator-i mbledh firmat nga zile të vërteta (HTTP) dhe klienti arrin kuorumin nga një
//! thirrje e vetme; firmat e falsifikuara ose të prishura s'kalojnë verifikimin e klientit.

use ed25519_dalek::SigningKey;
use serde_json::{json, Value};
use std::sync::Arc;
use zilja_aggregator::{Aggregator, Fetch, HttpFetch, NEW_SLOTS_PER_SEC};
use zilja_node::rpc::{self, HttpHandler, Routes};
use zilja_node::{AttestOptions, Event, Options, Service, Status};
use zilja_verifier::attest::{verify_quorum_with, Revoked, StakeTable};
use zilja_verifier::AccountMeta;

const G: [u8; 32] = [0x33; 32];

/// Një zile që firmos slotin 2 me çelësin `k`, e shërbyer në HTTP.
fn zile(k: &SigningKey) -> (Service, rpc::HttpServer) {
    let attest = AttestOptions { key: k.clone(), identity: None, authorization: None, authorization_file: None, revocations_file: None, genesis: G, log: None, adopt_legacy: false };
    let svc = Service::start_opts(Options { attest: Some(attest), ..Default::default() }).unwrap();
    svc.send(Event::Snapshot { slot: 1, pubkey: [7; 32], meta: Some(AccountMeta { lamports: 5, ..Default::default() }) });
    svc.send(Event::EndOfStartup);
    svc.send(Event::Status { slot: 2, parent: Some(1), bank_id: Some(9), status: Status::CreatedBank });
    svc.send(Event::Account { slot: 2, bank_id: 9, pubkey: [8; 32], write_version: 1, meta: Some(AccountMeta { lamports: 6, ..Default::default() }) });
    svc.send(Event::Status { slot: 2, parent: Some(1), bank_id: Some(9), status: Status::Rooted });
    for _ in 0..200 {
        if svc.shared.state.read().unwrap().attestations.contains_key(&2) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let srv = rpc::serve_routes("127.0.0.1:0", svc.shared.clone(), Routes { jsonrpc: true, metrics: false, cors: false }).unwrap();
    (svc, srv)
}

fn to_att(v: &Value) -> (String, zilja_verifier::attest::Attestation) {
    zilja_cli::parse_attestation(v, 2).unwrap()
}

#[test]
fn bundle_from_real_ziljas_reaches_quorum_with_one_call() {
    let keys: Vec<SigningKey> = (1u8..=4).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
    let ziles: Vec<_> = keys.iter().map(zile).collect();
    let mut urls: Vec<String> = ziles.iter().map(|(_, s)| format!("http://{}", s.addr())).collect();
    // Një adresë që s'përgjigjet: s'e prish bundle-in.
    urls.push("http://127.0.0.1:9".into());
    let agg = Arc::new(Aggregator::new(urls, HttpFetch));
    let srv = rpc::serve_handler("127.0.0.1:0", agg.clone(), Routes { jsonrpc: true, metrics: true, cors: false }, rpc::HttpLimits::default()).unwrap();
    // Klienti: një thirrje HTTP te aggregator-i.
    let b = zilja_cli::rpc(&format!("http://{}", srv.addr()), "getZiljaQuorumBundle", json!([2])).unwrap();
    assert_eq!((b["ziljas"].as_u64(), b["answered"].as_u64()), (Some(5), Some(4)), "{b}");
    let atts: Vec<_> = b["attestations"].as_array().unwrap().iter().map(to_att).collect();
    let root = zilja_cli::h32(&atts[0].0).unwrap();
    assert!(atts.iter().all(|(r, _)| *r == atts[0].0));
    let table = StakeTable { entries: keys.iter().map(|k| (k.verifying_key().to_bytes(), 10)).collect() };
    let list: Vec<_> = atts.iter().map(|(_, a)| a.clone()).collect();
    assert!(verify_quorum_with(&G, 0, 2, &root, &list, &table, &Revoked::default()).reached);
    // E dyta herë: nga kujtesa, e njëjta përgjigje.
    let b2 = zilja_cli::rpc(&format!("http://{}", srv.addr()), "getZiljaQuorumBundle", json!([2])).unwrap();
    assert_eq!(b["attestations"], b2["attestations"]);
    assert!(agg.metrics().contains("zilja_aggregator_bundles_served_total 2"));
}

/// Një aggregator keqdashës mund të fshehë ose të prishë firma, por s'krijon kuorum: klienti verifikon.
#[test]
fn a_lying_aggregator_cannot_create_a_quorum() {
    let keys: Vec<SigningKey> = (1u8..=4).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
    let (_svc, srv) = zile(&keys[0]);
    let b = zilja_cli::rpc(&format!("http://{}", srv.addr()), "getZiljaAttestation", json!([2])).unwrap();
    let (root_hex, real) = to_att(&b);
    let root = zilja_cli::h32(&root_hex).unwrap();
    let table = StakeTable { entries: keys.iter().map(|k| (k.verifying_key().to_bytes(), 10)).collect() };
    // "Firma" të tjera me identitetet e validatorëve të tjerë, por pa çelësat e tyre.
    let mut forged = vec![real.clone()];
    for k in &keys[1..] {
        forged.push(zilja_verifier::attest::Attestation { validator: k.verifying_key().to_bytes(), ..real.clone() });
    }
    let r = verify_quorum_with(&G, 0, 2, &root, &forged, &table, &Revoked::default());
    assert_eq!(r.valid_signatures, 1);
    assert!(!r.reached);
}

/// Kufijtë: slote larg majës dhe shumë slote të reja në sekondë refuzohen (kosto e fan-out-it).
#[test]
fn far_slots_and_new_slot_floods_are_refused() {
    struct Null;
    impl Fetch for Null {
        fn call(&self, _: &str, m: &str, _: Value) -> Result<Value, String> {
            if m == "getZiljaRoot" {
                Ok(json!({"slot": 10_000}))
            } else {
                Err("s'ka".into())
            }
        }
    }
    let agg = Aggregator::new(vec!["http://x".into()], Null);
    agg.learn_tip();
    assert_eq!(agg.bundle(1).unwrap_err().0, -32007, "shumë larg poshtë majës");
    assert_eq!(agg.bundle(10_010).unwrap_err().0, -32007, "mbi majë");
    for s in 0..NEW_SLOTS_PER_SEC as u64 {
        assert!(agg.bundle(9_990 + s).is_ok());
    }
    assert_eq!(agg.bundle(9_000).unwrap_err().0, -32005, "slot i ri përtej kufirit për sekondë");
    assert!(agg.bundle(9_990).is_ok(), "nga kujtesa s'numërohet");
    // Parametrat.
    let r = agg.jsonrpc(r#"{"id":1,"method":"getZiljaQuorumBundle","params":["x"]}"#);
    assert_eq!(r["error"]["code"], -32602);
    assert_eq!(agg.jsonrpc(r#"{"id":1,"method":"nope"}"#)["error"]["code"], -32601);
}
