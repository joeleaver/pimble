//! The explorer's one context menu, built once for the whole tree.

use rinch::prelude::*;
use rinch_tabler_icons::TablerIcon;

use crate::protocol::BackendCommand;
use crate::state::{parse_tree_value, AppStore};
// Only "Mount Store Here...", which picks a directory on this machine, sets one.
#[cfg(feature = "native")]
use crate::state::PendingMount;
use super::appearance_modal::open_appearance_modal;
use super::deleted_modal::open_deleted_modal;
use super::explorer::start_rename;
use super::hosting::{open_host_modal, open_stop_relay_modal};
use super::CAN_ADMINISTER_STORES;
use super::navigation::open_node_in;
use super::row_menu::{ExplorerMenu, RowKind};
use super::rows::{copy_rows, rows_for_menu};
use super::share_modal::open_share_modal;
use super::store_modals::open_connect_modal;
#[cfg(not(feature = "native"))]
use super::store_modals::open_mount_picker;

/// The explorer's one context menu around `tree` (`ExplorerMenu`): one list of
/// items per kind of row (docs/MENUS_CONTRACT.md section 3: grouped, what
/// cannot apply left out), each kind's built the first time the menu opens on
/// it. What varies from row to row is read reactively from the aim.
pub(super) fn explorer_menu(
    __scope: &mut RenderScope,
    store: AppStore,
    tree_state: UseTreeReturn,
    menu: ExplorerMenu,
    tree: NodeHandle,
) -> NodeHandle {
    let row = move || menu.target();
    let parsed = move || parse_tree_value(&row());
    let store_of = move || parsed().map(|(s_id, _)| s_id);
    // The node a store row stands for is its root.
    let node_of = move || parsed().and_then(|(s_id, n_id)| n_id.or_else(|| store.root_node_id(s_id)).map(|n| (s_id, n)));

    // With several rows selected and this one among them, the menu acts on
    // all of them where that makes sense (Delete, Cut, Copy) and turns off
    // what is for one row alone (docs/TREE_CONTRACT.md "Multi-select").
    let several = move || menu.acts_on.get() > 1;

    let on_start_discussion = move || {
        if let Some((s_id, Some(n_id))) = parsed() {
            crate::discussions::start_on(store, store.focused(), s_id, n_id);
        }
    };
    let on_cut = move || {
        let rows: Vec<String> = rows_for_menu(tree_state, &row()).into_iter().filter(|v| matches!(parse_tree_value(v), Some((_, Some(_))))).collect();
        if !rows.is_empty() {
            crate::tree_keys::set_cut(store, rows);
        }
    };
    let on_copy = move || copy_rows(store, &rows_for_menu(tree_state, &row()));
    let on_paste = move || crate::pasting::paste(store, &row());
    let on_close_store = move || {
        if let Some(s_id) = store_of() {
            store.send(BackendCommand::CloseStore { store_id: s_id });
        }
    };
    let on_link_to_remote = move || {
        if let Some(s_id) = store_of() {
            store.link_modal_store.set(Some(s_id));
            store.link_modal_url.set(String::new());
            store.link_modal_token.set(String::new());
            store.link_modal_token_visible.set(false);
            store.link_modal_error.set(String::new());
        }
    };
    let on_unlink_from_remote = move || {
        if let Some(s_id) = store_of() {
            store.send(BackendCommand::SetStoreSync { store_id: s_id, remote: None });
        }
    };
    let on_host_on_cloud = move || {
        if let Some(s_id) = store_of() {
            // Hosting asked for on its own is not on the way to sharing anything.
            store.share_after_host.set(None);
            open_host_modal(store, s_id);
        }
    };
    let on_stop_relaying = move || {
        if let Some(s_id) = store_of() {
            open_stop_relay_modal(store, s_id);
        }
    };
    // "Share...": grant people a scope on this store rooted at this node (a
    // store row shares its root node), so they hold the same documents and
    // co-author them (docs/NODE_DOCUMENT_CONTRACT.md section 5). Never off for
    // a store that is not hosted: the modal opens on the two ways to give the
    // share somewhere to live (docs/RELAY_CONTRACT.md).
    let on_share = move || {
        if let Some((s_id, n_id)) = node_of() {
            open_share_modal(store, s_id, n_id);
        }
    };
    let on_remove_replica = move || {
        if let Some(s_id) = store_of() {
            store.remove_replica_modal_store.set(Some(s_id));
            store.remove_replica_modal_error.set(String::new());
            store.remove_replica_modal_pending.set(false);
        }
    };
    let on_recently_deleted = move || {
        if let Some(s_id) = store_of() {
            open_deleted_modal(store, s_id);
        }
    };
    let on_mount_store = move || {
        if let Some((s_id, pid)) = node_of() {
            #[cfg(feature = "native")]
            {
                let dialog = rinch::dialogs::pick_folder().set_title("Select Store to Mount");
                if let Some(path) = dialog.pick() {
                    let path_str = path.to_string_lossy().to_string();
                    tracing::info!("Mounting store from: {} into {:?}", path_str, s_id);
                    store.send(BackendCommand::OpenStore { path: path_str.clone() });
                    store.pending_mount.set(Some(PendingMount {
                        target_store_id: s_id,
                        target_parent_id: pid,
                        source_path: path_str,
                    }));
                }
            }
            // A browser has no directories to offer, so it picks from the
            // stores the account already grants. The RPC either way is
            // `createMount`, which an editor may call.
            #[cfg(not(feature = "native"))]
            open_mount_picker(store, (s_id, pid));
        }
    };
    // "Mount Remote Store Here...": the connect modal in mount mode with this
    // row's canonical target (a store root's target is its own root node).
    let on_mount_remote_store = move || {
        if let Some(target) = crate::tree_view::create_parent_of_row(store, &row()) {
            open_connect_modal(store, Some(target));
        }
    };
    // The rows the menu acts on, all or none (`pimble_core::plan::can_delete`).
    let on_delete = move || {
        let Some(nodes) = crate::tree_view::deletable(store, &rows_for_menu(tree_state, &row())) else { return };
        for (s_id, n_id) in nodes {
            store.send(BackendCommand::DeleteNode { store_id: s_id, node_id: n_id });
        }
    };
    // "Appearance...": the icon, color and tags picker for this node (a store
    // row edits its root node).
    let on_appearance = move || {
        if let Some((s_id, n_id)) = node_of() {
            open_appearance_modal(store, s_id, n_id);
        }
    };
    let on_rename = move || {
        start_rename(store, tree_state, &row());
    };
    // "Watch" (docs/NOTIFICATIONS_CONTRACT.md section 3): hear when others
    // change this note, or anything under it; nothing is watched by default.
    let on_watch = move |subtree: bool| {
        if let Some((s_id, n_id)) = node_of() {
            crate::watches::watch(store, s_id, n_id, subtree);
        }
    };
    let on_unwatch = move || {
        if let Some((s_id, n_id)) = node_of() {
            crate::watches::unwatch(store, s_id, n_id);
        }
    };
    let watch_menu = move || match node_of() {
        Some((s_id, n_id)) => store.watches.with(|all| crate::watches::menu_state(all, s_id, n_id)),
        None => crate::watches::MenuState { watch: false, watch_subtree: false, unwatch: false },
    };
    // "Open in Split View" (docs/SPLIT_VIEW_CONTRACT.md decision 2): the
    // focused pane is split to the right and this document opens in the new
    // half. Off with four panes open.
    let on_open_in_split = move || {
        let focused = store.focused();
        if let Some(new) = crate::pane_view::split_pane(store, focused, crate::panes::Direction::Right) {
            open_node_in(store, tree_state, new, row());
        }
    };
    // New: one list, `crate::creating`, made in the row (a mount's source).
    let make = move |kind: &crate::creating::Kind| crate::creating::make_in_row(store, kind, &row());

    let sync_applies = move || !menu.store_off(|s| s.link_to_remote && s.host && s.stop_relaying && s.unlink && s.remove_replica);

    // Open only on a row: a right press on the tree's empty space aims the
    // menu at none, and the menu closes before it is drawn.
    let opened = Signal::new(false);
    let _ = rinch::Effect::new(move || {
        if opened.get() && menu.kind.get() == RowKind::None {
            opened.set(false);
        }
    });

    let target = tree.clone();
    let menu_view = rsx! {
        ContextMenu {
            opened: opened,
            ContextMenuTarget { {tree} }
            ContextMenuDropdown {
                div {
                    class: "pimble-menu-note",
                    style: {move || if menu.note.with(|n| n.is_empty()) { "display: none;" } else { "" }},
                    {move || menu.note.get()}
                }
                if menu.kind.get() == RowKind::Store {
                    DropdownMenuSub {
                        label: "New",
                        left_section: TablerIcon::Plus,
                        disabled: {move || menu.store_off(|s| s.new_node)},
                        for item in crate::creating::kinds(store) {
                            DropdownMenuItem {
                                key: item.label.clone(),
                                left_section: item.icon,
                                onclick: { let kind = item.kind.clone(); move || make(&kind) },
                                {item.label.clone()}
                            }
                        }
                    }
                    DropdownMenuDivider {}
                    DropdownMenuItem {
                        left_section: TablerIcon::Copy,
                        onclick: on_copy,
                        "Copy"
                    }
                    DropdownMenuItem {
                        left_section: TablerIcon::Clipboard,
                        disabled: {move || menu.store_off(|s| s.new_node)},
                        onclick: on_paste,
                        "Paste"
                    }
                    DropdownMenuDivider {}
                    if CAN_ADMINISTER_STORES {
                        DropdownMenuItem {
                            left_section: TablerIcon::Share,
                            disabled: {move || menu.store_off(|s| s.share)},
                            onclick: on_share,
                            "Share..."
                        }
                    }
                    // How the store reaches other places: only what can apply
                    // to it now (principle 2), in one submenu, left out when
                    // nothing applies.
                    if CAN_ADMINISTER_STORES && sync_applies() {
                        DropdownMenuSub {
                            label: "Sync",
                            left_section: TablerIcon::Cloud,
                            if !menu.store_off(|s| s.link_to_remote) {
                                DropdownMenuItem {
                                    left_section: TablerIcon::Cloud,
                                    onclick: on_link_to_remote,
                                    "Link to Remote..."
                                }
                            }
                            if !menu.store_off(|s| s.host) {
                                DropdownMenuItem {
                                    left_section: TablerIcon::CloudUpload,
                                    onclick: on_host_on_cloud,
                                    "Host on Pimble Cloud..."
                                }
                            }
                            if !menu.store_off(|s| s.stop_relaying) {
                                DropdownMenuItem {
                                    left_section: TablerIcon::ShareOff,
                                    onclick: on_stop_relaying,
                                    "Stop Sharing from This Computer..."
                                }
                            }
                            if !menu.store_off(|s| s.unlink) {
                                DropdownMenuItem {
                                    left_section: TablerIcon::Unlink,
                                    onclick: on_unlink_from_remote,
                                    "Unlink from Remote"
                                }
                            }
                            if !menu.store_off(|s| s.remove_replica) {
                                DropdownMenuItem {
                                    left_section: TablerIcon::Trash,
                                    onclick: on_remove_replica,
                                    "Remove Replica..."
                                }
                            }
                        }
                    }
                    if menu.mounts_apply.get() || CAN_ADMINISTER_STORES {
                        DropdownMenuSub {
                            label: "Mount",
                            left_section: TablerIcon::Link,
                            if menu.mounts_apply.get() {
                                DropdownMenuItem {
                                    left_section: TablerIcon::Link,
                                    disabled: {move || menu.store_off(|s| s.mount_store)},
                                    onclick: on_mount_store,
                                    "Mount Store Here..."
                                }
                            }
                            if CAN_ADMINISTER_STORES {
                                DropdownMenuItem {
                                    left_section: TablerIcon::CloudDownload,
                                    disabled: {move || menu.store_off(|s| s.mount_remote_store)},
                                    onclick: on_mount_remote_store,
                                    "Mount Remote Store Here..."
                                }
                            }
                        }
                    }
                    DropdownMenuDivider {}
                    DropdownMenuItem {
                        left_section: TablerIcon::Palette,
                        disabled: {move || menu.store_off(|s| s.appearance)},
                        onclick: on_appearance,
                        "Appearance..."
                    }
                    DropdownMenuItem {
                        left_section: TablerIcon::History,
                        disabled: {move || menu.store_off(|s| s.recently_deleted)},
                        onclick: on_recently_deleted,
                        "Recently Deleted..."
                    }
                    if CAN_ADMINISTER_STORES {
                        DropdownMenuDivider {}
                        DropdownMenuItem {
                            left_section: TablerIcon::X,
                            onclick: on_close_store,
                            "Close Store"
                        }
                    }
                }
                // New makes in the mount's source, never under the mount node
                // itself (the server rejects a mount node as a parent).
                if menu.kind.get() == RowKind::Mount {
                    DropdownMenuSub {
                        label: "New",
                        left_section: TablerIcon::Plus,
                        disabled: {move || !menu.can_write_mount_source.get()},
                        for item in crate::creating::kinds(store) {
                            DropdownMenuItem {
                                key: item.label.clone(),
                                left_section: item.icon,
                                onclick: { let kind = item.kind.clone(); move || make(&kind) },
                                {item.label.clone()}
                            }
                        }
                    }
                    DropdownMenuDivider {}
                    DropdownMenuItem {
                        left_section: TablerIcon::Palette,
                        disabled: {move || !menu.can_write_tree.get()},
                        onclick: on_appearance,
                        "Appearance..."
                    }
                    DropdownMenuDivider {}
                    DropdownMenuItem {
                        left_section: TablerIcon::Trash,
                        disabled: {move || !menu.can_write_tree.get()},
                        onclick: on_delete,
                        "Delete"
                    }
                }
                if menu.kind.get() == RowKind::Node {
                    DropdownMenuItem {
                        left_section: TablerIcon::LayoutColumns,
                        disabled: {move || several() || !store.tiling.with(|tiling| tiling.can_split())},
                        onclick: on_open_in_split,
                        "Open in Split View"
                    }
                    DropdownMenuSub {
                        label: "New",
                        left_section: TablerIcon::Plus,
                        disabled: {move || !menu.can_write_tree.get()},
                        for item in crate::creating::kinds(store) {
                            DropdownMenuItem {
                                key: item.label.clone(),
                                left_section: item.icon,
                                onclick: { let kind = item.kind.clone(); move || make(&kind) },
                                {item.label.clone()}
                            }
                        }
                    }
                    DropdownMenuItem {
                        left_section: TablerIcon::MessagePlus,
                        // A commenter's too (docs/DISCUSSIONS_CONTRACT.md section 7).
                        disabled: {move || !menu.can_comment.get()},
                        onclick: on_start_discussion,
                        "Discuss"
                    }
                    DropdownMenuSub {
                        label: "Watch",
                        left_section: TablerIcon::Eye,
                        disabled: {move || several() || !crate::watches::can_watch(store)},
                        DropdownMenuItem {
                            left_section: TablerIcon::Eye,
                            disabled: {move || !watch_menu().watch},
                            onclick: move || on_watch(false),
                            "Watch"
                        }
                        DropdownMenuItem {
                            left_section: TablerIcon::Eye,
                            disabled: {move || !watch_menu().watch_subtree},
                            onclick: move || on_watch(true),
                            "Watch with Subtree"
                        }
                        DropdownMenuItem {
                            left_section: TablerIcon::EyeOff,
                            disabled: {move || !watch_menu().unwatch},
                            onclick: on_unwatch,
                            "Unwatch"
                        }
                    }
                    DropdownMenuDivider {}
                    DropdownMenuItem {
                        left_section: TablerIcon::Edit,
                        disabled: {move || !menu.can_write_tree.get() || several()},
                        onclick: on_rename,
                        "Rename"
                    }
                    DropdownMenuItem {
                        left_section: TablerIcon::Palette,
                        disabled: {move || !menu.can_write_tree.get()},
                        onclick: on_appearance,
                        "Appearance..."
                    }
                    DropdownMenuDivider {}
                    DropdownMenuItem {
                        left_section: TablerIcon::Scissors,
                        disabled: {move || menu.cut_off.get()},
                        onclick: on_cut,
                        "Cut"
                    }
                    DropdownMenuItem {
                        left_section: TablerIcon::Copy,
                        onclick: on_copy,
                        "Copy"
                    }
                    DropdownMenuItem {
                        left_section: TablerIcon::Clipboard,
                        disabled: {move || menu.paste_off.get() || several()},
                        onclick: on_paste,
                        "Paste"
                    }
                    if menu.mounts_apply.get() || CAN_ADMINISTER_STORES {
                        DropdownMenuSub {
                            label: "Mount",
                            left_section: TablerIcon::Link,
                            disabled: {move || menu.paste_off.get()},
                            if menu.mounts_apply.get() {
                                DropdownMenuItem {
                                    left_section: TablerIcon::Link,
                                    onclick: on_mount_store,
                                    "Mount Store Here..."
                                }
                            }
                            if CAN_ADMINISTER_STORES {
                                DropdownMenuItem {
                                    left_section: TablerIcon::CloudDownload,
                                    onclick: on_mount_remote_store,
                                    "Mount Remote Store Here..."
                                }
                            }
                        }
                    }
                    if CAN_ADMINISTER_STORES {
                        DropdownMenuItem {
                            left_section: TablerIcon::Share,
                            disabled: {move || menu.share_disabled.get()},
                            onclick: on_share,
                            "Share..."
                        }
                    }
                    DropdownMenuDivider {}
                    DropdownMenuItem {
                        left_section: TablerIcon::Trash,
                        disabled: {move || menu.delete_off.get()},
                        onclick: on_delete,
                        {move || match menu.acts_on.get() {
                            1 => "Delete".to_string(),
                            n => format!("Delete {n} Nodes"),
                        }}
                    }
                }
            }
        }
    };
    // ContextMenu's own boxes are `display: contents`, invisible to the hit
    // test: the tree's box carries the handler (hoisted from the target).
    hoist_context_menu_handler(&menu_view, &target);
    menu_view
}

fn hoist_context_menu_handler(from: &NodeHandle, target: &NodeHandle) {
    if let Some(handler_id) = find_context_menu_handler(from) {
        target.set_attribute("data-oncontextmenu", &handler_id);
    }
}

fn find_context_menu_handler(node: &NodeHandle) -> Option<String> {
    if let Some(val) = node.get_attribute("data-oncontextmenu") {
        return Some(val);
    }
    for child in node.children() {
        if let Some(val) = find_context_menu_handler(&child) {
            return Some(val);
        }
    }
    None
}
