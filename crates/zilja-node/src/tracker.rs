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
    /// Llogari nga snapshot-i gjatë nisjes. Validatori dërgon çdo version të ruajtur të një
    /// llogarie (një për çdo slot ku ajo ndryshoi), në radhë të çfarëdoshme: fiton sloti më i ri (F-02).
    Snapshot { slot: Slot, pubkey: Key, meta: Option<AccountMeta> },
    /// Fundi i nisjes.
    EndOfStartup,
    /// Ndryshim llogarie gjatë përpunimit të transaksioneve.
    Account { slot: Slot, bank_id: BankId, pubkey: Key, write_version: u64, meta: Option<AccountMeta> },
    /// Ndryshim statusi. `bank_id = None` kur validatori nuk e jep (p.sh. Dead në disa versione).
    Status { slot: Slot, parent: Option<Slot>, bank_id: Option<BankId>, status: Status },
    /// Përmbajtja e sysvar-it SlotHashes në një bank: (slot, bank_hash) të paraardhësve, nga më i riu.
    /// Hyrja e parë është prindi i bankës. Vjen për çdo bank, edhe për ato që ripërsëriten gjatë
    /// nisjes pa asnjë status (F-01), ndaj është burimi kryesor i prindit.
    SlotHashes { slot: Slot, bank_id: BankId, entries: Vec<(Slot, [u8; 32])> },
    /// Alpenglow: një shënues UpdateParent e pastroi këtë bank (L-06). Zëvendësuesja vjen me CreatedBank.
    BankCleared { slot: Slot, bank_id: BankId },
}

/// Gjurma e zinxhirit: (slot, bank_hash). Ruhet me pemën dhe kontrollohet pas rinisjes.
pub type Fingerprint = zilja_tree::Fingerprint;

/// Paraardhësit e një banke nga SlotHashes: (slot, bank_hash), nga më i riu.
type Ancestors = Vec<(Slot, [u8; 32])>;

#[derive(Debug, PartialEq)]
pub enum Output {
    /// Pema fillestare nga snapshot-i: (sloti i llogarisë, ndryshimi).
    Startup { slot: Slot, accounts: Vec<(Slot, Update)> },
    /// Ndryshimet e bashkuara të një ose më shumë sloteve të sapo-rrënjosur.
    Rooted { slot: Slot, slots_in_batch: usize, updates: Vec<Update>, fingerprint: Option<Fingerprint> },
    /// Zilja nuk mund të vazhdojë saktë (boshllëk ose zinxhir tjetër). Duhet rindërtim nga snapshot-i.
    Broken(String),
}

#[derive(Default, Debug)]
struct Bank {
    slot: Slot,
    parent: Option<Slot>,
    dead: bool,
    /// Hyrja e parë e SlotHashes: (prindi, hash-i i bankës së prindit).
    parent_hash: Option<(Slot, [u8; 32])>,
    /// P-07: paraardhësit nga SlotHashes (nga më i riu), deri te sloti i fundit i rrënjosur. Lidhja e
    /// vetme e saktë bankë → bankë prind që jep Agave 4.3 (statuset japin vetëm slotin e prindit).
    ancestors: Vec<(Slot, [u8; 32])>,
    /// `CreatedBank` u pa: banka u vëzhgua që nga krijimi.
    created: bool,
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
    /// P-07: herët kur një slot kishte disa banka të gjalla që SlotHashes s'i dallonte (ndalim).
    pub ambiguous_banks: u64,
    pub parents_from_slot_hashes: u64,
}

/// A18: buxheti i bankave në pritje. Kur rrënjosja ngec, bankat dhe ndryshimet e tyre mbahen në RAM
/// brenda procesit të validatorit; mbi këtë buxhet zilja ndalon (fail-closed) para se ta shterojë.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_pending_banks: usize,
    pub max_pending_updates: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_pending_banks: 8192, max_pending_updates: 4_000_000 }
    }
}

