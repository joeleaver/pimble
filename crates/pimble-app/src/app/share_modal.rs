//! The "Share..." modal (docs/NODE_DOCUMENT_CONTRACT.md section 5).

use rinch::prelude::*;
use rinch_tabler_icons::TablerIcon;

use crate::protocol::{BackendCommand, CloudOp};
use crate::state::AppStore;
use crate::state::{ShareFace, HOST_ON_CLOUD_LABEL, HOST_ON_CLOUD_SENTENCE, SHARE_FROM_HERE_LABEL, SHARE_FROM_HERE_SENTENCE};
use super::account::open_account_modal;
use super::hosting::{host_to_share, share_from_this_computer};

/// "Share..." for `(store_id, node_id)` (docs/NODE_DOCUMENT_CONTRACT.md
/// section 5): a share is a scoped grant on this store — the node and the
/// documents under it — so it needs an account, and the Account modal opens
/// with a hint when there is none, the same rule "Host on Pimble Cloud..."
/// follows.
///
/// The node's own share marker decides which face opens, so a node already
/// shared shows its share at once; `CloudShareInfo` then fills in the members
/// the accounts service has. A share carries a name of its own, seeded here
/// from the node's title because that is usually what the owner would type.
/// In a store that is neither hosted nor shared from this computer the modal
/// opens on the two ways to share from it instead
/// (`crate::state::ShareFace::Ways`, docs/RELAY_CONTRACT.md), and moves on to
/// the name once the person has chosen one and it is done.
///
/// In a store that reached this device as someone else's share nothing can be
/// shared from here (`share_item_disabled` keeps the menu item off every node
/// but a shared root), and a shared root opens on the member's face: whose
/// share it is and who is on it, read only (`AppStore::share_modal_manages`).
/// That face is drawn from what this device already holds, so it needs no
/// sign-in; the member list is asked for when there is an account to ask as.
pub fn open_share_modal(store: AppStore, store_id: pimble_core::StoreId, node_id: pimble_core::NodeId) {
    let marker = store
        .get_node_signal(store_id, node_id)
        .and_then(|sig| untracked(|| sig.with(|n| n.metadata.share())));
    let held_as_share = store.shared_by(store_id).is_some();
    if held_as_share && marker.is_none() {
        // The menu never offers this; if something else asks, it hears the
        // reason rather than a form the server would refuse.
        crate::events::show_notice(store, crate::state::ONLY_AN_OWNER_SHARES_NOTE.to_string());
        return;
    }
    let signed_in = untracked(|| store.cloud_signed_in.get());
    if !signed_in && !held_as_share {
        open_account_modal(store, "Sign in to share a node");
        return;
    }
    let ask_for_members = marker.is_some() && signed_in;
    let name = match &marker {
        Some(marker) => marker.name.clone(),
        None => store.display_label(store_id, node_id),
    };
    store.share_modal_shared.set(marker.is_some());
    store.share_modal_name.set(name);
    store.share_modal_members.set(Vec::new());
    store.share_modal_state.set(String::new());
    store.share_modal_invite_email.set(String::new());
    store.share_modal_invite_role.set("editor".to_string());
    store.share_modal_error.set(String::new());
    store.share_modal_confirm_stop.set(false);
    store.share_modal_pending.set(ask_for_members.then_some(crate::protocol::CloudOp::ShareInfo));
    store.share_modal_node.set(Some((store_id, node_id)));
    if ask_for_members {
        store.send(BackendCommand::CloudShareInfo { store_id, node_id });
    }
}

/// How far a member is from being able to open the share, in words
/// (docs/NODE_DOCUMENT_CONTRACT.md section 5). An invitation to an address
/// with no account yet, and a member whose key no device of ours has wrapped
/// yet, are both ordinary states rather than failures, so both say what is
/// happening. The key is handed over by an owner's Pimble, which is "your
/// Pimble" only to someone who manages the share (`manages`); a member
/// reading the list is told whose it is.
pub(super) fn member_status_text(status: pimble_rpc::ShareMemberStatus, manages: bool) -> &'static str {
    match (status, manages) {
        (pimble_rpc::ShareMemberStatus::Invited, _) => "invited, no account yet",
        (pimble_rpc::ShareMemberStatus::WaitingForKey, true) => "waiting for your Pimble to hand over the key",
        (pimble_rpc::ShareMemberStatus::WaitingForKey, false) => "waiting for an owner's Pimble to hand over the key",
        (pimble_rpc::ShareMemberStatus::Active, _) => "active",
    }
}

