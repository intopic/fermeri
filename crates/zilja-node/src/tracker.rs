//! Ndjekja e sloteve, bankave dhe fork-eve.
//!
//! Validatori na dërgon ndryshime llogarish për çdo bank (një slot mund të ketë
//! më shumë se një bank, p.sh. kur njëri vdes dhe zëvendësohet). Ne i mbajmë
//! mënjanë dhe i aplikojmë në pemë **vetëm kur sloti bëhet Rooted**. Kur rrënjoset
//! sloti S, rrënjosen edhe të gjithë paraardhësit e tij: i ndjekim prapa me anë
//! të `parent` dhe i aplikojmë nga më i vjetri te më i riu. Bankat e tjera me
//! slot ≤ S janë fork-e të braktisura dhe hidhen.

use std::collections::HashMap;
use zilja_tree::Update;
use zilja_verifier::{AccountMeta, Key};

pub type Slot = u64;
pub type BankId = u64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    CreatedBank,
    Processed,
    Confirmed,
    Rooted,
    Dead,
    Other,
}

#[derive(Clone, Debug)]
pub enum Event {
    /// Llogari nga snapshot-i gjatë nisjes.
    Snapshot { slot: Slot, pubkey: Key, meta: Option<AccountMeta> },
    /// Fundi i nisjes.
    EndOfStartup,
    /// Ndryshim llogarie gjatë përpunimit të transaksioneve.
    Account { slot: Slot, bank_id: BankId, pubkey: Key, write_version: u64, meta: Option<AccountMeta> },
    /// Ndryshim statusi. `bank_id = None` kur validatori nuk e jep (p.sh. Dead në disa versione).
    Status { slot: Slot, parent: Option<Slot>, bank_id: Option<BankId>, status: Status },
    /// Meta e bllokut: përdoret për të kontrolluar që, pas rinisjes, jemi në të njëjtin zinxhir.
    Block { slot: Slot, parent_slot: Option<Slot>, blockhash: String, parent_blockhash: Option<String> },
    /// Përmbajtja e sysvar-it SlotHashes në një bank: (slot, bank_hash) të paraardhësve, nga më i riu.
    /// Gjurma e gishtit të zinxhirit, e dukshme edhe gjatë ripërsëritjes pas rinisjes.
    SlotHashes { slot: Slot, bank_id: BankId, entries: Vec<(Slot, [u8; 32])> },
}

#[derive(Debug, PartialEq)]
pub enum Output {
    /// Pema fillestare nga snapshot-i.
    Startup { slot: Slot, accounts: Vec<Update> },
    /// Ndryshimet e bashkuara të një ose më shumë sloteve të sapo-rrënjosur.
    Rooted { slot: Slot, slots_in_batch: usize, updates: Vec<Update>, blockhash: Option<String> },
    /// Zilja nuk mund të vazhdojë saktë (boshllëk ose zinxhir tjetër). Duhet rindërtim nga snapshot-i.
    Broken(String),
}

