//! A metanode's source, evaluated (docs/METANODES_CONTRACT.md sections 1.1
//! and 1.3): the [`Forest`] of nodes it shows, read from held documents, and
//! the judgement of an edit made through a metanode's viewer, which adds "it
//! leaves the view" to `plan`'s refusals and warnings.
//!
//! A source answers a forest, not a list: each node with its parent and its
//! children that are also in the result, in the tree's own order. That
//! order is data (a parent's list is co-edited, and a card's place in its
//! column is its priority), so nothing here sorts. Grouping and sorting are
//! a viewer's, for display only.

use std::collections::HashSet;

use pimble_core::metanode::{leaves_metanode_warning, Metanode, Source};
use pimble_core::plan::MovePlan;
use pimble_core::{Node, NodeId, Place, StoreId};

use crate::{Addr, Affected, ApiError, Change, Pimble, View};

/// One node a source found.
#[derive(Debug, Clone)]
pub struct ForestEntry {
    /// Where it lives (a mount's source store, for a node seen through one).
    pub addr: Addr,
    pub node: Node,
    /// Its parent in the forest; `None` for a top, a child of the metanode.
    pub parent: Option<Addr>,
    /// Its children that are in the forest too, in the tree's order.
    pub children: Vec<Addr>,
    /// How far below the metanode: 1 for its children.
    pub depth: u32,
}

/// What a metanode's source found, as a forest in the tree's own order.
#[derive(Debug, Clone)]
pub struct Forest {
    /// The metanode.
    pub metanode: Addr,
    /// The metanode as it reads now.
    pub node: Node,
    /// Its viewer and source; `None` for a node with no viewer, whose source
    /// is its children.
    pub spec: Option<Metanode>,
    /// The source that was evaluated.
    pub source: Source,
    /// Every node found, depth first in the tree's order: each parent
    /// before its children, siblings in their list's order, so an outline
    /// is these in turn, indented by depth.
    pub entries: Vec<ForestEntry>,
    /// The nodes whose list of children the source read: a node that lands
    /// in one of these lists is in the forest.
    lists: Vec<Addr>,
}

impl Forest {
    /// The tops: the metanode's own children, in order.
    pub fn tops(&self) -> impl Iterator<Item = &ForestEntry> {
        self.entries.iter().filter(|e| e.parent.is_none())
    }

    pub fn get(&self, addr: Addr) -> Option<&ForestEntry> {
        self.entries.iter().find(|e| e.addr == addr)
    }

    pub fn contains(&self, addr: Addr) -> bool {
        self.get(addr).is_some()
    }

    /// Whether a node put in `parent`'s list is in the forest: `parent` is
    /// the metanode, or a node of the forest whose children the source
    /// reaches (a mount counts through its source).
    pub fn lists_children_of(&self, parent: Addr) -> bool {
        self.lists.contains(&parent)
    }

    /// Whether what `affected` names in `store` may change this forest, so
    /// it should be read again ([`Pimble::metanode_source`]). Cheap: a
    /// membership test of the ids a change names against the metanode, the
    /// nodes found and the lists read. A wave-1 source is tree-shaped, so
    /// nothing outside those can bring a node into it: a node arriving lands
    /// in a list read, one leaving leaves one, and a row changing is a node
    /// found.
    pub fn touched(&self, store: StoreId, affected: &Affected) -> bool {
        let watched = |id: &NodeId| {
            let addr = Addr::new(store, *id);
            addr == self.metanode || self.lists.contains(&addr) || self.contains(addr)
        };
        affected.nodes.iter().chain(&affected.lists).chain(&affected.parents_of).chain(&affected.gone).any(watched)
    }

    /// [`Forest::touched`] for a change as [`Pimble::next_change`] gives it.
    pub fn touched_by(&self, change: &Change) -> bool {
        match change {
            Change::Store(change) => self.touched(change.store, &Affected::of(&change.kind)),
            Change::Resynced { store, changed } => self.touched(*store, &Affected::documents(changed.clone())),
            Change::Rejudged { store, nodes } => self.touched(*store, &Affected::rows(nodes.clone())),
        }
    }

    /// [`Forest::touched`] for what a client was shown of one change
    /// ([`Pimble::shown`]): its rows, its lists' parents and children, and
    /// what left.
    pub fn touched_by_view(&self, view: &View) -> bool {
        let mut ids: Vec<NodeId> = view.nodes.iter().map(|n| n.id).collect();
        ids.extend(view.gone.iter().copied());
        for listed in &view.lists {
            ids.push(listed.parent);
            if self.touched(listed.children_store, &Affected::rows(listed.children.iter().map(|n| n.id).collect())) {
                return true;
            }
        }
        self.touched(view.store, &Affected::rows(ids))
    }

    /// Why the source is shown other than as it was written, when it is
    /// (a newer Pimble wrote part of it).
    pub fn not_understood(&self) -> Option<&'static str> {
        self.source.not_understood()
    }
}

/// An edit made through a metanode's viewer, to be judged before it is sent.
#[derive(Debug, Clone)]
pub enum MetanodeEdit {
    /// Move `nodes` (top to bottom) under the row `target` at `place`: a
    /// move, or a transplant into another store.
    Move { nodes: Vec<Addr>, target: Addr, place: Place },
    Delete { nodes: Vec<Addr> },
}

