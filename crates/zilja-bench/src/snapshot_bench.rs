//! `zilja-snapshot-bench`: numrat realë mbi një snapshot të plotë Testnet/Mainnet, pa nisur validatorin.
//!
//! ```text
//! zilja-snapshot-bench <snapshot-SLOT-HASH.tar.zst> <db_dir> \
//!   --out results.csv --cache-depth 20 --slots 300 --updates 5000,15000,30000 --proofs 5000
//!   [--limit N] [--ingest-only] [--sample 500000] [--seed 1] [--slot S] [--cold none|profile|slot]
//! curl -sL <URL e snapshot-it> | zilja-snapshot-bench - <db_dir> --limit 3000000 …   # nga rrjeti, pa disk
//! ```
//!
//! 1. Lexon snapshot-in në streaming (zstd + tar), pa e çkompresuar në disk: skedarët
//!    `accounts/<slot>.<id>` janë AppendVec. Formati, i verifikuar mbi një snapshot të Agave 4.3.2
//!    (rreshtim 8 bajt, skedari i prerë te gjatësia e përdorur):
//!    `write_version u64 ‖ data_len u64 ‖ pubkey[32] ‖ lamports u64 ‖ rent_epoch u64 ‖ owner[32] ‖
//!    executable u8 ‖ 7 bajt mbushje ‖ hash[32] ‖ data[data_len]`.
//!    Çdo rekord kontrollohet (executable 0/1, data_len ≤ 10 MiB, brenda skedarit); një rekord i
//!    pavlefshëm e ndal bench-in (fail-closed), s'kapërcehet.
//! 2. Llogaritë shkojnë te `PageTree::stage_snapshot(slot_i_skedarit, …)` me copa 65 536; fiton
//!    versioni me slotin më të madh, si në plugin (F-02); `lamports = 0` = e fshirë.
//! 3. `finish_snapshot`, pastaj slote me 75% ndryshime çelësash realë, 15% të reja, 10% fshirje, për
//!    çdo profil `--updates`; matet `prepare_slot`, `commit_slot`, checkpoint-et, RSS/HWM, CPU, I/O.
//! 4. `--proofs` dëftesa (gjysma aktuale, gjysma historike), secila e verifikuar kundër root-it.
//!
//! Cache-i i sistemit (R8): pema sapo është shkruar, pra `pages.dat` është zakonisht i tëri në
//! cache-in e Linux-it dhe slotet matin lexime "të ngrohta". `--cold profile` e nxjerr skedarin nga
//! cache-i (`posix_fadvise(DONTNEED)`, pa root) pas ndërtimit, para çdo profili dhe para dëftesave;
//! `--cold slot` edhe para çdo sloti (rasti më i keq: çdo lexim nga disku). Heqja bëhet jashtë kohës
//! së matur. Për çdo slot shkruhen fazat (`read_ms` = leximi i faqeve, `fsync_ms`, …), faqet e
//! kërkuara nga disku dhe MB të lexuara realisht nga pajisja; `os_cache_hit_pct` tregon sa lexime
//! erdhën nga cache-i. Ndryshimet bien 75% mbi `--sample` llogari të rastit: sa më e madhe mostra,
//! aq më pak ngrohet cache-i gjatë profilit.
//!
//! Dalja: `results.csv` (një rresht për slot), `timeline.csv` (një rresht në sekondë, nga fillimi),
//! `summary.json`. Grafikët: `python3 scripts/plot-bench.py results.csv`.
//!
//! `db_dir` duhet të mos ekzistojë ose të jetë bosh: bench-i s'fshin kurrë diçka që s'e krijoi (A22).

use rand::{rngs::StdRng, Rng, SeedableRng};
use serde_json::{json, Value};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use zilja_store::{PageOptions, PageTree, PAGE_BLOCK};
use zilja_tree::{StateTree, Update};
use zilja_verifier::{tree_key, verify_tree_key, AccountMeta, Key, Verdict};

mod snapshot_rpc;

/// Madhësia e kokës së një rekordi AppendVec (StoredMeta 48 + AccountMeta 56 + hash 32).
pub const HEADER: usize = 136;
/// Kufiri i Solana-s për të dhënat e një llogarie.
pub const MAX_DATA: u64 = 10 * 1024 * 1024;
const CHUNK: usize = 65_536;

/// Një llogari e lexuar nga AppendVec.
pub struct Rec {
    pub pubkey: Key,
    pub lamports: u64,
    pub owner: Key,
    pub executable: bool,
    pub data: Vec<u8>,
}

