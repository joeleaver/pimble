//! What the formula engine sees from the MCP (docs/SHEETS_CONTRACT.md
//! sections 6 and 10): a [`Book`] over node documents fetched through the
//! connection. This part is plain values, so it is tested without a server;
//! `load.rs` fills it.

use std::collections::HashMap;

use chrono::{Datelike, NaiveDate};
use pimble_core::{NodeId, StoreId};
use pimble_sheet::formula::ast::{Expr, ExprKind, NoteName};
use pimble_sheet::formula::{self, dates};
use pimble_sheet::{Book, ColumnType, NodeInfo, NodeRef, Reach, SheetsSnap};

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
        self.nodes.insert(node, Known { title: title.into(), parent });
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

fn lower(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
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
        Some(NodeInfo { title: k.title.clone(), fields: Vec::new(), parent: k.parent })
    }

    fn find_nodes(&self, name_or_path: &str) -> Vec<NodeRef> {
        let parts: Vec<String> = name_or_path.split('/').map(lower).filter(|p| !p.is_empty()).collect();
        let Some(last) = parts.last() else { return Vec::new() };
        let mut found: Vec<NodeRef> = self
            .nodes
            .iter()
            .filter(|(_, k)| lower(&k.title) == *last)
            .filter(|(_, k)| {
                // Each earlier part names the parent of the part after it: a
                // path is a run of titles down the tree, starting anywhere.
                // (A path that could skip levels would leave a note with a
                // namesake below it, under one ancestor, with no unique name.)
                let mut parent = k.parent;
                for part in parts.iter().rev().skip(1) {
                    let Some(p) = parent else { return false };
                    let Some(up) = self.known(&p) else { return false };
                    if lower(&up.title) != *part {
                        return false;
                    }
                    parent = up.parent;
                }
                true
            })
            .map(|(n, _)| *n)
            .collect();
        found.sort_by_key(|n| (n.store.0, n.node.0));
        found
    }

    fn display_name(&self, node: &NodeRef) -> String {
        let Some(k) = self.known(node) else { return "That note".to_string() };
        let mut path = k.title.clone();
        let mut parent = k.parent;
        // The shortest path from an ancestor that names this node alone.
        for _ in 0..12 {
            if self.find_nodes(&path).len() <= 1 {
                break;
            }
            let Some(p) = parent else { break };
            let Some(up) = self.known(&p) else { break };
            path = format!("{}/{path}", up.title);
            parent = up.parent;
        }
        path
    }

    fn today(&self) -> f64 {
        serial_of(self.today)
    }
}

/// A date as an Excel serial number.
pub fn serial_of(d: NaiveDate) -> f64 {
    dates::serial(d.year() as i64, d.month() as i64, d.day() as i64) as f64
}

/// The nodes a stored formula names (`{n:<store>/<node>}`).
pub fn named_nodes(stored: &str) -> Vec<NodeRef> {
    let mut out = Vec::new();
    let mut rest = stored;
    while let Some(at) = rest.find("{n:") {
        rest = &rest[at + 3..];
        let Some(end) = rest.find('}') else { break };
        if let Some((s, n)) = rest[..end].split_once('/') {
            if let (Ok(s), Ok(n)) = (uuid::Uuid::parse_str(s), uuid::Uuid::parse_str(n)) {
                out.push(NodeRef { store: StoreId(s), node: NodeId(n) });
            }
        }
        rest = &rest[end..];
    }
    out
}

/// Every stored formula of a node's tables: summaries' sources and formula
/// columns'.
pub fn formulas_of(snap: &SheetsSnap) -> Vec<&str> {
    let mut all = Vec::new();
    for t in &snap.tables {
        if let Some(s) = &t.source {
            all.push(s.as_str());
        }
        for c in &t.columns {
            if let ColumnType::Formula { formula, .. } = &c.ty {
                all.push(formula.as_str());
            }
        }
    }
    all
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
