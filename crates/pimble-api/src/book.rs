//! What the formula engine sees beyond the node it computes
//! (docs/SHEETS_CONTRACT.md sections 6 and 8): a [`SheetBook`] of plain
//! values, filled from held documents by [`Pimble::sheet_book`]. The
//! values part is tested without a server; the filling reads what the API
//! holds (the home node, the notes its formulas name, and every store's
//! titles when a name must be looked up), so a second computation over the
//! same notes asks the server for nothing.

use std::collections::{HashMap, HashSet};

use chrono::NaiveDate;
use pimble_crdt::NodeDoc;
use pimble_sheet::book::{find_by_path, formulas_of, named_nodes, serial_of, shortest_path};
use pimble_sheet::doc::Sheets;
use pimble_sheet::formula;
use pimble_sheet::formula::ast::{Expr, ExprKind, NoteName};
use pimble_sheet::{Book, NodeInfo, NodeRef, Reach, SheetsSnap, Titles};

use crate::names::{label, store_tops};
use crate::{Addr, ApiError, Pimble};

/// How many notes deep the formulas of named notes are followed.
const HOPS: usize = 4;

/// Marks an entry of `shown` that is a column's `under` (a node name to
/// resolve) rather than a formula, so every node is indexed by title.
pub const UNDER_MARK: &str = "under:";

/// Why a node's sheets cannot be read (the states of contract section 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Absent {
    Deleted,
    NoAccess,
    Loading,
    Missing,
}

/// A node as the book knows it: its title and the parent it was found under.
#[derive(Debug, Clone, PartialEq)]
pub struct Known {
    pub title: String,
    pub parent: Option<NodeRef>,
    /// Its fields (contract section 8), as typed.
    pub fields: Vec<(String, String)>,
}

/// The engine's view of the stores from one node ("this note").
#[derive(Debug, Clone)]
pub struct SheetBook {
    home: NodeRef,
    snaps: HashMap<NodeRef, SheetsSnap>,
    absent: HashMap<NodeRef, Absent>,
    nodes: HashMap<NodeRef, Known>,
    today: NaiveDate,
}

impl SheetBook {
    pub fn new(home: NodeRef, home_snap: SheetsSnap, today: NaiveDate) -> Self {
        let mut snaps = HashMap::new();
        snaps.insert(home, home_snap);
        Self { home, snaps, absent: HashMap::new(), nodes: HashMap::new(), today }
    }

    /// Today in local time, as the app takes it.
    pub fn local_today() -> NaiveDate {
        chrono::Local::now().date_naive()
    }

    pub fn home_snap(&self) -> &SheetsSnap {
        self.snaps.get(&self.home).expect("the home node's sheets are always held")
    }

    /// The home node's tables as they are now (after a write).
    pub fn set_home_snap(&mut self, snap: SheetsSnap) {
        self.snaps.insert(self.home, snap);
    }

    pub fn today_date(&self) -> NaiveDate {
        self.today
    }

    /// Another node's tables, readable.
    pub fn add_sheets(&mut self, node: NodeRef, snap: SheetsSnap) {
        self.absent.remove(&node);
        self.snaps.insert(node, snap);
    }

    /// A node whose tables cannot be read, and why.
    pub fn add_absent(&mut self, node: NodeRef, why: Absent) {
        if node != self.home {
            self.absent.insert(node, why);
        }
    }

    /// A node's title and the parent it sits under, for naming and paths.
    pub fn add_node(&mut self, node: NodeRef, title: impl Into<String>, parent: Option<NodeRef>) {
        let fields = self.nodes.get(&node).map(|k| k.fields.clone()).unwrap_or_default();
        self.nodes.insert(node, Known { title: title.into(), parent, fields });
    }

    /// A known node's fields.
    pub fn set_fields(&mut self, node: NodeRef, fields: Vec<(String, String)>) {
        if let Some(k) = self.nodes.get_mut(&node) {
            k.fields = fields;
        }
    }

    pub fn has_sheets(&self, node: &NodeRef) -> bool {
        self.snaps.contains_key(node)
    }

    fn known(&self, node: &NodeRef) -> Option<&Known> {
        self.nodes.get(node)
    }

