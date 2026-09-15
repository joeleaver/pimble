//! Application state management
//!
//! Uses per-entity reactive signals so that data changes (rename, mount state)
//! only trigger effects on the affected node, while structural changes (children
//! loaded, node moved, store opened/closed) bump `tree_structure_version` to
//! trigger a full tree rebuild.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use pimble_core::{MountRef, MountState, Node, NodeId, RemoteEndpoint, Store, StoreId, SyncState};
use rinch::components::TreeNodeData;
use rinch::prelude::*;

use crate::backend::BackendHandle;

thread_local! {
    /// The tree value (with any mount-path suffix, decision 7) of the last
    /// drag-and-drop target, set by `app.rs`'s `on_drop` right before sending
    /// `MoveNode` and consumed once by the `NodeMoved` handler in `events.rs`
    /// to auto-expand exactly the place the user dropped into, rather than
    /// reconstructing an unqualified value that would not match a target
    /// reached only through a mount.
    static LAST_DROP_TARGET_VALUE: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Record the qualified tree value of a drag-and-drop target, just before the
/// `MoveNode` command that will result in a `NodeMoved` event for it.
pub fn set_last_drop_target_value(value: String) {
    LAST_DROP_TARGET_VALUE.with(|cell| *cell.borrow_mut() = Some(value));
}

/// Take (and clear) the last recorded drop-target tree value, if any.
pub fn take_last_drop_target_value() -> Option<String> {
    LAST_DROP_TARGET_VALUE.with(|cell| cell.borrow_mut().take())
}

/// A pending mount operation waiting for a store to be opened.
#[derive(Debug, Clone)]
pub struct PendingMount {
    pub target_store_id: StoreId,
    pub target_parent_id: NodeId,
    pub source_path: String,
}

/// Current state of the search box: idle (query empty), the last successful
/// results (possibly empty), or an error/status message from the server (a
/// connection failure, or the index still building — `SearchError::IndexBuilding`
/// arrives as a plain RPC error whose text already says so, so it displays as-is).
#[derive(Debug, Clone, Default)]
pub enum SearchState {
    #[default]
    Idle,
    Results(Vec<pimble_rpc::SearchResultItem>),
    Error(String),
}

/// Mount metadata for a tree node.
#[derive(Debug, Clone)]
pub struct MountInfo {
    pub is_mount: bool,
    pub mount_state: Option<MountState>,
    /// The mount's target: source store and node. Populated as soon as we see
    /// the mount node's own data (`Node::mount_ref()`), and refreshed whenever
    /// `getMountState` answers (which is authoritative, e.g. after Agent A's
    /// `source_path` hint is filled in server-side).
    pub mount_ref: Option<MountRef>,
}

/// Extract plain-text content from node content bytes, for tree previews / labels.
///
/// `content` is a yrs snapshot (or empty). Blocks are joined by `\n`; `""` for
/// empty or unreadable content, in which case the caller falls back to the
/// node's title.
pub fn get_node_content_text(content: &[u8]) -> String {
    pimble_crdt::ContentDoc::text_of(content)
}

/// Compute a display label from an explicit-title flag/title plus a plain-text
/// content projection: an explicit title wins; otherwise the first non-empty
/// line of `content_text` (truncated to 25 chars with an ellipsis); otherwise
/// `title`; otherwise `"Untitled"`.
///
/// Shared by [`display_label_from_node`] (which projects `content_text` from
/// stored node bytes) and the editor's debounced live-label refresh
/// (`editor::schedule_label_refresh`), which supplies `content_text` straight
/// from the in-memory document instead of decoding a CRDT snapshot.
pub fn label_from_title_and_content(has_explicit_title: bool, title: &str, content_text: &str) -> String {
    if has_explicit_title && !title.is_empty() {
        return title.to_string();
    }

    let first_line = content_text
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if !first_line.is_empty() {
        let char_count = first_line.chars().count();
        if char_count > 25 {
            let truncated: String = first_line.chars().take(25).collect();
            return format!("{truncated}…");
        } else {
            return first_line.to_string();
        }
    }

    if !title.is_empty() {
        return title.to_string();
    }

    "Untitled".to_string()
}

/// The suffix a mount node's tree label carries for its current state
/// (docs/history/REMOTE_MOUNTS_CONTRACT.md "B: app side"). One function, used
/// everywhere a mount row is labelled.
pub fn mount_label_suffix(state: &MountState) -> &'static str {
    match state {
        MountState::Live => "",
        MountState::Connecting => " (connecting...)",
        MountState::Cached { .. } => " (offline copy)",
        MountState::Unavailable { .. } => " (unavailable)",
    }
}

/// Whether a mount row's icon and label render dimmed: its source is out of
/// reach (`Unavailable`) or not reachable yet (`Connecting`). A `Cached`
/// mount still shows its replica's content, so it reads normally and only
/// carries the "(offline copy)" suffix.
pub fn mount_is_dimmed(state: &MountState) -> bool {
    matches!(state, MountState::Unavailable { .. } | MountState::Connecting)
}

