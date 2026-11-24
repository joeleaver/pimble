//! Reads of the documents this page holds, and searching them.

use pimble_api::ApiError;
use pimble_core::{node_types, DeletedNode, LinkResolution, Node, NodeId, StoreAccess, StoreId};
use pimble_rpc::SearchResultItem;

use super::{not_open, VaultClient};
use super::helpers::{resolve_in, snippet_around};

impl VaultClient {
    // ── Reads ───────────────────────────────────────────────────────────────
    //
    // What `crate::vault_backend::VaultBackend` answers `pimble_api::Backend`
    // with, from the documents held: the same nodes, in the same shape, as a
    // server's `getNode` and `getChildren`, each judged with what this
    // account may change of it. Whether the store's owner is away is the
    // router's to say (`pimble_api::Router::set_away`).

    /// The store's root, which a node made with no parent goes under.
    pub fn root_of(&self, store_id: StoreId) -> Option<NodeId> {
        self.stores.get(&store_id).map(|store| store.tree.root())
    }

    /// Every child arrives with its document's bytes, because the app
    /// derives a document's tree label from its first line and opens it
    /// from the bytes it is handed, with no second fetch in between.
    pub fn children(&self, store_id: StoreId, node_id: NodeId) -> Result<Vec<Node>, ApiError> {
        let store = self.stores.get(&store_id).ok_or_else(not_open)?;
        let row = self.rows.get(&store_id);
        let ids = store.children_of(node_id).map_err(|_| ApiError::node_not_found(node_id))?;
        Ok(ids.iter().filter_map(|id| store.node_for(*id, row).ok()).collect())
    }

    pub fn node(&self, store_id: StoreId, node_id: NodeId) -> Result<Node, ApiError> {
        let store = self.stores.get(&store_id).ok_or_else(not_open)?;
        store.node_for(node_id, self.rows.get(&store_id)).map_err(|_| ApiError::node_not_found(node_id))
    }

    /// The live nodes of `node_ids` this page holds; the rest left out, as
    /// a server's `getNodes` leaves out what is not there.
    pub fn nodes(&self, store_id: StoreId, node_ids: &[NodeId]) -> Result<Vec<Node>, ApiError> {
        let store = self.stores.get(&store_id).ok_or_else(not_open)?;
        let row = self.rows.get(&store_id);
        Ok(node_ids.iter().filter_map(|id| store.node_for(*id, row).ok()).collect())
    }

    /// The stateless reconcile (`syncNodes`), answered from the documents
    /// held: for each document named, what the asker lacks and this page's
    /// state vector; with `list_unknown`, every document held that was not
    /// named.
    pub fn sync_docs(&self, store_id: StoreId, have: &[(NodeId, Vec<u8>)], list_unknown: bool) -> Result<(Vec<pimble_api::backend::SyncedDoc>, Vec<NodeId>), ApiError> {
        let store = self.stores.get(&store_id).ok_or_else(not_open)?;
        let mut docs = Vec::new();
        for (node_id, state_vector) in have {
            let Some(doc) = store.tree.doc(*node_id) else { continue };
            docs.push(pimble_api::backend::SyncedDoc { node: *node_id, diff: doc.diff_since(state_vector)?, state_vector: doc.state_vector() });
        }
        let unknown = if list_unknown {
            store.tree.ids().into_iter().filter(|id| !have.iter().any(|(named, _)| named == id)).collect()
        } else {
            Vec::new()
        };
        Ok((docs, unknown))
    }

    /// A node as the UI is handed it while its store's owner's computer is
    /// off: to be read, whatever the account's grants say, because nothing
    /// written to it here would reach anyone (`pimble_api::router`). For the
    /// commands not routed through the router yet.
    fn judged(&self, store_id: StoreId, mut node: Node) -> Node {
        if self.owner_offline.contains(&store_id) {
            node.access = StoreAccess::Read;
        }
        node
    }

    /// The discussions of the store whose host is held (or that have none).
    pub fn list_discussions(&self, store_id: StoreId) -> Result<Vec<Node>, ApiError> {
        let store = self.stores.get(&store_id).ok_or_else(not_open)?;
        let row = self.rows.get(&store_id);
        let nodes = store
            .tree
            .list_node_ids()
            .into_iter()
            .filter(|id| {
                store.tree.live_fields(*id).is_ok_and(|f| {
                    f.node_type == node_types::DISCUSSION && f.attached_to.is_none_or(|host| store.tree.has_node(host))
                })
            })
            .filter_map(|id| store.node_for(id, row).ok())
            .map(|node| self.judged(store_id, node))
            .collect();
        Ok(nodes)
    }

