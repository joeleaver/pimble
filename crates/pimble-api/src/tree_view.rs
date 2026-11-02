//! The API's [`TreeView`]: what `pimble_core::plan` judges (docs/API_CONTRACT.md
//! wave 3), read from the documents [`Pimble`] holds. The judgements are pure
//! and synchronous, so a plan is made in two halves: first everything it
//! needs is held (the nodes, the target, a mount's source, their ancestors),
//! then it is judged over what is held, with nothing read in between.
//!
//! The stores' facts come from their rows as last listed; a node's access is
//! what the server judged when it handed the node over, else its store's
//! (`plan::node_access`, the same rule `HeldStore::node` answers with);
//! labels are `plan::display_label` over the node's document.

use pimble_core::plan::{self, MovePlan, MoveStep, Refusal, StoreFacts, TreeView};
use pimble_core::{node_types, Node, NodeId, Place, RelaySide, StoreAccess, StoreId, StoreKind};
use pimble_crdt::NodeDoc;

use crate::backend::Moved;
use crate::held::{Held, HeldStore};
use crate::{Addr, ApiError, MovedTo, Pimble};

impl From<Refusal> for ApiError {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::ReadOnly => ApiError::ReadOnly,
            other => ApiError::refused(other.sentence()),
        }
    }
}

/// What is held, seen as a tree. Lives only while the held documents are
/// locked ([`Pimble::view`]).
struct HeldView<'a>(&'a Held);

impl HeldView<'_> {
    fn held(&self, store: StoreId) -> Option<&HeldStore> {
        self.0.store(store)
    }

    /// A live held node's fields as a `Node`, without its content (what
    /// mounts and share markers are read from).
    fn bare(&self, store: StoreId, node: NodeId) -> Option<Node> {
        Some(self.held(store)?.tree.live_fields(node).ok()?.into_node(node))
    }
}

impl TreeView for HeldView<'_> {
    fn store(&self, store: StoreId) -> Option<StoreFacts> {
        let row = self.0.store_row(store)?;
        // A row does not say whether a plain store is linked to another
        // Pimble server (`getStoreSync` does); it says the links it can:
        // to a hosted or relayed twin, and a replica's to where it lives.
        let linked = row.sync_mode == StoreKind::Vault || row.relay != RelaySide::None || row.is_replica;
        Some(StoreFacts::of(row, linked))
    }

    fn store_access(&self, store: StoreId) -> Option<StoreAccess> {
        self.0.store_row(store).map(|row| row.access)
    }

    fn parent(&self, store: StoreId, node: NodeId) -> Option<NodeId> {
        self.held(store)?.tree.live_fields(node).ok()?.parent_id
    }

    fn children(&self, store: StoreId, node: NodeId) -> Option<Vec<NodeId>> {
        self.held(store)?.tree.get_children(node).ok()
    }

    fn is_mount(&self, store: StoreId, node: NodeId) -> bool {
        self.bare(store, node).is_some_and(|n| n.node_type == node_types::MOUNT)
    }

    fn mount_source(&self, store: StoreId, node: NodeId) -> Option<(StoreId, NodeId)> {
        self.bare(store, node)?.mount_ref().map(|m| (m.source_store, m.source_node))
    }

    fn is_shared(&self, store: StoreId, node: NodeId) -> bool {
        self.bare(store, node).is_some_and(|n| n.metadata.share().is_some())
    }

    fn held_access(&self, store: StoreId, node: NodeId) -> Option<StoreAccess> {
        self.held(store)?.judged.get(&node).map(|j| j.access)
    }

    fn label(&self, store: StoreId, node: NodeId) -> String {
        match self.held(store).and_then(|h| h.node(node)) {
            Some(n) => plan::display_label(&n, NodeDoc::text_of),
            None => plan::UNTITLED.to_string(),
        }
    }
}

impl Pimble {
    /// Judge over what is held now, reading nothing: `judge` gets the held
    /// documents as a [`TreeView`] for any of `pimble_core::plan`'s
    /// functions. What is not held reads as "not known"; [`Pimble::plan_move`],
    /// [`Pimble::create_parent`] and [`Pimble::can_delete`] hold what they
    /// need first.
    pub fn view<R>(&self, judge: impl FnOnce(&dyn TreeView) -> R) -> R {
        self.drain();
        let held = self.held.lock().unwrap();
        judge(&HeldView(&held))
    }

    /// Hold `addr`, and its store's row (listed again when a store opened
    /// since).
    async fn hold_node(&self, addr: Addr) -> Result<Node, ApiError> {
        self.store_row(addr.store).await;
        self.held_node(addr).await
    }

    /// Hold a row a plan starts from: the node, and when it is a mount, its
    /// source's store row and the source. Answers the node new things go
    /// under, as far as it is known.
    async fn hold_row(&self, addr: Addr) -> Result<Addr, ApiError> {
        let node = self.hold_node(addr).await?;
        let Some(mount) = node.mount_ref() else { return Ok(addr) };
        let source = Addr { store: mount.source_store, node: mount.source_node };
        // Whether its store is open here is asked now: a listing (and the
        // source held from it) may be from before the store closed.
        let open = self.stores().await?.iter().any(|s| s.id == source.store);
        if open {
            // Not there is the server's to say; the plan reads it as not known.
            let _ = self.held_node(source).await;
        }
        Ok(source)
    }

