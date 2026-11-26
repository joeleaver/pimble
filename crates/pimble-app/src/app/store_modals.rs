//! Making, opening, linking, mounting and removing stores: the folder
//! pickers and the store dialogs.

use rinch::prelude::*;

use crate::protocol::BackendCommand;
use crate::state::AppStore;

/// "New Store...", the desktop's way: pick a path, create, open.
#[cfg(feature = "native")]
pub fn pick_and_create_store(store: AppStore) {
    let dialog = rinch::dialogs::save_file()
        .set_title("Create New Store")
        .add_filter("Pimble Store", &["pimble"]);

    let Some(path) = dialog.save() else { return };
    let path_str = path.to_string_lossy().to_string();
    let path_str = if path_str.ends_with(".pimble") {
        path_str
    } else {
        format!("{}.pimble", path_str)
    };
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "New Store".to_string());
    tracing::info!("Creating new store at: {}", path_str);
    store.pending_create_path.set(Some(path_str.clone()));
    store.send(BackendCommand::CreateStore { path: path_str, name });
}

/// "Open Store...", the desktop's way: pick a directory and open it.
#[cfg(feature = "native")]
pub fn pick_and_open_store(store: AppStore) {
    let Some(path) = rinch::dialogs::pick_folder().set_title("Open Store").pick() else {
        return;
    };
    let path_str = path.to_string_lossy().to_string();
    tracing::info!("Opening store at: {}", path_str);
    store.send(BackendCommand::OpenStore { path: path_str });
}

/// Open the "Mount Store..." picker for a target node.
///
/// The desktop's "Mount Store..." picks a directory; a browser has none to pick
/// from, so it picks one of the stores the account already grants. Either way
/// the RPC is the same `createMount`, which an editor is allowed to call.
pub fn open_mount_picker(store: AppStore, target: (pimble_core::StoreId, pimble_core::NodeId)) {
    // The target goes in first: the picker's own option list reads it to leave
    // out the store being mounted into, and a list built before it was set
    // would offer that store as a place to put itself.
    store.mount_picker_target.set(Some(target));
    let first = untracked(|| {
        store.store_ids.with(|ids| ids.iter().find(|&&id| id != target.0).copied())
    });
    store.mount_picker_selected.set(first.map(|id| id.to_string()).unwrap_or_default());
    store.mount_picker_error.set(String::new());
    store.mount_picker_pending.set(false);
}

/// Open the "New Store..." modal, empty.
pub fn open_new_store_modal(store: AppStore) {
    store.new_store_modal_name.set(String::new());
    store.new_store_modal_error.set(String::new());
    store.new_store_modal_pending.set(false);
    store.new_store_modal_open.set(true);
}

/// Open the "Add Remote Store..." modal, resetting everything a previous run
/// left behind. `target` picks the mode (docs/history/REMOTE_MOUNTS_CONTRACT.md
/// decision 9): `None` is the plain File-menu flow that only adds a replica,
/// `Some(canonical pair)` is "Mount Remote Store Here...", whose action adds
/// the replica when this server does not already have the store and then
/// mounts its root under that pair.
pub fn open_connect_modal(store: AppStore, target: Option<(pimble_core::StoreId, pimble_core::NodeId)>) {
    store.connect_modal_target.set(target);
    store.connect_modal_stores.set(Vec::new());
    store.connect_modal_selected.set(String::new());
    store.connect_modal_error.set(String::new());
    store.connect_modal_busy.set(false);
    store.connect_modal_pending_add.set(false);
    store.connect_modal_token.set(String::new());
    store.connect_modal_token_visible.set(false);
    if untracked(|| store.connect_modal_url.get()).is_empty() {
        store.connect_modal_url.set("http://".to_string());
    }
    store.connect_modal_open.set(true);
}