/// Lexon rekordet e një skedari AppendVec me gjatësi `len`, në radhë, dhe ia jep `f`-së.
/// Gabim për çdo rekord që s'përputhet me formatin.
pub fn parse_append_vec<R: Read>(r: &mut R, len: u64, mut f: impl FnMut(Rec) -> Result<(), String>) -> Result<u64, String> {
    let mut off = 0u64;
    let mut n = 0u64;
    let mut hdr = [0u8; HEADER];
    while off + HEADER as u64 <= len {
        r.read_exact(&mut hdr).map_err(|e| format!("leximi i kokës te {off}: {e}"))?;
        let u64at = |i: usize| u64::from_le_bytes(hdr[i..i + 8].try_into().unwrap());
        let data_len = u64at(8);
        let executable = hdr[96];
        if executable > 1 {
            return Err(format!("rekord i pavlefshëm te {off}: executable = {executable}"));
        }
        if data_len > MAX_DATA || off + HEADER as u64 + data_len > len {
            return Err(format!("rekord i pavlefshëm te {off}: data_len = {data_len} (skedari {len} B)"));
        }
        let mut data = vec![0u8; data_len as usize];
        r.read_exact(&mut data).map_err(|e| format!("leximi i të dhënave te {off}: {e}"))?;
        f(Rec {
            pubkey: hdr[16..48].try_into().unwrap(),
            lamports: u64at(48),
            owner: hdr[64..96].try_into().unwrap(),
            executable: executable == 1,
            data,
        })?;
        n += 1;
        let end = off + HEADER as u64 + data_len;
        let next = (end + 7) & !7;
        // Mbushja deri në 8 (mungon pas rekordit të fundit: skedari është i prerë).
        let pad = next.min(len) - end;
        std::io::copy(&mut r.by_ref().take(pad), &mut std::io::sink()).map_err(|e| e.to_string())?;
        off = next;
    }
    // Bajtet e mbetura (< kokë) duhet të jenë vetëm mbushje.
    if off < len {
        let mut rest = Vec::new();
        r.take(len - off).read_to_end(&mut rest).map_err(|e| e.to_string())?;
        if rest.iter().any(|b| *b != 0) {
            return Err(format!("{} bajte jo-zero pas rekordit të fundit", rest.len()));
        }
    }
    Ok(n)
}

/// Sloti i një skedari `accounts/<slot>.<id>`.
pub fn storage_slot(name: &str) -> Option<u64> {
    let f = name.strip_prefix("accounts/")?;
    let (s, id) = f.split_once('.')?;
    id.parse::<u64>().ok()?;
    s.parse().ok()
}

/// Sloti nga emri `snapshot-<slot>-<hash>.tar.zst`.
pub fn archive_slot(p: &Path) -> Option<u64> {
    p.file_name()?.to_str()?.strip_prefix("snapshot-")?.split('-').next()?.parse().ok()
}

#[derive(Default, Clone, Debug)]
pub struct IngestStats {
    pub storages: u64,
    pub records: u64,
    pub zero_lamport: u64,
    pub data_bytes: u64,
    pub max_slot: u64,
    pub compressed_bytes: u64,
}

/// Lexon snapshot-in (`.tar.zst`) dhe i jep llogaritë `tree`-së. `sample` mbledh me reservoir
/// sampling deri në `sample_n` pubkey llogarish ekzistuese (R9: pubkey, jo `tree_key`, që të
/// pyeten edhe mbi RPC). `limit` ndal pas kaq rekordesh (teste).
pub fn ingest(
    archive: &Path,
    tree: &mut dyn StateTree,
    sample_n: usize,
    rng: &mut StdRng,
    limit: Option<u64>,
    progress: &dyn Fn(&IngestStats),
) -> Result<(IngestStats, Vec<Key>), String> {
    // `-` = stdin: arkivi mund të vijë direkt nga rrjeti (`curl … | zilja-snapshot-bench - …`), pa u
    // ruajtur në disk (runner-ët e GitHub-it kanë 14 GB).
    let input: Box<dyn Read> = if archive == Path::new("-") {
        Box::new(std::io::stdin().lock())
    } else {
        Box::new(std::fs::File::open(archive).map_err(|e| format!("{}: {e}", archive.display()))?)
    };
    let counted = Counted { inner: input, n: 0 };
    let mut dec = zstd::stream::read::Decoder::new(BufReader::with_capacity(8 << 20, counted)).map_err(|e| format!("zstd: {e}"))?;
    let st = ingest_from(&mut dec, tree, sample_n, rng, limit, progress)?;
    let compressed_bytes = dec.get_ref().get_ref().get_ref().n;
    Ok((IngestStats { compressed_bytes, ..st.0 }, st.1))
}

/// Numëron bajtet e lexuara (madhësia e arkivit kur vjen nga stdin).
struct Counted<R> {
    inner: R,
    n: u64,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let k = self.inner.read(buf)?;
        self.n += k as u64;
        Ok(k)
    }
}

fn ingest_from<R: Read>(
    dec: &mut R,
    tree: &mut dyn StateTree,
    sample_n: usize,
    rng: &mut StdRng,
    limit: Option<u64>,
    progress: &dyn Fn(&IngestStats),
) -> Result<(IngestStats, Vec<Key>), String> {
    let mut ar = tar::Archive::new(dec);
    let mut st = IngestStats::default();
    let mut sample: Vec<Key> = Vec::with_capacity(sample_n);
    let mut seen_live = 0u64;
    let mut last_progress = Instant::now();
    for entry in ar.entries().map_err(|e| format!("tar: {e}"))? {
        let mut entry = entry.map_err(|e| format!("tar: {e}"))?;
        let name = entry.path().map_err(|e| e.to_string())?.to_string_lossy().to_string();
        let Some(slot) = storage_slot(&name) else { continue };
        let len = entry.size();
        st.storages += 1;
        st.max_slot = st.max_slot.max(slot);
        let mut chunk: Vec<Update> = Vec::with_capacity(CHUNK.min(1 << 16));
        let mut stop = false;
        let mut err: Option<String> = None;
        let flush = |chunk: &mut Vec<Update>, tree: &mut dyn StateTree| -> Result<(), String> {
            if !chunk.is_empty() {
                tree.stage_snapshot(slot, std::mem::take(chunk))?;
            }
            Ok(())
        };
        let parsed = parse_append_vec(&mut entry, len, |r| {
            if stop {
                return Ok(());
            }
            st.records += 1;
            st.data_bytes += r.data.len() as u64;
            let tk = tree_key(&r.pubkey);
            let meta = if r.lamports == 0 {
                st.zero_lamport += 1;
                None
            } else {
                seen_live += 1;
                if sample.len() < sample_n {
                    sample.push(r.pubkey);
                } else {
                    let j = rng.gen_range(0..seen_live);
                    if (j as usize) < sample_n {
                        sample[j as usize] = r.pubkey;
                    }
                }
                Some(AccountMeta::from_account(r.lamports, &r.owner, r.executable, &r.data))
            };
            chunk.push((tk, meta));
            if chunk.len() >= CHUNK {
                if let Err(e) = flush(&mut chunk, tree) {
                    err = Some(e);
                    stop = true;
                }
            }
            if limit.is_some_and(|l| st.records >= l) {
                stop = true;
            }
            Ok(())
        });
        parsed.map_err(|e| format!("{name}: {e}"))?;
        if let Some(e) = err {
            return Err(e);
        }
        flush(&mut chunk, tree)?;
        if last_progress.elapsed().as_secs() >= 10 {
            progress(&st);
            last_progress = Instant::now();
        }
        if stop && limit.is_some_and(|l| st.records >= l) {
            break;
        }
    }
    Ok((st, sample))
}

