//! Signing in and the account, with the browser's own server
//! (docs/ONE_CODEBASE_CONTRACT.md sections 4 and 7): the password goes to the
//! server in this browser's worker, as the desktop's Account modal sends it to
//! its embedded server (`cloudSignIn`), and no key enters this page.
//!
//! The sign-in page offers "Don't remember this browser" and says, beside it,
//! what remembering protects and what it does not (section 7, in its words).
//! The account page signs out or forgets this browser; each deletes the
//! device key first, then everything this browser kept.

use rinch::prelude::*;
use wasm_bindgen_futures::spawn_local;

use super::{backend, hub};
use crate::pages::PAGE_CSS;
use crate::route::{self, Route};
use crate::util::{query_param, yield_to_browser};

/// What remembering this browser means, said beside the checkbox.
const REMEMBER_SENTENCE: &str = "Pimble keeps your stores in this browser, encrypted with a key the browser holds, so a \
     reload needs no password and your stores open without a network. That protects a copy of the browser's storage \
     taken on its own, and what is left after you sign out. It does not protect against someone using this browser \
     profile while you are signed in, a copy of the whole profile, or a script running on this site.";

fn origin() -> String {
    web_sys::window().and_then(|w| w.location().origin().ok()).unwrap_or_default()
}

fn banner() -> String {
    if query_param("verified").as_deref() == Some("1") {
        return "Your email is verified. Sign in to get started.".to_string();
    }
    if query_param("recovered").as_deref() == Some("1") {
        return "Your password is set. Sign in.".to_string();
    }
    String::new()
}

/// Sign in through this browser's server. The keys stay in its worker.
async fn sign_in(email: &str, password: &str, remember: bool) -> Result<(), String> {
    let local = backend::local();
    let client = local.client().await?;
    let status = client.cloud_status().await.map_err(|e| e.to_string())?;
    // A sign-in as another account first forgets this browser (section 7).
    let another = status.email.as_deref().is_some_and(|held| !held.eq_ignore_ascii_case(email));
    let mode = hub::status().mode.unwrap_or_default();
    if another || (!remember && mode == "remembered") {
        tracing::info!("Forgetting this browser before signing in ({})", if another { "another account" } else { "not to be remembered" });
        hub::forget().await?;
        local.reset().await;
    }
    if remember && hub::status().mode.as_deref() != Some("remembered") {
        hub::keep("remember").await?;
        local.reset().await;
    }
    let client = local.client().await?;
    client.cloud_sign_in(origin(), email, password).await.map_err(|e| e.to_string())?;
    if remember {
        super::ask_to_persist();
    }
    Ok(())
}

#[component]
pub fn login_page() -> NodeHandle {
    let email = Signal::new(String::new());
    let password = Signal::new(String::new());
    let dont_remember = Signal::new(false);
    let error = Signal::new(String::new());
    let notice = Signal::new(banner());
    let busy = Signal::new(false);

    // The account this browser's server last held (a session that ran out):
    // the address is offered again.
    spawn_local(async move {
        if let Ok(client) = backend::local().client().await {
            if let Ok(status) = client.cloud_status().await {
                if let Some(held) = status.email {
                    email.set(held);
                }
            }
        }
    });

    let submit = move || {
        if untracked(|| busy.get()) {
            return;
        }
        let address = untracked(|| email.get()).trim().to_string();
        let secret = untracked(|| password.get());
        let remember = !untracked(|| dont_remember.get());
        if address.is_empty() || !address.contains('@') {
            error.set("Enter your email address.".to_string());
            return;
        }
        if secret.is_empty() {
            error.set("Enter your password.".to_string());
            return;
        }
        error.set(String::new());
        busy.set(true);
        spawn_local(async move {
            yield_to_browser().await;
            match sign_in(&address, &secret, remember).await {
                Ok(()) => {
                    password.set(String::new());
                    route::go(Route::App);
                    return;
                }
                Err(e) => error.set(e),
            }
            busy.set(false);
        });
    };

    rsx! {
        div {
            class: "pimble-page",
            style { {PAGE_CSS} }
            div {
                class: "pimble-page__card",
                h1 { class: "pimble-page__brand", "Pimble" }
                p { class: "pimble-page__lede", "Sign in to reach your stores." }
                if !notice.get().is_empty() {
                    Alert { color: "blue", variant: "light", {|| notice.get()} }
                }
                TextInput {
                    label: "Email",
                    input_type: "email",
                    placeholder: "you@example.com",
                    value_fn: move || email.get(),
                    oninput: move |value: String| email.set(value),
                }
                TextInput {
                    label: "Password",
                    input_type: "password",
                    value_fn: move || password.get(),
                    oninput: move |value: String| password.set(value),
                    onsubmit: submit,
                }
                Checkbox {
                    label: "Don't remember this browser",
                    checked_fn: move || dont_remember.get(),
                    onchange: move || dont_remember.set(!untracked(|| dont_remember.get())),
                }
                p { class: "pimble-page__lede",
                    {|| if dont_remember.get() {
                        "Your stores and keys are kept in this tab's memory only, and this page asks for your password after every reload. For a computer others use."
                    } else {
                        REMEMBER_SENTENCE
                    }}
                }
                if !error.get().is_empty() {
                    div { class: "pimble-page__error", {|| error.get()} }
                }
                Button {
                    variant: "filled",
                    full_width: true,
                    loading: {|| busy.get()},
                    disabled: {|| busy.get()},
                    onclick: submit,
                    {|| if busy.get() { "Signing in..." } else { "Sign in" }}
                }
                div {
                    class: "pimble-page__footer",
                    span { "No account yet?" }
                    span { class: "pimble-link", onclick: move || route::go(Route::Signup), "Create one" }
                    span { class: "pimble-link", onclick: move || route::go(Route::Forgot), "Forgot your password?" }
                }
            }
        }
    }
}

