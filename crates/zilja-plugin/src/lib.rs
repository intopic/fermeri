//! # zilja-plugin
//!
//! "Shkruesi pranë plakut": një Geyser plugin që sheh çdo ndryshim llogarie dhe
//! statusi slotesh në validatorin Agave, pa ndryshuar asgjë në validator.
//!
//! Callback-et janë të lehta: llogarisin `data_hash` dhe e fusin ngjarjen në një
//! kanal. Puna e rëndë (pema, dëftesat, RPC) bëhet në `zilja-node`.
//!
//! Konfigurimi (JSON), p.sh. `zilja.json`:
//! ```json
//! { "libpath": "/rruga/libzilja_plugin.so",
//!   "bind": "127.0.0.1:7799",
//!   "roots_log": "/var/lib/zilja/roots.log",
//!   "attest_keypair": "/rruga/validator-keypair.json",
//!   "db_path": "/var/lib/zilja/db",
//!   "tree_engine": "pages",
//!   "history_slots": 32,
//!   "cache_depth": 20,
//!   "block_cache_mb": 512,
//!   "metrics_bind": "127.0.0.1:7798",
//!   "cors": false,
//!   "snapshot_on_resume": false }
//! ```
//!
//! RPC-ja publike duhet të qëndrojë pas një proxy (TLS, kufij kërkesash); shih `docs/OPERIMI.md`.

use agave_geyser_plugin_interface::geyser_plugin_interface::{
    GeyserPlugin, GeyserPluginError, ReplicaAccountInfoVersions, ReplicaEntryUpdateParentInfoVersions,
    Result as PluginResult,
    SlotStatus,
};
use std::sync::atomic::{AtomicBool, Ordering};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Mutex;
use zilja_node::{rpc, Event, Options, Service, Status};
use zilja_verifier::{AccountMeta, Key};

mod probe;

#[derive(Deserialize, Debug)]
pub struct Config {
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default)]
    pub roots_log: Option<PathBuf>,
    /// Keypair-i që firmos root-et. Rekomandohet një çelës i veçantë, i autorizuar nga
    /// identiteti me `zilja-authorize` (F-10); pa `attest_authorization`, ky duhet të jetë vetë identiteti.
    #[serde(default)]
    pub attest_keypair: Option<PathBuf>,
    /// Skedari që del nga `zilja-authorize` (identiteti + autorizimi).
    #[serde(default)]
    pub attest_authorization: Option<PathBuf>,
    /// Hash-i i genesis-it (base58) që hyn në çdo firmë. Ose `genesis_path`.
    #[serde(default)]
    pub genesis_hash: Option<String>,
    /// `genesis.bin` i ledger-it: hash-i i genesis-it = SHA256 i tij. Rekomandohet ky (e jo
    /// `genesis_hash`): skedari është i ledger-it të vetë validatorit, pra i rrjetit ku punon (P-05).
    #[serde(default)]
    pub genesis_path: Option<PathBuf>,
    /// Skedari i firmave (kundër firmosjes së dyfishtë). Parazgjedhja: pranë `db_path` ose `roots_log`.
    #[serde(default)]
    pub attest_state: Option<PathBuf>,
    /// P-05: pohim i operatorit që një log firmash pa kontekst (version i vjetër) është i këtij
    /// genesis-i dhe çelësi. Përdoret një herë; shih docs/OPERIMI.md.
    #[serde(default)]
    pub attest_adopt_legacy: bool,
    /// Pema në disk. Pa këtë, pema mbahet në RAM.
    #[serde(default)]
    pub db_path: Option<PathBuf>,
    /// Motori i diskut: `"rocksdb"` (parazgjedhja) ose `"pages"` (skedar blloqesh, pa bazë të dhënash).
    #[serde(default)]
    pub tree_engine: Option<String>,
    /// Sa slote pas jepen dëftesa (vetëm me `db_path`). Parazgjedhja: 32.
    #[serde(default)]
    pub history_slots: Option<u64>,
    /// Sa nivele të pemës mbahen në RAM (vetëm me `db_path`, kur krijohet disku).
    /// Më shumë = më shpejt dhe më shumë RAM: ~2^thellësia nyje. Parazgjedhja: 20.
    #[serde(default)]
    pub cache_depth: Option<u16>,
    /// Cache-ja e blloqeve të RocksDB në MB. Parazgjedhja: 512.
    #[serde(default)]
    pub block_cache_mb: Option<usize>,
    /// I-01: fsync i diskut çdo N slote (0 = kurrë; WAL-i mbetet). Parazgjedhja: 0.
    #[serde(default)]
    pub sync_every_slots: Option<u64>,
    /// Porti privat për `/metrics` (F-06). Parazgjedhja: 127.0.0.1:7798.
    #[serde(default = "default_metrics_bind")]
    pub metrics_bind: String,
    /// Kokat CORS në portin e RPC-së (për faqen demo në shfletues). Parazgjedhja: fikur.
    #[serde(default)]
    pub cors: bool,
    /// F-13: kur operatori pret një snapshot të ri (p.sh. e shkarkoi), njoftimet e snapshot-it
    /// mbeten të ndezura edhe kur pema në disk vazhdon. Nëse snapshot-i është më i ri se
    /// pema, zilja ndalon dhe e rindërton në nisjen e ardhshme (dy rinisje gjithsej).
    #[serde(default)]
    pub snapshot_on_resume: bool,
}