/// A member's role in words, for the member list. An editor edits everything
/// in the share — the text, the titles, the structure — so "can edit" is the
/// whole of it (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles").
pub(super) fn member_role_text(role: pimble_rpc::MemberRole) -> &'static str {
    match role {
        pimble_rpc::MemberRole::Owner => "owner",
        pimble_rpc::MemberRole::Editor => "can edit",
        pimble_rpc::MemberRole::Commenter => "can comment",
        pimble_rpc::MemberRole::Reader => "can read",
    }
}

/// The "Share..." modal.
pub(super) fn render(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    // ── "Share..." modal (node context menu) ────────────────────────
    // docs/NODE_DOCUMENT_CONTRACT.md section 5: a share is a scoped grant
    // on this store, so everyone on it edits the same documents. One
    // modal, two faces: a name and "Share" while the node is not shared;
    // the share, its members and "Stop sharing" once it is. A member
    // looking at the share they are in gets a third, read only
    // (`AppStore::share_modal_manages` decides). Every outcome
    // arrives as `CloudShareUpdated`, `CloudSharingStopped` or
    // `CloudError { op }`, so `events.rs` fills the busy and error lines
    // without guessing which request answered (the Account modal's rule,
    // decision 3).
    //
    // A store that is neither hosted nor shared from this computer has
    // nowhere for a share to live, and the server refuses one. So the
    // modal opens on a fourth face there (`ShareFace::Ways`,
    // docs/RELAY_CONTRACT.md "The apps"): the two ways, each with its
    // sentence. "Share from this computer" sends `CloudRelayStore`, and
    // when that answers the face moves on to the name by itself, because
    // it is read from the store's link (`AppStore::share_modal_face`);
    // "Host on Pimble Cloud..." hands over to the hosting confirmation,
    // and a store hosted there opens this modal again on the same node.
    // Nothing is chosen for the person: neither button is filled, neither
    // answers Enter, and closing the modal does neither.
    let share_node_now = move || {
        let Some((store_id, node_id)) = untracked(|| store.share_modal_node.get()) else { return };
        let name = untracked(|| store.share_modal_name.get()).trim().to_string();
        if name.is_empty() {
            store.share_modal_error.set("Give the share a name.".to_string());
            return;
        }
        store.share_modal_error.set(String::new());
        store.share_modal_pending.set(Some(CloudOp::Share));
        store.send(BackendCommand::CloudShareNode { store_id, node_id, name });
    };
    let invite_now = move || {
        let Some((store_id, node_id)) = untracked(|| store.share_modal_node.get()) else { return };
        let email = untracked(|| store.share_modal_invite_email.get()).trim().to_string();
        if email.is_empty() {
            store.share_modal_error.set("Type the address to invite.".to_string());
            return;
        }
        let role = match untracked(|| store.share_modal_invite_role.get()).as_str() {
            "reader" => pimble_rpc::MemberRole::Reader,
            "commenter" => pimble_rpc::MemberRole::Commenter,
            _ => pimble_rpc::MemberRole::Editor,
        };
        store.share_modal_error.set(String::new());
        store.share_modal_pending.set(Some(CloudOp::ShareInvite));
        store.send(BackendCommand::CloudShareInvite { store_id, node_id, email, role });
    };
    let remove_member = move |email: String| {
        let Some((store_id, node_id)) = untracked(|| store.share_modal_node.get()) else { return };
        store.share_modal_error.set(String::new());
        store.share_modal_pending.set(Some(CloudOp::ShareRemoveMember));
        store.send(BackendCommand::CloudShareRemoveMember { store_id, node_id, email });
    };
    let stop_sharing_now = move || {
        let Some((store_id, node_id)) = untracked(|| store.share_modal_node.get()) else { return };
        store.share_modal_error.set(String::new());
        store.share_modal_pending.set(Some(CloudOp::StopSharing));
        store.send(BackendCommand::CloudStopSharing { store_id, node_id });
    };
    rsx! {
        Modal {
            opened_fn: move || store.share_modal_node.get().is_some(),
            onclose: move || {
                store.share_modal_node.set(None);
                store.share_modal_error.set(String::new());
                store.share_modal_confirm_stop.set(false);
                store.share_modal_pending.set(None);
            },
            title: "Share",
            size: "md",

            div {
                style: "display: flex; flex-direction: column; gap: 10px;",

                // The store is neither hosted nor shared from this
                // computer: the two ways, and the person picks.
                if store.share_modal_face() == ShareFace::Ways {
                    div {
                        style: "display: flex; flex-direction: column; gap: 14px;",

                        div {
                            class: "pimble-share__note",
                            "This store is not hosted on Pimble Cloud or shared from this computer \
                             yet. Choose how the people you invite will reach it."
                        }

                        // The same weight for both, and neither filled:
                        // there is no default here.
                        div {
                            class: "pimble-share__way",
                            Button {
                                variant: "light",
                                size: "sm",
                                loading: {|| store.share_modal_pending.get() == Some(CloudOp::RelayStore)},
                                disabled: {|| store.share_modal_pending.get().is_some() || !store.share_modal_ways_note().is_empty()},
                                onclick: move || share_from_this_computer(store),
                                {SHARE_FROM_HERE_LABEL}
                            }
                            div { class: "pimble-share__note", {SHARE_FROM_HERE_SENTENCE} }
                        }

                        div {
                            class: "pimble-share__way",
                            Button {
                                variant: "light",
                                size: "sm",
                                disabled: {|| store.share_modal_pending.get().is_some() || !store.share_modal_ways_note().is_empty()},
                                onclick: move || host_to_share(store),
                                {HOST_ON_CLOUD_LABEL}
                            }
                            div { class: "pimble-share__note", {HOST_ON_CLOUD_SENTENCE} }
                        }

                        // A store neither way is open to says why, once.
                        if !store.share_modal_ways_note().is_empty() {
                            div {
                                class: "pimble-share__note",
                                {|| store.share_modal_ways_note()}
                            }
                        }
                    }
                }

                // The node is not shared yet: name it and share it.
                if store.share_modal_face() == ShareFace::Name {
                    div {
                        style: "display: flex; flex-direction: column; gap: 10px;",

                        TextInput {
                            label: "Name",
                            placeholder: "Recipes",
                            value_fn: move || store.share_modal_name.get(),
                            oninput: move |val: String| store.share_modal_name.set(val),
                            onsubmit: share_node_now,
                        }

                        div {
                            class: "pimble-share__note",
                            "This name is the share's own: it is what the people you invite \
                             see, and the name of your store is not. Pimble Cloud sees it \
                             too. The notes themselves stay encrypted."
                        }

                        div {
                            class: "pimble-share__note",
                            "Everyone you invite to edit gets this folder and everything in \
                             it, and edits all of it: the writing, the titles, and where \
                             things sit. Their changes and yours are the same notes."
                        }

                        div {
                            style: "display: flex; justify-content: flex-end;",
                            Button {
                                variant: "filled",
                                size: "sm",
                                loading: {|| store.share_modal_pending.get() == Some(CloudOp::Share)},
                                disabled: {|| store.share_modal_pending.get().is_some()},
                                onclick: share_node_now,
                                "Share"
                            }
                        }
                    }
                }

                // The node is shared and this person manages the share:
                // who has it, and how to stop.
                if store.share_modal_face() == ShareFace::Owner {
                    div {
                        style: "display: flex; flex-direction: column; gap: 10px;",

                        div {
                            style: "font-weight: 600;",
                            {|| store.share_modal_name.get()}
                        }
                        div {
                            class: "pimble-share__note",
                            {|| store.share_modal_state.get()}
                        }

                        // The confirmation takes the modal over, so that
                        // "Stop sharing" is never one stray click.
                        if store.share_modal_confirm_stop.get() {
                            div {
                                style: "display: flex; flex-direction: column; gap: 10px;",
                                // Stopping a share ends the grant. It
                                // deletes nothing: the notes are the
                                // owner's own documents and stay where
                                // they are (docs/NODE_DOCUMENT_CONTRACT.md
                                // section 5).
                                div {
                                    {|| format!(
                                        "Stop sharing \"{}\"? The people on it lose access and get nothing \
                                         new from it, and what they have already synced stays on their own \
                                         devices. The notes stay where they are, in your store and hosted \
                                         as before.",
                                        store.share_modal_name.get(),
                                    )}
                                }
                                div {
                                    style: "display: flex; justify-content: flex-end; gap: 8px;",
                                    Button {
                                        variant: "light",
                                        size: "sm",
                                        onclick: move || store.share_modal_confirm_stop.set(false),
                                        "Cancel"
                                    }
                                    Button {
                                        variant: "filled",
                                        color: "red",
                                        size: "sm",
                                        loading: {|| store.share_modal_pending.get() == Some(CloudOp::StopSharing)},
                                        disabled: {|| store.share_modal_pending.get().is_some()},
                                        onclick: stop_sharing_now,
                                        "Stop sharing"
                                    }
                                }
                            }
                        }

                        if !store.share_modal_confirm_stop.get() {
                            div {
                                style: "display: flex; flex-direction: column; gap: 10px;",

                                div {
                                    class: "pimble-share__members",
                                    // An empty bordered box would read as
                                    // a list that failed to load; the line
                                    // below says it plainly instead.
                                    style: {
                                        move || if store.share_modal_members.with(|m| m.is_empty()) {
                                            "display: none;"
                                        } else {
                                            ""
                                        }
                                    },
                                    for member in store.share_modal_members.get() {
                                        div {
                                            key: member.email.clone(),
                                            class: "pimble-share__member",
                                            span {
                                                class: "pimble-share__member-email",
                                                {crate::state::member_label(&member, true)}
                                            }
                                            span {
                                                class: "pimble-share__member-status",
                                                {format!("{}, {}", member_role_text(member.role), member_status_text(member.status, true))}
                                            }
                                            // The owner is the account this
                                            // Pimble is signed in as; there
                                            // is no share without them.
                                            if member.role != pimble_rpc::MemberRole::Owner {
                                                div {
                                                    title: "Remove from this share",
                                                    style: "display: inline-flex;",
                                                    ActionIcon {
                                                        icon: TablerIcon::X,
                                                        variant: "subtle",
                                                        size: "xs",
                                                        onclick: {
                                                            let email = member.email.clone();
                                                            move || remove_member(email.clone())
                                                        },
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }

                                div {
                                    style: {
                                        move || if store.share_modal_members.with(|m| m.is_empty()) {
                                            "font-size: 12px; color: var(--rinch-color-dimmed);"
                                        } else {
                                            "display: none;"
                                        }
                                    },
                                    "Nobody else yet."
                                }

                                div {
                                    class: "pimble-share__invite",
                                    div {
                                        style: "flex: 1;",
                                        TextInput {
                                            label: "Invite",
                                            placeholder: "them@example.com",
                                            value_fn: move || store.share_modal_invite_email.get(),
                                            oninput: move |val: String| store.share_modal_invite_email.set(val),
                                            onsubmit: invite_now,
                                        }
                                    }
                                    Select {
                                        value_fn: move || store.share_modal_invite_role.get(),
                                        onchange: move |val: String| store.share_modal_invite_role.set(val),
                                        data: {|| vec![
                                            SelectOption::new("editor", "Editor"),
                                            // docs/DISCUSSIONS_CONTRACT.md section 7.
                                            SelectOption::new("commenter", "Commenter"),
                                            SelectOption::new("reader", "Reader"),
                                        ]},
                                    }
                                    Button {
                                        variant: "filled",
                                        size: "sm",
                                        loading: {|| store.share_modal_pending.get() == Some(CloudOp::ShareInvite)},
                                        disabled: {|| store.share_modal_pending.get().is_some()},
                                        onclick: invite_now,
                                        "Invite"
                                    }
                                }

                                div {
                                    class: "pimble-share__note",
                                    "Someone who can edit changes everything in this folder: the writing, \
                                     the titles, and where things sit. Someone who can read changes nothing."
                                }

                                div {
                                    class: "pimble-share__note",
                                    {|| store.share_modal_keeping_note()}
                                }

                                div {
                                    style: "display: flex; justify-content: flex-end;",
                                    Button {
                                        variant: "light",
                                        color: "red",
                                        size: "sm",
                                        disabled: {|| store.share_modal_pending.get().is_some()},
                                        onclick: move || store.share_modal_confirm_stop.set(true),
                                        "Stop sharing"
                                    }
                                }
                            }
                        }
                    }
                }

                // The node is shared and the share is someone else's (a
                // member looking at the share they are in): whose it is
                // and who is on it, as the accounts service answers a
                // member. Inviting, removing and stopping are an owner's
                // and the server refuses them from anyone else, so none
                // of them is offered, and the owner's state line (what
                // the owner's devices are handing over) is not this
                // device's to report. The list is a courtesy: when it
                // cannot be loaded the sentence stands alone, with no
                // error line (`events.rs`, `CloudOp::ShareInfo`).
                if store.share_modal_face() == ShareFace::Member {
                    div {
                        style: "display: flex; flex-direction: column; gap: 10px;",

                        div {
                            style: "font-weight: 600;",
                            {|| store.share_modal_name.get()}
                        }
                        div {
                            class: "pimble-share__note",
                            {|| store.share_modal_shared_by_sentence()}
                        }
                        div {
                            class: "pimble-share__members",
                            style: {
                                move || if store.share_modal_members.with(|m| m.is_empty()) {
                                    "display: none;"
                                } else {
                                    ""
                                }
                            },
                            for member in store.share_modal_members.get() {
                                div {
                                    key: format!("{}|{}", member.name, member.email),
                                    class: "pimble-share__member",
                                    span {
                                        class: "pimble-share__member-email",
                                        {crate::state::member_label(&member, false)}
                                    }
                                    span {
                                        class: "pimble-share__member-status",
                                        {format!("{}, {}", member_role_text(member.role), member_status_text(member.status, false))}
                                    }
                                }
                            }
                        }
                    }
                }

                if !store.share_modal_error.get().is_empty() {
                    div {
                        style: "color: var(--rinch-color-red-6); font-size: 12px;",
                        {|| store.share_modal_error.get()}
                    }
                }
            }
        }
    }
}
