//! The explorer's keyboard (docs/TREE_CONTRACT.md "The keyboard"). rinch's
//! Tree moves the cursor (the arrows, Home, End, PageUp, PageDown, `*`,
//! type-ahead) and offers every key here first; this binds Pimble's own:
//! opening, renaming, making nodes, moving them (Alt+Shift+arrows, judged and
//! sent exactly as a drop is, by `tree_drop::plan_drop`), deleting, cut and
//! paste, and handing the keyboard back to the document.

use pimble_core::Place;
use rinch::prelude::{untracked, DropZone, TreeDrop, TreeKey, TreeMove, TreeSlot, UseTreeReturn};

use crate::protocol::BackendCommand;
use crate::state::{parse_tree_value, AppStore};
use crate::tree_drop::{plan_drop, send_plan, DropHint, DropPlan, BESIDE_A_STORE};

/// What a key does, before anything is done: pure, so the map is tested on
/// its own.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Give the cursor row's document the keyboard (opening it first).
    Enter,
    OpenInSplit,
    Rename,
    NewSibling,
    NewChild,
    Move(TreeMove),
    Delete,
    Cut,
    /// Paste the cut row after the cursor row, or (`into`) inside it, last.
    Paste { into: bool },
    CopyLink,
    /// Give up the keyboard (or drop a cut first).
    Escape,
}

/// The binding of `key` with these modifiers, or `None` for a key that is
/// the Tree's own or nobody's.
pub(crate) fn action(key: &str, ctrl: bool, shift: bool, alt: bool) -> Option<Action> {
    Some(match (key, ctrl, shift, alt) {
        ("Enter", false, false, false) => Action::Enter,
        ("Enter", true, false, false) | ("Enter", false, false, true) => Action::OpenInSplit,
        ("Enter", true, true, false) | ("Insert", false, false, false) => Action::NewSibling,
        ("Enter", true, false, true) => Action::NewChild,
        ("F2", false, false, false) => Action::Rename,
        ("ArrowRight", false, true, true) => Action::Move(TreeMove::Indent),
        ("ArrowLeft", false, true, true) => Action::Move(TreeMove::Outdent),
        ("ArrowUp", false, true, true) => Action::Move(TreeMove::Up),
        ("ArrowDown", false, true, true) => Action::Move(TreeMove::Down),
        ("Delete", false, false, false) => Action::Delete,
        ("x" | "X", true, false, false) => Action::Cut,
        ("v" | "V", true, shift, false) => Action::Paste { into: shift },
        ("c" | "C", true, false, false) => Action::CopyLink,
        ("Escape", false, false, false) => Action::Escape,
        _ => return None,
    })
}

