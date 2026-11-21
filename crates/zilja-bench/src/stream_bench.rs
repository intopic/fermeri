//! Faza 5: stresi i gjithë rrugës (ngjarjet e validatorit → tracker → pema),
//! me fork-e, banka të vdekura dhe Rooted që vonohet 32 slote si në realitet.
//!
//! `cargo run -p zilja-bench --release --bin zilja-stream-bench -- [llogari] [slote] [ndryshime_per_slot]`

use rand::{rngs::StdRng, Rng, SeedableRng};
use serde_json::json;
use std::sync::atomic::Ordering;
use std::time::Instant;
use zilja_node::{Event, Service, Status};
use zilja_tree::{reference_root, Tree};
use zilja_verifier::{tree_key, AccountMeta, Key};

const ROOT_LAG: u64 = 32;

/// Ndryshimet e një sloti: (çelësi, llogaria; `None` = fshirë).
type SlotChanges = Vec<(Key, Option<AccountMeta>)>;

fn meta(rng: &mut StdRng) -> Option<AccountMeta> {
    Some(AccountMeta { lamports: rng.gen_range(1..1 << 40), data_len: 165, data_hash: rng.gen(), ..Default::default() })
}

/// Hash-i (i rremë, por unik) i bankës kanonike të një sloti.
fn bank_hash(slot: u64) -> [u8; 32] {
    let mut h = [0xAB; 32];
    h[..8].copy_from_slice(&slot.to_le_bytes());
    h
}

