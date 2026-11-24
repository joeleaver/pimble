//! The stores the app has open or is opening: their signals and order, the
//! saved stores' rows while they open, their indexes, the paths saved, and
//! each store's sync link.

use super::*;

/// A saved store asked to open and not open yet (`AppStore::opening_stores`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpeningStore {
    /// The folder, as the saved list names it.
    pub path: String,
    /// What its row says until it opens: the folder's name less `.pimble`.
    pub name: String,
    /// Why it did not open, once it did not.
    pub error: Option<String>,
}

impl OpeningStore {
    pub fn new(path: &str) -> Self {
        let file = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
        let name = file.strip_suffix(".pimble").map(str::to_string).unwrap_or(file);
        Self { path: path.to_string(), name, error: None }
    }

    /// What its row says beside the name: "opening..." or why not.
    pub fn row_words(&self) -> String {
        match &self.error {
            None => "opening...".to_string(),
            Some(_) => "could not open".to_string(),
        }
    }

    /// The row's tooltip.
    pub fn row_tooltip(&self) -> String {
        match &self.error {
            None => format!("Opening {}.", self.path),
            Some(why) => format!("{} could not be opened: {why}", self.path),
        }
    }
}

/// The tree value of an opening store's row: never a store's or a node's
/// (`parse_tree_value` names neither for it).
pub const OPENING_ROW: &str = "opening_";

