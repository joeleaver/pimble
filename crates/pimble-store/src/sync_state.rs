//! One record of a store's link state on this computer:
//! `<state root>/<store id>/sync-state.json` (docs/ONE_CODEBASE_CONTRACT.md,
//! decision 7 and "Built: wave 0").
//!
//! Before it, the link's state was four files saved at different moments:
//! `link.json` (the read positions and the last sync), `vault-link.json`
//! (what the remote is known to hold, what changed while the link was down),
//! `convergence.json` (what waits parked, what was set aside) and this
//! store's entry in the server-wide `devices.json` (this computer's device id
//! in the store, the reports it has answered). They are one record now, each
//! in a section of its own, written atomically and durably (the file and its
//! directory synced) under one lock per file, so two writers in this process
//! never write over each other's sections:
//!
//! ```json
//! {
//!   "version": 1,
//!   "link":        { "last_sync": "...", "last_seq": { "node:<id>": 12 } },
//!   "device":      { "device": "<uuid>", "answered": { ... } },
//!   "vault":       { "epoch": "...", "pushed_sv": { ... }, "dirty": [ ... ], "dirty_all": false, "cursor_version": 1, "rekeying": [ ... ] },
//!   "convergence": { "parked": { ... }, "set_aside": { ... } }
//! }
//! ```
//!
//! Each section holds exactly what its old file held, so the migration
//! ([`migrate`]) copies values and interprets none of them. Who writes each:
//! `link` the store itself (`LocalStore::write_sync_config`, `last_sync`) and
//! the vault link (`last_seq`, only through documents already on disk);
//! `device`, `vault` and `convergence` the vault link (`pimble-server`).
//!
//! What every computer opening the store's folder shares (the remote, the
//! mode, the access: the folder's `sync.json`) is not here and never moves
//! (docs/STORE_FOLDERS_CONTRACT.md rule 1).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use pimble_core::StoreId;
use pimble_host::Disk;
use serde_json::{Map, Value};
use tracing::{info, warn};

/// The record's file name, in the store's state directory.
pub const FILE_NAME: &str = "sync-state.json";
/// The record's format.
pub const VERSION: u64 = 1;

/// `link.json`'s keys: `last_sync`, `last_seq`.
pub const LINK: &str = "link";
/// This store's entry of `devices.json`: `device`, `answered`.
pub const DEVICE: &str = "device";
/// `vault-link.json`'s keys.
pub const VAULT: &str = "vault";
/// `convergence.json`'s keys.
pub const CONVERGENCE: &str = "convergence";

/// The per-store files the record replaces, each with its section. Renamed
/// `<name>.migrated` once their contents are in the record, kept for a
/// release and never deleted here.
pub const MIGRATED_FILES: [(&str, &str); 3] = [("link.json", LINK), ("vault-link.json", VAULT), ("convergence.json", CONVERGENCE)];

/// The server-wide file whose entry for a store becomes its `device`
/// section, beside the state root (`<data dir>/pimble/devices.json` beside
/// `<data dir>/pimble/stores/`). It holds every store's entry, so it is read
/// at each store's migration and never renamed or written again.
pub const LEGACY_DEVICES_FILE: &str = "devices.json";

/// Where the record of the store whose state directory is `state_dir` lives.
pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE_NAME)
}

/// The legacy `devices.json` for stores kept under `state_root`.
pub fn legacy_devices_path(state_root: &Path) -> Option<PathBuf> {
    state_root.parent().map(|parent| parent.join(LEGACY_DEVICES_FILE))
}

/// One lock per record file for this process.
fn lock_for(path: &Path) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(Default::default);
    let mut locks = locks.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(locks.entry(path.to_path_buf()).or_default())
}

/// The record as it is on disk: empty when there is none. One that does not
/// parse is set aside (`sync-state.json.unreadable`) and read as empty: the
/// link starts over from nothing, which is safe (everything is read and
/// offered again), where writing sections over it would keep half of it.
async fn read_map(disk: &Disk, path: &Path) -> Map<String, Value> {
    let bytes = match disk.read(path).await {
        Ok(bytes) => bytes,
        Err(_) => return Map::new(),
    };
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(map)) => map,
        Ok(_) | Err(_) => {
            let aside = path.with_extension("json.unreadable");
            warn!("{} could not be read; set aside as {} and starting over", path.display(), aside.display());
            let _ = disk.rename(path, &aside).await;
            Map::new()
        }
    }
}

/// The whole record of `state_dir`; empty when there is none.
pub async fn read(disk: &Disk, state_dir: &Path) -> Map<String, Value> {
    let path = path(state_dir);
    let lock = lock_for(&path);
    let _held = lock.lock().await;
    read_map(disk, &path).await
}

/// One section of the record, when it has it.
pub async fn read_section(disk: &Disk, state_dir: &Path, section: &str) -> Option<Value> {
    read(disk, state_dir).await.remove(section)
}

/// Whether `state_dir` has a record.
pub async fn exists(disk: &Disk, state_dir: &Path) -> bool {
    disk.exists(path(state_dir)).await
}

