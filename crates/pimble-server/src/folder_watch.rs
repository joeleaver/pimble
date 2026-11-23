//! One store folder on several computers (docs/STORE_FOLDERS_CONTRACT.md):
//! the server's half.
//!
//! For every plain store open here, one task watches its folder
//! (`pimble_store::watch_folder`) and, a moment after a burst of changes
//! rests, has the store merge what changed (`LocalStore::absorb_folder_paths`:
//! documents, conflict copies, the manifest). Whatever that merge, or a
//! flush's merge before writing, changed in a held document goes out exactly
//! as a peer's update does: the notifications derived from what changed, the
//! bytes relayed to every window and every link, the index fed, the tree
//! repaired after a structural change. It is attributed to `folder:<store
//! id>`, which is no link's own id, so every link forwards it.
//!
//! The same task keeps `computers/<presence id>.json` in the folder up to date
//! (rule 5) and reads the other computers' files, which `Store::also_open_on`
//! carries to the app.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use chrono::{DateTime, Utc};
use pimble_core::{OtherComputer, StoreId};
use pimble_crdt::derive_changes;
use pimble_rpc::StoreChangeKind;
use serde::{Deserialize, Serialize};
use pimble_host::Instant;
use tracing::{debug, warn};

use crate::handler::{index_events_for, RpcHandler};

/// How long a folder's changes must rest before they are merged: sync tools
/// write in bursts (a file, its temporary, the rename, the next file).
const SETTLE: Duration = Duration::from_millis(500);
/// The longest a steady stream of changes defers the merge.
const SETTLE_AT_MOST: Duration = Duration::from_secs(2);
/// How often merges a flush made (`LocalStore::flush` merging before it
/// writes) are announced.
const ANNOUNCE_EVERY: Duration = Duration::from_secs(1);
/// How often this computer rewrites its `computers/` file while the store is
/// open, and reads the others'.
const PRESENCE_WRITE_EVERY: Duration = Duration::from_secs(180);
const PRESENCE_READ_EVERY: Duration = Duration::from_secs(60);
/// A computer not seen for this long is no longer named.
const PRESENCE_SHOWN_FOR: chrono::Duration = chrono::Duration::days(14);

/// The client id a merge from the folder carries.
pub(crate) fn folder_source(store_id: StoreId) -> String {
    format!("folder:{store_id}")
}

/// The name a computer goes by in a folder's `computers/`: derived from its
/// device id in the store (`crate::devices`), never the id itself, which the
/// convergence reports use and which stays out of the store's folder
/// (docs/CONVERGENCE_CONTRACT.md, 2026-11-19). The same device id gives the
/// same name every time, and no computer can take it for its own.
fn presence_id(device: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("pimble-computer:{device}").as_bytes());
    digest.iter().take(10).map(|b| format!("{b:02x}")).collect()
}

/// One computer's `computers/<presence id>.json`.
#[derive(Debug, Serialize, Deserialize)]
struct ComputerFile {
    device: String,
    name: String,
    last_seen: DateTime<Utc>,
    version: String,
}

/// A running folder task; dropping it stops the task (and the watch it holds).
pub(crate) struct FolderTask(pimble_host::Task);

impl Drop for FolderTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// This computer's name as people know it: the host name, or a plain phrase
/// when none can be read.
fn computer_name() -> String {
    let from_file = |path: &str| std::fs::read_to_string(path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| from_file("/etc/hostname"))
        .or_else(|| from_file("/proc/sys/kernel/hostname"))
        .or_else(|| std::env::var("HOSTNAME").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "another computer".to_string())
}

