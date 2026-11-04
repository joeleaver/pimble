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
//!
//! A query metanode (docs/QUERY_METANODES_CONTRACT.md) is the exception:
//! after its own children come the nodes its match finds, in the match's
//! order, each marked `found`. They are found afresh from the held
//! documents every time and never stored anywhere.

use std::collections::{HashMap, HashSet};

use pimble_core::metanode::{leaves_metanode_warning, parse_node_ref, Condition, Metanode, Named, Query, Source, Within};
use pimble_core::plan::{found_refusal, lives_in, MovePlan};
use pimble_core::{node_types, Node, NodeId, Place, StoreId};
use pimble_crdt::{NodeDoc, NodeFields};
use pimble_rpc::StoreChangeKind;

use crate::matching::{compare, Key, Matcher};
use crate::{label, Addr, Affected, ApiError, Change, Pimble, View};

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
    /// How far below the metanode: 1 for its children, and for a found node.
    pub depth: u32,
    /// Found by the metanode's match rather than one of its own children
    /// (or theirs). A found entry is the real node, living elsewhere: it
    /// changes itself but not its place from here
    /// (`plan::FoundRowAction`, [`Pimble::found_refusal`]). It has no
    /// parent and no children in the forest; a viewer that shows its
    /// children lists them as any node's.
    pub found: bool,
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
    /// Why a found entry is found, after "Shown here because" ("it is
    /// tagged #waiting"); `None` for a source that finds nothing.
    pub reason: Option<String>,
    /// The nodes whose list of children the source read: a node that lands
    /// in one of these lists is in the forest.
    lists: Vec<Addr>,
    /// The stores a match looked in: any node changing there may change
    /// what it finds.
    pub(crate) scanned: Vec<StoreId>,
    /// Whether what a match finds depends on node content (a text or links
    /// condition, or the last-changed order), so a content edit counts.
    reads_content: bool,
}

impl Forest {
    /// The tops: the metanode's own children, in order, then what its match
    /// found.
    pub fn tops(&self) -> impl Iterator<Item = &ForestEntry> {
        self.entries.iter().filter(|e| e.parent.is_none())
    }

    /// What the metanode's match found, in the match's order.
    pub fn found(&self) -> impl Iterator<Item = &ForestEntry> {
        self.entries.iter().filter(|e| e.found)
    }