    /// The known children of `parent`, with their titles.
    pub fn children_of(&self, parent: &NodeRef) -> Vec<(NodeRef, String)> {
        let mut out: Vec<(NodeRef, String)> =
            self.nodes.iter().filter(|(_, k)| k.parent == Some(*parent)).map(|(n, k)| (*n, k.title.clone())).collect();
        out.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
        out
    }
}

impl Titles for SheetBook {
    fn candidates(&self) -> Vec<NodeRef> {
        self.nodes.keys().copied().collect()
    }

    fn title_and_parent(&self, node: &NodeRef) -> Option<(String, Option<NodeRef>)> {
        self.known(node).map(|k| (k.title.clone(), k.parent))
    }
}

impl Book for SheetBook {
    fn home(&self) -> NodeRef {
        self.home
    }

    fn sheets(&self, node: &NodeRef) -> Reach<'_> {
        if let Some(s) = self.snaps.get(node) {
            return Reach::Ready(s);
        }
        match self.absent.get(node) {
            Some(Absent::Deleted) => Reach::Deleted,
            Some(Absent::NoAccess) => Reach::NoAccess,
            Some(Absent::Loading) => Reach::Loading,
            Some(Absent::Missing) | None => Reach::Missing,
        }
    }

    fn node(&self, node: &NodeRef) -> Option<NodeInfo> {
        let k = self.known(node)?;
        Some(NodeInfo { title: k.title.clone(), fields: k.fields.clone(), parent: k.parent })
    }

    fn find_nodes(&self, name_or_path: &str) -> Vec<NodeRef> {
        find_by_path(self, name_or_path)
    }

    fn display_name(&self, node: &NodeRef) -> String {
        shortest_path(self, node)
    }

    fn today(&self) -> f64 {
        serial_of(self.today)
    }
}

/// The note names a formula as typed uses before a `!` (`'Household
/// budget'!Transactions[amount]`, `Budget!rate`): what the loader must look
/// up before the formula can be stored.
pub fn note_names_in(shown: &str) -> Vec<String> {
    fn walk(e: &Expr, out: &mut Vec<String>) {
        if let ExprKind::Ref(r) = &e.kind {
            if let Some(np) = &r.note {
                if let NoteName::Name(s) = &np.name {
                    if !out.contains(s) {
                        out.push(s.clone());
                    }
                }
            }
        }
        for c in e.children() {
            walk(c, out);
        }
    }
    let mut out = Vec::new();
    if let Ok(expr) = formula::parse(shown) {
        walk(&expr, &mut out);
    }
    out
}

/// Whether a stored formula held by `formula_node` reads table `table_id` of
/// `target`: a same-note reference (no note token) when the two are one
/// node, or a reference carrying `target`'s note token.
pub fn reads_table(stored: &str, formula_node: &NodeRef, target: &NodeRef, table_id: &str) -> bool {
    let token = format!("{{t:{table_id}}}");
    let note = format!("{}!", formula::note_token(target));
    let mut from = 0;
    while let Some(at) = stored[from..].find(&token) {
        let at = from + at;
        let before = &stored[..at];
        if before.ends_with("}!") {
            if before.ends_with(&note) {
                return true;
            }
        } else if formula_node == target {
            return true;
        }
        from = at + token.len();
    }
    false
}

pub fn node_ref(addr: Addr) -> NodeRef {
    NodeRef { store: addr.store, node: addr.node }
}

/// A node's tables, read from its document; empty when it cannot be read.
pub fn sheets_of(node: &pimble_core::Node) -> SheetsSnap {
    match NodeDoc::load(&node.content) {
        Ok(doc) => Sheets::of_node(&doc).read(),
        Err(_) => SheetsSnap::default(),
    }
}

/// What a named note is to a formula.
enum Reached {
    Live(pimble_core::Node),
    Deleted { title: String, parent: Option<pimble_core::NodeId> },
    Absent(Absent),
}

