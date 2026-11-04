//! What points at a node (docs/PROPERTIES_CONTRACT.md section 5, wave 3):
//! the Metanodes whose queries find it ([`Pimble::shown_in`]), the nodes
//! whose text links to it ([`Pimble::backlinks`]) and the table rows whose
//! link columns choose it ([`Pimble::rows_pointing_here`]). The Properties
//! panel and the MCP's `rows_pointing_here` read these, so both say the
//! same.
//!
//! Finding Metanodes and tables walks a store, so what a walk found is kept
//! per store until a change touches what is held there (a store's
//! generation, `held.rs`), and so is each Metanode's evaluation. Nothing is
//! kept for a store whose backend holds its documents itself (the browser's
//! encrypted stores): this API does not hear those change.

use std::collections::{HashMap, HashSet};

use pimble_core::metanode::{parse_node_ref, Query, Source, Within};
use pimble_core::{Node, Store, StoreId};
use pimble_crdt::NodeDoc;
use pimble_sheet::{ColumnType, NodeRef, Stored, TableSnap};

use crate::book::{node_ref, sheets_of};
use crate::matching::Matcher;
use crate::{label, Addr, ApiError, Forest, Pimble};

/// The table rows of one table that link to a node.
#[derive(Debug, Clone)]
pub struct RowsHere {
    /// The node the table is in.
    pub at: Addr,
    /// That node, as the walk read it (its document as the content).
    pub node: Node,
    /// Titles from its store's name down to it, "Work/Projects/Budget".
    pub path: String,
    /// The table's id and name.
    pub table: String,
    pub table_name: String,
    /// The rows that link to the node, in the table's order: each row's id
    /// and its first column's value as stored ("" when it has none to say).
    pub rows: Vec<(String, String)>,
}

/// One Metanode whose query finds a node.
#[derive(Debug, Clone)]
pub struct ShownIn {
    pub metanode: Addr,
    pub node: Node,
}

/// One node whose text links to a node.
#[derive(Debug, Clone)]
pub struct LinksHere {
    pub addr: Addr,
    pub node: Node,
}

/// The generations of the stores something was derived from: still good
/// while every one of them is the same.
type Deps = Vec<(StoreId, u64)>;

/// A table of a walked node with the targets of each row's link cells.
#[derive(Debug, Clone)]
struct TableLinks {
    at: Addr,
    node: Node,
    path: String,
    table: String,
    table_name: String,
    rows: Vec<(String, String, Vec<NodeRef>)>,
}

/// What one walk of a store found.
#[derive(Debug, Clone, Default)]
struct Scan {
    /// Its query Metanodes, live, each with its source.
    metanodes: Vec<(Addr, Node, Source)>,
    tables: Vec<TableLinks>,
}

/// What was derived, and from which generations.
#[derive(Default)]
pub(crate) struct Cache {
    scans: HashMap<StoreId, (Deps, Scan)>,
    forests: HashMap<Addr, (Deps, Forest)>,
}

/// A row's first column's value as stored, to name the row by.
fn row_label(table: &TableSnap, cells: &std::collections::BTreeMap<String, Stored>) -> String {
    let Some(first) = table.columns.first() else { return String::new() };
    match cells.get(&first.id) {
        Some(Stored::Text(t) | Stored::Unparsed(t) | Stored::Date(t)) => t.clone(),
        Some(Stored::Number(n)) => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                format!("{}", *n as i64)
            } else {
                n.to_string()
            }
        }
        _ => String::new(),
    }
}

impl Pimble {
    /// The generation of what is held of `store`, when what is held is all
    /// of it and kept current; `None` when nothing derived from it can be
    /// kept.
    fn generation(&self, store: StoreId) -> Option<u64> {
        if self.passes_through(store) {
            return None;
        }
        let held = self.held.lock().unwrap();
        let h = held.store(store)?;
        (h.live && h.complete).then_some(h.generation)
    }

    /// The generations of `stores` now, when every one can be kept.
    fn deps_of(&self, stores: impl IntoIterator<Item = StoreId>) -> Option<Deps> {
        let mut out: Deps = Vec::new();
        for store in stores {
            if out.iter().any(|(s, _)| *s == store) {
                continue;
            }
            out.push((store, self.generation(store)?));
        }
        out.sort_by_key(|(s, _)| s.0);
        Some(out)
    }