/// The "Add Remote Store..." modal.
pub(super) fn render_connect_modal(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    // ── "Add Remote Store..." modal (File menu) ─────────────────────
    // docs/SYNC_CONTRACT.md "B: app side". No folder/path field: the
    // server places the replica in its own data directory, so nothing
    // in this flow opens a native OS dialog (the rinch debug tools can
    // drive it end to end).
    //
    // The same modal serves "Mount Remote Store Here..." when
    // `connect_modal_target` names a target node: its action adds the
    // replica (unless the store is already open here) and then mounts it
    // there, in one command (docs/history/REMOTE_MOUNTS_CONTRACT.md decision 9).
    rsx! {
        Modal {
            opened_fn: move || store.connect_modal_open.get(),
            onclose: move || {
                store.connect_modal_open.set(false);
                store.connect_modal_error.set(String::new());
                store.connect_modal_target.set(None);
            },
            title: {|| if store.connect_modal_target.get().is_some() {
                "Mount Remote Store"
            } else {
                "Add Remote Store"
            }},
            size: "sm",

            div {
                style: "display: flex; flex-direction: column; gap: 10px;",

                TextInput {
                    label: "Server URL",
                    placeholder: "http://host:7462",
                    value_fn: move || store.connect_modal_url.get(),
                    oninput: move |val: String| store.connect_modal_url.set(val),
                }

                PasswordInput {
                    label: "Token",
                    placeholder: "Leave empty to use a saved token",
                    value_fn: move || store.connect_modal_token.get(),
                    oninput: move |val: String| store.connect_modal_token.set(val),
                    visible_fn: move || store.connect_modal_token_visible.get(),
                    ontoggle: move || store.connect_modal_token_visible.update(|v| *v = !*v),
                }

                Button {
                    variant: "light",
                    size: "sm",
                    disabled: {|| store.connect_modal_busy.get()},
                    onclick: move || {
                        let url = untracked(|| store.connect_modal_url.get());
                        let token = untracked(|| store.connect_modal_token.get());
                        store.connect_modal_busy.set(true);
                        store.connect_modal_error.set(String::new());
                        store.send(BackendCommand::ListRemoteStores { url, token });
                    },
                    "List stores"
                }

                Select {
                    label: "Store",
                    placeholder: "Select a store",
                    value_fn: move || store.connect_modal_selected.get(),
                    onchange: move |val: String| store.connect_modal_selected.set(val),
                    data: {|| {
                        store.connect_modal_stores.get().iter()
                            .map(|s| SelectOption::new(s.id.to_string(), s.name.clone()))
                            .collect::<Vec<_>>()
                    }},
                }

                Button {
                    variant: "filled",
                    size: "sm",
                    disabled: {|| store.connect_modal_busy.get() || store.connect_modal_selected.get().is_empty()},
                    onclick: move || {
                        let selected = untracked(|| store.connect_modal_selected.get());
                        let Ok(remote_uuid) = selected.parse::<uuid::Uuid>() else {
                            store.connect_modal_error.set("Select a store first".to_string());
                            return;
                        };
                        let remote_store_id = pimble_core::StoreId(remote_uuid);
                        let url = untracked(|| store.connect_modal_url.get());
                        let token = untracked(|| store.connect_modal_token.get());
                        // Both modes report their outcome through the same
                        // route: `connect_modal_pending_add` claims the
                        // generic `Error` event for this modal's error line.
                        store.connect_modal_pending_add.set(true);
                        store.connect_modal_busy.set(true);
                        store.connect_modal_error.set(String::new());
                        match untracked(|| store.connect_modal_target.get()) {
                            Some((target_store_id, target_parent_id)) => {
                                store.send(BackendCommand::MountRemoteStore {
                                    url,
                                    remote_store_id,
                                    token,
                                    target_store_id,
                                    target_parent_id,
                                });
                            }
                            None => {
                                store.send(BackendCommand::AddRemoteStore { url, remote_store_id, token });
                            }
                        }
                    },
                    {|| if store.connect_modal_target.get().is_some() { "Mount" } else { "Add" }}
                }

                if !store.connect_modal_error.get().is_empty() {
                    div {
                        style: "color: var(--rinch-color-red-6); font-size: 12px;",
                        {|| store.connect_modal_error.get()}
                    }
                }
            }
        }
    }
}