impl Pimble {
    /// The book for `home`: its tables, the notes its stored formulas name
    /// (a few hops deep), and, when a formula names notes or shows them (or
    /// a column chooses among nodes), every store's titles, so names
    /// resolve and paths show as the app shows them. `shown` are formulas as
    /// typed that are about to be stored, and columns' `under`s marked with
    /// [`UNDER_MARK`]: the notes they name are looked up too.
    pub async fn sheet_book(&self, home: Addr, shown: &[&str]) -> Result<SheetBook, ApiError> {
        let home_node = self.held_node(home).await?;
        let mut book = SheetBook::new(node_ref(home), sheets_of(&home_node), SheetBook::local_today());
        book.add_node(node_ref(home), label(&home_node), home_node.parent_id.map(|p| NodeRef { store: home.store, node: p }));
        book.set_fields(node_ref(home), home_node.metadata.fields.clone());
        let mut pending: Vec<NodeRef> = formulas_of(book.home_snap()).into_iter().flat_map(named_nodes).collect();
        let mut wanted: HashSet<String> = HashSet::new();
        for f in shown {
            for name in note_names_in(f) {
                if let Some(last) = name.split('/').map(str::trim).filter(|p| !p.is_empty()).last() {
                    wanted.insert(last.to_lowercase());
                }
            }
        }
        // The notes stored formulas name, and the ones their formulas name.
        let mut seen: HashSet<NodeRef> = HashSet::from([node_ref(home)]);
        for _ in 0..HOPS {
            if pending.is_empty() {
                break;
            }
            let mut next = Vec::new();
            for n in std::mem::take(&mut pending) {
                if !seen.insert(n) {
                    continue;
                }
                match self.reached(Addr { store: n.store, node: n.node }).await {
                    Reached::Live(node) => {
                        wanted.insert(label(&node).to_lowercase());
                        let snap = sheets_of(&node);
                        next.extend(formulas_of(&snap).into_iter().flat_map(named_nodes));
                        book.add_sheets(n, snap);
                        book.add_node(n, label(&node), node.parent_id.map(|p| NodeRef { store: n.store, node: p }));
                        book.set_fields(n, node.metadata.fields.clone());
                    }
                    Reached::Deleted { title, parent } => {
                        // Its title, for "Household budget was deleted".
                        book.add_node(n, title, parent.map(|p| NodeRef { store: n.store, node: p }));
                        book.add_absent(n, Absent::Deleted);
                    }
                    Reached::Absent(why) => book.add_absent(n, why),
                }
            }
            pending = next;
        }
        // A link-to-a-node column chooses among nodes by title: they are all
        // known then (the tables of only the wanted ones are read).
        let node_links = book
            .home_snap()
            .tables
            .iter()
            .any(|t| t.columns.iter().any(|c| matches!(c.ty, pimble_sheet::ColumnType::NodeLink { .. })))
            || shown.iter().any(|s| s.starts_with(UNDER_MARK));
        if !wanted.is_empty() || node_links {
            self.index_titles(&mut book, &wanted).await;
        }
        Ok(book)
    }

    /// A named note, live, deleted (with the title it had), or why not.
    async fn reached(&self, addr: Addr) -> Reached {
        match self.held_node(addr).await {
            Ok(node) => Reached::Live(node),
            Err(e) if e.is_refusal() => Reached::Absent(Absent::NoAccess),
            Err(ApiError::Offline { .. } | ApiError::NotAvailable { .. }) => Reached::Absent(Absent::Loading),
            Err(_) => match self.any_fields(addr).await {
                // A tombstone is not served as a node; its document says so.
                Some(f) if f.deleted_at.is_some() => Reached::Deleted { title: f.title, parent: f.parent_id },
                _ => Reached::Absent(Absent::Missing),
            },
        }
    }