thread_local! {
    /// The rows cut with Ctrl+X (the selection, or the cursor row), waiting
    /// for Ctrl+V.
    static CUT: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn cut_rows() -> Vec<String> {
    CUT.with(|c| c.borrow().clone())
}

fn set_cut(store: AppStore, values: Vec<String>) {
    let hint = match values.as_slice() {
        [] => None,
        [one] => Some(format!("Cut {}. Ctrl+V puts it after a row, Ctrl+Shift+V into one.", label_of(store, one))),
        many => Some(format!("Cut {} items. Ctrl+V puts them after a row, Ctrl+Shift+V into one.", many.len())),
    }
    .map(|line| DropHint { line, warning: None, refused: false });
    CUT.with(|c| *c.borrow_mut() = values);
    store.drop_hint.set(hint);
}

/// The text a row is found by when typing its first letters, and named by.
pub(crate) fn label_of(store: AppStore, value: &str) -> String {
    match parse_tree_value(value) {
        Some((s, Some(n))) => store.display_label(s, n),
        Some((s, None)) => store
            .get_store_signal(s)
            .map(|sig| untracked(|| sig.with(|st| st.name.clone())))
            .unwrap_or_default(),
        None => String::new(),
    }
}

/// The mount-path suffix of a tree value (what follows the node's own
/// address), which a sibling at the same level shares.
fn suffix(value: &str) -> String {
    value.find('/').map(|at| value[at..].to_string()).unwrap_or_default()
}

/// Send what a drop-shaped move plans, or say why not. A key that would put
/// a node beside a store (outdenting the top level) does nothing.
fn deliver(store: AppStore, drop: &TreeDrop) {
    match plan_drop(store, drop) {
        DropPlan::Refused(hint) if hint.line == BESIDE_A_STORE => {}
        DropPlan::Refused(hint) => crate::events::show_notice(store, hint.line),
        plan => {
            send_plan(store, plan);
        }
    }
}

/// The Tree's `onkey`: `true` when the key was Pimble's.
pub(crate) fn on_key(store: AppStore, tree_state: UseTreeReturn, key: &TreeKey) -> bool {
    let Some(action) = action(&key.key, key.ctrl, key.shift, key.alt) else { return false };
    if key.meta {
        return false;
    }
    let cursor = key.cursor.clone();
    match action {
        Action::Enter => {
            let pane = store.focused();
            if untracked(|| store.pane(pane).selected.get()).as_deref() != Some(cursor.as_str()) {
                crate::app::open_node(store, tree_state, cursor.clone());
            }
            keyboard_to_document(store, pane, cursor, 40);
        }
        Action::OpenInSplit => {
            let focused = store.focused();
            if let Some(new) = crate::pane_view::split_pane(store, focused, crate::panes::Direction::Right) {
                crate::app::open_node_in(store, tree_state, new, cursor);
            }
        }
        Action::Rename => {
            crate::app::start_rename(store, tree_state, &cursor);
        }
        Action::NewSibling | Action::NewChild => new_node(store, key, action == Action::NewChild),
        Action::Move(movement) => {
            if let Some(drop) = key.drop_for(movement) {
                deliver(store, &drop);
            }
        }
        Action::Delete => {
            // The selection, or the cursor row; nodes only, all or none.
            let nodes: Vec<_> = key.carried().iter().filter_map(|v| match parse_tree_value(v) {
                Some((s, Some(n))) => Some((s, n)),
                _ => None,
            }).collect();
            if nodes.is_empty() {
                return true;
            }
            if nodes.iter().any(|&(s, n)| !store.node_access(s, n).allows_write()) {
                crate::events::show_notice(store, pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string());
                return true;
            }
            // The cursor goes on to the row below what is deleted (or
            // above) before it is gone, so the keyboard stays in the tree.
            if let Some(next) = key.below.clone().or_else(|| key.above.clone()) {
                tree_state.controller.set_cursor(&next);
                tree_state.controller.set_selection([next.clone()]);
                tree_state.controller.focus();
            }
            for (s, n) in nodes {
                store.send(BackendCommand::DeleteNode { store_id: s, node_id: n });
            }
        }
        Action::Cut => {
            let rows: Vec<String> = key.carried().iter().filter(|v| matches!(parse_tree_value(v), Some((_, Some(_))))).cloned().collect();
            if !rows.is_empty() {
                set_cut(store, rows);
            }
        }
        Action::Paste { into } => {
            let cut = cut_rows();
            if cut.is_empty() {
                return true;
            }
            let drop = TreeDrop {
                dragged: cut,
                parent: if into { Some(cursor.clone()) } else { key.parent.clone() },
                slot: if into { TreeSlot::Last } else { TreeSlot::After(cursor.clone()) },
                over: cursor,
                zone: if into { DropZone::Inside } else { DropZone::After },
            };
            set_cut(store, Vec::new());
            deliver(store, &drop);
        }
        Action::CopyLink => crate::app::copy_link(store, &cursor),
        Action::Escape => {
            let several = untracked(|| tree_state.selected.with(|s| s.len() > 1));
            if !cut_rows().is_empty() {
                set_cut(store, Vec::new());
            } else if several {
                // Back to the one row the keyboard is on.
                tree_state.controller.set_selection([cursor]);
            } else {
                crate::editor::give_keyboard_to(store, store.focused());
            }
        }
    }
    true
}

/// Give pane `pane`'s editor the keyboard once it shows `value` (a node the
/// cursor had not rested on long enough to open yet loads first), looking
/// again every 50 ms, `tries` more times; a folder, which opens no editor,
/// leaves the keyboard in the tree.
fn keyboard_to_document(store: AppStore, pane: crate::panes::PaneId, value: String, tries: u32) {
    let state = store.pane(pane);
    let showing = untracked(|| state.selected.get()).as_deref() == Some(value.as_str());
    if !showing {
        return;
    }
    if untracked(|| state.show_editor.get()) {
        crate::editor::give_keyboard_to(store, pane);
    } else if tries > 0 {
        let _ = rinch::core::set_timeout(50, move || keyboard_to_document(store, pane, value, tries - 1));
    }
}

/// A new node after the cursor row (or inside it, last), named at once.
fn new_node(store: AppStore, key: &TreeKey, child: bool) {
    let target = if child { Some(key.cursor.clone()) } else { key.parent.clone() };
    let Some(parent_value) = target else { return };
    // Under a store row: its root. Under a mount: the node it mounts, as a
    // drop into one does.
    let Some((s, parent)) = (match parse_tree_value(&parent_value) {
        Some((s, None)) => store.root_node_id(s).map(|root| (s, root)),
        Some((s, Some(n))) if store.is_mount(s, n) => store
            .get_mount_signal(s, n)
            .and_then(|sig| untracked(|| sig.with(|m| m.mount_ref.as_ref().map(|r| (r.source_store, r.source_node))))),
        Some((s, Some(n))) => Some((s, n)),
        None => None,
    }) else {
        return;
    };
    if !store.node_access(s, parent).allows_write() {
        crate::events::show_notice(store, pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string());
        return;
    }
    let place = if child {
        Place::Last
    } else {
        match parse_tree_value(&key.cursor) {
            Some((_, Some(anchor))) => Place::After(anchor),
            _ => Place::Last,
        }
    };
    // Its tree value: a sibling sits at the cursor row's mount level; a child
    // of a mount row one level deeper, under that mount.
    let level_suffix = match parse_tree_value(&key.cursor) {
        Some((ms, Some(mn))) if child && store.is_mount(ms, mn) => format!("{}/{ms}_{mn}", suffix(&key.cursor)),
        _ => suffix(&key.cursor),
    };
    crate::state::set_pending_create(s, parent, level_suffix);
    store.send(BackendCommand::CreateNode { store_id: s, parent_id: Some(parent), title: String::new(), init: None, place });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_map() {
        assert_eq!(action("Enter", false, false, false), Some(Action::Enter));
        assert_eq!(action("Enter", true, false, false), Some(Action::OpenInSplit));
        assert_eq!(action("Enter", false, false, true), Some(Action::OpenInSplit));
        assert_eq!(action("Enter", true, true, false), Some(Action::NewSibling));
        assert_eq!(action("Insert", false, false, false), Some(Action::NewSibling));
        assert_eq!(action("Enter", true, false, true), Some(Action::NewChild));
        assert_eq!(action("F2", false, false, false), Some(Action::Rename));
        assert_eq!(action("ArrowRight", false, true, true), Some(Action::Move(TreeMove::Indent)));
        assert_eq!(action("ArrowLeft", false, true, true), Some(Action::Move(TreeMove::Outdent)));
        assert_eq!(action("ArrowUp", false, true, true), Some(Action::Move(TreeMove::Up)));
        assert_eq!(action("ArrowDown", false, true, true), Some(Action::Move(TreeMove::Down)));
        assert_eq!(action("Delete", false, false, false), Some(Action::Delete));
        assert_eq!(action("x", true, false, false), Some(Action::Cut));
        assert_eq!(action("v", true, false, false), Some(Action::Paste { into: false }));
        assert_eq!(action("V", true, true, false), Some(Action::Paste { into: true }));
        assert_eq!(action("c", true, false, false), Some(Action::CopyLink));
        assert_eq!(action("Escape", false, false, false), Some(Action::Escape));
    }

    /// The Tree's own keys, and the ones nobody binds, are not taken here.
    #[test]
    fn the_trees_keys_pass_through() {
        for key in ["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight", "Home", "End", "PageDown", "*", "a"] {
            assert_eq!(action(key, false, false, false), None, "{key}");
        }
        assert_eq!(action("Tab", false, false, false), None, "Tab is left alone");
        assert_eq!(action("ArrowDown", false, true, false), None, "Shift+arrows are wave 3's");
    }

    #[test]
    fn a_sibling_shares_the_rows_mount_suffix() {
        assert_eq!(suffix("node_a_b"), "");
        assert_eq!(suffix("node_a_b/c_d"), "/c_d");
    }
}