/// Which of the four mount states a `MountState` is, without its payload.
/// `MountState` is not `PartialEq`, and neither `last_sync` nor `reason`
/// changes anything the tree does, so this is what the app compares by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountStateKind {
    Live,
    Cached,
    Unavailable,
    Connecting,
}

pub fn mount_state_kind(state: &MountState) -> MountStateKind {
    match state {
        MountState::Live => MountStateKind::Live,
        MountState::Cached { .. } => MountStateKind::Cached,
        MountState::Unavailable { .. } => MountStateKind::Unavailable,
        MountState::Connecting => MountStateKind::Connecting,
    }
}

/// Compute display label from a Node reference (no signal dependency).
pub fn display_label_from_node(node: &Node) -> String {
    let has_explicit_title = node.metadata.custom
        .get("explicit_title")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let content = get_node_content_text(&node.content);
    label_from_title_and_content(has_explicit_title, &node.metadata.title, &content)
}

/// Global application state with per-entity reactive signals.
///
/// Structural changes bump `tree_structure_version` to trigger tree rebuilds.
/// Data changes (rename, mount state) only update per-entity signals, so only
/// the affected node's render Effects fire.
#[derive(Clone, Copy)]
pub struct AppStore {
    // Backend
    pub backend: Signal<Option<BackendHandle>>,
    pub connection: Signal<ConnectionState>,
    pub pending_create_path: Signal<Option<String>>,

    // Per-entity signal registries
    pub store_ids: Signal<Vec<StoreId>>,
    pub store_data: Signal<HashMap<StoreId, Signal<Store>>>,
    pub node_data: Signal<HashMap<(StoreId, NodeId), Signal<Node>>>,
    /// Each parent's children, as their OWN canonical `(StoreId, NodeId)`
    /// pairs — the same as the parent's store for an ordinary parent, the
    /// mount's source store when the parent is a mount node (decision 1).
    pub children_of: Signal<HashMap<(StoreId, NodeId), Signal<Vec<(StoreId, NodeId)>>>>,
    pub mount_data: Signal<HashMap<(StoreId, NodeId), Signal<MountInfo>>>,

    /// Each registered store's remote endpoint (`None` when unlinked) and
    /// current sync state (docs/SYNC_CONTRACT.md "B: app side"). An entry
    /// exists for every store as soon as it's registered (see
    /// `ensure_sync_entry`), so the tree row's badge signal is always
    /// available by the time the row renders, before `GetStoreSync` answers.
    pub sync_data: Signal<HashMap<StoreId, Signal<(Option<RemoteEndpoint>, SyncState)>>>,

    /// Bumped only on structural changes (store open/close, children loaded, node moved).
    /// The data_source closure subscribes to this to trigger tree rebuilds.
    pub tree_structure_version: Signal<u64>,

    pub expanded: Signal<HashSet<(StoreId, NodeId)>>,

    // UI state
    pub connection_status: Signal<String>,
    pub server_addr: Signal<String>,
    pub selected_id: Signal<Option<String>>,
    pub node_title: Signal<String>,
    pub show_editor: Signal<bool>,

    // Inline rename
    pub renaming_node: Signal<Option<String>>,
    pub rename_text: Signal<String>,

    // Drag-and-drop
    pub drop_target: Signal<Option<String>>,

    // Rename input DOM node IDs (tree_value → input element node ID for request_focus)
    /// Rename inputs by tree value as `(doc_key, node_id)`, so the double-click
    /// handler can focus one via `request_focus`.
    pub rename_input_ids: Signal<HashMap<String, (u64, usize)>>,

    // Mount picker
    pub pending_mount: Signal<Option<PendingMount>>,

    // "Copy as Mount Source" / "Paste Mount Here": the canonical pair and
    // display title of the node last copied as a mount source, if any.
    pub mount_source: Signal<Option<(StoreId, NodeId, String)>>,

    // Editor dirty flag — set when user edits content, cleared on save/load
    pub editor_dirty: Signal<bool>,

    // Active editing state — which node is currently open in the shared editor.
    // Its content lives in the editor's collab session; edits broadcast to the
    // server live rather than being saved by wholesale replacement.
    pub active_edit: Signal<Option<ActiveEdit>>,

    // Locally-computed display label for the node currently being typed into,
    // refreshed on a debounce by `editor::schedule_label_refresh` straight from
    // the editor's in-memory document. Takes priority over the content-derived
    // label in the tree render, since the per-node signal's cached `content`
    // isn't updated while a node is under active local edit (see
    // `upsert_node`, which clears an entry once authoritative content arrives).
    pub live_label: Signal<HashMap<(StoreId, NodeId), String>>,

    // This client's unique ID (for echo suppression in notifications)
    pub client_id: Signal<String>,

