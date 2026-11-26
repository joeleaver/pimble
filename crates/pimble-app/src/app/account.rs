//! The account: the sidebar's account hooks, the Account modal's state,
//! and Settings > Account.

use std::cell::RefCell;
use std::rc::Rc;

use rinch::prelude::*;

#[cfg(feature = "native")]
use crate::protocol::BackendCommand;
use crate::state::AppStore;

thread_local! {
    /// What the sidebar's account button does, when there is one.
    ///
    /// The browser build has account pages of its own (`/app/account`) and has
    /// to be able to reach them without a page load, because a reload throws
    /// away the keys that make an encrypted store readable. It registers the
    /// move here; the desktop registers nothing, and the button is not drawn.
    /// A hook rather than a `cfg` so the shared UI stays free of target
    /// spelling, and rather than a `BackendCommand` because nothing about it
    /// involves the backend.
    static ACCOUNT_ACTION: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
    /// What "Sign Out" does, for a build that has an account to sign out of.
    /// Registered by the browser entry point, like [`ACCOUNT_ACTION`].
    static SIGN_OUT_ACTION: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
}

/// Give the sidebar an account button and say what it does. Called before
/// [`build_view`]; calling it again replaces the action.
pub fn set_account_action(action: impl Fn() + 'static) {
    ACCOUNT_ACTION.with(|slot| *slot.borrow_mut() = Some(Rc::new(action)));
}

/// Whether a build registered one. Read once, at render time: the browser
/// registers before the view is built and the desktop never does, so this never
/// changes while a view is on screen.
pub(super) fn has_account_action() -> bool {
    ACCOUNT_ACTION.with(|slot| slot.borrow().is_some())
}

pub fn run_account_action() {
    // Cloned out of the slot before it runs: the action navigates, which
    // unmounts this view, and a borrow held across that would be live while
    // the thing that owns it goes away.
    let action = ACCOUNT_ACTION.with(|slot| slot.borrow().clone());
    if let Some(action) = action {
        action();
    }
}

/// Give "Sign Out" something to do. Called before [`build_view`].
pub fn set_sign_out_action(action: impl Fn() + 'static) {
    SIGN_OUT_ACTION.with(|slot| *slot.borrow_mut() = Some(Rc::new(action)));
}

pub fn run_sign_out_action() {
    let action = SIGN_OUT_ACTION.with(|slot| slot.borrow().clone());
    if let Some(action) = action {
        action();
    }
}

/// Open the "Account..." modal (docs/DESKTOP_ACCOUNT_CONTRACT.md decision 1):
/// the sign-in form while nothing is signed in, the signed-in view otherwise.
/// `hint` is why it opened when it did not open from the menu ("Sign in to
/// host a store"), or empty. The URL field starts at `https://pimble.app` and
/// then keeps whatever was last typed in this run.
pub fn open_account_modal(store: AppStore, hint: &str) {
    crate::settings::open_at(store, crate::settings::Section::Account);
    store.account_modal_hint.set(hint.to_string());
}

/// The Account page is shown (Settings, docs/SETTINGS_CONTRACT.md section 2):
/// it starts clean, with no hint unless whoever opened it gives one.
pub(crate) fn account_shown(store: AppStore) {
    store.account_modal_error.set(String::new());
    store.account_modal_busy.set(false);
    store.account_modal_password.set(String::new());
    store.account_modal_password_visible.set(false);
    store.account_modal_hint.set(String::new());
    if untracked(|| store.account_modal_url.get()).is_empty() {
        store.account_modal_url.set("https://pimble.app".to_string());
    }
}

/// Settings closed: a typed password does not outlive it.
pub(crate) fn account_hidden(store: AppStore) {
    store.account_modal_error.set(String::new());
    store.account_modal_password.set(String::new());
    store.account_modal_hint.set(String::new());
}

/// Whether the Account page is showing.
pub(crate) fn account_showing(store: AppStore) -> bool {
    crate::settings::showing(store, crate::settings::Section::Account)
}

#[cfg(not(feature = "native"))]
pub(super) fn render_account_page(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    // Settings > Account (docs/SETTINGS_CONTRACT.md section 2): the
    // sign-in form, or who is signed in. The browser signs in through its
    // own pages, so there it is a way to them.
    rsx! {
        div {
            class: "pimble-settings__section",
            style: "display: flex; flex-direction: column; gap: 10px; max-width: 420px;",
            div { {|| format!("Signed in as {}", store.cloud_email.get())} }
            Checkbox {
                label: "Keep what I've read in step",
                checked_fn: move || store.personal_sync.get(),
                onchange: move || crate::personal::set_sync_enabled(store, !untracked(|| store.personal_sync.get())),
            }
            div {
                style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                "What you have read in discussions, and where you were in each note, sealed so only your devices can open it."
            }
            {crate::notification_settings::render(__scope, store, "Notify this browser when it is closed")}
            div {
                style: "display: flex; gap: 8px;",
                Button { variant: "default", size: "xs", onclick: run_account_action, "Your Account Page" }
                Button { variant: "light", size: "xs", onclick: run_sign_out_action, "Sign Out" }
            }
        }
    }
}

