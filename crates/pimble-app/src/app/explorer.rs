//! The explorer's tree: its callbacks, drops, keys and inline rename.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rinch::prelude::*;
use rinch::core::{set_keyboard_interceptor, clear_keyboard_interceptor};

use crate::protocol::BackendCommand;
use crate::state::{parse_tree_value, AppStore};
use super::explorer_menu::explorer_menu;
use super::{DOUBLE_CLICK_MS, TREE_INDENT, now_ms};
use super::navigation::open_node;
use super::row_menu::ExplorerMenu;
use super::tree_row::render_tree_row;

thread_local! {
    /// The explorer tree's state, for the View menu's "Focus Explorer",
    /// built before the tree is.
    pub(super) static TREE_STATE: Cell<Option<UseTreeReturn>> = const { Cell::new(None) };
    /// Each tree row's rename box, by tree value, for `start_rename` to give
    /// the keyboard to. A handle and not an id: `request_focus` is the
    /// desktop runtime's alone, and a browser needs the element's own focus.
    pub(super) static RENAME_INPUTS: RefCell<std::collections::HashMap<String, NodeHandle>> = RefCell::new(std::collections::HashMap::new());
}

/// Give the explorer the keyboard (View > "Focus Explorer"): its cursor row,
/// or its first (docs/TREE_CONTRACT.md "The keyboard").
pub fn focus_explorer() {
    if let Some(tree_state) = TREE_STATE.with(Cell::get) {
        tree_state.controller.focus();
    }
}

/// Start renaming the row of tree value `value` in place: a double-click, F2,
/// or a node the keyboard just made. Enter commits and Escape cancels, and
/// either gives the keyboard back to the tree. `false` when the row cannot
/// be renamed here (a mount, or a node this device may only read).
pub(crate) fn start_rename(store: AppStore, tree_state: UseTreeReturn, value: &str) -> bool {
    // Don't allow rename on mount nodes
    let is_mount_node = if let Some((sid, Some(nid))) = parse_tree_value(value) {
        store.is_mount(sid, nid)
    } else {
        false
    };
    if is_mount_node {
        return false;
    }

    // A node this device may only read changes in no way
    // (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles"):
    // the context menu's "Rename" is disabled there, so the
    // double-click shortcut is not a way around it. An editor
    // renames like anyone else. Judged per node: a reader's
    // root may sit beside an editor's in the same store.
    let renameable = match parse_tree_value(value) {
        Some((s_id, Some(n_id))) => store.node_access(s_id, n_id).allows_write(),
        Some((s_id, None)) => store.store_access(s_id).allows_write(),
        None => true,
    };
    if !renameable {
        return false;
    }

    let edit_text = if let Some((store_id, Some(node_id))) = parse_tree_value(value) {
        store.get_node_signal(store_id, node_id)
            .map(|sig| sig.with(|n| n.metadata.title.clone()))
            .unwrap_or_default()
    } else {
        String::new()
    };
    store.rename_text.set(edit_text);
    store.renaming_node.set(Some(value.to_string()));
    let nv_interceptor = value.to_string();
    set_keyboard_interceptor(move |data| {
        if data.key == "Escape" {
            run_on_main_thread(move || {
                store.renaming_node.set(None);
                clear_keyboard_interceptor();
                tree_state.controller.focus();
            });
            return true;
        }
        if data.key == "Enter" {
            let nv = nv_interceptor.clone();
            run_on_main_thread(move || {
                let new_title = rename_title(&nv, untracked(|| store.rename_text.get()));
                store.renaming_node.set(None);
                clear_keyboard_interceptor();
                tree_state.controller.focus();
                if let (Some((s_id, Some(n_id))), Some(new_title)) = (parse_tree_value(&nv), new_title) {
                    store.send(BackendCommand::RenameNode {
                        store_id: s_id, node_id: n_id, title: new_title,
                    });
                }
            });
            return true;
        }
        false
    });
    // A moment later: the box is shown by an effect of `renaming_node`, and
    // a browser will not focus it while it is still `display: none`.
    if let Some(input) = RENAME_INPUTS.with(|inputs| inputs.borrow().get(value).cloned()) {
        let _ = set_timeout(0, move || input.focus());
    }
    true
}