#[derive(Default)]
pub struct Tracker {
    started: bool,
    snapshot_slot: Option<Slot>,
    startup: Vec<(Slot, Update)>,
    banks: HashMap<BankId, Bank>,
    /// Sloti i fundit i rrënjosur dhe i aplikuar.
    last_rooted: Option<Slot>,
    /// Rooted që erdhën para fundit të nisjes.
    deferred_roots: Vec<(Slot, Option<BankId>)>,
    /// `true` kur pema u ndërtua nga snapshot-i i plotë (mbulon çdo llogari).
    complete: bool,
    /// Pas rinisjes nga disku: sloti i ruajtur dhe gjurma që duhet ta gjejmë në SlotHashes.
    resume_check: Option<(Slot, Option<Fingerprint>)>,
    /// P-06: hash-i i bankës së slotit të fundit të rrënjosur, kur dihet. Lidhja e parë e çdo
    /// zinxhiri të ri duhet të jetë pikërisht me këtë bankë, jo vetëm me këtë slot.
    last_fp: Option<Fingerprint>,
    /// Plugin-i u ngarkua në mes të punës (`reloadPlugin`). A02: s'ka bazë të verifikueshme.
    mid_run_load: bool,
    resumed: bool,
    /// A02: nisje pa snapshot. E vlefshme vetëm kur zinxhiri i parë zbret deri te sloti 0 (genesis).
    genesis_boot: bool,
    /// A24: në slotin e fundit të rrënjosur jetuan disa banka: hash-i i bankës së rrënjosur s'mund të
    /// lidhet me BankId-në e saj, ndaj as gjurma s'ruhet, as lidhja e radhës (vetëm me slot) s'pranohet.
    base_ambiguous: bool,
    /// A18: kufijtë e kujtesës për bankat në pritje (0 = pa kufi).
    limits: Limits,
    /// A18: ndryshimet e llogarive në pritje (në të gjitha bankat e parrënjosura).
    pending_updates: usize,
    broken: bool,
    /// Grupimi i sloteve: root-i del vetëm për slotin e fundit kanonik të çdo dritareje prej
    /// `root_every` slotesh (kufiri = shumëfish i `root_every`). 0 ose 1 = çdo slot i rrënjosur.
    root_every: u64,
    /// Dritarja e hapur: (dritarja, sloti i fundit i rrënjosur në të, slote, ndryshimet e bashkuara).
    group: Option<Group>,
    pub stats: TrackerStats,
}

#[derive(Debug)]
struct Group {
    window: u64,
    last_slot: Slot,
    slots: usize,
    updates: HashMap<Key, Option<AccountMeta>>,
}

