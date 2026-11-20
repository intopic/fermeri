//! P-02, P-03: rikuperimi i motorit me faqe.
//!
//! Këto teste dëmtojnë qëllimisht skedarët (bit-flip, rekorde të zhvendosura, format tjetër) dhe
//! kërkojnë që hapja të ndalojë me gabim diagnostikues, pa ndryshuar asnjë skedar. I vetmi rast i
//! toleruar është bishti i papërfunduar pas rekordit të fundit të vlefshëm (shih `tail_*`).
//!
//! Testet e rrëzimit të procesit dhe ato që simulojnë humbjen e shkrimeve të pasinkronizuara janë
//! në `pages.rs` (`crash_*` dhe `unsynced_loss_*`).

use std::collections::BTreeMap;
use std::path::Path;
use zilja_store::{PageOptions, PageTree};
use zilja_tree::StateTree;
use zilja_verifier::AccountMeta;

fn opts() -> PageOptions {
    PageOptions { history: 4, cache_depth: 4, durable: false, ckpt_every: 1_000_000, ..Default::default() }
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

/// Pemë me një checkpoint (snapshot-i) dhe `n` rekorde në log; kthen root-in pas çdo sloti.
fn build(dir: &Path, n: u64) -> Vec<(u64, [u8; 32])> {
    let mut t = PageTree::open(dir, opts()).unwrap();
    let accs = (0..400u32).map(|i| {
        let mut k = [0u8; 32];
        k[..4].copy_from_slice(&i.to_le_bytes());
        k[31] = 0xAA;
        (k, Some(AccountMeta { lamports: i as u64 + 1, ..Default::default() }))
    });
    t.stage_snapshot(1, accs.collect()).unwrap();
    t.finish_snapshot(1, true).unwrap();
    let mut roots = vec![(1, t.root())];
    for s in 2..2 + n {
        let ups = (0..20u8)
            .map(|j| {
                let mut k = [0x11u8; 32];
                k[0] = j;
                k[1] = s as u8;
                (k, Some(AccountMeta { lamports: s * 100 + j as u64, ..Default::default() }))
            })
            .collect();
        t.apply_slot(s, ups, None).unwrap();
        roots.push((s, t.root()));
    }
    roots
}

/// Pozicionet e rekordeve në `meta.log` (koka: magji 4 ‖ gjatësia 8 ‖ crc 4).
fn records(log: &[u8]) -> Vec<(usize, usize)> {
    let mut v = Vec::new();
    let mut at = 0;
    while at + 16 <= log.len() {
        let n = 16 + u64::from_le_bytes(log[at + 4..at + 12].try_into().unwrap()) as usize;
        v.push((at, n));
        at += n;
    }
    v
}

/// Ndryshon log-un, pastaj kërkon që hapja të refuzohet me `want` në mesazh dhe pa prekur skedarët.
fn refused(edit: impl FnOnce(&mut Vec<u8>), want: &str) {
    let d = tempfile::tempdir().unwrap();
    build(d.path(), 8);
    let lp = d.path().join("meta.log");
    let mut log = std::fs::read(&lp).unwrap();
    edit(&mut log);
    std::fs::write(&lp, &log).unwrap();
    let before = files(d.path());
    match PageTree::open(d.path(), opts()) {
        Ok(t) => panic!("hapja duhet të refuzohet ({want}); u hap në slotin {:?}", t.version()),
        Err(e) => assert!(e.contains(want), "mesazhi duhet të përmbajë '{want}': {e}"),
    }
    assert_eq!(files(d.path()), before, "hapja e refuzuar ndryshoi skedarët ({want})");
}

#[test]
fn crc_damage_with_valid_records_after_is_refused() {
    refused(
        |log| {
            let r = records(log);
            log[r[3].0 + 30] ^= 0x10;
        },
        "CRC",
    );
}

#[test]
fn unknown_format_in_the_middle_is_refused() {
    refused(
        |log| {
            let r = records(log);
            log[r[2].0 + 3] = b'9';
        },
        "format",
    );
}

#[test]
fn replayed_record_is_refused() {
    refused(
        |log| {
            let r = records(log);
            let copy = log[r[2].0..r[2].0 + r[2].1].to_vec();
            log.extend_from_slice(&copy);
        },
        "rend",
    );
}

#[test]
fn missing_record_is_refused() {
    refused(
        |log| {
            let r = records(log);
            log.drain(r[4].0..r[4].0 + r[4].1);
        },
        "rend",
    );
}

#[test]
fn absurd_length_is_refused_without_allocating() {
    refused(
        |log| {
            let r = records(log);
            let last = r.last().unwrap().0;
            log.truncate(last);
            let mut h = b"ZLR3".to_vec();
            h.extend_from_slice(&(1u64 << 40).to_le_bytes());
            h.extend_from_slice(&[0; 4]);
            log.extend_from_slice(&h);
        },
        "gjatësi",
    );
}

#[test]
fn log_without_checkpoint_is_refused() {
    let d = tempfile::tempdir().unwrap();
    build(d.path(), 4);
    std::fs::remove_file(d.path().join("meta.ckpt")).unwrap();
    let before = files(d.path());
    let e = PageTree::open(d.path(), opts()).err().expect("log pa checkpoint duhet të refuzohet");
    assert!(e.contains("checkpoint"), "{e}");
    assert_eq!(files(d.path()), before);
}

#[test]
fn page_corruption_is_refused_not_wiped() {
    let d = tempfile::tempdir().unwrap();
    build(d.path(), 4);
    let p = d.path().join("pages.dat");
    let mut b = std::fs::read(&p).unwrap();
    for x in b.iter_mut() {
        *x ^= 0x5a;
    }
    std::fs::write(&p, b).unwrap();
    let before = files(d.path());
    let e = PageTree::open(d.path(), opts()).err().expect("faqet e dëmtuara duhet të refuzohen, jo të fshihen");
    assert!(e.contains("ruajtën") || e.contains("ruajtur"), "mesazhi duhet të thotë që skedarët u ruajtën: {e}");
    assert_eq!(files(d.path()), before, "faqet e dëmtuara u fshinë");
}

/// I vetmi rast i toleruar: bishti pas rekordit të fundit të vlefshëm që s'është kornizë e plotë
/// (kokë ose trup i prerë, zero pas tij). Hapja jep slotin e fundit të plotë.
#[test]
fn tail_after_last_valid_record_is_tolerated() {
    type Edit = fn(&mut Vec<u8>, &[(usize, usize)]);
    let cases: [(&str, Edit, usize); 3] = [
        ("koka e prerë", |l, r| l.truncate(r.last().unwrap().0 + 9), 1),
        ("trupi i prerë", |l, r| l.truncate(r.last().unwrap().0 + r.last().unwrap().1 - 3), 1),
        ("zero pas rekordit të fundit", |l, _| l.extend_from_slice(&[0; 300]), 0),
    ];
    for (name, edit, lost) in cases {
        let d = tempfile::tempdir().unwrap();
        let roots = build(d.path(), 8);
        let lp = d.path().join("meta.log");
        let mut log = std::fs::read(&lp).unwrap();
        let r = records(&log);
        edit(&mut log, &r);
        std::fs::write(&lp, &log).unwrap();
        let t = PageTree::open(d.path(), opts()).unwrap_or_else(|e| panic!("{name}: {e}"));
        let want = roots[roots.len() - 1 - lost];
        assert_eq!((t.version().unwrap(), t.root()), want, "{name}");
        t.verify_integrity().unwrap();
    }
}

/// A04: rekordi i fundit është kornizë e PLOTË (kokë + gjithë trupi) por CRC s'përputhet. Mund të jetë
/// dëmtim i një sloti të konfirmuar: s'hidhet si bisht, hapja refuzohet dhe asnjë skedar s'ndryshon.
#[test]
fn complete_last_frame_with_bad_crc_is_refused() {
    type Edit = fn(&mut Vec<u8>, &[(usize, usize)]);
    let cases: [(&str, Edit); 2] = [
        ("bit i ndryshuar në rekordin e fundit", |l, r| l[r.last().unwrap().0 + 20] ^= 1),
        ("trupi i rekordit të fundit me zero", |l, r| {
            let (a, n) = *r.last().unwrap();
            l[a + 16..a + n].fill(0)
        }),
    ];
    for (name, edit) in cases {
        let d = tempfile::tempdir().unwrap();
        build(d.path(), 8);
        let lp = d.path().join("meta.log");
        let mut log = std::fs::read(&lp).unwrap();
        let r = records(&log);
        edit(&mut log, &r);
        std::fs::write(&lp, &log).unwrap();
        let before = files(d.path());
        let e = PageTree::open(d.path(), opts()).err().unwrap_or_else(|| panic!("{name}: u pranua (u hodh si bisht)"));
        assert!(e.contains("CRC"), "{name}: {e}");
        assert_eq!(files(d.path()), before, "{name}: skedarët ndryshuan");
    }
}

/// A05: checkpoint-i i një instalimi ekzistues humbet menjëherë pas zbrazjes së log-ut. Faqet s'fshihen.
#[test]
fn missing_checkpoint_with_existing_pages_is_refused() {
    let d = tempfile::tempdir().unwrap();
    build(d.path(), 0);
    assert_eq!(std::fs::metadata(d.path().join("meta.log")).unwrap().len(), 0);
    std::fs::remove_file(d.path().join("meta.ckpt")).unwrap();
    let before = files(d.path());
    assert!(!before["pages.dat"].is_empty());
    let e = PageTree::open(d.path(), opts()).err().expect("faqet pa checkpoint u fshinë");
    assert!(e.contains("meta.ckpt"), "{e}");
    assert_eq!(files(d.path()), before, "pages.dat u zbraz");
}

/// A05: ndërtimi i parë i ndërprerë (shënuesi BUILDING) rindërtohet automatikisht.
#[test]
fn interrupted_first_build_restarts_cleanly() {
    let d = tempfile::tempdir().unwrap();
    {
        let mut t = PageTree::open(d.path(), opts()).unwrap();
        t.stage_snapshot(1, vec![([7u8; 32], Some(AccountMeta { lamports: 5, ..Default::default() }))]).unwrap();
        t.flush_staging_for_test().unwrap();
        // "Rrëzim" para finish_snapshot.
    }
    assert!(d.path().join("BUILDING").exists());
    let mut t = PageTree::open(d.path(), opts()).unwrap();
    assert_eq!(t.version(), None);
    t.stage_snapshot(1, vec![([7u8; 32], Some(AccountMeta { lamports: 5, ..Default::default() }))]).unwrap();
    t.finish_snapshot(1, true).unwrap();
    assert!(!d.path().join("BUILDING").exists(), "shënuesi mbeti pas checkpoint-it");
    drop(t);
    let t = PageTree::open(d.path(), opts()).unwrap();
    assert_eq!(t.version(), Some(1));
}

/// A06: shënuesi INVALID ekziston por s'lexohet (këtu: është dosje). S'dihet nëse pema është e
/// vlefshme: hapja refuzohet dhe asgjë s'ndryshon.
#[test]
fn unreadable_invalid_marker_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let roots = build(d.path(), 2);
    std::fs::create_dir(d.path().join("INVALID")).unwrap();
    let before = files(d.path());
    let e = PageTree::open(d.path(), opts()).err().unwrap_or_else(|| panic!("u hap si pemë e vlefshme në slotin {:?}", roots.last()));
    assert!(e.contains("INVALID"), "{e}");
    assert_eq!(files(d.path()), before);
}

