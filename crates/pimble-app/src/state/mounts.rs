//! Mounts in the app's state: each mount node's state and target, the words
//! and dimming its row takes from them, and which mounts show a node.

use super::*;

/// A pending mount operation waiting for a store to be opened.
#[derive(Debug, Clone)]
pub struct PendingMount {
    pub target_store_id: StoreId,
    pub target_parent_id: NodeId,
    pub source_path: String,
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

impl AppStore {
    /// Get the per-mount signal (untracked read of the registry).
    pub fn get_mount_signal(&self, store_id: StoreId, node_id: NodeId) -> Option<Signal<MountInfo>> {
        untracked(|| self.mount_data.with(|map| map.get(&(store_id, node_id)).copied()))
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

    /// Check if a node is a mount point (untracked).
    pub fn is_mount(&self, store_id: StoreId, node_id: NodeId) -> bool {
        untracked(|| {
            self.mount_data.with(|map| {
                map.get(&(store_id, node_id)).map_or(false, |sig| sig.with(|m| m.is_mount))
            })
        })
    }
}
