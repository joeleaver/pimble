//! What the formula engine sees from the MCP (docs/SHEETS_CONTRACT.md
//! sections 6 and 10): a [`Book`] over node documents fetched through the
//! connection. This part is plain values, so it is tested without a server;
//! `load.rs` fills it.

use std::collections::HashMap;

use chrono::NaiveDate;
use pimble_sheet::book::{find_by_path, serial_of, shortest_path};
use pimble_sheet::formula::ast::{Expr, ExprKind, NoteName};
use pimble_sheet::formula;
use pimble_sheet::{Book, NodeInfo, NodeRef, Reach, SheetsSnap, Titles};

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
pub struct McpBook {
    home: NodeRef,
    snaps: HashMap<NodeRef, SheetsSnap>,
    absent: HashMap<NodeRef, Absent>,
    nodes: HashMap<NodeRef, Known>,
    today: NaiveDate,
}

impl McpBook {
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

impl Titles for McpBook {
    fn candidates(&self) -> Vec<NodeRef> {
        self.nodes.keys().copied().collect()
    }

    fn title_and_parent(&self, node: &NodeRef) -> Option<(String, Option<NodeRef>)> {
        self.known(node).map(|k| (k.title.clone(), k.parent))
    }
}

impl Book for McpBook {
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

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_core::{NodeId, StoreId};
    use pimble_sheet::book::named_nodes;

    fn r(n: u128) -> NodeRef {
        NodeRef { store: StoreId(uuid::Uuid::from_u128(7)), node: NodeId(uuid::Uuid::from_u128(n)) }
    }

    fn book() -> McpBook {
        let mut b = McpBook::new(r(1), SheetsSnap::default(), NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
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
