//! What the formula engine can see beyond the node it computes (contract
//! sections 6 and 8): other notes' tables, nodes' titles and fields, and how
//! to name them. The app implements it over the documents it holds, the MCP
//! over the documents it fetches, and tests over plain values.

use crate::model::{NodeRef, SheetsSnap};

/// Whether a node's sheets can be read right now (contract section 6, "When a
/// referenced note is not there").
#[derive(Debug, Clone, PartialEq)]
pub enum Reach<'a> {
    Ready(&'a SheetsSnap),
    /// Tombstoned; `Put Back` brings it back.
    Deleted,
    /// Not readable by this person.
    NoAccess,
    /// Not arrived yet (a mount connecting, a replica never synced).
    Loading,
    /// No such node.
    Missing,
}

/// A node as formulas see it.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeInfo {
    pub title: String,
    /// The node's fields (contract section 8), name and value as typed.
    pub fields: Vec<(String, String)>,
    pub parent: Option<NodeRef>,
}

pub trait Book {
    /// The node whose sheets are being computed ("this note").
    fn home(&self) -> NodeRef;

    /// A node's sheets, `home()` included.
    fn sheets(&self, node: &NodeRef) -> Reach<'_>;

    /// A node's title, fields and parent, when it can be read.
    fn node(&self, node: &NodeRef) -> Option<NodeInfo>;

    /// The nodes a typed name could mean: a title, or a path of titles joined
    /// by `/` from any ancestor (`Home/Budget`), case-insensitive. Used only
    /// when turning a shown formula into its stored form.
    fn find_nodes(&self, name_or_path: &str) -> Vec<NodeRef>;

    /// The shortest path (titles joined by `/`) that names this node uniquely
    /// for this person, for showing a stored formula.
    fn display_name(&self, node: &NodeRef) -> String;

    /// Today, as an Excel serial number (days since 1899-12-30), local time.
    fn today(&self) -> f64;
}

/// What naming nodes by path needs from a book: the nodes it can name, and
/// each one's title (as the tree shows it) and parent.
pub trait Titles {
    /// Every node a name may mean.
    fn candidates(&self) -> Vec<NodeRef>;

    /// A node's title and parent, when it is known.
    fn title_and_parent(&self, node: &NodeRef) -> Option<(String, Option<NodeRef>)>;
}

fn lower(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// The nodes a typed name could mean (what [`Book::find_nodes`] answers): a
/// title, or a path of titles joined by `/` from any ancestor, compared
/// case-insensitively, in a stable order.
pub fn find_by_path(titles: &dyn Titles, name_or_path: &str) -> Vec<NodeRef> {
    let parts: Vec<String> = name_or_path.split('/').map(lower).filter(|p| !p.is_empty()).collect();
    let Some(last) = parts.last() else { return Vec::new() };
    let mut found: Vec<NodeRef> = titles
        .candidates()
        .into_iter()
        .filter(|n| {
            let Some((title, mut parent)) = titles.title_and_parent(n) else { return false };
            if lower(&title) != *last {
                return false;
            }
            // Each earlier part names the parent of the part after it: a path
            // is a run of titles down the tree, starting anywhere. (A path that
            // could skip levels would leave a note with a namesake below it,
            // under one ancestor, with no unique name.)
            for part in parts.iter().rev().skip(1) {
                let Some(p) = parent else { return false };
                let Some((up, above)) = titles.title_and_parent(&p) else { return false };
                if lower(&up) != *part {
                    return false;
                }
                parent = above;
            }
            true
        })
        .collect();
    found.sort_by_key(|n| (n.store.0, n.node.0));
    found
}

/// The shortest path from an ancestor that names `node` alone (what
/// [`Book::display_name`] answers), or "That note" for a node not known.
pub fn shortest_path(titles: &dyn Titles, node: &NodeRef) -> String {
    let Some((title, mut parent)) = titles.title_and_parent(node) else { return "That note".to_string() };
    let mut path = title;
    for _ in 0..12 {
        if find_by_path(titles, &path).len() <= 1 {
            break;
        }
        let Some(p) = parent else { break };
        let Some((up, above)) = titles.title_and_parent(&p) else { break };
        path = format!("{up}/{path}");
        parent = above;
    }
    path
}

/// The nodes a stored formula names (`{n:<store>/<node>}`), in order.
pub fn named_nodes(stored: &str) -> Vec<NodeRef> {
    let mut out = Vec::new();
    let mut rest = stored;
    while let Some(at) = rest.find("{n:") {
        rest = &rest[at + 3..];
        let Some(end) = rest.find('}') else { break };
        if let Some((s, n)) = rest[..end].split_once('/') {
            if let (Ok(s), Ok(n)) = (uuid::Uuid::parse_str(s), uuid::Uuid::parse_str(n)) {
                out.push(NodeRef { store: pimble_core::StoreId(s), node: pimble_core::NodeId(n) });
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
            if let crate::ColumnType::Formula { formula, .. } = &c.ty {
                all.push(formula.as_str());
            }
        }
    }
    all
}

/// A date as an Excel serial number (what [`Book::today`] answers).
pub fn serial_of(d: chrono::NaiveDate) -> f64 {
    use chrono::Datelike;
    crate::formula::dates::serial(d.year() as i64, d.month() as i64, d.day() as i64) as f64
}