/// Change the record under its lock: `change` gets the whole of it (empty
/// when there is none) and what it leaves is written, atomically and
/// durably, when it differs from what was read. Each writer changes only its
/// own sections.
pub async fn update<R>(disk: &Disk, state_dir: &Path, change: impl FnOnce(&mut Map<String, Value>) -> R) -> std::io::Result<R> {
    let path = path(state_dir);
    let lock = lock_for(&path);
    let _held = lock.lock().await;
    let mut map = read_map(disk, &path).await;
    let before = map.clone();
    let answer = change(&mut map);
    if map != before {
        map.insert("version".to_string(), Value::from(VERSION));
        disk.create_dir_all(state_dir).await?;
        let json = serde_json::to_vec_pretty(&Value::Object(map)).map_err(std::io::Error::other)?;
        disk.write_durable(&path, &json).await?;
    }
    Ok(answer)
}

/// Replace one section (`None` removes it).
pub async fn set_section(disk: &Disk, state_dir: &Path, section: &str, value: Option<Value>) -> std::io::Result<()> {
    update(disk, state_dir, |map| match value {
        Some(value) => {
            map.insert(section.to_string(), value);
        }
        None => {
            map.remove(section);
        }
    })
    .await
}

/// A JSON file's value, `None` when it is absent or does not parse.
async fn read_json(disk: &Disk, path: &Path) -> Option<Value> {
    let bytes = disk.read(path).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// A free name to rename `from` to: `<name>.migrated`, or with a number after
/// it when that is taken (an earlier migration's copy is never written over).
async fn migrated_name(disk: &Disk, from: &Path) -> PathBuf {
    let name = from.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let first = from.with_file_name(format!("{name}.migrated"));
    if !disk.exists(&first).await {
        return first;
    }
    for n in 2.. {
        let next = from.with_file_name(format!("{name}.migrated.{n}"));
        if !disk.exists(&next).await {
            return next;
        }
    }
    unreachable!("some name is free")
}

/// Fold the old per-store files of `state_dir` into the record, once
/// (docs/ONE_CODEBASE_CONTRACT.md decision 7).
///
/// 1. When there is no record yet, it is built from `link.json`,
///    `vault-link.json`, `convergence.json` and this store's entry of
///    `legacy_devices` (each value as it is, in its section) and written in
///    one atomic, durable write. Nothing to fold in writes nothing.
/// 2. Then each old file still there is renamed `<name>.migrated`.
///
/// A crash between the two leaves the record and some old files: the next
/// open finds the record and only renames them, so nothing is read twice
/// and nothing is lost. When the record exists the old files are never read:
/// the record is newer than anything they could hold. `devices.json` is
/// shared by every store and is only ever read.
pub async fn migrate(disk: &Disk, state_dir: &Path, store_id: StoreId, legacy_devices: Option<&Path>) {
    let record = path(state_dir);
    if !disk.exists(&record).await {
        let mut map = Map::new();
        let mut from = Vec::new();
        for (name, section) in MIGRATED_FILES {
            let file = state_dir.join(name);
            match read_json(disk, &file).await {
                Some(value) => {
                    map.insert(section.to_string(), value);
                    from.push(name);
                }
                None if disk.exists(&file).await => {
                    warn!("Store {}: {} does not parse; it is not carried into {} (the link starts that part over) and is kept as it was renamed", store_id, file.display(), FILE_NAME);
                }
                None => {}
            }
        }
        if let Some(devices) = legacy_devices {
            if let Some(entry) = read_json(disk, devices).await.and_then(|mut v| v.get_mut("stores").and_then(|s| s.get_mut(store_id.to_string())).map(Value::take)) {
                if entry.get("device").and_then(Value::as_str).is_some_and(|d| !d.is_empty()) {
                    map.insert(DEVICE.to_string(), entry);
                    from.push(LEGACY_DEVICES_FILE);
                }
            }
        }
        // Nothing of the old layout (a store that was never linked) writes
        // no record; a leftover that does not parse is still renamed below.
        if !from.is_empty() {
            map.insert("version".to_string(), Value::from(VERSION));
            let written = async {
                disk.create_dir_all(state_dir).await?;
                let json = serde_json::to_vec_pretty(&Value::Object(map)).map_err(std::io::Error::other)?;
                let lock = lock_for(&record);
                let _held = lock.lock().await;
                disk.write_durable(&record, &json).await
            };
            if let Err(e) = written.await {
                // The old files stay where they are and are read again at
                // the next open.
                warn!("Store {}: could not write {} ({}); the link's state stays in its old files until the next open", store_id, record.display(), e);
                return;
            }
            info!("Store {}: the link's state moved into {} (from {})", store_id, record.display(), from.join(", "));
        }
    }
    for (name, _) in MIGRATED_FILES {
        let file = state_dir.join(name);
        if !disk.exists(&file).await {
            continue;
        }
        let to = migrated_name(disk, &file).await;
        match disk.rename(&file, &to).await {
            Ok(()) => info!("Store {}: {} renamed {}", store_id, file.display(), to.display()),
            Err(e) => warn!("Store {}: could not rename {} ({}); it is left where it is and not read", store_id, file.display(), e),
        }
    }
}