thread_local! {
    /// The row of a node just made whose name is being typed: its box starts
    /// empty, and an empty Enter keeps the name it was made with ("Metanode").
    pub(super) static FRESH_RENAME: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The title a rename of `value` commits, or `None` to keep the one it has: a
/// node just made, named nothing, keeps the name it was made with.
pub(super) fn rename_title(value: &str, text: String) -> Option<String> {
    let fresh = FRESH_RENAME.with(|f| f.borrow_mut().take()).as_deref() == Some(value);
    (!(fresh && text.trim().is_empty())).then_some(text)
}

/// The explorer's tree, its one context menu, drops and keys, and the
/// way an inline rename ends (`cancel_rename(commit)`), which the page
/// around it needs too.
pub(super) fn render_explorer(
    __scope: &mut RenderScope,
    store: AppStore,
    tree_state: UseTreeReturn,
    last_click: Rc<Cell<(f64, String)>>,
    drag_ctx: DragContext<String>,
) -> (NodeHandle, impl Fn(bool) + Clone + 'static) {
    // Cancel any in-progress rename, optionally committing the change.
    let cancel_last_click = last_click.clone();
    let cancel_rename = move |commit: bool| {
        if let Some(prev_value) = untracked(|| store.renaming_node.get()) {
            if commit {
                let new_title = rename_title(&prev_value, untracked(|| store.rename_text.get()));
                if let (Some((sid, Some(nid))), Some(new_title)) = (parse_tree_value(&prev_value), new_title) {
                    store.send(BackendCommand::RenameNode {
                        store_id: sid, node_id: nid, title: new_title,
                    });
                }
            }
            store.renaming_node.set(None);
            clear_keyboard_interceptor();
        }
        // Reset double-click timer so stale timestamps can't cause
        // a spurious rename on the next click.
        cancel_last_click.set((now_ms(), String::new()));
    };

    // Tree callbacks
    let select_last_click = last_click.clone();
    let cancel_rename_for_select = cancel_rename.clone();
    let on_tree_select = ValueCallback::new(move |value: String| {
        tracing::info!("Tree node selected: {}", value);

        // Double-click on a node → enter rename mode
        {
            let now = now_ms();
            let (prev_time, prev_value) = select_last_click.replace((now, value.clone()));
            let is_double_click = prev_value == value && now - prev_time < DOUBLE_CLICK_MS;

            if is_double_click && value.starts_with("node_") {
                start_rename(store, tree_state, &value);
                return;
            }

            // If we were renaming a node, commit it before switching
            cancel_rename_for_select(true);
        }

        // (The previously-edited node's changes already persisted live through
        // the collab delta relay — no explicit save-on-switch is needed.)

        // Update selection and load new node — the same `open_node` a
        // search-result click uses, so there is exactly one way to open one.
        open_node(store, tree_state, value);
    });

    let on_tree_expand = ValueCallback::new(move |value: String| {
        tracing::info!("Tree node expanded: {}", value);
        if let Some((s_id, node_id_opt)) = parse_tree_value(&value) {
            let resolved = node_id_opt.or_else(|| store.root_node_id(s_id));
            if let Some(nid) = resolved {
                store.expanded.update(|e| { e.insert((s_id, nid)); });
                let needs_fetch = !store.has_children_loaded(s_id, nid);
                if needs_fetch {
                    store.send(BackendCommand::GetChildren { store_id: s_id, node_id: nid });
                    store.children_asked(s_id, nid);
                }
            }
        }
        // No signal mutation — UseTreeReturn handles expanded state internally
    });

    let on_tree_collapse = ValueCallback::new(move |value: String| {
        tracing::info!("Tree node collapsed: {}", value);
        if let Some((s_id, node_id_opt)) = parse_tree_value(&value) {
            let resolved = node_id_opt.or_else(|| store.root_node_id(s_id));
            if let Some(nid) = resolved {
                store.expanded.update(|e| { e.remove(&(s_id, nid)); });
            }
        }
    });

    // Build the tree with reactive data_source.
    // ONLY subscribes to tree_structure_version; everything else is untracked.
    let data_source: Rc<dyn Fn() -> Vec<TreeNodeData>> =
        Rc::new(move || {
            let _ = store.tree_structure_version.get(); // structural subscription
            untracked(|| store.build_tree_data_structural())
        });

    // The explorer's one context menu, aimed by a right press on a row.
    let menu = ExplorerMenu::new();

    // Custom render_node with per-node reactive Effects for label/icon/mount
    let render_node_fn: RenderTreeNode = Rc::new(move |payload: &RenderTreeNodePayload, __scope: &mut RenderScope| {
        render_tree_row(__scope, store, tree_state, menu, payload)
    });

    // Drops (docs/TREE_CONTRACT.md "The drop model"): `tree_drop` judges
    // each one the Tree offers, the strip at the foot of the explorer
    // says where it lands, and a release sends what was planned.
    let tree_drag_values: rinch::prelude::DragValues = Rc::new(crate::tree_drop::drag_values);
    let tree_judge_drop: rinch::prelude::JudgeDrop =
        Rc::new(move |drop: &TreeDrop| crate::tree_drop::plan_drop(store, drop).verdict());
    let on_tree_drop_change = ValueCallback::new(move |offer: Option<(TreeDrop, DropVerdict)>| {
        store.drop_hint.set(offer.map(|(drop, _)| crate::tree_drop::plan_drop(store, &drop).hint().clone()));
    });
    let on_tree_drop = ValueCallback::new(move |drop: TreeDrop| {
        crate::tree_drop::send_plan(store, crate::tree_drop::plan_drop(store, &drop));
    });
    // A row dragged out of the tree onto a board becomes a node card
    // there (docs/IMAGE_MARKUP_CONTRACT.md section 4).
    let on_tree_drag_start = ValueCallback::new(move |values: Vec<String>| {
        if let Some(value) = values.into_iter().next() {
            drag_ctx.set(value);
        }
    });
    let on_tree_drag_end = ValueCallback::new(move |()| {
        drag_ctx.clear();
        store.drop_hint.set(None);
    });

    // The keyboard (docs/TREE_CONTRACT.md "The keyboard"): the Tree moves
    // the cursor; `tree_keys` binds Pimble's keys; the document follows
    // the cursor once it rests (Joe, 2026-10-19).
    let tree_onkey: rinch::prelude::TreeKeyHandler =
        Rc::new(move |key: &rinch::prelude::TreeKey| crate::tree_keys::on_key(store, tree_state, key));
    let on_tree_cursor =
        ValueCallback::new(move |value: String| crate::tree_pages::cursor_rested(store, tree_state, value));
    let tree_label_of: rinch::prelude::LabelOf = Rc::new(move |value: &str| crate::tree_keys::label_of(store, value));

    let tree_scroll = rsx! {
        div {
            class: "pimble-sidebar__tree",
            // A right press on the tree's empty space aims the menu at no
            // row (a row's own press, nearer, aims it at the row).
            onmousedown: move || {
                if rinch::core::get_click_context().button == rinch::core::MouseButton::Right {
                    menu.aim(store, tree_state, None);
                }
            },
            Tree {
                data: untracked(|| store.build_tree_data_structural()),
                tree: tree_state,
                data_source: data_source,
                indent: TREE_INDENT,
                guides: true,
                select_on_click: true,
                expand_on_click: false,
                onselect: on_tree_select,
                onexpand: on_tree_expand,
                oncollapse: on_tree_collapse,
                render_node: render_node_fn,
                draggable: true,
                // Just below an expanded row, the pointer's column says
                // inside (first child) or outside (next sibling), and the
                // line starts at that depth (Joe, 2026-11-14).
                after_open_row: rinch::prelude::AfterOpenRow::ByColumn,
                drag_values: tree_drag_values,
                judge_drop: tree_judge_drop,
                ondropchange: on_tree_drop_change,
                ondrop: on_tree_drop,
                ondragstart: on_tree_drag_start,
                ondragend: on_tree_drag_end,
                keyboard: true,
                handle_key: tree_onkey,
                oncursor: on_tree_cursor,
                label_of: tree_label_of,
            }
        }
    };
    let tree_scroll = explorer_menu(__scope, store, tree_state, menu, tree_scroll);
    (tree_scroll, cancel_rename)
}
