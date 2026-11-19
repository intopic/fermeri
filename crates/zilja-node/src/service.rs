//! Thread-i punëtor: merr ngjarjet nga validatori, përditëson pemën dhe ruan root-et.
//!
//! Validatori nuk pret kurrë për ne: callback-et e plugin-it vetëm i fusin
//! ngjarjet në një kanal, dhe e gjithë puna e rëndë bëhet këtu.
//!
//! Pema mund të jetë në RAM ([`MemTree`]) ose në disk (`zilja-store`); punëtori
//! flet me të vetëm përmes [`StateTree`].

use crate::tracker::{Event, Output, Slot, Tracker};
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use crate::attester::{AttestOptions, Attester, AttesterInfo};
use ed25519_dalek::SigningKey;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use std::time::Instant;
use zilja_tree::{MemTree, StateTree};
use zilja_verifier::{tree_key, Hash};

/// Sa slote të fundit mbahen me firmë (që disa zile të krahasohen në të njëjtin slot).
pub const ATTEST_HISTORY: usize = 1024;
/// Sa root-e slotesh të fundit mbahen në RAM.
pub const ROOT_HISTORY: usize = 1024;
/// Llogaritë e snapshot-it i kalojmë pemës me copa të kësaj madhësie.
const SNAPSHOT_CHUNK: usize = 1 << 16;
/// Sa ngjarje mund të presin në kanal (F-05). Kur mbushet pas nisjes, zilja ndalon
/// në vend që të rritë memorien e validatorit pa kufi.
pub const QUEUE_CAP: usize = 1 << 20;
/// Stack-u i thread-it punëtor (F-09: rrugë deri në 256 nivele rekursioni).
const WORKER_STACK: usize = 64 << 20;

/// Firma e një validatori mbi (slot, root).
#[derive(Clone, Debug)]
pub struct SignedRoot {
    pub root: Hash,
    pub signature: [u8; 64],
}

/// Gjendja që lexon RPC-ja.
pub struct State {
    pub tree: Box<dyn StateTree>,
    pub slot: Slot,
    pub ready: bool,
    /// `true` kur pema mbulon çdo llogari (nisje nga snapshot).
    pub complete: bool,
    /// Arsyeja pse zilja ndaloi (boshllëk, zinxhir tjetër), nëse ndaloi.
    pub broken: Option<String>,
    /// Kush firmos (nëse është konfiguruar).
    pub attester: Option<AttesterInfo>,
    /// Root-et e firmosura të sloteve të fundit.
    pub attestations: BTreeMap<Slot, SignedRoot>,
    /// Root-et e sloteve të fundit të rrënjosur (ROOT_HISTORY), për `getZiljaRoot [slot]` dhe etiketat e dëftesave.
    pub roots: BTreeMap<Slot, Hash>,
}

impl Default for State {
    fn default() -> Self {
        State {
            tree: Box::new(MemTree::new()),
            slot: 0,
            ready: false,
            complete: false,
            broken: None,
            attester: None,
            attestations: BTreeMap::new(),
            roots: BTreeMap::new(),
        }
    }
}

