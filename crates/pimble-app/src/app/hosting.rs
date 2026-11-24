//! Pimble Cloud hosting and sharing from this computer: the confirmations
//! and the "Add Hosted Store..." list.

use rinch::prelude::*;

use crate::protocol::BackendCommand;
use crate::state::AppStore;
use super::account::open_account_modal;

/// "Host on Pimble Cloud..." for `store_id`: the confirmation when an account
/// is signed in, the Account modal with a hint otherwise (decision 5).
pub fn open_host_modal(store: AppStore, store_id: pimble_core::StoreId) {
    if !untracked(|| store.cloud_signed_in.get()) {
        open_account_modal(store, "Sign in to host a store");
        return;
    }
    store.host_modal_error.set(String::new());
    store.host_modal_busy.set(false);
    store.host_modal_store.set(Some(store_id));
}

/// "Stop Sharing from This Computer..." for `store_id`
/// (docs/RELAY_CONTRACT.md): the confirmation, which sends
/// `CloudStopRelaying`. The server refuses it while the store still has
/// shares, and its sentence lands on the confirmation's error line.
pub fn open_stop_relay_modal(store: AppStore, store_id: pimble_core::StoreId) {
    store.stop_relay_modal_error.set(String::new());
    store.stop_relay_modal_busy.set(false);
    store.stop_relay_modal_store.set(Some(store_id));
}

/// "Share from this computer", the first of the Share modal's two ways for a
/// store that is not hosted (docs/RELAY_CONTRACT.md, "The apps"): the person
/// chose it, so the store is shared from here (`CloudRelayStore`; nothing is
/// uploaded), and `CloudStoreRelayed` moves the modal on to the share's name.
/// A refusal is the server's sentence on the modal's error line.
pub fn share_from_this_computer(store: AppStore) {
    let Some((store_id, _)) = untracked(|| store.share_modal_node.get()) else { return };
    store.share_modal_error.set(String::new());
    store.share_modal_pending.set(Some(crate::protocol::CloudOp::RelayStore));
    store.send(BackendCommand::CloudRelayStore { store_id });
}

/// "Host on Pimble Cloud...", the second way: the hosting confirmation the
/// store row's menu opens, which says what hosting does and asks before
/// anything is uploaded. The Share modal makes way for it and remembers its
/// node, so that a store the person then hosts carries on into the share
/// (`CloudStoreHosted`); closing the confirmation forgets it.
pub fn host_to_share(store: AppStore) {
    let Some((store_id, node_id)) = untracked(|| store.share_modal_node.get()) else { return };
    store.share_modal_node.set(None);
    store.share_modal_error.set(String::new());
    store.share_modal_pending.set(None);
    // Nothing to carry on from when the confirmation does not open (nobody
    // is signed in, and the Account modal opened in its place).
    let signed_in = untracked(|| store.cloud_signed_in.get());
    store.share_after_host.set(signed_in.then_some((store_id, node_id)));
    open_host_modal(store, store_id);
}

/// Open the "Add Hosted Store..." modal and ask for the account's stores at
/// once (decision 6). Not signed in, the modal says so and offers the
/// Account modal instead of a list.
pub fn open_hosted_modal(store: AppStore) {
    store.hosted_modal_stores.set(Vec::new());
    store.hosted_modal_relayed.set(Vec::new());
    store.hosted_modal_selected.set(String::new());
    store.hosted_modal_error.set(String::new());
    let signed_in = untracked(|| store.cloud_signed_in.get());
    store.hosted_modal_busy.set(signed_in);
    store.hosted_modal_open.set(true);
    if signed_in {
        store.send(BackendCommand::CloudListHostedStores);
    }
}

/// How a hosted store reads in the "Add Hosted Store..." list. A row with a
/// `root` is a share — a scope inside someone else's store — so it reads as
/// the share's own name and who shared it, never the owner's store name,
/// which no recipient ever sees (docs/NODE_DOCUMENT_CONTRACT.md section 5,
/// "A share has a name of its own").
///
/// `relayed` is the row's tier (docs/RELAY_CONTRACT.md): such a store is not
/// hosted anywhere, it is served from its owner's computer while that is on,
/// and the row says whose. Pimble Cloud holds no name for one, so a row that
/// is not a share (one's own store on another of one's computers, or a grant
/// on the whole of someone else's) has none to show and reads as where it is.
pub(super) fn hosted_store_label(info: &pimble_rpc::CloudHostedStoreInfo, relayed: bool) -> String {
    let name = info.name.trim();
    match (relayed, &info.shared_by) {
        (false, Some(email)) if info.root.is_some() => format!("{}, shared by {}", info.name, email),
        (false, _) => info.name.clone(),
        (true, Some(email)) if name.is_empty() => format!("Shared from {email}'s computer"),
        (true, Some(email)) => format!("{name}, shared from {email}'s computer"),
        (true, None) if name.is_empty() => crate::state::RELAYED_PLACEHOLDER_NAME.to_string(),
        (true, None) => format!("{name}, shared from another computer"),
    }
}

