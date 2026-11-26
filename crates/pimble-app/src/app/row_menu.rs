//! What the explorer's one context menu knows about the row it is aimed at.

use rinch::prelude::*;

use crate::state::{parse_tree_value, AppStore};
use super::{CAN_ADMINISTER_STORES, MOUNT_NOTE};

/// Walk the DOM subtree to find a `data-oncontextmenu` handler and copy it
/// to `target`. Used to hoist the context menu handler from the invisible
/// ContextMenuTarget (display:contents) to a visible wrapper div.
/// The kind of explorer row a context menu is open on: each kind has its own
/// list of items (docs/MENUS_CONTRACT.md section 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum RowKind {
    #[default]
    None,
    Store,
    Mount,
    Node,
}

impl RowKind {
    fn of(store: AppStore, value: &str) -> RowKind {
        if value.starts_with("store_") {
            return RowKind::Store;
        }
        match parse_tree_value(value) {
            Some((s_id, Some(n_id))) if store.is_mount(s_id, n_id) => RowKind::Mount,
            Some((_, Some(_))) => RowKind::Node,
            _ => RowKind::None,
        }
    }
}

/// The explorer's one context menu: which row it is aimed at, that row's kind,
/// and what the menu's items need of the row, one signal each. A right press
/// on a row aims it; the menu, built once for the whole tree
/// (`explorer_menu`), reads only those signals. Each is written only when its
/// value changes, so aiming the menu at another row of the same kind touches
/// just the items whose state differs. Before 2026-11-12 every row built a
/// whole menu of its own, about 500 DOM nodes a row, and every change to the
/// tree's shape rebuilt them all.
#[derive(Clone, Copy)]
pub(super) struct ExplorerMenu {
    row: Signal<Option<String>>,
    pub(super) kind: Signal<RowKind>,
    pub(super) note: Signal<String>,
    /// Which of a store row's items are off (`Capabilities::store_row_menu`).
    store_menu: Signal<Option<pimble_core::plan::StoreRowMenu>>,
    pub(super) can_write_tree: Signal<bool>,
    pub(super) can_write_mount_source: Signal<bool>,
    pub(super) can_comment: Signal<bool>,
    pub(super) share_disabled: Signal<bool>,
    pub(super) mounts_apply: Signal<bool>,
    pub(super) cut_off: Signal<bool>,
    pub(super) paste_off: Signal<bool>,
    pub(super) delete_off: Signal<bool>,
    /// How many rows the menu acts on: the selection when the row is one of
    /// several selected rows (docs/TREE_CONTRACT.md "Multi-select").
    pub(super) acts_on: Signal<usize>,
}

/// Write `value` into `signal` only when it differs, so its readers hear only
/// of real changes.
pub(super) fn put<T: PartialEq + Clone + 'static>(signal: Signal<T>, value: T) {
    if untracked(|| signal.get()) != value {
        signal.set(value);
    }
}

impl ExplorerMenu {
    pub(super) fn new() -> Self {
        ExplorerMenu {
            row: Signal::new(None),
            kind: Signal::new(RowKind::None),
            note: Signal::new(String::new()),
            store_menu: Signal::new(None),
            can_write_tree: Signal::new(false),
            can_write_mount_source: Signal::new(false),
            can_comment: Signal::new(false),
            share_disabled: Signal::new(true),
            mounts_apply: Signal::new(true),
            cut_off: Signal::new(true),
            paste_off: Signal::new(true),
            delete_off: Signal::new(true),
            acts_on: Signal::new(1),
        }
    }

    /// Aim the menu at `row` (or at no row: a press on the tree's empty
    /// space), judging the row now, as each row's menu once judged it when the
    /// row rendered.
    pub(super) fn aim(self, store: AppStore, tree_state: UseTreeReturn, row: Option<String>) {
        use pimble_core::plan::FoundRowAction as Fra;
        let value = row.clone().unwrap_or_default();
        let kind = row.as_deref().map_or(RowKind::None, |value| RowKind::of(store, value));
        let judged = untracked(|| RowMenu::of(store, &value));
        let acts_on = untracked(|| tree_state.selected.with(|s| if s.contains(&value) { s.len().max(1) } else { 1 }));
        put(self.kind, kind);
        put(self.row, row);
        put(self.note, if kind == RowKind::Mount { judged.mount_note.clone() } else { judged.note.clone() });
        put(self.store_menu, (kind == RowKind::Store).then_some(judged.store_menu));
        put(self.can_write_tree, judged.can_write_tree);
        put(self.can_write_mount_source, judged.can_write_mount_source);
        put(self.can_comment, judged.can_comment);
        put(self.share_disabled, judged.share_disabled || acts_on > 1);
        put(self.mounts_apply, judged.mounts_apply);
        put(self.cut_off, !judged.can_write_tree || !judged.allows(Fra::Cut));
        put(self.paste_off, !judged.can_write_tree || !judged.allows(Fra::PasteInto));
        put(self.delete_off, !judged.can_write_tree || !judged.allows(Fra::Delete));
        put(self.acts_on, acts_on);
    }

