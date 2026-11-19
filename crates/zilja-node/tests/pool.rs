//! A08: pas `shutdown` (thirret nga `on_unload` i plugin-it) s'mbetet asnjë thread që mund të
//! ekzekutojë kod të bibliotekës. Puna paralele e pemës (Rayon) s'duhet të kalojë kurrë te regjistri
//! global i Rayon, i cili s'mbyllet kurrë.
//!
//! Test i vetëm në këtë skedar: numri i thread-eve të procesit (`/proc/self/task`) s'prishet nga
//! teste të tjera paralele.

use std::time::{Duration, Instant};
use zilja_node::{Event, Options, Service};
use zilja_store::{PageOptions, PageTree};
use zilja_verifier::AccountMeta;

fn threads() -> Vec<String> {
    std::fs::read_dir("/proc/self/task")
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path().join("comm")).unwrap_or_default().trim().to_string())
        .collect()
}

#[test]
fn unload_leaves_no_threads_behind() {
    let before = threads();
    for round in 0..3 {
        let d = tempfile::tempdir().unwrap();
        let tree = PageTree::open(d.path(), PageOptions { durable: false, cache_depth: 4, history: 4, ..Default::default() }).unwrap();
        let mut svc = Service::start_opts(Options { tree: Some(Box::new(tree)), ..Default::default() }).unwrap();
        // Snapshot mjaftueshëm i madh që ndërtimi të kalojë nga rruga paralele (rayon::join, par_iter).
        for i in 0..20_000u32 {
            let mut k = [0u8; 32];
            k[..4].copy_from_slice(&i.wrapping_mul(2_654_435_761).to_le_bytes());
            svc.send(Event::Snapshot { slot: 5, pubkey: k, meta: Some(AccountMeta { lamports: i as u64 + 1, ..Default::default() }) });
        }
        svc.send(Event::EndOfStartup);
        let t = Instant::now();
        while svc.shared.state.read().unwrap().tree.len() < 20_000 && t.elapsed() < Duration::from_secs(60) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(svc.shared.state.read().unwrap().tree.len(), 20_000, "raundi {round}: pema s'u ndërtua");
        assert!(svc.pool_threads() > 0, "pool-i i instancës s'u përdor");
        svc.shutdown();
        assert_eq!(svc.pool_threads(), 0, "raundi {round}: thread-e të pool-it mbetën gjallë pas shutdown");
        drop(svc);
        // `join` kthehet kur kerneli zgjon pritësin (CLONE_CHILD_CLEARTID, në `do_exit`): thread-i s'ekzekuton
        // më kod të bibliotekës, por hyrja e tij te /proc hiqet pak më vonë. Pritet ky kontabilitet (≤ 2 s);
        // një thread i gjallë (p.sh. i regjistrit global të Rayon) s'zhduket kurrë.
        let t = Instant::now();
        let mut after = threads();
        while after.len() != before.len() && t.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(10));
            after = threads();
        }
        assert_eq!(after.len(), before.len(), "raundi {round}: thread-e mbetën pas shutdown: para {before:?}, pas {after:?}");
    }
}
