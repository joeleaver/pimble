//! Internal nodes in the explorer (docs/THEMES_CONTRACT.md section 3.1a):
//! which kinds this window shows, whether a row is left out, and how many a
//! store row hides. Hidden means not shown: the nodes are held, replicate
//! and open by link as any other.

use std::cell::Cell;

use pimble_core::internal::{self, InternalKind};
use pimble_core::{NodeId, StoreId};
use rinch::prelude::*;

use super::AppStore;

impl AppStore {
    /// The internal kinds shown, tracked.
    pub fn internal_kinds_shown(&self) -> Vec<InternalKind> {
        InternalKind::ALL.into_iter().filter(|kind| self.internal_shown[kind.index()].get()).collect()
    }

    /// Whether the explorer leaves `(store_id, node_id)` out: an internal
    /// node of a kind not shown. Tracked on the kinds shown.
    pub fn hides_row(&self, store_id: StoreId, node_id: NodeId) -> bool {
        let Some(node) = self.get_node_signal(store_id, node_id) else { return false };
        let shown = self.internal_kinds_shown();
        node.with(|n| internal::hidden(&n.node_type, &shown))
    }

    /// The kinds this device showed when it was last used, and from now on
    /// a change of them (View > "Show Internal") remembered and the explorer
    /// drawn again.
    pub fn keep_internal_shown(self) {
        for kind in crate::views::internal_shown() {
            self.internal_shown[kind.index()].set(true);
        }
        // The first run only subscribes.
        let subscribed = Cell::new(false);
        let _ = rinch::Effect::new(move || {
            let shown = self.internal_kinds_shown();
            if subscribed.replace(true) {
                untracked(|| {
                    crate::views::set_internal_shown(&shown);
                    self.bump_tree_structure();
                });
            }
        });
    }
}

/// What a store row's tooltip says of the internal nodes it hides ("3
/// themes hidden"), tracked; empty when it hides none. Themes are counted
/// from the store's list of themes, which names every one wherever it sits;
/// a store whose themes nothing has needed yet is not listed, and its row
/// counts none until then.
pub fn hidden_count_text(store: AppStore, store_id: StoreId) -> String {
    let shown = store.internal_kinds_shown();
    let mut parts = Vec::new();
    for kind in InternalKind::ALL {
        if shown.contains(&kind) {
            continue;
        }
        let count = match kind {
            InternalKind::Theme => store.store_themes.with(|all| all.get(&store_id).map_or(0, Vec::len)),
        };
        if count > 0 {
            parts.push(kind.count_text(count));
        }
    }
    parts.join(", ")
}
