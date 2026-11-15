//! Thread-i punëtor: merr ngjarjet nga validatori, përditëson pemën dhe ruan root-et.
//!
//! Validatori nuk pret kurrë për ne: callback-et e plugin-it vetëm i fusin
//! ngjarjet në një kanal, dhe e gjithë puna e rëndë bëhet këtu.
//!
//! Pema mund të jetë në RAM ([`MemTree`]) ose në disk (`zilja-store`); punëtori
//! flet me të vetëm përmes [`StateTree`].

use crate::tracker::{Event, Output, Slot, Tracker};
use crossbeam_channel::{unbounded, Receiver, Sender};
use ed25519_dalek::{Signer, SigningKey};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use std::time::Instant;
use zilja_tree::{MemTree, StateTree};
use zilja_verifier::attest::attest_message;
use zilja_verifier::Hash;

/// Sa slote të fundit mbahen me firmë (që disa zile të krahasohen në të njëjtin slot).
pub const ATTEST_HISTORY: usize = 1024;
/// Sa root-e slotesh të fundit mbahen në RAM.
pub const ROOT_HISTORY: usize = 1024;
/// Llogaritë e snapshot-it i kalojmë pemës me copa të kësaj madhësie.
const SNAPSHOT_CHUNK: usize = 1 << 16;

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
    /// Çelësi publik i validatorit që firmos (nëse është konfiguruar).
    pub attester: Option<[u8; 32]>,
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
}

