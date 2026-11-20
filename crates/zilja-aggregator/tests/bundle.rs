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
    let agg = Arc::new(Aggregator::new(urls, G, HttpFetch));
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
    let agg = Aggregator::new(vec!["http://x".into()], G, Null);
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

/// R5 (rishikimi): maja s'zvarritet nga kërkesa për slote që askush s'i ka firmosur; numrat e mëdhenj
/// s'tejmbushen; revokimet e pavlefshme, të dyfishta ose të një genesis-i tjetër s'përcillen.
#[test]
fn tip_does_not_creep_overflow_is_safe_and_revocations_are_verified() {
    use ed25519_dalek::Signer;
    use zilja_verifier::attest::revoke_message;
    let id = SigningKey::from_bytes(&[9; 32]);
    let signer = SigningKey::from_bytes(&[8; 32]);
    let thief = SigningKey::from_bytes(&[7; 32]);
    let rev = |by: &SigningKey, g: &[u8; 32]| {
        json!({
            "validator": bs58::encode(id.verifying_key().to_bytes()).into_string(),
            "signer": bs58::encode(signer.verifying_key().to_bytes()).into_string(),
            "signature": hex::encode(by.sign(&revoke_message(g, &signer.verifying_key().to_bytes())).to_bytes()),
        })
    };
    let revs = json!([rev(&id, &G), rev(&id, &G), rev(&thief, &G), rev(&id, &[1; 32]), {"validator": "x"}]);
    struct Fx(Value);
    impl Fetch for Fx {
        fn call(&self, _: &str, m: &str, _: Value) -> Result<Value, String> {
            match m {
                "getZiljaRoot" => Ok(json!({"slot": 10_000})),
                "getZiljaRevocations" => Ok(self.0.clone()),
                _ => Err("s'ka firmë".into()),
            }
        }
    }
    let agg = Aggregator::new(vec!["http://x".into(), "http://y".into()], G, Fx(revs));
    agg.learn_tip();
    // +4 lejohet, por askush s'e ka firmosur: maja mbetet 10 000, ndaj +8 refuzohet sërish.
    let b = agg.bundle(10_004).unwrap();
    assert_eq!(b["answered"], 0);
    assert_eq!(agg.bundle(10_008).unwrap_err().0, -32007, "maja u zvarrit nga një slot pa firma");
    // Pa tejmbushje (në debug do të ishte panic).
    assert_eq!(agg.bundle(u64::MAX).unwrap_err().0, -32007);
    // Vetëm revokimi i vlefshëm, një herë (i njëjti nga dy zile, i falsifikuar, genesis tjetër, i prishur).
    assert_eq!(b["revocations"].as_array().unwrap().len(), 1, "{}", b["revocations"]);
}

/// M5: me shumë zile të ngadalta, bundle-i kthehet brenda afatit të serverit HTTP (5 s), me firmat
/// që erdhën në kohë; zilet e vonuara pyeten sërish më vonë. Revokimet s'merren brenda kërkesës pasi
/// janë marrë një herë.
#[test]
fn many_slow_ziljas_do_not_exceed_the_request_deadline() {
    use std::time::{Duration, Instant};
    use zilja_aggregator::FANOUT_BUDGET;
    let k = SigningKey::from_bytes(&[1; 32]);
    let (_svc, srv) = zile(&k);
    let good = zilja_cli::rpc(&format!("http://{}", srv.addr()), "getZiljaAttestation", json!([2])).unwrap();
    struct Slow(Value);
    impl Fetch for Slow {
        fn call(&self, url: &str, m: &str, _: Value) -> Result<Value, String> {
            match m {
                "getZiljaRoot" => Ok(json!({"slot": 2})),
                "getZiljaRevocations" => Ok(json!([])),
                _ if url == "http://fast" => Ok(self.0.clone()),
                _ => {
                    std::thread::sleep(Duration::from_secs(3));
                    Err("vonë".into())
                }
            }
        }
    }
    // 200 zile: me fan-out-in e vjetër (copa 32 njëra pas tjetrës, 3 s secila) do të ishin ~21 s.
    let mut urls: Vec<String> = (0..199).map(|i| format!("http://slow{i}")).collect();
    urls.push("http://fast".into());
    let agg = Aggregator::new(urls, G, Slow(good));
    agg.learn_tip();
    agg.refresh_revocations();
    let t = Instant::now();
    let b = agg.bundle(2).unwrap();
    let took = t.elapsed();
    assert!(took < FANOUT_BUDGET + Duration::from_millis(500), "bundle-i zgjati {took:?}");
    assert!(took < zilja_node::rpc::REQUEST_DEADLINE, "mbi afatin e serverit: {took:?}");
    assert_eq!((b["ziljas"].as_u64(), b["answered"].as_u64()), (Some(200), Some(1)), "{b}");
}