    /// Whether `deps` are the generations now.
    fn still(&self, deps: &Deps) -> bool {
        deps.iter().all(|(store, generation)| self.generation(*store) == Some(*generation))
    }

    /// What a walk of `store` finds: its query Metanodes and the tables
    /// whose link columns link anywhere, in the walk's order (through
    /// mounts, as [`Pimble::walk`] goes). Kept until a store it read
    /// changes.
    async fn scan(&self, store: &Store) -> Result<Scan, ApiError> {
        if let Some((deps, scan)) = self.refs.lock().unwrap().scans.get(&store.id) {
            if self.still(deps) {
                return Ok(scan.clone());
            }
        }
        let before = self.deps_of([store.id]);
        let walked = self.walk(store).await?;
        let mut scan = Scan::default();
        let mut stores: HashSet<StoreId> = HashSet::from([store.id]);
        for w in walked {
            stores.insert(w.addr.store);
            if let Some(spec) = &w.node.metadata.metanode {
                if matches!(spec.source.query, Query::Match { .. }) && !scan.metanodes.iter().any(|(a, _, _)| *a == w.addr) {
                    scan.metanodes.push((w.addr, w.node.clone(), spec.source.clone()));
                }
            }
            let snap = sheets_of(&w.node);
            for table in &snap.tables {
                let cols: Vec<&str> = table.columns.iter().filter(|c| matches!(c.ty, ColumnType::NodeLink { .. })).map(|c| c.id.as_str()).collect();
                if cols.is_empty() {
                    continue;
                }
                let rows: Vec<(String, String, Vec<NodeRef>)> = table
                    .rows
                    .iter()
                    .filter_map(|r| {
                        let targets: Vec<NodeRef> = cols
                            .iter()
                            .filter_map(|c| match r.cells.get(*c) {
                                Some(Stored::Node(n)) => Some(*n),
                                _ => None,
                            })
                            .collect();
                        (!targets.is_empty()).then(|| (r.id.clone(), row_label(table, &r.cells), targets))
                    })
                    .collect();
                if rows.is_empty() {
                    continue;
                }
                scan.tables.push(TableLinks {
                    at: w.addr,
                    node: w.node.clone(),
                    path: w.path.clone(),
                    table: table.id.clone(),
                    table_name: table.name.clone(),
                    rows,
                });
            }
        }
        // Kept only when the walk read stores that are held whole, and
        // nothing changed in them while it walked (a walk holds what it
        // reads, which counts as a change the first time).
        if let (Some(before), Some(deps)) = (before, self.deps_of(stores)) {
            if before.iter().all(|d| deps.contains(d)) {
                self.refs.lock().unwrap().scans.insert(store.id, (deps, scan.clone()));
            }
        }
        Ok(scan)
    }

    /// A Metanode's forest, kept until a store it read changes.
    async fn forest_kept(&self, metanode: Addr) -> Result<Forest, ApiError> {
        if let Some((deps, forest)) = self.refs.lock().unwrap().forests.get(&metanode) {
            if self.still(deps) {
                return Ok(forest.clone());
            }
        }
        let before = self.deps_of([metanode.store]);
        let forest = self.metanode_source(metanode).await?;
        let mut stores = vec![metanode.store];
        stores.extend(forest.scanned.iter().copied());
        stores.extend(forest.entries.iter().map(|e| e.addr.store));
        if let Some(deps) = self.deps_of(stores) {
            // Nothing in the Metanode's own store moved while it was read.
            let unchanged = before.as_ref().is_some_and(|b| b.iter().all(|d| deps.contains(d)));
            if unchanged {
                self.refs.lock().unwrap().forests.insert(metanode, (deps, forest.clone()));
            }
        }
        Ok(forest)
    }

