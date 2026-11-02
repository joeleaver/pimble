//! The app's [`TreeView`]: what `pimble_core::plan` judges, read from
//! `AppStore`'s signals. Every read is untracked, as the code the judgements
//! came from read them: a judgement is asked from a callback (a drop, a key,
//! a menu item) or while a row renders, and must not subscribe either to the
//! signals it reads.

use pimble_core::plan::{self, StoreFacts, TreeView};
use pimble_core::{NodeId, StoreAccess, StoreId};
use rinch::prelude::untracked;

use crate::state::{parse_tree_value, AppStore};

/// `AppStore` seen as a tree ([`AppStore::view`]).
#[derive(Clone, Copy)]
pub struct AppView(pub AppStore);

impl AppStore {
    /// This app's state as the tree `pimble_core::plan` judges.
    pub fn view(self) -> AppView {
        AppView(self)
    }
}

impl TreeView for AppView {
    fn store(&self, store_id: StoreId) -> Option<StoreFacts> {
        let store = self.0;
        let sig = store.get_store_signal(store_id)?;
        Some(untracked(|| sig.with(|s| StoreFacts::of(s, store.is_linked(store_id)))))
    }

    fn store_access(&self, store_id: StoreId) -> Option<StoreAccess> {
        self.0.get_store_signal(store_id).map(|sig| untracked(|| sig.with(|s| s.access)))
    }

    fn parent(&self, store_id: StoreId, node_id: NodeId) -> Option<NodeId> {
        self.0.cached_parent_id(store_id, node_id)
    }

    fn children(&self, store_id: StoreId, node_id: NodeId) -> Option<Vec<NodeId>> {
        let sig = self.0.get_children_signal(store_id, node_id)?;
        Some(untracked(|| sig.with(|c| c.iter().map(|(_, n)| *n).collect())))
    }

    fn is_mount(&self, store_id: StoreId, node_id: NodeId) -> bool {
        self.0.is_mount(store_id, node_id)
    }

    /// What `getMountState` last said the mount points at, else the mount
    /// node's own `MountRef` (both are kept, and either can arrive first).
    fn mount_source(&self, store_id: StoreId, node_id: NodeId) -> Option<(StoreId, NodeId)> {
        let store = self.0;
        let from_state = store
            .get_mount_signal(store_id, node_id)
            .and_then(|sig| untracked(|| sig.with(|m| m.mount_ref.as_ref().map(|r| (r.source_store, r.source_node)))));
        from_state.or_else(|| {
            store
                .get_node_signal(store_id, node_id)
                .and_then(|sig| untracked(|| sig.with(|n| n.mount_ref().map(|r| (r.source_store, r.source_node)))))
        })
    }

    fn is_shared(&self, store_id: StoreId, node_id: NodeId) -> bool {
        self.0.is_shared(store_id, node_id)
    }

    fn held_access(&self, store_id: StoreId, node_id: NodeId) -> Option<StoreAccess> {
        self.0.get_node_signal(store_id, node_id).map(|sig| untracked(|| sig.with(|n| n.access)))
    }

    fn label(&self, store_id: StoreId, node_id: NodeId) -> String {
        self.0.display_label(store_id, node_id)
    }
}

/// Where a new node made on the tree row `row` goes
/// ([`plan::create_parent`]), or `None` after saying why not. Every way of
/// making something on a row comes here: the row menus, the keyboard, the
/// toolbar's "+", pictures dropped or pasted on a row, "Paste Mount Here".
pub(crate) fn create_parent_of_row(store: AppStore, row: &str) -> Option<(StoreId, NodeId)> {
    let (store_id, node_id) = parse_tree_value(row)?;
    match plan::create_parent(&store.view(), store_id, node_id) {
        Ok(parent) => Some(parent),
        Err(refusal) => {
            crate::events::show_notice(store, refusal.sentence().to_string());
            None
        }
    }
}

/// Whether the nodes of the rows `values` may all be deleted
/// ([`plan::can_delete`]), answered with their canonical pairs; `None` after
/// saying why not, and quietly when no row is a node.
pub(crate) fn deletable(store: AppStore, values: &[String]) -> Option<Vec<(StoreId, NodeId)>> {
    let nodes: Vec<(StoreId, NodeId)> = values
        .iter()
        .filter_map(|v| match parse_tree_value(v) {
            Some((s, Some(n))) => Some((s, n)),
            _ => None,
        })
        .collect();
    if nodes.is_empty() {
        return None;
    }
    match plan::can_delete(&store.view(), &nodes) {
        Ok(()) => Some(nodes),
        Err(refusal) => {
            crate::events::show_notice(store, refusal.sentence().to_string());
            None
        }
    }
}