/// The "Host on Pimble Cloud..." confirmation.
pub(super) fn render_host_modal(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    // ── "Host on Pimble Cloud..." confirmation (store root context
    // menu) ─────────────────────────────────────────────────────────
    // decision 5: names the store and the account, then `CloudHostStore`.
    // `CloudStoreHosted` closes it; a `CloudError { HostStore }` lands
    // on its error line.
    rsx! {
        Modal {
            opened_fn: move || store.host_modal_store.get().is_some(),
            onclose: move || {
                store.host_modal_store.set(None);
                store.host_modal_error.set(String::new());
                // Not hosted, so there is no share to carry on into.
                store.share_after_host.set(None);
            },
            title: "Host on Pimble Cloud",
            size: "sm",

            div {
                style: "display: flex; flex-direction: column; gap: 10px;",

                div {
                    {|| {
                        let Some(store_id) = store.host_modal_store.get() else {
                            return String::new();
                        };
                        let name = store.get_store_signal(store_id)
                            .map(|sig| sig.with(|s| s.name.clone()))
                            .unwrap_or_default();
                        format!("Host \"{}\" on Pimble Cloud as {}?", name, store.cloud_email.get())
                    }}
                }

                div {
                    style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                    "Encrypted before it leaves this machine. Its key is made here and \
                     sealed to your account; the server stores what it cannot read."
                }

                if !store.host_modal_error.get().is_empty() {
                    div {
                        style: "color: var(--rinch-color-red-6); font-size: 12px;",
                        {|| store.host_modal_error.get()}
                    }
                }

                div {
                    style: "display: flex; justify-content: flex-end; gap: 8px;",
                    Button {
                        variant: "light",
                        size: "sm",
                        disabled: {|| store.host_modal_busy.get()},
                        onclick: move || {
                            store.host_modal_store.set(None);
                            store.host_modal_error.set(String::new());
                            store.share_after_host.set(None);
                        },
                        "Cancel"
                    }
                    Button {
                        variant: "filled",
                        size: "sm",
                        loading: {|| store.host_modal_busy.get()},
                        disabled: {|| store.host_modal_busy.get()},
                        onclick: move || {
                            let Some(store_id) = untracked(|| store.host_modal_store.get()) else { return };
                            store.host_modal_error.set(String::new());
                            store.host_modal_busy.set(true);
                            store.send(BackendCommand::CloudHostStore { store_id });
                        },
                        "Host"
                    }
                }
            }
        }
    }
}

/// The "Stop Sharing from This Computer..." confirmation.
pub(super) fn render_stop_relay_modal(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    // ── "Stop Sharing from This Computer..." confirmation (store root
    // context menu) ─────────────────────────────────────────────────
    // docs/RELAY_CONTRACT.md: the way back from "Share from this
    // computer". Names the store and says what stopping does, then
    // `CloudStopRelaying`. `CloudRelayingStopped` closes it; the server's
    // refusal while the store still has shares (a
    // `CloudError { StopRelaying }`) lands on its error line as it is.
    rsx! {
        Modal {
            opened_fn: move || store.stop_relay_modal_store.get().is_some(),
            onclose: move || {
                store.stop_relay_modal_store.set(None);
                store.stop_relay_modal_error.set(String::new());
            },
            title: "Stop Sharing from This Computer",
            size: "sm",

            div {
                style: "display: flex; flex-direction: column; gap: 10px;",

                div {
                    {|| {
                        let Some(store_id) = store.stop_relay_modal_store.get() else {
                            return String::new();
                        };
                        let name = store.get_store_signal(store_id)
                            .map(|sig| sig.with(|s| s.name.clone()))
                            .unwrap_or_default();
                        format!("Stop sharing \"{}\" from this computer?", name)
                    }}
                }

                div {
                    style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                    "Nobody will reach this store through Pimble Cloud any more, and its \
                     encrypted copy for them on this computer is deleted. The store and \
                     everything in it stay as they are. Each share in it has to be stopped first."
                }

                if !store.stop_relay_modal_error.get().is_empty() {
                    div {
                        style: "color: var(--rinch-color-red-6); font-size: 12px;",
                        {|| store.stop_relay_modal_error.get()}
                    }
                }

                div {
                    style: "display: flex; justify-content: flex-end; gap: 8px;",
                    Button {
                        variant: "light",
                        size: "sm",
                        disabled: {|| store.stop_relay_modal_busy.get()},
                        onclick: move || {
                            store.stop_relay_modal_store.set(None);
                            store.stop_relay_modal_error.set(String::new());
                        },
                        "Cancel"
                    }
                    Button {
                        variant: "filled",
                        color: "red",
                        size: "sm",
                        loading: {|| store.stop_relay_modal_busy.get()},
                        disabled: {|| store.stop_relay_modal_busy.get()},
                        onclick: move || {
                            let Some(store_id) = untracked(|| store.stop_relay_modal_store.get()) else { return };
                            store.stop_relay_modal_error.set(String::new());
                            store.stop_relay_modal_busy.set(true);
                            store.send(BackendCommand::CloudStopRelaying { store_id });
                        },
                        "Stop Sharing"
                    }
                }
            }
        }
    }
}

