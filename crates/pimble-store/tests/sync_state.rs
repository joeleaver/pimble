//! The link's state as one record (docs/ONE_CODEBASE_CONTRACT.md decision 7,
//! "Built: wave 0"): `link.json`, `vault-link.json`, `convergence.json` and the
//! store's entry of `devices.json` become `sync-state.json` in the store's
//! state directory, once, when the store opens; the old per-store files are
//! renamed `*.migrated` and `devices.json` is only read.

use std::path::{Path, PathBuf};

use pimble_core::{StoreAccess, StoreId};
use pimble_store::{sync_state, LocalStore, SyncConfig, SyncMode};
use serde_json::{json, Value};
use pimble_host::disk::testfs as tfs;
use pimble_host::Disk;
use tempfile::TempDir;

/// A computer's layout: the data directory's `stores/` (the state root) and
/// `devices.json` beside it, and a store folder.
struct Layout {
    _dir: TempDir,
    store_path: PathBuf,
    state_root: PathBuf,
    state_dir: PathBuf,
    devices: PathBuf,
    store_id: StoreId,
}

async fn layout() -> Layout {
    let dir = TempDir::new().unwrap();
    let store_path = dir.path().join("s.pimble");
    let store = LocalStore::create(&store_path, "Linked").await.unwrap();
    let store_id = store.id;
    drop(store);
    let data = dir.path().join("pimble");
    let state_root = data.join("stores");
    let state_dir = state_root.join(store_id.to_string());
    tfs::create_dir_all(&state_dir).unwrap();
    Layout { store_path, state_dir, devices: data.join("devices.json"), state_root, store_id, _dir: dir }
}

/// The shared part of a link, in the folder's `sync.json`.
fn write_folder_link(l: &Layout, mode: SyncMode) {
    let config = SyncConfig {
        remote: pimble_core::RemoteEndpoint { url: "https://pimble.example/rpc".parse().unwrap(), auth: pimble_core::AuthMethod::None },
        last_sync: None,
        mode,
        via_relay: false,
        last_seq: Default::default(),
        vault_key_id: mode.is_vault_link().then(uuid::Uuid::new_v4),
        access: StoreAccess::Full,
        shared_by: None,
        read_only_roots: Vec::new(),
        comment_roots: Vec::new(),
    };
    tfs::write(l.store_path.join("sync.json"), serde_json::to_vec_pretty(&config).unwrap()).unwrap();
}

fn write(path: &Path, value: &Value) {
    tfs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(&tfs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))).unwrap()
}

fn link_json() -> Value {
    json!({ "last_sync": "2026-11-21T09:30:00Z", "last_seq": { "node:3f1c0a52-7d0e-4b1e-9c55-8a1b2c3d4e5f": 41, "node:0a0b0c0d-0e0f-4011-8213-141516171819": 7 } })
}

fn vault_link_json() -> Value {
    json!({
        "epoch": "e-17",
        "pushed_sv": { "node:3f1c0a52-7d0e-4b1e-9c55-8a1b2c3d4e5f": "AQKYvtTqDA0=" },
        "dirty": ["node:0a0b0c0d-0e0f-4011-8213-141516171819"],
        "dirty_all": false,
        "cursor_version": 1,
        "rekeying": ["node:0a0b0c0d-0e0f-4011-8213-141516171819"]
    })
}

fn convergence_json() -> Value {
    json!({
        "parked": { "node:3f1c0a52-7d0e-4b1e-9c55-8a1b2c3d4e5f": { "missing": "AQGqAQ==", "since": "2026-11-21T08:00:00Z" } },
        "set_aside": {}
    })
}

fn device_entry() -> Value {
    json!({ "device": "7d4f2a10-5b8e-4c3d-9e1f-a2b3c4d5e6f7", "answered": { "node:3f1c0a52-7d0e-4b1e-9c55-8a1b2c3d4e5f": { "other-device": { "state_vector": "AQ==", "deletes": "AA==" } } } })
}

/// The four files of a vault (or relay) link as a computer before the record
/// kept them, `devices.json` holding another store's entry too.
fn write_old_vault_files(l: &Layout, mode: SyncMode) {
    write_folder_link(l, mode);
    write(&l.state_dir.join("link.json"), &link_json());
    write(&l.state_dir.join("vault-link.json"), &vault_link_json());
    write(&l.state_dir.join("convergence.json"), &convergence_json());
    write(&l.devices, &json!({ "stores": { l.store_id.to_string(): device_entry(), "00000000-0000-4000-8000-000000000001": { "device": "someone-else" } } }));
}

fn record(l: &Layout) -> Value {
    read(&l.state_dir.join(sync_state::FILE_NAME))
}