impl Tracker {
    /// A mbulon pema të gjitha llogaritë (nisje nga snapshot) apo vetëm ato që ndryshuan (nisje nga genesis)?
    pub fn complete(&self) -> bool {
        self.complete
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_mid_run_load(&mut self, v: bool) {
        self.mid_run_load = v;
    }

    /// Grupimi i sloteve (`root_every` = K): një root për slotin e fundit kanonik të çdo dritareje
    /// `((n−1)·K, n·K]`. I njëjti rregull në çdo nyje jep root-e te të njëjtat slote, sido që
    /// validatori t'i njoftojë rrënjosjet (një nga një ose disa bashkë). 0 ose 1 = çdo slot.
    pub fn set_root_every(&mut self, k: u64) {
        self.root_every = k.max(1);
    }

    /// Dritarja e slotit `s` për grupimin: `ceil(s / K)`. Kufiri i dritares `n` është sloti `n·K`.
    fn window(&self, s: Slot) -> u64 {
        let k = self.root_every.max(1);
        s.div_ceil(k)
    }

    /// A18: vendos buxhetin e kujtesës.
    pub fn set_limits(&mut self, l: Limits) {
        self.limits = l;
    }

    pub fn pending_updates(&self) -> usize {
        self.pending_updates
    }

    /// A18: kontrollon buxhetin pas çdo ngjarjeje që shton gjendje.
    fn over_budget(&mut self) -> Vec<Output> {
        let l = self.limits;
        if l.max_pending_banks > 0 && self.banks.len() > l.max_pending_banks {
            return self.break_with(format!(
                "{} banka në pritje (kufiri {}): rrënjosja ka ngecur; zilja ndalon para se të shterojë kujtesën e validatorit",
                self.banks.len(),
                l.max_pending_banks
            ));
        }
        if l.max_pending_updates > 0 && self.pending_updates > l.max_pending_updates {
            return self.break_with(format!(
                "{} ndryshime llogarish në pritje (kufiri {}): rrënjosja ka ngecur; zilja ndalon para se të shterojë kujtesën e validatorit",
                self.pending_updates,
                l.max_pending_updates
            ));
        }
        vec![]
    }

    /// Vazhdon nga gjendja e ruajtur në disk, pa snapshot. Asnjë slot nuk aplikohet derisa
    /// gjurma të gjendet në SlotHashes e një banke të re (F-11).
    pub fn resume(&mut self, slot: Slot, fingerprint: Option<Fingerprint>, complete: bool) {
        self.started = true;
        self.resumed = true;
        self.complete = complete;
        self.last_rooted = Some(slot);
        // P-06: vetëm gjurma e vetë bankës së slotit të ruajtur e konfirmon atë. Një gjurmë e prindit
        // (gjendje të vjetra) s'mjafton: trajtohet si mungesë, dhe pema rindërtohet.
        self.resume_check = Some((slot, fingerprint.filter(|(s, _)| *s == slot)));
    }

    /// A u konfirmua që pas rinisjes jemi në të njëjtin zinxhir?
    pub fn resume_verified(&self) -> bool {
        self.resumed && self.resume_check.is_none()
    }

    pub fn is_broken(&self) -> bool {
        self.broken
    }

    /// Merr llogaritë e snapshot-it të grumbulluara deri tani (që të mos mbahen të gjitha në RAM).
    pub fn take_startup_chunk(&mut self, min: usize) -> Option<Vec<(Slot, Update)>> {
        if self.started || self.startup.len() < min {
            return None;
        }
        Some(std::mem::take(&mut self.startup))
    }

    fn break_with(&mut self, why: String) -> Vec<Output> {
        if self.broken {
            return vec![];
        }
        self.halt();
        vec![Output::Broken(why)]
    }

    /// Ndalim i përhershëm (F-05): hidhet gjithçka në pritje dhe asgjë e re nuk mbahet më.
    pub fn halt(&mut self) {
        self.broken = true;
        self.pending_updates = 0;
        self.banks = HashMap::new();
        self.deferred_roots = Vec::new();
        self.startup = Vec::new();
    }

    pub fn last_rooted(&self) -> Option<Slot> {
        self.last_rooted
    }

    pub fn pending_banks(&self) -> usize {
        self.banks.len()
    }

    fn is_old(&self, slot: Slot) -> bool {
        self.last_rooted.is_some_and(|r| slot <= r)
    }

    pub fn handle(&mut self, ev: Event) -> Vec<Output> {
        match ev {
            Event::Snapshot { .. } if self.broken => vec![],
            // Pema në disk vazhdon: snapshot-i injorohet, përveç kur është më i ri se ajo (boshllëk).
            Event::Snapshot { slot, .. } if self.resumed => match self.last_rooted {
                Some(v) if slot > v => self.break_with(format!(
                    "snapshot-i (slot {slot}) është më i ri se pema në disk (slot {v}): do të rindërtohet në nisjen e ardhshme"
                )),
                _ => vec![],
            },
            Event::Snapshot { slot, pubkey, meta } => {
                self.snapshot_slot = Some(self.snapshot_slot.map_or(slot, |s| s.max(slot)));
                self.startup.push((slot, (pubkey, meta)));
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
            Event::SlotHashes { slot, bank_id, entries } => {
                if let Some((v, Some((ps, ph)))) = self.resume_check {
                    let newest = entries.first().map(|e| e.0);
                    let oldest = entries.last().map(|e| e.0);
                    let verdict = match (newest, oldest) {
                        (Some(n), Some(o)) if ps <= n && ps >= o => Some(entries.iter().any(|(s, h)| *s == ps && *h == ph)),
                        // Sloti ynë është më i vjetër se slotet e SlotHashes të një banke të re: s'lidhet dot.
                        (Some(_), Some(o)) if ps < o && slot > v => Some(false),
                        _ => None,
                    };
                    match verdict {
                        Some(true) => {
                            self.resume_check = None;
                            self.last_fp = Some((ps, ph));
                        }
                        Some(false) => {
                            return self.break_with(format!(
                                "pas rinisjes, hash-i i bankës së slotit {ps} nuk gjendet në SlotHashes: zinxhir tjetër ose boshllëk"
                            ))
                        }
                        None => {}
                    }
                }
                if !self.is_old(slot) && !self.broken {
                    let floor = self.last_rooted.unwrap_or(0);
                    let bank = self.banks.entry(bank_id).or_insert_with(|| Bank { slot, ..Default::default() });
                    bank.ancestors = entries.iter().take_while(|(s, _)| *s >= floor).take(1024).copied().collect();
                    if let Some(first) = entries.first() {
                        bank.parent_hash = Some(*first);
                        if bank.parent != Some(first.0) {
                            self.stats.parents_from_slot_hashes += 1;
                        }
                        bank.parent = Some(first.0);
                    }
                }
                vec![]
            }
            Event::BankCleared { bank_id, .. } => {
                if let Some(b) = self.banks.get_mut(&bank_id) {
                    b.dead = true;
                }
                vec![]
            }
            Event::Account { slot, bank_id, pubkey, write_version, meta } => {
                if self.is_old(slot) || self.broken {
                    return vec![];
                }
                let bank = self.banks.entry(bank_id).or_insert_with(|| Bank { slot, ..Default::default() });
                match bank.updates.get(&pubkey) {
                    Some((wv, _)) if *wv > write_version => {}
                    _ => {
                        if bank.updates.insert(pubkey, (write_version, meta)).is_none() {
                            self.pending_updates += 1;
                        }
                    }
                }
                self.over_budget()
            }
            Event::Status { slot, parent, bank_id, status } => {
                if let Some(b) = bank_id {
                    if !self.is_old(slot) && !self.broken {
                        let bank = self.banks.entry(b).or_insert_with(|| Bank { slot, ..Default::default() });
                        if status == Status::CreatedBank {
                            bank.created = true;
                        }
                        // SlotHashes ka përparësi; statusi plotëson kur ai mungon.
                        if bank.parent.is_none() {
                            bank.parent = parent;
                        }
                    }
                }
                if !self.broken && self.limits.max_pending_banks > 0 && self.banks.len() > self.limits.max_pending_banks {
                    return self.over_budget();
                }
                match status {
                    Status::Dead => {
                        match bank_id {
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
                        if self.mid_run_load {
                            // A02: Agave lëshon `CreatedBank` pasi konstruktori i bankës ka shkruar
                            // tashmë llogari (epoka, shpërblimet, sysvar-ët), dhe ndryshimet e bankave
                            // në fluturim i shkuan instancës së vjetër. Asgjë s'provon që një bankë u
                            // vëzhgua e plotë: s'aplikohet asnjë; rinisja e validatorit jep bazën.
                            return self.break_with(format!(
                                "plugin-i u ringarkua në mes të punës (sloti i rrënjosur {slot}): s'ka bazë të verifikueshme për bankat në fluturim; rinis validatorin"
                            ));
                        }
                        if !self.started {
                            if self.startup.is_empty() && self.snapshot_slot.is_none() {
                                // Nisje nga genesis (pa snapshot): validatori nuk dërgon fund-nisjeje.
                                // Pema nis bosh dhe mbulon vetëm llogaritë që ndryshojnë: `complete = false`.
                                self.started = true;
                                self.complete = false;
                                self.genesis_boot = true;
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

    /// P-07: banka e gjallë në `slot` që është paraardhësi i `child` (kur jepet). Kandidatët filtrohen
    /// sipas SlotHashes: paraardhësit e kandidatit duhet të përputhen me ata të fëmijës. Kur mbetet
    /// më shumë se një, s'zgjidhet asnjë me hamendje: gabim (ndalim).
    fn bank_at(&mut self, slot: Slot, child: Option<BankId>) -> Result<Option<BankId>, String> {
        let want: Vec<(Slot, [u8; 32])> = child.and_then(|c| self.banks.get(&c)).map(|c| c.ancestors.clone()).unwrap_or_default();
        // Te fëmija, hyrja pas `slot` është prindi i vërtetë i bankës që kërkojmë.
        let want_parent = want.iter().position(|(s, _)| *s == slot).and_then(|i| want.get(i + 1)).copied();
        let consistent = |b: &Bank| {
            let parent_ok = match (b.ancestors.first(), want_parent) {
                (Some(got), Some(w)) => *got == w,
                _ => true,
            };
            // Çdo paraardhës i përbashkët duhet të ketë të njëjtin hash.
            parent_ok && b.ancestors.iter().all(|(s, h)| want.iter().find(|(ws, _)| ws == s).is_none_or(|(_, wh)| wh == h))
        };
        let cands: Vec<BankId> = self.banks.iter().filter(|(_, b)| b.slot == slot && !b.dead && consistent(b)).map(|(id, _)| *id).collect();
        match cands.len() {
            0 => Ok(None),
            1 => Ok(Some(cands[0])),
            n => {
                self.stats.ambiguous_banks += 1;
                Err(format!(
                    "sloti {slot} ka {n} banka të gjalla që SlotHashes s'i dallon (bankat {cands:?}): s'zgjidhet asnjë me hamendje"
                ))
            }
        }
    }

    fn root(&mut self, slot: Slot, bank_id: Option<BankId>) -> Vec<Output> {
        if self.is_old(slot) {
            self.stats.stale_roots_ignored += 1;
            return vec![];
        }
        // F-11: pas rinisjes asgjë nuk aplikohet pa e konfirmuar zinxhirin.
        if let Some((v, fp)) = self.resume_check {
            let why = match fp {
                None => format!("pas rinisjes, pema në disk (slot {v}) nuk ka gjurmë zinxhiri: s'mund të konfirmohet"),
                Some((ps, _)) => format!(
                    "pas rinisjes, gjurma e slotit {ps} nuk u gjet në asnjë SlotHashes para rrënjosjes së slotit {slot}"
                ),
            };
            return self.break_with(why);
        }
        let start = match bank_id.filter(|b| self.banks.contains_key(b)) {
            Some(b) => Some(b),
            None => match self.bank_at(slot, None) {
                Ok(b) => b,
                Err(e) => return self.break_with(e),
            },
        };
        let Some(start) = start else {
            self.stats.missing_parents += 1;
            return self.break_with(format!("sloti i rrënjosur {slot} nuk ka asnjë bank të njohur"));
        };
        // Zinxhiri i bankave nga S prapa. Duhet të mbarojë SAKTËSISHT te sloti i fundit i rrënjosur (F-01).
        let mut chain: Vec<BankId> = vec![start];
        let mut cur = start;
        loop {
            let parent = self.banks[&cur].parent;
            match (parent, self.last_rooted) {
                (Some(p), Some(r)) if p == r => {
                    // A24: në slotin e rrënjosur jetuan disa banka dhe hash-i i së rrënjosurës s'dihet:
                    // lidhja vetëm me slot mund të jetë me bankën tjetër. S'pranohet.
                    if self.last_fp.is_none() && self.base_ambiguous {
                        return self.break_with(format!(
                            "sloti {} lidhet me slotin {r}, ku jetuan disa banka: lidhja me BankId-në e rrënjosur s'mund të verifikohet",
                            self.banks[&cur].slot
                        ));
                    }
                    // P-06: lidhja me bankën e rrënjosur, jo vetëm me slotin e saj.
                    if let Some((_, want)) = self.last_fp {
                        match self.banks[&cur].parent_hash {
                            Some((_, got)) if got == want => {}
                            Some(_) => {
                                return self.break_with(format!(
                                    "sloti {} rrjedh nga një bankë tjetër në slotin {r} (hash-i s'përputhet me bankën e rrënjosur)",
                                    self.banks[&cur].slot
                                ))
                            }
                            None => {
                                return self.break_with(format!(
                                    "banka e slotit {} s'ka SlotHashes: lidhja me bankën e rrënjosur në {r} s'mund të verifikohet",
                                    self.banks[&cur].slot
                                ))
                            }
                        }
                    }
                    break;
                }
                (Some(p), Some(r)) if p < r => {
                    return self.break_with(format!(
                        "zinxhiri i slotit {slot} kalon nga sloti {p}, nën slotin e fundit të ruajtur {r}: zinxhir tjetër ose boshllëk"
                    ));
                }
                (Some(p), _) => match self.bank_at(p, Some(cur)) {
                    Err(e) => return self.break_with(e),
                    Ok(Some(next)) => {
                        chain.push(next);
                        cur = next;
                    }
                    // Nisje nga genesis: banka e parë rrjedh nga sloti 0. A02: një zinxhir që s'zbret te
                    // genesis do të thotë që plugin-i u ngarkua në mes të punës (`loadPlugin`), pas
                    // bankave që s'i pa: s'ka bazë.
                    Ok(None) if self.last_rooted.is_none() && p == 0 => break,
                    Ok(None) if self.last_rooted.is_none() => {
                        self.stats.missing_parents += 1;
                        return self.break_with(format!(
                            "nisje pa snapshot, por zinxhiri i slotit {slot} s'zbret te genesis (mungon sloti {p}): plugin-i u ngarkua në mes të punës; rinis validatorin"
                        ));
                    }
                    Ok(None) => {
                        self.stats.missing_parents += 1;
                        return self.break_with(format!(
                            "boshllëk: sloti {p} (paraardhës i slotit të rrënjosur {slot}) mungon pas slotit të fundit të ruajtur {}",
                            self.last_rooted.unwrap()
                        ));
                    }
                },
                // Pa prind dhe pa bazë: vetëm vetë banka e genesis-it.
                (None, None) if self.banks[&cur].slot == 0 => break,
                (None, None) => {
                    self.stats.missing_parents += 1;
                    let s = self.banks[&cur].slot;
                    return self.break_with(format!("nisje pa snapshot: prindi i slotit {s} është i panjohur, zinxhiri s'lidhet me genesis"));
                }
                (None, Some(r)) => {
                    self.stats.missing_parents += 1;
                    let s = self.banks[&cur].slot;
                    return self.break_with(format!(
                        "prindi i slotit {s} është i panjohur (pa status dhe pa SlotHashes): s'mund të lidhet me slotin {r}"
                    ));
                }
            }
        }
        chain.reverse();
        // A24: sa banka të gjalla (jo Dead, jo të pastruara) kishte sloti i rrënjosur. Agave s'e jep
        // lidhjen BankId → hash-i i bankës; hash-i nxirret nga SlotHashes i pasardhësve, të cilët
        // lidhen me slotin, jo me BankId-në. Vetëm me një bankë të vetme në slot ky hash është i saji.
        let live_at_slot = self.banks.values().filter(|b| b.slot == slot && !b.dead).count();
        // Grupimi: bankat e zinxhirit (nga më e vjetra) ndahen sipas dritareve. Një dritare mbyllet kur
        // arrin kufirin (sloti n·K) ose kur vjen një bankë e dritares tjetër; e fundit, e pambyllur,
        // mbetet e hapur për rrënjosjen e radhës. Me K = 1 çdo slot mbyllet menjëherë.
        let k = self.root_every.max(1);
        let mut closed: Vec<Group> = Vec::new();
        // Gjurmët e mundshme për slotet e grupeve: hash-et e paraardhësve në bankat e zinxhirit.
        let mut chain_anc: Vec<(Slot, Ancestors)> = Vec::new();
        let mut live_at: HashMap<Slot, usize> = HashMap::new();
        for b in self.banks.values().filter(|b| !b.dead && b.slot <= slot) {
            *live_at.entry(b.slot).or_default() += 1;
        }
        for b in &chain {
            if let Some(bank) = self.banks.remove(b) {
                let w = self.window(bank.slot);
                if k > 1 && self.group.as_ref().is_some_and(|g| g.window != w) {
                    closed.push(self.group.take().unwrap());
                }
                let g = self.group.get_or_insert_with(|| Group { window: w, last_slot: bank.slot, slots: 0, updates: HashMap::new() });
                g.last_slot = bank.slot;
                g.slots += 1;
                for (key, (_, m)) in bank.updates {
                    g.updates.insert(key, m);
                }
                if k > 1 && bank.slot.is_multiple_of(k) {
                    closed.push(self.group.take().unwrap());
                }
                chain_anc.push((bank.slot, bank.ancestors));
            }
        }
        // K = 1: si më parë, një Rooted i vetëm për gjithë zinxhirin.
        if k <= 1 {
            closed.extend(self.group.take());
        }
        // Hidh fork-et e braktisura.
        let before = self.banks.len();
        self.banks.retain(|_, b| b.slot > slot);
        self.stats.abandoned_banks += (before - self.banks.len()) as u64;
        self.stats.rooted_slots += 1;
        self.last_rooted = Some(slot);
        // Gjurma (F-11, P-06): hash-i i bankës së vetë slotit S, nga SlotHashes i bankave pasardhëse.
        // Kurrë hash-i i prindit. Kur s'dihet (asnjë pasardhës ende) ose kur pasardhësit s'bien dakord,
        // gjurma mungon: gjendja mbetet e paverifikueshme pas rinisjes dhe rindërtohet.
        let mut seen: Vec<[u8; 32]> = self.banks.values().filter(|b| !b.dead).filter_map(|b| b.ancestors.iter().find(|(s, _)| *s == slot).map(|(_, h)| *h)).collect();
        seen.sort();
        seen.dedup();
        self.base_ambiguous = live_at_slot > 1;
        let fingerprint = match seen.as_slice() {
            [h] if !self.base_ambiguous => Some((slot, *h)),
            _ => None,
        };
        if self.base_ambiguous {
            self.stats.ambiguous_banks += 1;
        }
        if let Some((_, h)) = fingerprint {
            // Bankat që njohin një bankë tjetër në S s'rrjedhin nga rrënja: hidhen.
            self.banks.retain(|_, b| b.ancestors.iter().find(|(s, _)| *s == slot).is_none_or(|(_, x)| *x == h));
        }
        self.last_fp = fingerprint;
        // A18: ndryshimet në pritje pas heqjes së zinxhirit dhe të fork-eve.
        self.pending_updates = self.banks.values().map(|b| b.updates.len()).sum::<usize>() + self.group.as_ref().map_or(0, |g| g.updates.len());
        closed
            .into_iter()
            .map(|g| {
                let fp = if g.last_slot == slot {
                    fingerprint
                } else {
                    // Sloti i grupit është nën S: hash-i i bankës së tij nga SlotHashes i pasardhësve
                    // (bankat e zinxhirit pas tij dhe ato që mbetën), vetëm kur është i vetëm dhe i qartë.
                    let mut seen: Vec<[u8; 32]> = chain_anc
                        .iter()
                        .filter(|(s, _)| *s > g.last_slot)
                        .flat_map(|(_, a)| a.iter())
                        .chain(self.banks.values().filter(|b| !b.dead).flat_map(|b| b.ancestors.iter()))
                        .filter(|(s, _)| *s == g.last_slot)
                        .map(|(_, h)| *h)
                        .collect();
                    seen.sort();
                    seen.dedup();
                    match seen.as_slice() {
                        [h] if live_at.get(&g.last_slot).copied().unwrap_or(0) <= 1 => Some((g.last_slot, *h)),
                        _ => None,
                    }
                };
                Output::Rooted { slot: g.last_slot, slots_in_batch: g.slots, updates: g.updates.into_iter().collect(), fingerprint: fp }
            })
            .collect()
    }
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
        u.sort_by_key(|a| a.0);
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

    /// Zinxhiri 10 → 11 → … → `to`, një llogari për slot (çelësi = sloti % 4, vlera = sloti).
    fn chain(t: &mut Tracker, from: Slot, to: Slot, skip: &[Slot]) {
        let mut parent = from - 1;
        for s in from..=to {
            if skip.contains(&s) {
                continue;
            }
            t.handle(status(s, Some(parent), Some(s), Status::CreatedBank));
            t.handle(acc(s, s, (s % 4) as u8, s, s));
            parent = s;
        }
    }
    fn rooted_all(out: Vec<Output>) -> Vec<(Slot, usize, Vec<Update>)> {
        out.into_iter()
            .map(|o| match o {
                Output::Rooted { slot, slots_in_batch, updates, .. } => (slot, slots_in_batch, sorted(updates)),
                x => panic!("pritej Rooted, erdhi {x:?}"),
            })
            .collect()
    }

    /// Grupimi K = 4: root-e vetëm te kufijtë 12 dhe 16, me ndryshimet e bashkuara, sido që
    /// rrënjosjet të vijnë një nga një apo të gjitha bashkë (të njëjtat root-e në çdo nyje).
    #[test]
    fn root_every_k_is_deterministic() {
        let mut one = started(10);
        one.set_root_every(4);
        chain(&mut one, 11, 17, &[]);
        let mut a = Vec::new();
        for s in 11..=17 {
            a.extend(rooted_all(one.handle(status(s, Some(s - 1), Some(s), Status::Rooted))));
        }
        let mut all = started(10);
        all.set_root_every(4);
        chain(&mut all, 11, 17, &[]);
        let b = rooted_all(all.handle(status(17, Some(16), Some(17), Status::Rooted)));
        let want = vec![
            (12, 2, sorted(vec![(key(3), meta(11)), (key(0), meta(12))])),
            (16, 4, sorted(vec![(key(1), meta(13)), (key(2), meta(14)), (key(3), meta(15)), (key(0), meta(16))])),
        ];
        assert_eq!(a, want);
        assert_eq!(b, want);
        // Sloti 17 mbetet në dritaren e hapur: del te kufiri 20 bashkë me 18–20.
        assert_eq!(one.pending_updates(), 1);
        chain(&mut one, 18, 20, &[]);
        let c = rooted_all(one.handle(status(20, Some(19), Some(20), Status::Rooted)));
        assert_eq!(c, vec![(20, 4, sorted(vec![(key(1), meta(17)), (key(2), meta(18)), (key(3), meta(19)), (key(0), meta(20))]))]);
        assert_eq!(one.pending_updates(), 0);
    }

    /// Kufiri i kapërcyer (sloti 16 pa bllok): dritarja mbyllet te sloti i fundit kanonik (15),
    /// kur vjen banka e parë e dritares tjetër.
    #[test]
    fn root_every_k_skipped_boundary_closes_at_last_slot() {
        let mut t = started(10);
        t.set_root_every(4);
        chain(&mut t, 11, 17, &[16]);
        let r = rooted_all(t.handle(status(15, Some(14), Some(15), Status::Rooted)));
        assert_eq!(r.iter().map(|x| x.0).collect::<Vec<_>>(), vec![12], "15 pret: dritarja 13–16 s'është mbyllur");
        let r = rooted_all(t.handle(status(17, Some(15), Some(17), Status::Rooted)));
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].0, r[0].1), (15, 3));
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

    fn sh(slot: Slot, bank_id: BankId, newest_parent: Slot, tag: u8) -> Event {
        // SlotHashes i bankës: paraardhësit nga prindi poshtë, me hash [tag ^ slot].
        let entries = (newest_parent.saturating_sub(20)..=newest_parent).rev().map(|s| (s, [tag ^ (s as u8); 32])).collect();
        Event::SlotHashes { slot, bank_id, entries }
    }
    fn fp(s: Slot, tag: u8) -> Option<Fingerprint> {
        Some((s, [tag ^ (s as u8); 32]))
    }
    fn fingerprint_of(out: &[Output]) -> Option<Fingerprint> {
        out.iter().find_map(|o| match o {
            Output::Rooted { fingerprint, .. } => *fingerprint,
            _ => None,
        })
    }

    #[test]
    fn resume_on_same_chain_continues_and_carries_fingerprint() {
        let mut t = Tracker::new();
        t.resume(50, fp(50, 7), true);
        // Snapshot-i nuk duhet të vijë, por nëse vjen injorohet.
        t.handle(Event::Snapshot { slot: 40, pubkey: key(1), meta: meta(1) });
        // Ripërsëritja e sloteve të vjetra injorohet.
        t.handle(status(45, Some(44), Some(1), Status::CreatedBank));
        t.handle(acc(45, 1, 9, 1, 999));
        assert!(t.handle(status(45, Some(44), Some(1), Status::Rooted)).is_empty());
        t.handle(sh(51, 2, 50, 7));
        assert!(t.resume_verified());
        t.handle(status(51, Some(50), Some(2), Status::CreatedBank));
        t.handle(acc(51, 2, 3, 2, 30));
        t.handle(sh(52, 3, 51, 7)); // fëmija i 51: jep hash-in e vetë slotit 51
        let out = t.handle(status(51, Some(50), Some(2), Status::Rooted));
        assert_eq!(fingerprint_of(&out), fp(51, 7));
        assert!(matches!(&out[0], Output::Rooted { slot: 51, updates, .. } if updates.len() == 1));
    }

    #[test]
    fn resume_on_other_chain_breaks() {
        let mut t = Tracker::new();
        t.resume(50, fp(50, 7), true);
        let out = t.handle(sh(51, 2, 50, 8));
        assert!(matches!(&out[0], Output::Broken(_)));
        assert!(t.is_broken());
        // Pas kësaj, asgjë nuk aplikohet.
        t.handle(status(51, Some(50), Some(2), Status::CreatedBank));
        assert!(t.handle(status(51, Some(50), Some(2), Status::Rooted)).is_empty());
    }

    #[test]
    fn resume_unverified_never_applies() {
        // F-11: pa asnjë SlotHashes që e konfirmon, Rooted-i i parë mbi 50 ndalon.
        let mut t = Tracker::new();
        t.resume(50, fp(50, 7), true);
        t.handle(status(51, Some(50), Some(2), Status::CreatedBank));
        t.handle(acc(51, 2, 3, 2, 30));
        let out = t.handle(status(51, Some(50), Some(2), Status::Rooted));
        assert!(matches!(&out[0], Output::Broken(_)), "{out:?}");
        // Edhe pa gjurmë fare.
        let mut t = Tracker::new();
        t.resume(50, None, true);
        t.handle(sh(51, 2, 50, 7));
        let out = t.handle(status(51, Some(50), Some(2), Status::Rooted));
        assert!(matches!(&out[0], Output::Broken(_)), "{out:?}");
    }

    #[test]
    fn fingerprint_from_slot_hashes_roundtrip() {
        let mut t = started(10);
        t.handle(status(11, Some(10), Some(1), Status::CreatedBank));
        t.handle(sh(11, 1, 10, 7));
        t.handle(acc(11, 1, 2, 1, 5));
        t.handle(sh(12, 2, 11, 7));
        let f = fingerprint_of(&t.handle(status(11, Some(10), Some(1), Status::Rooted)));
        assert_eq!(f, fp(11, 7));

        // Rinisja në të njëjtin zinxhir: SlotHashes i një sloti të mëvonshëm e përmban → verifikuar.
        let mut t2 = Tracker::new();
        t2.resume(11, f, true);
        t2.handle(sh(5, 9, 4, 7)); // ripërsëritje e vjetër: s'jep vendim
        assert!(!t2.resume_verified());
        t2.handle(sh(30, 99, 29, 7));
        assert!(t2.resume_verified());

        // Rinisja në zinxhir tjetër (hash-e të tjera) → ndalon.
        let mut t3 = Tracker::new();
        t3.resume(11, f, true);
        let out = t3.handle(sh(30, 99, 29, 8));
        assert!(matches!(&out[0], Output::Broken(_)), "{out:?}");
    }

    #[test]
    fn startup_replay_without_statuses_uses_slot_hashes_parents() {
        // F-01: snapshot 10, ripërsëritje 11..13 me shkrime dhe SlotHashes por pa statuse.
        let mut t = started(10);
        for s in 11..=13u64 {
            t.handle(sh(s, 100 + s, s - 1, 7));
            t.handle(acc(s, 100 + s, s as u8, s, s * 10));
        }
        t.handle(status(14, Some(13), Some(114), Status::CreatedBank));
        let (slot, n, ups) = rooted_updates(t.handle(status(13, Some(12), Some(113), Status::Rooted)));
        assert_eq!((slot, n, ups.len()), (13, 3, 3));
    }

    #[test]
    fn gap_after_resume_breaks() {
        // Disku është në slotin 50, por validatori nis nga një snapshot i slotit 80.
        let mut t = Tracker::new();
        t.resume(50, fp(50, 7), true);
        t.handle(sh(81, 1, 80, 7)); // përmban 61..80: sloti 50 është jashtë → s'lidhet
        t.handle(status(81, Some(80), Some(1), Status::CreatedBank));
        t.handle(acc(81, 1, 3, 1, 30));
        let out = t.handle(status(81, Some(80), Some(1), Status::Rooted));
        assert!(matches!(out.first(), Some(Output::Broken(_))) || t.is_broken(), "{out:?}");
    }

    #[test]
    fn startup_chunks_are_drained() {
        let mut t = Tracker::new();
        for i in 0..10u8 {
            t.handle(Event::Snapshot { slot: 7, pubkey: key(i), meta: meta(1) });
        }
        assert!(t.take_startup_chunk(20).is_none());
        let c = t.take_startup_chunk(5).unwrap();
        assert_eq!(c.len(), 10);
        assert!(c.iter().all(|(s, _)| *s == 7));
        t.handle(Event::Snapshot { slot: 7, pubkey: key(11), meta: meta(1) });
        let out = t.handle(Event::EndOfStartup);
        assert!(matches!(&out[0], Output::Startup { slot: 7, accounts } if accounts.len() == 1));
    }

    #[test]
    fn genesis_start_without_snapshot_is_marked_incomplete() {
        let mut t = Tracker::new();
        // Si në validator: banka shkruan sysvar-in SlotHashes kur krijohet.
        t.handle(Event::SlotHashes { slot: 1, bank_id: 1, entries: vec![(0, [0; 32])] });
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
