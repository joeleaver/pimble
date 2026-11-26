//! The rows of "Recently Deleted..." (docs/MOVE_CONTRACT.md "Seeing and
//! undoing what was removed") and what each says.

use super::*;

/// One row of "Recently Deleted..." (docs/MOVE_CONTRACT.md "Seeing and
/// undoing what was removed"), copied out of a `pimble_core::DeletedNode`
/// into its own small type: `Clone` and `PartialEq` so the row list can key
/// and diff like any other (`DeletedNode` carries a whole `Node`, with no
/// other reason to be either).
#[derive(Debug, Clone, PartialEq)]
pub struct DeletedRow {
    pub node_id: NodeId,
    /// Never empty: an untitled node reads "Untitled" here already.
    pub title: String,
    /// The folder it was in, when the app holds that folder's title.
    pub parent_title: Option<String>,
    /// When it was deleted (RFC 3339); `None` for a live node no list names.
    pub deleted_at: Option<String>,
    /// For a live node no list names: where "Put Back" moves it.
    pub put_back_under: Option<NodeId>,
    /// What this account may change of the node itself.
    pub access: pimble_core::StoreAccess,
}

/// Turn one `DeletedNode` the server sent into the row "Recently
/// Deleted..." renders and acts on.
pub fn deleted_row(entry: &pimble_core::DeletedNode) -> DeletedRow {
    let title = entry.node.metadata.title.clone();
    DeletedRow {
        node_id: entry.node.id,
        title: if title.is_empty() { "Untitled".to_string() } else { title },
        parent_title: entry.parent_title.clone(),
        deleted_at: entry.deleted_at.clone(),
        put_back_under: entry.put_back_under,
        access: entry.node.access,
    }
}

/// `deleted_at` read the way a person reads a timestamp rather than the RFC
/// 3339 a server sends it as: the date and the minute, seconds and offset
/// dropped. Pulling in a date library for one line is not worth it —
/// pimble-app depends on no date crate of its own.
pub fn readable_instant(rfc3339: &str) -> String {
    match rfc3339.split_once('T') {
        Some((date, rest)) => {
            let time = rest.get(0..5).unwrap_or(rest);
            format!("{date} {time}")
        }
        None => rfc3339.to_string(),
    }
}

/// The second line of a "Recently Deleted..." row: the folder it was in,
/// when known, and either when it was deleted or, for a live node no list
/// names, that it is in none.
pub fn deleted_row_meta(row: &DeletedRow) -> String {
    let mut parts = Vec::new();
    if let Some(parent) = row.parent_title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        parts.push(format!("in {parent}"));
    }
    match &row.deleted_at {
        Some(when) => parts.push(readable_instant(when)),
        None => parts.push("not in any folder".to_string()),
    }
    parts.join(" · ")
}

/// Whether "Put Back" is off for this row (`pimble_core::plan::can_put_back`).
pub fn deleted_row_read_only(store_access: pimble_core::StoreAccess, row: &DeletedRow) -> bool {
    !pimble_core::plan::can_put_back(store_access, row.access)
}
