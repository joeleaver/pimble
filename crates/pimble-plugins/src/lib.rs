//! What search finds in a node, per node type.
//!
//! [`index_units_for`] is the registry of search units per built-in node type: the
//! server's search index asks it for a node's [`IndexUnit`]s (the pieces the index
//! chunks and embeds, see [`pimble_core::IndexUnit`]). It runs on the server, which runs
//! no UI plugins, so it is a plain table of the built-in types rather than a plugin trait.
//!
//! The WASM plugin host (guests running plain rinch over the Pimble API,
//! docs/PLUGINS_CONTRACT.md) is `pimble-plugin-host`, whose wasmtime engine only the
//! desktop app turns on.

use pimble_core::{node_types, IndexUnit, UnitKind};
use pimble_crdt::NodeDoc;

/// The node types [`index_units_for`] knows. A type not listed here (a mount, a type
/// from a newer version) indexes with no units: its title is still found.
pub const INDEXED_NODE_TYPES: &[&str] =
    &[node_types::DOCUMENT, node_types::FOLDER, node_types::BOARD, node_types::SHEET, node_types::DISCUSSION];

/// The search units of a node of type `node_type` whose document is `content` (the
/// node document's bytes). Content that does not load as a node document has no units,
/// so a damaged document is still found by its title rather than failing the index.
pub fn index_units_for(node_type: &str, content: &[u8]) -> Vec<IndexUnit> {
    // A folder is only its children, which are indexed on their own.
    if node_type == node_types::FOLDER || !INDEXED_NODE_TYPES.contains(&node_type) {
        return Vec::new();
    }
    let Ok(doc) = NodeDoc::load(content) else { return Vec::new() };
    match node_type {
        // The text, then the typed cells of the tables in it
        // (docs/SHEETS_CONTRACT.md section 10), then the node's own fields.
        node_types::DOCUMENT => {
            let mut units = doc.units();
            units.extend(pimble_sheet::index::index_units(&doc));
            units.extend(field_units(&doc));
            units
        }
        // A canvas is only its board (docs/IMAGE_MARKUP_CONTRACT.md), so what search
        // finds in it is the text written on the board, never the unused `content` root.
        node_types::BOARD => doc.board_units(),
        // A sheet is only its tables (docs/SHEETS_CONTRACT.md section 1), so what search
        // finds in it is its fields and their typed cells.
        node_types::SHEET => {
            let mut units = field_units(&doc);
            units.extend(pimble_sheet::index::index_units(&doc));
            units
        }
        // A discussion is only its messages (docs/DISCUSSIONS_CONTRACT.md section 9):
        // each message that is not deleted, with its author's name.
        node_types::DISCUSSION => pimble_discussion::Discussion::of_node(&doc).read().index_units(),
        _ => Vec::new(),
    }
}

/// A node's own fields as index units (`name: value`), so "Acme" finds the
/// project whose client field is Acme.
fn field_units(doc: &NodeDoc) -> Vec<IndexUnit> {
    doc.node_fields()
        .into_iter()
        .filter(|(_, v)| !v.trim().is_empty())
        .map(|(n, v)| IndexUnit::new(UnitKind::Field(n.clone()), format!("field:{n}"), format!("{n}: {v}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_documents_units_are_its_node_docs() {
        let content = NodeDoc::from_plain_text("hello\nworld").unwrap();
        let units = index_units_for("document", &content.save());
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].kind, UnitKind::Prose);
        assert_eq!(units[0].text, "hello");
        assert_eq!(units[1].text, "world");
    }

    #[test]
    fn an_empty_document_has_no_text() {
        let empty = NodeDoc::new().save();
        assert!(index_units_for("document", &empty).iter().all(|u| u.text.is_empty()));
    }

    #[test]
    fn a_folder_has_no_units() {
        let content = NodeDoc::from_plain_text("ignored").unwrap().save();
        assert!(index_units_for("folder", &content).is_empty());
    }

    #[test]
    fn an_unknown_type_or_unreadable_content_has_no_units() {
        let content = NodeDoc::from_plain_text("words").unwrap().save();
        assert!(index_units_for("mount", &content).is_empty());
        assert!(index_units_for("document", b"not a document").is_empty());
    }
}