/// The record a vault or relay link's four files make.
fn assert_vault_record(l: &Layout) {
    let record = record(l);
    assert_eq!(record["version"], 1, "{record}");
    assert_eq!(record["link"], link_json(), "link.json, as it was");
    assert_eq!(record["vault"], vault_link_json(), "vault-link.json, as it was");
    assert_eq!(record["convergence"], convergence_json(), "convergence.json, as it was");
    assert_eq!(record["device"], device_entry(), "this store's entry of devices.json, as it was");
    for (name, section) in [("link.json", link_json()), ("vault-link.json", vault_link_json()), ("convergence.json", convergence_json())] {
        assert!(!tfs::exists(&l.state_dir.join(name)), "{name} is renamed");
        assert_eq!(read(&l.state_dir.join(format!("{name}.migrated"))), section, "{name}.migrated keeps what it held");
    }
}

#[tokio::test]
async fn a_vault_links_four_files_become_one_record() {
    let l = layout().await;
    write_old_vault_files(&l, SyncMode::Vault);
    let devices_before = tfs::read(&l.devices).unwrap();
    let folder_before = tfs::read(l.store_path.join("sync.json")).unwrap();

    let store = LocalStore::open_in(&l.store_path, Some(&l.state_root)).await.unwrap();
    assert_vault_record(&l);
    assert_eq!(tfs::read(&l.devices).unwrap(), devices_before, "devices.json holds every store's entry: read, never written");
    assert_eq!(tfs::read(l.store_path.join("sync.json")).unwrap(), folder_before, "what the folder shares stays where it is");

    // The store reads its link through the record.
    let config = store.read_sync_config().await.unwrap().unwrap();
    assert_eq!(config.mode, SyncMode::Vault);
    assert_eq!(config.last_seq.get("node:3f1c0a52-7d0e-4b1e-9c55-8a1b2c3d4e5f"), Some(&41));
    assert_eq!(config.last_sync.map(|t| t.to_rfc3339()), Some("2026-11-21T09:30:00+00:00".to_string()));

    // A write of the link changes its own section and keeps the others.
    let mut config = config;
    config.last_sync = Some("2026-11-22T10:00:00Z".parse().unwrap());
    store.write_sync_config(&config).await.unwrap();
    let record = record(&l);
    assert_eq!(record["link"]["last_sync"], "2026-11-22T10:00:00Z");
    assert_eq!(record["vault"], vault_link_json());
    assert_eq!(record["device"], device_entry());
    assert!(!tfs::exists(&l.state_dir.join("link.json")), "nothing writes the old files");
}

#[tokio::test]
async fn a_relay_link_migrates_the_same_way() {
    let l = layout().await;
    write_old_vault_files(&l, SyncMode::Relay);
    let store = LocalStore::open_in(&l.store_path, Some(&l.state_root)).await.unwrap();
    assert_vault_record(&l);
    assert_eq!(store.read_sync_config().await.unwrap().unwrap().mode, SyncMode::Relay);
}

#[tokio::test]
async fn a_plain_link_keeps_its_last_sync() {
    let l = layout().await;
    write_folder_link(&l, SyncMode::Sync);
    write(&l.state_dir.join("link.json"), &json!({ "last_sync": "2026-11-21T09:30:00Z" }));
    let store = LocalStore::open_in(&l.store_path, Some(&l.state_root)).await.unwrap();
    let record = record(&l);
    assert_eq!(record, json!({ "version": 1, "link": { "last_sync": "2026-11-21T09:30:00Z" } }), "a plain link holds no belief about its remote");
    assert!(tfs::exists(&l.state_dir.join("link.json.migrated")));
    let config = store.read_sync_config().await.unwrap().unwrap();
    assert_eq!(config.mode, SyncMode::Sync);
    assert!(config.last_sync.is_some() && config.last_seq.is_empty());
}

#[tokio::test]
async fn a_store_with_no_link_gets_no_record() {
    let l = layout().await;
    let store = LocalStore::open_in(&l.store_path, Some(&l.state_root)).await.unwrap();
    assert!(store.read_sync_config().await.unwrap().is_none());
    let left: Vec<_> = tfs::names(&l.state_dir).unwrap();
    assert!(left.is_empty(), "nothing written for a store that was never linked: {left:?}");

    // Linking it later writes the record, not the old files.
    let mut config: SyncConfig = serde_json::from_value(json!({ "remote": { "url": "https://pimble.example/rpc", "auth": { "method": "none" } } })).unwrap();
    config.last_sync = Some("2026-11-22T10:00:00Z".parse().unwrap());
    store.write_sync_config(&config).await.unwrap();
    assert_eq!(record(&l)["link"]["last_sync"], "2026-11-22T10:00:00Z");
    assert!(!tfs::exists(&l.state_dir.join("link.json")));
}

