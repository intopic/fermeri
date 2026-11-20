//! Sondë e përkohshme (Faza 0 e auditimit): kur `ZILJA_PROBE=/rruga/skedari` është
//! vendosur, shënon në skedar çfarë thërret validatori dhe në çfarë radhe. Përdoret
//! nga e2e për t'iu përgjigjur pyetjeve të hapura të auditimit. Pa variablin, nuk bën asgjë.

use std::collections::HashSet;
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

struct Probe {
    file: std::fs::File,
    seen_banks: HashSet<u64>,
    snapshot_accounts: u64,
}

fn probe() -> Option<&'static Mutex<Probe>> {
    static P: OnceLock<Option<Mutex<Probe>>> = OnceLock::new();
    P.get_or_init(|| {
        let path = std::env::var_os("ZILJA_PROBE")?;
        let file = std::fs::OpenOptions::new().create(true).append(true).open(path).ok()?;
        Some(Mutex::new(Probe { file, seen_banks: HashSet::new(), snapshot_accounts: 0 }))
    })
    .as_ref()
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

/// L3: sonda është e ndezur? Thirrësit e përdorin para `format!`, që pa `ZILJA_PROBE` rruga e
/// nxehtë (`update_slot_status`, `update_bank_status`) të mos bëjë asnjë alokim.
#[inline]
pub fn enabled() -> bool {
    probe().is_some()
}

pub fn line(s: &str) {
    if let Some(p) = probe() {
        let mut p = p.lock().unwrap();
        let _ = writeln!(p.file, "{} {s}", now_ms());
    }
}

/// Shënon vetëm herën e parë që shihet një bank me shkrime llogarish.
pub fn account(slot: u64, bank_id: u64) {
    if let Some(p) = probe() {
        let mut p = p.lock().unwrap();
        if p.seen_banks.insert(bank_id) {
            let _ = writeln!(p.file, "{} account slot={slot} bank={bank_id}", now_ms());
        }
    }
}

pub fn snapshot_account() {
    if let Some(p) = probe() {
        p.lock().unwrap().snapshot_accounts += 1;
    }
}

pub fn end_of_startup() {
    if let Some(p) = probe() {
        let mut p = p.lock().unwrap();
        let n = p.snapshot_accounts;
        let _ = writeln!(p.file, "{} end_of_startup snapshot_accounts={n}", now_ms());
    }
}