/// Ndërton opsionet e firmosjes nga konfigurimi (F-10).
fn attest_options(cfg: &Config, keypair: &std::path::Path) -> Result<zilja_node::AttestOptions, String> {
    let key = zilja_node::service::read_solana_keypair(keypair).map_err(|e| format!("{}: {e}", keypair.display()))?;
    let genesis = config_genesis(cfg)?.ok_or("attest_keypair kërkon genesis_path (ose genesis_hash)")?;
    let (identity, authorization) = match &cfg.attest_authorization {
        None => (None, None),
        Some(p) => {
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?).map_err(|e| e.to_string())?;
            let id: [u8; 32] = bs58::decode(v["identity"].as_str().ok_or("identity")?)
                .into_vec()
                .map_err(|e| e.to_string())?
                .try_into()
                .map_err(|_| "identity duhet 32 bajt")?;
            let auth: [u8; 64] = hex::decode(v["authorization"].as_str().ok_or("authorization")?)
                .map_err(|e| e.to_string())?
                .try_into()
                .map_err(|_| "authorization duhet 64 bajt")?;
            (Some(id), Some(auth))
        }
    };
    let log = match (&cfg.attest_state, &cfg.db_path, &cfg.roots_log) {
        (Some(p), _, _) => p.clone(),
        (None, Some(db), _) => db.with_extension("attest.log"),
        (None, None, Some(r)) => r.with_extension("attest.log"),
        _ => return Err("attest_keypair kërkon attest_state (ose db_path / roots_log)".into()),
    };
    Ok(zilja_node::AttestOptions { key, identity, authorization, genesis, log: Some(log), adopt_legacy: cfg.attest_adopt_legacy })
}

/// P-05: genesis-i nga konfigurimi. Kur jepen të dyja, `genesis_hash` duhet të përputhet me
/// `genesis.bin` e ledger-it (rrjeti real i validatorit).
fn config_genesis(cfg: &Config) -> Result<Option<[u8; 32]>, String> {
    let from_hash = match &cfg.genesis_hash {
        Some(h) => Some(<[u8; 32]>::try_from(bs58::decode(h).into_vec().map_err(|e| format!("genesis_hash: {e}"))?).map_err(|_| "genesis_hash duhet 32 bajt")?),
        None => None,
    };
    let from_file = match &cfg.genesis_path {
        Some(p) => Some(zilja_verifier::sha256(&std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?)),
        None => None,
    };
    match (from_hash, from_file) {
        (Some(a), Some(b)) if a != b => Err(format!(
            "genesis_hash {} nuk përputhet me genesis_path ({}): validatori punon në një rrjet tjetër",
            bs58::encode(a).into_string(),
            bs58::encode(b).into_string()
        )),
        (a, b) => Ok(b.or(a)),
    }
}