#[derive(Default)]
pub struct Metrics {
    pub last_rooted_slot: AtomicU64,
    pub accounts: AtomicU64,
    pub startup_accounts: AtomicU64,
    pub startup_build_ms: AtomicU64,
    pub batches: AtomicU64,
    pub updates_applied: AtomicU64,
    pub last_batch_updates: AtomicU64,
    pub last_apply_us: AtomicU64,
    pub max_apply_us: AtomicU64,
    pub total_apply_us: AtomicU64,
    pub events_received: AtomicU64,
    pub missing_parents: AtomicU64,
    pub abandoned_banks: AtomicU64,
    pub dead_banks: AtomicU64,
    pub queue_len: AtomicU64,
    pub proofs_served: AtomicU64,
    pub complete: AtomicU64,
    pub resumed: AtomicU64,
    pub broken: AtomicU64,
    pub provable_from_slot: AtomicU64,
    pub resume_verified: AtomicU64,
    pub ambiguous_banks: AtomicU64,
    pub partial_banks_skipped: AtomicU64,
    /// Firmat e refuzuara (do të ishin firmosje e dyfishtë).
    pub attest_refused: AtomicU64,
    /// 1 kur pema është gati dhe zilja nuk ka ndaluar (lexohet nga /health pa kyçur gjendjen).
    pub ready: AtomicU64,
    /// Koha (sekonda Unix) e root-it të fundit.
    pub last_root_unix: AtomicU64,
    /// `true` pas ndalimit: plugin-i ndalon së dërguari ngjarje (F-05).
    pub halted: AtomicBool,
    /// `true` pasi pema fillestare u ndërtua ose u hap nga disku: kanali s'pret më.
    pub startup_done: AtomicBool,
    /// `true` kur kanali u mbush (validatori shkon më shpejt se pema).
    pub queue_overflow: AtomicBool,
    /// `true` kur shënimi "i pavlefshëm" i pemës në disk s'u shkrua (P-03).
    pub invalidate_failed: AtomicBool,
    /// P-08: faza e shërbimit (`PHASE_*`).
    pub phase: AtomicU64,
    /// P-10: pragu i freskisë së root-it në sekonda (0 = pa kontroll). Mbi të, /health = 503.
    pub max_root_age_secs: AtomicU64,
    /// Ndalim i rregullt (`shutdown`): fundi i punëtorit s'është rënie.
    pub stopping: AtomicBool,
    /// P-10: `false` pasi thread-i i punëtorit mbaron (normalisht ose me panic).
    pub worker_alive: AtomicBool,
    /// A01: njoftimet e entry-ve nga Agave (kostoja e `entry_notifications_enabled = true`).
    pub entries_seen: AtomicU64,
    /// A01: njoftimet UpdateParent (Alpenglow) që arritën te plugin-i.
    pub update_parent_seen: AtomicU64,
}

/// P-08: fazat. `starting`: pema po ndërtohet (snapshot/genesis). `recovered`: pema u hap nga disku,
/// por zinxhiri s'e ka konfirmuar ende (s'shërbehet). `serving`: jep dëftesa. `halted`: ndaloi, s'shërben.
pub const PHASE_STARTING: u64 = 0;
pub const PHASE_RECOVERED: u64 = 1;
pub const PHASE_SERVING: u64 = 2;
pub const PHASE_HALTED: u64 = 3;

