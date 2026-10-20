//! Filling a [`McpBook`] through the connection: the home node's tables, the
//! notes its stored formulas name (a few hops deep), and, when a formula
//! names notes or shows them, the titles of the open stores' nodes so names
//! resolve and paths show as the app shows them.

use std::collections::HashSet;

use pimble_client::PimbleClient;
use pimble_core::{LinkResolution, Node};
use pimble_crdt::NodeDoc;
use pimble_sheet::doc::Sheets;
use pimble_sheet::{NodeRef, SheetsSnap};

use super::book::{formulas_of, named_nodes, Absent, McpBook};
use crate::nodes::{self, label, Addr};

/// How many notes deep the formulas of named notes are followed.
const HOPS: usize = 4;

pub fn node_ref(addr: Addr) -> NodeRef {
    NodeRef { store: addr.store, node: addr.node }
}

/// A node's tables, read from its document; empty when it cannot be read.
pub fn sheets_of(node: &Node) -> SheetsSnap {
    match NodeDoc::load(&node.content) {
        Ok(doc) => Sheets::of_node(&doc).read(),
        Err(_) => SheetsSnap::default(),
    }
}

fn absent_from(message: &str) -> Absent {
    let m = message.to_lowercase();
    if m.contains("forbidden") || m.contains("denied") || m.contains("no grant") || m.contains("-32004") {
        Absent::NoAccess
    } else if m.contains("connecting") || m.contains("not synced") {
        Absent::Loading
    } else {
        Absent::Missing
    }
}

/// The book for `home`, whose node (and so its tables) was just fetched.
/// `shown` are formulas as typed that are about to be stored: the notes they
/// name are looked up too.
/// Marks an entry of `shown` that is a column's `under` (a node name to
/// resolve) rather than a formula, so every node is indexed by title.
pub const UNDER_MARK: &str = "under:";

pub async fn load_book(client: &PimbleClient, home: Addr, home_node: &Node, shown: &[&str]) -> McpBook {
    let mut book = McpBook::new(node_ref(home), sheets_of(home_node), McpBook::local_today());
    book.add_node(node_ref(home), label(home_node), home_node.parent_id.map(|p| NodeRef { store: home.store, node: p }));
    book.set_fields(node_ref(home), home_node.metadata.fields.clone());
    let mut pending: Vec<NodeRef> = formulas_of(book.home_snap()).into_iter().flat_map(named_nodes).collect();
    let mut wanted: HashSet<String> = HashSet::new();
    for f in shown {
        for name in super::book::note_names_in(f) {
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
        for n in pending.drain(..) {
            if !seen.insert(n) {
                continue;
            }
            match client.get_node(n.store, n.node).await {
                Ok(node) => {
                    wanted.insert(label(&node).to_lowercase());
                    let deleted = matches!(client.resolve_link(n.store, n.node).await, Ok(LinkResolution::Deleted { .. }));
                    if deleted {
                        book.add_absent(n, Absent::Deleted);
                    } else {
                        let snap = sheets_of(&node);
                        next.extend(formulas_of(&snap).into_iter().flat_map(named_nodes));
                        book.add_sheets(n, snap);
                    }
                    book.add_node(n, label(&node), node.parent_id.map(|p| NodeRef { store: n.store, node: p }));
                    book.set_fields(n, node.metadata.fields.clone());
                }
                Err(e) => {
                    // A tombstone is not served as a node; the link says so.
                    let why = match client.resolve_link(n.store, n.node).await {
                        Ok(LinkResolution::Deleted { .. }) => Absent::Deleted,
                        _ => absent_from(&e.to_string()),
                    };
                    if why == Absent::Deleted {
                        // Its title, for "Household budget was deleted".
                        if let Ok(deleted) = client.list_deleted(n.store).await {
                            if let Some(d) = deleted.iter().find(|d| d.node.id == n.node) {
                                book.add_node(n, label(&d.node), d.node.parent_id.map(|p| NodeRef { store: n.store, node: p }));
                            }
                        }
                    }
                    book.add_absent(n, why);
                }
            }
        }
        pending = next;
    }
    // A link-to-a-node column chooses among nodes by title: they are all
    // known then (the tables of only the wanted ones are read).
    let node_links = book.home_snap().tables.iter().any(|t| t.columns.iter().any(|c| matches!(c.ty, pimble_sheet::ColumnType::NodeLink { .. })))
        || shown.iter().any(|s| s.starts_with(UNDER_MARK));
    if !wanted.is_empty() || node_links {
        index_titles(client, &mut book, &wanted).await;
    }
    book
}

/// Every node of the open stores by title and parent (paths are the tree as
/// this device sees it, mounts included), and the tables of the ones whose
/// title is `wanted`.
async fn index_titles(client: &PimbleClient, book: &mut McpBook, wanted: &HashSet<String>) {
    let Ok(stores) = client.list_stores().await else { return };
    let mut seen = HashSet::new();
    for store in &stores {
        let tops = nodes::store_tops(store);
        let mut queue: Vec<(Addr, Option<NodeRef>)> = Vec::new();
        for top in tops {
            // A whole store's root goes by the store's name, so a path can
            // start there ('Family/Household budget'!).
            let title = if top.node == store.root_node_id {
                store.name.clone()
            } else {
                match client.get_node(top.store, top.node).await {
                    Ok(n) => label(&n),
                    Err(_) => continue,
                }
            };
            book.add_node(node_ref(top), title, None);
            queue.push((top, None));
        }
        while let Some((at, _)) = queue.pop() {
            if !seen.insert(at) {
                continue;
            }
            let Ok(children) = nodes::children(client, at).await else { continue };
            for (addr, node) in children {
                let title = label(&node);
                let r = node_ref(addr);
                if wanted.contains(&title.to_lowercase()) && !book.has_sheets(&r) {
                    book.add_sheets(r, sheets_of(&node));
                }
                book.add_node(r, title, Some(node_ref(at)));
                book.set_fields(r, node.metadata.fields.clone());
                queue.push((addr, Some(node_ref(at))));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_read_as_their_state() {
        assert_eq!(absent_from("Forbidden: access denied (-32004)"), Absent::NoAccess);
        assert_eq!(absent_from("no grant for this document"), Absent::NoAccess);
        assert_eq!(absent_from("mount source is connecting"), Absent::Loading);
        assert_eq!(absent_from("Node not found"), Absent::Missing);
    }
}