/// The "Link to Remote..." modal.
pub(super) fn render_link_modal(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    // ── "Link to Remote..." modal (store root context menu) ─────────
    // docs/SYNC_CONTRACT.md "B: app side".
    rsx! {
        Modal {
            opened_fn: move || store.link_modal_store.get().is_some(),
            onclose: move || {
                store.link_modal_store.set(None);
                store.link_modal_error.set(String::new());
            },
            title: "Link to Remote",
            size: "sm",

            div {
                style: "display: flex; flex-direction: column; gap: 10px;",

                TextInput {
                    label: "Server URL",
                    placeholder: "http://host:7462",
                    value_fn: move || store.link_modal_url.get(),
                    oninput: move |val: String| store.link_modal_url.set(val),
                }

                PasswordInput {
                    label: "Token",
                    placeholder: "Leave empty to use a saved token",
                    value_fn: move || store.link_modal_token.get(),
                    oninput: move |val: String| store.link_modal_token.set(val),
                    visible_fn: move || store.link_modal_token_visible.get(),
                    ontoggle: move || store.link_modal_token_visible.update(|v| *v = !*v),
                }

                Button {
                    variant: "filled",
                    size: "sm",
                    disabled: {|| store.link_modal_pending.get()},
                    onclick: move || {
                        let Some(store_id) = untracked(|| store.link_modal_store.get()) else { return };
                        let url_str = untracked(|| store.link_modal_url.get());
                        let Ok(url) = url_str.parse::<url::Url>() else {
                            store.link_modal_error.set("Invalid URL".to_string());
                            return;
                        };
                        let token = untracked(|| store.link_modal_token.get());
                        let auth = if token.is_empty() {
                            pimble_core::AuthMethod::None
                        } else {
                            pimble_core::AuthMethod::Bearer { token }
                        };
                        store.link_modal_pending.set(true);
                        store.link_modal_error.set(String::new());
                        store.send(BackendCommand::SetStoreSync {
                            store_id,
                            remote: Some(pimble_core::RemoteEndpoint { url, auth }),
                        });
                    },
                    "Link"
                }

                if !store.link_modal_error.get().is_empty() {
                    div {
                        style: "color: var(--rinch-color-red-6); font-size: 12px;",
                        {|| store.link_modal_error.get()}
                    }
                }
            }
        }
    }
}

/// The "New Store..." modal.
pub(super) fn render_new_store_modal(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let create_hosted_store = move || {
        let name = untracked(|| store.new_store_modal_name.get()).trim().to_string();
        if name.is_empty() {
            store.new_store_modal_error.set("Give the store a name.".to_string());
            return;
        }
        store.new_store_modal_error.set(String::new());
        store.new_store_modal_pending.set(true);
        store.send(BackendCommand::CreateHostedStore { name, kind: "vault".to_string() });
    };

    // ── "New Store..." modal ────────────────────────────────────────
    // A browser build's stores live on the account, not on disk, so there
    // is no path to pick: a name and a kind is the whole of it. The
    // backend makes the store, mints its key if it is encrypted, and asks
    // for a token that carries the new grant; the store then arrives
    // through `StoresListed` like any other.

    rsx! {
        Modal {
            opened_fn: move || store.new_store_modal_open.get(),
            onclose: move || {
                store.new_store_modal_open.set(false);
                store.new_store_modal_error.set(String::new());
            },
            title: "New Store",
            size: "sm",

            div {
                style: "display: flex; flex-direction: column; gap: 10px;",

                TextInput {
                    label: "Name",
                    placeholder: "Notes",
                    value_fn: move || store.new_store_modal_name.get(),
                    oninput: move |val: String| store.new_store_modal_name.set(val),
                    onsubmit: create_hosted_store,
                }

                div {
                    style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                    "Encrypted. Its key is made here and sealed to your account; \
                     the server stores what it cannot read."
                }

                div {
                    style: {
                        move || if store.new_store_modal_error.get().is_empty() {
                            "display: none;"
                        } else {
                            "color: var(--rinch-color-red-6); font-size: 12px;"
                        }
                    },
                    {|| store.new_store_modal_error.get()}
                }

                Button {
                    variant: "filled",
                    size: "sm",
                    loading: {|| store.new_store_modal_pending.get()},
                    disabled: {|| store.new_store_modal_pending.get()},
                    onclick: create_hosted_store,
                    "Create"
                }
            }
        }
    }
}

