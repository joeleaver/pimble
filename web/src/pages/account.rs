//! `/app/account` — the stores this account has, who else is in them, and the
//! way out.
//!
//! Creating an encrypted store is two steps that must both happen: the service
//! records the store, and this page mints its key and seals it to the creator.
//! A vault store with no key envelope is a store nobody can ever open, so the
//! failure of the second step is reported as a failure of the whole thing.
//!
//! Adding a member to a vault store is likewise two steps: the grant (which is
//! what the Pimble server checks) and the key envelope (which is what makes the
//! blobs readable). The store key has to be opened here to seal a copy of it,
//! so this only works while the account is unlocked.

use rinch::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::accounts::{self, MemberView, StoreView};
use crate::keys;
use crate::pages::PAGE_CSS;
use crate::route::{self, Route};
use crate::session;

const ROLES: [&str; 3] = ["owner", "editor", "reader"];

/// How a store's kind reads in the list. A plain function rather than an `if`
/// in the markup: `if` inside `rsx!` is reactive control flow, and the `move`
/// closure it builds would take the field with it, leaving the rest of the row
/// nothing to read (Rule 16).
fn kind_label(kind: &str) -> &'static str {
    if kind == "vault" {
        "encrypted"
    } else {
        "plain"
    }
}

#[component]
pub fn account_page() -> NodeHandle {
    let stores: Signal<Vec<StoreView>> = Signal::new(Vec::new());
    let loading = Signal::new(true);
    let error = Signal::new(String::new());

    let new_name = Signal::new(String::new());
    let new_kind = Signal::new("vault".to_string());
    let creating = Signal::new(false);

    // One store's members are shown at a time; `open_store` is which.
    let open_store: Signal<Option<String>> = Signal::new(None);
    let members: Signal<Vec<MemberView>> = Signal::new(Vec::new());
    let members_error = Signal::new(String::new());
    let member_email = Signal::new(String::new());
    let member_role = Signal::new("reader".to_string());
    let member_busy = Signal::new(false);

    let who = Signal::new(session::email().unwrap_or_default());

    let refresh = move || {
        loading.set(true);
        spawn_local(async move {
            match accounts::list_stores().await {
                Ok(list) => {
                    stores.set(list);
                    error.set(String::new());
                }
                Err(e) if e.is_unauthorized() => route::replace_with(Route::Login),
                Err(e) => error.set(e.message),
            }
            loading.set(false);
        });
    };
    refresh();

    // Who the session belongs to, in case this page was reached without the
    // keys ever being unlocked in this tab.
    if untracked(|| who.get()).is_empty() {
        spawn_local(async move {
            if let Ok(user) = accounts::me().await {
                who.set(user.email);
            }
        });
    }

    let load_members = move |store_id: String| {
        members.set(Vec::new());
        members_error.set(String::new());
        spawn_local(async move {
            match accounts::list_members(&store_id).await {
                Ok(list) => members.set(list),
                Err(e) => members_error.set(e.message),
            }
        });
    };

    rsx! {
        div {
            class: "pimble-page pimble-page--wide",
            style { {PAGE_CSS} }

            div {
                class: "pimble-account__bar",
                div {
                    h1 { class: "pimble-page__brand", "Pimble" }
                    div { class: "pimble-account__who", {|| who.get()} }
                }
                div {
                    style: "display: flex; gap: 8px;",
                    Button {
                        variant: "light",
                        size: "sm",
                        onclick: move || route::go(Route::App),
                        "Open Pimble"
                    }
                    Button {
                        variant: "subtle",
                        size: "sm",
                        onclick: move || {
                            spawn_local(async move {
                                // Drop the keys first: whatever the network
                                // does next, this page stops holding them.
                                session::clear();
                                let _ = accounts::logout().await;
                                route::replace_with(Route::Login);
                            });
                        },
                        "Log out"
                    }
                }
            }

            div {
                class: "pimble-page__card pimble-page__card--wide",

                if !error.get().is_empty() {
                    div { class: "pimble-page__error", {|| error.get()} }
                }

                h2 { style: "margin: 0; font-size: 16px;", "Your stores" }

                if loading.get() {
                    Loader { size: "sm" }
                }

                if !loading.get() && stores.get().is_empty() {
                    p { class: "pimble-page__lede", "No stores yet. Make one below." }
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
                                    span { {store.role.clone()} }
                                    span { {kind_label(&store.kind)} }
                                }
                            }
                            Button {
                                variant: "subtle",
                                size: "xs",
                                onclick: {
                                    let id = store.store_id.clone();
                                    move || {
                                        let id = id.clone();
                                        let already = untracked(|| open_store.get()) == Some(id.clone());
                                        if already {
                                            open_store.set(None);
                                        } else {
                                            open_store.set(Some(id.clone()));
                                            load_members(id);
                                        }
                                    }
                                },
                                "Members"
                            }
                        }

                        if open_store.get().as_deref() == Some(store.store_id.as_str()) {
                            div {
                                style: "display: flex; flex-direction: column; gap: 8px;",

                                if !members_error.get().is_empty() {
                                    div { class: "pimble-page__error", {|| members_error.get()} }
                                }

                                for member in members.get() {
                                    div {
                                        key: member.user_id.clone(),
                                        class: "pimble-member",
                                        span { {member.email.clone()} }
                                        span {
                                            style: "display: flex; gap: 8px; align-items: center;",
                                            span { class: "pimble-member__role", {member.role.clone()} }
                                            Button {
                                                variant: "subtle",
                                                size: "xs",
                                                color: "red",
                                                onclick: {
                                                    let store_id = store.store_id.clone();
                                                    let user_id = member.user_id.clone();
                                                    move || {
                                                        let store_id = store_id.clone();
                                                        let user_id = user_id.clone();
                                                        spawn_local(async move {
                                                            match accounts::delete_member(&store_id, &user_id).await {
                                                                Ok(()) => load_members(store_id),
                                                                Err(e) => members_error.set(e.message),
                                                            }
                                                        });
                                                    }
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
                                        value_fn: move || member_email.get(),
                                        oninput: move |value: String| member_email.set(value),
                                    }
                                    Select {
                                        label: "Role",
                                        size: "xs",
                                        value_fn: move || member_role.get(),
                                        onchange: move |value: String| member_role.set(value),
                                        data: ROLES.iter().map(|r| SelectOption::new(*r, *r)).collect::<Vec<_>>(),
                                    }
                                    div {
                                        class: "pimble-row__fixed",
                                        Button {
                                            variant: "light",
                                            size: "xs",
                                            disabled: {|| member_busy.get()},
                                            onclick: {
                                                let store_id = store.store_id.clone();
                                                let is_vault = store.kind == "vault";
                                                move || {
                                                    let store_id = store_id.clone();
                                                    let email = untracked(|| member_email.get()).trim().to_string();
                                                    let role = untracked(|| member_role.get());
                                                    if email.is_empty() {
                                                        members_error.set("Enter an email address.".to_string());
                                                        return;
                                                    }
                                                    member_busy.set(true);
                                                    members_error.set(String::new());
                                                    spawn_local(async move {
                                                        match add_member(&store_id, &email, &role, is_vault).await {
                                                            Ok(()) => {
                                                                member_email.set(String::new());
                                                                load_members(store_id);
                                                            }
                                                            Err(message) => members_error.set(message),
                                                        }
                                                        member_busy.set(false);
                                                    });
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

                Divider { size: "sm" }

                h2 { style: "margin: 0; font-size: 16px;", "New store" }

                div {
                    class: "pimble-row",
                    TextInput {
                        label: "Name",
                        placeholder: "Notes",
                        size: "sm",
                        value_fn: move || new_name.get(),
                        oninput: move |value: String| new_name.set(value),
                    }
                    Select {
                        label: "Kind",
                        size: "sm",
                        value_fn: move || new_kind.get(),
                        onchange: move |value: String| new_kind.set(value),
                        data: vec![
                            SelectOption::new("vault", "Encrypted"),
                            SelectOption::new("plain", "Plain"),
                        ],
                    }
                    div {
                        class: "pimble-row__fixed",
                        Button {
                            variant: "filled",
                            size: "sm",
                            loading: {|| creating.get()},
                            disabled: {|| creating.get()},
                            onclick: move || {
                                let name = untracked(|| new_name.get()).trim().to_string();
                                let kind = untracked(|| new_kind.get());
                                if name.is_empty() {
                                    error.set("Give the store a name.".to_string());
                                    return;
                                }
                                creating.set(true);
                                error.set(String::new());
                                spawn_local(async move {
                                    match create_store(&name, &kind).await {
                                        Ok(()) => {
                                            new_name.set(String::new());
                                            refresh();
                                        }
                                        Err(message) => error.set(message),
                                    }
                                    creating.set(false);
                                });
                            },
                            "Create"
                        }
                    }
                }

                p { class: "pimble-page__lede",
                    "An encrypted store's key is made here and sealed to your account. \
                     Pimble Cloud stores the blobs and never sees the key."
                }
            }
        }
    }
}

/// Create the store and, for a vault, the key that makes it usable.
async fn create_store(name: &str, kind: &str) -> Result<(), String> {
    if kind == "vault" && !session::is_unlocked() {
        return Err("Unlock your account before making an encrypted store.".to_string());
    }

    let created = accounts::create_store(name, kind, None).await.map_err(|e| e.message)?;

    if kind == "vault" {
        // A vault store with no envelope can never be opened, so a failure
        // here is a failure of the whole thing and says so plainly.
        keys::mint_store_key(&created.store_id).await.map_err(|e| {
            format!("The store was created but its key could not be stored: {e}")
        })?;
    }
    Ok(())
}

/// Give someone a role, and — for a vault — a copy of the store key.
async fn add_member(store_id: &str, email: &str, role: &str, is_vault: bool) -> Result<(), String> {
    if is_vault {
        // Look them up before granting anything: no account, no key, and a
        // grant with no key would look like access and read as gibberish.
        let them = accounts::lookup_user(email).await.map_err(|e| match e.status {
            404 => "No verified account with that address.".to_string(),
            _ => e.message,
        })?;
        let keyring = keys::fetch_keyring(store_id).await?;
        let key = keyring
            .current_key()
            .ok_or("This store's key is not available on this device.")?;
        keys::grant_store_key(store_id, keyring.current, key, &them.id, &them.public_keys).await?;
    }

    accounts::put_member(store_id, email, role).await.map_err(|e| e.message)?;
    Ok(())
}