impl RpcHandler {
    /// Start watching plain store `store_id`'s folder, once: what the folder
    /// task does is this module's doc. Writes this computer's presence file
    /// and reads the others' first, so the answer to the call that opened the
    /// store already names them. Nothing for a store that is not open as a
    /// local plain store, or is already watched.
    pub(crate) async fn watch_folder(&self, store_id: StoreId) {
        if self.folders().lock().unwrap().contains_key(&store_id) {
            return;
        }
        let Some(path) = self.store_manager_handle().read().await.local_path(store_id) else { return };
        self.write_presence(store_id, &path).await;
        self.read_presence(store_id, &path).await;
        let handler = self.clone();
        let task = self.runtime().spawn(async move { handler.run_folder_task(store_id, path).await });
        let previous = self.folders().lock().unwrap().insert(store_id, FolderTask(task));
        // Two opens racing: the later task replaces the earlier, which stops.
        drop(previous);
    }

    /// Stop watching `store_id`'s folder (it closed). What was merged and not
    /// yet announced is dropped with the store.
    pub(crate) fn unwatch_folder(&self, store_id: StoreId) {
        let task = self.folders().lock().unwrap().remove(&store_id);
        drop(task);
        self.computers().lock().unwrap().remove(&store_id);
    }

    /// Stop every folder task (the server is stopping).
    pub(crate) fn stop_folder_watches(&self) {
        let tasks: Vec<FolderTask> = self.folders().lock().unwrap().drain().map(|(_, task)| task).collect();
        drop(tasks);
    }

