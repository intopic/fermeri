//! Testi model i tracker-it (Faza 0, Safer-2 i auditimit).
//!
//! Gjeneron histori të rastësishme blloqesh siç i dërgon Agave 4.3.2 (fork-e,
//! banka Dead të zëvendësuara, njoftime Rooted që humbasin, ripërsëritje e ledger-it
//! gjatë nisjes pa statuse, versione të dyfishta në snapshot), ia jep ngjarjet
//! tracker-it, zbaton daljet në një pemë në RAM, dhe e krahason gjendjen
//! përfundimtare me një llogaritje të drejtpërdrejtë të zinxhirit kanonik.
//!
//! Rregulli: tracker-i ose jep gjendjen e saktë, ose ndalon (`Broken`). Ndalimi
//! pranohet vetëm kur historia ka vërtet një boshllëk; në këto skenarë s'ka.

use rand::{rngs::StdRng, seq::SliceRandom, Rng, SeedableRng};
use std::collections::{BTreeMap, HashMap};
use zilja_node::tracker::{Output, Tracker};
use zilja_node::{Event, Status};
use zilja_tree::{reference_root, MemTree, StateTree};
use zilja_verifier::{AccountMeta, Key};

#[derive(Clone, Copy)]
struct Cfg {
    /// Blloqet e para pas snapshot-it ripërsëriten nga ledger-i gjatë nisjes: vetëm
    /// shkrime llogarish dhe SlotHashes, pa CreatedBank/Rooted (sjellja e Agave 4.3.2).
    replay: usize,
    /// Snapshot-i dërgon edhe versione më të vjetra të llogarive, në radhë të rastësishme.
    snapshot_dups: bool,
    /// Probabiliteti që një slot të jetë fork që braktiset.
    forks: f64,
    /// Probabiliteti që një bllok fork të vdesë, ose që një slot kanonik të ketë fillimisht një bank Dead.
    dead: f64,
    /// Probabiliteti që një njoftim Rooted të humbasë.
    drop_root: f64,
}

struct Block {
    slot: u64,
    parent: u64,
    bank_id: u64,
    canonical: bool,
    writes: Vec<(Key, Option<AccountMeta>, u64)>,
}

const SNAP: u64 = 100;
const LAG: usize = 4;

fn key(i: u8) -> Key {
    let mut k = [0u8; 32];
    k[0] = i.wrapping_mul(37);
    k[1] = i;
    k
}

fn meta(rng: &mut StdRng) -> AccountMeta {
    AccountMeta { lamports: rng.gen_range(1..1_000_000), data_len: rng.gen_range(0..64), data_hash: rng.gen(), ..Default::default() }
}

fn bank_hash(bank_id: u64) -> [u8; 32] {
    let mut h = [0xB0u8; 32];
    h[..8].copy_from_slice(&bank_id.to_le_bytes());
    h
}

