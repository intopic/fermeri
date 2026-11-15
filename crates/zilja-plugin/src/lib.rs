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
//!   "roots_log": "/var/lib/zilja/roots.log" }
//! ```

use agave_geyser_plugin_interface::geyser_plugin_interface::{
    GeyserPlugin, GeyserPluginError, ReplicaAccountInfoVersions, Result as PluginResult, SlotStatus,
};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Mutex;
use zilja_node::{rpc, Event, Service, Status};
use zilja_verifier::{AccountMeta, Key};

#[derive(Deserialize, Debug)]
pub struct Config {
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default)]
    pub roots_log: Option<PathBuf>,
}

fn default_bind() -> String {
    "127.0.0.1:7799".into()
}

#[derive(Default)]
pub struct ZiljaPlugin {
    service: Option<Service>,
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
        let service = Service::start(cfg.roots_log.clone()).map_err(custom)?;
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
        true
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