fn default_metrics_bind() -> String {
    "127.0.0.1:7798".into()
}

/// L-08: kufijtë e `cache_depth`. Nën 12 disku lexohet shumë herë për slot; mbi 24 RAM-i shkon
/// në dhjetëra GB (~2^thellësia nyje).
pub const CACHE_DEPTH_RANGE: std::ops::RangeInclusive<u16> = 12..=24;

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(cd) = self.cache_depth {
            // Vetëm testet e vogla e2e përdorin pemë të cekëta me qëllim.
            let test_override = std::env::var_os("ZILJA_ALLOW_SMALL_CACHE_DEPTH").is_some();
            if !CACHE_DEPTH_RANGE.contains(&cd) && !(test_override && cd <= 24) {
                return Err(format!("cache_depth {cd} jashtë kufijve {CACHE_DEPTH_RANGE:?}"));
            }
        }
        if self.history_slots.is_some_and(|h| h > 100_000) {
            return Err("history_slots shumë i madh (> 100000)".into());
        }
        if self.block_cache_mb.is_some_and(|m| m == 0 || m > 1 << 20) {
            return Err("block_cache_mb duhet në (0, 1048576]".into());
        }
        match self.tree_engine.as_deref() {
            None | Some("rocksdb") | Some("pages") => {}
            Some(e) => return Err(format!("tree_engine i panjohur: {e} (rocksdb | pages)")),
        }
        if self.tree_engine.is_some() && self.db_path.is_none() {
            return Err("tree_engine kërkon db_path".into());
        }
        // P-03: motori me faqe mbështet vetëm fsync në çdo slot në këtë release.
        if self.tree_engine.as_deref() == Some("pages") && self.sync_every_slots.is_some_and(|n| n != 1) {
            return Err("tree_engine \"pages\" kërkon sync_every_slots = 1 (ose lëre pa vendosur)".into());
        }
        if self.history_slots.is_some() || self.cache_depth.is_some() || self.block_cache_mb.is_some() {
            if self.db_path.is_none() {
                return Err("history_slots/cache_depth/block_cache_mb kërkojnë db_path".into());
            }
        }
        Ok(())
    }
}

fn default_bind() -> String {
    "127.0.0.1:7799".into()
}

#[derive(Default)]
pub struct ZiljaPlugin {
    service: Option<Service>,
    /// Vazhdon nga pema në disk: s'ka nevojë për llogaritë e snapshot-it.
    resuming: AtomicBool,
    snapshot_on_resume: AtomicBool,
    /// Serverët HTTP (RPC dhe metrikat); ndalen dhe priten në `on_unload` (F-07).
    servers: Mutex<Vec<rpc::HttpServer>>,
}

impl std::fmt::Debug for ZiljaPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ZiljaPlugin")
    }
}

/// Fushat që na duhen nga çdo version i strukturës së llogarisë.
pub struct AccountView<'a> {
    pub pubkey: &'a [u8],
    pub lamports: u64,
    pub owner: &'a [u8],
    pub executable: bool,
    pub data: &'a [u8],
    pub write_version: u64,
}

pub fn view<'a>(a: &'a ReplicaAccountInfoVersions<'a>) -> AccountView<'a> {
    match a {
        ReplicaAccountInfoVersions::V0_0_1(i) => AccountView {
            pubkey: i.pubkey,
            lamports: i.lamports,
            owner: i.owner,
            executable: i.executable,
            data: i.data,
            write_version: i.write_version,
        },
        ReplicaAccountInfoVersions::V0_0_2(i) => AccountView {
            pubkey: i.pubkey,
            lamports: i.lamports,
            owner: i.owner,
            executable: i.executable,
            data: i.data,
            write_version: i.write_version,
        },
        ReplicaAccountInfoVersions::V0_0_3(i) => AccountView {
            pubkey: i.pubkey,
            lamports: i.lamports,
            owner: i.owner,
            executable: i.executable,
            data: i.data,
            write_version: i.write_version,
        },
    }
}