    /// Where something made "under" `addr` goes ([`plan::create_parent`]): a
    /// mount's source (open decision 5: every creation path, discussions and
    /// moves too), anything else itself, refused when this principal may
    /// only read there or a mount's store is not open here.
    pub async fn create_parent(&self, addr: Addr) -> Result<Addr, ApiError> {
        self.hold_row(addr).await?;
        let (store, node) = self.view(|v| plan::create_parent(v, addr.store, Some(addr.node)))?;
        Ok(Addr { store, node })
    }

    /// Where an attachment of `host` goes: [`Pimble::create_parent`] without
    /// the write check, since a commenter may start a discussion where they
    /// may not write (the server judges it).
    pub(crate) async fn attachment_host(&self, host: Addr) -> Result<Addr, ApiError> {
        self.hold_row(host).await?;
        let (store, node) = self.view(|v| plan::parent_of_row(v, host.store, Some(host.node)))?;
        Ok(Addr { store, node })
    }

    /// Judge and plan a move of `nodes` (top to bottom) under the row
    /// `target` (a mount's source for a mount) at `place`
    /// ([`plan::plan_move`]): the app's refusals and sentences, and what
    /// else it does (a new node in another store, leaving a share) in
    /// `MovePlan::warning`. Nothing is sent; [`Pimble::move_nodes`] sends it.
    pub async fn plan_move(&self, nodes: &[Addr], target: Addr, place: Place) -> Result<MovePlan, ApiError> {
        let stores = self.known_stores().await?;
        let is_root = |addr: Addr| stores.iter().any(|s| s.id == addr.store && s.root_node_id == addr.node);
        // A store's root is its row, which is not a node to move, and no
        // place is beside it: the tree's shape cannot say either.
        if nodes.iter().any(|&addr| is_root(addr)) {
            return Err(Refusal::NotANode.into());
        }
        if let Place::After(anchor) = place {
            if stores.iter().any(|s| s.root_node_id == anchor) {
                return Err(Refusal::BesideAStore.into());
            }
        }
        for &addr in nodes {
            self.hold_node(addr).await?;
            // Above it: the list it leaves, and the shares it may leave.
            self.ancestors(addr).await?;
        }
        let parent = self.hold_row(target).await?;
        // Above the new parent: whether it is under a moved node, and the
        // shares it stays in. Not known reads as no.
        let _ = self.ancestors(parent).await;
        if let Place::After(anchor) = place {
            let _ = self.held_node(Addr { store: parent.store, node: anchor }).await;
        }
        // Whether one node is already where it goes needs the list in full.
        if let [one] = nodes {
            if one.store == parent.store && self.held_doc_fields(*one).and_then(|f| f.parent_id) == Some(parent.node) {
                let _ = self.children(parent).await;
            }
        }
        let pairs: Vec<(StoreId, NodeId)> = nodes.iter().map(|a| (a.store, a.node)).collect();
        Ok(self.view(|v| plan::plan_move(v, &pairs, (target.store, Some(target.node)), place))?)
    }

    /// Send a [`MovePlan`]'s requests in its order (as the app's
    /// `tree_drop::send_plan` does): where each node went, in the order they
    /// were sent. A plan that stays sends nothing and answers nothing.
    pub async fn move_nodes(&self, plan: &MovePlan) -> Result<Vec<MovedTo>, ApiError> {
        let mut out = Vec::new();
        let went = |from: Addr, to: StoreId, moved: Moved| {
            let addr = Addr { store: to, node: moved.node };
            MovedTo { addr, new_link: addr != from, left_shares: moved.left_shares.len() }
        };
        for step in &plan.steps {
            match step {
                MoveStep::Move { store, node, parent, place } => {
                    let moved = self.backend().move_node(*store, *node, *parent, *place).await?;
                    out.push(went(Addr { store: *store, node: *node }, *store, moved));
                }
                MoveStep::MoveMany { store, nodes, parent, place } => {
                    let moved = self.backend().move_nodes(*store, nodes.clone(), *parent, *place).await?;
                    for (node, moved) in nodes.iter().zip(moved) {
                        out.push(went(Addr { store: *store, node: *node }, *store, moved));
                    }
                }
                MoveStep::Transplant { from, node, to, parent, place } => {
                    let moved = self.backend().transplant(*from, *node, *to, *parent, *place).await?;
                    out.push(went(Addr { store: *from, node: *node }, *to, moved));
                }
            }
        }
        Ok(out)
    }

    /// Whether `nodes` may be deleted, all of them or none ([`plan::can_delete`]).
    pub async fn can_delete(&self, nodes: &[Addr]) -> Result<(), ApiError> {
        for &addr in nodes {
            self.hold_node(addr).await?;
        }
        let pairs: Vec<(StoreId, NodeId)> = nodes.iter().map(|a| (a.store, a.node)).collect();
        Ok(self.view(|v| plan::can_delete(v, &pairs))?)
    }
}
