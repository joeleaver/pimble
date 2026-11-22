//! One store folder opened on several computers through a sync tool
//! (docs/STORE_FOLDERS_CONTRACT.md): what a computer finds in the folder is
//! merged in, never written over, and what belongs to one computer is kept
//! outside it.
//!
//! - Rule 1: `migrate_per_computer_state` moves this computer's state out
//!   of the folder at open, once.
//! - Rule 2: `LocalStore::write_node` and `LocalStore::write_manifest` read
//!   the file first and merge what another computer put there.
//! - Rule 3: `LocalStore::absorb_folder_paths` merges files the folder watch
//!   saw change (the server's `crate::folder_watch` drives it); what changed
//!   in held documents queues as `FolderMerge`s for the server to announce.
//! - Rule 4: `classify_node_file` names a sync tool's conflict copy of a
//!   document, and `LocalStore::absorb_conflict_file` merges and removes it.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use pimble_core::{NodeId, StoreId, StoreManifest};
use pimble_crdt::{DocShape, NodeDoc, NodeUpdateEffect};
use tokio::fs;
use tracing::{info, warn};

use super::{atomic_write, LocalStore};
use crate::error::{Result, StoreError};

/// A hash of a file's bytes, compared only within one process (std's
/// `DefaultHasher` has fixed keys, so the same bytes hash the same every
/// time here). It tells "the bytes this process last read or wrote" from
/// anything else; nothing is ever trusted to be equal on a hash match alone
/// except "nothing new to merge", and a wrong "nothing new" heals at the next
/// write, which reads the file again.
pub(crate) fn content_hash(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// What a file in `nodes/` is (docs/STORE_FOLDERS_CONTRACT.md rule 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeFile {
    /// `<id>.yrs`: the node's document.
    Document(NodeId),
    /// A name that starts with a node id and ends in `.yrs` but is not
    /// exactly `<id>.yrs`: a sync tool's copy of the document it could not
    /// reconcile (Dropbox `<id> (Joe's conflicted copy 2026-11-19).yrs`,
    /// OneDrive `<id>-COMPUTER.yrs`, Syncthing
    /// `<id>.sync-conflict-20261119-101112-ABCDEFG.yrs`, Google Drive
    /// `<id> (1).yrs`).
    Conflict(NodeId),
    /// Anything else: a temporary of this or another process, a sync tool's
    /// own partial download, a stray file. Left alone.
    Other,
}

/// What the file named `name` in `nodes/` is.
pub fn classify_node_file(name: &str) -> NodeFile {
    // A node id is written as a hyphenated UUID, 36 characters.
    const ID_LEN: usize = 36;
    if name.len() < ID_LEN || !name.is_char_boundary(ID_LEN) || !name.ends_with(".yrs") {
        return NodeFile::Other;
    }
    let (id, rest) = name.split_at(ID_LEN);
    let Ok(id) = NodeId::parse(id) else { return NodeFile::Other };
    if rest == ".yrs" {
        NodeFile::Document(id)
    } else {
        NodeFile::Conflict(id)
    }
}

/// The naming styles of the sync tools a store folder is known to live in,
/// for tests and for the record of what `classify_node_file` must take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictStyle {
    Dropbox,
    OneDrive,
    Syncthing,
    GoogleDrive,
}

impl ConflictStyle {
    pub const ALL: [ConflictStyle; 4] = [ConflictStyle::Dropbox, ConflictStyle::OneDrive, ConflictStyle::Syncthing, ConflictStyle::GoogleDrive];
}

/// The name `style`'s tool gives the `n`th conflict copy of `<id>.yrs`.
pub fn conflict_copy_name(id: NodeId, style: ConflictStyle, n: u32) -> String {
    match style {
        ConflictStyle::Dropbox => format!("{id} (Joe's conflicted copy 2026-11-{:02}).yrs", 1 + n % 28),
        ConflictStyle::OneDrive => format!("{id}-JOE-LAPTOP{}.yrs", if n == 0 { String::new() } else { format!("-{n}") }),
        ConflictStyle::Syncthing => format!("{id}.sync-conflict-20261119-1011{:02}-ABCDEFG.yrs", n % 60),
        ConflictStyle::GoogleDrive => format!("{id} ({}).yrs", n + 1),
    }
}

