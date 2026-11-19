//! Testi i "rrymës që ikën": nis një proces që shkruan slote pa pushim në pemën në disk,
//! e vret me SIGKILL në një moment të rastësishëm, rihap diskun dhe kontrollon që:
//!
//! 1. gjendja është e plotë: root-i i ruajtur = root-i i rillogaritur nga llogaritë;
//! 2. asnjë slot i konfirmuar si i shkruar nuk humbi.
//!
//! `cargo run -p zilja-bench --release --bin zilja-crash-test -- [raunde]`
//!
//! `-- --snapshot [raunde]` (Safer-6): vret procesin gjatë ndërtimit nga snapshot-i. Pas
//! rihapjes disku duhet të jetë ose bosh (ndërtim i mbetur në mes → rindërtim), ose i plotë.
//!
//! Motori është pages (i vetmi që nga R6; `ZILJA_ENGINE` injorohet).

use rand::{rngs::StdRng, Rng, SeedableRng};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::Duration;
use zilja_store::{PageOptions, PageTree};
use zilja_tree::StateTree;

/// Pema në disk (pages).
struct Disk(Box<PageTree>);

impl Disk {
    fn open(dir: &str) -> Disk {
        Disk(Box::new(PageTree::open(dir, PageOptions { history: 16, ..Default::default() }).unwrap()))
    }
    fn tree(&mut self) -> &mut dyn StateTree {
        self.0.as_mut()
    }
    fn resume_point(&self) -> Option<zilja_tree::Resume> {
        self.0.resume_point()
    }
    fn verify_integrity(&self) -> Result<(), String> {
        self.0.verify_integrity()
    }
}
use zilja_verifier::{AccountMeta, Key};

fn writer(dir: &str) {
    let mut d = Disk::open(dir);
    let t = d.tree();
    let start = match t.resume_point() {
        Some(r) => r.slot + 1,
        None => {
            let mut rng = StdRng::seed_from_u64(0);
            t.stage_snapshot(0, (0..50_000).map(|i| (key(i), Some(meta(&mut rng)))).collect()).unwrap();
            t.finish_snapshot(0, true).unwrap();
            1
        }
    };
    for slot in start.. {
        let mut rng = StdRng::seed_from_u64(slot);
        let ups = (0..3000)
            .map(|_| {
                let i = rng.gen_range(0..80_000u64);
                (key(i), if rng.gen_bool(0.05) { None } else { Some(meta(&mut rng)) })
            })
            .collect();
        t.apply_slot(slot, ups, Some((slot, [slot as u8; 32]))).unwrap();
        // Vetëm pasi shkrimi u krye: "ky slot është në disk".
        println!("{slot}");
    }
}

fn key(i: u64) -> Key {
    let mut k = [0u8; 32];
    k[..8].copy_from_slice(&i.wrapping_mul(0x9E37_79B9_7F4A_7C15).to_le_bytes());
    k[8..16].copy_from_slice(&i.to_le_bytes());
    k
}

fn meta(rng: &mut StdRng) -> AccountMeta {
    AccountMeta { lamports: rng.gen_range(1..1 << 40), data_len: 165, data_hash: rng.gen(), ..Default::default() }
}

fn snap_writer(dir: &str) {
    let mut d = Disk::open(dir);
    let t = d.tree();
    let mut rng = StdRng::seed_from_u64(5);
    for c in 0..40u64 {
        t.stage_snapshot(c % 7, (0..10_000).map(|i| (key(c * 10_000 + i), Some(meta(&mut rng)))).collect()).unwrap();
    }
    t.finish_snapshot(7, true).unwrap();
    println!("done");
}

fn snapshot_kills(exe: &str, rounds: usize) {
    let mut rng = StdRng::seed_from_u64(7);
    let (mut empty, mut full) = (0, 0);
    for round in 1..=rounds {
        let dir = tempdir();
        let mut child = Command::new(exe).args(["--snap-writer", &dir]).stdout(Stdio::null()).spawn().unwrap();
        let wait = Duration::from_millis(rng.gen_range(100..4000));
        std::thread::sleep(wait);
        let _ = child.kill();
        child.wait().unwrap();
        let t = Disk::open(&dir);
        match t.resume_point() {
            None => empty += 1,
            Some(r) => {
                t.verify_integrity().unwrap_or_else(|e| panic!("raundi {round}: ndërtim i dëmtuar: {e}"));
                assert_eq!(r.len, 400_000, "raundi {round}: ndërtim i paplotë u pranua si i plotë");
                full += 1;
            }
        }
        println!(
            "raundi {round:2}: u vra pas {:4} ms → {}",
            wait.as_millis(),
            if t.resume_point().is_some() { "i plotë ✅" } else { "bosh, do të rindërtohet ✅" }
        );
        drop(t);
        let _ = std::fs::remove_dir_all(&dir);
    }
    println!("✅ {rounds} vrasje gjatë ndërtimit: {empty} bosh, {full} të plota, asnjë gjendje gjysmake");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--writer") {
        writer(&args[2]);
        return;
    }
    if args.get(1).map(String::as_str) == Some("--snap-writer") {
        snap_writer(&args[2]);
        return;
    }
    if args.get(1).map(String::as_str) == Some("--snapshot") {
        snapshot_kills(&args[0], args.get(2).and_then(|a| a.parse().ok()).unwrap_or(6));
        return;
    }
    let rounds: usize = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(10);
    let dir = tempdir();
    let mut rng = StdRng::seed_from_u64(99);
    let mut last_confirmed = 0u64;
    for round in 1..=rounds {
        let mut child = Command::new(&args[0]).args(["--writer", &dir]).stdout(Stdio::piped()).spawn().unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                let _ = tx.send(line.parse::<u64>().unwrap());
            }
        });
        let wait = Duration::from_millis(rng.gen_range(300..2500));
        std::thread::sleep(wait);
        child.kill().unwrap(); // SIGKILL: asnjë mbyllje e rregullt
        child.wait().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let confirmed = rx.try_iter().last().unwrap_or(last_confirmed);
        last_confirmed = last_confirmed.max(confirmed);

        let t = Disk::open(&dir);
        let r = t.resume_point().expect("gjendja humbi");
        t.verify_integrity().unwrap_or_else(|e| panic!("raundi {round}: disku i dëmtuar: {e}"));
        assert!(r.slot >= last_confirmed, "raundi {round}: humbi sloti {last_confirmed} (në disk: {})", r.slot);
        println!(
            "raundi {round:2}: u vra pas {:4} ms, sloti i fundit në disk {:4} (i konfirmuar {:4}), {} llogari ✅",
            wait.as_millis(),
            r.slot,
            confirmed,
            r.len
        );
        last_confirmed = r.slot;
    }
    println!("✅ {rounds} vrasje, disku mbeti gjithmonë i plotë dhe pa humbje");
    let _ = std::fs::remove_dir_all(&dir);
}

fn tempdir() -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("zilja-crash-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d.to_string_lossy().into_owned()
}