pub fn phase_name(p: u64) -> &'static str {
    match p {
        PHASE_STARTING => "starting",
        PHASE_RECOVERED => "recovered",
        PHASE_SERVING => "serving",
        _ => "halted",
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Metrics {
    pub fn prometheus(&self) -> String {
        // I-03: numrat që vetëm rriten janë `counter`, të tjerët `gauge`.
        const COUNTERS: &[&str] = &[
            "batches", "updates_applied", "total_apply_us", "events_received", "missing_parents",
            "abandoned_banks", "dead_banks", "proofs_served", "ambiguous_banks", "attest_refused",
            "entries_seen", "update_parent_seen",
        ];
        let g = |n: &str, h: &str, v: &AtomicU64| {
            let t = if COUNTERS.contains(&n) { "counter" } else { "gauge" };
            let n2 = if t == "counter" { format!("{n}_total") } else { n.to_string() };
            format!("# HELP zilja_{n2} {h}\n# TYPE zilja_{n2} {t}\nzilja_{n2} {}\n", v.load(Ordering::Relaxed))
        };
        let last = self.last_root_unix.load(Ordering::Relaxed);
        let age = if last == 0 { 0 } else { now_unix().saturating_sub(last) };
        let age = format!(
            "# HELP zilja_last_root_age_seconds Sa sekonda kanë kaluar nga root-i i fundit\n# TYPE zilja_last_root_age_seconds gauge\nzilja_last_root_age_seconds {age}\n"
        );
        [
            age,
            g("ready", "1 kur zilja jep dëftesa", &self.ready),
            g("attest_refused", "Firmat e refuzuara (do të ishin firmosje e dyfishtë)", &self.attest_refused),
            g("last_rooted_slot", "Sloti i fundit i aplikuar në pemë", &self.last_rooted_slot),
            g("accounts", "Numri i llogarive në pemë", &self.accounts),
            g("startup_accounts", "Llogaritë nga snapshot-i", &self.startup_accounts),
            g("startup_build_ms", "Koha e ndërtimit të pemës fillestare (ms)", &self.startup_build_ms),
            g("batches", "Grupet e aplikuara (një për çdo Rooted)", &self.batches),
            g("updates_applied", "Ndryshimet e llogarive të aplikuara gjithsej", &self.updates_applied),
            g("last_batch_updates", "Ndryshimet në grupin e fundit", &self.last_batch_updates),
            g("last_apply_us", "Koha e aplikimit të grupit të fundit (µs)", &self.last_apply_us),
            g("max_apply_us", "Koha maksimale e aplikimit të një grupi (µs)", &self.max_apply_us),
            g("total_apply_us", "Koha totale e aplikimit (µs)", &self.total_apply_us),
            g("events_received", "Ngjarjet e marra nga validatori", &self.events_received),
            g("missing_parents", "Herët kur prindi i një sloti mungonte", &self.missing_parents),
            g("abandoned_banks", "Bankat e hedhura si fork-e të braktisura", &self.abandoned_banks),
            g("dead_banks", "Bankat e shënuara Dead", &self.dead_banks),
            g("queue_len", "Ngjarjet në pritje në kanal", &self.queue_len),
            g("proofs_served", "Dëftesat e shërbyera", &self.proofs_served),
            g("complete", "1 kur pema mbulon çdo llogari (nisje nga snapshot)", &self.complete),
            g("resumed", "1 kur zilja vazhdoi nga pema në disk pa snapshot", &self.resumed),
            g("broken", "1 kur zilja ndaloi (boshllëk ose zinxhir tjetër)", &self.broken),
            g("provable_from_slot", "Sloti më i vjetër për të cilin jepen dëftesa", &self.provable_from_slot),
            g("partial_banks_skipped", "Banka të krijuara para ngarkimit të plugin-it (pa SlotHashes) që s'u aplikuan (nisje pa bazë)", &self.partial_banks_skipped),
            g("ambiguous_banks", "Herët kur një slot kishte disa banka të gjalla që SlotHashes s'i dallonte (ndalim, P-07)", &self.ambiguous_banks),
            g("resume_verified", "1 kur pas rinisjes u konfirmua që jemi në të njëjtin zinxhir (SlotHashes)", &self.resume_verified),
            g("phase", "Faza: 0 starting, 1 recovered (nga disku, e paverifikuar), 2 serving, 3 halted", &self.phase),
            g("entries_seen", "Njoftimet e entry-ve nga validatori (A01: kostoja e marrjes së UpdateParent)", &self.entries_seen),
            g("update_parent_seen", "Njoftimet UpdateParent (Alpenglow) të marra", &self.update_parent_seen),
            g("max_root_age_secs", "Pragu i freskisë së root-it për /health (0 = pa kontroll)", &self.max_root_age_secs),
            format!(
                "# HELP zilja_invalidate_failed 1 kur shënimi 'i pavlefshëm' i pemës në disk dështoi (rindërtoje me dorë)\n# TYPE zilja_invalidate_failed gauge\nzilja_invalidate_failed {}\n",
                self.invalidate_failed.load(Ordering::Relaxed) as u8
            ),
        ]
        .concat()
    }
}

/// Gjendja e përbashkët mes punëtorit dhe RPC-së.
#[derive(Clone, Default)]
pub struct Shared {
    pub state: Arc<RwLock<State>>,
    pub metrics: Arc<Metrics>,
}

/// Si niset shërbimi.
#[derive(Default)]
pub struct Options {
    /// Skedari ku shkruhet `slot root_hex` për çdo slot të rrënjosur.
    pub roots_log: Option<PathBuf>,
    /// Firmosja e root-eve (F-10).
    pub attest: Option<AttestOptions>,
    /// Pema: `None` = në RAM.
    pub tree: Option<Box<dyn StateTree>>,
    /// P-10: /health = 503 kur root-i i fundit është më i vjetër se kaq sekonda.
    pub max_root_age_secs: Option<u64>,
    /// Plugin-i u ngarkua ndërsa validatori punonte (`reloadPlugin`): bankat që po përpunoheshin
    /// u krijuan para ngarkimit dhe vëzhgohen vetëm pjesërisht.
    pub mid_run_load: bool,
}

pub struct Service {
    pub shared: Shared,
    /// `true` kur pema u gjet në disk dhe zilja vazhdon pa snapshot.
    pub resumed: bool,
    tx: Option<Sender<Event>>,
    worker: Option<JoinHandle<()>>,
}

impl Service {
    pub fn start_opts(opts: Options) -> std::io::Result<Self> {
        let shared = Shared::default();
        let mut tracker = Tracker::new();
        tracker.set_mid_run_load(opts.mid_run_load);
        let attester = match opts.attest {
            Some(a) => Some(Attester::open(a).map_err(std::io::Error::other)?),
            None => None,
        };
        let tree = opts.tree.unwrap_or_else(|| Box::new(MemTree::new()));
        let resume = tree.resume_point();
        {
            let mut st = shared.state.write().unwrap();
            if let Some(a) = &attester {
                st.attester = Some(a.info());
                // I-02: firmat e fundit nga skedari, gati menjëherë pas rinisjes.
                for (slot, root, signature) in a.recent(ATTEST_HISTORY) {
                    st.attestations.insert(slot, SignedRoot { root, signature });
                }
            }
            st.tree = tree;
            if let Some(r) = &resume {
                tracker.resume(r.slot, r.fingerprint, r.complete);
                st.slot = r.slot;
                st.roots.insert(r.slot, r.root);
                // P-08: e hapur nga disku ≠ e verifikuar. Shërbehet vetëm pasi SlotHashes ta konfirmojë.
                st.complete = r.complete;
                let m = &shared.metrics;
                m.resumed.store(1, Ordering::Relaxed);
                m.accounts.store(r.len as u64, Ordering::Relaxed);
                m.last_rooted_slot.store(r.slot, Ordering::Relaxed);
                m.complete.store(r.complete as u64, Ordering::Relaxed);
                m.phase.store(PHASE_RECOVERED, Ordering::Relaxed);
                m.startup_done.store(true, Ordering::Relaxed);
                log::info!("zilja: vazhdon nga pema në disk, slot {}, {} llogari", r.slot, r.len);
            }
        }
        let (tx, rx) = bounded(QUEUE_CAP);
        let log = match opts.roots_log {
            Some(p) => Some(RootsLog::open(p)?),
            None => None,
        };
        shared.metrics.max_root_age_secs.store(opts.max_root_age_secs.unwrap_or(0), Ordering::Relaxed);
        shared.metrics.worker_alive.store(true, Ordering::Relaxed);
        let s2 = shared.clone();
        let s3 = shared.clone();
        let signer = attester;
        let worker = std::thread::Builder::new().name("zilja-worker".into()).stack_size(WORKER_STACK).spawn(move || {
            // P-10: një rënie e punëtorit (panic) ose një fund i papritur ndalon shërbimin.
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| worker_loop(rx, s2, log, signer, tracker)));
            let m = &s3.metrics;
            let why = match r {
                Err(e) => {
                    let msg = e.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| e.downcast_ref::<String>().cloned()).unwrap_or_default();
                    Some(format!("punëtori i Ziles ra (panic: {msg})"))
                }
                Ok(()) if !m.stopping.load(Ordering::Relaxed) => Some("punëtori i Ziles mbaroi papritur".to_string()),
                Ok(()) => None,
            };
            if let Some(why) = why {
                stop_serving(&s3, why);
            }
            m.ready.store(0, Ordering::Relaxed);
            m.worker_alive.store(false, Ordering::Relaxed);
        })?;
        Ok(Service { shared, resumed: resume.is_some(), tx: Some(tx), worker: Some(worker) })
    }

    /// Thirret nga callback-et e validatorit. Pas ndalimit nuk bën asgjë (F-05). Gjatë nisjes
    /// pret kur kanali është plot; pas saj nuk pret kurrë: një kanal plot e ndalon ziles.
    pub fn send(&self, ev: Event) {
        let m = &self.shared.metrics;
        if m.halted.load(Ordering::Relaxed) {
            return;
        }
        let Some(tx) = &self.tx else { return };
        let disconnected = if !m.startup_done.load(Ordering::Relaxed) {
            tx.send(ev).is_err()
        } else {
            match tx.try_send(ev) {
                Err(TrySendError::Full(_)) => {
                    // Punëtori e kryen ndalimin; këtu vetëm ndalojmë së dërguari, pa kyçur asgjë.
                    m.ready.store(0, Ordering::Relaxed);
                    m.queue_overflow.store(true, Ordering::Relaxed);
                    m.halted.store(true, Ordering::Relaxed);
                    m.phase.store(PHASE_HALTED, Ordering::Relaxed);
                    false
                }
                Err(TrySendError::Disconnected(_)) => true,
                Ok(()) => false,
            }
        };
        if disconnected {
            // P-10: s'ka më punëtor. Vetëm atomike: validatori s'pret kurrë këtu.
            m.ready.store(0, Ordering::Relaxed);
            m.halted.store(true, Ordering::Relaxed);
            m.broken.store(1, Ordering::Relaxed);
            m.phase.store(PHASE_HALTED, Ordering::Relaxed);
        }
        m.queue_len.store(tx.len() as u64, Ordering::Relaxed);
    }

    /// Metrikat (vetëm lexim/numërim me atomike, pa kyçur gjë).
    pub fn metrics(&self) -> &Metrics {
        &self.shared.metrics
    }

    pub fn sender(&self) -> Option<Sender<Event>> {
        self.tx.clone()
    }

    /// Mbyll kanalin dhe pret që punëtori të mbarojë gjithçka në radhë.
    pub fn shutdown(&mut self) {
        self.shared.metrics.stopping.store(true, Ordering::Relaxed);
        self.tx.take();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }

    pub fn current(&self) -> (Slot, Hash) {
        let st = self.shared.state.read().unwrap();
        (st.slot, st.tree.root())
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Ruan root-in e slotit dhe e firmos kur lejohet: vetëm një pemë e plotë firmoset (F-10).
/// Pas rinisjes, tracker-i nuk lëshon asnjë Rooted pa e konfirmuar zinxhirin.
fn sign_root(st: &mut State, signer: &mut Option<Attester>, m: &Metrics, slot: Slot, root: Hash) {
    st.roots.insert(slot, root);
    while st.roots.len() > ROOT_HISTORY {
        st.roots.pop_first();
    }
    let Some(a) = signer else { return };
    if !st.complete {
        return;
    }
    match a.sign(slot, &root) {
        Ok(signature) => {
            st.attestations.insert(slot, SignedRoot { root, signature });
            while st.attestations.len() > ATTEST_HISTORY {
                st.attestations.pop_first();
            }
        }
        Err(e) => log::error!("zilja: firma u refuzua: {e}"),
    }
    m.attest_refused.store(a.refused, Ordering::Relaxed);
}

/// Lexon një keypair Solana (JSON me 64 numra, si `validator-keypair.json`).
pub fn read_solana_keypair(path: &std::path::Path) -> std::io::Result<SigningKey> {
    let text = std::fs::read_to_string(path)?;
    let bytes: Vec<u8> = serde_json::from_str(&text).map_err(std::io::Error::other)?;
    let secret: [u8; 32] =
        bytes.get(..32).and_then(|b| b.try_into().ok()).ok_or_else(|| std::io::Error::other("keypair i pavlefshëm"))?;
    let key = SigningKey::from_bytes(&secret);
    if bytes.len() == 64 && bytes[32..] != key.verifying_key().to_bytes() {
        return Err(std::io::Error::other("çelësi publik nuk përputhet me sekretin"));
    }
    Ok(key)
}

fn update_range(st: &State, m: &Metrics) {
    if let Some((lo, _)) = st.tree.provable_range() {
        m.provable_from_slot.store(lo, Ordering::Relaxed);
    }
}

/// Safer-1: ndalimi nga punëtori (tracker-i nuk pranon më asgjë), përmes rrugës qendrore.
fn halt(shared: &Shared, tracker: &mut Tracker, why: String) {
    tracker.halt();
    stop_serving(shared, why);
}

/// P-10: rruga qendrore e ndalimit kur korrektësia s'garantohet më: punëtori (zinxhir/boshllëk),
/// RPC-ja (gabim ruajtjeje), rënia e punëtorit. E dukshme te /health, te metrikat dhe te logu.
/// Pema në disk shënohet e pavlefshme (rindërtohet në nisjen e ardhshme), por nuk fshihet (F-13).
pub fn stop_serving(shared: &Shared, why: String) {
    let m = &shared.metrics;
    m.halted.store(true, Ordering::Relaxed);
    m.ready.store(0, Ordering::Relaxed);
    m.broken.store(1, Ordering::Relaxed);
    m.phase.store(PHASE_HALTED, Ordering::Relaxed);
    let mut st = shared.state.write().unwrap_or_else(|e| e.into_inner());
    if st.broken.is_none() {
        log::error!("zilja: NDALOI: {why}. Pema u shënua e pavlefshme; rinis validatorin që zilja të rindërtohet nga snapshot-i.");
        st.ready = false;
        // P-03: nëse shënimi s'shkruhet, nisja e ardhshme s'e di që duhet rindërtim. Fail-closed:
        // metrika e tregon, dhe fingerprint-i i zinxhirit e ndalon përsëri pas rinisjes.
        if let Err(e) = st.tree.invalidate(&why) {
            m.invalidate_failed.store(true, Ordering::Relaxed);
            log::error!("zilja: shënimi 'i pavlefshëm' i pemës dështoi: {e}. Rindërtoje me dorë para rinisjes.");
        }
        st.broken = Some(why);
    }
}

fn worker_loop(rx: Receiver<Event>, shared: Shared, mut log: Option<RootsLog>, mut signer: Option<Attester>, mut tracker: Tracker) {
    let m = &shared.metrics;
    let mut staged_total = 0u64;
    for ev in rx.iter() {
        m.events_received.fetch_add(1, Ordering::Relaxed);
        if tracker.is_broken() {
            continue; // F-05/Faster-4: asnjë punë pas ndalimit
        }
        if m.queue_overflow.load(Ordering::Relaxed) {
            halt(&shared, &mut tracker, "kanali i ngjarjeve u mbush: pema mbeti pas validatorit".into());
            continue;
        }
        let outs = tracker.handle(ev);
        // Snapshot-i kalon te pema me copa, që të mos mbahet i gjithi në RAM.
        if let Some(chunk) = tracker.take_startup_chunk(SNAPSHOT_CHUNK) {
            staged_total += chunk.len() as u64;
            let chunk: Vec<(Slot, zilja_tree::Update)> = chunk.into_iter().map(|(s, (k, m))| (s, (tree_key(&k), m))).collect();
            let r = stage_by_slot(&mut *shared.state.write().unwrap().tree, chunk);
            if let Err(e) = r {
                halt(&shared, &mut tracker, e);
                continue;
            }
        }
        for out in outs {
            match out {
                Output::Startup { slot, accounts } => {
                    let accounts: Vec<(Slot, zilja_tree::Update)> = accounts.into_iter().map(|(s, (k, m))| (s, (tree_key(&k), m))).collect();
                    let n = staged_total + accounts.len() as u64;
                    let t0 = Instant::now();
                    let complete = tracker.complete();
                    let res = {
                        let mut st = shared.state.write().unwrap();
                        let r = stage_by_slot(&mut *st.tree, accounts).and_then(|_| st.tree.finish_snapshot(slot, complete));
                        r.map(|root| {
                            st.slot = slot;
                            st.ready = true;
                            st.complete = complete;
                            sign_root(&mut st, &mut signer, m, slot, root);
                            update_range(&st, m);
                            (root, st.tree.len())
                        })
                    };
                    let (root, len) = match res {
                        Ok(x) => x,
                        Err(e) => {
                            halt(&shared, &mut tracker, format!("ndërtimi nga snapshot-i dështoi: {e}"));
                            break;
                        }
                    };
                    let ms = t0.elapsed().as_millis() as u64;
                    m.startup_accounts.store(n, Ordering::Relaxed);
                    m.startup_build_ms.store(ms, Ordering::Relaxed);
                    m.complete.store(complete as u64, Ordering::Relaxed);
                    m.accounts.store(len as u64, Ordering::Relaxed);
                    m.last_rooted_slot.store(slot, Ordering::Relaxed);
                    m.last_root_unix.store(now_unix(), Ordering::Relaxed);
                    m.ready.store(1, Ordering::Relaxed);
                    m.phase.store(PHASE_SERVING, Ordering::Relaxed);
                    m.startup_done.store(true, Ordering::Relaxed);
                    log::info!("zilja: pema fillestare me {n} llogari u ndërtua në {ms} ms, slot {slot}");
                    write_root(&mut log, slot, &root);
                }
                Output::Rooted { slot, slots_in_batch: _, updates, fingerprint } => {
                    // F-09: pema renditet sipas tree_key, jo sipas pubkey-it.
                    let updates: Vec<zilja_tree::Update> = updates.into_iter().map(|(k, m)| (tree_key(&k), m)).collect();
                    let n = updates.len() as u64;
                    let t0 = Instant::now();
                    // Faster-1: leximet dhe llogaritja nën kyçin e leximit (dëftesat vazhdojnë);
                    // vetëm shkrimi i shkurtër nën kyçin e shkrimit. Vetëm ky thread e ndryshon pemën.
                    let prepared = shared.state.read().unwrap().tree.prepare_slot(slot, updates, fingerprint);
                    let res = {
                        let mut st = shared.state.write().unwrap();
                        prepared.and_then(|p| st.tree.commit_slot(slot, p)).map(|root| {
                            st.slot = slot;
                            st.ready = true;
                            sign_root(&mut st, &mut signer, m, slot, root);
                            update_range(&st, m);
                            (root, st.tree.len())
                        })
                    };
                    let (root, len) = match res {
                        Ok(x) => x,
                        Err(e) => {
                            halt(&shared, &mut tracker, e);
                            break;
                        }
                    };
                    let us = t0.elapsed().as_micros() as u64;
                    m.batches.fetch_add(1, Ordering::Relaxed);
                    m.updates_applied.fetch_add(n, Ordering::Relaxed);
                    m.last_batch_updates.store(n, Ordering::Relaxed);
                    m.last_apply_us.store(us, Ordering::Relaxed);
                    m.max_apply_us.fetch_max(us, Ordering::Relaxed);
                    m.total_apply_us.fetch_add(us, Ordering::Relaxed);
                    m.accounts.store(len as u64, Ordering::Relaxed);
                    m.last_rooted_slot.store(slot, Ordering::Relaxed);
                    m.last_root_unix.store(now_unix(), Ordering::Relaxed);
                    m.ready.store(1, Ordering::Relaxed);
                    m.phase.store(PHASE_SERVING, Ordering::Relaxed);
                    write_root(&mut log, slot, &root);
                }
                Output::Broken(why) => {
                    halt(&shared, &mut tracker, why);
                    break;
                }
            }
        }
        m.missing_parents.store(tracker.stats.missing_parents, Ordering::Relaxed);
        m.abandoned_banks.store(tracker.stats.abandoned_banks, Ordering::Relaxed);
        m.dead_banks.store(tracker.stats.dead_banks, Ordering::Relaxed);
        m.queue_len.store(rx.len() as u64, Ordering::Relaxed);
        m.resume_verified.store(tracker.resume_verified() as u64, Ordering::Relaxed);
        // P-08: gjendja nga disku shërbehet vetëm pasi zinxhiri ta konfirmojë.
        if tracker.resume_verified() && m.phase.load(Ordering::Relaxed) == PHASE_RECOVERED {
            shared.state.write().unwrap().ready = true;
            m.last_root_unix.store(now_unix(), Ordering::Relaxed);
            m.ready.store(1, Ordering::Relaxed);
            m.phase.store(PHASE_SERVING, Ordering::Relaxed);
            log::info!("zilja: gjendja nga disku u konfirmua nga zinxhiri; po shërben");
        }
        m.ambiguous_banks.store(tracker.stats.ambiguous_banks, Ordering::Relaxed);
        m.partial_banks_skipped.store(tracker.stats.partial_banks_skipped, Ordering::Relaxed);
    }
}

/// Llogaritë e snapshot-it, të grupuara sipas slotit të versionit, në radhën e mbërritjes.
fn stage_by_slot(tree: &mut dyn StateTree, chunk: Vec<(Slot, zilja_tree::Update)>) -> Result<(), String> {
    let mut by_slot: BTreeMap<Slot, Vec<zilja_tree::Update>> = BTreeMap::new();
    for (s, u) in chunk {
        by_slot.entry(s).or_default().push(u);
    }
    for (s, ups) in by_slot {
        tree.stage_snapshot(s, ups)?;
    }
    Ok(())
}

/// `roots.log` me rrotullim (I-02): mbi 64 MB bëhet `roots.log.1` dhe nis një i ri.
pub struct RootsLog {
    path: PathBuf,
    file: File,
    bytes: u64,
}

const ROOTS_LOG_MAX: u64 = 64 << 20;

impl RootsLog {
    fn open(path: PathBuf) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let bytes = file.metadata()?.len();
        Ok(RootsLog { path, file, bytes })
    }
}

fn write_root(log: &mut Option<RootsLog>, slot: Slot, root: &Hash) {
    let Some(l) = log else { return };
    let line = format!("{slot} {}\n", hex::encode(root));
    if l.bytes + line.len() as u64 > ROOTS_LOG_MAX {
        let old = l.path.with_extension("log.1");
        if std::fs::rename(&l.path, &old).is_ok() {
            if let Ok(f) = OpenOptions::new().create(true).append(true).open(&l.path) {
                l.file = f;
                l.bytes = 0;
            }
        }
    }
    if l.file.write_all(line.as_bytes()).is_ok() {
        l.bytes += line.len() as u64;
    }
}