/// The "Add Hosted Store..." modal.
pub(super) fn render_hosted_modal(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    // ── "Add Hosted Store..." modal (Account menu) ──────────────────
    // decision 6: the account's encrypted stores and shares not already
    // open here (the event handler filters `CloudHostedStoresListed`),
    // one of which becomes a local replica — a whole one for a store of
    // one's own, a partial one for a share. The store arrives as
    // `StoreOpened`, which closes the modal and carries the roots this
    // device now holds (docs/NODE_DOCUMENT_CONTRACT.md section 5).
    let add_hosted_store = move || {
        let selected = untracked(|| store.hosted_modal_selected.get());
        let Ok(uuid) = selected.parse::<uuid::Uuid>() else {
            store.hosted_modal_error.set("Select a store first".to_string());
            return;
        };
        store.hosted_modal_error.set(String::new());
        store.hosted_modal_busy.set(true);
        store.send(BackendCommand::CloudAddHostedStore { store_id: pimble_core::StoreId(uuid) });
    };
    rsx! {
        Modal {
            opened_fn: move || store.hosted_modal_open.get(),
            onclose: move || {
                store.hosted_modal_open.set(false);
                store.hosted_modal_error.set(String::new());
            },
            title: "Add Hosted Store",
            size: "sm",

            div {
                style: "display: flex; flex-direction: column; gap: 10px;",

                // Nothing to list without an account.
                if !store.cloud_signed_in.get() {
                    div {
                        style: "display: flex; flex-direction: column; gap: 10px;",
                        div { "Sign in first." }
                        Button {
                            variant: "filled",
                            size: "sm",
                            onclick: move || {
                                store.hosted_modal_open.set(false);
                                store.hosted_modal_error.set(String::new());
                                open_account_modal(store, "");
                            },
                            "Account..."
                        }
                    }
                }

                if store.cloud_signed_in.get() {
                    div {
                        style: "display: flex; flex-direction: column; gap: 10px;",

                        Select {
                            label: "Store",
                            placeholder: "Select a store",
                            value_fn: move || store.hosted_modal_selected.get(),
                            onchange: move |val: String| store.hosted_modal_selected.set(val),
                            data: {|| {
                                // A row served from its owner's computer
                                // says so (docs/RELAY_CONTRACT.md).
                                let relayed = store.hosted_modal_relayed.get();
                                store.hosted_modal_stores.get().iter()
                                    .map(|s| SelectOption::new(s.store_id.clone(), hosted_store_label(s, relayed.contains(&s.store_id))))
                                    .collect::<Vec<_>>()
                            }},
                        }

                        div {
                            style: {
                                move || {
                                    let nothing = !store.hosted_modal_busy.get()
                                        && store.hosted_modal_error.get().is_empty()
                                        && store.hosted_modal_stores.with(|s| s.is_empty());
                                    if nothing {
                                        "font-size: 12px; color: var(--rinch-color-dimmed);"
                                    } else {
                                        "display: none;"
                                    }
                                }
                            },
                            "Every encrypted store and share on this account is already open here."
                        }

                        Button {
                            variant: "filled",
                            size: "sm",
                            loading: {|| store.hosted_modal_busy.get()},
                            disabled: {|| store.hosted_modal_busy.get() || store.hosted_modal_selected.get().is_empty()},
                            onclick: add_hosted_store,
                            "Add"
                        }
                    }
                }

                if !store.hosted_modal_error.get().is_empty() {
                    div {
                        style: "color: var(--rinch-color-red-6); font-size: 12px;",
                        {|| store.hosted_modal_error.get()}
                    }
                }
            }
        }
    }
}