/// Kthen llogarinë në (pubkey, meta). `lamports = 0` do të thotë llogari e fshirë.
pub fn to_meta(v: &AccountView) -> Option<(Key, Option<AccountMeta>)> {
    let pubkey: Key = v.pubkey.try_into().ok()?;
    if v.lamports == 0 {
        return Some((pubkey, None));
    }
    let owner: [u8; 32] = v.owner.try_into().ok()?;
    Some((pubkey, Some(AccountMeta::from_account(v.lamports, &owner, v.executable, v.data))))
}

/// `SysvarS1otHashes111111111111111111111111111`
pub const SLOT_HASHES_ID: [u8; 32] = [
    6, 167, 213, 23, 25, 47, 10, 175, 198, 242, 101, 227, 251, 119, 204, 122, 218, 130, 197, 41, 208, 190, 59, 19,
    110, 45, 0, 85, 32, 0, 0, 0,
];

/// Lexon sysvar-in SlotHashes (bincode: `u64 numri ‖ (u64 slot ‖ [u8;32] hash)*`).
pub fn parse_slot_hashes(data: &[u8]) -> Vec<(u64, [u8; 32])> {
    let Some(n) = data.get(..8).map(|b| u64::from_le_bytes(b.try_into().unwrap()) as usize) else {
        return vec![];
    };
    data[8..]
        .chunks_exact(40)
        .take(n.min(512))
        .map(|c| (u64::from_le_bytes(c[..8].try_into().unwrap()), c[8..40].try_into().unwrap()))
        .collect()
}

fn status(s: &SlotStatus) -> Status {
    match s {
        SlotStatus::CreatedBank => Status::CreatedBank,
        SlotStatus::Processed => Status::Processed,
        SlotStatus::Confirmed => Status::Confirmed,
        SlotStatus::Rooted => Status::Rooted,
        SlotStatus::Dead(_) => Status::Dead,
        _ => Status::Other,
    }
}

fn custom<E: std::fmt::Display>(e: E) -> GeyserPluginError {
    GeyserPluginError::Custom(e.to_string().into())
}

impl ZiljaPlugin {
    fn send(&self, ev: Event) {
        if let Some(s) = &self.service {
            s.send(ev);
        }
    }
}