// ---------------------------------------------------------------- matjet e procesit

fn proc_status(key: &str) -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with(key)).and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok()))
        .unwrap_or(0.0)
        / 1024.0
}

/// (utime + stime) në sekonda (supozohet CLK_TCK = 100, si në Linux x86/ARM).
fn cpu_secs() -> f64 {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let after = s.rsplit_once(')').map(|x| x.1).unwrap_or("");
    let f: Vec<&str> = after.split_whitespace().collect();
    let t = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    (t(11) + t(12)) / 100.0
}

/// (read_bytes, write_bytes) realë drejt diskut.
fn proc_io() -> (u64, u64) {
    let s = std::fs::read_to_string("/proc/self/io").unwrap_or_default();
    let get = |k: &str| s.lines().find(|l| l.starts_with(k)).and_then(|l| l.split_whitespace().nth(1)?.parse().ok()).unwrap_or(0);
    (get("read_bytes:"), get("write_bytes:"))
}

/// Nxjerr skedarin nga cache-i i sistemit (vetëm faqet e pastra: pema bën fsync çdo slot).
/// Cache-i është për inode, kështu që mjafton një fd i ri mbi të njëjtin skedar.
fn drop_os_cache(p: &Path) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        let f = std::fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
        // SAFETY: fd i vlefshëm gjatë gjithë thirrjes; posix_fadvise s'prek memorie.
        let r = unsafe { libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
        if r != 0 {
            return Err(format!("posix_fadvise({}): {}", p.display(), std::io::Error::from_raw_os_error(r)));
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = p;
        Err("--cold punon vetëm në Linux (edhe WSL2)".into())
    }
}

fn meminfo_gb(key: &str) -> f64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with(key)).and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok()))
        .unwrap_or(0.0)
        / 1048576.0
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Cold {
    None,
    Profile,
    Slot,
}

fn file_gb(p: &Path) -> f64 {
    std::fs::metadata(p).map(|m| m.len() as f64 / 1e9).unwrap_or(0.0)
}

fn pct(v: &mut [f64], q: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * q).round() as usize]
}

fn stats(mut v: Vec<f64>) -> Value {
    let max = v.iter().cloned().fold(0.0, f64::max);
    let mean = if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 };
    json!({"p50": pct(&mut v, 0.5), "p95": pct(&mut v, 0.95), "p99": pct(&mut v, 0.99), "max": max, "mean": mean, "n": v.len()})
}

const PHASES: [&str; 6] = ["staging", "finish", "slots", "proofs", "rpc", "done"];

/// Një rresht në sekondë: koha, faza, RSS, CPU, I/O.
fn timeline(path: PathBuf, phase: Arc<AtomicU8>, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let Ok(mut f) = std::fs::File::create(&path) else { return };
        let _ = writeln!(f, "t_s,phase,rss_mb,hwm_mb,cpu_pct,read_mb_s,write_mb_s");
        let t0 = Instant::now();
        let (mut c0, mut io0, mut w0) = (cpu_secs(), proc_io(), Instant::now());
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let (c1, io1) = (cpu_secs(), proc_io());
            let dt = w0.elapsed().as_secs_f64().max(1e-6);
            let _ = writeln!(
                f,
                "{:.1},{},{:.0},{:.0},{:.0},{:.1},{:.1}",
                t0.elapsed().as_secs_f64(),
                PHASES[phase.load(Ordering::Relaxed) as usize],
                proc_status("VmRSS:"),
                proc_status("VmHWM:"),
                100.0 * (c1 - c0) / dt,
                (io1.0 - io0.0) as f64 / 1e6 / dt,
                (io1.1 - io0.1) as f64 / 1e6 / dt
            );
            let _ = f.flush();
            (c0, io0, w0) = (c1, io1, Instant::now());
        }
    })
}

struct Args {
    archive: PathBuf,
    db: PathBuf,
    out: PathBuf,
    cache_depth: u16,
    slots: u64,
    updates: Vec<usize>,
    proofs: usize,
    sample: usize,
    limit: Option<u64>,
    ingest_only: bool,
    seed: u64,
    /// Sloti i snapshot-it, kur s'del nga emri i skedarit (p.sh. stdin).
    slot: Option<u64>,
    cold: Cold,
    rpc: snapshot_rpc::RpcOpts,
}

