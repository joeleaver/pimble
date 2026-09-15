//! `/app/login` — sign in, or unlock after a reload.
//!
//! The page has two modes and picks between them by asking who the session
//! cookie belongs to:
//!
//! * **Sign in** — no session. `GET /kdf?email=` for the salt, derive, then
//!   `POST /login { email, auth_key }`. The password never leaves the page.
//! * **Unlock** — the session is still good but the account keys went with the
//!   last reload (they only ever live in memory). One request for the wrapped
//!   blob, then the password either opens it here or it does not: a wrong
//!   password costs nothing and tells the server nothing.
//!
//! The verification banners live here now, because the accounts service
//! redirects verification outcomes to this path.

use rinch::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::accounts;
use crate::pages::PAGE_CSS;
use crate::route::{self, Route};
use crate::session::{self, SignInError};
use crate::util::{query_param, yield_to_browser};

/// What the URL said about a just-followed verification link.
fn banner() -> String {
    if query_param("verified").as_deref() == Some("1") {
        return "Your email is verified. Sign in to get started.".to_string();
    }
    match query_param("verify_error").as_deref() {
        Some("expired") => {
            "That verification link has expired. Sign in and we will send a new one.".to_string()
        }
        Some("invalid") => "That verification link is not valid.".to_string(),
        _ => String::new(),
    }
}

#[component]
pub fn login_page() -> NodeHandle {
    let email = Signal::new(String::new());
    let password = Signal::new(String::new());
    let error = Signal::new(String::new());
    let notice = Signal::new(banner());
    let busy = Signal::new(false);
    // True once we know the session cookie is still good: the page then asks
    // only for the password, and answers it without a request.
    let unlocking = Signal::new(false);
    let show_resend = Signal::new(false);

    // Who is this? A live session means "unlock", not "sign in". Failing means
    // no session, which is the ordinary case and not an error to show.
    spawn_local(async move {
        if let Ok(who) = accounts::me().await {
            email.set(who.email);
            unlocking.set(true);
        }
    });

    let submit = move || {
        if untracked(|| busy.get()) {
            return;
        }
        let address = untracked(|| email.get()).trim().to_string();
        let secret = untracked(|| password.get());
        let unlock_mode = untracked(|| unlocking.get());

        if !unlock_mode && (address.is_empty() || !address.contains('@')) {
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

            let outcome = if unlock_mode {
                session::unlock(&secret).await
            } else {
                session::sign_in(&address, &secret).await
            };

            match outcome {
                Ok(()) => {
                    password.set(String::new());
                    route::go(Route::App);
                    return;
                }
                Err(SignInError::Unverified) => {
                    show_resend.set(true);
                    error.set(SignInError::Unverified.to_string());
                }
                Err(e) => error.set(e.to_string()),
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

                p { class: "pimble-page__lede",
                    {|| if unlocking.get() {
                        "Your keys are only ever held in memory, so this page asks for your \
                         password again after a reload."
                    } else {
                        "Sign in to reach your stores."
                    }}
                }

                if !notice.get().is_empty() {
                    Alert { color: "blue", variant: "light", {|| notice.get()} }
                }

                // In unlock mode the address is settled by the session; showing
                // it read-only makes clear whose password is being asked for.
                if unlocking.get() {
                    div { class: "pimble-page__lede", "Signed in as " {|| email.get()} }
                } else {
                    TextInput {
                        label: "Email",
                        input_type: "email",
                        placeholder: "you@example.com",
                        value_fn: move || email.get(),
                        oninput: move |value: String| email.set(value),
                    }
                }

                PasswordInput {
                    label: "Password",
                    toggle_visibility: false,
                    value_fn: move || password.get(),
                    oninput: move |value: String| password.set(value),
                }

                if !error.get().is_empty() {
                    div { class: "pimble-page__error", {|| error.get()} }
                }

                if show_resend.get() {
                    Button {
                        variant: "light",
                        full_width: true,
                        onclick: move || {
                            let address = untracked(|| email.get());
                            notice.set("Sending...".to_string());
                            spawn_local(async move {
                                match accounts::resend_verification(&address).await {
                                    Ok(()) => notice.set(
                                        "Sent. Follow the link, then sign in.".to_string(),
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
                    {|| match (busy.get(), unlocking.get()) {
                        (true, _) => "Deriving your keys...",
                        (false, true) => "Unlock",
                        (false, false) => "Sign in",
                    }}
                }

                if unlocking.get() {
                    div {
                        class: "pimble-page__footer",
                        span {
                            class: "pimble-link",
                            onclick: move || {
                                spawn_local(async move {
                                    session::clear();
                                    let _ = accounts::logout().await;
                                    unlocking.set(false);
                                    email.set(String::new());
                                    password.set(String::new());
                                });
                            },
                            "Sign in as someone else"
                        }
                    }
                } else {
                    div {
                        class: "pimble-page__footer",
                        span { "No account yet?" }
                        span {
                            class: "pimble-link",
                            onclick: move || route::go(Route::Signup),
                            "Create one"
                        }
                    }
                }
            }
        }
    }
}