/// File > Import's question: where the files go.
pub(super) fn render_import_modal(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    // ── File > Import: where it goes ────────────────────────────────
    // docs/IMPORT_CONTRACT.md. The files are already read; this only
    // asks for the place: under the selected node (when there is one
    // this device may write), or a new store made for it.
    rsx! {
        Modal {
            opened_fn: move || store.import_modal_open.get(),
            onclose: move || crate::import::cancel(store),
            title: "Import",
            size: "sm",

            div {
                style: "display: flex; flex-direction: column; gap: 10px;",

                div {
                    style: "font-size: 13px;",
                    {|| store.import_modal_label.get()}
                }

                div {
                    style: {
                        move || if store.import_modal_under.get().is_some() { "display: flex; flex-direction: column; gap: 6px;" } else { "display: none;" }
                    },
                    Button {
                        variant: "filled",
                        size: "sm",
                        onclick: move || crate::import::under_selection(store),
                        {|| match store.import_modal_under.get() {
                            Some((_, _, title)) => format!("Import under \"{title}\""),
                            None => String::new(),
                        }}
                    }
                    div {
                        style: "font-size: 12px; color: var(--rinch-color-dimmed); text-align: center;",
                        "or"
                    }
                }

                TextInput {
                    label: "New store",
                    value_fn: move || store.import_modal_store_name.get(),
                    oninput: move |val: String| store.import_modal_store_name.set(val),
                    onsubmit: move || crate::import::into_new_store(store),
                }

                div {
                    style: {
                        move || if store.import_modal_error.get().is_empty() {
                            "display: none;"
                        } else {
                            "color: var(--rinch-color-red-6); font-size: 12px;"
                        }
                    },
                    {|| store.import_modal_error.get()}
                }

                Button {
                    variant: "default",
                    size: "sm",
                    onclick: move || crate::import::into_new_store(store),
                    "Import into a new store"
                }
            }
        }
    }
}

/// The "Mount Store..." picker.
pub(super) fn render_mount_picker(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    // ── "Mount Store..." picker ─────────────────────────────────────
    // The desktop picks a directory on this machine. A browser has none to
    // pick from: its stores live on the account, so it picks one of those.
    // Either way the RPC is `createMount`, which an editor may call — the
    // picker is a different way to name the same thing, not a lesser one.
    let do_mount_store = move || {
        let Some((target_store, target_parent)) = untracked(|| store.mount_picker_target.get())
        else {
            return;
        };
        let selected = untracked(|| store.mount_picker_selected.get());
        let Ok(uuid) = selected.parse::<uuid::Uuid>() else {
            store.mount_picker_error.set("Choose a store to mount.".to_string());
            return;
        };
        let source_store_id = pimble_core::StoreId(uuid);
        // A store's root is what gets mounted; the source keeps its own name.
        let Some(source_root) = store.root_node_id(source_store_id) else {
            store.mount_picker_error.set("That store has not finished opening.".to_string());
            return;
        };
        let title = store
            .get_store_signal(source_store_id)
            .map(|sig| untracked(|| sig.with(|s| s.name.clone())));

        store.mount_picker_error.set(String::new());
        store.mount_picker_pending.set(true);
        store.send(BackendCommand::CreateMount {
            store_id: target_store,
            parent_id: target_parent,
            source_store_id,
            source_node_id: source_root,
            title,
        });
    };

    rsx! {
        Modal {
            opened_fn: move || store.mount_picker_target.get().is_some(),
            onclose: move || {
                store.mount_picker_target.set(None);
                store.mount_picker_error.set(String::new());
            },
            title: "Mount Store",
            size: "sm",

            div {
                style: "display: flex; flex-direction: column; gap: 10px;",

                Select {
                    label: "Store",
                    placeholder: "Choose a store",
                    value_fn: move || store.mount_picker_selected.get(),
                    onchange: move |val: String| store.mount_picker_selected.set(val),
                    data: {|| {
                        // Everything open except the one being mounted into:
                        // a store cannot contain itself.
                        let target = store.mount_picker_target.get().map(|(sid, _)| sid);
                        store.store_ids.get().iter()
                            .filter(|sid| Some(**sid) != target)
                            .filter_map(|sid| {
                                store.get_store_signal(*sid)
                                    .map(|sig| sig.with(|s| SelectOption::new(sid.to_string(), s.name.clone())))
                            })
                            .collect::<Vec<_>>()
                    }},
                }

                div {
                    style: {
                        move || if store.mount_picker_error.get().is_empty() {
                            "display: none;"
                        } else {
                            "color: var(--rinch-color-red-6); font-size: 12px;"
                        }
                    },
                    {|| store.mount_picker_error.get()}
                }

                Button {
                    variant: "filled",
                    size: "sm",
                    loading: {|| store.mount_picker_pending.get()},
                    disabled: {|| store.mount_picker_pending.get() || store.mount_picker_selected.get().is_empty()},
                    onclick: do_mount_store,
                    "Mount"
                }
            }
        }
    }
}