    // Search: the toolbar search box's text and the last response for it.
    // The results panel replaces the tree whenever `search_query` is non-empty.
    pub search_query: Signal<String>,
    pub search_results: Signal<SearchState>,

    // "Add Remote Store..." modal (File menu, docs/history/HARDENING_CONTRACT.md "B:
    // app"): browse a remote's open stores and add one as a local replica.
    // No folder/path field — the server places the replica in its own data
    // directory, so nothing here opens a native OS dialog.
    pub connect_modal_open: Signal<bool>,
    pub connect_modal_url: Signal<String>,
    /// Empty means "use what the server saved for this remote's origin"
    /// (docs/history/HARDENING_CONTRACT.md decision 4); non-empty is sent as
    /// `AuthMethod::Bearer`.
    pub connect_modal_token: Signal<String>,
    /// Whether the token field's `PasswordInput` shows the token in clear text.
    pub connect_modal_token_visible: Signal<bool>,
    pub connect_modal_stores: Signal<Vec<Store>>,
    /// The selected store's id, as a string (the raw `Select` value).
    pub connect_modal_selected: Signal<String>,
    pub connect_modal_error: Signal<String>,
    /// True while a `ListRemoteStores` or `AddRemoteStore` request is in flight.
    pub connect_modal_busy: Signal<bool>,
    /// True while specifically waiting for `AddRemoteStore`'s answer, so the
    /// generic `BackendEvent::Error`/`StoreOpened` outcome routes into this
    /// modal's own error line / auto-close instead of the global status bar.
    pub connect_modal_pending_add: Signal<bool>,
    /// Set by "Mount Remote Store Here..." to the canonical target the mount
    /// is created under (a store root's target is its own root node,
    /// docs/history/REMOTE_MOUNTS_CONTRACT.md "B: app side"). `None` is the plain
    /// "Add Remote Store..." mode: the same modal, but the action only adds
    /// the replica. Cleared whenever the modal closes.
    pub connect_modal_target: Signal<Option<(StoreId, NodeId)>>,

    // "Link to Remote..." modal (store root context menu). `Some(store_id)`
    // is the store the modal is open for; `None` means closed.
    pub link_modal_store: Signal<Option<StoreId>>,
    pub link_modal_url: Signal<String>,
    /// Same token semantics as `connect_modal_token`.
    pub link_modal_token: Signal<String>,
    /// Whether the token field's `PasswordInput` shows the token in clear text.
    pub link_modal_token_visible: Signal<bool>,
    pub link_modal_error: Signal<String>,
    /// True while a `SetStoreSync` request from this modal is in flight.
    pub link_modal_pending: Signal<bool>,

    // "Appearance..." modal (node context menu): the node whose icon and
    // colour are being picked; `None` means closed.
    pub appearance_modal_node: Signal<Option<(StoreId, NodeId)>>,

    // "Remove Replica..." confirmation modal (store root context menu,
    // docs/history/HARDENING_CONTRACT.md "B: app"). `Some(store_id)` is the replica
    // the modal is open for; `None` means closed.
    pub remove_replica_modal_store: Signal<Option<StoreId>>,
    pub remove_replica_modal_error: Signal<String>,
    /// True while a `RemoveReplica` request from this modal is in flight.
    pub remove_replica_modal_pending: Signal<bool>,
}

/// Identifies the node currently open in the shared editor.
#[derive(Clone)]
pub struct ActiveEdit {
    pub store_id: StoreId,
    pub node_id: NodeId,
}

impl AppStore {
    pub fn new() -> Self {
        Self {
            backend: Signal::new(None),
            connection: Signal::new(ConnectionState::Disconnected),
            pending_create_path: Signal::new(None),
            store_ids: Signal::new(Vec::new()),
            store_data: Signal::new(HashMap::new()),
            node_data: Signal::new(HashMap::new()),
            children_of: Signal::new(HashMap::new()),
            mount_data: Signal::new(HashMap::new()),
            sync_data: Signal::new(HashMap::new()),
            tree_structure_version: Signal::new(0),
            expanded: Signal::new(HashSet::new()),
            connection_status: Signal::new("Connecting...".to_string()),
            server_addr: Signal::new(String::new()),
            selected_id: Signal::new(None),
            node_title: Signal::new(String::new()),
            show_editor: Signal::new(false),
            renaming_node: Signal::new(None),
            rename_text: Signal::new(String::new()),
            drop_target: Signal::new(None),
            rename_input_ids: Signal::new(HashMap::new()),
            pending_mount: Signal::new(None),
            mount_source: Signal::new(None),
            editor_dirty: Signal::new(false),
            active_edit: Signal::new(None),
            live_label: Signal::new(HashMap::new()),
            client_id: Signal::new(String::new()),
            search_query: Signal::new(String::new()),
            search_results: Signal::new(SearchState::Idle),
            connect_modal_open: Signal::new(false),
            connect_modal_url: Signal::new(String::new()),
            connect_modal_token: Signal::new(String::new()),
            connect_modal_token_visible: Signal::new(false),
            connect_modal_stores: Signal::new(Vec::new()),
            connect_modal_selected: Signal::new(String::new()),
            connect_modal_error: Signal::new(String::new()),
            connect_modal_busy: Signal::new(false),
            connect_modal_pending_add: Signal::new(false),
            connect_modal_target: Signal::new(None),
            link_modal_store: Signal::new(None),
            link_modal_url: Signal::new(String::new()),
            link_modal_token: Signal::new(String::new()),
            link_modal_token_visible: Signal::new(false),
            link_modal_error: Signal::new(String::new()),
            link_modal_pending: Signal::new(false),
            appearance_modal_node: Signal::new(None),
            remove_replica_modal_store: Signal::new(None),
            remove_replica_modal_error: Signal::new(String::new()),
            remove_replica_modal_pending: Signal::new(false),
        }
    }