impl AppStore {
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
        let rank = crate::persistence::store_rank(&crate::persistence::store_order_key(&store));
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
        // A new store takes its place in the order the person left the
        // stores in (docs/TREE_CONTRACT.md "Reordering stores"): before the
        // first store saved after it; one never placed goes last.
        if untracked(|| self.store_ids.with(|ids| ids.contains(&sid))) {
            return;
        }
        let ranks: Vec<Option<usize>> = untracked(|| {
            self.store_ids.with(|ids| {
                ids.iter()
                    .map(|id| {
                        let key = self.store_data.with(|map| map.get(id).map(|sig| sig.with(crate::persistence::store_order_key)));
                        key.and_then(|k| crate::persistence::store_rank(&k))
                    })
                    .collect()
            })
        });
        let at = rank.and_then(|rank| ranks.iter().position(|r| r.is_none_or(|r| r > rank)));
        self.store_ids.update(|ids| match at {
            Some(at) => ids.insert(at, sid),
            None => ids.push(sid),
        });
    }

    /// Move the stores `moved` (top to bottom) to just after `after`, or to
    /// the top when it is `None`, and save the order the person picked: the
    /// window's open-store list, nothing in any store (docs/TREE_CONTRACT.md
    /// "Reordering stores").
    pub fn move_stores(&self, moved: &[StoreId], after: Option<StoreId>) {
        let mut ids = untracked(|| self.store_ids.get());
        let moving: Vec<StoreId> = moved.iter().copied().filter(|id| ids.contains(id)).collect();
        ids.retain(|id| !moving.contains(id));
        let at = match after {
            Some(anchor) => match ids.iter().position(|&id| id == anchor) {
                Some(at) => at + 1,
                None => ids.len(),
            },
            None => 0,
        };
        ids.splice(at..at, moving);
        self.store_ids.set(ids);
        let keys: Vec<String> = untracked(|| {
            self.store_ids.with(|ids| {
                ids.iter()
                    .filter_map(|id| self.store_data.with(|map| map.get(id).map(|sig| sig.with(crate::persistence::store_order_key))))
                    .collect()
            })
        });
        crate::persistence::save_store_order(&keys);
        self.bump_tree_structure();
    }

    /// Remove a store and all its associated per-entity signals.
    pub fn remove_store(&self, store_id: StoreId) {
        self.store_ids.update(|ids| ids.retain(|&id| id != store_id));
        self.store_data.update(|map| { map.remove(&store_id); });
        self.node_data.update(|map| { map.retain(|(sid, _), _| *sid != store_id); });
        self.children_of.update(|map| { map.retain(|(sid, _), _| *sid != store_id); });
        self.mount_data.update(|map| { map.retain(|(sid, _), _| *sid != store_id); });
        untracked(|| self.live_label.update(|map| { map.retain(|(sid, _), _| *sid != store_id); }));
        self.sync_data.update(|map| { map.remove(&store_id); });
        self.set_owner_offline(store_id, false);
        self.set_waiting(store_id, Vec::new());
        crate::metanodes::store_closed(store_id);
    }

    /// Get the per-store signal (untracked read of the registry).
    pub fn get_store_signal(&self, store_id: StoreId) -> Option<Signal<Store>> {
        untracked(|| self.store_data.with(|map| map.get(&store_id).copied()))
    }

    /// Check if children have been loaded for a parent.
    /// A saved store is asked to open: its row shows at once, "opening...",
    /// unless a store from that folder is open already.
    pub fn begin_opening(&self, path: &str) {
        let open = self.all_store_local_paths().iter().any(|p| p == path);
        if open {
            return;
        }
        let known = untracked(|| self.opening_stores.with(|list| list.iter().any(|o| o.path == path)));
        if known {
            self.opening_stores.update(|list| {
                if let Some(o) = list.iter_mut().find(|o| o.path == path) {
                    o.error = None;
                }
            });
        } else {
            self.opening_stores.update(|list| list.push(OpeningStore::new(path)));
        }
        self.bump_tree_structure();
    }

    /// The store from `path` opened: its opening row goes (the store's own
    /// row takes its place).
    pub fn finish_opening(&self, path: &str) {
        let known = untracked(|| self.opening_stores.with(|list| list.iter().any(|o| o.path == path)));
        if known {
            self.opening_stores.update(|list| list.retain(|o| o.path != path));
            self.bump_tree_structure();
        }
    }

    /// The store from `path` did not open: its row says why.
    pub fn opening_failed(&self, path: &str, why: &str) {
        let known = untracked(|| self.opening_stores.with(|list| list.iter().any(|o| o.path == path)));
        if !known {
            self.opening_stores.update(|list| list.push(OpeningStore::new(path)));
        }
        self.opening_stores.update(|list| {
            if let Some(o) = list.iter_mut().find(|o| o.path == path) {
                o.error = Some(why.to_string());
            }
        });
        self.bump_tree_structure();
    }

    /// The opening store whose row is `value`, if it is one.
    pub fn opening_row(&self, value: &str) -> Option<OpeningStore> {
        let path = value.strip_prefix(OPENING_ROW)?;
        untracked(|| self.opening_stores.with(|list| list.iter().find(|o| o.path == path).cloned()))
    }

    /// A store's search index changed state (docs/STARTUP_CONTRACT.md): its
    /// row's "indexing" words follow the store's own signal.
    pub fn set_index_state(&self, store_id: StoreId, state: pimble_core::IndexState) {
        if let Some(sig) = self.get_store_signal(store_id) {
            if untracked(|| sig.with(|s| s.index != state)) {
                sig.update(|s| s.index = state);
            }
        }
    }

    /// What a search over `stores` (every open store when empty) says above
    /// its results while some of them are still being indexed: "Personal is
    /// still being indexed; results may be incomplete."
    pub fn search_indexing_note(&self, stores: &[StoreId]) -> String {
        // Tracked: the search panel's note follows each store's index.
        let names: Vec<String> = self.store_ids.with(|ids| {
            ids.iter()
                .filter(|id| stores.is_empty() || stores.contains(id))
                .filter_map(|id| self.store_data.with(|map| map.get(id).copied()))
                .filter_map(|sig| sig.with(|s| s.index.is_indexing().then(|| s.name.clone())))
                .collect()
        });
        pimble_core::IndexState::search_note(&names)
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

    /// The open-store list to save: the open stores and the saved ones
    /// still opening, in the saved order (`persistence::store_rank`). With
    /// the stores opening side by side, saving only the open ones as each
    /// opened would forget the rest if Pimble closed mid-start. A store that
    /// could not open is left out, as before (decided in
    /// docs/STARTUP_CONTRACT.md "Built").
    pub fn store_paths_to_save(&self) -> Vec<String> {
        let mut paths = self.all_store_local_paths();
        let opening: Vec<String> = untracked(|| self.opening_stores.with(|list| list.iter().filter(|o| o.error.is_none()).map(|o| o.path.clone()).collect()));
        for path in opening {
            if paths.contains(&path) {
                continue;
            }
            let rank = crate::persistence::store_rank(&path);
            let at = rank.and_then(|rank| paths.iter().position(|p| crate::persistence::store_rank(p).is_none_or(|r| r > rank)));
            match at {
                Some(at) => paths.insert(at, path),
                None => paths.push(path),
            }
        }
        paths
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

    /// Whether a store holds only encrypted blobs (untracked).
    ///
    /// Nothing about mounts, replication or a server-side index applies to one,
    /// so the tree hides those where they would only fail.
    pub fn is_vault(&self, store_id: StoreId) -> bool {
        untracked(|| {
            self.store_data.with(|map| {
                map.get(&store_id)
                    .map_or(false, |sig| sig.with(|s| s.kind == pimble_core::StoreKind::Vault))
            })
        })
    }

    /// Whether a store currently has a remote endpoint linked (untracked).
    pub fn is_linked(&self, store_id: StoreId) -> bool {
        untracked(|| {
            self.sync_data.with(|map| {
                map.get(&store_id).map_or(false, |sig| sig.with(|(remote, _)| remote.is_some()))
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_fixtures::store_with_a_child;

    /// The store row's menu snapshots which end of the relay this device is
    /// ("Stop Sharing from This Computer...", and what a store shared from
    /// here turns off), so sharing from this computer and stopping both have
    /// to change the row's data, and `set_store_relay` says when they did
    /// (docs/RELAY_CONTRACT.md; CLAUDE.md "Hardening", the row data rule).
    /// docs/STARTUP_CONTRACT.md: a saved store asked to open has its row at
    /// once, named from its folder, saying "opening..."; one that fails says
    /// why on the same row (its data carries the error, so the row is drawn
    /// again); one that opens leaves the opening row for its own.
    #[test]
    fn a_saved_store_has_its_row_while_it_opens_and_says_why_if_it_cannot() {
        let (app, _, _) = store_with_a_child();
        app.begin_opening("/home/me/Dropbox/Personal.pimble");
        app.begin_opening("/tmp/notes.pimble"); // open already: no second row
        let rows = untracked(|| app.build_tree_data_structural());
        let values: Vec<&str> = rows.iter().map(|r| r.value.as_str()).collect();
        assert_eq!(values.len(), 2, "{values:?}");
        assert_eq!(rows[1].value, format!("{OPENING_ROW}/home/me/Dropbox/Personal.pimble"));
        let opening = app.opening_row(&rows[1].value).expect("the row is an opening store's");
        assert_eq!(opening.name, "Personal");
        assert_eq!(opening.row_words(), "opening...");
        assert!(parse_tree_value(&rows[1].value).is_none(), "no store or node acts on it");
        assert!(app.store_paths_to_save().contains(&"/home/me/Dropbox/Personal.pimble".to_string()), "a store still opening stays in the saved list");

        app.opening_failed("/home/me/Dropbox/Personal.pimble", "The folder is not there.");
        let failed = untracked(|| app.build_tree_data_structural());
        assert_ne!(rows[1].label, failed[1].label, "the row is drawn again with why");
        let opening = app.opening_row(&failed[1].value).unwrap();
        assert_eq!(opening.row_words(), "could not open");
        assert_eq!(opening.row_tooltip(), "/home/me/Dropbox/Personal.pimble could not be opened: The folder is not there.");
        assert!(!app.store_paths_to_save().contains(&"/home/me/Dropbox/Personal.pimble".to_string()), "one that could not open is not saved");

        // Asked again (a reconnect) it is opening again; once open, its row goes.
        app.begin_opening("/home/me/Dropbox/Personal.pimble");
        assert_eq!(app.opening_row(&failed[1].value).unwrap().error, None);
        app.finish_opening("/home/me/Dropbox/Personal.pimble");
        assert_eq!(untracked(|| app.build_tree_data_structural()).len(), 1);
    }

    /// A store's index state rides its own signal (the row's words follow it
    /// without a rebuild), and a search over it says it is still indexed.
    #[test]
    fn a_store_being_indexed_says_so_on_its_row_and_in_search() {
        let (app, store_id, _) = store_with_a_child();
        assert_eq!(app.search_indexing_note(&[]), "");
        app.set_index_state(store_id, pimble_core::IndexState::Rebuilding { done: 0, total: 484 });
        let words = untracked(|| app.get_store_signal(store_id).unwrap().with(|s| s.index.row_words()));
        assert_eq!(words, "indexing 484 notes");
        assert_eq!(app.search_indexing_note(&[]), "Notes is still being indexed; results may be incomplete.");
        assert_eq!(app.search_indexing_note(&[StoreId::new()]), "", "a search not covering it says nothing");
        app.set_index_state(store_id, pimble_core::IndexState::Ready);
        assert_eq!(app.search_indexing_note(&[store_id]), "");
    }
}
