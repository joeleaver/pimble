//! Application state management
//!
//! Uses per-entity reactive signals so that data changes (rename, mount state)
//! only trigger effects on the affected node, while structural changes (children
//! loaded, node moved, store opened/closed) bump `tree_structure_version` to
//! trigger a full tree rebuild.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use pimble_core::{MountRef, MountState, Node, NodeId, Store, StoreId};
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
    }

    /// Remove a node and its per-entity signals (node_data, mount_data, children_of).
    pub fn remove_node(&self, store_id: StoreId, node_id: NodeId) {
        let key = (store_id, node_id);
        self.node_data.update(|map| { map.remove(&key); });
        self.mount_data.update(|map| { map.remove(&key); });
        self.children_of.update(|map| { map.remove(&key); });
        self.live_label.update(|map| { map.remove(&key); });
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

    /// Canonical pairs of every mount node whose `mount_ref.source_store` is
    /// `source_store` (untracked). Used to refresh mounts in response to a
    /// structural change reported on that store's own `storeChanged`
    /// subscription (decision 6: no new server-side fan-out for mounts).
    pub fn mounts_sourced_from(&self, source_store: StoreId) -> Vec<(StoreId, NodeId)> {
        untracked(|| {
            self.mount_data.with(|map| {
                map.iter()
                    .filter_map(|(&key, sig)| {
                        let matches = sig.with(|m| {
                            m.mount_ref.as_ref().map_or(false, |r| r.source_store == source_store)
                        });
                        if matches { Some(key) } else { None }
                    })
                    .collect()
            })
        })
    }

    /// Canonical pairs of every node in `store_id` whose children are loaded
    /// (untracked): the parents a remote structural change in that store may
    /// have touched. A `storeChanged` notification names only the node, not
    /// its parent, so the client refetches every loaded list of that store.
    pub fn loaded_parents_in(&self, store_id: StoreId) -> Vec<(StoreId, NodeId)> {
        untracked(|| {
            self.children_of.with(|map| {
                map.keys().copied().filter(|(sid, _)| *sid == store_id).collect()
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
        let mut result = Vec::new();
        for &sid in &store_ids {
            let store_info = self.store_data.with(|map| {
                map.get(&sid).map(|sig| sig.with(|s| (s.name.clone(), s.root_node_id)))
            });
            let Some((store_name, root_id)) = store_info else { continue };

            let store_node = TreeNodeData::new(format!("store_{}", sid), &store_name);
            let children = self.build_children_structural(sid, root_id, &[]);
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

            // Empty label — render_node Effects will populate reactively
            let tree_node = TreeNodeData::new(
                format!("node_{}_{}{}", child_store, child_id, suffix),
                "",
            );

            // Crossing a mount node adds it to the path for everything below it.
            let children_data = if is_mount {
                let mut next_path = mount_path.to_vec();
                next_path.push((child_store, child_id));
                self.build_children_structural(child_store, child_id, &next_path)
            } else {
                self.build_children_structural(child_store, child_id, mount_path)
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
