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

/// Convert content bytes to HTML for rendering in the CE div.
fn content_bytes_to_html(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "<p><br></p>".to_string();
    }
    if let Ok(doc) = EditorDocument::from_bytes(bytes) {
        let html = doc.to_html();
        if html.is_empty() { "<p><br></p>".to_string() } else { html }
    } else {
        // Fall back to old format
        let text = get_node_content_text(bytes);
        if text.is_empty() {
            "<p><br></p>".to_string()
        } else {
            format!("<p>{}</p>", text)
        }
    }
}

/// Load content bytes into the CE div via the CE API.
pub(crate) fn load_content_into_ce(bytes: &[u8], ce_div: &NodeHandle) {
    let html = content_bytes_to_html(bytes);
    ce_div.with_ce_api(|api| {
        api.borrow_mut().load_html(&html);
    });
}