    /// Bump the tree structure version to trigger a tree rebuild.
    pub fn bump_tree_structure(&self) {
        self.tree_structure_version.update(|v| *v += 1);
    }

    /// Insert or update a store's signal and add to store_ids if new.
    ///
    /// Looks up the inner signal first, then sets it *outside* the outer
    /// HashMap borrow to avoid RefCell re-entrancy if subscribers read back.
    pub fn upsert_store(&self, store: Store) {
        let sid = store.id;
        let existing = self.store_data.with(|map| map.get(&sid).copied());
        if let Some(sig) = existing {
            sig.set(store);
        } else {
            // Pre-create signal outside .update() — Signal::new() and .update()
            // both borrow SIGNAL_STORE mutably, so nesting them panics.
            let new_sig = Signal::new(store);
            self.store_data.update(|map| {
                map.insert(sid, new_sig);
            });
        }
        self.store_ids.update(|ids| {
            if !ids.contains(&sid) {
                ids.push(sid);
            }
        });
    }

    /// Remove a store and all its associated per-entity signals.
    pub fn remove_store(&self, store_id: StoreId) {
        self.store_ids.update(|ids| ids.retain(|&id| id != store_id));
        self.store_data.update(|map| { map.remove(&store_id); });
        self.node_data.update(|map| { map.retain(|(sid, _), _| *sid != store_id); });
        self.children_of.update(|map| { map.retain(|(sid, _), _| *sid != store_id); });
        self.mount_data.update(|map| { map.retain(|(sid, _), _| *sid != store_id); });
        self.live_label.update(|map| { map.retain(|(sid, _), _| *sid != store_id); });
        self.sync_data.update(|map| { map.remove(&store_id); });
    }

    /// Remove a node and its per-entity signals (node_data, mount_data, children_of).
    pub fn remove_node(&self, store_id: StoreId, node_id: NodeId) {
        let key = (store_id, node_id);
        self.node_data.update(|map| { map.remove(&key); });
        self.mount_data.update(|map| { map.remove(&key); });
        self.children_of.update(|map| { map.remove(&key); });
        self.live_label.update(|map| { map.remove(&key); });
    }

    /// Remove `node_id` and every cached descendant from the app's caches,
    /// and clear the selection if it was anywhere inside — the server
    /// deletes a whole subtree in one go (docs/history/HARDENING_CONTRACT.md item
    /// 10), so both the local echo of a delete and a remote `NodeDeleted`
    /// notification (which names only the subtree's root) need to drop the
    /// rest of it here too.
    ///
    /// Recurses through each cached node's own `children` field, which is
    /// always addressed within `store_id` — never through the `children_of`
    /// cache, whose entry for a MOUNT node holds pairs from the mount's
    /// SOURCE store: those are a different store's real nodes, unaffected by
    /// deleting the mount placeholder, and must not be swept up here.
    pub fn remove_subtree(&self, store_id: StoreId, node_id: NodeId) {
        let child_ids: Vec<NodeId> = untracked(|| {
            self.node_data.with(|map| {
                map.get(&(store_id, node_id))
                    .map(|sig| sig.with(|n| n.children.clone()))
                    .unwrap_or_default()
            })
        });
        for child_id in child_ids {
            self.remove_subtree(store_id, child_id);
        }
        self.remove_node(store_id, node_id);

        if let Some(selected_id) = self.selected_id.get() {
            if let Some((sel_sid, Some(sel_nid))) = parse_tree_value(&selected_id) {
                if sel_sid == store_id && sel_nid == node_id {
                    self.selected_id.set(None);
                    self.node_title.set(String::new());
                    self.show_editor.set(false);
                }
            }
        }
    }