/// Pimble Cloud refused this computer's session: the Account page offers to
/// sign in again, the status bar says so.
#[cfg(feature = "native")]
fn sign_in_expired(store: AppStore) -> bool {
    store.cloud_connection.with(|c| c.as_ref().is_some_and(|c| c.session_expired))
}

#[cfg(feature = "native")]
pub(super) fn render_account_page(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    // ── "Account..." modal ──────────────────────────────────────────
    // docs/DESKTOP_ACCOUNT_CONTRACT.md decision 1: one modal, two faces.
    // The sign-in form while nothing is signed in; the account and a
    // "Sign Out" button once something is. Both outcomes arrive as
    // `CloudStatusChanged`, which flips the face and leaves the modal
    // open so the person sees it worked.
    let sign_in = move || {
        // Signing in again after an expiry: what the page shows filled in
        // is the account's own.
        let again = untracked(|| sign_in_expired(store));
        let or_account = |typed: String, account: String| if typed.trim().is_empty() && again { account } else { typed };
        let url = or_account(untracked(|| store.account_modal_url.get()), untracked(|| store.cloud_url.get())).trim().to_string();
        let email = or_account(untracked(|| store.account_modal_email.get()), untracked(|| store.cloud_email.get())).trim().to_string();
        let password = untracked(|| store.account_modal_password.get());
        if url.is_empty() || email.is_empty() || password.is_empty() {
            store.account_modal_error.set("Fill in the service URL, email and password.".to_string());
            return;
        }
        store.account_modal_error.set(String::new());
        store.account_modal_busy.set(true);
        store.send(BackendCommand::CloudSignIn { url, email, password });
    };
    // Settings > Account (docs/SETTINGS_CONTRACT.md section 2): the
    // sign-in form, or who is signed in. The browser signs in through its
    // own pages, so there it is a way to them.
    rsx! {
            div {
                class: "pimble-settings__section",
                style: "display: flex; flex-direction: column; gap: 10px; max-width: 420px;",

                if !store.account_modal_hint.get().is_empty() {
                    div {
                        style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                        {|| store.account_modal_hint.get()}
                    }
                }

                // The sign-in face: signed out, or signed in with a
                // session Pimble Cloud refused (it expired).
                if !store.cloud_signed_in.get() || sign_in_expired(store) {
                    div {
                        style: "display: flex; flex-direction: column; gap: 10px;",

                        if sign_in_expired(store) {
                            div {
                                style: "font-size: 13px;",
                                {|| format!("Your sign-in as {} expired. Sign in again to sync your hosted and shared stores; your notes are all here, and edits made meanwhile go once you do.", store.cloud_email.get())}
                            }
                        }

                        TextInput {
                            label: "Service URL",
                            placeholder: "https://pimble.app",
                            value_fn: move || store.account_modal_url.get(),
                            oninput: move |val: String| store.account_modal_url.set(val),
                        }

                        TextInput {
                            label: "Email",
                            placeholder: "you@example.com",
                            value_fn: move || {
                                let typed = store.account_modal_email.get();
                                if typed.is_empty() && sign_in_expired(store) { store.cloud_email.get() } else { typed }
                            },
                            oninput: move |val: String| store.account_modal_email.set(val),
                        }

                        PasswordInput {
                            label: "Password",
                            value_fn: move || store.account_modal_password.get(),
                            oninput: move |val: String| store.account_modal_password.set(val),
                            visible_fn: move || store.account_modal_password_visible.get(),
                            ontoggle: move || store.account_modal_password_visible.update(|v| *v = !*v),
                        }

                        Button {
                            variant: "filled",
                            size: "sm",
                            loading: {|| store.account_modal_busy.get()},
                            disabled: {|| store.account_modal_busy.get()},
                            onclick: sign_in,
                            "Sign In"
                        }
                    }
                }

                // The signed-in face.
                if store.cloud_signed_in.get() && !sign_in_expired(store) {
                    div {
                        style: "display: flex; flex-direction: column; gap: 10px;",

                        div {
                            {|| format!("Signed in as {}", store.cloud_email.get())}
                        }
                        div {
                            style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                            {|| store.cloud_url.get()}
                        }

                        // docs/DISCUSSIONS_CONTRACT.md section 8: signing in
                        // is the asking; this turns it off.
                        Checkbox {
                            label: "Keep what I've read in step",
                            checked_fn: move || store.personal_sync.get(),
                            onchange: move || crate::personal::set_sync_enabled(store, !untracked(|| store.personal_sync.get())),
                        }
                        div {
                            style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                            "What you have read in discussions, and where you were in each note, sealed so only your devices can open it."
                        }

                        // docs/NOTIFICATIONS_CONTRACT.md section 6.
                        {crate::notification_settings::render(__scope, store, "Notify my browsers when Pimble is closed")}

                        Button {
                            variant: "light",
                            size: "sm",
                            loading: {|| store.account_modal_busy.get()},
                            disabled: {|| store.account_modal_busy.get()},
                            onclick: move || {
                                store.account_modal_error.set(String::new());
                                store.account_modal_busy.set(true);
                                store.send(BackendCommand::CloudSignOut);
                            },
                            "Sign Out"
                        }
                    }
                }

                if !store.account_modal_error.get().is_empty() {
                    div {
                        style: "color: var(--rinch-color-red-6); font-size: 12px;",
                        {|| store.account_modal_error.get()}
                    }
                }
            }
    }
}
