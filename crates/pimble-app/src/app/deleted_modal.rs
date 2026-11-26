//! The "Recently Deleted..." modal (docs/MOVE_CONTRACT.md).

use rinch::prelude::*;

use crate::protocol::BackendCommand;
use crate::state::{parse_tree_value, AppStore};
use crate::state::{deleted_row_meta, deleted_row_read_only};

/// "Recently Deleted..." for `store_id` (docs/MOVE_CONTRACT.md "Seeing and
/// undoing what was removed"): the store row's context menu already knows
/// which store. Clears the last list before asking, so a reopen never shows
/// a stale one while the answer is on its way.
pub fn open_deleted_modal(store: AppStore, store_id: pimble_core::StoreId) {
    store.deleted_modal_store.set(Some(store_id));
    store.deleted_modal_nodes.set(Vec::new());
    store.deleted_modal_error.set(String::new());
    store.deleted_modal_pending.set(true);
    store.send(BackendCommand::ListDeleted { store_id });
}

/// View > "Recently Deleted...": the store of the current selection (a node
/// or a store root), else the first open store — there is always one to ask
/// while any store is open.
pub fn open_deleted_modal_for_selection(store: AppStore) {
    let selected_store = untracked(|| store.selected_id.get())
        .and_then(|value| parse_tree_value(&value))
        .map(|(store_id, _)| store_id);
    let target = selected_store.or_else(|| untracked(|| store.store_ids.get().first().copied()));
    if let Some(store_id) = target {
        open_deleted_modal(store, store_id);
    }
}

/// The "Recently Deleted..." modal.
pub(super) fn render(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    // ── "Recently Deleted..." modal (View menu, store row context menu)
    // ─────────────────────────────────────────────────────────────────
    // docs/MOVE_CONTRACT.md "Seeing and undoing what was removed": the
    // top-most tombstones and the live nodes no list names, one store at
    // a time. `DeletedListed` fills `deleted_modal_nodes`; a "Put Back"
    // re-asks so the row it worked on leaves.
    let put_back_now = move |node_id: pimble_core::NodeId, deleted_at: Option<String>, put_back_under: Option<pimble_core::NodeId>| {
        let Some(store_id) = untracked(|| store.deleted_modal_store.get()) else { return };
        if deleted_at.is_some() {
            // A tombstone: `undeleteNode` brings back what its deletion took.
            store.deleted_modal_pending.set(true);
            store.send(BackendCommand::UndeleteNode { store_id, node_id });
        } else if let Some(new_parent_id) = put_back_under {
            // A live node no list names: put it back under the scope's root.
            store.deleted_modal_pending.set(true);
            store.send(BackendCommand::MoveNode { store_id, node_id, new_parent_id, place: pimble_core::Place::Last });
        } else {
            return;
        }
        store.deleted_modal_pending.set(true);
        store.send(BackendCommand::ListDeleted { store_id });
    };
    rsx! {
        Modal {
            opened_fn: move || store.deleted_modal_store.get().is_some(),
            onclose: move || {
                store.deleted_modal_store.set(None);
                store.deleted_modal_error.set(String::new());
            },
            title: {move || {
                let name = store.deleted_modal_store.get()
                    .and_then(|sid| store.get_store_signal(sid))
                    .map(|sig| sig.with(|s| s.name.clone()))
                    .unwrap_or_default();
                format!("Recently Deleted in \"{}\"", name)
            }},
            size: "sm",

            div {
                style: "display: flex; flex-direction: column; gap: 10px;",

                div {
                    style: {
                        move || if store.deleted_modal_nodes.with(|n| n.is_empty()) {
                            "font-size: 12px; color: var(--rinch-color-dimmed);"
                        } else {
                            "display: none;"
                        }
                    },
                    "Nothing deleted recently."
                }

                div {
                    class: "pimble-deleted__list",
                    style: {
                        move || if store.deleted_modal_nodes.with(|n| n.is_empty()) { "display: none;" } else { "" }
                    },
                    for row in store.deleted_modal_nodes.get() {
                        let read_only = store.deleted_modal_store.get()
                            .map(|sid| deleted_row_read_only(store.store_access(sid), &row))
                            .unwrap_or(true);
                        let node_id = row.node_id;
                        let deleted_at = row.deleted_at.clone();
                        let put_back_under = row.put_back_under;
                        let title_text = row.title.clone();
                        let meta_text = deleted_row_meta(&row);
                        div {
                            key: node_id.to_string(),
                            class: "pimble-deleted__row",
                            div {
                                style: "flex: 1; min-width: 0;",
                                div {
                                    style: "font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                    {title_text}
                                }
                                div {
                                    style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                                    {meta_text}
                                }
                            }
                            span {
                                title: if read_only { pimble_core::StoreAccess::READ_ONLY_REFUSAL } else { "" },
                                Button {
                                    variant: "light",
                                    size: "xs",
                                    disabled: read_only,
                                    onclick: move || put_back_now(node_id, deleted_at.clone(), put_back_under),
                                    "Put Back"
                                }
                            }
                        }
                    }
                }

                if !store.deleted_modal_error.get().is_empty() {
                    div {
                        style: "color: var(--rinch-color-red-6); font-size: 12px;",
                        {|| store.deleted_modal_error.get()}
                    }
                }
            }
        }
    }
}