    /// Insert or update a node's per-entity signal.
    ///
    /// Sets the inner signal outside the outer borrow to avoid re-entrancy.
    pub fn upsert_node(&self, store_id: StoreId, node: Node) {
        let key = (store_id, node.id);
        let existing = self.node_data.with(|map| map.get(&key).copied());
        if let Some(sig) = existing {
            sig.set(node);
        } else {
            let new_sig = Signal::new(node);
            self.node_data.update(|map| {
                map.insert(key, new_sig);
            });
        }
        // Authoritative content just replaced the cached node — drop any stale
        // locally-computed label override for it (see `live_label`).
        self.live_label.update(|m| { m.remove(&key); });
    }

    /// Set children for a parent node's per-entity signal. `children` are
    /// each child's own canonical `(StoreId, NodeId)` pair (decision 1).
    ///
    /// Sets the inner signal outside the outer borrow to avoid re-entrancy.
    pub fn set_children(&self, store_id: StoreId, parent_id: NodeId, children: Vec<(StoreId, NodeId)>) {
        let key = (store_id, parent_id);
        let existing = self.children_of.with(|map| map.get(&key).copied());
        if let Some(sig) = existing {
            sig.set(children);
        } else {
            let new_sig = Signal::new(children);
            self.children_of.update(|map| {
                map.insert(key, new_sig);
            });
        }
    }

    /// Get the per-node signal (untracked read of the registry).
    pub fn get_node_signal(&self, store_id: StoreId, node_id: NodeId) -> Option<Signal<Node>> {
        untracked(|| self.node_data.with(|map| map.get(&(store_id, node_id)).copied()))
    }

    /// Get the per-store signal (untracked read of the registry).
    pub fn get_store_signal(&self, store_id: StoreId) -> Option<Signal<Store>> {
        untracked(|| self.store_data.with(|map| map.get(&store_id).copied()))
    }

    /// Get the per-mount signal (untracked read of the registry).
    pub fn get_mount_signal(&self, store_id: StoreId, node_id: NodeId) -> Option<Signal<MountInfo>> {
        untracked(|| self.mount_data.with(|map| map.get(&(store_id, node_id)).copied()))
    }

    /// Get the children signal for a parent (untracked read of the registry).
    pub fn get_children_signal(&self, store_id: StoreId, node_id: NodeId) -> Option<Signal<Vec<(StoreId, NodeId)>>> {
        untracked(|| self.children_of.with(|map| map.get(&(store_id, node_id)).copied()))
    }

    /// Check if children have been loaded for a parent.
    pub fn has_children_loaded(&self, store_id: StoreId, node_id: NodeId) -> bool {
        untracked(|| self.children_of.with(|map| map.contains_key(&(store_id, node_id))))
    }

    /// Get the root node id for a store (untracked).
    pub fn root_node_id(&self, store_id: StoreId) -> Option<NodeId> {
        self.get_store_signal(store_id).map(|sig| untracked(|| sig.with(|s| s.root_node_id)))
    }

    /// Get all local paths of open stores (untracked, for persistence).
    pub fn all_store_local_paths(&self) -> Vec<String> {
        untracked(|| {
            let ids = self.store_ids.get();
            ids.iter().filter_map(|&sid| {
                self.store_data.with(|map| {
                    map.get(&sid).and_then(|sig| {
                        sig.with(|s| s.local_path().map(|p| p.to_string_lossy().to_string()))
                    })
                })
            }).collect()
        })
    }

    /// Update mount info for a node. Call this whenever a node is inserted into the cache.
    /// Picks up `mount_ref` straight from the node's own data
    /// (`Node::mount_ref()`) when present, so a mount's target is known as
    /// soon as its node is seen, without waiting on `getMountState`.
    pub fn track_mount_info(&self, store_id: StoreId, node: &pimble_core::Node) {
        if node.is_mount() {
            let key = (store_id, node.id);
            let mount_ref = node.mount_ref();
            let existing = self.mount_data.with(|map| map.get(&key).copied());
            if let Some(sig) = existing {
                sig.update(|m| {
                    m.is_mount = true;
                    if mount_ref.is_some() {
                        m.mount_ref = mount_ref.clone();
                    }
                });
            } else {
                let new_sig = Signal::new(MountInfo {
                    is_mount: true,
                    mount_state: None,
                    mount_ref,
                });
                self.mount_data.update(|map| {
                    map.insert(key, new_sig);
                });
            }
        }
    }

    /// Update the mount state (and the authoritative `mount_ref` the
    /// `getMountState` RPC returns alongside it) for a specific node.
    pub fn set_mount_state(&self, store_id: StoreId, node_id: NodeId, state: MountState, mount_ref: MountRef) {
        let key = (store_id, node_id);
        let existing = self.mount_data.with(|map| map.get(&key).copied());
        if let Some(sig) = existing {
            sig.update(|m| { m.mount_state = Some(state); m.mount_ref = Some(mount_ref); });
        } else {
            let new_sig = Signal::new(MountInfo {
                is_mount: true,
                mount_state: Some(state),
                mount_ref: Some(mount_ref),
            });
            self.mount_data.update(|map| {
                map.insert(key, new_sig);
            });
        }
    }

