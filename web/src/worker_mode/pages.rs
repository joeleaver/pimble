//! Signing in and the account, with the browser's own server
//! (docs/ONE_CODEBASE_CONTRACT.md sections 4 and 7): the password goes to the
//! server in this browser's worker, as the desktop's Account modal sends it to
//! its embedded server (`cloudSignIn`), and no key enters this page.
//!
//! The sign-in page offers "Don't remember this browser" and says, beside it,
//! what remembering protects and what it does not (section 7, in its words).
//! It shows how a followed verification link went (the accounts service
//! redirects every outcome here), and offers to send the link again when the
//! server refuses a sign-in because the address is not verified yet.
//! The account page signs out or forgets this browser; each deletes the
//! device key first, then everything this browser kept. Its other sections
//! (the members of the stores this account owns, the password, the recovery
//! code) are `account_sections`.

use rinch::prelude::*;
use wasm_bindgen_futures::spawn_local;

use super::account_sections::{change_password_section, recovery_code_section, store_members_section};
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

/// What the URL said about a just-followed verification link or a password
/// just set: the values of `verified`, `recovered` and `verify_error`.
fn banner_for(verified: Option<&str>, recovered: Option<&str>, verify_error: Option<&str>) -> &'static str {
    if verified == Some("1") {
        return "Your email is verified. Sign in to get started.";
    }
    if recovered == Some("1") {
        return "Your password is set. Sign in.";
    }
    match verify_error {
        Some("expired") => "That verification link has expired. Sign in and we will send a new one.",
        Some("invalid") => "That verification link is not valid.",
        _ => "",
    }
}

fn banner() -> String {
    banner_for(query_param("verified").as_deref(), query_param("recovered").as_deref(), query_param("verify_error").as_deref()).to_string()
}

/// Why a sign-in did not happen: the sentence to show, and whether the
/// address is not verified yet (the page then offers the link again).
struct SignInFailure {
    sentence: String,
    unverified: bool,
}

impl From<String> for SignInFailure {
    fn from(sentence: String) -> Self {
        Self { sentence, unverified: false }
    }
}

impl From<pimble_client::ClientError> for SignInFailure {
    fn from(e: pimble_client::ClientError) -> Self {
        Self { unverified: e.is_email_unverified(), sentence: e.to_string() }
    }
}

/// Sign in through this browser's server. The keys stay in its worker.
async fn sign_in(email: &str, password: &str, remember: bool) -> Result<(), SignInFailure> {
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
    client.cloud_sign_in(origin(), email, password).await?;
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
    // True once a sign-in was refused because the address is not verified.
    let show_resend = Signal::new(false);

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
        show_resend.set(false);
        busy.set(true);
        spawn_local(async move {
            yield_to_browser().await;
            match sign_in(&address, &secret, remember).await {
                Ok(()) => {
                    password.set(String::new());
                    route::go(Route::App);
                    return;
                }
                Err(failure) => {
                    show_resend.set(failure.unverified);
                    error.set(failure.sentence);
                }
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
                if show_resend.get() {
                    Button {
                        variant: "light",
                        full_width: true,
                        onclick: move || {
                            let address = untracked(|| email.get()).trim().to_string();
                            notice.set("Sending...".to_string());
                            spawn_local(async move {
                                match crate::accounts::resend_verification(&address).await {
                                    // The service answers 202 even when it drops a resend
                                    // inside its one-minute limit, so say only what is true.
                                    Ok(()) => notice.set(
                                        "If the link can be sent, it is on its way. Follow it, then sign in."
                                            .to_string(),
                                    ),
                                    Err(e) => notice.set(e.message),
                                }
                            });
                        },
                        "Send the verification link again"
                    }
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
    // This browser's pushes end with it (wave 2): the subscription here
    // first; signing out, the server then takes it back from the accounts
    // service while its session still works, and a browser forgotten while
    // signed in is dropped by the service at its first push (the push service
    // answers 410).
    let _ = pimble_app::inbox::browser_push::end().await;
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
            class: "pimble-page pimble-page--wide",
            style { {PAGE_CSS} }
            div {
                class: "pimble-page__card pimble-page__card--wide",
                h1 { class: "pimble-page__brand", "Account" }
                p { class: "pimble-page__lede", "Signed in as " {|| email.get()} }
                p { class: "pimble-page__lede",
                    {|| if mode.get() == "remembered" {
                        "This browser remembers you: your stores are kept here, encrypted, and open without a password or a network."
                    } else {
                        "This browser keeps your stores in memory only: a reload asks for your password."
                    }}
                }
                // docs/DISCUSSIONS_CONTRACT.md section 8: one switch, in the
                // app, which this browser's server answers (`cloudPersonalSync`).
                p { class: "pimble-page__lede",
                    "Whether what you've read and your place in each note follow you to your other devices is \"Keep what I've read in step\", in Pimble's Settings > Account."
                }
                Divider { size: "sm" }
                {store_members_section(__scope)}
                Divider { size: "sm" }
                {change_password_section(__scope)}
                Divider { size: "sm" }
                {recovery_code_section(__scope)}
                Divider { size: "sm" }
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

#[cfg(test)]
mod tests {
    use super::banner_for;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// The banners the accounts service's redirects ask for, in the old
    /// sign-in page's words.
    #[wasm_bindgen_test]
    fn each_redirect_reads_as_its_banner() {
        assert_eq!(banner_for(Some("1"), None, None), "Your email is verified. Sign in to get started.");
        assert_eq!(banner_for(None, Some("1"), None), "Your password is set. Sign in.");
        assert_eq!(banner_for(None, None, Some("expired")), "That verification link has expired. Sign in and we will send a new one.");
        assert_eq!(banner_for(None, None, Some("invalid")), "That verification link is not valid.");
        assert_eq!(banner_for(None, None, Some("other")), "");
        assert_eq!(banner_for(Some("0"), Some("0"), None), "");
        assert_eq!(banner_for(None, None, None), "");
        // A verified link wins over anything else the URL says.
        assert_eq!(banner_for(Some("1"), None, Some("invalid")), "Your email is verified. Sign in to get started.");
    }
}
