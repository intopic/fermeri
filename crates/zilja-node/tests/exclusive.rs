//! P-01/P-04 në nivel shërbimi: një shërbim i dytë mbi të njëjtën pemë ose të njëjtin log
//! firmash refuzohet, dhe ndalimi i rregullt (si `on_unload` gjatë ringarkimit) i liron të dyja.

use ed25519_dalek::SigningKey;
use zilja_node::{rpc, AttestOptions, Options, Service};
use zilja_store::{PageOptions, PageTree};

fn attest(log: &std::path::Path) -> AttestOptions {
    AttestOptions { key: SigningKey::from_bytes(&[3; 32]), identity: None, authorization: None, authorization_file: None, revocations_file: None, genesis: [1; 32], log: Some(log.into()), adopt_legacy: false }
}

fn tree(dir: &std::path::Path) -> Result<PageTree, String> {
    PageTree::open(dir, PageOptions { durable: false, ..Default::default() })
}

#[test]
fn second_service_is_refused_and_reload_releases_locks() {
    let d = tempfile::tempdir().unwrap();
    let (tdir, log) = (d.path().join("pages"), d.path().join("attest.log"));
    for round in 0..3 {
        let svc =
            Service::start_opts(Options { tree: Some(Box::new(tree(&tdir).unwrap())), attest: Some(attest(&log)), ..Default::default() })
                .unwrap();
        // Serverët HTTP mbajnë kopje të gjendjes, si në plugin.
        let mut srv =
            rpc::serve_routes("127.0.0.1:0", svc.shared.clone(), rpc::Routes { jsonrpc: true, metrics: true, cors: false }).unwrap();
        assert!(tree(&tdir).is_err(), "round {round}: pema e dytë u hap ndërsa e para punon");
        let other = d.path().join(format!("pages-{round}"));
        let r =
            Service::start_opts(Options { tree: Some(Box::new(tree(&other).unwrap())), attest: Some(attest(&log)), ..Default::default() });
        assert!(r.is_err(), "round {round}: shërbimi i dytë firmosi përmes të njëjtit log");
        // Ringarkimi: serverët, pastaj shërbimi (si `on_unload`).
        srv.stop();
        drop(srv);
        drop(svc);
    }
}