    /// Update just a mount's state, keeping whatever `mount_ref` is already
    /// known — for a `MountStateChanged` notification, which carries only the
    /// node and the new state (docs/history/REMOTE_MOUNTS_CONTRACT.md decision 5).
    /// [`set_mount_state`] stays for the `getMountState` answer, which is
    /// authoritative about the ref as well.
    ///
    /// Returns whether this is news, which is what decides if the caller
    /// refetches. It is `false` in two cases:
    ///
    /// - the state's kind is the one the row already shows. The server
    ///   re-derives a mount's state on every category transition of the
    ///   source's sync link, and a link whose remote is down flaps `Syncing`
    ///   ↔ `Offline` on a backoff forever — every one of those maps to the
    ///   same `Cached`, and refetching the mount's children on each would be
    ///   an endless poll.
    /// - the app holds no entry for that node. Stale `resolved_mounts`
    ///   entries for a deleted mount are tolerated server-side (decision 5),
    ///   so a notification can name a mount this app has already dropped;
    ///   caching state for a node that no longer exists would only produce
    ///   `GetChildren` calls the server answers with "node not found".
    pub fn update_mount_state(&self, store_id: StoreId, node_id: NodeId, state: MountState) -> bool {
        let key = (store_id, node_id);
        let Some(sig) = untracked(|| self.mount_data.with(|map| map.get(&key).copied())) else {
            return false;
        };
        let kind = mount_state_kind(&state);
        let was = untracked(|| sig.with(|m| m.mount_state.as_ref().map(mount_state_kind)));
        sig.update(|m| { m.is_mount = true; m.mount_state = Some(state); });
        was != Some(kind)
    }

    /// The source store a mount node points at, if its `mount_ref` is known
    /// (untracked).
    pub fn mount_source_store(&self, store_id: StoreId, node_id: NodeId) -> Option<StoreId> {
        untracked(|| {
            self.mount_data.with(|map| {
                map.get(&(store_id, node_id))
                    .and_then(|sig| sig.with(|m| m.mount_ref.as_ref().map(|r| r.source_store)))
            })
        })
    }

    /// Whether a node's row is currently expanded in the tree (untracked).
    pub fn is_expanded(&self, store_id: StoreId, node_id: NodeId) -> bool {
        untracked(|| self.expanded.with(|e| e.contains(&(store_id, node_id))))
    }

    /// Ensure a store has a sync-status entry, defaulting to unlinked/offline.
    /// Called as soon as a store is registered (`register_opened_store`) so
    /// the tree row's badge signal always exists by the time the row
    /// renders, ahead of `GetStoreSync`'s answer.
    pub fn ensure_sync_entry(&self, store_id: StoreId) {
        let exists = self.sync_data.with(|map| map.contains_key(&store_id));
        if !exists {
            let new_sig = Signal::new((None, SyncState::Offline));
            self.sync_data.update(|map| {
                map.insert(store_id, new_sig);
            });
        }
    }

    /// Get the per-store sync signal (untracked read of the registry).
    pub fn get_sync_signal(&self, store_id: StoreId) -> Option<Signal<(Option<RemoteEndpoint>, SyncState)>> {
        untracked(|| self.sync_data.with(|map| map.get(&store_id).copied()))
    }

    /// Set a store's remote endpoint and sync state (answer to
    /// `SetStoreSync`/`GetStoreSync`, which always carry both).
    pub fn set_sync(&self, store_id: StoreId, remote: Option<RemoteEndpoint>, state: SyncState) {
        let existing = self.sync_data.with(|map| map.get(&store_id).copied());
        if let Some(sig) = existing {
            sig.set((remote, state));
        } else {
            let new_sig = Signal::new((remote, state));
            self.sync_data.update(|map| {
                map.insert(store_id, new_sig);
            });
        }
    }

    /// Update just a store's sync state, preserving whatever remote endpoint
    /// is already known — for a bare `SyncStateChanged` notification, which
    /// carries only the new state (decision 6, docs/SYNC_CONTRACT.md).
    pub fn update_sync_state(&self, store_id: StoreId, state: SyncState) {
        let existing = self.sync_data.with(|map| map.get(&store_id).copied());
        if let Some(sig) = existing {
            sig.update(|(_, s)| *s = state);
        } else {
            let new_sig = Signal::new((None, state));
            self.sync_data.update(|map| {
                map.insert(store_id, new_sig);
            });
        }
    }

    /// Whether a store currently has a remote endpoint linked (untracked).
    pub fn is_linked(&self, store_id: StoreId) -> bool {
        untracked(|| {
            self.sync_data.with(|map| {
                map.get(&store_id).map_or(false, |sig| sig.with(|(remote, _)| remote.is_some()))
            })
        })
    }