    /// The row the menu is aimed at, read without subscribing (for actions).
    pub(super) fn target(self) -> String {
        untracked(|| self.row.get()).unwrap_or_default()
    }

    /// A store row's item switch, read reactively.
    pub(super) fn store_off(self, pick: fn(&pimble_core::plan::StoreRowMenu) -> bool) -> bool {
        self.store_menu.with(|m| m.as_ref().map_or(true, pick))
    }
}

/// What may be done with the row a menu is open on
/// (`pimble_core::plan::capabilities`, one rule for every menu): what this
/// device may change of the row's node (docs/NODE_DOCUMENT_CONTRACT.md section
/// 5, "Roles"), judged per node because one store can hold a root this
/// account reads beside a root it edits; a store row asks about the store.
/// `parse_tree_value` returns the CANONICAL pair, so a node reached through a
/// mount is judged as the node it is in its source store.
#[derive(Clone)]
pub(super) struct RowMenu {
    /// Which of a store row's items are off (`Capabilities::store_row_menu`).
    store_menu: pimble_core::plan::StoreRowMenu,
    /// The one dimmed line at the top saying why items are off, if any are.
    note: String,
    /// A mount's own top line.
    mount_note: String,
    can_write_tree: bool,
    /// A mount's "New" makes in the mount's SOURCE node, so that node's
    /// access governs that one item.
    can_write_mount_source: bool,
    can_comment: bool,
    share_disabled: bool,
    /// Mounts do not exist in an encrypted store (the server has no tree there).
    mounts_apply: bool,
    /// A row a query metanode found: its place is not changed from here.
    found: bool,
}

impl RowMenu {
    fn of(store: AppStore, value: &str) -> RowMenu {
        let parsed = parse_tree_value(value);
        let is_store_root = value.starts_with("store_");
        let is_mount = !is_store_root && matches!(parsed, Some((s_id, Some(n_id))) if store.is_mount(s_id, n_id));
        let found = crate::state::found_row(value);
        let caps = parsed
            .map(|(s_id, n_id)| pimble_core::plan::capabilities(&store.view(), s_id, n_id))
            .unwrap_or_default();
        let can_write_tree = caps.writable();
        let can_write_mount_source = match parsed {
            Some((s_id, Some(n_id))) if is_mount => store
                .get_mount_signal(s_id, n_id)
                .and_then(|sig| sig.with(|m| m.mount_ref.as_ref().map(|r| (r.source_store, r.source_node))))
                .map_or(can_write_tree, |(source_store, source_node)| store.node_access(source_store, source_node).allows_write()),
            _ => can_write_tree,
        };
        // A browser build has none of these items to explain.
        let relayed_note = if CAN_ADMINISTER_STORES && is_store_root { caps.relayed_note() } else { "" };
        let share_note = if CAN_ADMINISTER_STORES { caps.share_note() } else { "" };
        let access_note = crate::state::access_note(caps.access);
        let found_note = found.as_ref().map(|f| f.refusal.clone()).unwrap_or_default();
        let note = if is_store_root && caps.every_share_ended {
            crate::state::ENDED_REPLICA_NOTE.to_string()
        } else {
            crate::state::menu_note_text(&[found_note.as_str(), access_note, share_note, relayed_note])
        };
        RowMenu {
            store_menu: caps.store_row_menu(false),
            note,
            mount_note: crate::state::menu_note_text(&[access_note, MOUNT_NOTE]),
            can_write_tree,
            can_write_mount_source,
            can_comment: caps.access.allows_comment(),
            share_disabled: !caps.can_share(),
            mounts_apply: parsed.map_or(true, |(s_id, _)| !store.is_vault(s_id)),
            found: found.is_some(),
        }
    }

    /// What a found row allows (`pimble_core::plan::FoundRowAction`, one rule
    /// for the explorer, plugins and the MCP).
    fn allows(&self, action: pimble_core::plan::FoundRowAction) -> bool {
        !self.found || action.allowed()
    }
}
