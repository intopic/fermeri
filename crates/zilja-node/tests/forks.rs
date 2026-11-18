//! P-06, P-07: lidhja e saktë me bankën, sipas kontratës së Agave 4.3.2.
//!
//! Kontrata (agave-geyser-plugin-interface 4.3.2): `update_bank_status` jep `bank_id` dhe
//! **slotin** e prindit, jo bankën prind. Dy banka mund të kenë të njëjtin slot: Alpenglow
//! UpdateParent (banka e vjetër "pastrohet" me `notify_entry_update_parent`; zëvendësuesja vjen me
//! `CreatedBank`) ose bllok i dyfishtë. Lidhja e saktë me bankën prind vjen vetëm nga SlotHashes
//! i fëmijës: `(slot_prind, hash_bankës_prind)`, dhe më tej paraardhësit.
//!
//! Këto teste janë sintetike (ndërtojnë ngjarjet me dorë). Rendi i ngjarjeve ndjek kontratën; dy
//! banka të gjalla në një slot s'mund të prodhohen me një validator të vetëm në e2e.

use zilja_node::tracker::{Event, Output, Status, Tracker};
use zilja_verifier::AccountMeta;

type H = [u8; 32];
fn h(s: u64, fork: u8) -> H {
    let mut x = [fork; 32];
    x[..8].copy_from_slice(&s.to_le_bytes());
    x
}
fn sh(slot: u64, bank_id: u64, entries: Vec<(u64, H)>) -> Event {
    Event::SlotHashes { slot, bank_id, entries }
}
fn created(slot: u64, parent: u64, bank_id: u64) -> Event {
    Event::Status { slot, parent: Some(parent), bank_id: Some(bank_id), status: Status::CreatedBank }
}
fn rooted(slot: u64, parent: u64, bank_id: u64) -> Event {
    Event::Status { slot, parent: Some(parent), bank_id: Some(bank_id), status: Status::Rooted }
}
fn acc(slot: u64, bank_id: u64, k: u8, l: u64) -> Event {
    Event::Account { slot, bank_id, pubkey: [k; 32], write_version: l, meta: Some(AccountMeta { lamports: l, ..Default::default() }) }
}
fn from_snapshot(slot: u64) -> Tracker {
    let mut t = Tracker::new();
    t.handle(Event::Snapshot { slot, pubkey: [1; 32], meta: Some(AccountMeta { lamports: 1, ..Default::default() }) });
    t.handle(Event::EndOfStartup);
    t
}
fn lamports(out: &[Output]) -> Option<Vec<(u8, u64)>> {
    out.iter().find_map(|o| match o {
        Output::Rooted { updates, .. } => {
            let mut v: Vec<(u8, u64)> = updates.iter().map(|(k, m)| (k[0], m.as_ref().map_or(0, |m| m.lamports))).collect();
            v.sort();
            Some(v)
        }
        _ => None,
    })
}
fn broken(out: &[Output]) -> bool {
    out.iter().any(|o| matches!(o, Output::Broken(_)))
}

/// P-06: gjurma e slotit të rrënjosur duhet të jetë e bankës së tij, jo e prindit. Ndryshe pas
/// rinisjes një bankë tjetër në të njëjtin slot (me të njëjtin prind) pranohet si "e jona".
#[test]
fn fingerprint_is_the_exact_bank_not_its_parent() {
    let mut t = from_snapshot(49);
    t.handle(sh(50, 10, vec![(49, h(49, 0))]));
    t.handle(created(50, 49, 10));
    t.handle(acc(50, 10, 2, 100));
    let out = t.handle(rooted(50, 49, 10));
    let fp = out.iter().find_map(|o| match o {
        Output::Rooted { fingerprint, .. } => Some(*fingerprint),
        _ => None,
    });
    let fp = fp.expect("sloti 50 duhej të rrënjosej");
    assert!(fp.is_none_or(|(s, _)| s == 50), "gjurma e slotit 50 u mor nga prindi: {fp:?}");

    // Rinisja: një bankë e re që rrjedh nga një bankë TJETËR në slotin 50 (prind i njëjtë 49).
    let mut r = Tracker::new();
    r.resume(50, fp, true);
    r.handle(sh(51, 21, vec![(50, h(50, 9)), (49, h(49, 0))]));
    r.handle(created(51, 50, 21));
    let out = r.handle(rooted(51, 50, 21));
    assert!(broken(&out), "u pranua një bankë tjetër në slotin 50 vetëm sepse prindi përputhej: {out:?}");
}