/// A26: kovat e staging-ut kontrollohen (numri + SHA-256) para ndërtimit. Një fushë e ndryshuar që
/// dekodohet ende, ose një kovë që mungon, ndalin ndërtimin; pa ndryshime root-i = referenca.
#[test]
fn staging_buckets_are_checked_end_to_end() {
    let accs: Vec<_> = (0..300u32)
        .map(|i| {
            let mut k = [0u8; 32];
            k[0] = (i * 7) as u8;
            k[1..5].copy_from_slice(&i.to_le_bytes());
            (k, Some(AccountMeta { lamports: i as u64 + 1, ..Default::default() }))
        })
        .collect();
    let reference = {
        let d = tempfile::tempdir().unwrap();
        let mut t = PageTree::open(d.path(), opts()).unwrap();
        t.stage_snapshot(9, accs.clone()).unwrap();
        t.finish_snapshot(9, true).unwrap()
    };
    let bucket = |dir: &Path| {
        let mut v: Vec<_> = std::fs::read_dir(dir.join("staging")).unwrap().map(|e| e.unwrap().path()).collect();
        v.sort();
        v[0].clone()
    };
    type Edit = fn(&Path);
    let cases: [(&str, Option<Edit>); 3] = [
        ("pa ndryshim", None),
        ("lamports i ndryshuar", Some(|p| {
            let mut b = std::fs::read(p).unwrap();
            b[49] ^= 0x01;
            std::fs::write(p, b).unwrap();
        })),
        ("kova mungon", Some(|p| std::fs::remove_file(p).unwrap())),
    ];
    for (name, edit) in cases {
        let d = tempfile::tempdir().unwrap();
        let mut t = PageTree::open(d.path(), opts()).unwrap();
        t.stage_snapshot(9, accs.clone()).unwrap();
        t.flush_staging_for_test().unwrap();
        let r = match edit {
            None => t.finish_snapshot(9, true),
            Some(f) => {
                f(&bucket(d.path()));
                t.finish_snapshot(9, true)
            }
        };
        match edit {
            None => assert_eq!(r.unwrap(), reference, "{name}"),
            Some(_) => {
                let e = r.err().unwrap_or_else(|| panic!("{name}: u ndërtua dhe do të shpallej e plotë"));
                assert!(e.contains("staging"), "{name}: {e}");
            }
        }
    }
}