fn parse_args() -> Result<Args, String> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let usage = "përdorimi: zilja-snapshot-bench <snapshot.tar.zst> <db_dir> [--out results.csv] [--cache-depth 20] [--slots 300] [--updates 5000,15000,30000] [--proofs 5000] [--sample 500000] [--limit N] [--ingest-only] [--seed 1] [--slot S] [--cold none|profile|slot] [--rpc-clients 1,8,32,128] [--rpc-seconds 30] [--rpc-slot-ms 400] [--rpc-slot-updates 5000] [--rpc-verify-every 100] [--rpc-multi-keys 10] [--rpc-one-ip]\n<snapshot.tar.zst> mund të jetë `-` (stdin): curl -sL URL | zilja-snapshot-bench - <db_dir> --limit 3000000 …";
    if a.len() < 2 {
        return Err(usage.into());
    }
    let mut r = Args {
        archive: a[0].clone().into(),
        db: a[1].clone().into(),
        out: "results.csv".into(),
        cache_depth: 20,
        slots: 300,
        updates: vec![5_000, 15_000, 30_000],
        proofs: 5_000,
        sample: 500_000,
        limit: None,
        ingest_only: false,
        seed: 1,
        slot: None,
        cold: Cold::None,
        rpc: snapshot_rpc::RpcOpts { clients: vec![], seconds: 30, slot_ms: 400, slot_updates: 5_000, verify_every: 100, multi_keys: 10, one_ip: false },
    };
    let mut it = a[2..].iter();
    while let Some(k) = it.next() {
        let mut v = || it.next().cloned().ok_or(format!("{k} kërkon vlerë\n{usage}"));
        let num = |s: String| s.parse::<u64>().map_err(|_| format!("{k}: numër i pavlefshëm {s:?}"));
        match k.as_str() {
            "--out" => r.out = v()?.into(),
            "--cache-depth" => r.cache_depth = num(v()?)? as u16,
            "--slots" => r.slots = num(v()?)?,
            "--updates" => r.updates = v()?.split(',').map(|x| x.trim().parse::<usize>().map_err(|_| format!("--updates: {x:?}"))).collect::<Result<_, _>>()?,
            "--proofs" => r.proofs = num(v()?)? as usize,
            "--sample" => r.sample = num(v()?)? as usize,
            "--limit" => r.limit = Some(num(v()?)?),
            "--seed" => r.seed = num(v()?)?,
            "--slot" => r.slot = Some(num(v()?)?),
            "--ingest-only" => r.ingest_only = true,
            "--rpc-clients" => r.rpc.clients = v()?.split(',').map(|x| x.trim().parse::<usize>().map_err(|e| format!("--rpc-clients: {e}"))).collect::<Result<_, _>>()?,
            "--rpc-seconds" => r.rpc.seconds = v()?.parse().map_err(|e| format!("--rpc-seconds: {e}"))?,
            "--rpc-slot-ms" => r.rpc.slot_ms = v()?.parse().map_err(|e| format!("--rpc-slot-ms: {e}"))?,
            "--rpc-slot-updates" => r.rpc.slot_updates = v()?.parse().map_err(|e| format!("--rpc-slot-updates: {e}"))?,
            "--rpc-verify-every" => r.rpc.verify_every = v()?.parse().map_err(|e| format!("--rpc-verify-every: {e}"))?,
            "--rpc-multi-keys" => r.rpc.multi_keys = v()?.parse().map_err(|e| format!("--rpc-multi-keys: {e}"))?,
            "--rpc-one-ip" => r.rpc.one_ip = true,
            "--cold" => {
                r.cold = match v()?.as_str() {
                    "none" => Cold::None,
                    "profile" => Cold::Profile,
                    "slot" => Cold::Slot,
                    x => return Err(format!("--cold: {x:?} (none, profile, slot)")),
                }
            }
            _ => return Err(format!("argument i panjohur {k:?}\n{usage}")),
        }
    }
    if r.cache_depth % 4 != 0 {
        return Err("--cache-depth duhet shumëfish i 4".into());
    }
    if r.rpc.multi_keys == 0 || r.rpc.multi_keys > zilja_verifier::multi::MAX_MULTI_KEYS {
        return Err(format!("--rpc-multi-keys: 1..={}", zilja_verifier::multi::MAX_MULTI_KEYS));
    }
    Ok(r)
}

fn rand_meta(rng: &mut StdRng) -> AccountMeta {
    let len = if rng.gen_bool(0.7) { 0 } else { rng.gen_range(0..512) };
    let data: Vec<u8> = (0..len).map(|_| rng.gen()).collect();
    AccountMeta::from_account(rng.gen_range(890_880..10_000_000_000), &rng.gen(), false, &data)
}

