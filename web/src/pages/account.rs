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

use pimble_crypto::{
    derive_recovery_kek, encode_auth_key, generate_recovery_code, wrap_account_keys, KdfParams,
};
use rinch::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::accounts::{
    self, ChangePasswordRequest, MemberView, NewRecoveryCodeRequest, StoreView,
};
use crate::keys;
use crate::pages::PAGE_CSS;
use crate::route::{self, Route};
use crate::session::{self, derive_timed};
use crate::util::yield_to_browser;

/// The shortest password the accounts service accepts.
const MIN_PASSWORD: usize = 8;

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

/// What a member's row says beside their address: their role, and for an
/// address that has been asked but has no account yet, that it is still an
/// invitation. A plain function for the same reason as [`kind_label`].
fn member_role_words(member: &MemberView) -> String {
    if member.status == "invited" {
        format!("{} · invited", member.role)
    } else {
        member.role.clone()
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

    // "Change password" and "Generate a new recovery code". Both need the
    // account keys in hand: the point of either is to wrap the same keys under
    // something new, and this page is the only place that holds them unwrapped.
    let current_password = Signal::new(String::new());
    let new_password = Signal::new(String::new());
    let new_password_again = Signal::new(String::new());
    let password_busy = Signal::new(false);
    let password_note = Signal::new(String::new());
    let password_error = Signal::new(String::new());

    let fresh_code = Signal::new(String::new());
    let code_saved = Signal::new(false);
    let code_busy = Signal::new(false);
    let code_error = Signal::new(String::new());

    let do_change_password = move || {
        if untracked(|| password_busy.get()) {
            return;
        }
        let current = untracked(|| current_password.get());
        let next = untracked(|| new_password.get());
        let again = untracked(|| new_password_again.get());
        if current.is_empty() {
            password_error.set("Enter your current password.".to_string());
            return;
        }
        if next.chars().count() < MIN_PASSWORD {
            password_error.set(format!("Use at least {MIN_PASSWORD} characters."));
            return;
        }
        if next != again {
            password_error.set("The two new passwords do not match.".to_string());
            return;
        }
        password_error.set(String::new());
        password_note.set(String::new());
        password_busy.set(true);
        spawn_local(async move {
            yield_to_browser().await;
            match change_password(&current, &next).await {
                Ok(()) => {
                    current_password.set(String::new());
                    new_password.set(String::new());
                    new_password_again.set(String::new());
                    password_note.set("Your password is changed.".to_string());
                }
                Err(message) => password_error.set(message),
            }
            password_busy.set(false);
        });
    };

    let do_new_recovery_code = move || {
        if untracked(|| code_busy.get()) {
            return;
        }
        code_error.set(String::new());
        code_busy.set(true);
        spawn_local(async move {
            yield_to_browser().await;
            match new_recovery_code().await {
                Ok(code) => {
                    code_saved.set(false);
                    fresh_code.set(code);
                }
                Err(message) => code_error.set(message),
            }
            code_busy.set(false);
        });
    };

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

    // This page lists whole-store memberships, so it asks for no scope: a
    // share's members are the Share dialog's business, on the desktop that
    // owns the store. The answer is now `{ members, share_name }`; the name is
    // `null` for a whole-store listing, which is what this always asks for.
    let load_members = move |store_id: String| {
        members.set(Vec::new());
        members_error.set(String::new());
        spawn_local(async move {
            match accounts::list_members(&store_id, None).await {
                Ok(listing) => members.set(listing.members),
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
                                        // The address, not the user id: an
                                        // invited address has no account yet
                                        // and so no id, and one address is one
                                        // membership of one scope.
                                        key: member.email.clone(),
                                        class: "pimble-member",
                                        span { {member.email.clone()} }
                                        span {
                                            style: "display: flex; gap: 8px; align-items: center;",
                                            span { class: "pimble-member__role", {member_role_words(&member)} }
                                            Button {
                                                variant: "subtle",
                                                size: "xs",
                                                color: "red",
                                                // An invitation has no account
                                                // to remove; withdrawing one
                                                // is its own endpoint, and the
                                                // owner's Share dialog's to
                                                // offer.
                                                disabled: {
                                                    let invited = member.user_id.is_none();
                                                    move || invited
                                                },
                                                onclick: {
                                                    let store_id = store.store_id.clone();
                                                    let user_id = member.user_id.clone().unwrap_or_default();
                                                    move || {
                                                        let store_id = store_id.clone();
                                                        let user_id = user_id.clone();
                                                        if user_id.is_empty() {
                                                            return;
                                                        }
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

                Divider { size: "sm" }

                h2 { style: "margin: 0; font-size: 16px;", "Change password" }

                div {
                    class: "pimble-row",
                    PasswordInput {
                        label: "Current password",
                        size: "sm",
                        toggle_visibility: false,
                        value_fn: move || current_password.get(),
                        oninput: move |value: String| current_password.set(value),
                    }
                    PasswordInput {
                        label: "New password",
                        size: "sm",
                        toggle_visibility: false,
                        value_fn: move || new_password.get(),
                        oninput: move |value: String| new_password.set(value),
                    }
                    PasswordInput {
                        label: "New password again",
                        size: "sm",
                        toggle_visibility: false,
                        value_fn: move || new_password_again.get(),
                        oninput: move |value: String| new_password_again.set(value),
                    }
                    div {
                        class: "pimble-row__fixed",
                        Button {
                            variant: "filled",
                            size: "sm",
                            loading: {|| password_busy.get()},
                            disabled: {|| password_busy.get()},
                            onclick: do_change_password,
                            "Change password"
                        }
                    }
                }

                if !password_error.get().is_empty() {
                    div { class: "pimble-page__error", {|| password_error.get()} }
                }
                if !password_note.get().is_empty() {
                    div { class: "pimble-page__lede", {|| password_note.get()} }
                }

                Divider { size: "sm" }

                h2 { style: "margin: 0; font-size: 16px;", "Recovery code" }

                if fresh_code.get().is_empty() {
                    div {
                        style: "display: flex; flex-direction: column; gap: 10px;",
                        p { class: "pimble-page__lede",
                            "A new code replaces the old one, which stops working. Your \
                             password is unchanged."
                        }
                        Button {
                            variant: "light",
                            size: "sm",
                            loading: {|| code_busy.get()},
                            disabled: {|| code_busy.get()},
                            onclick: do_new_recovery_code,
                            "Generate a new recovery code"
                        }
                    }
                } else {
                    div {
                        style: "display: flex; flex-direction: column; gap: 10px;",
                        p { class: "pimble-page__lede",
                            "Shown once. The old code no longer works."
                        }
                        div { class: "pimble-recovery", {|| fresh_code.get()} }
                        Checkbox {
                            label: "I have written this down somewhere safe",
                            checked_fn: move || code_saved.get(),
                            onchange: move || code_saved.update(|v| *v = !*v),
                        }
                        Button {
                            variant: "filled",
                            size: "sm",
                            disabled: {|| !code_saved.get()},
                            onclick: move || fresh_code.set(String::new()),
                            "Done"
                        }
                    }
                }

                if !code_error.get().is_empty() {
                    div { class: "pimble-page__error", {|| code_error.get()} }
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
        let keyring = keys::fetch_keyring(store_id, None).await.map_err(|e| e.message())?;
        let (key_id, key) = keyring
            .current
            .zip(keyring.current_key())
            .ok_or("This store's key is not available on this device.")?;
        keys::grant_store_key(store_id, key_id, key, &them.id, &them.public_keys).await?;
    }

    accounts::put_member(store_id, email, role).await.map_err(|e| e.message)?;
    Ok(())
}

/// Wrap the account keys under a new password and tell the service.
///
/// The keys themselves do not change, so nothing that was shared stops being
/// readable; only what opens them does. The current password is proved the way
/// login proves one, with its `auth_key`, and the service checks that before
/// accepting the new material.
async fn change_password(current: &str, next: &str) -> Result<(), String> {
    let keys = session::keys().ok_or("Unlock your account first.")?;

    // The current password's `auth_key` is derived under the *current*
    // parameters, which are the ones the service still has.
    let my_keys = accounts::my_keys().await.map_err(|e| e.message)?;
    let current_derived = derive_timed(current, &my_keys.kdf).map_err(|e| e.to_string())?;

    let kdf = KdfParams::generate();
    let derived = derive_timed(next, &kdf).map_err(|e| e.to_string())?;
    let account_key_blob =
        wrap_account_keys(&keys, &derived.kek).map_err(|e| format!("Wrapping failed: {e}"))?;

    accounts::change_password(&ChangePasswordRequest {
        current_auth_key: encode_auth_key(&current_derived.auth_key),
        auth_key: encode_auth_key(&derived.auth_key),
        kdf,
        account_key_blob,
    })
    .await
    .map_err(|e| match e.status {
        401 => "That is not your current password.".to_string(),
        _ => e.message,
    })
}

/// Mint a recovery code, wrap the account keys under it, and replace the old
/// one. Returns the code, to show once.
async fn new_recovery_code() -> Result<String, String> {
    let keys = session::keys().ok_or("Unlock your account first.")?;

    let code = generate_recovery_code();
    let params = KdfParams::generate();
    let kek = derive_recovery_kek(&code, &params)
        .map_err(|e| format!("Deriving from the new code failed: {e}"))?;
    let recovery_key_blob =
        wrap_account_keys(&keys, &kek).map_err(|e| format!("Wrapping failed: {e}"))?;

    accounts::replace_recovery_code(&NewRecoveryCodeRequest {
        recovery_salt: params.salt.clone(),
        recovery_key_blob,
    })
    .await
    .map_err(|e| e.message)?;

    Ok(code)
}