#[derive(Default, Debug)]
struct Bank {
    slot: Slot,
    parent: Option<Slot>,
    dead: bool,
    /// pubkey → (write_version, meta)
    updates: HashMap<Key, (u64, Option<AccountMeta>)>,
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct TrackerStats {
    pub rooted_slots: u64,
    pub abandoned_banks: u64,
    pub missing_parents: u64,
    pub dead_banks: u64,
    pub stale_roots_ignored: u64,
    pub blocks_seen: u64,
}

#[derive(Default)]
pub struct Tracker {
    started: bool,
    snapshot_slot: Option<Slot>,
    startup: Vec<Update>,
    banks: HashMap<BankId, Bank>,
    /// Sloti i fundit i rrënjosur dhe i aplikuar.
    last_rooted: Option<Slot>,
    /// Rooted që erdhën para fundit të nisjes.
    deferred_roots: Vec<(Slot, Option<BankId>)>,
    /// `true` kur pema u ndërtua nga snapshot-i i plotë (mbulon çdo llogari).
    complete: bool,
    /// Hash-et e blloqeve për slotet ende të parrënjosura.
    blockhashes: HashMap<Slot, String>,
    /// Pas rinisjes nga disku: (sloti, blockhash) që duhet ta gjejmë në zinxhir.
    resume_check: Option<(Slot, Option<String>)>,
    resumed: bool,
    broken: bool,
    /// Për çdo bank: hyrja e parë e SlotHashes (prindi dhe hash-i i bankës së tij).
    parent_hash_by_bank: HashMap<BankId, (Slot, [u8; 32])>,
    pub stats: TrackerStats,
}

impl Tracker {
    /// A mbulon pema të gjitha llogaritë (nisje nga snapshot) apo vetëm ato që ndryshuan (nisje nga genesis)?
    pub fn complete(&self) -> bool {
        self.complete
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Vazhdon nga gjendja e ruajtur në disk, pa snapshot.
    pub fn resume(&mut self, slot: Slot, blockhash: Option<String>, complete: bool) {
        self.started = true;
        self.resumed = true;
        self.complete = complete;
        self.last_rooted = Some(slot);
        self.resume_check = Some((slot, blockhash));
    }

    /// A u konfirmua që pas rinisjes jemi në të njëjtin zinxhir?
    pub fn resume_verified(&self) -> bool {
        self.resumed && self.resume_check.is_none()
    }

    pub fn is_broken(&self) -> bool {
        self.broken
    }

    /// Merr llogaritë e snapshot-it të grumbulluara deri tani (që të mos mbahen të gjitha në RAM).
    pub fn take_startup_chunk(&mut self, min: usize) -> Option<(Slot, Vec<Update>)> {
        if self.started || self.startup.len() < min {
            return None;
        }
        Some((self.snapshot_slot.unwrap_or(0), std::mem::take(&mut self.startup)))
    }

    fn break_with(&mut self, why: String) -> Vec<Output> {
        if self.broken {
            return vec![];
        }
        self.broken = true;
        vec![Output::Broken(why)]
    }

    pub fn last_rooted(&self) -> Option<Slot> {
        self.last_rooted
    }

    pub fn pending_banks(&self) -> usize {
        self.banks.len()
    }

    pub fn handle(&mut self, ev: Event) -> Vec<Output> {
        match ev {
            Event::Snapshot { .. } if self.resumed => vec![],
            Event::Snapshot { slot, pubkey, meta } => {
                self.snapshot_slot = Some(self.snapshot_slot.map_or(slot, |s| s.max(slot)));
                self.startup.push((pubkey, meta));
                vec![]
            }
            Event::EndOfStartup => {
                if self.started {
                    return vec![];
                }
                self.started = true;
                self.complete = true;
                let accounts = std::mem::take(&mut self.startup);
                let slot = self.snapshot_slot.unwrap_or(0);
                if self.snapshot_slot.is_some() {
                    self.last_rooted = Some(slot);
                }
                let mut out = vec![Output::Startup { slot, accounts }];
                for (s, b) in std::mem::take(&mut self.deferred_roots) {
                    out.extend(self.root(s, b));
                }
                out
            }
            Event::Block { slot, parent_slot, blockhash, parent_blockhash } => {
                self.stats.blocks_seen += 1;
                if let Some((v, Some(expected))) = self.resume_check.clone().filter(|(_, f)| f.as_deref().is_some_and(|f| !f.starts_with("bank:"))) {
                    let verdict = if slot == v {
                        Some(blockhash == expected)
                    } else if parent_slot == Some(v) {
                        parent_blockhash.as_ref().map(|p| *p == expected)
                    } else if slot > v && parent_slot.is_some_and(|p| p < v) {
                        // Ky bllok kapërcen slotin tonë: s'është i njëjti zinxhir.
                        Some(false)
                    } else {
                        None
                    };
                    match verdict {
                        Some(true) => self.resume_check = None,
                        Some(false) => {
                            return self.break_with(format!(
                                "pas rinisjes, blloku i slotit {v} nuk përputhet: ledger-i është ndryshe nga ai i pemës në disk"
                            ))
                        }
                        None => {}
                    }
                } else if matches!(self.resume_check, Some((_, None))) {
                    // Pema në disk s'ka blockhash (p.sh. e ndërtuar nga snapshot pa slot të mëpasshëm).
                    self.resume_check = None;
                }
                if !self.last_rooted.is_some_and(|r| slot <= r) {
                    self.blockhashes.insert(slot, blockhash);
                }
                vec![]
            }
            Event::SlotHashes { slot, bank_id, entries } => {
                if let Some((v, Some(fp))) = self.resume_check.clone() {
                    if let Some((ps, ph)) = parse_fingerprint(&fp) {
                        let newest = entries.first().map(|e| e.0);
                        let oldest = entries.last().map(|e| e.0);
                        let verdict = match (newest, oldest) {
                            (Some(n), Some(o)) if ps <= n && ps >= o => {
                                Some(entries.iter().any(|(s, h)| *s == ps && *h == ph))
                            }
                            // Sloti ynë është më i vjetër se 512 slotet e SlotHashes: s'lidhet dot.
                            (Some(_), Some(o)) if ps < o && slot > v => Some(false),
                            _ => None,
                        };
                        match verdict {
                            Some(true) => self.resume_check = None,
                            Some(false) => {
                                return self.break_with(format!(
                                    "pas rinisjes, hash-i i bankës së slotit {ps} nuk gjendet në SlotHashes: zinxhir tjetër ose boshllëk"
                                ))
                            }
                            None => {}
                        }
                    }
                }
                if !self.last_rooted.is_some_and(|r| slot <= r) {
                    if let Some(first) = entries.first() {
                        self.parent_hash_by_bank.insert(bank_id, *first);
                    }
                }
                vec![]
            }
            Event::Account { slot, bank_id, pubkey, write_version, meta } => {
                if self.last_rooted.is_some_and(|r| slot <= r) {
                    return vec![];
                }
                let bank = self.banks.entry(bank_id).or_insert_with(|| Bank { slot, ..Default::default() });
                match bank.updates.get(&pubkey) {
                    Some((wv, _)) if *wv > write_version => {}
                    _ => {
                        bank.updates.insert(pubkey, (write_version, meta));
                    }
                }
                vec![]
            }
            Event::Status { slot, parent, bank_id, status } => {
                if let Some(b) = bank_id {
                    if !self.last_rooted.is_some_and(|r| slot <= r) {
                        let bank = self.banks.entry(b).or_insert_with(|| Bank { slot, ..Default::default() });
                        if parent.is_some() {
                            bank.parent = parent;
                        }
                    }
                }
                match status {
                    Status::Dead => {
                        match bank_id {
                            // Kur kemi bank_id, shëno vetëm atë bank.
                            Some(b) => {
                                if let Some(bank) = self.banks.get_mut(&b) {
                                    bank.dead = true;
                                }
                            }
                            // Përndryshe të gjitha bankat aktuale të këtij sloti; zëvendësuesja vjen më vonë me id të ri.
                            None => {
                                for bank in self.banks.values_mut().filter(|b| b.slot == slot) {
                                    bank.dead = true;
                                }
                            }
                        }
                        self.stats.dead_banks += 1;
                        vec![]
                    }
                    Status::Rooted => {
                        if self.broken {
                            return vec![];
                        }
                        if !self.started {
                            if self.startup.is_empty() && self.snapshot_slot.is_none() {
                                // Nisje nga genesis (pa snapshot): validatori nuk dërgon fund-nisjeje.
                                // Pema nis bosh dhe mbulon vetëm llogaritë që ndryshojnë: `complete = false`.
                                self.started = true;
                                self.complete = false;
                                let mut out = vec![Output::Startup { slot: 0, accounts: vec![] }];
                                out.extend(self.root(slot, bank_id));
                                return out;
                            }
                            self.deferred_roots.push((slot, bank_id));
                            return vec![];
                        }
                        self.root(slot, bank_id)
                    }
                    _ => vec![],
                }
            }
        }
    }

    fn live_bank_at(&self, slot: Slot) -> Option<BankId> {
        // Nëse ka disa (rrallë), merr atë që s'është i vdekur dhe ka id më të madh (më i riu).
        self.banks
            .iter()
            .filter(|(_, b)| b.slot == slot && !b.dead)
            .map(|(id, _)| *id)
            .max()
    }

    fn root(&mut self, slot: Slot, bank_id: Option<BankId>) -> Vec<Output> {
        if self.last_rooted.is_some_and(|r| slot <= r) {
            self.stats.stale_roots_ignored += 1;
            return vec![];
        }
        let start = bank_id.filter(|b| self.banks.contains_key(b)).or_else(|| self.live_bank_at(slot));
        // Zinxhiri i bankave nga S prapa deri te sloti i fundit i rrënjosur.
        let mut chain: Vec<BankId> = Vec::new();
        let mut cur = start;
        while let Some(b) = cur {
            chain.push(b);
            let parent = self.banks[&b].parent;
            cur = match parent {
                Some(p) if self.last_rooted.is_none_or(|r| p > r) => {
                    let next = self.live_bank_at(p);
                    if next.is_none() {
                        // Çdo slot ndryshon të paktën sysvar-ët, kështu që prindi duhet ta kishte bankën.
                        self.stats.missing_parents += 1;
                        if let Some(r) = self.last_rooted {
                            // Mungojnë slote mes r dhe p: vula do të ishte e gabuar.
                            return self.break_with(format!(
                                "boshllëk: sloti {p} (prindi i një sloti të rrënjosur) mungon pas slotit të fundit të ruajtur {r}"
                            ));
                        }
                    }
                    next
                }
                _ => None,
            };
        }
        chain.reverse();
        let slots_in_batch = chain.len();
        let mut merged: HashMap<Key, Option<AccountMeta>> = HashMap::new();
        for b in &chain {
            if let Some(bank) = self.banks.remove(b) {
                for (k, (_, m)) in bank.updates {
                    merged.insert(k, m);
                }
            }
        }
        // Hidh fork-et e braktisura.
        let before = self.banks.len();
        self.banks.retain(|_, b| b.slot > slot);
        self.stats.abandoned_banks += (before - self.banks.len()) as u64;
        self.stats.rooted_slots += 1;
        self.last_rooted = Some(slot);
        // Gjurma e gishtit: hash-i i bankës së prindit nga SlotHashes i vetë sloti S (zinxhiri kanonik),
        // ose blockhash-i kur SlotHashes mungon.
        let fp = start
            .and_then(|b| self.parent_hash_by_bank.get(&b))
            .map(|(ps, h)| format!("bank:{ps}:{}", hex32(h)));
        let blockhash = fp.or_else(|| self.blockhashes.get(&slot).cloned());
        self.blockhashes.retain(|s, _| *s > slot);
        let live: std::collections::HashSet<BankId> = self.banks.keys().copied().collect();
        self.parent_hash_by_bank.retain(|b, _| live.contains(b));
        vec![Output::Rooted { slot, slots_in_batch, updates: merged.into_iter().collect(), blockhash }]
    }
}

fn hex32(h: &[u8; 32]) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

/// `bank:<slot>:<hex>` → (slot, hash)
fn parse_fingerprint(s: &str) -> Option<(Slot, [u8; 32])> {
    let rest = s.strip_prefix("bank:")?;
    let (slot, hexs) = rest.split_once(':')?;
    let slot = slot.parse().ok()?;
    if hexs.len() != 64 {
        return None;
    }
    let mut h = [0u8; 32];
    for (i, b) in h.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hexs[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some((slot, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(l: u64) -> Option<AccountMeta> {
        Some(AccountMeta { lamports: l, ..Default::default() })
    }
    fn key(b: u8) -> Key {
        [b; 32]
    }
    fn acc(slot: Slot, bank_id: BankId, k: u8, wv: u64, l: u64) -> Event {
        Event::Account { slot, bank_id, pubkey: key(k), write_version: wv, meta: meta(l) }
    }
    fn status(slot: Slot, parent: Option<Slot>, bank_id: Option<BankId>, s: Status) -> Event {
        Event::Status { slot, parent, bank_id, status: s }
    }
    fn sorted(mut u: Vec<Update>) -> Vec<Update> {
        u.sort_by(|a, b| a.0.cmp(&b.0));
        u
    }

    fn started(snapshot_slot: Slot) -> Tracker {
        let mut t = Tracker::new();
        t.handle(Event::Snapshot { slot: snapshot_slot, pubkey: key(1), meta: meta(1) });
        let out = t.handle(Event::EndOfStartup);
        assert!(matches!(&out[0], Output::Startup { slot, accounts } if *slot == snapshot_slot && accounts.len() == 1));
        t
    }

    #[test]
    fn applies_only_rooted_and_highest_write_version() {
        let mut t = started(10);
        t.handle(status(11, Some(10), Some(100), Status::CreatedBank));
        t.handle(acc(11, 100, 2, 5, 50));
        t.handle(acc(11, 100, 2, 3, 30)); // më i vjetër: injorohet
        t.handle(acc(11, 100, 3, 6, 60));
        assert!(t.handle(status(11, Some(10), Some(100), Status::Confirmed)).is_empty());
        let (slot, n, ups) = rooted_updates(t.handle(status(11, Some(10), Some(100), Status::Rooted)));
        assert_eq!((slot, n), (11, 1));
        assert_eq!(ups, sorted(vec![(key(2), meta(50)), (key(3), meta(60))]));
        assert_eq!(t.last_rooted(), Some(11));
        assert_eq!(t.pending_banks(), 0);
    }

    fn rooted_updates(out: Vec<Output>) -> (Slot, usize, Vec<Update>) {
        match out.into_iter().next() {
            Some(Output::Rooted { slot, slots_in_batch, updates, .. }) => (slot, slots_in_batch, sorted(updates)),
            x => panic!("pritej Rooted, erdhi {x:?}"),
        }
    }

    #[test]
    fn root_skipping_slots_applies_ancestors_in_order_and_drops_forks() {
        let mut t = started(10);
        // Zinxhiri kryesor: 10 → 11 → 12 → 14 ; fork: 11 → 13 (i braktisur)
        t.handle(status(11, Some(10), Some(1), Status::CreatedBank));
        t.handle(acc(11, 1, 5, 1, 100));
        t.handle(status(12, Some(11), Some(2), Status::CreatedBank));
        t.handle(acc(12, 2, 5, 2, 200)); // mbishkruan 11
        t.handle(status(13, Some(11), Some(3), Status::CreatedBank));
        t.handle(acc(13, 3, 6, 3, 999)); // fork-u që humbet
        t.handle(status(14, Some(12), Some(4), Status::CreatedBank));
        t.handle(acc(14, 4, 7, 4, 400));
        let (slot, n, ups) = rooted_updates(t.handle(status(14, Some(12), Some(4), Status::Rooted)));
        assert_eq!((slot, n), (14, 3));
        assert_eq!(ups, sorted(vec![(key(5), meta(200)), (key(7), meta(400))]));
        assert_eq!(t.stats.abandoned_banks, 1);
        assert_eq!(t.pending_banks(), 0);
        // Ndryshim i vonuar për një slot të rrënjosur: injorohet.
        t.handle(acc(13, 3, 6, 9, 1));
        assert_eq!(t.pending_banks(), 0);
        // Rooted i vjetër: injorohet.
        assert!(t.handle(status(12, Some(11), Some(2), Status::Rooted)).is_empty());
        assert_eq!(t.stats.stale_roots_ignored, 1);
    }

    #[test]
    fn dead_bank_replaced_at_same_slot() {
        let mut t = started(10);
        t.handle(status(11, Some(10), Some(1), Status::CreatedBank));
        t.handle(acc(11, 1, 5, 1, 111)); // banka që vdes
        t.handle(status(11, None, None, Status::Dead));
        t.handle(status(11, Some(10), Some(2), Status::CreatedBank));
        t.handle(acc(11, 2, 5, 2, 222)); // zëvendësuesja
        t.handle(status(12, Some(11), Some(3), Status::CreatedBank));
        t.handle(acc(12, 3, 6, 3, 333));
        let (_, n, ups) = rooted_updates(t.handle(status(12, Some(11), Some(3), Status::Rooted)));
        assert_eq!(n, 2);
        assert_eq!(ups, sorted(vec![(key(5), meta(222)), (key(6), meta(333))]));
    }

    fn block(slot: Slot, parent: Slot, h: &str, ph: &str) -> Event {
        Event::Block { slot, parent_slot: Some(parent), blockhash: h.into(), parent_blockhash: Some(ph.into()) }
    }

    #[test]
    fn resume_on_same_chain_continues_and_carries_blockhash() {
        let mut t = Tracker::new();
        t.resume(50, Some("H50".into()), true);
        // Snapshot-i nuk duhet të vijë, por nëse vjen injorohet.
        t.handle(Event::Snapshot { slot: 40, pubkey: key(1), meta: meta(1) });
        // Ripërsëritja e sloteve të vjetra injorohet.
        t.handle(status(45, Some(44), Some(1), Status::CreatedBank));
        t.handle(acc(45, 1, 9, 1, 999));
        assert!(t.handle(status(45, Some(44), Some(1), Status::Rooted)).is_empty());
        t.handle(block(51, 50, "H51", "H50"));
        assert!(t.resume_verified());
        t.handle(status(51, Some(50), Some(2), Status::CreatedBank));
        t.handle(acc(51, 2, 3, 2, 30));
        let out = t.handle(status(51, Some(50), Some(2), Status::Rooted));
        match &out[0] {
            Output::Rooted { slot, updates, blockhash, .. } => {
                assert_eq!((*slot, updates.len(), blockhash.as_deref()), (51, 1, Some("H51")));
            }
            x => panic!("{x:?}"),
        }
    }

    #[test]
    fn resume_on_other_chain_breaks() {
        let mut t = Tracker::new();
        t.resume(50, Some("H50".into()), true);
        let out = t.handle(block(51, 50, "X51", "X50"));
        assert!(matches!(&out[0], Output::Broken(_)));
        assert!(t.is_broken());
        // Pas kësaj, asgjë nuk aplikohet.
        t.handle(status(51, Some(50), Some(2), Status::CreatedBank));
        assert!(t.handle(status(51, Some(50), Some(2), Status::Rooted)).is_empty());
    }

    fn sh(slot: Slot, bank_id: BankId, newest_parent: Slot, tag: u8) -> Event {
        // SlotHashes i bankës: paraardhësit nga prindi poshtë, me hash [tag; slot].
        let entries = (newest_parent.saturating_sub(20)..=newest_parent).rev().map(|s| (s, [tag ^ (s as u8); 32])).collect();
        Event::SlotHashes { slot, bank_id, entries }
    }

    #[test]
    fn fingerprint_from_slot_hashes_roundtrip() {
        // Punë normale: rrënjosja e slotit 11 ruan gjurmën (10, hash(10)) nga SlotHashes i bankës 11.
        let mut t = started(10);
        t.handle(status(11, Some(10), Some(1), Status::CreatedBank));
        t.handle(sh(11, 1, 10, 7));
        t.handle(acc(11, 1, 2, 1, 5));
        let out = t.handle(status(11, Some(10), Some(1), Status::Rooted));
        let fp = match &out[0] {
            Output::Rooted { blockhash: Some(f), .. } => f.clone(),
            x => panic!("{x:?}"),
        };
        assert_eq!(parse_fingerprint(&fp), Some((10, [7 ^ 10u8; 32])));

        // Rinisja në të njëjtin zinxhir: SlotHashes i një sloti të mëvonshëm e përmban → verifikuar.
        let mut t2 = Tracker::new();
        t2.resume(11, Some(fp.clone()), true);
        t2.handle(sh(5, 9, 4, 7)); // ripërsëritje e vjetër: s'jep vendim
        assert!(!t2.resume_verified());
        t2.handle(sh(30, 99, 29, 7));
        assert!(t2.resume_verified());

        // Rinisja në zinxhir tjetër (hash-e të tjera) → ndalon.
        let mut t3 = Tracker::new();
        t3.resume(11, Some(fp), true);
        let out = t3.handle(sh(30, 99, 29, 8));
        assert!(matches!(&out[0], Output::Broken(_)), "{out:?}");
    }

    #[test]
    fn gap_after_resume_breaks() {
        // Disku është në slotin 50, por validatori nis nga një snapshot i slotit 80.
        let mut t = Tracker::new();
        t.resume(50, Some("H50".into()), true);
        t.handle(status(81, Some(80), Some(1), Status::CreatedBank));
        t.handle(acc(81, 1, 3, 1, 30));
        let out = t.handle(status(81, Some(80), Some(1), Status::Rooted));
        assert!(matches!(&out[0], Output::Broken(_)), "{out:?}");
    }

    #[test]
    fn startup_chunks_are_drained() {
        let mut t = Tracker::new();
        for i in 0..10u8 {
            t.handle(Event::Snapshot { slot: 7, pubkey: key(i), meta: meta(1) });
        }
        assert!(t.take_startup_chunk(20).is_none());
        let (slot, c) = t.take_startup_chunk(5).unwrap();
        assert_eq!((slot, c.len()), (7, 10));
        t.handle(Event::Snapshot { slot: 7, pubkey: key(11), meta: meta(1) });
        let out = t.handle(Event::EndOfStartup);
        assert!(matches!(&out[0], Output::Startup { slot: 7, accounts } if accounts.len() == 1));
    }

    #[test]
    fn genesis_start_without_snapshot_is_marked_incomplete() {
        let mut t = Tracker::new();
        t.handle(status(1, Some(0), Some(1), Status::CreatedBank));
        t.handle(acc(1, 1, 5, 1, 10));
        let out = t.handle(status(1, Some(0), Some(1), Status::Rooted));
        assert_eq!(out.len(), 2);
        assert!(matches!(&out[0], Output::Startup { accounts, .. } if accounts.is_empty()));
        assert!(!t.complete());
        assert_eq!(t.last_rooted(), Some(1));
    }

    #[test]
    fn deletion_and_roots_before_startup_are_deferred() {
        let mut t = Tracker::new();
        t.handle(Event::Snapshot { slot: 20, pubkey: key(1), meta: meta(1) });
        t.handle(status(21, Some(20), Some(1), Status::CreatedBank));
        t.handle(Event::Account { slot: 21, bank_id: 1, pubkey: key(1), write_version: 1, meta: None });
        assert!(t.handle(status(21, Some(20), Some(1), Status::Rooted)).is_empty());
        let out = t.handle(Event::EndOfStartup);
        assert_eq!(out.len(), 2);
        let (slot, _, ups) = rooted_updates(out.into_iter().skip(1).collect());
        assert_eq!(slot, 21);
        assert_eq!(ups, vec![(key(1), None)]);
    }
}