/// P-07: dy banka të gjalla në slotin 50 me të njëjtin prind: s'ka mënyrë të sigurt për të ditur
/// cilën ndoqi sloti i rrënjosur. Duhet ndalim, jo zgjedhje sipas ID-së më të madhe.
#[test]
fn indistinguishable_banks_halt_instead_of_guessing() {
    let mut t = from_snapshot(49);
    for (id, l) in [(10u64, 100u64), (11, 200)] {
        t.handle(sh(50, id, vec![(49, h(49, 0))]));
        t.handle(created(50, 49, id));
        t.handle(acc(50, id, 2, l));
    }
    t.handle(sh(51, 12, vec![(50, h(50, 1)), (49, h(49, 0))]));
    t.handle(created(51, 50, 12));
    let out = t.handle(rooted(51, 50, 12));
    assert!(broken(&out), "u zgjodh një bankë me hamendje: {:?}", lamports(&out));
}

/// P-07: dy banka në slotin 50 me prindër të ndryshëm (UpdateParent pa njoftim pastrimi): SlotHashes
/// i fëmijës e zgjidh saktë, dhe përditësimet merren vetëm nga zinxhiri i vërtetë.
#[test]
fn reparented_banks_are_resolved_from_slot_hashes() {
    let mut t = from_snapshot(47);
    let anc = |top: u64| (47..=top).rev().map(|s| (s, h(s, 0))).collect::<Vec<_>>();
    t.handle(sh(48, 5, anc(47)));
    t.handle(created(48, 47, 5));
    t.handle(acc(48, 5, 3, 48));
    t.handle(sh(49, 6, anc(48)));
    t.handle(created(49, 48, 6));
    t.handle(acc(49, 6, 4, 49));
    // A: 50 mbi 49 (zinxhiri i vërtetë). B: 50 mbi 48 (ri-prindëruar), id më i madh.
    t.handle(sh(50, 10, anc(49)));
    t.handle(created(50, 49, 10));
    t.handle(acc(50, 10, 2, 100));
    t.handle(sh(50, 11, anc(48)));
    t.handle(created(50, 48, 11));
    t.handle(acc(50, 11, 2, 200));
    t.handle(acc(50, 11, 9, 999));
    // X: 51 mbi A.
    let mut x = vec![(50, h(50, 0))];
    x.extend(anc(49));
    t.handle(sh(51, 12, x));
    t.handle(created(51, 50, 12));
    t.handle(acc(51, 12, 5, 51));
    let out = t.handle(rooted(51, 50, 12));
    assert_eq!(lamports(&out), Some(vec![(2, 100), (3, 48), (4, 49), (5, 51)]), "{out:?}");
}

/// P-06: lidhja e fundit e zinxhirit duhet të jetë me bankën e rrënjosur (hash), jo vetëm me slotin.
#[test]
fn bottom_link_must_match_the_rooted_bank_hash() {
    let mut t = Tracker::new();
    t.resume(50, Some((50, h(50, 1))), true);
    t.handle(sh(51, 2, vec![(50, h(50, 1)), (49, h(49, 0))]));
    assert!(t.resume_verified());
    // Y: bankë në 51, fëmijë i një banke TJETËR në 50.
    t.handle(sh(51, 3, vec![(50, h(50, 9)), (49, h(49, 0))]));
    t.handle(created(51, 50, 3));
    t.handle(acc(51, 3, 7, 70));
    let out = t.handle(rooted(51, 50, 3));
    assert!(broken(&out), "u pranua një bankë që s'rrjedh nga banka e rrënjosur: {:?}", lamports(&out));
}

/// Nisje pa bazë (genesis ose ringarkim në RAM): një bankë e krijuar para ngarkimit (pa SlotHashes,
/// ndryshimet e saj i mori instanca tjetër) s'aplikohet me ndryshime që mungojnë; bëhet baza.
#[test]
fn partially_observed_banks_become_the_base_without_a_snapshot() {
    let mut t = Tracker::new();
    // Bankat 10 dhe 11 u krijuan para ngarkimit: vetëm statuse dhe një pjesë e llogarive.
    t.handle(acc(10, 10, 2, 999));
    let out = t.handle(rooted(10, 9, 10));
    assert!(!broken(&out) && lamports(&out).is_none(), "banka e pjesshme u aplikua: {out:?}");
    t.handle(acc(11, 11, 3, 888));
    // Banka 12: e vëzhguar plotësisht.
    t.handle(sh(12, 12, vec![(11, h(11, 0)), (10, h(10, 0))]));
    t.handle(created(12, 11, 12));
    t.handle(acc(12, 12, 4, 12));
    let out = t.handle(rooted(12, 11, 12));
    assert_eq!(lamports(&out), Some(vec![(4, 12)]), "vetëm banka e vëzhguar plotësisht: {out:?}");
}