    pub fn list_attachments(&self, store_id: StoreId, node_id: NodeId) -> Result<Vec<Node>, ApiError> {
        let store = self.stores.get(&store_id).ok_or_else(not_open)?;
        let row = self.rows.get(&store_id);
        Ok(store.tree.attachments_of(node_id).into_iter().filter_map(|id| store.node_for(id, row).ok()).collect())
    }

    /// "Recently Deleted..." (docs/MOVE_CONTRACT.md "Seeing and undoing what
    /// was removed"), answered from the tree this page holds: a scoped
    /// member's page holds only its scope's documents to begin with, so
    /// nothing further is judged here.
    pub fn list_deleted(&self, store_id: StoreId) -> Result<Vec<DeletedNode>, ApiError> {
        let store = self.stores.get(&store_id).ok_or_else(not_open)?;
        Ok(store.list_deleted(self.rows.get(&store_id)))
    }

    /// `resolveLink` for an encrypted store (docs/LINKS_CONTRACT.md
    /// "Following a link"): the server's, answered from the documents this
    /// page holds, hop by hop through the tombstones' `became`. A hop into a
    /// store this page does not hold answers `StoreNotHere` there, and the
    /// app asks whoever holds that one.
    pub fn resolve_link(&self, store_id: StoreId, node_id: NodeId) -> LinkResolution {
        resolve_in(&self.stores, store_id, node_id)
    }

    // ── Search ──────────────────────────────────────────────────────────────

    /// Client-side search: a case-insensitive substring over decrypted titles
    /// and the text of every document held, which is every node's.
    ///
    /// There is no server index for a vault store and there cannot be one — the
    /// server has never seen a word of it. Plain stores in the same query are
    /// the router's to send to their own server (`pimble_api::Router`'s
    /// `search`); an encrypted store not open here (its key or its owner's
    /// computer is still to come) has nothing anywhere to search.
    /// The nodes of `store_id` whose text links to `node_id`, from the
    /// documents this page holds (there is no index for an encrypted store
    /// anywhere): what the Properties panel's "Links here" lists
    /// (docs/PROPERTIES_CONTRACT.md section 5).
    pub fn backlinks(&self, store_id: StoreId, node_id: NodeId) -> Vec<NodeId> {
        if !self.owns(store_id) {
            return Vec::new();
        }
        let Some(store) = self.stores.get(&store_id) else { return Vec::new() };
        let ids = store.tree.list_node_ids();
        let docs = ids.iter().filter_map(|id| store.tree.doc(*id).map(|doc| (*id, doc)));
        pimble_api::references::backlinks_among(pimble_api::Addr::new(store_id, node_id), docs)
    }

    pub fn search(&self, query: &str, stores: &[StoreId], limit: usize) -> Vec<SearchResultItem> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }

        let mut results = Vec::new();
        for store_id in stores.iter().filter(|id| self.owns(**id)) {
            let Some(store) = self.stores.get(store_id) else { continue };
            for node_id in store.tree.list_node_ids() {
                let Ok(info) = store.tree.get_node_info(node_id) else { continue };
                // A board node's words are the ones on its board.
                let text = store
                    .tree
                    .doc(node_id)
                    .map(|doc| {
                        if info.node_type == node_types::BOARD {
                            doc.board_units().into_iter().map(|u| u.text).collect::<Vec<_>>().join("\n")
                        } else {
                            // The text, then the typed cells of its tables
                            // (docs/SHEETS_CONTRACT.md section 10).
                            let cells = pimble_sheet::index::index_units(doc).into_iter().map(|u| u.text).collect::<Vec<_>>().join("\n");
                            match doc.text() {
                                text if cells.is_empty() => text,
                                text if text.is_empty() => cells,
                                text => format!("{text}\n{cells}"),
                            }
                        }
                    })
                    .unwrap_or_default();

                let in_title = info.title.to_lowercase().contains(&needle);
                let at = text.to_lowercase().find(&needle);
                if !in_title && at.is_none() {
                    continue;
                }

                results.push(SearchResultItem {
                    node_id,
                    store_id: *store_id,
                    // A title match ranks above a body match; there is no
                    // scoring model here and pretending otherwise would be
                    // worse than saying so.
                    score: if in_title { 1.0 } else { 0.5 },
                    title: if info.title.is_empty() { "Untitled".to_string() } else { info.title },
                    snippet: snippet_around(&text, at),
                    kind: "prose".to_string(),
                    node_type: info.node_type,
                    path: String::new(),
                    section: Vec::new(),
                });
            }
        }

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);
        results
    }
}
