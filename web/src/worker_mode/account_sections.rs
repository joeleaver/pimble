//! The account page's own sections with the browser's server
//! (docs/ONE_CODEBASE_CONTRACT.md, "Built: wave 5, part 3"): changing the
//! password, a new recovery code, and the members of the stores this account
//! owns whole. Every one is a Service-only RPC of the server in this
//! browser's worker, which holds the keys, derives and rewraps them and talks
//! to the accounts service; this page sends what the person typed and shows
//! the sentence that comes back. No key enters the page; the one secret that
//! comes out is the new recovery code, shown once and dropped at "Done".

use pimble_core::StoreId;
use pimble_rpc::{CloudHostedStoreInfo, MemberRole, ShareMember, ShareMemberStatus};
use rinch::prelude::*;
use wasm_bindgen_futures::spawn_local;

use super::backend;
use crate::util::yield_to_browser;

/// The shortest password the server takes (it says so in a sentence too).
const MIN_PASSWORD: usize = 8;

/// The roles a whole store's member may hold, in the order offered.
const ROLES: [MemberRole; 4] = [MemberRole::Owner, MemberRole::Editor, MemberRole::Commenter, MemberRole::Reader];

/// What a member's row says beside their address.
fn status_words(member: &ShareMember) -> &'static str {
    match member.status {
        ShareMemberStatus::Active => "",
        ShareMemberStatus::Invited => " (invited)",
        ShareMemberStatus::WaitingForKey => " (waiting for the key)",
    }
}

async fn client() -> Result<std::sync::Arc<pimble_client::PimbleClient>, String> {
    backend::local().client().await
}

pub fn change_password_section(__scope: &mut RenderScope) -> NodeHandle {
    let current = Signal::new(String::new());
    let next = Signal::new(String::new());
    let again = Signal::new(String::new());
    let error = Signal::new(String::new());
    let note = Signal::new(String::new());
    let busy = Signal::new(false);

    let submit = move || {
        if untracked(|| busy.get()) {
            return;
        }
        let (old, new, repeat) = (untracked(|| current.get()), untracked(|| next.get()), untracked(|| again.get()));
        note.set(String::new());
        if old.is_empty() {
            error.set("Enter your current password.".to_string());
            return;
        }
        if new.chars().count() < MIN_PASSWORD {
            error.set(format!("A new password needs at least {MIN_PASSWORD} characters."));
            return;
        }
        if new != repeat {
            error.set("The two new passwords are not the same.".to_string());
            return;
        }
        error.set(String::new());
        busy.set(true);
        spawn_local(async move {
            yield_to_browser().await;
            let changed = match client().await {
                Ok(client) => client.cloud_change_password(old, new).await.map_err(|e| e.to_string()),
                Err(e) => Err(e),
            };
            match changed {
                Ok(()) => {
                    for field in [current, next, again] {
                        field.set(String::new());
                    }
                    note.set("Your password is changed. Use it the next time you sign in; devices signed in now stay signed in.".to_string());
                }
                Err(e) => error.set(e),
            }
            busy.set(false);
        });
    };

    rsx! {
        div {
            style: "display: flex; flex-direction: column; gap: 10px;",
            h2 { style: "margin: 0; font-size: 16px;", "Change password" }
            PasswordInput {
                label: "Current password",
                size: "sm",
                toggle_visibility: false,
                value_fn: move || current.get(),
                oninput: move |value: String| current.set(value),
            }
            PasswordInput {
                label: "New password",
                size: "sm",
                toggle_visibility: false,
                value_fn: move || next.get(),
                oninput: move |value: String| next.set(value),
            }
            PasswordInput {
                label: "New password again",
                size: "sm",
                toggle_visibility: false,
                value_fn: move || again.get(),
                oninput: move |value: String| again.set(value),
            }
            if !error.get().is_empty() {
                div { class: "pimble-page__error", {|| error.get()} }
            }
            if !note.get().is_empty() {
                Alert { color: "blue", variant: "light", {|| note.get()} }
            }
            Button {
                variant: "light",
                full_width: true,
                loading: {|| busy.get()},
                disabled: {|| busy.get()},
                onclick: submit,
                "Change Password"
            }
        }
    }
}

