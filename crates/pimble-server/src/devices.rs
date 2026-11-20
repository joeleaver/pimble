//! This computer's device id in each vault-linked store, and the record of
//! which other devices' state reports it has answered
//! (docs/CONVERGENCE_CONTRACT.md, wave 1): `<data dir>/pimble/devices.json`,
//! a map of store id to both, beside the replicas directory.
//!
//! Not in the store's folder (decided 2026-11-19): a store lives where its
//! person keeps it, often a Dropbox folder, and two computers syncing one
//! `.pimble` folder would share whatever it holds. A device id shared that
//! way makes the two computers one device to the reports, so each ignores
//! what the other says it holds, which is exactly the comparison reports are
//! for. One file per computer, created on first use.

use std::collections::HashMap;
use std::path::PathBuf;

use pimble_core::StoreId;
use pimble_rpc::vault::Comparing;
use tokio::sync::Mutex;
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

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct DevicesFile {
    #[serde(default)]
    stores: HashMap<String, DeviceEntry>,
}

/// `devices.json`, read once and written whenever an entry changes. Every
/// link of this server goes through the one instance, so two stores' links
/// never write over each other.
pub(crate) struct Devices {
    path: PathBuf,
    file: Mutex<Option<DevicesFile>>,
}

/// Where `devices.json` lives for a server whose replicas are in
/// `replicas_dir`: beside it, so `<data dir>/pimble/devices.json` by default,
/// and inside a test's temp directory whenever its replicas are.
pub(crate) fn devices_path_beside(replicas_dir: &std::path::Path) -> PathBuf {
    replicas_dir.parent().map(|parent| parent.join("devices.json")).unwrap_or_else(|| replicas_dir.join("devices.json"))
}

impl Devices {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path, file: Mutex::new(None) }
    }

    async fn loaded<'a>(&self, slot: &'a mut Option<DevicesFile>) -> &'a mut DevicesFile {
        if slot.is_none() {
            let file = match tokio::fs::read_to_string(&self.path).await {
                Ok(json) => serde_json::from_str(&json).unwrap_or_else(|e| {
                    warn!("{} could not be read ({}); this computer's device ids start again", self.path.display(), e);
                    DevicesFile::default()
                }),
                Err(_) => DevicesFile::default(),
            };
            *slot = Some(file);
        }
        slot.as_mut().expect("loaded above")
    }

    async fn save(&self, file: &DevicesFile) {
        let json = match serde_json::to_vec_pretty(file) {
            Ok(json) => json,
            Err(e) => {
                warn!("Could not encode {}: {}", self.path.display(), e);
                return;
            }
        };
        let path = self.path.clone();
        let written = tokio::task::spawn_blocking(move || crate::fs_util::write_atomic_0600(&path, &json)).await;
        if !matches!(written, Ok(Ok(()))) {
            warn!("Could not write {}", self.path.display());
        }
    }

    /// `store_id`'s entry, made (with a fresh device id) the first time.
    pub(crate) async fn entry(&self, store_id: StoreId) -> DeviceEntry {
        let mut slot = self.file.lock().await;
        let file = self.loaded(&mut slot).await;
        let key = store_id.to_string();
        if let Some(entry) = file.stores.get(&key).filter(|entry| !entry.device.is_empty()) {
            return entry.clone();
        }
        let entry = DeviceEntry { device: Uuid::new_v4().to_string(), answered: Comparing::default() };
        info!("Store {}: this computer reports as device {}", store_id, entry.device);
        file.stores.insert(key, entry.clone());
        self.save(file).await;
        entry
    }

    /// Record which reports this computer has answered in `store_id`.
    pub(crate) async fn set_answered(&self, store_id: StoreId, answered: &Comparing) {
        let mut slot = self.file.lock().await;
        let file = self.loaded(&mut slot).await;
        let Some(entry) = file.stores.get_mut(&store_id.to_string()) else { return };
        if &entry.answered == answered {
            return;
        }
        entry.answered = answered.clone();
        self.save(file).await;
    }
}
