//! P-05: pema me faqe është e lidhur me formatin dhe me genesis-in (MANIFEST).

use std::collections::BTreeMap;
use std::path::Path;
use zilja_store::{PageOptions, PageTree};
use zilja_tree::StateTree;
use zilja_verifier::AccountMeta;

fn opts(genesis: [u8; 32]) -> PageOptions {
    PageOptions { durable: false, genesis: Some(genesis), ..Default::default() }
}

fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            let e = e.unwrap();
            e.file_type().unwrap().is_file().then(|| (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap()))
        })
        .collect()
}

#[test]
fn tree_of_genesis_a_is_refused_for_genesis_b() {
    let d = tempfile::tempdir().unwrap();
    {
        let mut t = PageTree::open(d.path(), opts([1; 32])).unwrap();
        t.stage_snapshot(1, vec![([7; 32], Some(AccountMeta { lamports: 3, ..Default::default() }))]).unwrap();
        t.finish_snapshot(1, true).unwrap();
    }
    let before = files(d.path());
    let e = PageTree::open(d.path(), opts([2; 32])).err().expect("pema e genesis A s'duhet të hapet për genesis B");
    assert!(e.contains("genesis"), "{e}");
    assert_eq!(files(d.path()), before);
    let t = PageTree::open(d.path(), opts([1; 32])).unwrap();
    assert_eq!(t.version(), Some(1));
}

#[test]
fn rocksdb_directory_is_not_opened_as_a_new_pages_install() {
    let d = tempfile::tempdir().unwrap();
    drop(zilja_store::DiskTree::open(d.path(), 4).unwrap());
    let before = files(d.path());
    let e = PageTree::open(d.path(), opts([1; 32])).err().expect("dosja RocksDB s'duhet të hapet si pages");
    assert!(e.contains("RocksDB"), "{e}");
    assert_eq!(files(d.path()), before);
}

#[test]
fn pages_directory_without_manifest_is_refused() {
    let d = tempfile::tempdir().unwrap();
    {
        let mut t = PageTree::open(d.path(), opts([1; 32])).unwrap();
        t.finish_snapshot(1, true).unwrap();
    }
    std::fs::remove_file(d.path().join("MANIFEST")).unwrap();
    let before = files(d.path());
    let e = PageTree::open(d.path(), opts([1; 32])).err().expect("dosje me të dhëna pa MANIFEST");
    assert!(e.contains("MANIFEST"), "{e}");
    assert_eq!(files(d.path()), before);
}

#[test]
fn manifest_with_other_format_version_is_refused() {
    let d = tempfile::tempdir().unwrap();
    drop(PageTree::open(d.path(), opts([1; 32])).unwrap());
    let m = std::fs::read_to_string(d.path().join("MANIFEST")).unwrap();
    std::fs::write(d.path().join("MANIFEST"), m.replace("\"format_version\": 1", "\"format_version\": 99")).unwrap();
    let e = PageTree::open(d.path(), opts([1; 32])).err().expect("format i panjohur");
    assert!(e.contains("format"), "{e}");
}
