//! What the explorer waits for between a gesture and the event that answers
//! it: the drop target a `NodeMoved` expands, and the node the explorer asked
//! for, opened when it is made and renamed when its row comes.

use super::*;

thread_local! {
    /// The tree value (with any mount-path suffix, decision 7) of the last
    /// drag-and-drop target, set by `app.rs`'s `on_drop` right before sending
    /// `MoveNode` and consumed once by the `NodeMoved` handler in `events/nodes.rs`
    /// to auto-expand exactly the place the user dropped into, rather than
    /// reconstructing an unqualified value that would not match a target
    /// reached only through a mount.
    static LAST_DROP_TARGET_VALUE: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Record the qualified tree value of a drag-and-drop target, just before the
/// `MoveNode` command that will result in a `NodeMoved` event for it.
pub fn set_last_drop_target_value(value: String) {
    LAST_DROP_TARGET_VALUE.with(|cell| *cell.borrow_mut() = Some(value));
}

/// Take (and clear) the last recorded drop-target tree value, if any.
pub fn take_last_drop_target_value() -> Option<String> {
    LAST_DROP_TARGET_VALUE.with(|cell| cell.borrow_mut().take())
}

thread_local! {
    /// A node the explorer asked for (docs/TREE_CONTRACT.md "The keyboard": a
    /// new sibling or child, renamed at once; "+" and New alike): its store,
    /// its parent, and the mount-path suffix its tree value will carry.
    static PENDING_CREATE: RefCell<Option<(StoreId, NodeId, String)>> = const { RefCell::new(None) };
    /// The tree value of that node once it exists, renamed when its row is
    /// drawn.
    static PENDING_RENAME: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The explorer is making a text node or a query metanode under `parent`
/// (its keyboard, "+", a row menu's New, File > New): once made it opens in
/// the focused pane (Joe, 2026-11-26), and its row, when it comes, starts
/// renaming.
pub fn set_pending_create(store_id: StoreId, parent: NodeId, suffix: String) {
    PENDING_CREATE.with(|cell| *cell.borrow_mut() = Some((store_id, parent, suffix)));
}

/// `NodeCreated` for `node` under `parent`: the tree value to open and
/// rename, when it is the node the explorer asked for.
pub fn take_pending_create(store_id: StoreId, parent: NodeId, node: NodeId) -> Option<String> {
    PENDING_CREATE.with(|cell| {
        let mut pending = cell.borrow_mut();
        match pending.as_ref() {
            Some((s, p, _)) if *s == store_id && *p == parent => {
                let (_, _, suffix) = pending.take()?;
                let value = format!("node_{store_id}_{node}{suffix}");
                PENDING_RENAME.with(|r| *r.borrow_mut() = Some(value.clone()));
                Some(value)
            }
            _ => None,
        }
    })
}

/// Whether the row of `value` is the one to rename as it is drawn (once).
pub fn take_pending_rename(value: &str) -> bool {
    PENDING_RENAME.with(|cell| {
        let mut pending = cell.borrow_mut();
        if pending.as_deref() == Some(value) {
            pending.take();
            true
        } else {
            false
        }
    })
}
