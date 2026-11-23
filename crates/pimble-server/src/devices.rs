//! This computer's device id in each vault-linked store, and the record of
//! which other devices' state reports it has answered
//! (docs/CONVERGENCE_CONTRACT.md, wave 1): the `device` section of the store's
//! `sync-state.json` in its state directory (`pimble_store::sync_state`,
//! docs/ONE_CODEBASE_CONTRACT.md decision 7). Until 2026-11-22 it was one
//! server-wide `devices.json` beside the replicas directory, a map of store
//! id to both; each store's entry is carried into its record when the store
//! first opens with this version, and that file is never written again.
//!
//! Not in the store's folder (decided 2026-11-19): a store lives where its
//! person keeps it, often a Dropbox folder, and two computers syncing one
//! `.pimble` folder would share whatever it holds. A device id shared that
//! way makes the two computers one device to the reports, so each ignores
//! what the other says it holds, which is exactly the comparison reports are
//! for.

use std::collections::HashMap;
use std::path::Path;

use pimble_core::StoreId;
use pimble_rpc::vault::Comparing;
use pimble_store::sync_state;
use serde_json::{Map, Value};
use tracing::{info, warn};
use uuid::Uuid;

/// One store's entry.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct DeviceEntry {
    /// This computer's device id in the store: random, never the account id.
    pub device: String,
    /// Which other devices' reports this computer has answered, at which
    /// vector and delete digest (`pimble_rpc::vault::Comparing`).
    #[serde(default, skip_serializing_if = "Comparing::is_empty")]
    pub answered: Comparing,
}

impl DeviceEntry {
    /// The entry in a record, when it has a usable one.
    /// An `answered` that does not read is forgotten (each report is answered
    /// once more), never the device id with it.
    pub(crate) fn of(record: &Map<String, Value>) -> Option<Self> {
        let section = record.get(sync_state::DEVICE)?;
        let device = section.get("device")?.as_str().filter(|d| !d.is_empty())?.to_string();
        let answered = section.get("answered").and_then(|a| serde_json::from_value(a.clone()).ok()).unwrap_or_default();
        Some(Self { device, answered })
    }

    /// Write this entry into a record.
    pub(crate) fn put(&self, record: &mut Map<String, Value>) {
        match serde_json::to_value(self) {
            Ok(value) => {
                record.insert(sync_state::DEVICE.to_string(), value);
            }
            Err(e) => warn!("Could not encode a device entry: {}", e),
        }
    }
}

/// The device id and answered reports a browser used before its server ran in
/// a worker (docs/ONE_CODEBASE_CONTRACT.md wave 4): one device id for the
/// whole browser (`pimble.vault-device` in its `localStorage`) and, per store,
/// which reports it had answered (`pimble.vault-answered.<store>`). A store's
/// entry made for the first time on such a server takes them, so the browser
/// keeps its report slot and answers nothing twice.
#[derive(Debug, Clone, Default)]
pub struct InheritedDevice {
    pub device: String,
    pub answered: HashMap<StoreId, Comparing>,
}

/// The entries, read from and written to each store's record. A store with
/// no state directory (none on a server, which always has a state root)
/// keeps its entry in memory for the life of the process.
pub(crate) struct Devices {
    /// The disk the records are on.
    disk: pimble_host::Disk,
    memory: tokio::sync::Mutex<HashMap<StoreId, DeviceEntry>>,
    /// What a first entry takes in place of a fresh id, when the host handed
    /// one over.
    inherited: Option<InheritedDevice>,
}

impl Devices {
    pub(crate) fn new(disk: pimble_host::Disk) -> Self {
        Self { disk, memory: Default::default(), inherited: None }
    }

    pub(crate) fn inheriting(mut self, inherited: Option<InheritedDevice>) -> Self {
        self.inherited = inherited.filter(|i| !i.device.trim().is_empty());
        self
    }

    /// `store_id`'s entry, made (with a fresh device id) the first time.
    pub(crate) async fn entry(&self, store_id: StoreId, state_dir: Option<&Path>) -> DeviceEntry {
        let fresh = || {
            if let Some(inherited) = &self.inherited {
                let entry = DeviceEntry { device: inherited.device.clone(), answered: inherited.answered.get(&store_id).cloned().unwrap_or_default() };
                info!("Store {}: this browser reports as device {}, as it did before its server ran in a worker", store_id, entry.device);
                return entry;
            }
            let entry = DeviceEntry { device: Uuid::new_v4().to_string(), answered: Comparing::default() };
            info!("Store {}: this computer reports as device {}", store_id, entry.device);
            entry
        };
        let Some(state_dir) = state_dir else {
            return self.memory.lock().await.entry(store_id).or_insert_with(fresh).clone();
        };
        let made = sync_state::update(&self.disk, state_dir, |record| match DeviceEntry::of(record) {
            Some(entry) => entry,
            None => {
                let entry = fresh();
                entry.put(record);
                entry
            }
        })
        .await;
        match made {
            Ok(entry) => entry,
            Err(e) => {
                warn!("Store {}: could not keep this computer's device id in {} ({}); it lasts until the server stops", store_id, state_dir.display(), e);
                self.memory.lock().await.entry(store_id).or_insert_with(fresh).clone()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A browser's old device id and answers are what a store's first entry
    /// takes; an entry already made keeps its own.
    #[tokio::test]
    async fn an_inherited_device_is_taken_by_first_entries_only() {
        let disk = pimble_host::Disk::memory();
        disk.create_dir_all("/state/a").await.unwrap();
        disk.create_dir_all("/state/b").await.unwrap();
        let (a, b) = (StoreId::new(), StoreId::new());
        let answered: Comparing = serde_json::from_value(serde_json::json!({ "answered": { "doc": { "dev-2": "AA:" } } })).unwrap();

        let before = Devices::new(disk.clone()).entry(b, Some(Path::new("/state/b"))).await;
        let devices = Devices::new(disk.clone()).inheriting(Some(InheritedDevice { device: "browser-1".into(), answered: HashMap::from([(a, answered.clone())]) }));
        let entry = devices.entry(a, Some(Path::new("/state/a"))).await;
        assert_eq!(entry.device, "browser-1");
        assert_eq!(entry.answered, answered);
        assert_eq!(devices.entry(b, Some(Path::new("/state/b"))).await.device, before.device, "a store that had an entry keeps it");
        // Written: a server without the hand-over reads the same.
        assert_eq!(Devices::new(disk).entry(a, Some(Path::new("/state/a"))).await.device, "browser-1");
    }
}
