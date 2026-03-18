//! ContentEditable editor helpers.

use rinch::prelude::*;
use rinch_editor::document::EditorDocument;

use crate::state::get_node_content_text;

/// Save current CE content as Automerge bytes.
/// Returns `None` when the CE API isn't available.
pub(crate) fn save_content_via_ce_api(ce_div: &NodeHandle) -> Option<Vec<u8>> {
    let blocks = ce_div.with_ce_api(|api| api.borrow().extract_content())?;
    let mut doc = EditorDocument::from_block_data(&blocks);
    Some(doc.to_bytes())
}

/// Load content bytes into the CE div via the CE API.
pub(crate) fn load_content_into_ce(bytes: &[u8], ce_div: &NodeHandle) {
    if bytes.is_empty() {
        // Empty content: use load_content with an empty block list.
        // This creates a proper empty <p> with a text node and positions
        // the cursor correctly. Using load_html("<p></p>") would create
        // a <p> with no text node, leaving the cursor on the CE root.
        ce_div.with_ce_api(|api| {
            api.borrow_mut().load_content(&[]);
        });
        return;
    }

    if let Ok(doc) = EditorDocument::from_bytes(bytes) {
        // Prefer the block data round-trip: it goes through
        // load_content which sets up text nodes and cursor properly.
        let blocks = doc.to_block_data();
        if blocks.is_empty() {
            ce_div.with_ce_api(|api| {
                api.borrow_mut().load_content(&[]);
            });
        } else {
            ce_div.with_ce_api(|api| {
                api.borrow_mut().load_content(&blocks);
            });
        }
    } else {
        // Fall back to old format — parse as plain text
        let text = get_node_content_text(bytes);
        let html = if text.is_empty() {
            "<p></p>".to_string()
        } else {
            format!("<p>{}</p>", text)
        };
        ce_div.with_ce_api(|api| {
            api.borrow_mut().load_html(&html);
        });
    }
}