impl Metrics {
    pub fn prometheus(&self) -> String {
        let g = |n: &str, h: &str, v: &AtomicU64| {
            format!("# HELP zilja_{n} {h}\n# TYPE zilja_{n} gauge\nzilja_{n} {}\n", v.load(Ordering::Relaxed))
        };
        [
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
            g("ambiguous_banks", "Herët kur një slot kishte disa banka të gjalla (rrugë rezervë)", &self.ambiguous_banks),
            g("resume_verified", "1 kur pas rinisjes u konfirmua që jemi në të njëjtin zinxhir (SlotHashes)", &self.resume_verified),
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
    /// Çelësi që firmos çdo root (Faza 4).
    pub signer: Option<SigningKey>,
    /// Pema: `None` = në RAM.
    pub tree: Option<Box<dyn StateTree>>,
}

pub struct Service {
    pub shared: Shared,
    /// `true` kur pema u gjet në disk dhe zilja vazhdon pa snapshot.
    pub resumed: bool,
    tx: Option<Sender<Event>>,
    worker: Option<JoinHandle<()>>,
}

impl Service {
    pub fn start(roots_log: Option<PathBuf>) -> std::io::Result<Self> {
        Self::start_opts(Options { roots_log, ..Default::default() })
    }

    pub fn start_with(roots_log: Option<PathBuf>, signer: Option<SigningKey>) -> std::io::Result<Self> {
        Self::start_opts(Options { roots_log, signer, ..Default::default() })
    }

    pub fn start_opts(opts: Options) -> std::io::Result<Self> {
        let shared = Shared::default();
        let mut tracker = Tracker::new();
        let tree = opts.tree.unwrap_or_else(|| Box::new(MemTree::new()));
        let resume = tree.resume_point();
        {
            let mut st = shared.state.write().unwrap();
            st.attester = opts.signer.as_ref().map(|k| k.verifying_key().to_bytes());
            st.tree = tree;
            if let Some(r) = &resume {
                tracker.resume(r.slot, r.fingerprint, r.complete);
                st.slot = r.slot;
                st.roots.insert(r.slot, r.root);
                st.ready = true;
                st.complete = r.complete;
                let m = &shared.metrics;
                m.resumed.store(1, Ordering::Relaxed);
                m.accounts.store(r.len as u64, Ordering::Relaxed);
                m.last_rooted_slot.store(r.slot, Ordering::Relaxed);
                m.complete.store(r.complete as u64, Ordering::Relaxed);
                log::info!("zilja: vazhdon nga pema në disk, slot {}, {} llogari", r.slot, r.len);
            }
        }
        let (tx, rx) = unbounded();
        let log = match opts.roots_log {
            Some(p) => Some(OpenOptions::new().create(true).append(true).open(p)?),
            None => None,
        };
        let s2 = shared.clone();
        let signer = opts.signer;
        let worker = std::thread::Builder::new()
            .name("zilja-worker".into())
            .spawn(move || worker_loop(rx, s2, log, signer, tracker))?;
        Ok(Service { shared, resumed: resume.is_some(), tx: Some(tx), worker: Some(worker) })
    }

    pub fn send(&self, ev: Event) {
        if let Some(tx) = &self.tx {
            // Nëse punëtori ka rënë, nuk e rrëzojmë validatorin.
            let _ = tx.send(ev);
            self.shared.metrics.queue_len.store(tx.len() as u64, Ordering::Relaxed);
        }
    }

    pub fn sender(&self) -> Option<Sender<Event>> {
        self.tx.clone()
    }

    /// Mbyll kanalin dhe pret që punëtori të mbarojë gjithçka në radhë.
    pub fn shutdown(&mut self) {
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

fn sign_root(st: &mut State, signer: &Option<SigningKey>, slot: Slot, root: Hash) {
    st.roots.insert(slot, root);
    while st.roots.len() > ROOT_HISTORY {
        st.roots.pop_first();
    }
    if let Some(k) = signer {
        let signature = k.sign(&attest_message(slot, &root)).to_bytes();
        st.attestations.insert(slot, SignedRoot { root, signature });
        while st.attestations.len() > ATTEST_HISTORY {
            st.attestations.pop_first();
        }
    }
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

fn worker_loop(rx: Receiver<Event>, shared: Shared, mut log: Option<File>, signer: Option<SigningKey>, mut tracker: Tracker) {
    let m = &shared.metrics;
    let mut staged_total = 0u64;
    for ev in rx.iter() {
        m.events_received.fetch_add(1, Ordering::Relaxed);
        let outs = tracker.handle(ev);
        // Snapshot-i kalon te pema me copa, që të mos mbahet i gjithi në RAM.
        if let Some(chunk) = tracker.take_startup_chunk(SNAPSHOT_CHUNK) {
            staged_total += chunk.len() as u64;
            stage_by_slot(&mut *shared.state.write().unwrap().tree, chunk);
        }
        for out in outs {
            match out {
                Output::Startup { slot, accounts } => {
                    let n = staged_total + accounts.len() as u64;
                    let t0 = Instant::now();
                    let complete = tracker.complete();
                    let (root, len) = {
                        let mut st = shared.state.write().unwrap();
                        stage_by_slot(&mut *st.tree, accounts);
                        let root = st.tree.finish_snapshot(slot, complete);
                        st.slot = slot;
                        st.ready = true;
                        st.complete = complete;
                        sign_root(&mut st, &signer, slot, root);
                        update_range(&st, m);
                        (root, st.tree.len())
                    };
                    let ms = t0.elapsed().as_millis() as u64;
                    m.startup_accounts.store(n, Ordering::Relaxed);
                    m.startup_build_ms.store(ms, Ordering::Relaxed);
                    m.complete.store(complete as u64, Ordering::Relaxed);
                    m.accounts.store(len as u64, Ordering::Relaxed);
                    m.last_rooted_slot.store(slot, Ordering::Relaxed);
                    log::info!("zilja: pema fillestare me {n} llogari u ndërtua në {ms} ms, slot {slot}");
                    write_root(&mut log, slot, &root);
                }
                Output::Rooted { slot, slots_in_batch: _, updates, fingerprint } => {
                    let n = updates.len() as u64;
                    let t0 = Instant::now();
                    let (root, len) = {
                        let mut st = shared.state.write().unwrap();
                        let root = st.tree.apply_slot(slot, updates, fingerprint);
                        st.slot = slot;
                        st.ready = true;
                        sign_root(&mut st, &signer, slot, root);
                        update_range(&st, m);
                        (root, st.tree.len())
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
                    write_root(&mut log, slot, &root);
                }
                Output::Broken(why) => {
                    log::error!("zilja: NDALOI: {why}. Pema në disk u fshi; rinis validatorin që zilja të rindërtohet nga snapshot-i.");
                    let mut st = shared.state.write().unwrap();
                    st.ready = false;
                    st.broken = Some(why);
                    st.tree.invalidate();
                    m.broken.store(1, Ordering::Relaxed);
                }
            }
        }
        m.missing_parents.store(tracker.stats.missing_parents, Ordering::Relaxed);
        m.abandoned_banks.store(tracker.stats.abandoned_banks, Ordering::Relaxed);
        m.dead_banks.store(tracker.stats.dead_banks, Ordering::Relaxed);
        m.queue_len.store(rx.len() as u64, Ordering::Relaxed);
        m.resume_verified.store(tracker.resume_verified() as u64, Ordering::Relaxed);
        m.ambiguous_banks.store(tracker.stats.ambiguous_banks, Ordering::Relaxed);
    }
}

/// Llogaritë e snapshot-it, të grupuara sipas slotit të versionit, në radhën e mbërritjes.
fn stage_by_slot(tree: &mut dyn StateTree, chunk: Vec<(Slot, zilja_tree::Update)>) {
    let mut by_slot: BTreeMap<Slot, Vec<zilja_tree::Update>> = BTreeMap::new();
    for (s, u) in chunk {
        by_slot.entry(s).or_default().push(u);
    }
    for (s, ups) in by_slot {
        tree.stage_snapshot(s, ups);
    }
}

fn write_root(log: &mut Option<File>, slot: Slot, root: &Hash) {
    if let Some(f) = log {
        let _ = writeln!(f, "{slot} {}", hex::encode(root));
    }
}
