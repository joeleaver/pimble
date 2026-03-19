//! ContentEditable editor helpers.

use pimble_core::{NodeId, StoreId};
use rinch::prelude::*;
use rinch_editor::document::EditorDocument;

use crate::state::{get_node_content_text, AppStore};

/// Save current CE content as Automerge bytes.
/// Returns `None` when the CE API isn't available.
pub(crate) fn save_content_via_ce_api(ce_div: &NodeHandle) -> Option<Vec<u8>> {
    let blocks = ce_div.with_ce_api(|api| api.borrow().extract_content())?;
    let mut doc = EditorDocument::from_block_data(&blocks);
    Some(doc.to_bytes())
}

/// Convert automerge content bytes to HTML, using the cache when available.
fn content_to_html(bytes: &[u8], store: AppStore, store_id: StoreId, node_id: NodeId) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }

    // Check cache first
    let cached = untracked(|| {
        store.html_cache.with(|cache| {
            cache.get(&(store_id, node_id)).cloned()
        })
    });
    if let Some(html) = cached {
        return Some(html);
    }

    // Parse and cache
    if let Ok(doc) = EditorDocument::from_bytes(bytes) {
        let html = doc.to_html();
        if !html.is_empty() {
            store.html_cache.update(|cache| {
                cache.insert((store_id, node_id), html.clone());
            });
            return Some(html);
        }
    } else {
        // Fall back to old format
        let text = get_node_content_text(bytes);
        if !text.is_empty() {
            let html = format!("<p>{}</p>", text);
            store.html_cache.update(|cache| {
                cache.insert((store_id, node_id), html.clone());
            });
            return Some(html);
        }
    }

    None
}

/// Load content bytes into the CE div via the CE API.
pub(crate) fn load_content_into_ce(
    bytes: &[u8],
    ce_div: &NodeHandle,
    store: AppStore,
    store_id: StoreId,
    node_id: NodeId,
) {
    if let Some(html) = content_to_html(bytes, store, store_id, node_id) {
        ce_div.with_ce_api(|api| {
            api.borrow_mut().load_html(&html);
        });
    } else {
        ce_div.with_ce_api(|api| {
            api.borrow_mut().load_content(&[]);
        });
    }
}

/// Invalidate the HTML cache for a node (called when content changes).
pub(crate) fn invalidate_html_cache(store: AppStore, store_id: StoreId, node_id: NodeId) {
    store.html_cache.update(|cache| {
        cache.remove(&(store_id, node_id));
    });
}
