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
//!   "metrics_bind": "127.0.0.1:7798",
//!   "cors": false,
//!   "snapshot_on_resume": false }
//! ```
//!
//! RPC-ja publike duhet të qëndrojë pas një proxy (TLS, kufij kërkesash); shih `docs/OPERIMI.md`.

use agave_geyser_plugin_interface::geyser_plugin_interface::{
    GeyserPlugin, GeyserPluginError, ReplicaAccountInfoVersions, ReplicaEntryInfoVersions,
    ReplicaEntryUpdateParentInfoVersions, Result as PluginResult,
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
    /// Skedari që del nga `zilja-authorize` (identiteti + autorizimi v2 me afat, maksimumi 15 epoka).
    /// Rilexohet kur ndryshon: rinovimi s'kërkon rinisje të validatorit.
    #[serde(default)]
    pub attest_authorization: Option<PathBuf>,
    /// R5-1: revokimet e çelësave të firmosjes (`zilja-authorize --revoke`), një objekt ose listë, i
    /// çdo validatori. Shërbehen te `getZiljaRevocations`; rilexohen kur skedari ndryshon.
    #[serde(default)]
    pub attest_revocations: Option<PathBuf>,
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
    /// P-10: /health = 503 kur root-i i fundit është më i vjetër se kaq sekonda (0 = pa kontroll).
    /// Në Solana rrënjosja ndodh çdo ~0.4 s; 60 s do të thotë që zilja ose validatori kanë mbetur pas.
    #[serde(default = "default_max_root_age")]
    pub max_root_age_secs: u64,
    /// A18: mbi kaq banka të parrënjosura zilja ndalon (fail-closed) para se të shterojë kujtesën
    /// e validatorit. 0 = pa kufi.
    #[serde(default = "default_max_pending_banks")]
    pub max_pending_banks: usize,
    /// A18: mbi kaq ndryshime llogarish në pritje (~250 B secili) zilja ndalon. 0 = pa kufi.
    #[serde(default = "default_max_pending_updates")]
    pub max_pending_updates: usize,
}

fn default_max_pending_banks() -> usize {
    zilja_node::tracker::Limits::default().max_pending_banks
}

fn default_max_pending_updates() -> usize {
    zilja_node::tracker::Limits::default().max_pending_updates
}

fn default_max_root_age() -> u64 {
    60
}