impl GeyserPlugin for ZiljaPlugin {
    fn name(&self) -> &'static str {
        "zilja"
    }

    fn on_load(&mut self, config_file: &str, is_reload: bool) -> PluginResult<()> {
        probe::line(&format!("on_load reload={is_reload}"));
        let text = std::fs::read_to_string(config_file)?;
        let cfg: Config = serde_json::from_str(&text).map_err(custom)?;
        cfg.validate().map_err(custom)?;
        let attest = match &cfg.attest_keypair {
            Some(p) => Some(attest_options(&cfg, p).map_err(custom)?),
            None => None,
        };
        let tree: Option<Box<dyn zilja_tree::StateTree>> = match &cfg.db_path {
            Some(p) if cfg.tree_engine.as_deref() == Some("pages") => {
                let d = zilja_store::PageOptions::default();
                // P-05: pema lidhet me genesis-in e rrjetit (MANIFEST).
                let genesis = config_genesis(&cfg).map_err(custom)?.ok_or_else(|| custom("tree_engine \"pages\" kërkon genesis_path (ose genesis_hash)"))?;
                let o = zilja_store::PageOptions {
                    genesis: Some(genesis),
                    history: cfg.history_slots.unwrap_or(d.history),
                    cache_depth: cfg.cache_depth.unwrap_or(d.cache_depth),
                    durable: true,
                    sync_every: cfg.sync_every_slots.unwrap_or(d.sync_every),
                    ..d
                };
                Some(Box::new(zilja_store::PageTree::open(p, o).map_err(custom)?))
            }
            Some(p) => {
                let d = zilja_store::StoreOptions::default();
                let o = zilja_store::StoreOptions {
                    history: cfg.history_slots.unwrap_or(d.history),
                    cache_depth: cfg.cache_depth.unwrap_or(d.cache_depth),
                    block_cache_mb: cfg.block_cache_mb.unwrap_or(d.block_cache_mb),
                    durable: true,
                    sync_every: cfg.sync_every_slots.unwrap_or(0),
                };
                Some(Box::new(zilja_store::DiskTree::open_opts(p, o).map_err(custom)?))
            }
            None => None,
        };
        let service = Service::start_opts(Options { roots_log: cfg.roots_log.clone(), attest, tree, mid_run_load: is_reload }).map_err(custom)?;
        self.resuming.store(service.resumed, Ordering::Relaxed);
        self.snapshot_on_resume.store(cfg.snapshot_on_resume, Ordering::Relaxed);
        let main = rpc::serve_routes(&cfg.bind, service.shared.clone(), rpc::Routes { jsonrpc: true, metrics: false, cors: cfg.cors })
            .map_err(custom)?;
        let metrics = rpc::serve_routes(&cfg.metrics_bind, service.shared.clone(), rpc::Routes { jsonrpc: false, metrics: true, cors: false })
            .map_err(custom)?;
        *self.servers.lock().unwrap() = vec![main, metrics];
        self.service = Some(service);
        log::info!("zilja: u ngarkua me konfigurimin {cfg:?}");
        Ok(())
    }

    fn setup_logger(&self, logger: &'static dyn log::Log, level: log::LevelFilter) -> PluginResult<()> {
        // F-12: pa këtë, logjet e plugin-it (përfshirë arsyen e ndalimit) nuk shfaqen kurrë.
        let _ = log::set_logger(logger);
        log::set_max_level(level);
        Ok(())
    }

    fn on_unload(&mut self) {
        probe::line("on_unload");
        // F-07: ndalo serverët (dhe prit thread-et), pastaj punëtorin; asgjë nuk mbetet gjallë.
        for mut srv in self.servers.lock().unwrap().drain(..) {
            srv.stop();
        }
        if let Some(mut s) = self.service.take() {
            s.shutdown();
        }
    }

    fn update_account_from_snapshot(&self, account: ReplicaAccountInfoVersions, slot: u64) -> PluginResult<()> {
        probe::snapshot_account();
        if let Some((pubkey, meta)) = to_meta(&view(&account)) {
            self.send(Event::Snapshot { slot, pubkey, meta });
        }
        Ok(())
    }

    fn update_account_for_bank(&self, account: ReplicaAccountInfoVersions, slot: u64, bank_id: u64) -> PluginResult<()> {
        probe::account(slot, bank_id);
        let v = view(&account);
        let write_version = v.write_version;
        if v.pubkey == SLOT_HASHES_ID {
            self.send(Event::SlotHashes { slot, bank_id, entries: parse_slot_hashes(v.data) });
        }
        if let Some((pubkey, meta)) = to_meta(&v) {
            self.send(Event::Account { slot, bank_id, pubkey, write_version, meta });
        }
        Ok(())
    }

    fn notify_end_of_startup(&self) -> PluginResult<()> {
        probe::end_of_startup();
        self.send(Event::EndOfStartup);
        Ok(())
    }

    fn update_slot_status(&self, slot: u64, parent: Option<u64>, s: &SlotStatus) -> PluginResult<()> {
        probe::line(&format!("slot_status slot={slot} parent={parent:?} status={s:?}"));
        // Statuse pa bank (p.sh. Dead në këtë version të ndërfaqes).
        self.send(Event::Status { slot, parent, bank_id: None, status: status(s) });
        Ok(())
    }

    fn update_bank_status(&self, slot: u64, parent: Option<u64>, s: &SlotStatus, bank_id: u64) -> PluginResult<()> {
        probe::line(&format!("bank_status slot={slot} parent={parent:?} bank={bank_id} status={s:?}"));
        self.send(Event::Status { slot, parent, bank_id: Some(bank_id), status: status(s) });
        Ok(())
    }

    fn account_data_notifications_enabled(&self) -> bool {
        true
    }

    fn account_data_snapshot_notifications_enabled(&self) -> bool {
        // Kur pema është në disk dhe vazhdon, nisja e validatorit është shumë më e shpejtë pa to.
        !self.resuming.load(Ordering::Relaxed) || self.snapshot_on_resume.load(Ordering::Relaxed)
    }

    // L-06 (Alpenglow): një shënues UpdateParent pastron një bank; zëvendësuesja vjen me CreatedBank.
    fn notify_entry_update_parent(&self, info: ReplicaEntryUpdateParentInfoVersions) -> PluginResult<()> {
        match info {
            ReplicaEntryUpdateParentInfoVersions::V0_0_1(u) => {
                self.send(Event::BankCleared { slot: u.slot, bank_id: u.cleared_bank_id })
            }
        }
        Ok(())
    }

    fn transaction_notifications_enabled(&self) -> bool {
        false
    }

    fn entry_notifications_enabled(&self) -> bool {
        false
    }
}