/// The "Remove Replica..." confirmation.
pub(super) fn render_remove_replica_modal(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    // ── "Remove Replica..." confirmation modal (store root context
    // menu) ──────────────────────────────────────────────────────────
    // docs/history/HARDENING_CONTRACT.md "B: app". Names the store and its
    // remote; when the link isn't `Synced`, warns before removing with
    // `force`.

    rsx! {
        Modal {
            opened_fn: move || store.remove_replica_modal_store.get().is_some(),
            onclose: move || {
                store.remove_replica_modal_store.set(None);
                store.remove_replica_modal_error.set(String::new());
            },
            title: "Remove Replica",
            size: "sm",

            div {
                style: "display: flex; flex-direction: column; gap: 10px;",

                div {
                    {|| {
                        let Some(store_id) = store.remove_replica_modal_store.get() else {
                            return String::new();
                        };
                        let name = store.get_store_signal(store_id)
                            .map(|sig| sig.with(|s| s.name.clone()))
                            .unwrap_or_default();
                        let remote_url = store.get_sync_signal(store_id)
                            .and_then(|sig| sig.with(|(remote, _)| {
                                remote.as_ref().map(|r| r.url.as_str().trim_end_matches('/').to_string())
                            }));
                        match remote_url {
                            Some(url) => format!("Remove the local copy of \"{}\"? The store stays on {}.", name, url),
                            None => format!("Remove the local copy of \"{}\"?", name),
                        }
                    }}
                }

                div {
                    style: {
                        move || {
                            let unsynced = store.remove_replica_modal_store.get()
                                .and_then(|sid| store.get_sync_signal(sid))
                                .map(|sig| sig.with(|(_, state)| {
                                    !matches!(state, pimble_core::SyncState::Synced { .. })
                                }))
                                .unwrap_or(false);
                            if unsynced {
                                "color: var(--rinch-color-yellow-6); font-size: 12px;"
                            } else {
                                "display: none;"
                            }
                        }
                    },
                    "This replica is not synced right now. Anything changed here since it last synced will be lost."
                }

                Button {
                    variant: "filled",
                    color: "red",
                    size: "sm",
                    disabled: {|| store.remove_replica_modal_pending.get()},
                    onclick: move || {
                        let Some(store_id) = untracked(|| store.remove_replica_modal_store.get()) else { return };
                        let force = store.get_sync_signal(store_id)
                            .map(|sig| untracked(|| sig.with(|(_, state)| {
                                !matches!(state, pimble_core::SyncState::Synced { .. })
                            })))
                            .unwrap_or(false);
                        store.remove_replica_modal_pending.set(true);
                        store.remove_replica_modal_error.set(String::new());
                        store.send(BackendCommand::RemoveReplica { store_id, force });
                    },
                    "Remove"
                }

                if !store.remove_replica_modal_error.get().is_empty() {
                    div {
                        style: "color: var(--rinch-color-red-6); font-size: 12px;",
                        {|| store.remove_replica_modal_error.get()}
                    }
                }
            }
        }
    }
}