    /// Canonical pairs of every mount node whose `mount_ref` names exactly
    /// `(source_store, source_node)` (untracked). Used for the exact refetch
    /// (docs/history/HARDENING_CONTRACT.md "B: app"): a structural notification names
    /// every parent it touched, so only mounts sourced from those precise
    /// `(store, parent)` pairs need refreshing — never a whole-store scan.
    pub fn mounts_sourced_from_node(&self, source_store: StoreId, source_node: NodeId) -> Vec<(StoreId, NodeId)> {
        untracked(|| {
            self.mount_data.with(|map| {
                map.iter()
                    .filter_map(|(&key, sig)| {
                        let matches = sig.with(|m| {
                            m.mount_ref.as_ref().map_or(false, |r| {
                                r.source_store == source_store && r.source_node == source_node
                            })
                        });
                        if matches { Some(key) } else { None }
                    })
                    .collect()
            })
        })
    }

    /// A node's cached parent id (untracked) — `None` if the node isn't
    /// cached or has no parent (a store root). Used by the `TreeStructure`
    /// exact refetch to find "that listed id's cached parent"
    /// (docs/history/HARDENING_CONTRACT.md "B: app").
    pub fn cached_parent_id(&self, store_id: StoreId, node_id: NodeId) -> Option<NodeId> {
        untracked(|| {
            self.node_data.with(|map| {
                map.get(&(store_id, node_id)).and_then(|sig| sig.with(|n| n.parent_id))
            })
        })
    }

    /// Check if a node is a mount point (untracked).
    pub fn is_mount(&self, store_id: StoreId, node_id: NodeId) -> bool {
        untracked(|| {
            self.mount_data.with(|map| {
                map.get(&(store_id, node_id)).map_or(false, |sig| sig.with(|m| m.is_mount))
            })
        })
    }

    /// Build structural tree data for the Tree component's data_source.
    ///
    /// Must be called from within an `untracked()` context. Builds TreeNodeData
    /// with empty labels for nodes (render_node Effects fill them reactively)
    /// and store names for store roots.
    pub fn build_tree_data_structural(&self) -> Vec<TreeNodeData> {
        let store_ids = self.store_ids.get();
        // Same rinch #714 reason as the link state below: "Paste Mount Here"
        // snapshots whether a mount source is copied at render time, so
        // every row's data must change when that flips, or no row re-renders
        // and the item never enables.
        let paste = self.mount_source.with(|s| s.is_some());
        let mut result = Vec::new();
        for &sid in &store_ids {
            let store_info = self.store_data.with(|map| {
                map.get(&sid).map(|sig| sig.with(|s| (s.name.clone(), s.root_node_id)))
            });
            let Some((store_name, root_id)) = store_info else { continue };

            // rinch's Tree re-renders a row only when its `TreeNodeData`
            // changes, and the store row's context menu snapshots whether the
            // store is linked at render time (rinch #714 keeps those items
            // static). The row renderer draws its own label from the store
            // signal and never reads this one, so the label carries the link
            // state: linking or unlinking changes the data and re-renders the
            // row with fresh menu items.
            let linked = self.sync_data.with(|map| {
                map.get(&sid).map_or(false, |sig| sig.with(|(remote, _)| remote.is_some()))
            });
            let store_node = TreeNodeData::new(
                format!("store_{}", sid),
                format!("{store_name} (linked: {linked}, paste: {paste})"),
            );
            let children = self.build_children_structural(sid, root_id, paste, &[]);
            if children.is_empty() {
                result.push(store_node);
            } else {
                result.push(store_node.with_children(children));
            }
        }
        result
    }

