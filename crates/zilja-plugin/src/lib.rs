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
//!   "history_slots": 32,
//!   "cache_depth": 20,
//!   "block_cache_mb": 512 }
//! ```

use agave_geyser_plugin_interface::geyser_plugin_interface::{
    GeyserPlugin, GeyserPluginError, ReplicaAccountInfoVersions, ReplicaBlockInfoVersions, Result as PluginResult,
    SlotStatus,
};
use std::sync::atomic::{AtomicBool, Ordering};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Mutex;
use zilja_node::{rpc, Event, Options, Service, Status};
use zilja_verifier::{AccountMeta, Key};

#[derive(Deserialize, Debug)]
pub struct Config {
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default)]
    pub roots_log: Option<PathBuf>,
    /// Faza 4: keypair Solana (p.sh. identiteti i validatorit) që firmos çdo root.
    #[serde(default)]
    pub attest_keypair: Option<PathBuf>,
    /// Pema në disk (RocksDB). Pa këtë, pema mbahet në RAM.
    #[serde(default)]
    pub db_path: Option<PathBuf>,
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
}

fn default_bind() -> String {
    "127.0.0.1:7799".into()
}

#[derive(Default)]
pub struct ZiljaPlugin {
    service: Option<Service>,
    /// Vazhdon nga pema në disk: s'ka nevojë për llogaritë e snapshot-it.
    resuming: AtomicBool,
    // Thread-i i RPC-së jeton sa procesi; e mbajmë që të mos fshihet.
    _rpc: Mutex<Option<std::thread::JoinHandle<()>>>,
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

    fn on_load(&mut self, config_file: &str, _is_reload: bool) -> PluginResult<()> {
        let text = std::fs::read_to_string(config_file)?;
        let cfg: Config = serde_json::from_str(&text).map_err(custom)?;
        let signer = match &cfg.attest_keypair {
            Some(p) => Some(zilja_node::service::read_solana_keypair(p).map_err(custom)?),
            None => None,
        };
        let tree: Option<Box<dyn zilja_tree::StateTree>> = match &cfg.db_path {
            Some(p) => {
                let d = zilja_store::StoreOptions::default();
                let o = zilja_store::StoreOptions {
                    history: cfg.history_slots.unwrap_or(d.history),
                    cache_depth: cfg.cache_depth.unwrap_or(d.cache_depth),
                    block_cache_mb: cfg.block_cache_mb.unwrap_or(d.block_cache_mb),
                    durable: true,
                };
                Some(Box::new(zilja_store::DiskTree::open_opts(p, o).map_err(custom)?))
            }
            None => None,
        };
        let service = Service::start_opts(Options { roots_log: cfg.roots_log.clone(), signer, tree }).map_err(custom)?;
        self.resuming.store(service.resumed, Ordering::Relaxed);
        let handle = rpc::serve(&cfg.bind, service.shared.clone()).map_err(custom)?;
        *self._rpc.lock().unwrap() = Some(handle);
        self.service = Some(service);
        log::info!("zilja: u ngarkua me konfigurimin {cfg:?}");
        Ok(())
    }

    fn on_unload(&mut self) {
        if let Some(mut s) = self.service.take() {
            s.shutdown();
        }
    }

    fn update_account_from_snapshot(&self, account: ReplicaAccountInfoVersions, slot: u64) -> PluginResult<()> {
        if let Some((pubkey, meta)) = to_meta(&view(&account)) {
            self.send(Event::Snapshot { slot, pubkey, meta });
        }
        Ok(())
    }

    fn update_account_for_bank(&self, account: ReplicaAccountInfoVersions, slot: u64, bank_id: u64) -> PluginResult<()> {
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
        self.send(Event::EndOfStartup);
        Ok(())
    }

    fn update_slot_status(&self, slot: u64, parent: Option<u64>, s: &SlotStatus) -> PluginResult<()> {
        // Statuse pa bank (p.sh. Dead në këtë version të ndërfaqes).
        self.send(Event::Status { slot, parent, bank_id: None, status: status(s) });
        Ok(())
    }

    fn update_bank_status(&self, slot: u64, parent: Option<u64>, s: &SlotStatus, bank_id: u64) -> PluginResult<()> {
        self.send(Event::Status { slot, parent, bank_id: Some(bank_id), status: status(s) });
        Ok(())
    }

    fn account_data_notifications_enabled(&self) -> bool {
        true
    }

    fn account_data_snapshot_notifications_enabled(&self) -> bool {
        // Kur pema është në disk dhe vazhdon, nisja e validatorit është shumë më e shpejtë pa to.
        !self.resuming.load(Ordering::Relaxed)
    }

    // Validatori mund të thërrasë këtë drejtpërdrejt (metoda e vjetër) ose përmes
    // `notify_block_metadata_for_bank`, që sipas parazgjedhjes e thërret këtë.
    #[allow(deprecated)]
    fn notify_block_metadata(&self, info: ReplicaBlockInfoVersions) -> PluginResult<()> {
        let ev = match info {
            ReplicaBlockInfoVersions::V0_0_1(b) => {
                Event::Block { slot: b.slot, parent_slot: None, blockhash: b.blockhash.to_string(), parent_blockhash: None }
            }
            ReplicaBlockInfoVersions::V0_0_2(b) => Event::Block {
                slot: b.slot,
                parent_slot: Some(b.parent_slot),
                blockhash: b.blockhash.to_string(),
                parent_blockhash: Some(b.parent_blockhash.to_string()),
            },
            ReplicaBlockInfoVersions::V0_0_3(b) => Event::Block {
                slot: b.slot,
                parent_slot: Some(b.parent_slot),
                blockhash: b.blockhash.to_string(),
                parent_blockhash: Some(b.parent_blockhash.to_string()),
            },
            ReplicaBlockInfoVersions::V0_0_4(b) => Event::Block {
                slot: b.slot,
                parent_slot: Some(b.parent_slot),
                blockhash: b.blockhash.to_string(),
                parent_blockhash: Some(b.parent_blockhash.to_string()),
            },
        };
        self.send(ev);
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
}