/// What merging a file from the folder changed in a held document
/// (docs/STORE_FOLDERS_CONTRACT.md rule 3), for the server to announce as it
/// announces a peer's update: derive the notifications from `before` and
/// `after` (`pimble_crdt::derive_changes`), relay `update`, feed the index.
#[derive(Debug, Clone)]
pub struct FolderMerge {
    pub node_id: NodeId,
    /// Everything the merge added to the document: a diff from where it
    /// stood before, the whole document when it was new here.
    pub update: Vec<u8>,
    pub effect: NodeUpdateEffect,
    pub before: Option<DocShape>,
    pub after: Option<DocShape>,
}

/// Merge `theirs` (the manifest another computer wrote) with `ours` (this
/// process's), field by field, from `base` (what both last agreed on, as far
/// as this process knows). A field only one side changed takes that side's
/// value; one both changed keeps ours (the next write puts it in the file, and
/// the other computer takes it then). Scope roots only ever grow, so they are
/// the union; ended roots are a set merged by what each side added and
/// removed. The id and kind are never another computer's to change.
pub(crate) fn merge_manifests(base: &StoreManifest, ours: &StoreManifest, theirs: &StoreManifest) -> StoreManifest {
    fn pick<T: PartialEq + Clone>(base: &T, ours: &T, theirs: &T) -> T {
        if ours == base {
            theirs.clone()
        } else {
            ours.clone()
        }
    }
    let mut merged = ours.clone();
    merged.name = pick(&base.name, &ours.name, &theirs.name);
    merged.root_node_id = pick(&base.root_node_id, &ours.root_node_id, &theirs.root_node_id);
    merged.version = ours.version.max(theirs.version);
    merged.created_at = ours.created_at.min(theirs.created_at);
    merged.modified_at = ours.modified_at.max(theirs.modified_at);
    // Scope roots only grow. When theirs holds one ours lacks, the union
    // is put in one order both computers arrive at, whichever merges: the
    // first (the tree's root) where it was, the rest by id.
    if theirs.scope_roots.iter().any(|root| !ours.scope_roots.contains(root)) {
        let mut rest: Vec<NodeId> = ours.scope_roots.iter().chain(theirs.scope_roots.iter()).skip(1).copied().collect();
        let first = ours.scope_roots.first().or(theirs.scope_roots.first()).copied();
        rest.retain(|root| Some(*root) != first);
        rest.sort_by_key(|root| root.to_string());
        rest.dedup();
        merged.scope_roots = first.into_iter().chain(rest).collect();
    }
    let removed_by_them: Vec<NodeId> = base.ended_roots.iter().filter(|r| !theirs.ended_roots.contains(r)).copied().collect();
    let added_by_them: Vec<NodeId> = theirs.ended_roots.iter().filter(|r| !base.ended_roots.contains(r)).copied().collect();
    let ended: Vec<NodeId> = ours.ended_roots.iter().filter(|r| !removed_by_them.contains(r)).chain(added_by_them.iter()).copied().collect();
    // In scope-root order, as `set_ended_roots` keeps them.
    merged.ended_roots = merged.scope_roots.iter().filter(|r| ended.contains(r)).copied().collect();
    if ours.id != theirs.id {
        warn!("Store {}: the folder's manifest names store {}; keeping this store's id", ours.id, theirs.id);
    }
    merged
}