/// Kthen (ngjarjet, gjendjen e pritur, slotin e fundit të rrënjosur).
fn generate(seed: u64, cfg: Cfg) -> (Vec<Event>, BTreeMap<Key, AccountMeta>, u64) {
    let mut rng = StdRng::seed_from_u64(seed);
    let nkeys: u8 = 30;
    let mut wv: u64 = 1;
    let mut next_bank: u64 = 1;
    let mut events = Vec::new();

    // ---- snapshot-i në slotin SNAP
    let mut state: BTreeMap<Key, AccountMeta> = BTreeMap::new();
    let mut snap_events = Vec::new();
    for i in 0..nkeys {
        // Llogaria 0 luan rolin e sysvar-it (Clock), që shkruhet në çdo slot: gjithmonë në SNAP.
        if i == 0 || rng.gen_bool(0.7) {
            let m = meta(&mut rng);
            state.insert(key(i), m.clone());
            // Llogaria e fundit e ndryshuar në një slot ≤ SNAP; një llogari gjithmonë në SNAP.
            let s = if i == 0 { SNAP } else { rng.gen_range(SNAP - 40..=SNAP) };
            snap_events.push(Event::Snapshot { slot: s, pubkey: key(i), meta: Some(m) });
            if cfg.snapshot_dups {
                for _ in 0..rng.gen_range(0..3) {
                    let old = rng.gen_range(SNAP - 90..s.max(SNAP - 89));
                    snap_events.push(Event::Snapshot { slot: old, pubkey: key(i), meta: Some(meta(&mut rng)) });
                }
            }
        } else if cfg.snapshot_dups && rng.gen_bool(0.3) {
            // Llogari e mbyllur: varri në një slot të ri, një kopje e vjetër në një slot më të vjetër.
            let s = rng.gen_range(SNAP - 40..=SNAP);
            snap_events.push(Event::Snapshot { slot: s, pubkey: key(i), meta: None });
            snap_events.push(Event::Snapshot { slot: s - 5, pubkey: key(i), meta: Some(meta(&mut rng)) });
        }
    }
    snap_events.shuffle(&mut rng);
    events.extend(snap_events);
    events.push(Event::EndOfStartup);

    // ---- blloqet
    let mut blocks: Vec<Block> = Vec::new();
    let mut bank_of_slot: HashMap<u64, u64> = HashMap::new();
    let mut canon_tip = SNAP;
    let mut fork_tips: Vec<u64> = Vec::new();
    let mut slot = SNAP;
    let n_blocks = 50 + cfg.replay;
    let mut canon_count = 0usize;
    while canon_count < n_blocks {
        slot += 1;
        if rng.gen_bool(0.08) {
            continue; // slot i kapërcyer
        }
        let replaying = canon_count < cfg.replay;
        let is_fork = !replaying && rng.gen_bool(cfg.forks);
        let parent = if is_fork {
            if !fork_tips.is_empty() && rng.gen_bool(0.5) {
                *fork_tips.choose(&mut rng).unwrap()
            } else {
                canon_tip
            }
        } else {
            canon_tip
        };
        // Çdo bllok shkruan sysvar-ët (këtu: llogaria 0, si Clock).
        let mut writes = vec![(key(0), Some(meta(&mut rng)), wv)];
        wv += 1;
        for _ in 0..rng.gen_range(0..7) {
            let k = key(rng.gen_range(0..nkeys));
            let m = if rng.gen_bool(0.12) { None } else { Some(meta(&mut rng)) };
            writes.push((k, m, wv));
            wv += 1;
        }
        let bank_id = next_bank;
        next_bank += 1;
        bank_of_slot.insert(slot, bank_id);
        blocks.push(Block { slot, parent, bank_id, canonical: !is_fork, writes });
        if is_fork {
            fork_tips.push(slot);
        } else {
            canon_tip = slot;
            canon_count += 1;
        }
    }

    // SlotHashes i bankës: paraardhësit nga prindi poshtë.
    let parent_of: HashMap<u64, u64> = blocks.iter().map(|b| (b.slot, b.parent)).collect();
    let slot_hashes = |b: &Block| -> Event {
        let mut entries = Vec::new();
        let mut s = b.parent;
        while entries.len() < 16 {
            let bid = bank_of_slot.get(&s).copied().unwrap_or(s * 1_000_003); // para snapshot-it: hash i qëndrueshëm
            entries.push((s, bank_hash(bid)));
            match parent_of.get(&s) {
                Some(p) => s = *p,
                None => break,
            }
        }
        Event::SlotHashes { slot: b.slot, bank_id: b.bank_id, entries }
    };
    let writes_of = |b: &Block, bank_id: u64| -> Vec<Event> {
        b.writes
            .iter()
            .map(|(k, m, w)| Event::Account { slot: b.slot, bank_id, pubkey: *k, write_version: *w, meta: m.clone() })
            .collect()
    };

    let canon: Vec<&Block> = blocks.iter().filter(|b| b.canonical).collect();
    let mut rooted_upto = 0usize; // sa blloqe kanonike janë rrënjosur
    let mut canon_seen = 0usize;
    let mut final_root = SNAP;
    for b in &blocks {
        let replaying = b.canonical && canon_seen < cfg.replay;
        if replaying {
            // Ripërsëritje gjatë nisjes: pa statuse.
            events.push(slot_hashes(b));
            events.extend(writes_of(b, b.bank_id));
            canon_seen += 1;
            if canon_seen == cfg.replay {
                // Rrënja e nisjes: i pari Rooted pas nisjes, për bllokun e fundit të ripërsëritur.
                events.push(Event::Status { slot: b.slot, parent: Some(b.parent), bank_id: Some(b.bank_id), status: Status::Rooted });
                rooted_upto = canon_seen;
                final_root = b.slot;
            }
            continue;
        }
        // Ndonjëherë sloti kanonik ka fillimisht një bank që vdes dhe zëvendësohet.
        if b.canonical && rng.gen_bool(cfg.dead) {
            let dead_id = next_bank;
            next_bank += 1;
            events.push(Event::Status { slot: b.slot, parent: Some(b.parent), bank_id: Some(dead_id), status: Status::CreatedBank });
            for _ in 0..rng.gen_range(1..4) {
                events.push(Event::Account { slot: b.slot, bank_id: dead_id, pubkey: key(rng.gen_range(0..nkeys)), write_version: wv, meta: Some(meta(&mut rng)) });
                wv += 1;
            }
            events.push(Event::Status { slot: b.slot, parent: None, bank_id: None, status: Status::Dead });
        }
        events.push(Event::Status { slot: b.slot, parent: Some(b.parent), bank_id: Some(b.bank_id), status: Status::CreatedBank });
        events.push(slot_hashes(b));
        events.extend(writes_of(b, b.bank_id));
        if !b.canonical && rng.gen_bool(cfg.dead) {
            events.push(Event::Status { slot: b.slot, parent: None, bank_id: None, status: Status::Dead });
        }
        if b.canonical {
            canon_seen += 1;
            // Rrënjos zinxhirin deri në canon_seen - LAG, nga më i vjetri, me humbje të rastësishme.
            while rooted_upto + LAG < canon_seen {
                let r = canon[rooted_upto];
                rooted_upto += 1;
                if !rng.gen_bool(cfg.drop_root) {
                    events.push(Event::Status { slot: r.slot, parent: Some(r.parent), bank_id: Some(r.bank_id), status: Status::Rooted });
                    final_root = r.slot;
                }
            }
        }
    }
    // Njoftimi i fundit arrin gjithmonë.
    let last = canon[rooted_upto - 1];
    if final_root != last.slot {
        events.push(Event::Status { slot: last.slot, parent: Some(last.parent), bank_id: Some(last.bank_id), status: Status::Rooted });
        final_root = last.slot;
    }

    // ---- gjendja e pritur: snapshot-i + blloqet kanonike deri te final_root
    for b in canon.iter().take_while(|b| b.slot <= final_root) {
        let mut ws = b.writes.clone();
        ws.sort_by_key(|w| w.2);
        for (k, m, _) in ws {
            match m {
                Some(m) => {
                    state.insert(k, m);
                }
                None => {
                    state.remove(&k);
                }
            }
        }
    }
    (events, state, final_root)
}