    /// Every node of the stores by title and parent (paths are the tree as
    /// this device sees it, mounts included), and the tables of the ones
    /// whose title is `wanted`.
    async fn index_titles(&self, book: &mut SheetBook, wanted: &HashSet<String>) {
        let Ok(stores) = self.known_stores().await else { return };
        for store in &stores {
            for top in store_tops(store) {
                // A whole store's root goes by the store's name, so a path
                // can start there ('Family/Household budget'!).
                let title = if top.node == store.root_node_id {
                    store.name.clone()
                } else {
                    match self.held_node(top).await {
                        Ok(n) => label(&n),
                        Err(_) => continue,
                    }
                };
                book.add_node(node_ref(top), title, None);
            }
            let Ok(walked) = self.walk(store).await else { continue };
            for w in walked {
                let Some(parent) = w.parent else { continue };
                let title = label(&w.node);
                let r = node_ref(w.addr);
                if wanted.contains(&title.to_lowercase()) && !book.has_sheets(&r) {
                    book.add_sheets(r, sheets_of(&w.node));
                }
                book.add_node(r, title, Some(node_ref(parent)));
                book.set_fields(r, w.node.metadata.fields.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_core::{NodeId, StoreId};

    fn r(n: u128) -> NodeRef {
        NodeRef { store: StoreId(uuid_of(7)), node: NodeId(uuid_of(n)) }
    }

    fn uuid_of(n: u128) -> uuid::Uuid {
        uuid::Uuid::from_u128(n)
    }

    fn book() -> SheetBook {
        let mut b = SheetBook::new(r(1), SheetsSnap::default(), NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
        b.add_node(r(10), "Family", None);
        b.add_node(r(11), "Home", Some(r(10)));
        b.add_node(r(12), "Work", Some(r(10)));
        b.add_node(r(13), "Budget", Some(r(11)));
        b.add_node(r(14), "Budget", Some(r(12)));
        b.add_node(r(15), "Household budget", Some(r(11)));
        b
    }

    #[test]
    fn finds_by_title_and_by_path() {
        let b = book();
        assert_eq!(b.find_nodes("household BUDGET"), vec![r(15)]);
        assert_eq!(b.find_nodes("Budget").len(), 2);
        assert_eq!(b.find_nodes("Home/Budget"), vec![r(13)]);
        // A path is a run of titles: Family is not Budget's parent.
        assert!(b.find_nodes("Family/Budget").is_empty());
        assert_eq!(b.find_nodes("Family/Work/Budget"), vec![r(14)]);
        assert!(b.find_nodes("Work/Household budget").is_empty());
        assert!(b.find_nodes("Nothing").is_empty());
    }

    #[test]
    fn display_names_are_the_shortest_unique_path() {
        let b = book();
        assert_eq!(b.display_name(&r(15)), "Household budget");
        assert_eq!(b.display_name(&r(13)), "Home/Budget");
        assert_eq!(b.display_name(&r(14)), "Work/Budget");
        assert_eq!(b.display_name(&r(99)), "That note");
        // A namesake below a note, under one ancestor: both stay nameable.
        let mut b = book();
        b.add_node(r(20), "Trip costs", Some(r(10)));
        b.add_node(r(21), "Plain", Some(r(10)));
        b.add_node(r(22), "Trip costs", Some(r(21)));
        assert_eq!(b.display_name(&r(20)), "Family/Trip costs");
        assert_eq!(b.display_name(&r(22)), "Plain/Trip costs");
        assert_eq!(b.find_nodes("Family/Trip costs"), vec![r(20)]);
    }

    #[test]
    fn reach_follows_what_was_loaded() {
        let mut b = book();
        b.add_absent(r(13), Absent::Deleted);
        b.add_absent(r(14), Absent::NoAccess);
        b.add_absent(r(15), Absent::Loading);
        assert!(matches!(b.sheets(&r(1)), Reach::Ready(_)));
        assert_eq!(b.sheets(&r(13)), Reach::Deleted);
        assert_eq!(b.sheets(&r(14)), Reach::NoAccess);
        assert_eq!(b.sheets(&r(15)), Reach::Loading);
        assert_eq!(b.sheets(&r(16)), Reach::Missing);
        b.add_sheets(r(15), SheetsSnap::default());
        assert!(matches!(b.sheets(&r(15)), Reach::Ready(_)));
        // 2026-10-19 is serial 46314.
        assert_eq!(b.today(), 46_314.0);
    }

    #[test]
    fn note_names_and_tokens() {
        assert_eq!(
            note_names_in("=SUM('Household budget'!Transactions[amount])+Budget!rate+[@x]"),
            vec!["Household budget".to_string(), "Budget".to_string()]
        );
        assert!(note_names_in("=[@miles]*2").is_empty());
        assert!(note_names_in("=(").is_empty());
        let n = r(15);
        let stored = format!("=SUM({}!{{t:abc}}[{{c:d}}])+{{t:own}}[{{c:e}}]", formula::note_token(&n));
        assert_eq!(named_nodes(&stored), vec![n]);
        assert!(reads_table(&stored, &r(1), &n, "abc"));
        assert!(!reads_table(&stored, &r(1), &n, "own"));
        assert!(reads_table(&stored, &r(1), &r(1), "own"));
        assert!(!reads_table(&stored, &r(1), &r(1), "abc"));
    }
}
