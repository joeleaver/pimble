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