/// Move what the folder still holds of one computer's state into
/// `state_dir`, once (docs/STORE_FOLDERS_CONTRACT.md rule 1): `sync.json`'s
/// per-computer keys (into `link.json`), `vault-link.json`, `people.json` and
/// `index/`. The link's files then go into the one record
/// (`crate::sync_state::migrate`, run right after this).
///
/// Whose state is it? The computer that kept it in the folder ran an older
/// Pimble. When no newer Pimble has opened the folder yet (it has no
/// `computers/` directory, which every newer Pimble makes at open, and which
/// an older one never touches) and this computer has none of its own yet,
/// the state is taken as this computer's: the usual case, one computer
/// opening its store after an update. Otherwise it belongs to an older Pimble
/// still running on another computer, and is left where it is and never
/// read: reading another computer's read positions as this one's is what
/// skipped entries and left edits unsent before 2026-11-19. Failures are
/// logged and the store still opens: everything moved here is either
/// derived (the index, people) or heals (a link that starts over from no
/// positions fetches everything again).
pub(crate) async fn migrate_per_computer_state(store_path: &Path, state_dir: &Path, store_id: StoreId) {
    let computers = store_path.join(LocalStore::COMPUTERS_DIR);
    let first = !computers.exists();
    // This computer's own link state, once it has the one record
    // (`crate::sync_state`): whatever the folder holds then is not its.
    let has_record = crate::sync_state::path(state_dir).exists();

    // `sync.json`'s per-computer keys.
    let sync_path = store_path.join(LocalStore::SYNC_CONFIG_FILE);
    if let Some(serde_json::Value::Object(mut map)) = super::read_json(&sync_path).await {
        let mut state = serde_json::Map::new();
        for key in LocalStore::PER_COMPUTER_KEYS {
            if let Some(v) = map.remove(key) {
                state.insert(key.to_string(), v);
            }
        }
        if !state.is_empty() {
            let state_path = state_dir.join(LocalStore::LINK_STATE_FILE);
            if first && !state_path.exists() && !has_record {
                let moved = async {
                    fs::create_dir_all(state_dir).await?;
                    atomic_write(&state_path, serde_json::to_string_pretty(&serde_json::Value::Object(state))?).await?;
                    atomic_write(&sync_path, serde_json::to_string_pretty(&serde_json::Value::Object(map))?).await?;
                    Ok::<(), std::io::Error>(())
                };
                match moved.await {
                    Ok(()) => info!("Store {}: the link's read positions moved out of the folder into {}", store_id, state_dir.display()),
                    Err(e) => warn!("Store {}: could not move the link's read positions out of the folder: {}", store_id, e),
                }
            } else {
                info!("Store {}: sync.json holds read positions an older Pimble on another computer wrote; they are not this computer's and are left alone", store_id);
            }
        }
    }

    // Whole files and directories, the same rule each.
    for name in ["vault-link.json", "people.json", LocalStore::INDEX_DIR] {
        let from = store_path.join(name);
        if !from.exists() {
            continue;
        }
        let to = state_dir.join(name);
        if !first {
            info!("Store {}: {} in the folder belongs to an older Pimble on another computer; left alone", store_id, name);
            continue;
        }
        if to.exists() || (name == "vault-link.json" && has_record) {
            // This computer already has its own; the folder's is a leftover.
            remove_path(&from).await;
            continue;
        }
        if let Err(e) = fs::create_dir_all(state_dir).await {
            warn!("Store {}: could not make {}: {}", store_id, state_dir.display(), e);
            return;
        }
        match fs::rename(&from, &to).await {
            Ok(()) => info!("Store {}: {} moved out of the folder into {}", store_id, name, state_dir.display()),
            Err(_) if name == LocalStore::INDEX_DIR => {
                // Another filesystem: the index is derived, so it is built
                // again rather than copied.
                remove_path(&from).await;
                info!("Store {}: the search index in the folder was removed; it is built again outside it", store_id);
            }
            Err(_) => match fs::copy(&from, &to).await {
                Ok(_) => remove_path(&from).await,
                Err(e) => warn!("Store {}: could not move {} out of the folder: {}", store_id, name, e),
            },
        }
    }

    if let Err(e) = fs::create_dir_all(&computers).await {
        warn!("Store {}: could not make {}: {}", store_id, computers.display(), e);
    }
}

async fn remove_path(path: &Path) {
    let removed = if path.is_dir() { fs::remove_dir_all(path).await } else { fs::remove_file(path).await };
    if let Err(e) = removed {
        warn!("Could not remove {}: {}", path.display(), e);
    }
}

/// Remove temporaries (`*.tmp`) an interrupted write left in `dir` more than
/// an hour ago. A younger one may be another process's write in progress
/// (two computers, or two servers, on one folder).
pub(crate) async fn sweep_stale_temporaries(dir: &Path) {
    let Ok(mut entries) = fs::read_dir(dir).await else { return };
    let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("tmp") {
            continue;
        }
        let old = entry.metadata().await.ok().and_then(|m| m.modified().ok()).is_some_and(|t| t < hour_ago);
        if old {
            let _ = fs::remove_file(&path).await;
        }
    }
}

impl LocalStore {
    /// Merge `bytes` (a node document's file, or any update to it) into
    /// `node_id`'s document, queueing what it changed as a [`FolderMerge`].
    /// The one way bytes from the folder reach a held document after open.
    fn merge_from_folder(&mut self, node_id: NodeId, bytes: &[u8]) -> Result<NodeUpdateEffect> {
        let before = self.tree.shape(node_id);
        let sv_before = self.tree.doc(node_id).map(|d| d.state_vector());
        let effect = self.tree.apply_update(node_id, bytes).map_err(StoreError::from)?;
        if effect.changed {
            let doc = self.tree.doc(node_id).ok_or(StoreError::NodeNotFound(node_id))?;
            let update = match &sv_before {
                Some(sv) => doc.diff_since(sv).map_err(StoreError::from)?,
                None => doc.save(),
            };
            let after = self.tree.shape(node_id);
            self.folder_merges.push(FolderMerge { node_id, update, effect, before, after });
        }
        Ok(effect)
    }