/// What an edit made through a metanode does, said before it is sent.
#[derive(Debug, Clone)]
pub struct Judged {
    /// For a move, `plan`'s plan of it ([`Pimble::move_nodes`] sends it).
    pub plan: Option<MovePlan>,
    /// What the person is told beside it, in order: `plan`'s warning (a new
    /// node in another store, leaving a share), then that it leaves the view.
    pub warnings: Vec<String>,
}

impl Pimble {
    /// The forest `metanode`'s source finds (docs/METANODES_CONTRACT.md
    /// section 1.1), read from held documents and holding what it reads: the
    /// metanode's list, then each found node's down to the source's depth,
    /// through mounts. A node with no viewer is read as its children. A list
    /// that cannot be read below the metanode (a mount whose store is not
    /// open) leaves that node with no children rather than failing the
    /// whole forest.
    pub async fn metanode_source(&self, metanode: Addr) -> Result<Forest, ApiError> {
        let node = self.node(metanode).await?;
        let spec = node.metadata.metanode.clone();
        let source = spec.as_ref().map(|s| s.source.clone()).unwrap_or_default();
        let depth = source.depth();
        let mut entries: Vec<ForestEntry> = Vec::new();
        let mut lists = vec![metanode];
        let mut seen: HashSet<Addr> = HashSet::from([metanode]);
        if let Some(mount) = node.mount_ref() {
            lists.push(Addr::new(mount.source_store, mount.source_node));
        }

        // Depth first, each list pushed in reverse so the first child is
        // visited next: entries come out in the tree's order.
        let mut stack: Vec<(Addr, Node, Option<Addr>, u32)> = Vec::new();
        for (addr, child) in self.children(metanode).await?.into_iter().rev() {
            stack.push((addr, child, None, 1));
        }
        while let Some((addr, node, parent, at)) = stack.pop() {
            // A mount can show a node twice (or itself); each is shown once.
            if !seen.insert(addr) {
                continue;
            }
            if let Some(parent) = parent {
                if let Some(p) = entries.iter_mut().find(|e| e.addr == parent) {
                    p.children.push(addr);
                }
            }
            let mount = node.mount_ref().map(|m| Addr::new(m.source_store, m.source_node));
            entries.push(ForestEntry { addr, node, parent, children: Vec::new(), depth: at });
            if at >= depth {
                continue;
            }
            lists.push(addr);
            if let Some(source) = mount {
                lists.push(source);
            }
            match self.children(addr).await {
                Ok(children) => {
                    for (child_addr, child) in children.into_iter().rev() {
                        stack.push((child_addr, child, Some(addr), at + 1));
                    }
                }
                Err(e) => tracing::debug!("a metanode's source could not read the children of {}: {e}", addr.link()),
            }
        }
        Ok(Forest { metanode, node, spec, source, entries, lists })
    }

    /// Make `addr` a metanode shown with `viewer`, or with `None` an
    /// ordinary node again (its outline), through the one write. `source`
    /// sets which nodes it shows; `None` leaves the stored source as it is
    /// (a node shown as an outline and then with a viewer again keeps it).
    pub async fn set_metanode(&self, addr: Addr, viewer: Option<&str>, source: Option<&Source>) -> Result<crate::Edited<()>, ApiError> {
        let viewer = viewer.map(str::to_string);
        let source = source.cloned();
        self.edit(addr, move |doc, now| -> Result<(), ApiError> {
            if let Some(source) = &source {
                doc.set_metanode_source(Some(source), now)?;
            }
            doc.set_metanode_viewer(viewer.as_deref(), now)?;
            Ok(())
        })
        .await
    }

    /// Judge an edit made through the metanode `metanode`
    /// (docs/METANODES_CONTRACT.md section 1.3): `plan`'s refusals and
    /// warnings, as the explorer gets them, and on top of them that the edit
    /// takes nodes out of the metanode's source ([`leaves_metanode_warning`]).
    /// Nothing is sent.
    pub async fn judge_in_metanode(&self, metanode: Addr, edit: MetanodeEdit) -> Result<Judged, ApiError> {
        let forest = self.metanode_source(metanode).await?;
        match edit {
            MetanodeEdit::Move { nodes, target, place } => {
                let plan = self.plan_move(&nodes, target, place).await?;
                let mut warnings: Vec<String> = plan.warning.iter().cloned().collect();
                if !plan.stays() {
                    let landing = Addr::new(plan.parent.0, plan.parent.1);
                    let shown = nodes.iter().filter(|a| forest.contains(**a)).count();
                    if shown > 0 && !forest.lists_children_of(landing) {
                        warnings.push(leaves_metanode_warning(shown));
                    }
                }
                Ok(Judged { plan: Some(plan), warnings })
            }
            MetanodeEdit::Delete { nodes } => {
                self.can_delete(&nodes).await?;
                let shown = nodes.iter().filter(|a| forest.contains(**a)).count();
                let warnings = if shown > 0 { vec![leaves_metanode_warning(shown)] } else { Vec::new() };
                Ok(Judged { plan: None, warnings })
            }
        }
    }
}