/// Sign out, or forget this browser: either way the device key goes first,
/// then every store and key this browser kept, and the page starts again at
/// the sign-in page.
pub async fn leave(sign_out: bool) -> Result<bool, String> {
    let local = backend::local();
    if let Ok(client) = local.client().await {
        if sign_out {
            if let Err(e) = client.cloud_sign_out().await {
                tracing::warn!("Signing out of this browser's server: {e}");
            }
        }
    }
    let empty = hub::forget().await?;
    if sign_out {
        let _ = crate::accounts::logout().await;
    }
    tracing::info!("This browser was forgotten; its storage is {}", if empty { "empty" } else { "NOT empty" });
    Ok(empty)
}

fn restart_at_login() {
    if let Some(window) = web_sys::window() {
        let _ = window.location().replace(Route::Login.path());
    }
}

#[component]
pub fn account_page() -> NodeHandle {
    let email = Signal::new(String::new());
    let mode = Signal::new(String::new());
    let error = Signal::new(String::new());
    let busy = Signal::new(false);

    spawn_local(async move {
        if let Ok(client) = backend::local().client().await {
            if let Ok(status) = client.cloud_status().await {
                email.set(status.email.unwrap_or_default());
            }
        }
        mode.set(hub::status().mode.unwrap_or_default());
    });

    let run = move |sign_out: bool| {
        if untracked(|| busy.get()) {
            return;
        }
        busy.set(true);
        spawn_local(async move {
            match leave(sign_out).await {
                Ok(_) => restart_at_login(),
                Err(e) => {
                    error.set(e);
                    busy.set(false);
                }
            }
        });
    };

    rsx! {
        div {
            class: "pimble-page",
            style { {PAGE_CSS} }
            div {
                class: "pimble-page__card",
                h1 { class: "pimble-page__brand", "Account" }
                p { class: "pimble-page__lede", "Signed in as " {|| email.get()} }
                p { class: "pimble-page__lede",
                    {|| if mode.get() == "remembered" {
                        "This browser remembers you: your stores are kept here, encrypted, and open without a password or a network."
                    } else {
                        "This browser keeps your stores in memory only: a reload asks for your password."
                    }}
                }
                if !error.get().is_empty() {
                    div { class: "pimble-page__error", {|| error.get()} }
                }
                Button { variant: "filled", full_width: true, onclick: move || route::go(Route::App), "Back to Pimble" }
                Button { variant: "light", full_width: true, disabled: {|| busy.get()}, onclick: move || run(true), "Sign Out" }
                Button { variant: "light", color: "red", full_width: true, disabled: {|| busy.get()}, onclick: move || run(false), "Forget This Browser" }
                p { class: "pimble-page__lede",
                    "Forgetting this browser deletes its key first, then every store and key it kept; you sign in again with your password."
                }
                div {
                    class: "pimble-page__footer",
                    span { class: "pimble-link", onclick: move || route::go(Route::Login), "Sign in again" }
                }
            }
        }
    }
}