    /// The Metanodes whose queries find `target` (docs/PROPERTIES_CONTRACT.md
    /// section 5, "Shown in"): the query Metanodes of every open store whose
    /// match looks where `target` lives, evaluated as the explorer evaluates
    /// them ([`Pimble::metanode_source`], so the limit and the exclusions
    /// count), each once, by name. A Metanode whose conditions do not hold
    /// for `target` is not evaluated.
    pub async fn shown_in(&self, target: Addr) -> Result<Vec<ShownIn>, ApiError> {
        let doc = self.doc(target).await?;
        let fields = doc.fields().map_err(ApiError::unreadable)?;
        if fields.deleted_at.is_some() {
            return Ok(Vec::new());
        }
        let today = crate::book::SheetBook::local_today();
        let mut out: Vec<ShownIn> = Vec::new();
        for store in self.known_stores().await? {
            let scan = match self.scan(&store).await {
                Ok(scan) => scan,
                Err(e) => {
                    tracing::debug!("the Metanodes of store {} could not be read: {e}", store.id);
                    continue;
                }
            };
            for (metanode, node, source) in scan.metanodes {
                if metanode == target || out.iter().any(|s| s.metanode == metanode) {
                    continue;
                }
                let Query::Match { within, .. } = &source.query else { continue };
                let looks_there = match within {
                    None => metanode.store == target.store,
                    Some(Within::Store(s)) => *s == target.store,
                    Some(Within::Under(n)) => parse_node_ref(n).is_some_and(|(s, _)| s == target.store),
                    Some(Within::Everywhere) => true,
                };
                if !looks_there {
                    continue;
                }
                // The cheap judgement first: most Metanodes do not find it.
                let Some(matcher) = Matcher::of(&source.query, today) else { continue };
                if !matcher.matches(&doc, &fields) {
                    continue;
                }
                match self.forest_kept(metanode).await {
                    Ok(forest) if forest.is_found(target) => out.push(ShownIn { metanode, node }),
                    Ok(_) => {}
                    Err(e) => tracing::debug!("the Metanode {} could not be read: {e}", metanode.link()),
                }
            }
        }
        out.sort_by_key(|s| label(&s.node).to_lowercase());
        Ok(out)
    }

    /// The nodes whose text links to `target` (docs/PROPERTIES_CONTRACT.md
    /// section 5, "Links here"): its own store's, as its backend knows them
    /// (a server's search index, the browser's vault from the documents it
    /// holds), each readable and live, by name. A link from another store
    /// is not known: an index is per store.
    pub async fn backlinks(&self, target: Addr) -> Result<Vec<LinksHere>, ApiError> {
        let ids = self.backend().backlinks(target.store, target.node).await?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let addr = Addr::new(target.store, id);
            if let Ok(node) = self.held_node(addr).await {
                out.push(LinksHere { addr, node });
            }
        }
        out.sort_by_key(|l| label(&l.node).to_lowercase());
        Ok(out)
    }

    /// Every table, in every open store, whose link-to-a-node column links
    /// to `target`, with the rows that do (docs/SHEETS_CONTRACT.md section
    /// 8, "rows that point here"; docs/PROPERTIES_CONTRACT.md section 5):
    /// stores as listed, nodes in the order a walk of each meets them
    /// (through mounts, so a table seen through one is met there too),
    /// tables in their node's order.
    pub async fn rows_pointing_here(&self, target: Addr) -> Result<Vec<RowsHere>, ApiError> {
        let target_ref = node_ref(target);
        let mut out = Vec::new();
        for store in self.known_stores().await? {
            let Ok(scan) = self.scan(&store).await else { continue };
            for t in scan.tables {
                let rows: Vec<(String, String)> = t.rows.into_iter().filter(|(_, _, to)| to.contains(&target_ref)).map(|(id, label, _)| (id, label)).collect();
                if rows.is_empty() {
                    continue;
                }
                out.push(RowsHere { at: t.at, node: t.node, path: t.path, table: t.table, table_name: t.table_name, rows });
            }
        }
        Ok(out)
    }
}

/// What links to `target` among `docs` (one store's live documents), for a
/// backend that holds them itself and has no index (the browser's vault):
/// a document whose text (its content's links, its canvases' cards) names
/// it, not itself, each once.
pub fn backlinks_among<'a>(target: Addr, docs: impl IntoIterator<Item = (pimble_core::NodeId, &'a NodeDoc)>) -> Vec<pimble_core::NodeId> {
    let mut out = Vec::new();
    for (id, doc) in docs {
        if id == target.node || out.contains(&id) {
            continue;
        }
        let live = doc.fields().is_ok_and(|f| f.deleted_at.is_none() && f.attached_to.is_none());
        if live && doc.links().iter().any(|l| l.target.store == target.store && l.target.node == target.node) {
            out.push(id);
        }
    }
    out
}
