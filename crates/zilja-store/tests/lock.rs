//! P-01: pronësia ekskluzive e dosjes së motorit me faqe.
//!
//! Një instancë e dytë mbi të njëjtën dosje nuk duhet të bëjë asgjë: as recovery (që shkurton
//! log-un), as wipe (kur gjen `INVALID`), as shkrim. Duhet të refuzohet, me skedarët të paprekur.

use std::collections::BTreeMap;
use std::path::Path;
use zilja_store::{PageOptions, PageTree};
use zilja_tree::StateTree;
use zilja_verifier::AccountMeta;

fn opts() -> PageOptions {
    PageOptions { history: 4, cache_depth: 4, durable: false, ..Default::default() }
}

/// Përmbajtja e çdo skedari në dosje (për të provuar që asgjë s'ndryshoi).
fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            let e = e.unwrap();
            e.file_type().unwrap().is_file().then(|| (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap()))
        })
        .collect()
}

fn filled(dir: &Path) -> PageTree {
    let mut t = PageTree::open(dir, opts()).unwrap();
    let accs = (0..500u32).map(|i| {
        let mut k = [0u8; 32];
        k[..4].copy_from_slice(&i.to_le_bytes());
        (k, Some(AccountMeta { lamports: i as u64 + 1, ..Default::default() }))
    });
    t.stage_snapshot(1, accs.collect()).unwrap();
    t.finish_snapshot(1, true).unwrap();
    for s in 2..6u64 {
        t.apply_slot(s, vec![([s as u8; 32], Some(AccountMeta { lamports: s, ..Default::default() }))], None).unwrap();
    }
    t
}

#[test]
fn second_open_is_refused_and_touches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let a = filled(dir.path());
    // Bishti i log-ut "i prerë": një hapje e dytë do ta shkurtonte gjatë recovery.
    std::fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join("meta.log"))
        .unwrap()
        .set_len(std::fs::metadata(dir.path().join("meta.log")).unwrap().len() + 7)
        .unwrap();
    let before = files(dir.path());
    let err = PageTree::open(dir.path(), opts()).err().expect("hapja e dytë duhet të refuzohet");
    assert!(err.contains("përdorim"), "mesazh diagnostikues: {err}");
    assert_eq!(files(dir.path()), before, "hapja e refuzuar ndryshoi skedarët");
    drop(a);
    assert!(PageTree::open(dir.path(), opts()).is_ok(), "pasi mbyllet e para, hapja lejohet");
}

#[test]
fn second_open_does_not_wipe_an_invalidated_tree_in_use() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = filled(dir.path());
    a.invalidate("test").unwrap();
    let before = files(dir.path());
    assert!(PageTree::open(dir.path(), opts()).is_err(), "hapja e dytë duhet të refuzohet, jo të fshijë");
    assert_eq!(files(dir.path()), before, "pema e instancës së parë u fshi");
    // Instanca e parë vazhdon të lexojë nga skedarët e saj.
    a.verify_integrity().unwrap();
}

#[test]
fn rebuild_keeps_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = filled(dir.path());
    a.invalidate("test").unwrap();
    drop(a);
    // Rihapja rindërton (fshin faqet), por lock-u mbetet i saji gjatë gjithë jetës.
    let mut b = PageTree::open(dir.path(), opts()).unwrap();
    assert!(b.resume_point().is_none());
    assert!(PageTree::open(dir.path(), opts()).is_err());
    b.clear().unwrap();
    assert!(PageTree::open(dir.path(), opts()).is_err(), "clear() e liroi lock-un");
    b.finish_snapshot(9, true).unwrap();
    assert!(PageTree::open(dir.path(), opts()).is_err());
}