fn run() -> Result<(), String> {
    let a = parse_args()?;
    if a.db.exists() && std::fs::read_dir(&a.db).map_err(|e| e.to_string())?.next().is_some() {
        return Err(format!("{} ekziston dhe s'është bosh: bench-i s'fshin asgjë që s'e krijoi (A22); zgjidh një dosje të re", a.db.display()));
    }
    std::fs::create_dir_all(&a.db).map_err(|e| e.to_string())?;
    let out_dir = a.out.parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf).unwrap_or_else(|| ".".into());
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let phase = Arc::new(AtomicU8::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let tl = timeline(out_dir.join("timeline.csv"), phase.clone(), stop.clone());
    let mem_avail0 = meminfo_gb("MemAvailable:");
    if a.cold != Cold::None {
        // Gabimi del këtu, para orëve të ndërtimit, jo pas tyre.
        let probe = a.db.join(".cold-probe");
        std::fs::write(&probe, [0u8; 4096]).map_err(|e| e.to_string())?;
        let r = drop_os_cache(&probe);
        let _ = std::fs::remove_file(&probe);
        r?;
    }
    let mut rng = StdRng::seed_from_u64(a.seed);
    let opts = PageOptions { history: 32, cache_depth: a.cache_depth, durable: true, ..Default::default() };
    let mut tree = PageTree::open(&a.db, opts).map_err(|e| format!("PageTree: {e}"))?;

    // 1. Staging.
    let t_stage = Instant::now();
    eprintln!("zilja-snapshot-bench: po lexon {} ...", a.archive.display());
    let (ist, pubs) = ingest(&a.archive, &mut tree, a.sample, &mut rng, a.limit, &|s| {
        eprintln!("  {} skedarë, {} rekorde, {:.1} GB të dhëna, RSS {:.0} MB", s.storages, s.records, s.data_bytes as f64 / 1e9, proc_status("VmRSS:"));
    })?;
    let stage_s = t_stage.elapsed().as_secs_f64();
    let mut sample: Vec<Key> = pubs.iter().map(tree_key).collect();
    let snap_slot = a.slot.or_else(|| archive_slot(&a.archive)).unwrap_or(ist.max_slot);
    let hwm_stage = proc_status("VmHWM:");

    // 2. Ndërtimi i pemës.
    phase.store(1, Ordering::Relaxed);
    let t_fin = Instant::now();
    let root = tree.finish_snapshot(snap_slot, a.limit.is_none()).map_err(|e| format!("finish_snapshot: {e}"))?;
    let finish_s = t_fin.elapsed().as_secs_f64();
    let hwm_build = proc_status("VmHWM:");
    let pages = a.db.join("pages.dat");
    let accounts = tree.len();
    eprintln!(
        "pema: {accounts} llogari, root {}, sloti {snap_slot}, staging {stage_s:.0} s, ndërtimi {finish_s:.0} s, {:.1} GB, RAM maksimal {:.0} MB",
        hex::encode(root),
        file_gb(&pages),
        hwm_build
    );
    let mut summary = json!({
        "archive": a.archive.display().to_string(),
        "snapshot_slot": snap_slot,
        "root": hex::encode(root),
        "accounts": accounts,
        "ingest": {
            "storages": ist.storages, "records": ist.records, "zero_lamport_records": ist.zero_lamport,
            "data_gb": ist.data_bytes as f64 / 1e9, "compressed_gb": ist.compressed_bytes as f64 / 1e9,
            "stage_seconds": stage_s, "records_per_second": ist.records as f64 / stage_s.max(1e-9),
            "hwm_mb_after_staging": hwm_stage,
        },
        "build": {
            "finish_seconds": finish_s, "hwm_mb": hwm_build, "pages_gb": file_gb(&pages),
            "disk_bytes_per_account": if accounts > 0 { file_gb(&pages) * 1e9 / accounts as f64 } else { 0.0 },
            "packer_peak_blocks": tree.snapshot_peak_blocks(),
        },
        "machine": {
            "cpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
            "mem_total_gb": std::fs::read_to_string("/proc/meminfo").ok().and_then(|s| s.lines().next().and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())).unwrap_or(0.0) / 1048576.0,
            "cpu_model": std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|s| s.lines().find(|l| l.starts_with("model name")).map(|l| l.split(':').nth(1).unwrap_or("").trim().to_string())),
        },
        "cache_depth": a.cache_depth,
        "params": {
            "cold": format!("{:?}", a.cold).to_lowercase(), "sample": a.sample, "slots": a.slots, "updates": a.updates,
            "proofs": a.proofs, "seed": a.seed, "limit": a.limit,
        },
        "ram": {
            "mem_available_gb_at_start": mem_avail0, "pages_gb_vs_mem_total": file_gb(&pages) / meminfo_gb("MemTotal:").max(1e-9),
            "wsl2": std::fs::read_to_string("/proc/version").map(|v| v.to_lowercase().contains("microsoft")).unwrap_or(false),
        },
    });
    if a.ingest_only || sample.is_empty() {
        phase.store(4, Ordering::Relaxed);
        std::fs::write(out_dir.join("summary.json"), serde_json::to_string_pretty(&summary).unwrap()).map_err(|e| e.to_string())?;
        stop.store(true, Ordering::Relaxed);
        let _ = tl.join();
        println!("{}", serde_json::to_string_pretty(&summary).unwrap());
        return Ok(());
    }

    // 3. Slotet.
    phase.store(2, Ordering::Relaxed);
    let csv = Mutex::new(std::fs::File::create(&a.out).map_err(|e| format!("{}: {e}", a.out.display()))?);
    writeln!(
        csv.lock().unwrap(),
        "profile,slot,updates,prepare_ms,commit_ms,total_ms,checkpoint,rss_mb,hwm_mb,cpu_pct,read_mb_s,write_mb_s,pages_gb,read_ms,tree_ms,build_ms,pwrite_ms,fsync_ms,log_ms,disk_reads,disk_read_mb,pages_written"
    )
    .map_err(|e| e.to_string())?;
    let mut slot = snap_slot;
    let mut profiles = serde_json::Map::new();
    let mut created: Vec<Key> = Vec::new();
    let mut cold_warn: Vec<String> = Vec::new();
    let ms = |d: std::time::Duration| d.as_secs_f64() * 1e3;
    for &n in &a.updates {
        if a.cold != Cold::None {
            drop_os_cache(&pages)?;
        }
        let (mut prep, mut comm, mut tot, mut ck, mut tot_nock) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let (mut rd, mut fs, mut lg, mut reads, mut rmb) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let (mut sum_reads, mut sum_read_bytes) = (0u64, 0u64);
        // Sloti i parë pas heqjes nga cache-i: (faqe, bajte nga pajisja). Faqet e shkruara më pas
        // janë natyrshëm në cache, kështu që vetëm ky slot tregon nëse heqja pati efekt.
        let mut first_cold: Option<(u64, u64)> = None;
        for _ in 0..a.slots {
            slot += 1;
            let mut ups: Vec<Update> = Vec::with_capacity(n);
            for _ in 0..n {
                let r: f64 = rng.gen();
                if r < 0.75 && !sample.is_empty() {
                    ups.push((sample[rng.gen_range(0..sample.len())], Some(rand_meta(&mut rng))));
                } else if r < 0.90 || sample.len() < 2 {
                    let k: Key = rng.gen();
                    created.push(k);
                    ups.push((k, Some(rand_meta(&mut rng))));
                } else {
                    let i = rng.gen_range(0..sample.len());
                    ups.push((sample.swap_remove(i), None));
                }
            }
            // Llogaritë e reja bëhen pjesë e mostrës (ndryshohen/fshihen më vonë).
            sample.append(&mut created);
            if a.cold == Cold::Slot {
                drop_os_cache(&pages)?;
            }
            let (c0, io0) = (cpu_secs(), proc_io());
            let t0 = Instant::now();
            let p = tree.prepare_slot(slot, ups, None).map_err(|e| format!("prepare_slot {slot}: {e}"))?;
            let t1 = Instant::now();
            tree.commit_slot(slot, p).map_err(|e| format!("commit_slot {slot}: {e}"))?;
            let t2 = Instant::now();
            let (c1, io1) = (cpu_secs(), proc_io());
            let dt = (t2 - t0).as_secs_f64().max(1e-9);
            let st = tree.last_timing().unwrap_or_default();
            let (pm, cm, tm) = ((t1 - t0).as_secs_f64() * 1e3, (t2 - t1).as_secs_f64() * 1e3, dt * 1e3);
            let read_bytes = io1.0.saturating_sub(io0.0);
            prep.push(pm);
            comm.push(cm);
            tot.push(tm);
            rd.push(ms(st.read));
            fs.push(ms(st.fsync));
            lg.push(ms(st.commit));
            reads.push(st.disk_reads as f64);
            rmb.push(read_bytes as f64 / 1e6);
            sum_reads += st.disk_reads;
            sum_read_bytes += read_bytes;
            if a.cold != Cold::None && first_cold.is_none() {
                first_cold = Some((st.disk_reads, read_bytes));
            }
            if st.checkpoint {
                ck.push(tm);
            } else {
                tot_nock.push(tm);
            }
            writeln!(
                csv.lock().unwrap(),
                "{n},{slot},{n},{pm:.3},{cm:.3},{tm:.3},{},{:.0},{:.0},{:.0},{:.1},{:.1},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{},{:.3},{}",
                st.checkpoint as u8,
                proc_status("VmRSS:"),
                proc_status("VmHWM:"),
                100.0 * (c1 - c0) / dt,
                read_bytes as f64 / 1e6 / dt,
                (io1.1 - io0.1) as f64 / 1e6 / dt,
                file_gb(&pages),
                ms(st.read),
                ms(st.tree),
                ms(st.build),
                ms(st.pwrite),
                ms(st.fsync),
                ms(st.commit),
                st.disk_reads,
                read_bytes as f64 / 1e6,
                st.pages
            )
            .map_err(|e| e.to_string())?;
        }
        // Sa nga faqet e kërkuara erdhën nga cache-i: 1 − (bajte nga pajisja) / (faqe × 4 KB).
        // I përafërt: disa faqe ndajnë një bllok 4 KB; readahead-i i kernelit e ul (kufizohet në 0).
        let asked = sum_reads as f64 * PAGE_BLOCK as f64;
        let hit = if asked > 0.0 { (100.0 * (1.0 - sum_read_bytes as f64 / asked)).clamp(0.0, 100.0) } else { 100.0 };
        if let Some((r, b)) = first_cold {
            if r >= 100 && (b as f64) < 0.05 * r as f64 * PAGE_BLOCK as f64 {
                cold_warn.push(format!(
                    "profili {n}: --cold s'pati efekt: sloti i parë pas heqjes nga cache-i kërkoi {r} faqe, por pajisja lexoi vetëm {:.2} MB (cache jashtë VM-së, p.sh. WSL2/Windows, ose /proc/self/io s'numëron)",
                    b as f64 / 1e6
                ));
            }
        }
        eprintln!(
            "profili {n}/slot: total p50 {:.1} ms, p99 {:.1} ms, max {:.1} ms | leximi p50 {:.1} ms, p99 {:.1} ms | fsync p50 {:.1} ms | {} checkpoint-e, max {:.1} ms | {:.0} faqe/slot nga disku, {:.1} MB/slot nga pajisja, cache {hit:.0}%",
            pct(&mut tot.clone(), 0.5),
            pct(&mut tot.clone(), 0.99),
            tot.iter().cloned().fold(0.0, f64::max),
            pct(&mut rd.clone(), 0.5),
            pct(&mut rd.clone(), 0.99),
            pct(&mut fs.clone(), 0.5),
            ck.len(),
            ck.iter().cloned().fold(0.0, f64::max),
            sum_reads as f64 / a.slots.max(1) as f64,
            sum_read_bytes as f64 / 1e6 / a.slots.max(1) as f64,
        );
        profiles.insert(
            n.to_string(),
            json!({
                "slots": a.slots, "prepare_ms": stats(prep), "commit_ms": stats(comm), "total_ms": stats(tot.clone()),
                "total_ms_without_checkpoints": stats(tot_nock),
                "checkpoint_ms": stats(ck), "updates_per_second": (n as f64 * a.slots as f64) / (tot.iter().sum::<f64>() / 1e3).max(1e-9),
                "read_ms": stats(rd), "fsync_ms": stats(fs), "log_ms": stats(lg),
                "disk_reads_per_slot": stats(reads), "device_read_mb_per_slot": stats(rmb),
                "os_cache_hit_pct": hit, "device_bytes_per_page_read": if sum_reads > 0 { sum_read_bytes as f64 / sum_reads as f64 } else { 0.0 },
            }),
        );
    }
    summary["slots"] = Value::Object(profiles);
    summary["after_slots"] = json!({"hwm_mb": proc_status("VmHWM:"), "rss_mb": proc_status("VmRSS:"), "pages_gb": file_gb(&pages), "accounts": tree.len()});
    if !cold_warn.is_empty() {
        for w in &cold_warn {
            eprintln!("KUJDES: {w}");
        }
        summary["warnings"] = json!(cold_warn);
    }

    // 4. Dëftesat, secila e verifikuar.
    phase.store(3, Ordering::Relaxed);
    let (lo, hi) = tree.provable_range().ok_or("pema s'jep dëftesa")?;
    if a.cold != Cold::None {
        drop_os_cache(&pages)?;
    }
    let (mut lat_now, mut lat_hist, mut sizes) = (Vec::new(), Vec::new(), Vec::new());
    let mut bad = 0u64;
    for i in 0..a.proofs {
        let k = if rng.gen_bool(0.9) && !sample.is_empty() { sample[rng.gen_range(0..sample.len())] } else { rng.gen() };
        let at = if i % 2 == 0 { None } else { Some(rng.gen_range(lo..=hi)) };
        let t = Instant::now();
        let (_, root, proof, meta) = tree.prove_at(&k, at).map_err(|e| format!("prove_at: {e}"))?;
        let ms = t.elapsed().as_secs_f64() * 1e3;
        if at.is_none() { lat_now.push(ms) } else { lat_hist.push(ms) }
        sizes.push(proof.to_bytes().len() as f64);
        if verify_tree_key(&root, &k, meta.as_ref(), &proof) == Verdict::Invalid {
            bad += 1;
        }
    }
    summary["proofs"] = json!({
        "n": a.proofs, "invalid": bad, "current_ms": stats(lat_now), "historic_ms": stats(lat_hist),
        "bytes": stats(sizes), "provable_range": [lo, hi],
    });
    if bad > 0 {
        return Err(format!("{bad} dëftesa NUK u verifikuan: pema ose bench-i ka gabim"));
    }
    // 5. Dëftesa nga shumë klientë njëkohësisht, mbi serverin e vërtetë, me slote në sfond (R9).
    if !a.rpc.clients.is_empty() {
        phase.store(4, Ordering::Relaxed);
        let (complete, root_now) = (a.limit.is_none(), tree.root());
        eprintln!("rpc: {:?} klientë × {} s, slot çdo {} ms me {} ndryshime …", a.rpc.clients, a.rpc.seconds, a.rpc.slot_ms, a.rpc.slot_updates);
        let r = snapshot_rpc::run(Box::new(tree), complete, slot, root_now, pubs, sample, &a.rpc, a.seed, a.cold, &pages);
        match r {
            Ok(v) => summary["rpc"] = v,
            Err(e) => {
                summary["rpc_error"] = json!(e);
                let _ = std::fs::write(out_dir.join("summary.json"), serde_json::to_string_pretty(&summary).unwrap());
                return Err(e);
            }
        }
    }
    phase.store(5, Ordering::Relaxed);
    std::fs::write(out_dir.join("summary.json"), serde_json::to_string_pretty(&summary).unwrap()).map_err(|e| e.to_string())?;
    stop.store(true, Ordering::Relaxed);
    let _ = tl.join();
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
    eprintln!("gati: {}, {}, {}", a.out.display(), out_dir.join("timeline.csv").display(), out_dir.join("summary.json").display());
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("gabim: {e}");
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zilja_tree::Tree;

    /// Një rekord AppendVec në formatin e Agave 4.3.2.
    fn rec(out: &mut Vec<u8>, pk: Key, lamports: u64, owner: Key, exec: bool, data: &[u8], last: bool) {
        out.extend_from_slice(&7u64.to_le_bytes());
        out.extend_from_slice(&(data.len() as u64).to_le_bytes());
        out.extend_from_slice(&pk);
        out.extend_from_slice(&lamports.to_le_bytes());
        out.extend_from_slice(&u64::MAX.to_le_bytes());
        out.extend_from_slice(&owner);
        out.push(exec as u8);
        out.extend_from_slice(&[0; 7]);
        out.extend_from_slice(&[0xee; 32]);
        out.extend_from_slice(data);
        if !last {
            while out.len() % 8 != 0 {
                out.push(0);
            }
        }
    }

    fn archive(files: &[(String, Vec<u8>)]) -> tempfile::NamedTempFile {
        let f = tempfile::Builder::new().prefix("snapshot-500-x").suffix(".tar.zst").tempfile().unwrap();
        let enc = zstd::stream::write::Encoder::new(f.reopen().unwrap(), 3).unwrap();
        let mut b = tar::Builder::new(enc);
        let mut add = |name: &str, data: &[u8]| {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, name, data).unwrap();
        };
        add("version", b"1.2.0");
        add("snapshots/500/500", b"bank");
        for (n, d) in files {
            add(n, d);
        }
        b.into_inner().unwrap().finish().unwrap();
        f
    }

    /// Përmbledhja e plotë: disa skedarë, versione të vjetra në slote më të ulëta, fshirje me
    /// lamports = 0, të dhëna me gjatësi jo-shumëfish 8, skedar i prerë → root i njëjtë me RAM-in.
    #[test]
    fn synthetic_snapshot_root_equals_memory_tree() {
        let mut rng = StdRng::seed_from_u64(7);
        let mut expect: std::collections::HashMap<Key, AccountMeta> = Default::default();
        let mut files = Vec::new();
        let keys: Vec<Key> = (0..400).map(|_| rng.gen()).collect();
        for (slot, id) in [(100u64, 0u64), (200, 1), (300, 2)] {
            let mut buf = Vec::new();
            let picks: Vec<usize> = (0..250).map(|_| rng.gen_range(0..keys.len())).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
            for (j, &i) in picks.iter().enumerate() {
                let data: Vec<u8> = (0..rng.gen_range(0..90)).map(|_| rng.gen()).collect();
                let lamports = if slot > 100 && rng.gen_bool(0.1) { 0 } else { rng.gen_range(1..1 << 40) };
                let owner: Key = rng.gen();
                let exec = rng.gen_bool(0.05);
                rec(&mut buf, keys[i], lamports, owner, exec, &data, j + 1 == picks.len());
                if lamports == 0 {
                    expect.remove(&keys[i]);
                } else {
                    expect.insert(keys[i], AccountMeta::from_account(lamports, &owner, exec, &data));
                }
            }
            files.push((format!("accounts/{slot}.{id}"), buf));
        }
        // Radha në arkiv s'ka rëndësi: fiton sloti më i madh.
        files.reverse();
        let ar = archive(&files);
        let dir = tempfile::tempdir().unwrap();
        let mut tree = PageTree::open(dir.path(), PageOptions { history: 4, cache_depth: 8, durable: false, ..Default::default() }).unwrap();
        let (st, sample) = ingest(ar.path(), &mut tree, 50, &mut rng, None, &|_| {}).unwrap();
        let root = tree.finish_snapshot(300, true).unwrap();
        let mem = Tree::from_accounts(expect.iter().map(|(k, m)| (tree_key(k), Some(m.clone()))).collect());
        assert_eq!(st.storages, 3);
        assert_eq!(root, mem.root());
        assert_eq!(tree.len(), expect.len());
        assert_eq!(sample.len(), 50);
        assert_eq!(archive_slot(Path::new("/x/snapshot-123-abc.tar.zst")), Some(123));
        // Mostra mban pubkey llogarish ekzistuese (R9: pyeten mbi RPC).
        assert!(sample.iter().all(|k| expect.contains_key(k)));

        // R9: faza RPC mbi të njëjtën pemë, me slote në sfond: çdo përgjigje verifikohet.
        let tks: Vec<Key> = sample.iter().map(tree_key).collect();
        let o = snapshot_rpc::RpcOpts { clients: vec![1, 4], seconds: 1, slot_ms: 50, slot_updates: 20, verify_every: 1, multi_keys: 5, one_ip: true };
        let r = snapshot_rpc::run(Box::new(tree), true, 300, root, sample, tks, &o, 1, Cold::None, &dir.path().join("pages.dat")).unwrap();
        let lv = r["levels"].as_array().unwrap();
        assert_eq!(lv.len(), 3, "1, 4 dhe 4 nga një IP");
        for l in lv {
            assert_eq!(l["invalid"], 0);
            assert!(l["verified"].as_u64().unwrap() > 0);
            if !l["one_ip"].as_bool().unwrap() {
                assert_eq!(l["error_pct"].as_f64().unwrap(), 0.0, "{l}");
            }
        }
        assert!(lv[0]["writer"]["slots"].as_u64().unwrap() > 0, "slotet vazhdojnë gjatë ngarkesës");
    }

    /// Një rekord që s'përputhet me formatin ndal leximin (s'kapërcehet në heshtje).
    #[test]
    fn malformed_records_fail_closed() {
        let mut buf = Vec::new();
        rec(&mut buf, [1; 32], 5, [2; 32], false, b"abc", true);
        let mut bad = buf.clone();
        bad[96] = 2; // executable
        assert!(parse_append_vec(&mut bad.as_slice(), bad.len() as u64, |_| Ok(())).is_err());
        let mut long = buf.clone();
        long[8..16].copy_from_slice(&(MAX_DATA + 1).to_le_bytes());
        assert!(parse_append_vec(&mut long.as_slice(), long.len() as u64, |_| Ok(())).is_err());
        let mut past = buf.clone();
        past[8..16].copy_from_slice(&1000u64.to_le_bytes());
        assert!(parse_append_vec(&mut past.as_slice(), past.len() as u64, |_| Ok(())).is_err());
        let mut tail = buf.clone();
        tail.extend_from_slice(&[0, 0, 0, 0, 0, 9]);
        assert!(parse_append_vec(&mut tail.as_slice(), tail.len() as u64, |_| Ok(())).is_err(), "bajte jo-zero pas fundit");
        let mut n = 0;
        assert_eq!(parse_append_vec(&mut buf.as_slice(), buf.len() as u64, |_| { n += 1; Ok(()) }).unwrap(), 1);
        assert_eq!(storage_slot("accounts/77.3"), Some(77));
        assert_eq!(storage_slot("snapshots/77/77"), None);
    }
}