    /// Whether `addr` is in the forest as a found entry.
    pub fn is_found(&self, addr: Addr) -> bool {
        self.get(addr).is_some_and(|e| e.found)
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
    ///
    /// A match is not tree-shaped: any node of a store it looked in may
    /// come to match or stop matching, so any document named there touches
    /// it (still one membership test per change).
    pub fn touched(&self, store: StoreId, affected: &Affected) -> bool {
        if self.scanned.contains(&store) && !affected.is_empty() {
            return true;
        }
        let watched = |id: &NodeId| {
            let addr = Addr::new(store, *id);
            addr == self.metanode || self.lists.contains(&addr) || self.contains(addr)
        };
        affected.nodes.iter().chain(&affected.lists).chain(&affected.parents_of).chain(&affected.gone).any(watched)
    }

    /// [`Forest::touched`] for a change as [`Pimble::next_change`] gives it.
    /// A content edit in a store a match looked in counts only when what it
    /// finds reads content or the node is shown.
    pub fn touched_by(&self, change: &Change) -> bool {
        match change {
            Change::Store(change) => {
                if let StoreChangeKind::ContentUpdated { node_id } = &change.kind {
                    if !self.reads_content && self.scanned.contains(&change.store) {
                        let addr = Addr::new(change.store, *node_id);
                        return addr == self.metanode || self.contains(addr);
                    }
                }
                self.touched(change.store, &Affected::of(&change.kind))
            }
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
            entries.push(ForestEntry { addr, node, parent, children: Vec::new(), depth: at, found: false });
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
        let mut scanned = Vec::new();
        let mut reason = None;
        let reads_content = match &source.query {
            Query::Match { all, order, .. } => {
                matches!(order, pimble_core::metanode::Order::Modified)
                    || all.iter().any(|c| matches!(c, Condition::Text { .. } | Condition::LinksTo { .. }))
            }
            _ => false,
        };
        if let Some(matcher) = Matcher::of(&source.query, crate::book::SheetBook::local_today()) {
            // Never the metanode, what is above it, or its own children.
            let mut skip: HashSet<Addr> = entries.iter().map(|e| e.addr).collect();
            skip.insert(metanode);
            skip.extend(self.ancestors(metanode).await.unwrap_or_default());
            let (found, stores) = self.find(metanode, &source, &matcher, &skip).await?;
            scanned = stores;
            entries.extend(found);
            let names = self.names_in(metanode, &source).await;
            reason = source.found_reason(&|n| names.get(&n.key()).cloned());
        }
        Ok(Forest { metanode, node, spec, source, entries, reason, lists, scanned, reads_content })
    }

    /// What a match finds (docs/QUERY_METANODES_CONTRACT.md section 3): the
    /// live nodes below the tops of each store it looks in (or below the
    /// node it names), as held, that every condition holds for and `skip`
    /// does not name, sorted and cut to the limit. A store's own nodes
    /// only: a mount in it is judged as a node, and not entered. Answers the
    /// stores looked in too.
    async fn find(&self, metanode: Addr, source: &Source, matcher: &Matcher, skip: &HashSet<Addr>) -> Result<(Vec<ForestEntry>, Vec<StoreId>), ApiError> {
        let Query::Match { within, order, descending, .. } = &source.query else { return Ok((Vec::new(), Vec::new())) };
        let places: Vec<(StoreId, Option<NodeId>)> = match within {
            None => vec![(metanode.store, None)],
            Some(Within::Store(store)) => vec![(*store, None)],
            Some(Within::Under(node)) => parse_node_ref(node).map(|(s, n)| vec![(s, Some(n))]).unwrap_or_default(),
            Some(Within::Everywhere) => self.stores().await?.iter().map(|s| (s.id, None)).collect(),
        };
        // (address, sort key, lowercased title, the node when already read)
        let mut hits: Vec<(Addr, Key, String, Option<Node>)> = Vec::new();
        let mut scanned = Vec::new();
        for (store, under) in places {
            let tops = match under {
                Some(node) => vec![node],
                None => match self.store_row(store).await {
                    Some(row) => row.shown_roots(),
                    None => continue,
                },
            };
            scanned.push(store);
            let judge = |addr: Addr, doc: &NodeDoc, fields: &NodeFields, node: Option<Node>, hits: &mut Vec<_>| {
                if !skip.contains(&addr) && fields.deleted_at.is_none() && matcher.matches(doc, fields) {
                    hits.push((addr, matcher.key(order, fields), fields.title.trim().to_lowercase(), node));
                }
            };
            if self.passes_through(store) {
                // The backend holds these documents: walk them through it.
                let mut stack: Vec<Addr> = tops.iter().map(|n| Addr::new(store, *n)).collect();
                let mut seen = HashSet::new();
                while let Some(at) = stack.pop() {
                    if !seen.insert(at) {
                        continue;
                    }
                    let Ok(children) = self.children(at).await else { continue };
                    for (addr, child) in children {
                        if addr.store != store {
                            continue;
                        }
                        let doc = if child.content.is_empty() { self.doc(addr).await.ok() } else { NodeDoc::load(&child.content).ok() };
                        let Some(doc) = doc else { continue };
                        let Ok(fields) = doc.fields() else { continue };
                        if child.node_type != node_types::MOUNT {
                            stack.push(addr);
                        }
                        judge(addr, &doc, &fields, Some(child), &mut hits);
                    }
                }
                continue;
            }
            if let Err(e) = self.hold_store(store).await {
                tracing::debug!("a match could not look in store {store}: {e}");
                continue;
            }
            let held = self.held.lock().unwrap();
            let Some(h) = held.store(store) else { continue };
            let mut stack = tops;
            let mut seen = HashSet::new();
            while let Some(at) = stack.pop() {
                if !seen.insert(at) {
                    continue;
                }
                for id in h.tree.get_children(at).unwrap_or_default() {
                    let Some(doc) = h.tree.doc(id) else { continue };
                    let Ok(fields) = doc.fields() else { continue };
                    if fields.node_type != node_types::MOUNT {
                        stack.push(id);
                    }
                    judge(Addr::new(store, id), doc, &fields, None, &mut hits);
                }
            }
        }
        hits.sort_by(|a, b| compare((&a.1, &a.2, a.0.node), (&b.1, &b.2, b.0.node), *descending));
        hits.dedup_by_key(|h| h.0);
        hits.truncate(source.limit());
        let mut found = Vec::with_capacity(hits.len());
        for (addr, _, _, node) in hits {
            let node = match node {
                Some(node) => node,
                None => match self.held_node(addr).await {
                    Ok(node) => node,
                    Err(_) => continue,
                },
            };
            found.push(ForestEntry { addr, node, parent: None, children: Vec::new(), depth: 1, found: true });
        }
        Ok((found, scanned))
    }

    /// What the stores and nodes a match names are called, keyed as
    /// [`Named::key`] keys them, for saying the source in words.
    async fn names_in(&self, metanode: Addr, source: &Source) -> HashMap<String, String> {
        let mut names = HashMap::new();
        let Query::Match { within, all, .. } = &source.query else { return names };
        if let Some(row) = self.store_row(metanode.store).await {
            names.insert(Named::Here.key(), row.name);
        }
        if let Some(Within::Store(store)) = within {
            if let Some(row) = self.store_row(*store).await {
                names.insert(Named::Store(*store).key(), row.name);
            }
        }
        let mut refs: Vec<&str> = all.iter().filter_map(|c| if let Condition::LinksTo { links_to } = c { Some(links_to.as_str()) } else { None }).collect();
        if let Some(Within::Under(node)) = within {
            refs.push(node);
        }
        for raw in refs {
            if let Some((store, node)) = parse_node_ref(raw) {
                if let Ok(n) = self.held_node(Addr::new(store, node)).await {
                    names.insert(Named::Node(raw).key(), label(&n));
                }
            }
        }
        names
    }

    /// A metanode's source in words, with the stores and nodes it names
    /// called by their names ("the nodes tagged waiting in Personal, ...").
    pub async fn describe_source(&self, metanode: Addr, source: &Source) -> String {
        let of = match self.held_node(metanode).await {
            Ok(n) => label(&n),
            Err(_) => "this node".to_string(),
        };
        let names = self.names_in(metanode, source).await;
        source.describe_named(&of, &|n| names.get(&n.key()).cloned())
    }

    /// Where `addr` lives, as a found row's refusal says it: its store's
    /// name and the titles above it, "Personal / Projects".
    pub async fn lives_in(&self, addr: Addr) -> String {
        let store = self.store_row(addr.store).await;
        let mut titles: Vec<String> = Vec::new();
        for above in self.ancestors(addr).await.unwrap_or_default() {
            if store.as_ref().is_some_and(|s| s.root_node_id == above.node) {
                break;
            }
            match self.held_node(above).await {
                Ok(n) => titles.push(label(&n)),
                Err(_) => break,
            }
        }
        titles.push(store.map_or_else(|| addr.store.to_string(), |s| s.name));
        titles.reverse();
        lives_in(&titles)
    }

    /// Why a found entry's place cannot be changed from the metanode
    /// (docs/QUERY_METANODES_CONTRACT.md section 4): "Shown here because it
    /// is tagged #waiting. It lives in Personal / Projects." `None` for an
    /// entry that is not found (an own child, or not in the forest).
    pub async fn found_refusal(&self, forest: &Forest, addr: Addr) -> Option<String> {
        if !forest.is_found(addr) {
            return None;
        }
        let reason = forest.reason.clone().unwrap_or_else(|| "this list finds it".to_string());
        Some(found_refusal(&reason, &self.lives_in(addr).await))
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
        // A found node's place is not changed from here, and nothing is put
        // into or beside it (plan::FoundRowAction).
        let named: Vec<Addr> = match &edit {
            MetanodeEdit::Move { nodes, target, .. } => nodes.iter().copied().chain([*target]).collect(),
            MetanodeEdit::Delete { nodes } => nodes.clone(),
        };
        for addr in named {
            if let Some(sentence) = self.found_refusal(&forest, addr).await {
                return Err(ApiError::refused(sentence));
            }
        }
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