#[tokio::test]
async fn the_migration_runs_once() {
    let l = layout().await;
    write_old_vault_files(&l, SyncMode::Vault);
    drop(LocalStore::open_in(&l.store_path, Some(&l.state_root)).await.unwrap());
    let first = tfs::read(l.state_dir.join(sync_state::FILE_NAME)).unwrap();
    drop(LocalStore::open_in(&l.store_path, Some(&l.state_root)).await.unwrap());
    assert_eq!(tfs::read(l.state_dir.join(sync_state::FILE_NAME)).unwrap(), first, "a second open changes nothing");
    assert_vault_record(&l);
    assert!(!tfs::exists(&l.state_dir.join("link.json.migrated.2")));

    // An older Pimble run in between writes its file again: the record is
    // newer than anything it holds and stays, and the file is renamed beside
    // the first one, which is never written over.
    write(&l.state_dir.join("vault-link.json"), &json!({ "pushed_sv": {} }));
    drop(LocalStore::open_in(&l.store_path, Some(&l.state_root)).await.unwrap());
    assert_eq!(record(&l)["vault"], vault_link_json());
    assert_eq!(read(&l.state_dir.join("vault-link.json.migrated")), vault_link_json());
    assert_eq!(read(&l.state_dir.join("vault-link.json.migrated.2")), json!({ "pushed_sv": {} }));
}

/// A crash before the record is in place (a temporary half written) leaves
/// the old files as they were, and the next open migrates from them.
#[tokio::test]
async fn a_crash_before_the_record_is_written_loses_nothing() {
    let l = layout().await;
    write_old_vault_files(&l, SyncMode::Vault);
    tfs::write(l.state_dir.join("sync-state.3f9a0c1d2e4b.tmp"), b"{\"version\":1,\"li").unwrap();
    drop(LocalStore::open_in(&l.store_path, Some(&l.state_root)).await.unwrap());
    assert_vault_record(&l);
}

/// A crash after the record is written and before every old file is renamed:
/// the next open finds the record and only renames what is left, reading
/// none of it.
#[tokio::test]
async fn a_crash_between_the_record_and_the_renames_loses_nothing() {
    let l = layout().await;
    write_old_vault_files(&l, SyncMode::Vault);
    // What the migration writes, and what a crash right after it leaves:
    // the record, `link.json` renamed, the other two not yet.
    sync_state::migrate(&Disk::default(), &l.state_dir, l.store_id, Some(&l.devices)).await;
    tfs::rename(l.state_dir.join("vault-link.json.migrated"), l.state_dir.join("vault-link.json")).unwrap();
    tfs::rename(l.state_dir.join("convergence.json.migrated"), l.state_dir.join("convergence.json")).unwrap();
    let written = tfs::read(l.state_dir.join(sync_state::FILE_NAME)).unwrap();

    drop(LocalStore::open_in(&l.store_path, Some(&l.state_root)).await.unwrap());
    assert_eq!(tfs::read(l.state_dir.join(sync_state::FILE_NAME)).unwrap(), written, "the record is not rebuilt");
    assert_vault_record(&l);
}

/// An old file that does not parse is not carried into the record (the link
/// starts that part over, as it did when it found it unreadable) and is kept,
/// renamed, as it was.
#[tokio::test]
async fn an_unreadable_old_file_is_kept_and_not_carried() {
    let l = layout().await;
    write_old_vault_files(&l, SyncMode::Vault);
    tfs::write(l.state_dir.join("convergence.json"), b"{ not json").unwrap();
    drop(LocalStore::open_in(&l.store_path, Some(&l.state_root)).await.unwrap());
    let record = record(&l);
    assert!(record.get("convergence").is_none(), "{record}");
    assert_eq!(record["vault"], vault_link_json());
    assert_eq!(tfs::read(l.state_dir.join("convergence.json.migrated")).unwrap(), b"{ not json");
}

/// What the folder of an older Pimble holds of this computer's link goes
/// through the record too, in one open.
#[tokio::test]
async fn state_from_an_older_folder_reaches_the_record_in_one_open() {
    let l = layout().await;
    let mut folder = serde_json::to_value({
        let mut config: SyncConfig = serde_json::from_value(json!({ "remote": { "url": "https://pimble.example/rpc", "auth": { "method": "none" } }, "mode": "vault" })).unwrap();
        config.last_seq.insert("node:x".to_string(), 5);
        config
    })
    .unwrap();
    folder["last_sync"] = json!("2026-11-18T10:00:00Z");
    write(&l.store_path.join("sync.json"), &folder);
    write(&l.store_path.join("vault-link.json"), &vault_link_json());
    drop(LocalStore::open_in(&l.store_path, Some(&l.state_root)).await.unwrap());
    let record = record(&l);
    assert_eq!(record["link"], json!({ "last_sync": "2026-11-18T10:00:00Z", "last_seq": { "node:x": 5 } }));
    assert_eq!(record["vault"], vault_link_json());
    assert!(!tfs::exists(&l.store_path.join("vault-link.json")));
    let shared = read(&l.store_path.join("sync.json"));
    assert!(shared.get("last_seq").is_none() && shared.get("last_sync").is_none());
}
