//! A22: `zilja-disk-bench` s'fshin kurrë dosjen që i jepet, as skedarët e tjerë në të: punon në një
//! dosje të re brenda saj dhe fshin vetëm atë.

#[test]
fn disk_bench_never_deletes_what_it_did_not_create() {
    let parent = tempfile::tempdir().unwrap();
    let mine = parent.path().join("te-dhenat-e-operatorit.txt");
    std::fs::write(&mine, b"mos me fshi").unwrap();
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_zilja-disk-bench"))
        .args(["2000", "2", "50"])
        .arg(parent.path())
        .env("ZILJA_ENGINE", "pages")
        .env("ZILJA_CACHE_DEPTH", "8")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(std::fs::read(&mine).unwrap(), b"mos me fshi", "bench-i preku skedarin e operatorit");
    let left: Vec<_> = std::fs::read_dir(parent.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(left.len(), 1, "bench-i la ose fshiu gjëra te prindi: {left:?}");
}
