//! Thread-i punëtor: merr ngjarjet nga validatori, përditëson pemën dhe ruan root-et.
//!
//! Validatori nuk pret kurrë për ne: callback-et e plugin-it vetëm i fusin
//! ngjarjet në një kanal, dhe e gjithë puna e rëndë bëhet këtu.

use crate::tracker::{Event, Output, Slot, Tracker};
use crossbeam_channel::{unbounded, Receiver, Sender};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use std::time::Instant;
use ed25519_dalek::{Signer, SigningKey};
use std::collections::BTreeMap;
use zilja_tree::Tree;
use zilja_verifier::attest::attest_message;
use zilja_verifier::Hash;

/// Sa slote të fundit mbahen me firmë (që disa zile të krahasohen në të njëjtin slot).
pub const ATTEST_HISTORY: usize = 1024;

/// Firma e një validatori mbi (slot, root).
#[derive(Clone, Debug)]
pub struct SignedRoot {
    pub root: Hash,
    pub signature: [u8; 64],
}

/// Gjendja që lexon RPC-ja: pema dhe sloti që i përket root-it të saj.
#[derive(Default)]
pub struct State {
    pub tree: Tree,
    pub slot: Slot,
    pub ready: bool,
    /// `true` kur pema mbulon çdo llogari (nisje nga snapshot).
    pub complete: bool,
    /// Çelësi publik i validatorit që firmos (nëse është konfiguruar).
    pub attester: Option<[u8; 32]>,
    /// Root-et e firmosura të sloteve të fundit.
    pub attestations: BTreeMap<Slot, SignedRoot>,
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

pub struct Service {
    pub shared: Shared,
    tx: Option<Sender<Event>>,
    worker: Option<JoinHandle<()>>,
}

impl Service {
    /// Nis punëtorin. `roots_log`: skedari ku shkruhet `slot root_hex` për çdo slot të rrënjosur.
    pub fn start(roots_log: Option<PathBuf>) -> std::io::Result<Self> {
        Self::start_with(roots_log, None)
    }

    /// Si `start`, por me një çelës që firmos çdo root (Faza 4: "pleqtë firmosin ziles").
    pub fn start_with(roots_log: Option<PathBuf>, signer: Option<SigningKey>) -> std::io::Result<Self> {
        let shared = Shared::default();
        shared.state.write().unwrap().attester = signer.as_ref().map(|k| k.verifying_key().to_bytes());
        let (tx, rx) = unbounded();
        let log = match roots_log {
            Some(p) => Some(OpenOptions::new().create(true).append(true).open(p)?),
            None => None,
        };
        let s2 = shared.clone();
        let worker = std::thread::Builder::new()
            .name("zilja-worker".into())
            .spawn(move || worker_loop(rx, s2, log, signer))?;
        Ok(Service { shared, tx: Some(tx), worker: Some(worker) })
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
    let secret: [u8; 32] = bytes.get(..32).and_then(|b| b.try_into().ok()).ok_or_else(|| std::io::Error::other("keypair i pavlefshëm"))?;
    let key = SigningKey::from_bytes(&secret);
    if bytes.len() == 64 && bytes[32..] != key.verifying_key().to_bytes() {
        return Err(std::io::Error::other("çelësi publik nuk përputhet me sekretin"));
    }
    Ok(key)
}

fn worker_loop(rx: Receiver<Event>, shared: Shared, mut log: Option<File>, signer: Option<SigningKey>) {
    let mut tracker = Tracker::new();
    let m = &shared.metrics;
    for ev in rx.iter() {
        m.events_received.fetch_add(1, Ordering::Relaxed);
        for out in tracker.handle(ev) {
            match out {
                Output::Startup { slot, accounts } => {
                    let n = accounts.len();
                    let t0 = Instant::now();
                    let tree = Tree::from_accounts(accounts);
                    let ms = t0.elapsed().as_millis() as u64;
                    let root = tree.root();
                    m.startup_accounts.store(n as u64, Ordering::Relaxed);
                    m.startup_build_ms.store(ms, Ordering::Relaxed);
                    m.complete.store(tracker.complete() as u64, Ordering::Relaxed);
                    m.accounts.store(tree.len() as u64, Ordering::Relaxed);
                    m.last_rooted_slot.store(slot, Ordering::Relaxed);
                    {
                        let mut st = shared.state.write().unwrap();
                        st.tree = tree;
                        st.slot = slot;
                        st.ready = true;
                        st.complete = tracker.complete();
                        sign_root(&mut st, &signer, slot, root);
                    }
                    log::info!("zilja: pema fillestare me {n} llogari u ndërtua në {ms} ms, slot {slot}");
                    write_root(&mut log, slot, &root);
                }
                Output::Rooted { slot, slots_in_batch: _, updates } => {
                    let n = updates.len() as u64;
                    let t0 = Instant::now();
                    let (root, len) = {
                        let mut st = shared.state.write().unwrap();
                        let root = st.tree.apply_batch(updates);
                        st.slot = slot;
                        st.ready = true;
                        sign_root(&mut st, &signer, slot, root);
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
            }
        }
        m.missing_parents.store(tracker.stats.missing_parents, Ordering::Relaxed);
        m.abandoned_banks.store(tracker.stats.abandoned_banks, Ordering::Relaxed);
        m.dead_banks.store(tracker.stats.dead_banks, Ordering::Relaxed);
        m.queue_len.store(rx.len() as u64, Ordering::Relaxed);
    }
}

fn write_root(log: &mut Option<File>, slot: Slot, root: &Hash) {
    if let Some(f) = log {
        let _ = writeln!(f, "{slot} {}", hex::encode(root));
    }
}