/// Ndërton opsionet e firmosjes nga konfigurimi (F-10).
fn attest_options(cfg: &Config, keypair: &std::path::Path) -> Result<zilja_node::AttestOptions, String> {
    let key = zilja_node::service::read_solana_keypair(keypair).map_err(|e| format!("{}: {e}", keypair.display()))?;
    let genesis = config_genesis(cfg)?.ok_or("attest_keypair kërkon genesis_path (ose genesis_hash)")?;
    let (identity, authorization) = match &cfg.attest_authorization {
        None => (None, None),
        Some(p) => {
            let (id, a) = zilja_node::attester::read_authorization_file(p)?;
            (Some(id), Some(a))
        }
    };
    let log = match (&cfg.attest_state, &cfg.db_path, &cfg.roots_log) {
        (Some(p), _, _) => p.clone(),
        (None, Some(db), _) => db.with_extension("attest.log"),
        (None, None, Some(r)) => r.with_extension("attest.log"),
        _ => return Err("attest_keypair kërkon attest_state (ose db_path / roots_log)".into()),
    };
    Ok(zilja_node::AttestOptions {
        key,
        identity,
        authorization,
        authorization_file: cfg.attest_authorization.clone(),
        revocations_file: cfg.attest_revocations.clone(),
        genesis,
        log: Some(log),
        adopt_legacy: cfg.attest_adopt_legacy,
    })
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
        // A21: release-i mbështet vetëm "pages". Një konfigurim i vjetër me `db_path` pa `tree_engine`
        // (parazgjedhja e dikurshme ishte RocksDB) refuzohet, që motori të mos ndërrohet në heshtje.
        match self.tree_engine.as_deref() {
            Some("pages") => {}
            Some("rocksdb") if cfg!(feature = "legacy-rocksdb") => {}
            Some("rocksdb") => {
                return Err("tree_engine \"rocksdb\" s'është në këtë build (A21: release-i përdor vetëm \"pages\"); shih docs/OPERIMI.md për kalimin".into())
            }
            Some(e) => return Err(format!("tree_engine i panjohur: {e} (vetëm \"pages\")")),
            None if self.db_path.is_some() => {
                return Err("db_path pa tree_engine: release-i mbështet vetëm tree_engine = \"pages\" (parazgjedhja e vjetër RocksDB s'ekziston më; A21)".into())
            }
            None => {}
        }
        if self.tree_engine.is_some() && self.db_path.is_none() {
            return Err("tree_engine kërkon db_path".into());
        }
        if self.tree_engine.as_deref() == Some("pages") && self.block_cache_mb.is_some() {
            return Err("block_cache_mb është opsion i motorit të vjetër RocksDB; s'vlen për \"pages\" (A21)".into());
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
    /// Testet: ngjarjet që do t'i shkonin shërbimit.
    #[cfg(test)]
    sent: Mutex<Vec<Event>>,
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
        #[cfg(test)]
        self.sent.lock().unwrap().push(ev.clone());
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
            #[cfg(feature = "legacy-rocksdb")]
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
            #[cfg(not(feature = "legacy-rocksdb"))]
            Some(_) => unreachable!("validate() pranon db_path vetëm me tree_engine \"pages\""),
            None => None,
        };
        let service = Service::start_opts(Options { roots_log: cfg.roots_log.clone(), attest, tree, mid_run_load: is_reload, max_root_age_secs: Some(cfg.max_root_age_secs), limits: Some(zilja_node::tracker::Limits { max_pending_banks: cfg.max_pending_banks, max_pending_updates: cfg.max_pending_updates }) }).map_err(custom)?;
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
    // A01: Agave 4.3.2 e thërret këtë VETËM kur `entry_notifications_enabled()` është `true`
    // (geyser-plugin-manager/src/entry_notifier.rs, `notify_entry_update_parent`). Vjen nga thread-i
    // i njoftuesit të entry-ve, pa radhë të garantuar ndaj `update_bank_status`: tracker-i e lidh me
    // `cleared_bank_id`, jo me radhën.
    fn notify_entry_update_parent(&self, info: ReplicaEntryUpdateParentInfoVersions) -> PluginResult<()> {
        match info {
            ReplicaEntryUpdateParentInfoVersions::V0_0_1(u) => {
                if let Some(s) = &self.service {
                    s.metrics().update_parent_seen.fetch_add(1, Ordering::Relaxed);
                }
                self.send(Event::BankCleared { slot: u.slot, bank_id: u.cleared_bank_id })
            }
        }
        Ok(())
    }

    /// A01: entry-t vetë s'na duhen; numërohen vetëm (kostoja e marrjes së UpdateParent).
    fn notify_entry(&self, _entry: ReplicaEntryInfoVersions) -> PluginResult<()> {
        if let Some(s) = &self.service {
            s.metrics().entries_seen.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    fn transaction_notifications_enabled(&self) -> bool {
        false
    }

    /// A01: `true`, ndryshe Agave s'na dërgon kurrë UpdateParent (njoftuesi i entry-ve s'krijohet fare).
    fn entry_notifications_enabled(&self) -> bool {
        true
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
    use agave_geyser_plugin_interface::geyser_plugin_interface::ReplicaEntryUpdateParentInfo;

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

    /// A01: kopje e saktë e portës së Agave 4.3.2 (geyser-plugin-manager/src/entry_notifier.rs:
    /// `if plugin.entry_notifications_enabled() && let Err(err) = plugin.notify_entry_update_parent(..)`)
    /// dhe e kushtit që krijon vetë njoftuesin (geyser_plugin_service.rs: `entry_notifications_enabled()
    /// || block_footer_notifications_enabled()`). Banka e pastruar duhet të arrijë si `BankCleared`.
    #[test]
    fn update_parent_passes_the_agave_gate() {
        let p = ZiljaPlugin::default();
        let plugins: Vec<&dyn GeyserPlugin> = vec![&p];
        let notifier_created = plugins.iter().any(|p| p.entry_notifications_enabled() || p.block_footer_notifications_enabled());
        assert!(notifier_created, "Agave s'do ta krijonte njoftuesin e entry-ve: UpdateParent s'vjen kurrë");
        let parent_block_id = Default::default();
        let info = ReplicaEntryUpdateParentInfo { slot: 77, cleared_bank_id: 9, parent_slot: 75, parent_block_id: &parent_block_id };
        for plugin in &plugins {
            if plugin.entry_notifications_enabled() {
                plugin.notify_entry_update_parent(ReplicaEntryUpdateParentInfoVersions::V0_0_1(&info)).unwrap();
            }
        }
        let sent = p.sent.lock().unwrap();
        assert!(
            sent.iter().any(|e| matches!(e, Event::BankCleared { slot: 77, bank_id: 9 })),
            "banka e pastruar s'arriti te tracker-i: {sent:?}"
        );
    }

    #[test]
    fn tree_engine_is_validated() {
        let cfg = |extra: &str| serde_json::from_str::<Config>(&format!(r#"{{"libpath": "x"{extra}}}"#)).unwrap();
        assert!(cfg(r#", "db_path": "/tmp/z", "tree_engine": "pages""#).validate().is_ok());
        // A21: RocksDB vetëm me feature-in legacy-rocksdb; db_path pa tree_engine (parazgjedhja e
        // vjetër) refuzohet gjithmonë; opsionet e RocksDB s'pranohen me pages.
        assert_eq!(cfg(r#", "db_path": "/tmp/z", "tree_engine": "rocksdb""#).validate().is_ok(), cfg!(feature = "legacy-rocksdb"));
        assert!(cfg(r#", "db_path": "/tmp/z""#).validate().is_err(), "db_path pa tree_engine");
        assert!(cfg(r#", "db_path": "/tmp/z", "tree_engine": "pages", "block_cache_mb": 64"#).validate().is_err(), "block_cache_mb me pages");
        assert!(cfg(r#", "db_path": "/tmp/z", "tree_engine": "lmdb""#).validate().is_err());
        assert!(cfg(r#", "tree_engine": "pages""#).validate().is_err(), "pa db_path");
        assert!(cfg(r#", "db_path": "/tmp/z", "tree_engine": "pages", "sync_every_slots": 1"#).validate().is_ok());
        assert!(cfg(r#", "db_path": "/tmp/z", "tree_engine": "pages", "sync_every_slots": 0"#).validate().is_err(), "P-03");
        assert!(cfg(r#", "db_path": "/tmp/z", "tree_engine": "pages", "sync_every_slots": 4"#).validate().is_err(), "P-03");
    }
}
