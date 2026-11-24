//! The nodes the app holds: each node's signal and live label, each parent's
//! children and whether they are loaded, asked for or slow to come.

use super::*;

/// How long a branch's children may take before its row shows a spinner,
/// so a fast load does not flicker (docs/STARTUP_CONTRACT.md).
pub const SLOW_CHILDREN_MS: u64 = 150;

impl AppStore {
    /// Remove a node and its per-entity signals (node_data, mount_data, children_of).
    pub fn remove_node(&self, store_id: StoreId, node_id: NodeId) {
        let key = (store_id, node_id);
        self.node_data.update(|map| { map.remove(&key); });
        self.mount_data.update(|map| { map.remove(&key); });
        self.children_of.update(|map| { map.remove(&key); });
        untracked(|| self.live_label.update(|map| { map.remove(&key); }));
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

        // Every pane that holds it, not only the focused one
        // (docs/SPLIT_VIEW_CONTRACT.md "What the person sees").
        for pane in self.panes_showing(store_id, node_id) {
            self.clear_pane(pane);
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
        // locally-computed label override for it (see `live_label`). Only a
        // row that has an entry hears about it, and only if it held a label.
        if let Some(live) = untracked(|| self.live_label.with(|m| m.get(&key).copied())) {
            live.set_if_changed(None);
        }
    }

    /// `key`'s locally-computed tree label (see `live_label`), created empty on
    /// first use. Reading the returned signal subscribes to this node's label
    /// alone; nothing ever subscribes to the map.
    pub fn live_label_signal(&self, key: (StoreId, NodeId)) -> Signal<Option<String>> {
        untracked(|| {
            if let Some(sig) = self.live_label.with(|m| m.get(&key).copied()) {
                return sig;
            }
            // Detached from whichever scope is current: the first caller is
            // usually a tree row's render, and a row is disposed on every
            // re-render while the store keeps the entry (which then panicked
            // on the next read). Like the `node_data` signals, it lives as
            // long as the app; one per node that has had a row or an edit.
            let sig = Signal::new(None).leak();
            self.live_label.update(|m| { m.insert(key, sig); });
            sig
        })
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

    /// Get the children signal for a parent (untracked read of the registry).
    pub fn get_children_signal(&self, store_id: StoreId, node_id: NodeId) -> Option<Signal<Vec<(StoreId, NodeId)>>> {
        untracked(|| self.children_of.with(|map| map.get(&(store_id, node_id)).copied()))
    }

    /// Children of `(store_id, node_id)` were asked for: after
    /// `SLOW_CHILDREN_MS` without them, its row shows a spinner.
    pub fn children_asked(&self, store_id: StoreId, node_id: NodeId) {
        let new = untracked(|| self.children_pending.with(|set| !set.contains(&(store_id, node_id))));
        if new {
            self.children_pending.update(|set| {
                set.insert((store_id, node_id));
            });
        }
        let store = *self;
        rinch::core::reactive::unowned(|| {
            let _ = rinch::prelude::set_timeout(SLOW_CHILDREN_MS as u32, move || store.children_slow(store_id, node_id));
        });
    }

    /// `SLOW_CHILDREN_MS` passed since children of `(store_id, node_id)`
    /// were asked for: if they have not arrived, its row shows a spinner.
    pub fn children_slow(&self, store_id: StoreId, node_id: NodeId) {
        if self.has_children_loaded(store_id, node_id) {
            return;
        }
        let new = untracked(|| self.slow_children.with(|set| !set.contains(&(store_id, node_id))));
        if new {
            self.slow_children.update(|set| {
                set.insert((store_id, node_id));
            });
            self.bump_tree_structure();
        }
    }

    /// Children of `(store_id, node_id)` arrived: no spinner.
    pub fn children_arrived(&self, store_id: StoreId, node_id: NodeId) {
        let pending = untracked(|| self.children_pending.with(|set| set.contains(&(store_id, node_id))));
        if pending {
            self.children_pending.update(|set| {
                set.remove(&(store_id, node_id));
            });
        }
        let was = untracked(|| self.slow_children.with(|set| set.contains(&(store_id, node_id))));
        if was {
            self.slow_children.update(|set| {
                set.remove(&(store_id, node_id));
            });
        }
    }

    /// Whether children of `(store_id, node_id)` were asked for and are on
    /// their way.
    pub fn children_on_their_way(&self, store_id: StoreId, node_id: NodeId) -> bool {
        untracked(|| self.children_pending.with(|set| set.contains(&(store_id, node_id))))
    }

    /// Whether any branch's children are on their way.
    pub fn children_pending_any(&self) -> bool {
        untracked(|| self.children_pending.with(|set| !set.is_empty()))
    }

    /// Whether `(store_id, node_id)`'s row shows the children spinner.
    pub fn children_loading(&self, store_id: StoreId, node_id: NodeId) -> bool {
        untracked(|| self.slow_children.with(|set| set.contains(&(store_id, node_id)))) && !self.has_children_loaded(store_id, node_id)
    }

    pub fn has_children_loaded(&self, store_id: StoreId, node_id: NodeId) -> bool {
        untracked(|| self.children_of.with(|map| map.contains_key(&(store_id, node_id))))
    }

    /// Whether a node's row is currently expanded in the tree (untracked).
    pub fn is_expanded(&self, store_id: StoreId, node_id: NodeId) -> bool {
        untracked(|| self.expanded.with(|e| e.contains(&(store_id, node_id))))
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_fixtures::store_with_a_child;

    /// A tree row's label follows its own node's live label and nothing
    /// else: typing into one node, or fresh content arriving for it, must not
    /// re-run the label of every other visible row.
    #[test]
    fn a_live_label_wakes_only_its_own_row() {
        use std::cell::Cell;
        use std::rc::Rc;

        let (app, store_id, child_id) = store_with_a_child();
        let other = Node::document("Other");
        let other_id = other.id;
        app.upsert_node(store_id, other);

        let mine = app.live_label_signal((store_id, child_id));
        let theirs = app.live_label_signal((store_id, other_id));
        let runs = Rc::new(Cell::new(0));
        let _effect = {
            let runs = runs.clone();
            rinch::Effect::new(move || {
                let _ = theirs.get();
                runs.set(runs.get() + 1);
            })
        };
        let before = runs.get();

        mine.set_if_changed(Some("Typed in the editor".into()));
        let child = untracked(|| app.get_node_signal(store_id, child_id).unwrap().get());
        app.upsert_node(store_id, child);
        assert_eq!(mine.get(), None, "authoritative content clears the node's own live label");
        assert_eq!(runs.get(), before, "another row's label is untouched");

        // The same node's signal every time, so a row keeps hearing its label.
        app.live_label_signal((store_id, other_id)).set_if_changed(Some("Other, typed".into()));
        assert_eq!(runs.get(), before + 1);
    }

    /// The first caller of `live_label_signal` is usually a tree row's render,
    /// and rows are disposed on every tree re-render: the signal must outlive
    /// the row that created it, or the next row to read it panics.
    #[test]
    fn a_live_label_outlives_the_row_that_asked_for_it_first() {
        let (app, store_id, child_id) = store_with_a_child();
        let row = rinch::Scope::new();
        let first = row.run(|| app.live_label_signal((store_id, child_id)));
        row.dispose();

        assert!(first.is_alive());
        app.live_label_signal((store_id, child_id)).set_if_changed(Some("Typed".into()));
        assert_eq!(first.get().as_deref(), Some("Typed"));
    }

    /// A branch whose children are slow (past `SLOW_CHILDREN_MS`) is drawn
    /// with no placeholder child, so rinch draws the spacer the spinner sits
    /// on, and its data says so; when they arrive it is an ordinary row.
    #[test]
    fn a_branch_slow_to_load_shows_a_spinner_until_its_children_arrive() {
        let (app, store_id, child_id) = store_with_a_child();
        let mut child = untracked(|| app.get_node_signal(store_id, child_id).unwrap().get());
        let grandchild = Node::document("Soup");
        child.children = vec![grandchild.id];
        app.upsert_node(store_id, child);
        let row = |app: &AppStore| untracked(|| app.build_tree_data_structural())[0].children[0].clone();
        let before = row(&app);
        assert_eq!(before.children.len(), 1, "an unloaded branch has its placeholder");

        app.children_slow(store_id, child_id);
        assert!(app.children_loading(store_id, child_id));
        let loading = row(&app);
        assert!(loading.children.is_empty(), "no placeholder while the spinner shows");
        assert!(loading.label.ends_with("|loading"), "{}", loading.label);

        app.set_children(store_id, child_id, vec![(store_id, grandchild.id)]);
        app.children_arrived(store_id, child_id);
        assert!(!app.children_loading(store_id, child_id));
        let loaded = row(&app);
        assert!(!loaded.label.contains("loading"));

        // Children already there are never slow.
        app.children_slow(store_id, child_id);
        assert!(!app.children_loading(store_id, child_id));
    }
}