pub fn recovery_code_section(__scope: &mut RenderScope) -> NodeHandle {
    let code = Signal::new(String::new());
    let saved = Signal::new(false);
    let error = Signal::new(String::new());
    let busy = Signal::new(false);

    let generate = move || {
        if untracked(|| busy.get()) {
            return;
        }
        busy.set(true);
        error.set(String::new());
        spawn_local(async move {
            yield_to_browser().await;
            let made = match client().await {
                Ok(client) => client.cloud_new_recovery_code().await.map_err(|e| e.to_string()),
                Err(e) => Err(e),
            };
            match made {
                Ok(fresh) => {
                    saved.set(false);
                    code.set(fresh);
                }
                Err(e) => error.set(e),
            }
            busy.set(false);
        });
    };

    rsx! {
        div {
            style: "display: flex; flex-direction: column; gap: 10px;",
            h2 { style: "margin: 0; font-size: 16px;", "Recovery code" }
            if code.get().is_empty() {
                p { class: "pimble-page__lede",
                    "A new code replaces the old one, which stops working. Your password is unchanged."
                }
                Button {
                    variant: "light",
                    full_width: true,
                    loading: {|| busy.get()},
                    disabled: {|| busy.get()},
                    onclick: generate,
                    "New Recovery Code"
                }
            } else {
                p { class: "pimble-page__lede", "Shown once. The old code no longer works." }
                div { class: "pimble-recovery", {|| code.get()} }
                Checkbox {
                    label: "I have written this down somewhere safe",
                    checked_fn: move || saved.get(),
                    onchange: move || saved.set(!untracked(|| saved.get())),
                }
                Button {
                    variant: "filled",
                    full_width: true,
                    disabled: {|| !saved.get()},
                    onclick: move || code.set(String::new()),
                    "Done"
                }
            }
            if !error.get().is_empty() {
                div { class: "pimble-page__error", {|| error.get()} }
            }
        }
    }
}

/// A store this account owns whole, as its row on the page shows it.
#[derive(Debug, Clone, PartialEq)]
struct OwnedStore {
    store_id: String,
    name: String,
    encrypted: bool,
}

/// The stores this account owns whole: shares the account was given, and
/// stores it holds in another role, are the Share dialog's and the owner's.
fn owned_whole(stores: Vec<CloudHostedStoreInfo>) -> Vec<OwnedStore> {
    stores
        .into_iter()
        .filter(|store| store.root.is_none() && store.role == "owner")
        .map(|store| OwnedStore { encrypted: store.kind == "vault", store_id: store.store_id, name: store.name })
        .collect()
}