    async fn run_folder_task(self, store_id: StoreId, path: PathBuf) {
        // Only a folder on the machine's own disk can be watched: a memory
        // disk (or the browser's OPFS, which sends no change events) has no
        // other computer writing into it. The rest of the task (announcing
        // what a flush merged, the presence files) runs on any disk.
        #[cfg(not(target_arch = "wasm32"))]
        let watched = if self.disk().is_native() { pimble_store::watch_folder(&path) } else { None };
        #[cfg(target_arch = "wasm32")]
        let watched: Option<((), tokio::sync::mpsc::UnboundedReceiver<PathBuf>)> = None;
        let (_watch, mut changes) = match watched {
            Some((watch, rx)) => (Some(watch), Some(rx)),
            None => (None, None),
        };
        let mut pending: BTreeSet<PathBuf> = BTreeSet::new();
        let mut first: Option<Instant> = None;
        let mut deadline: Option<Instant> = None;
        let runtime = self.runtime().clone();
        let mut announce = runtime.interval(ANNOUNCE_EVERY);
        let mut write_presence = runtime.interval_at(runtime.now() + PRESENCE_WRITE_EVERY, PRESENCE_WRITE_EVERY);
        let mut read_presence = runtime.interval_at(runtime.now() + PRESENCE_READ_EVERY, PRESENCE_READ_EVERY);
        loop {
            let next_change = async {
                match changes.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            };
            let settle = async {
                match deadline {
                    Some(at) => runtime.sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                changed = next_change => {
                    let Some(changed) = changed else { changes = None; continue };
                    pending.insert(changed);
                    let now = runtime.now();
                    let started = *first.get_or_insert(now);
                    deadline = Some((now + SETTLE).min(started + SETTLE_AT_MOST));
                }
                _ = settle => {
                    deadline = None;
                    first = None;
                    let paths: Vec<PathBuf> = std::mem::take(&mut pending).into_iter().collect();
                    // Another computer said it is there (or left a newer
                    // file): read the others' at once, not at the next tick.
                    let computers = path.join(pimble_store::LocalStore::COMPUTERS_DIR);
                    if paths.iter().any(|p| p.parent() == Some(computers.as_path())) {
                        self.read_presence(store_id, &path).await;
                    }
                    self.absorb_folder_changes(store_id, &paths).await;
                }
                _ = announce.tick() => self.announce_folder_merges(store_id).await,
                _ = write_presence.tick() => self.write_presence(store_id, &path).await,
                _ = read_presence.tick() => self.read_presence(store_id, &path).await,
            }
        }
    }

    /// Merge what changed at `paths` in `store_id`'s folder and announce it.
    pub(crate) async fn absorb_folder_changes(&self, store_id: StoreId, paths: &[PathBuf]) {
        let unwritten = {
            let manager = self.store_manager_handle();
            let mut manager = manager.write().await;
            if let Err(e) = manager.absorb_folder_paths(store_id, paths).await {
                warn!("Store {}: merging what changed in its folder failed: {}", store_id, e);
            }
            manager.has_unwritten(store_id)
        };
        self.announce_folder_merges(store_id).await;
        // A document held here with more than its file now holds (a sync tool
        // put an older copy over it) goes back to disk.
        if unwritten {
            self.schedule_flush(store_id);
        }
    }

    /// Announce what merges from the folder changed, as `apply_node_update_from`
    /// announces a peer's update.
    pub(crate) async fn announce_folder_merges(&self, store_id: StoreId) {
        let (merges, root) = {
            let manager = self.store_manager_handle();
            if !manager.read().await.has_folder_merges(store_id) {
                return;
            }
            let mut manager = manager.write().await;
            let merges = manager.take_folder_merges(store_id);
            let Ok(tree) = manager.tree(store_id) else { return };
            (merges, tree.root())
        };
        let source = folder_source(store_id);
        let mut structure = false;
        for merge in merges {
            debug!("Store {}: node {} changed in the folder", store_id, merge.node_id);
            structure |= merge.effect.structure;
            let update_b64 = base64::engine::general_purpose::STANDARD.encode(&merge.update);
            for change in derive_changes(merge.node_id, root, merge.before.as_ref(), merge.after.as_ref(), merge.effect) {
                let kind = StoreChangeKind::from(change);
                for event in index_events_for(&kind) {
                    self.enqueue_index_event(store_id, event).await;
                }
                self.broadcast_document_change(store_id, kind, Some(&source), Some(update_b64.clone())).await;
            }
        }
        if structure {
            self.adopt_document_root(store_id).await;
            self.schedule_repair(store_id);
        }
    }

    /// Write this computer's `computers/<presence id>.json` into the folder.
    async fn write_presence(&self, store_id: StoreId, path: &Path) {
        let device = presence_id(&self.device_entry(store_id).await.device);
        let file = ComputerFile { device: device.clone(), name: computer_name(), last_seen: Utc::now(), version: self.version().to_string() };
        let dir = path.join(pimble_store::LocalStore::COMPUTERS_DIR);
        let disk = self.disk();
        let written = async {
            disk.create_dir_all(&dir).await?;
            let json = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
            // `<device>.<12 random hex>.tmp`, renamed to `<device>.json`.
            disk.write_atomic(dir.join(format!("{device}.json")), json, pimble_host::TmpName::Unique).await
        };
        if let Err(e) = written.await {
            warn!("Store {}: could not say in its folder that this computer has it open: {}", store_id, e);
        }
    }

    /// Read the other computers' `computers/` files, for `Store::also_open_on`.
    async fn read_presence(&self, store_id: StoreId, path: &Path) {
        let own = presence_id(&self.device_entry(store_id).await.device);
        let mut others = Vec::new();
        let disk = self.disk();
        if let Ok(entries) = disk.list(path.join(pimble_store::LocalStore::COMPUTERS_DIR)).await {
            for entry in entries {
                let file = entry.path;
                if file.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let Ok(bytes) = disk.read(&file).await else { continue };
                let Ok(computer) = serde_json::from_slice::<ComputerFile>(&bytes) else { continue };
                if computer.device == own || Utc::now() - computer.last_seen > PRESENCE_SHOWN_FOR {
                    continue;
                }
                let older = pimble_core::version_older(&computer.version, self.version());
                others.push(OtherComputer { name: computer.name, last_seen: computer.last_seen, version: computer.version, older });
            }
        }
        others.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
        self.computers().lock().unwrap().insert(store_id, others);
    }

    /// The other computers known to open `store_id`'s folder.
    pub(crate) fn also_open_on(&self, store_id: StoreId) -> Vec<OtherComputer> {
        self.computers().lock().unwrap().get(&store_id).cloned().unwrap_or_default()
    }
}