/// Pika hyrëse që validatori kërkon në librarinë `.so`.
///
/// # Safety
/// Thirret nga validatori Agave, që merr pronësinë e pointer-it.
#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub unsafe extern "C" fn _create_plugin() -> *mut dyn GeyserPlugin {
    let plugin: Box<dyn GeyserPlugin> = Box::new(ZiljaPlugin::default());
    Box::into_raw(plugin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_hashes_id_and_parse() {
        // Kontroll i ID-së kundër base58.
        const B58: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
        let mut n = vec![0u8; 0];
        for c in "SysvarS1otHashes111111111111111111111111111".chars() {
            let mut carry = B58.find(c).unwrap() as u32;
            for b in n.iter_mut().rev() {
                carry += (*b as u32) * 58;
                *b = (carry & 0xff) as u8;
                carry >>= 8;
            }
            while carry > 0 {
                n.insert(0, (carry & 0xff) as u8);
                carry >>= 8;
            }
        }
        while n.len() < 32 {
            n.insert(0, 0);
        }
        assert_eq!(n, SLOT_HASHES_ID.to_vec());
        let mut d = 2u64.to_le_bytes().to_vec();
        d.extend(9u64.to_le_bytes());
        d.extend([1u8; 32]);
        d.extend(8u64.to_le_bytes());
        d.extend([2u8; 32]);
        assert_eq!(parse_slot_hashes(&d), vec![(9, [1u8; 32]), (8, [2u8; 32])]);
        assert!(parse_slot_hashes(&[1, 2]).is_empty());
    }

    #[test]
    fn zero_lamports_means_deleted() {
        let v = AccountView { pubkey: &[1u8; 32], lamports: 0, owner: &[2u8; 32], executable: false, data: b"x", write_version: 1 };
        assert_eq!(to_meta(&v), Some(([1u8; 32], None)));
        let v = AccountView { lamports: 7, ..v };
        let (_, m) = to_meta(&v).unwrap();
        assert_eq!(m.unwrap().lamports, 7);
        // Pubkey me gjatësi të gabuar: injorohet.
        let bad = AccountView { pubkey: &[1u8; 5], ..v };
        assert!(to_meta(&bad).is_none());
    }

    #[test]
    fn tree_engine_is_validated() {
        let cfg = |extra: &str| serde_json::from_str::<Config>(&format!(r#"{{"libpath": "x"{extra}}}"#)).unwrap();
        assert!(cfg(r#", "db_path": "/tmp/z", "tree_engine": "pages""#).validate().is_ok());
        assert!(cfg(r#", "db_path": "/tmp/z", "tree_engine": "rocksdb""#).validate().is_ok());
        assert!(cfg(r#", "db_path": "/tmp/z""#).validate().is_ok());
        assert!(cfg(r#", "db_path": "/tmp/z", "tree_engine": "lmdb""#).validate().is_err());
        assert!(cfg(r#", "tree_engine": "pages""#).validate().is_err(), "pa db_path");
        assert!(cfg(r#", "db_path": "/tmp/z", "tree_engine": "pages", "sync_every_slots": 1"#).validate().is_ok());
        assert!(cfg(r#", "db_path": "/tmp/z", "tree_engine": "pages", "sync_every_slots": 0"#).validate().is_err(), "P-03");
        assert!(cfg(r#", "db_path": "/tmp/z", "tree_engine": "pages", "sync_every_slots": 4"#).validate().is_err(), "P-03");
    }
}