fn run(seed: u64, cfg: Cfg) -> Result<(), String> {
    let (events, expect, final_root) = generate(seed, cfg);
    let mut t = Tracker::new();
    let mut mt = MemTree::new();
    for ev in events {
        for out in t.handle(ev) {
            match out {
                Output::Startup { slot, accounts } => {
                    for (s, u) in accounts {
                        mt.stage_snapshot(s, vec![u]).unwrap();
                    }
                    mt.finish_snapshot(slot, true).unwrap();
                }
                Output::Rooted { slot, updates, fingerprint, .. } => {
                    mt.apply_slot(slot, updates, fingerprint).unwrap();
                }
                Output::Broken(why) => return Err(format!("seed {seed}: ndaloi pa boshllëk: {why}")),
            }
        }
    }
    if t.last_rooted() != Some(final_root) {
        return Err(format!("seed {seed}: sloti i fundit {:?}, pritej {final_root}", t.last_rooted()));
    }
    let tree = &mt.tree;
    let accs: Vec<(Key, AccountMeta)> = expect.iter().map(|(k, m)| (*k, m.clone())).collect();
    if tree.root() != reference_root(&accs) {
        let got: BTreeMap<Key, u64> = accs.iter().filter_map(|(k, _)| tree.get(k).map(|m| (*k, m.lamports))).collect();
        let want: BTreeMap<Key, u64> = accs.iter().map(|(k, m)| (*k, m.lamports)).collect();
        return Err(format!(
            "seed {seed}: gjendja ndryshon (llogari: {} kundrejt {} të pritura; të ndryshme: {})",
            tree.len(),
            expect.len(),
            want.iter().filter(|(k, v)| got.get(*k) != Some(v)).count()
        ));
    }
    Ok(())
}

fn many(cfg: Cfg, seeds: u64) {
    // ZILJA_MODEL_SEEDS=10000 për një kontroll më të gjerë.
    let seeds = std::env::var("ZILJA_MODEL_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(seeds);
    let errs: Vec<String> = (0..seeds).filter_map(|s| run(s, cfg).err()).collect();
    assert!(errs.is_empty(), "{} nga {seeds} histori dështuan, p.sh.:\n{}", errs.len(), errs.iter().take(5).cloned().collect::<Vec<_>>().join("\n"));
}

/// Punë normale pas nisjes: fork-e, banka Dead, njoftime Rooted që humbasin. Kalon sot.
#[test]
fn model_live_forks_dead_banks_dropped_roots() {
    many(Cfg { replay: 0, snapshot_dups: false, forks: 0.25, dead: 0.15, drop_root: 0.3 }, 1000);
}

/// F-01: ripërsëritja e ledger-it gjatë nisjes, pa statuse.
#[test]
fn model_startup_ledger_replay() {
    many(Cfg { replay: 6, snapshot_dups: false, forks: 0.2, dead: 0.1, drop_root: 0.2 }, 1000);
}

/// F-02: snapshot-i dërgon versione të vjetra në radhë të rastësishme.
#[test]
fn model_snapshot_duplicate_versions() {
    many(Cfg { replay: 0, snapshot_dups: true, forks: 0.2, dead: 0.1, drop_root: 0.2 }, 1000);
}

/// Gjithçka bashkë, siç do të ndodhë në një rinisje të vërtetë.
#[test]
fn model_realistic_restart() {
    many(Cfg { replay: 8, snapshot_dups: true, forks: 0.25, dead: 0.15, drop_root: 0.3 }, 2000);
}
