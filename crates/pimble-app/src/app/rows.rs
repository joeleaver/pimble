//! Explorer rows as things to point at: revealing one, copying rows and
//! their links.

use std::cell::RefCell;

use rinch::prelude::*;

use crate::state::{parse_tree_value, AppStore};

thread_local! {
    /// Each tree row's label element by its tree value, the latest render's,
    /// so a revealed row can be scrolled to (`reveal_row`).
    pub(super) static TREE_ROWS: RefCell<std::collections::HashMap<String, NodeHandle>> = RefCell::new(std::collections::HashMap::new());
    /// A row to scroll to as soon as it is rendered (its parents' children
    /// may still be loading when it is revealed).
    pub(super) static REVEAL_ROW: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Scroll the tree to the row for `value` (a followed link's target, after
/// `events::reveal_in_tree` opened its parents): now if the row is drawn,
/// and again when it is next drawn, since rows appear only once their
/// parent's children arrive.
pub(crate) fn reveal_row(value: String) {
    if let Some(row) = TREE_ROWS.with(|rows| rows.borrow().get(&value).cloned()) {
        row.scroll_into_view();
    }
    REVEAL_ROW.with(|pending| *pending.borrow_mut() = Some(value));
}
/// Remember the node of row `value` (a store row: its root) as copied, for a
/// later Paste to mount (docs/MENUS_CONTRACT.md section 3.1). A row of an
/// encrypted store is not remembered: no mount reaches into one.
fn remember_copied(store: AppStore, value: &str) {
    let Some((s_id, node_id_opt)) = parse_tree_value(value) else { return };
    if store.is_vault(s_id) {
        return;
    }
    match node_id_opt {
        Some(n_id) => {
            let title = store.display_label(s_id, n_id);
            store.mount_source.set(Some((s_id, n_id, title)));
        }
        None => {
            if let Some(root_id) = store.root_node_id(s_id) {
                let name = store.get_store_signal(s_id)
                    .map(|sig| untracked(|| sig.with(|s| s.name.clone())))
                    .unwrap_or_default();
                store.mount_source.set(Some((s_id, root_id, name)));
            }
        }
    }
}

/// Copy (docs/MENUS_CONTRACT.md section 3.1; Joe: "'copy' copies
/// everything"): the rows' `pimble:` links on the system clipboard, and the
/// first row remembered for a later Paste to mount. A row menu's Copy and the
/// explorer's Ctrl+C.
pub(crate) fn copy_rows(store: AppStore, values: &[String]) {
    if let Some(first) = values.first() {
        remember_copied(store, first);
    }
    copy_links(store, values);
}

/// What a row's menu acts on: the selection when the row is one of several
/// selected rows, the row alone otherwise (docs/TREE_CONTRACT.md
/// "Multi-select"). A right-click does not change the selection.
pub(super) fn rows_for_menu(tree_state: UseTreeReturn, value: &str) -> Vec<String> {
    let selected = untracked(|| tree_state.selected.get());
    if selected.len() > 1 && selected.contains(value) {
        let mut rows: Vec<String> = selected.into_iter().collect();
        rows.sort();
        rows
    } else {
        vec![value.to_string()]
    }
}

/// "Copy Link" over several rows: one link per line.
fn copy_links(store: AppStore, values: &[String]) {
    if let [one] = values {
        return copy_link(store, one);
    }
    let links: Vec<String> = values
        .iter()
        .filter_map(|v| {
            let (s_id, node_id_opt) = parse_tree_value(v)?;
            let node_id = node_id_opt.or_else(|| store.root_node_id(s_id))?;
            Some(pimble_core::PimbleUrl::node(s_id, node_id).to_string())
        })
        .collect();
    match crate::links::copy_text(&links.join("\n")) {
        Ok(()) => crate::events::show_notice(store, format!("{} links copied.", links.len())),
        Err(e) => crate::events::show_notice(store, format!("The links could not be copied: {e}")),
    }
}

/// "Copy Link" (docs/LINKS_CONTRACT.md "Making a link"): the row's node, or a
/// store row's root, as a `pimble:` link on the system clipboard. Through a
/// mount the value names the source store's node, as every address does.
pub(crate) fn copy_link(store: AppStore, value: &str) {
    let Some((s_id, node_id_opt)) = parse_tree_value(value) else { return };
    let Some(node_id) = node_id_opt.or_else(|| store.root_node_id(s_id)) else { return };
    let url = pimble_core::PimbleUrl::node(s_id, node_id).to_string();
    match crate::links::copy_text(&url) {
        Ok(()) => crate::events::show_notice(store, "Link copied.".to_string()),
        Err(e) => crate::events::show_notice(store, format!("The link could not be copied: {e}")),
    }
}