fn main() {
    let a: Vec<u64> = std::env::args().skip(1).filter_map(|x| x.parse().ok()).collect();
    let n = *a.first().unwrap_or(&1_000_000) as usize;
    let slots = *a.get(1).unwrap_or(&300);
    let per_slot = *a.get(2).unwrap_or(&5_000) as usize;
    let mut rng = StdRng::seed_from_u64(9);
    let mut keys: Vec<Key> = (0..n).map(|_| rng.gen()).collect();

    // Modeli: çfarë duhet të jetë gjendja në fund (vetëm zinxhiri kanonik).
    let mut model: std::collections::HashMap<Key, AccountMeta> = std::collections::HashMap::new();

    let mut svc = Service::start_opts(Default::default()).unwrap();
    let t0 = Instant::now();
    let mut events = 0u64;
    for k in &keys {
        let m = meta(&mut rng);
        model.insert(*k, m.clone().unwrap());
        svc.send(Event::Snapshot { slot: 0, pubkey: *k, meta: m });
        events += 1;
    }
    svc.send(Event::EndOfStartup);
    let mut bank_id = 0u64;
    let mut wv = 0u64;
    let mut forks = 0;
    let mut deads = 0;
    // pending[slot] = ndryshimet kanonike, aplikohen në model kur rrënjosen.
    let mut canonical: Vec<(u64, SlotChanges)> = Vec::new();
    // G14: zinxhiri kanonik (slot → bankë) dhe slotet e tij me radhë, për SlotHashes dhe Rooted.
    let mut canon_bank: std::collections::BTreeMap<u64, u64> = std::collections::BTreeMap::new();
    let mut chain: Vec<u64> = vec![0];
    let hashes = |chain: &[u64]| -> Vec<(u64, [u8; 32])> { chain.iter().rev().take(64).map(|s| (*s, bank_hash(*s))).collect() };
    for slot in 1..=slots {
        let prev = *chain.last().unwrap();
        // Ndonjëherë: një fork që do të braktiset, me ndryshime "helmuese". G14: zinxhiri kanonik e
        // kapërcen këtë slot (si në Solana: blloku i një udhëheqësi s'u pranua), ndaj fork-u s'ndan
        // slot me asnjë bankë kanonike; tracker-i ndalon (P-07/A24) kur dy banka të gjalla ndajnë një
        // slot të rrënjosur, sepse SlotHashes s'i dallon. Sloti i fundit mbetet gjithmonë kanonik.
        if slot < slots && rng.gen_bool(0.1) {
            bank_id += 1;
            forks += 1;
            svc.send(Event::SlotHashes { slot, bank_id, entries: hashes(&chain) });
            svc.send(Event::Status { slot, parent: Some(prev), bank_id: Some(bank_id), status: Status::CreatedBank });
            for _ in 0..per_slot / 10 {
                wv += 1;
                let k = keys[rng.gen_range(0..keys.len())];
                svc.send(Event::Account { slot, bank_id, pubkey: k, write_version: wv, meta: meta(&mut rng) });
                events += 1;
            }
            // Ndonjëherë fork-u vdes, ndonjëherë thjesht humbet garën.
            if rng.gen_bool(0.5) {
                deads += 1;
                svc.send(Event::Status { slot, parent: None, bank_id: Some(bank_id), status: Status::Dead });
            }
            continue;
        }
        bank_id += 1;
        let b = bank_id;
        canon_bank.insert(slot, b);
        svc.send(Event::SlotHashes { slot, bank_id: b, entries: hashes(&chain) });
        chain.push(slot);
        svc.send(Event::Status { slot, parent: Some(prev), bank_id: Some(b), status: Status::CreatedBank });
        let mut ups = Vec::with_capacity(per_slot);
        for _ in 0..per_slot {
            wv += 1;
            let r: f64 = rng.gen();
            let (k, m) = if r < 0.9 {
                (keys[rng.gen_range(0..keys.len())], meta(&mut rng))
            } else if r < 0.98 {
                let k: Key = rng.gen();
                keys.push(k);
                (k, meta(&mut rng))
            } else {
                (keys[rng.gen_range(0..keys.len())], None)
            };
            svc.send(Event::Account { slot, bank_id: b, pubkey: k, write_version: wv, meta: m.clone() });
            ups.push((k, m));
            events += 1;
        }
        canonical.push((slot, ups));
        svc.send(Event::Status { slot, parent: Some(prev), bank_id: Some(b), status: Status::Confirmed });
        // Rrënjoset sloti kanonik ROOT_LAG pas majës. Rooted mund të kapërcejë slote (si "highest
        // rooted"): ndonjëherë dërgohet vetëm çdo i dyti.
        if chain.len() > ROOT_LAG as usize + 1 {
            let i = chain.len() - 1 - ROOT_LAG as usize;
            let r = chain[i];
            if i.is_multiple_of(2) || rng.gen_bool(0.5) {
                svc.send(Event::Status { slot: r, parent: Some(chain[i - 1]), bank_id: canon_bank.get(&r).copied(), status: Status::Rooted });
            }
        }
    }
    // Rrënjos deri në fund (sloti i fundit është gjithmonë kanonik).
    let n_chain = chain.len();
    svc.send(Event::Status { slot: slots, parent: Some(chain[n_chain - 2]), bank_id: canon_bank.get(&slots).copied(), status: Status::Rooted });
    let shared = svc.shared.clone();
    svc.shutdown();
    let secs = t0.elapsed().as_secs_f64();

    for (_, ups) in canonical {
        for (k, m) in ups {
            match m {
                Some(m) => {
                    model.insert(k, m);
                }
                None => {
                    model.remove(&k);
                }
            }
        }
    }
    let st = shared.state.read().unwrap();
    // G14: modeli mban pubkey-t; pema e shërbimit mban tree_key(pubkey). Root-i i pritur llogaritet mbi
    // të njëjtët çelësa që përdor pema, përndryshe krahasimi dështon (ose kalon) pa kuptim.
    let expected = if n <= 200_000 {
        reference_root(&model.iter().map(|(k, m)| (tree_key(k), m.clone())).collect::<Vec<_>>())
    } else {
        Tree::from_accounts(model.iter().map(|(k, m)| (tree_key(k), Some(m.clone()))).collect()).root()
    };
    let ok = st.tree.root() == expected && st.slot == slots;
    let m = &shared.metrics;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "snapshot_accounts": n,
            "slots": slots,
            "updates_per_slot": per_slot,
            "forks": forks,
            "dead_banks": deads,
            "events": events,
            "seconds": secs,
            "events_per_second": events as f64 / secs,
            "abandoned_banks": m.abandoned_banks.load(Ordering::Relaxed),
            "missing_parents": m.missing_parents.load(Ordering::Relaxed),
            "startup_build_ms": m.startup_build_ms.load(Ordering::Relaxed),
            "max_apply_ms": m.max_apply_us.load(Ordering::Relaxed) as f64 / 1000.0,
            "mean_apply_ms": m.total_apply_us.load(Ordering::Relaxed) as f64 / 1000.0 / m.batches.load(Ordering::Relaxed).max(1) as f64,
            "final_slot": st.slot,
            "broken": st.broken,
            "root_matches_canonical_model": ok,
        }))
        .unwrap()
    );
    if !ok {
        std::process::exit(1);
    }
}