pub fn store_members_section(__scope: &mut RenderScope) -> NodeHandle {
    let stores = Signal::new(Vec::<OwnedStore>::new());
    let open = Signal::new(None::<String>);
    let members = Signal::new(Vec::<ShareMember>::new());
    let error = Signal::new(String::new());
    let email = Signal::new(String::new());
    let role = Signal::new(MemberRole::Editor.as_str().to_string());
    let busy = Signal::new(false);
    // Bumped when a call is refused, so each row's role select shows the
    // role the member still holds, not the one just picked.
    let refused = Signal::new(0u32);

    spawn_local(async move {
        match client().await {
            Ok(client) => match client.cloud_list_hosted_stores().await {
                Ok(listed) => stores.set(owned_whole(listed)),
                Err(e) => error.set(e.to_string()),
            },
            Err(e) => error.set(e),
        }
    });

    // One members call at a time: its answer is the store's list after it.
    let act = move |store_id: String, how: Action| {
        if untracked(|| busy.get()) {
            return;
        }
        let Ok(store) = StoreId::parse(&store_id) else {
            return;
        };
        busy.set(true);
        error.set(String::new());
        spawn_local(async move {
            yield_to_browser().await;
            let answer = match client().await {
                Ok(client) => match how {
                    Action::List => client.cloud_store_members(store).await,
                    Action::Set(address, role) => client.cloud_store_set_member(store, address, role).await,
                    Action::Remove(address) => client.cloud_store_remove_member(store, address).await,
                }
                .map_err(|e| e.to_string()),
                Err(e) => Err(e),
            };
            match answer {
                Ok(listed) => {
                    if untracked(|| open.get()).as_deref() == Some(store_id.as_str()) {
                        members.set(listed.members);
                    }
                    email.set(String::new());
                }
                Err(e) => {
                    error.set(e);
                    refused.update(|n| *n += 1);
                }
            }
            busy.set(false);
        });
    };

    rsx! {
        div {
            style: "display: flex; flex-direction: column; gap: 10px;",
            h2 { style: "margin: 0; font-size: 16px;", "Your stores" }
            if stores.get().is_empty() {
                p { class: "pimble-page__lede", "You own no store on Pimble Cloud yet." }
            }
            if !error.get().is_empty() {
                div { class: "pimble-page__error", {|| error.get()} }
            }
            for store in stores.get() {
                div {
                    key: store.store_id.clone(),
                    class: "pimble-store",
                    div {
                        class: "pimble-store__head",
                        div {
                            div { class: "pimble-store__name", {store.name.clone()} }
                            div {
                                class: "pimble-store__meta",
                                span { {if store.encrypted { "Encrypted" } else { "Plain" }} }
                            }
                        }
                        Button {
                            variant: "subtle",
                            size: "xs",
                            onclick: {
                                let id = store.store_id.clone();
                                move || {
                                    if untracked(|| open.get()).as_deref() == Some(id.as_str()) {
                                        open.set(None);
                                    } else {
                                        members.set(Vec::new());
                                        open.set(Some(id.clone()));
                                        act(id.clone(), Action::List);
                                    }
                                }
                            },
                            "Members"
                        }
                    }
                    if open.get().as_deref() == Some(store.store_id.as_str()) {
                        div {
                            style: "display: flex; flex-direction: column; gap: 8px;",
                            for member in members.get() {
                                div {
                                    key: member.email.clone(),
                                    class: "pimble-member",
                                    span { {format!("{}{}", member.email, status_words(&member))} }
                                    span {
                                        style: "display: flex; gap: 8px; align-items: center;",
                                        Select {
                                            size: "xs",
                                            value_fn: {
                                                let address = member.email.clone();
                                                move || {
                                                    refused.get();
                                                    members.get().iter().find(|m| m.email == address).map(|m| m.role.as_str().to_string()).unwrap_or_default()
                                                }
                                            },
                                            onchange: {
                                                let (id, address) = (store.store_id.clone(), member.email.clone());
                                                move |value: String| {
                                                    if let Some(chosen) = MemberRole::parse(&value) {
                                                        act(id.clone(), Action::Set(address.clone(), chosen));
                                                    }
                                                }
                                            },
                                            data: ROLES.iter().map(|r| SelectOption::new(r.as_str(), r.as_str())).collect::<Vec<_>>(),
                                        }
                                        Button {
                                            variant: "subtle",
                                            size: "xs",
                                            color: "red",
                                            disabled: {|| busy.get()},
                                            onclick: {
                                                let (id, address) = (store.store_id.clone(), member.email.clone());
                                                move || act(id.clone(), Action::Remove(address.clone()))
                                            },
                                            "Remove"
                                        }
                                    }
                                }
                            }
                            div {
                                class: "pimble-row",
                                TextInput {
                                    label: "Add by email",
                                    placeholder: "them@example.com",
                                    size: "xs",
                                    value_fn: move || email.get(),
                                    oninput: move |value: String| email.set(value),
                                }
                                Select {
                                    label: "Role",
                                    size: "xs",
                                    value_fn: move || role.get(),
                                    onchange: move |value: String| role.set(value),
                                    data: ROLES.iter().map(|r| SelectOption::new(r.as_str(), r.as_str())).collect::<Vec<_>>(),
                                }
                                div {
                                    class: "pimble-row__fixed",
                                    Button {
                                        variant: "light",
                                        size: "xs",
                                        disabled: {|| busy.get()},
                                        onclick: {
                                            let id = store.store_id.clone();
                                            move || {
                                                let address = untracked(|| email.get()).trim().to_string();
                                                if !address.contains('@') {
                                                    error.set("Enter an email address.".to_string());
                                                    return;
                                                }
                                                let chosen = MemberRole::parse(&untracked(|| role.get())).unwrap_or(MemberRole::Editor);
                                                act(id.clone(), Action::Set(address, chosen));
                                            }
                                        },
                                        "Add"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// What a members call does.
enum Action {
    List,
    Set(String, MemberRole),
    Remove(String),
}
