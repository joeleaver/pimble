//! The explorer keys rinch's Tree leaves to the app (rinch #3062), as
//! docs/TREE_CONTRACT.md "The keyboard" binds them: PageUp and PageDown move
//! the cursor a page of visible rows, the document following it as it follows
//! any cursor that rests; Ctrl+A selects every row at the cursor's level under
//! its parent. `tree_keys` maps the keys here.

use std::collections::HashSet;

use rinch::prelude::{set_timeout, untracked, TreeNodeData, UseTreeReturn};

use crate::state::AppStore;

/// How many rows a page is: the Tree's own page before rinch #3062.
const PAGE_ROWS: usize = 10;

/// How long the cursor rests before its document opens: the Tree's
/// `cursor_delay_ms`, which the explorer leaves at its default.
const REST_MS: u32 = 120;

/// The rows drawn, top to bottom, given what is expanded.
fn visible(data: &[TreeNodeData], expanded: &HashSet<String>) -> Vec<String> {
    fn walk(rows: &[TreeNodeData], expanded: &HashSet<String>, out: &mut Vec<String>) {
        for row in rows {
            out.push(row.value.clone());
            if !row.children.is_empty() && expanded.contains(&row.value) {
                walk(&row.children, expanded, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(data, expanded, &mut out);
    out
}

/// The row a page down (or up) from `cursor` among the rows drawn, stopping
/// at the last (first); `None` when the cursor is on no drawn row or is
/// already there.
pub(crate) fn paged(data: &[TreeNodeData], expanded: &HashSet<String>, cursor: &str, down: bool) -> Option<String> {
    let rows = visible(data, expanded);
    let at = rows.iter().position(|r| r == cursor)?;
    let to = if down { (at + PAGE_ROWS).min(rows.len() - 1) } else { at.saturating_sub(PAGE_ROWS) };
    (to != at).then(|| rows[to].clone())
}

/// The rows at `cursor`'s level under its parent (the top level's rows when
/// it has none), in order; empty when no row is `cursor`.
pub(crate) fn siblings(data: &[TreeNodeData], cursor: &str) -> Vec<String> {
    fn find(rows: &[TreeNodeData], cursor: &str) -> Option<Vec<String>> {
        if rows.iter().any(|r| r.value == cursor) {
            return Some(rows.iter().map(|r| r.value.clone()).collect());
        }
        rows.iter().find_map(|r| find(&r.children, cursor))
    }
    find(data, cursor).unwrap_or_default()
}

/// PageDown (`down`) or PageUp with the cursor on `cursor`.
pub(crate) fn page(store: AppStore, tree_state: UseTreeReturn, cursor: &str, down: bool) {
    let data = untracked(|| store.build_tree_data_structural());
    let expanded = untracked(|| tree_state.expanded.get());
    let Some(target) = paged(&data, &expanded, cursor, down) else { return };
    // The row takes the keyboard and is scrolled to, as an arrow's row is.
    tree_state.controller.set_cursor(&target);
    tree_state.controller.focus();
    // It opens once it rests there (`cursor_rested` skips a rest the
    // cursor has left since).
    let _ = set_timeout(REST_MS, move || cursor_rested(store, tree_state, target));
}

/// Ctrl+A with the cursor on `cursor`: its level under its parent becomes the
/// selection. Several rows selected open nothing, as a Shift run's do.
pub(crate) fn select_siblings(store: AppStore, tree_state: UseTreeReturn, cursor: &str) {
    let data = untracked(|| store.build_tree_data_structural());
    let rows = siblings(&data, cursor);
    if !rows.is_empty() {
        tree_state.controller.set_selection(rows);
        tree_state.controller.set_anchor(Some(cursor));
    }
}

/// The cursor came to rest on `value` (the Tree's `oncursor`, and a page
/// move's): the document follows it. A cursor that rests on one of several
/// selected rows (the end of a Shift run) opens nothing: opening selects the
/// row alone. Off the selection it opens the row, as a click would. A rest
/// the cursor has since left (an arrow's, overtaken by a page move inside
/// the rest) opens nothing.
pub(crate) fn cursor_rested(store: AppStore, tree_state: UseTreeReturn, value: String) {
    if untracked(|| tree_state.controller.cursor()).as_deref() != Some(value.as_str()) {
        return;
    }
    let selected = untracked(|| tree_state.selected.get());
    if selected.len() > 1 && selected.contains(&value) {
        return;
    }
    if untracked(|| store.selected_id.get()).as_deref() != Some(value.as_str()) {
        crate::app::open_node(store, tree_state, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(value: &str, children: Vec<TreeNodeData>) -> TreeNodeData {
        TreeNodeData::new(value, value).with_children(children)
    }

    /// A store row `s` over `a` (with `a0`..`a11` under it) and `b`..`e`.
    fn tree() -> Vec<TreeNodeData> {
        let under_a = (0..12).map(|i| row(&format!("a{i}"), vec![])).collect();
        vec![row("s", vec![row("a", under_a), row("b", vec![]), row("c", vec![]), row("d", vec![]), row("e", vec![])])]
    }

    fn open(values: &[&str]) -> HashSet<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn a_page_is_ten_drawn_rows_and_stops_at_the_ends() {
        let data = tree();
        // Collapsed `a`: s a b c d e are drawn, fewer than a page.
        assert_eq!(paged(&data, &open(&["s"]), "s", true).as_deref(), Some("e"));
        assert_eq!(paged(&data, &open(&["s"]), "e", true), None, "already at the end");
        assert_eq!(paged(&data, &open(&["s"]), "c", false).as_deref(), Some("s"));
        // Open `a`: s a a0..a11 b ..., so ten rows on from s is a8.
        let both = open(&["s", "a"]);
        assert_eq!(paged(&data, &both, "s", true).as_deref(), Some("a8"));
        assert_eq!(paged(&data, &both, "a8", false).as_deref(), Some("s"));
        assert_eq!(paged(&data, &both, "e", false).as_deref(), Some("a5"));
        // A row inside a collapsed one is not drawn.
        assert_eq!(paged(&data, &open(&["s"]), "a3", true), None);
    }

    #[test]
    fn ctrl_a_takes_the_cursor_rows_level_under_its_parent() {
        let data = tree();
        assert_eq!(siblings(&data, "c"), ["a", "b", "c", "d", "e"]);
        assert_eq!(siblings(&data, "a4").len(), 12);
        assert_eq!(siblings(&data, "s"), ["s"], "the top level");
        assert!(siblings(&data, "nowhere").is_empty());
    }
}
