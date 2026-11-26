//! The explorer's rows: the `TreeNodeData` the tree is built from (each row's
//! value, and the label carrying what its menu snapshots), node icons and
//! labels, and reading a row's value back.

use super::*;
use super::found::FOUND_ROWS;

/// Extract plain-text content from node content bytes, for tree previews / labels.
///
/// `content` is a yrs snapshot (or empty). Blocks are joined by `\n`; `""` for
/// empty or unreadable content, in which case the caller falls back to the
/// node's title.
pub fn get_node_content_text(content: &[u8]) -> String {
    pimble_crdt::NodeDoc::text_of(content)
}

/// What a node is called (`pimble_core::plan::display_label`, the one rule),
/// with the content decode the rule asks for, which `pimble-core` cannot do.
pub fn display_label_from_node(node: &Node) -> String {
    pimble_core::plan::display_label(node, get_node_content_text)
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

impl AppStore {
    /// Everything a node's row snapshots from the node itself at render time:
    /// its custom icon, colour and row colour, and whether it is shared. Part of the row's
    /// `TreeNodeData` label so a change re-renders the row (rinch #714), and
    /// nothing reads it back.
    pub(super) fn row_snapshot(&self, store_id: StoreId, node_id: NodeId) -> String {
        let Some(sig) = self.node_data.with(|map| map.get(&(store_id, node_id)).copied()) else {
            return String::new();
        };
        sig.with(|n| {
            format!(
                "{}|{}|{}|{}|{}|{}",
                n.metadata.icon().unwrap_or(""),
                n.metadata.color().unwrap_or(""),
                n.metadata.background().unwrap_or(""),
                if n.metadata.share().is_some() { "shared" } else { "" },
                // A query metanode looks like one (its icon and
                // violet), and stops when its source changes.
                n.metadata.metanode.as_ref().map_or(String::new(), |m| {
                    format!("{}{}|{}", m.viewer, if m.is_query() { " query" } else { "" }, self.viewer_icon(&m.viewer).unwrap_or_default())
                }),
                // A kanban's column looks like one once the plugin
                // providing its child viewer is known.
                self.child_viewer(store_id, n).and_then(|c| c.icon).unwrap_or_default(),
            )
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
        FOUND_ROWS.with(|rows| rows.borrow_mut().clear());
        let mut result = Vec::new();
        // The saved stores still opening, each a row in its saved place
        // (docs/STARTUP_CONTRACT.md); the label carries what the row draws.
        let mut opening: Vec<(Option<usize>, TreeNodeData)> = self.opening_stores.with(|list| {
            list.iter()
                .map(|o| {
                    let rank = crate::persistence::store_rank(&o.path);
                    (rank, TreeNodeData::new(format!("{OPENING_ROW}{}", o.path), format!("{}|{}", o.name, o.error.clone().unwrap_or_default())))
                })
                .collect()
        });
        for &sid in &store_ids {
            // An opening row placed before this store goes first.
            let store_rank = self.store_data.with(|map| map.get(&sid).map(|sig| sig.with(crate::persistence::store_order_key))).and_then(|k| crate::persistence::store_rank(&k));
            while let Some(i) = opening.iter().position(|(r, _)| matches!((r, store_rank), (Some(r), Some(sr)) if *r < sr) || (r.is_some() && store_rank.is_none())) {
                result.push(opening.remove(i).1);
            }
            let store_info = self.store_data.with(|map| {
                map.get(&sid).map(|sig| sig.with(|s| (s.name.clone(), s.root_node_id, s.roots.clone(), s.ended_roots.clone())))
            });
            let Some((store_name, root_id, roots, ended_roots)) = store_info else { continue };

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
            // A store row's icon and colour come from its root node's
            // metadata — a partial replica's row has no root of its own to
            // take them from (see `store_row_node`).
            let root_appearance =
                if roots.is_empty() { self.row_snapshot(sid, root_id) } else { String::new() };
            // What this device may change here, and who shared it: the row's
            // menu snapshots both (docs/NODE_DOCUMENT_CONTRACT.md section 5),
            // so a change has to change the data. The scope roots are in there
            // too: a second share of the same store extends this replica, and
            // the row has to rebuild with the new root under it.
            let access = self.store_data.with(|map| {
                map.get(&sid).map(|sig| sig.with(|s| format!("{:?}{:?}", s.access, s.shared_by))).unwrap_or_default()
            });
            // Which end of the relay this device is for the store
            // (docs/RELAY_CONTRACT.md): the row's menu snapshots it for "Stop
            // Sharing from This Computer..." and for what a store shared from
            // here turns off, so sharing from this computer, or stopping,
            // has to change the data.
            let relay = self
                .store_data
                .with(|map| map.get(&sid).map_or(pimble_core::RelaySide::None, |sig| sig.with(|s| s.relay)));
            // The shares that have ended are in there as well: the last one
            // to end leaves the row with no child to change, and its menu
            // snapshots that every share has ended (everything that changes
            // anything goes off, "Remove Replica..." stays).
            let roots_key: Vec<String> = roots.iter().map(|r| r.to_string()).collect();
            let ended_key: Vec<String> = ended_roots.iter().map(|r| r.to_string()).collect();
            // A whole store's root children slow to come: the store row
            // draws the spinner where its chevron goes (docs/STARTUP_CONTRACT.md).
            let loading = roots.is_empty() && self.children_loading(sid, root_id);
            let store_node = TreeNodeData::new(
                format!("store_{}", sid),
                format!(
                    "{store_name} (linked: {linked}, relay: {relay:?}, paste: {paste}, {access}, {root_appearance}, roots: {}, ended: {}{})",
                    roots_key.join(","),
                    ended_key.join(","),
                    if loading { ", loading" } else { "" }
                ),
            );
            // A whole store's row stands for its root: the root's children hang
            // straight off it, as they always have. A partial replica's row
            // stands for someone else's store, of which this device holds only
            // the shared subtrees — so each scope root is a row of its own
            // (docs/NODE_DOCUMENT_CONTRACT.md section 5, "The recipient's
            // replica"). Its tree value is the ordinary `node_{store}_{node}`,
            // so opening, renaming and the context menu need no special case.
            // A share that has ended is not shown: its root is still one of
            // the replica's, and is no row (`Store::shown_roots`). When every
            // share has ended the row has nothing under it, and so has a
            // whole replica whose own root has (the account holds the store
            // no more).
            let children = if roots.is_empty() {
                if ended_roots.contains(&root_id) {
                    Vec::new()
                } else {
                    self.build_children_structural(sid, root_id, paste, &[], 0)
                }
            } else {
                roots
                    .iter()
                    .filter(|root| !ended_roots.contains(root))
                    .map(|&root| self.structural_node(sid, root, paste, &[], 0, None))
                    .collect()
            };
            if children.is_empty() {
                result.push(store_node);
            } else {
                result.push(store_node.with_children(children));
            }
        }
        result.extend(opening.into_iter().map(|(_, row)| row));
        result
    }

    /// Build structural children recursively (called from untracked context).
    ///
    /// `mount_path` accumulates the canonical `(mount_store, mount_node)` pair
    /// of every mount node crossed to reach this level, outermost first
    /// (decision 7) — empty for a node reached directly from its own store's
    /// root. It disambiguates the same canonical node appearing in several
    /// places, since rinch's Tree keys expansion and selection by value string.
    /// A query metanode's finds are reached through it the same way
    /// (docs/QUERY_METANODES_CONTRACT.md section 4), and `finds_crossed`
    /// counts the finds crossed to get here: a found metanode shows its own
    /// finds, one level down only.
    fn build_children_structural(
        &self,
        store_id: StoreId,
        parent_id: NodeId,
        paste: bool,
        mount_path: &[(StoreId, NodeId)],
        finds_crossed: u8,
    ) -> Vec<TreeNodeData> {
        let children = self.children_of.with(|map| {
            map.get(&(store_id, parent_id)).map(|sig| sig.get())
        });
        let Some(children) = children else { return Vec::new() };

        children
            .iter()
            .map(|&(child_store, child_id)| self.structural_node(child_store, child_id, paste, mount_path, finds_crossed, None))
            .collect()
    }

    /// One row: the node itself plus whatever hangs below it. Used for every
    /// child of a loaded parent, and for each scope root of a partial replica,
    /// which is a row under the store row rather than a child of anything
    /// (see `build_tree_data_structural`), and for each node a query metanode
    /// found (`found_in`: the metanode and its row's value), whose own path
    /// already ends with the metanode.
    fn structural_node(
        &self,
        child_store: StoreId,
        child_id: NodeId,
        paste: bool,
        mount_path: &[(StoreId, NodeId)],
        finds_crossed: u8,
        found_in: Option<(&Found, (StoreId, NodeId), &str)>,
    ) -> TreeNodeData {
        let suffix = mount_path_suffix(mount_path);
        let value = format!("node_{}_{}{}", child_store, child_id, suffix);
        let is_mount = self.mount_data.with(|map| {
            map.get(&(child_store, child_id)).map_or(false, |sig| sig.with(|m| m.is_mount))
        });

        // The renderer never reads this label (render_node Effects draw
        // it reactively); it only carries what the row snapshots at render
        // time — the "Paste Mount Here" state, the node's custom icon and
        // colour, its share marker, and what this device may change of the
        // node (`node_access`: its own store's access, then the server's
        // judgement of this node, so a reader's root beside an editor's
        // disables its own rows and no others) — so the row re-renders when
        // any of them changes (see `build_tree_data_structural`). A mount
        // row's "New Node" goes to the mount's source, so that node's access
        // rides along too. "Share..." also snapshots whether the node's store
        // reached this device as someone else's share (`share_item_disabled`),
        // which the access does not say: an editor of a share and an owner
        // both read as `Full`. A found row's menu and look snapshot that it
        // is found, and the sentence its refused items say.
        let appearance = self.row_snapshot(child_store, child_id);
        let access = format!("{:?}", self.node_access(child_store, child_id));
        let held_as_share = self
            .store_data
            .with(|map| map.get(&child_store).map_or(false, |sig| sig.with(|s| s.shared_by.is_some())));
        let source_access = self
            .mount_data
            .with(|map| map.get(&(child_store, child_id)).and_then(|sig| sig.with(|m| m.mount_ref.as_ref().map(|r| (r.source_store, r.source_node)))))
            .map(|(source_store, source_node)| format!("{:?}", self.node_access(source_store, source_node)))
            .unwrap_or_default();
        let found_note = found_in.map(|(found, _, _)| format!("|found: {}", found.refusal)).unwrap_or_default();
        // A note out of step says so on its row (docs/CONVERGENCE_CONTRACT.md
        // section 4), snapshotted like everything else the row draws.
        let out_of_step = crate::out_of_step::row_snapshot(self.out_of_step_of(child_store, child_id).as_ref());
        if let Some((found, metanode, metanode_row)) = found_in {
            FOUND_ROWS.with(|rows| {
                rows.borrow_mut().insert(
                    value.clone(),
                    FoundRow { metanode, metanode_row: metanode_row.to_string(), lives_in: found.lives_in.clone(), refusal: found.refusal.clone() },
                )
            });
        }
        // Children asked for and slow to come: the row draws a spinner where
        // its chevron goes (docs/STARTUP_CONTRACT.md "Children loading"),
        // and has no placeholder child meanwhile, so rinch draws the spacer
        // the spinner sits on.
        let loading = self.children_loading(child_store, child_id);
        let tree_node = TreeNodeData::new(
            value.clone(),
            format!(
                "{}|{}{}|{}|{}{}{}{}",
                if paste { "paste" } else { "" },
                access,
                source_access,
                if held_as_share { "theirs" } else { "" },
                appearance,
                found_note,
                out_of_step,
                if loading { "|loading" } else { "" }
            ),
        );

        // Crossing a mount node adds it to the path for everything below it;
        // so does a found row, whose own children are its, not the
        // metanode's (and a node a metanode finds can be its own child
        // elsewhere).
        let below_path = if is_mount || found_in.is_some() {
            let mut next_path = mount_path.to_vec();
            next_path.push((child_store, child_id));
            next_path
        } else {
            mount_path.to_vec()
        };
        let mut children_data = self.build_children_structural(child_store, child_id, paste, &below_path, finds_crossed);
        // A query metanode's finds, after its own children: reached through
        // the metanode, so each is its own row wherever else it shows. A
        // found metanode shows its own finds, and no deeper.
        let finds = if finds_crossed <= 1 { self.finds.with(|f| f.get(&(child_store, child_id)).cloned()) } else { None };
        if let Some(finds) = finds {
            let mut through = below_path.clone();
            if through.last() != Some(&(child_store, child_id)) {
                through.push((child_store, child_id));
            }
            for found in &finds {
                children_data.push(self.structural_node(
                    found.store_id,
                    found.node_id,
                    paste,
                    &through,
                    finds_crossed + 1,
                    Some((found, (child_store, child_id), &value)),
                ));
            }
        }
        let has_children = !children_data.is_empty();
        let has_loaded_children = self.children_of.with(|map| {
            map.contains_key(&(child_store, child_id))
        });

        // Check if the node itself reports having children (from its
        // children list) even if we haven't fetched them yet. This
        // lets the tree show an expand chevron for unfetched subtrees. A
        // query metanode has finds to show whether or not it has children.
        let node_reports_children = !has_loaded_children && self.node_data.with(|map| {
            map.get(&(child_store, child_id))
                .map_or(false, |sig| sig.with(|n| !n.children.is_empty() || n.metadata.metanode.as_ref().is_some_and(|m| m.is_query())))
        });

        if has_children {
            tree_node.with_children(children_data)
        } else if loading {
            tree_node
        } else if is_mount && !has_loaded_children {
            let placeholder = TreeNodeData::new(
                format!("mount_loading_{}_{}{}", child_store, child_id, suffix),
                "Loading...",
            );
            tree_node.with_children(vec![placeholder])
        } else if node_reports_children {
            // Node has children we haven't fetched yet — show a
            // placeholder so the tree renders an expand chevron.
            let placeholder = TreeNodeData::new(
                format!("placeholder_{}_{}{}", child_store, child_id, suffix),
                "",
            );
            tree_node.with_children(vec![placeholder])
        } else {
            tree_node
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_fixtures::store_with_a_child;
    use pimble_core::StoreAccess;

    /// A metanode with no icon of its own shows its viewer's, as its plugin's
    /// recipe makes them (docs/METANODES_CONTRACT.md); a chosen icon wins, a
    /// viewer no plugin offers falls back to the type's, and the row's data
    /// changes when the plugins arrive.
    #[test]
    fn a_metanode_shows_its_viewers_icon_unless_it_has_its_own() {
        let (app, store_id, child_id) = store_with_a_child();
        let mut node = Node::document("Board");
        node.metadata.metanode = pimble_core::metanode::Metanode::from_stored(Some("kanban"), Some("children 2"));
        assert_eq!(app.node_icon(store_id, &node), "file");
        let before = app.row_snapshot(store_id, child_id);
        app.recipes.set(vec![PluginRecipe {
            plugin: "kanban".into(),
            recipe: pimble_plugin_abi::RecipeDecl {
                id: "new-kanban".into(),
                label: "New Kanban".into(),
                title: "Kanban".into(),
                icon: Some("layout-kanban".into()),
                viewer: "kanban".into(),
                depth: 2,
                children: Vec::new(),
            },
        }]);
        assert_eq!(app.node_icon(store_id, &node), "layout-kanban");
        node.metadata.set_icon(Some("star".into()));
        assert_eq!(app.node_icon(store_id, &node), "star");
        let mut other = Node::document("Gallery");
        other.metadata.metanode = pimble_core::metanode::Metanode::from_stored(Some("gallery"), None);
        assert_eq!(app.node_icon(store_id, &other), "file");
        // The row of a kanban metanode re-renders once the kanban's recipe is known.
        let mut board = Node::document("Recipes");
        board.id = child_id;
        board.metadata.metanode = pimble_core::metanode::Metanode::from_stored(Some("kanban"), None);
        app.upsert_node(store_id, board);
        let with = app.row_snapshot(store_id, child_id);
        // The last segment is the child viewer's icon: a kanban of its own has none.
        assert!(with.ends_with("kanban|layout-kanban|"), "{with}");
        assert_ne!(before, with);
    }

    /// rinch re-renders a tree row only when its `TreeNodeData` changes, and a
    /// row's menu snapshots the store's access and the node's share marker at
    /// render time — so both have to be in the data
    /// (docs/NODE_DOCUMENT_CONTRACT.md section 5, CLAUDE.md "Hardening").
    #[test]
    fn row_data_carries_access_and_the_share_marker() {
        let (app, store_id, child_id) = store_with_a_child();
        let before = untracked(|| app.build_tree_data_structural());

        // A store shared read-only: every row that could write the tree has
        // to re-render with its items disabled.
        if let Some(sig) = app.get_store_signal(store_id) {
            sig.update(|s| {
                s.access = StoreAccess::Read;
                s.shared_by = Some("ann@example.com".to_string());
            });
        }
        let after_access = untracked(|| app.build_tree_data_structural());
        assert_ne!(before[0].label, after_access[0].label, "the store row never re-renders");
        assert_ne!(
            before[0].children[0].label, after_access[0].children[0].label,
            "the node row never re-renders"
        );

        // A marker appearing is the badge appearing.
        if let Some(sig) = app.get_node_signal(store_id, child_id) {
            sig.update(|n| {
                n.metadata.set_share(Some(&pimble_core::ShareMarker {
                    v: pimble_core::ShareMarker::VERSION,
                    key_id: uuid::Uuid::new_v4(),
                    url: "https://pimble.app".to_string(),
                    name: "Recipes".to_string(),
                }));
            });
        }
        let after_share = untracked(|| app.build_tree_data_structural());
        assert_ne!(
            after_access[0].children[0].label, after_share[0].children[0].label,
            "a shared node's row never re-renders"
        );
        assert!(app.is_shared(store_id, child_id));
    }

    #[test]
    fn store_row_data_carries_the_relay_side() {
        let (app, store_id, _) = store_with_a_child();
        assert_eq!(app.store_relay(store_id), pimble_core::RelaySide::None);
        let before = untracked(|| app.build_tree_data_structural());

        assert!(app.set_store_relay(store_id, pimble_core::RelaySide::Owner));
        assert!(!app.set_store_relay(store_id, pimble_core::RelaySide::Owner), "an answer that changes nothing rebuilt the tree");
        assert_eq!(app.store_relay(store_id), pimble_core::RelaySide::Owner);
        let relayed = untracked(|| app.build_tree_data_structural());
        assert_ne!(before[0].label, relayed[0].label, "the store row never re-renders");

        assert!(app.set_store_relay(store_id, pimble_core::RelaySide::None));
        let stopped = untracked(|| app.build_tree_data_structural());
        assert_eq!(before[0].label, stopped[0].label);
    }

    /// A row's menu snapshots what may be done with its node at render time,
    /// so the node's own access has to be in the row's data: when the server
    /// answers `read` for one node, that row re-renders with its items
    /// disabled and its neighbour's does not change at all.
    #[test]
    fn row_data_carries_the_nodes_own_access() {
        let (app, store_id, child_id) = store_with_a_child();
        let sibling = Node::document("Sibling");
        let sibling_id = sibling.id;
        let root = app.root_node_id(store_id).unwrap();
        app.upsert_node(store_id, sibling);
        app.set_children(store_id, root, vec![(store_id, child_id), (store_id, sibling_id)]);
        let before = untracked(|| app.build_tree_data_structural());

        if let Some(sig) = app.get_node_signal(store_id, child_id) {
            sig.update(|n| n.access = StoreAccess::Read);
        }
        let after = untracked(|| app.build_tree_data_structural());
        assert_ne!(before[0].children[0].label, after[0].children[0].label, "the read-only node's row never re-renders");
        assert_eq!(before[0].children[1].label, after[0].children[1].label, "its sibling has nothing to re-render for");
        assert_eq!(before[0].label, after[0].label, "nor has the store row: the store's access did not change");

        // And back, when the owner makes the account an editor again.
        if let Some(sig) = app.get_node_signal(store_id, child_id) {
            sig.update(|n| n.access = StoreAccess::Full);
        }
        let again = untracked(|| app.build_tree_data_structural());
        assert_eq!(before[0].children[0].label, again[0].children[0].label);
    }

    /// A node row's "Share..." snapshots whether its store is someone else's,
    /// and an editor of a share reads as `Full` like an owner does, so the
    /// access in the row's data does not say it: `shared_by` has to be there
    /// itself, or the item never goes off (rinch re-renders a row only when
    /// its `TreeNodeData` changes).
    #[test]
    fn row_data_carries_whose_store_it_is() {
        let (app, store_id, _) = store_with_a_child();
        let before = untracked(|| app.build_tree_data_structural());
        if let Some(sig) = app.get_store_signal(store_id) {
            sig.update(|s| s.shared_by = Some("ann@example.com".to_string()));
        }
        assert_eq!(app.store_access(store_id), StoreAccess::Full, "an editor's share");
        let after = untracked(|| app.build_tree_data_structural());
        assert_ne!(before[0].label, after[0].label, "the store row never re-renders");
        assert_ne!(before[0].children[0].label, after[0].children[0].label, "the node row never re-renders");
    }

    /// A partial replica of someone else's store shows each shared root as a
    /// row of its own under the store row, addressed like any other node, and
    /// with the root's own children below it — the store row stands for the
    /// owner's store, not for the shared folder
    /// (docs/NODE_DOCUMENT_CONTRACT.md section 5, "The recipient's replica").
    #[test]
    fn a_partial_replica_shows_every_shared_root() {
        let app = AppStore::new();
        let mut store = Store::new_local("Ann's notes", "/tmp/anns.pimble".into());
        let recipes = Node::folder("Recipes");
        let trips = Node::folder("Trips");
        let pasta = Node::document("Pasta");
        store.root_node_id = recipes.id;
        store.roots = vec![recipes.id, trips.id];
        store.shared_by = Some("ann@example.com".to_string());
        let store_id = store.id;
        app.upsert_store(store);
        app.upsert_node(store_id, recipes.clone());
        app.upsert_node(store_id, trips.clone());
        app.upsert_node(store_id, pasta.clone());
        app.set_children(store_id, recipes.id, vec![(store_id, pasta.id)]);

        let data = untracked(|| app.build_tree_data_structural());
        let store_row = &data[0];
        assert_eq!(store_row.value, format!("store_{store_id}"));
        assert_eq!(store_row.children.len(), 2, "both shared roots are rows under the store row");
        assert_eq!(store_row.children[0].value, format!("node_{store_id}_{}", recipes.id));
        assert_eq!(store_row.children[1].value, format!("node_{store_id}_{}", trips.id));
        // The root's own children hang off its row, not off the store row.
        assert_eq!(store_row.children[0].children[0].value, format!("node_{store_id}_{}", pasta.id));
        assert!(store_row.children[1].children.is_empty());
        assert_eq!(app.shown_roots(store_id), vec![recipes.id, trips.id]);
        assert!(app.is_partial_replica(store_id));
        // The store row stands for someone else's store, whose root this
        // device does not hold: it takes no node's icon, colour or badge.
        assert_eq!(app.store_row_node(store_id), None);

        // A second share of the same store extends the replica: the row's data
        // has to change, or rinch re-renders nothing and the new root never
        // appears.
        let before = store_row.label.clone();
        let walks = Node::folder("Walks");
        app.upsert_node(store_id, walks.clone());
        if let Some(sig) = app.get_store_signal(store_id) {
            sig.update(|s| s.roots.push(walks.id));
        }
        let after = untracked(|| app.build_tree_data_structural());
        assert_ne!(before, after[0].label, "the store row never re-renders with the new root");
        assert_eq!(after[0].children.len(), 3);
    }

    /// A whole store keeps the behaviour it always had: the store row stands
    /// for the root, and the root's children hang straight off it.
    #[test]
    fn a_whole_store_still_shows_its_roots_children() {
        let (app, store_id, child_id) = store_with_a_child();
        let data = untracked(|| app.build_tree_data_structural());
        assert_eq!(data[0].children.len(), 1);
        assert_eq!(data[0].children[0].value, format!("node_{store_id}_{child_id}"));
        assert!(!app.is_partial_replica(store_id));
        assert_eq!(app.shown_roots(store_id), vec![app.root_node_id(store_id).unwrap()]);
        assert_eq!(app.store_row_node(store_id), app.root_node_id(store_id));
    }
}