#[test]
fn durable_pages_require_sync_every_slot() {
    let d = tempfile::tempdir().unwrap();
    for n in [0u64, 2, 8] {
        let o = PageOptions { durable: true, sync_every: n, ..Default::default() };
        let e = PageTree::open(d.path(), o).err().unwrap_or_else(|| panic!("sync_every = {n} duhet të refuzohet"));
        assert!(e.contains("sync_every"), "{e}");
    }
    assert!(PageTree::open(d.path(), PageOptions { durable: true, sync_every: 1, ..Default::default() }).is_ok());
}

#[test]
fn invalidate_and_wipe_report_errors() {
    let d = tempfile::tempdir().unwrap();
    build(d.path(), 2);
    let mut t = PageTree::open(d.path(), opts()).unwrap();
    // `INVALID` si dosje: shkrimi i shënimit dështon dhe duhet raportuar.
    std::fs::create_dir(d.path().join("INVALID")).unwrap();
    assert!(t.invalidate("test").is_err(), "gabimi i shënimit 'i pavlefshëm' u injorua");
    std::fs::remove_dir(d.path().join("INVALID")).unwrap();
    t.invalidate("test").unwrap();
    drop(t);
    // Wipe s'mund të fshijë `meta.ckpt.tmp` (dosje): hapja duhet të dështojë, jo të vazhdojë gjysmë.
    std::fs::create_dir(d.path().join("meta.ckpt.tmp")).unwrap();
    assert!(PageTree::open(d.path(), opts()).is_err(), "gabimi i wipe u injorua");
}