    /// Build structural children recursively (called from untracked context).
    ///
    /// `mount_path` accumulates the canonical `(mount_store, mount_node)` pair
    /// of every mount node crossed to reach this level, outermost first
    /// (decision 7) — empty for a node reached directly from its own store's
    /// root. It disambiguates the same canonical node appearing in several
    /// places, since rinch's Tree keys expansion and selection by value string.
    fn build_children_structural(
        &self,
        store_id: StoreId,
        parent_id: NodeId,
        paste: bool,
        mount_path: &[(StoreId, NodeId)],
    ) -> Vec<TreeNodeData> {
        let children = self.children_of.with(|map| {
            map.get(&(store_id, parent_id)).map(|sig| sig.get())
        });
        let Some(children) = children else { return Vec::new() };

        let suffix = mount_path_suffix(mount_path);

        let mut result = Vec::new();
        for &(child_store, child_id) in &children {
            let is_mount = self.mount_data.with(|map| {
                map.get(&(child_store, child_id)).map_or(false, |sig| sig.with(|m| m.is_mount))
            });

            // The renderer never reads this label (render_node Effects draw
            // it reactively); it only carries what the row snapshots at render
            // time — the "Paste Mount Here" state and the node's custom icon
            // and colour — so the row re-renders when any of them changes
            // (see `build_tree_data_structural`).
            let appearance = self.node_data.with(|map| {
                map.get(&(child_store, child_id)).map(|sig| {
                    sig.with(|n| format!("{}|{}", n.metadata.icon().unwrap_or(""), n.metadata.color().unwrap_or("")))
                })
            });
            let tree_node = TreeNodeData::new(
                format!("node_{}_{}{}", child_store, child_id, suffix),
                format!("{}|{}", if paste { "paste" } else { "" }, appearance.unwrap_or_default()),
            );

            // Crossing a mount node adds it to the path for everything below it.
            let children_data = if is_mount {
                let mut next_path = mount_path.to_vec();
                next_path.push((child_store, child_id));
                self.build_children_structural(child_store, child_id, paste, &next_path)
            } else {
                self.build_children_structural(child_store, child_id, paste, mount_path)
            };
            let has_children = !children_data.is_empty();
            let has_loaded_children = self.children_of.with(|map| {
                map.contains_key(&(child_store, child_id))
            });

            // Check if the node itself reports having children (from its
            // children list) even if we haven't fetched them yet. This
            // lets the tree show an expand chevron for unfetched subtrees.
            let node_reports_children = !has_loaded_children && self.node_data.with(|map| {
                map.get(&(child_store, child_id))
                    .map_or(false, |sig| sig.with(|n| !n.children.is_empty()))
            });

            if has_children {
                result.push(tree_node.with_children(children_data));
            } else if is_mount && !has_loaded_children {
                let placeholder = TreeNodeData::new(
                    format!("mount_loading_{}_{}{}", child_store, child_id, suffix),
                    "Loading...",
                );
                result.push(tree_node.with_children(vec![placeholder]));
            } else if node_reports_children {
                // Node has children we haven't fetched yet — show a
                // placeholder so the tree renders an expand chevron.
                let placeholder = TreeNodeData::new(
                    format!("placeholder_{}_{}{}", child_store, child_id, suffix),
                    "",
                );
                result.push(tree_node.with_children(vec![placeholder]));
            } else {
                result.push(tree_node);
            }
        }
        result
    }

    /// Compute the display label for a node (untracked, for use in callbacks).
    pub fn display_label(&self, store_id: StoreId, node_id: NodeId) -> String {
        untracked(|| {
            self.node_data.with(|map| {
                map.get(&(store_id, node_id))
                    .map(|sig| sig.with(|node| display_label_from_node(node)))
                    .unwrap_or_else(|| "Untitled".to_string())
            })
        })
    }

    /// Get the store_id and node_id of the selected node (if any)
    pub fn selected_store_and_node(&self) -> Option<(StoreId, NodeId)> {
        let selected_id = self.selected_id.get()?;
        let (store_id, node_id) = parse_tree_value(&selected_id)?;
        let node_id = node_id?;
        Some((store_id, node_id))
    }

    /// Send a command to the backend if connected.
    pub fn send(&self, cmd: crate::backend::BackendCommand) {
        self.backend.with(|b| {
            if let Some(backend) = b {
                backend.send(cmd);
            }
        });
    }
}

/// Build the mount-path suffix for a tree value (decision 7): one
/// `/{mount_store}_{mount_node}` segment per mount level crossed, innermost
/// last. Empty for a node reached directly (no mounts crossed).
fn mount_path_suffix(mount_path: &[(StoreId, NodeId)]) -> String {
    let mut s = String::new();
    for (mount_store, mount_node) in mount_path {
        s.push('/');
        s.push_str(&mount_store.to_string());
        s.push('_');
        s.push_str(&mount_node.to_string());
    }
    s
}

/// Parse a tree value ID like "store_{uuid}" or
/// "node_{store_uuid}_{node_uuid}[/{mount_store}_{mount_node}...]" — always
/// returning the CANONICAL pair (the first 73 characters after "node_"),
/// ignoring any mount-path suffix (decision 7). Every existing consumer wants
/// the canonical node identity regardless of which place in the tree it was
/// reached through.
pub fn parse_tree_value(value: &str) -> Option<(StoreId, Option<NodeId>)> {
    if let Some(rest) = value.strip_prefix("store_") {
        let uuid: uuid::Uuid = rest.parse().ok()?;
        Some((StoreId(uuid), None))
    } else if let Some(rest) = value.strip_prefix("node_") {
        // Format: node_{store_uuid}_{node_uuid}[/...path suffix]
        // UUIDs are 36 chars each; 36 + 1 ('_') + 36 = 73.
        if rest.len() >= 73 {
            let store_str = &rest[..36];
            let node_str = &rest[37..73];
            let store_uuid: uuid::Uuid = store_str.parse().ok()?;
            let node_uuid: uuid::Uuid = node_str.parse().ok()?;
            Some((StoreId(store_uuid), Some(NodeId(node_uuid))))
        } else {
            None
        }
    } else {
        None
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum ConnectionState {
    #[default]
    Disconnected,
    Connected,
    Error(String),
}