    /// Write `node_id`'s document to `nodes/<id>.yrs`, merging first whatever
    /// the file holds that this process did not put there (rule 2). A file
    /// that does not load (half written by a tool that writes in place) is
    /// written over with a warning: there is nothing in it to merge.
    pub(super) async fn write_node(&mut self, node_id: NodeId) -> Result<()> {
        let path = self.node_doc_path(node_id);
        match fs::read(&path).await {
            Ok(on_disk) => {
                if self.disk_hashes.get(&node_id) != Some(&content_hash(&on_disk)) {
                    if let Err(e) = self.merge_from_folder(node_id, &on_disk) {
                        warn!("Store {}: {} changed on disk but could not be merged ({}); writing over it", self.id, path.display(), e);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let Some(doc) = self.tree.doc(node_id) else { return Ok(()) };
        let bytes = doc.save();
        atomic_write(&path, &bytes).await?;
        self.disk_hashes.insert(node_id, content_hash(&bytes));
        Ok(())
    }

    /// Whether the manifest differs from what was last read or written,
    /// `modified_at` aside.
    pub(super) fn manifest_changed(&self) -> bool {
        let mut base = self.manifest_base.clone();
        base.modified_at = self.manifest.modified_at;
        base != self.manifest
    }

    /// Write `manifest.json`, merging first what another computer wrote to
    /// it since this process last read or wrote it (rule 2).
    pub(super) async fn write_manifest(&mut self) -> Result<()> {
        let path = self.path.join(Self::MANIFEST_FILE);
        if let Ok(on_disk) = fs::read(&path).await {
            if Some(content_hash(&on_disk)) != self.manifest_hash {
                match serde_json::from_slice::<StoreManifest>(&on_disk) {
                    Ok(theirs) => self.take_manifest(theirs),
                    Err(e) => warn!("Store {}: manifest.json changed on disk but does not parse ({}); writing over it", self.id, e),
                }
            }
        }
        let json = serde_json::to_string_pretty(&self.manifest)?;
        atomic_write(&path, &json).await?;
        self.manifest_hash = Some(content_hash(json.as_bytes()));
        self.manifest_base = self.manifest.clone();
        Ok(())
    }

    /// Merge `theirs` into the manifest held here (`merge_manifests`), and
    /// make it the base of the next merge.
    fn take_manifest(&mut self, theirs: StoreManifest) {
        let merged = merge_manifests(&self.manifest_base, &self.manifest, &theirs);
        if merged.root_node_id != self.manifest.root_node_id {
            self.tree.reroot(merged.root_node_id);
        }
        self.manifest = merged;
        self.manifest_base = theirs;
    }

    /// `manifest.json` changed in the folder: merge it in. A merge that
    /// keeps something of ours the file lacks leaves the manifest changed
    /// (`manifest_changed`), so the next flush writes it.
    async fn absorb_manifest(&mut self) -> Result<()> {
        let path = self.path.join(Self::MANIFEST_FILE);
        let Ok(on_disk) = fs::read(&path).await else { return Ok(()) };
        let hash = content_hash(&on_disk);
        if Some(hash) == self.manifest_hash {
            return Ok(());
        }
        match serde_json::from_slice::<StoreManifest>(&on_disk) {
            Ok(theirs) => {
                self.take_manifest(theirs);
                self.manifest_hash = Some(hash);
            }
            // Half written: the next change reads it again.
            Err(e) => warn!("Store {}: manifest.json changed on disk but does not parse yet: {}", self.id, e),
        }
        Ok(())
    }

    /// Merge what changed in the folder at `paths` (rule 3): documents under
    /// `nodes/` (a new one is a node another computer made, and joins the
    /// tree), conflict copies there (rule 4), and `manifest.json`. Anything
    /// else is ignored, and so is a file whose bytes are the ones this
    /// process last read or wrote (its own write, seen by the watch). What
    /// changed in held documents queues for [`LocalStore::take_folder_merges`];
    /// a document held here with more than its file holds is marked dirty, so
    /// the next flush writes it back (a sync tool can put an older copy over
    /// a newer one). Nothing is repaired here: the caller repairs after a
    /// structural merge, as for any peer's update.
    pub async fn absorb_folder_paths(&mut self, paths: &[PathBuf]) -> Result<()> {
        let nodes_dir = self.path.join(Self::NODES_DIR);
        let manifest = self.path.join(Self::MANIFEST_FILE);
        let mut conflicts = Vec::new();
        let mut manifest_seen = false;
        for path in paths {
            if *path == manifest {
                manifest_seen = true;
                continue;
            }
            if path.parent() != Some(nodes_dir.as_path()) {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
            match classify_node_file(name) {
                NodeFile::Document(id) => self.absorb_node_file(id, path).await?,
                NodeFile::Conflict(id) => conflicts.push((id, path.clone())),
                NodeFile::Other => {}
            }
        }
        conflicts.sort_by(|a, b| a.1.cmp(&b.1));
        conflicts.dedup_by(|a, b| a.1 == b.1);
        for (id, path) in conflicts {
            self.absorb_conflict_file(id, &path).await?;
        }
        if manifest_seen {
            self.absorb_manifest().await?;
        }
        Ok(())
    }

    /// [`LocalStore::absorb_folder_paths`] over everything in the folder:
    /// every file in `nodes/` and the manifest.
    pub async fn absorb_folder(&mut self) -> Result<()> {
        let mut paths = vec![self.path.join(Self::MANIFEST_FILE)];
        let mut entries = fs::read_dir(self.path.join(Self::NODES_DIR)).await?;
        while let Some(entry) = entries.next_entry().await? {
            paths.push(entry.path());
        }
        paths.sort();
        self.absorb_folder_paths(&paths).await
    }

    async fn absorb_node_file(&mut self, node_id: NodeId, path: &Path) -> Result<()> {
        let bytes = match fs::read(path).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let hash = content_hash(&bytes);
        if self.disk_hashes.get(&node_id) == Some(&hash) {
            return Ok(());
        }
        let on_disk = match NodeDoc::load(&bytes) {
            Ok(doc) => doc,
            Err(e) => {
                // Half written, most likely: the write that finishes it is
                // another change, and reads it again.
                warn!("Store {}: {} changed but does not load yet: {}", self.id, path.display(), e);
                return Ok(());
            }
        };
        self.merge_from_folder(node_id, &bytes)?;
        self.disk_hashes.insert(node_id, hash);
        let doc = self.tree.doc(node_id).ok_or(StoreError::NodeNotFound(node_id))?;
        if doc.state_vector() == on_disk.state_vector() && doc.delete_set_digest() == on_disk.delete_set_digest() {
            // The file holds everything held here: nothing to write back.
            self.dirty.remove(&node_id);
        } else {
            self.dirty.insert(node_id);
        }
        Ok(())
    }

    /// Merge a sync tool's conflict copy of `node_id`'s document into the
    /// document, write the merged document, then delete the copy (rule 4).
    /// A copy that does not load as a yrs document is left alone with a
    /// warning: it may not be ours to delete.
    pub(super) async fn absorb_conflict_file(&mut self, node_id: NodeId, path: &Path) -> Result<()> {
        let bytes = match fs::read(path).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        if let Err(e) = NodeDoc::load(&bytes) {
            warn!("Store {}: {} looks like a conflict copy but is not a document ({}); left alone", self.id, path.display(), e);
            return Ok(());
        }
        self.merge_from_folder(node_id, &bytes)?;
        self.write_node(node_id).await?;
        self.dirty.remove(&node_id);
        match fs::remove_file(path).await {
            Ok(()) => info!("Store {}: merged the conflict copy {} into its document and removed it", self.id, path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!("Store {}: merged {} but could not remove it: {}", self.id, path.display(), e),
        }
        Ok(())
    }

    /// What merges from the folder changed since the last call, in the order
    /// they were made, for the server to announce.
    pub fn take_folder_merges(&mut self) -> Vec<FolderMerge> {
        std::mem::take(&mut self.folder_merges)
    }

    /// Whether any merge from the folder waits to be announced.
    pub fn has_folder_merges(&self) -> bool {
        !self.folder_merges.is_empty()
    }

    /// Whether any document has changes not yet written.
    pub fn has_unwritten(&self) -> bool {
        !self.dirty.is_empty() || self.manifest_changed()
    }
}
