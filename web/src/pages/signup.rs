//! `/app/signup` — make an account, and make its keys.
//!
//! Everything that matters happens in this browser. The password is turned
//! into an `auth_key` (what the server is allowed to see) and a KEK (what it
//! never sees); a fresh X25519/Ed25519 keypair is generated and wrapped twice,
//! once under that KEK and once under a KEK derived from a recovery code shown
//! here exactly once. The server receives only the wrapped forms.
//!
//! After the code is acknowledged the page says to check the inbox: an account
//! is unusable until the address is verified, and verification lands back on
//! `/app/login?verified=1`.

use pimble_crypto::{
    encode_auth_key, generate_recovery_code, wrap_account_keys, AccountKeys, KdfParams,
};
use rinch::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::accounts::{self, SignupRequest};
use crate::pages::PAGE_CSS;
use crate::route::{self, Route};
use crate::session::derive_timed;
use crate::util::yield_to_browser;

/// Where the signup page is in its three-step flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Stage {
    /// Email and password twice.
    #[default]
    Form,
    /// The recovery code, shown once, with an acknowledgement.
    Recovery,
    /// "Check your inbox", with a resend button.
    CheckInbox,
}

/// The shortest password the accounts service accepts. Checked here too so a
/// short one never costs a round trip, let alone a key derivation.
const MIN_PASSWORD: usize = 8;

#[component]
pub fn signup_page() -> NodeHandle {
    let email = Signal::new(String::new());
    let password = Signal::new(String::new());
    let confirm = Signal::new(String::new());
    let error = Signal::new(String::new());
    let busy = Signal::new(false);
    let stage = Signal::new(Stage::Form);
    let recovery_code = Signal::new(String::new());
    let saved = Signal::new(false);
    let resent = Signal::new(String::new());

    // Everything the button does, so Enter in the last field does it too.
    let submit = move || {
        if untracked(|| busy.get()) {
            return;
        }
        let address = untracked(|| email.get()).trim().to_string();
        let secret = untracked(|| password.get());
        let again = untracked(|| confirm.get());

        if address.is_empty() || !address.contains('@') {
            error.set("Enter an email address.".to_string());
            return;
        }
        if secret.chars().count() < MIN_PASSWORD {
            error.set(format!("Use at least {MIN_PASSWORD} characters."));
            return;
        }
        // Checked here, before anything is derived or sent: a mismatch is the
        // page's own business.
        if secret != again {
            error.set("The two passwords do not match.".to_string());
            return;
        }

        error.set(String::new());
        busy.set(true);

        spawn_local(async move {
            // Let the button repaint as busy before Argon2id takes the thread.
            yield_to_browser().await;

            match build_and_send(&address, &secret).await {
                Ok(code) => {
                    recovery_code.set(code);
                    password.set(String::new());
                    confirm.set(String::new());
                    stage.set(Stage::Recovery);
                }
                Err(message) => error.set(message),
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

                match stage.get() {
                    Stage::Form => div {
                        style: "display: flex; flex-direction: column; gap: 14px;",

                        p { class: "pimble-page__lede",
                            "Your notes are encrypted on this device. Pimble Cloud stores \
                             what it cannot read."
                        }

                        TextInput {
                            label: "Email",
                            input_type: "email",
                            placeholder: "you@example.com",
                            value_fn: move || email.get(),
                            oninput: move |value: String| email.set(value),
                        }

                        PasswordInput {
                            label: "Password",
                            description: "At least 8 characters. There is no way to reset it \
                                          apart from the recovery code on the next screen.",
                            toggle_visibility: false,
                            value_fn: move || password.get(),
                            oninput: move |value: String| password.set(value),
                        }

                        PasswordInput {
                            label: "Password again",
                            toggle_visibility: false,
                            value_fn: move || confirm.get(),
                            oninput: move |value: String| confirm.set(value),
                            onchange: move |_value: String| {},
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
                            {|| if busy.get() { "Making your keys..." } else { "Create account" }}
                        }

                        div {
                            class: "pimble-page__footer",
                            span { "Already have an account?" }
                            span {
                                class: "pimble-link",
                                onclick: move || route::go(Route::Login),
                                "Sign in"
                            }
                        }
                    },

                    Stage::Recovery => div {
                        style: "display: flex; flex-direction: column; gap: 14px;",

                        h2 { style: "margin: 0; font-size: 16px;", "Your recovery code" }
                        p { class: "pimble-page__lede",
                            "This is the only way back into your notes if you forget your \
                             password. It is shown once and we cannot show it again."
                        }

                        div { class: "pimble-recovery", {|| recovery_code.get()} }

                        Checkbox {
                            label: "I have written this down somewhere safe",
                            checked_fn: move || saved.get(),
                            onchange: move || saved.update(|v| *v = !*v),
                        }

                        Button {
                            variant: "filled",
                            full_width: true,
                            disabled: {|| !saved.get()},
                            onclick: move || {
                                recovery_code.set(String::new());
                                stage.set(Stage::CheckInbox);
                            },
                            "Continue"
                        }
                    },

                    Stage::CheckInbox => div {
                        style: "display: flex; flex-direction: column; gap: 14px;",

                        h2 { style: "margin: 0; font-size: 16px;", "Check your inbox" }
                        p { class: "pimble-page__lede",
                            "We sent a verification link to "
                            {|| email.get()}
                            ". Your account starts working as soon as you follow it."
                        }

                        if !resent.get().is_empty() {
                            div { class: "pimble-page__lede", {|| resent.get()} }
                        }

                        Button {
                            variant: "light",
                            full_width: true,
                            onclick: move || {
                                let address = untracked(|| email.get());
                                resent.set("Sending...".to_string());
                                spawn_local(async move {
                                    match accounts::resend_verification(&address).await {
                                        Ok(()) => resent.set(
                                            "Sent. It can take a minute to arrive.".to_string(),
                                        ),
                                        Err(e) => resent.set(e.message),
                                    }
                                });
                            },
                            "Send the link again"
                        }

                        div {
                            class: "pimble-page__footer",
                            span {
                                class: "pimble-link",
                                onclick: move || route::go(Route::Login),
                                "Go to sign in"
                            }
                        }
                    },
                }
            }
        }
    }
}

/// Derive, generate, wrap twice, and post. Returns the recovery code to show.
///
/// Two Argon2id runs at the contract's cost (the password's and the recovery
/// code's), which is why the caller yields to the browser first.
async fn build_and_send(email: &str, password: &str) -> Result<String, String> {
    let kdf = KdfParams::generate();
    let derived = derive_timed(password, &kdf).map_err(|e| e.to_string())?;

    let account = AccountKeys::generate();
    let public_keys = account.public_keys();
    let account_key_blob = wrap_account_keys(&account, &derived.kek)
        .map_err(|e| format!("Wrapping your keys failed: {e}"))?;

    // The recovery code gets its own salt, so recovering never needs the
    // password's parameters and the two derivations share nothing.
    let recovery_code = generate_recovery_code();
    let recovery_params = KdfParams::generate();
    let recovery_kek = pimble_crypto::derive_recovery_kek(&recovery_code, &recovery_params)
        .map_err(|e| format!("Deriving the recovery key failed: {e}"))?;
    let recovery_key_blob = wrap_account_keys(&account, &recovery_kek)
        .map_err(|e| format!("Wrapping your recovery copy failed: {e}"))?;

    let request = SignupRequest {
        email: email.to_string(),
        auth_key: encode_auth_key(&derived.auth_key),
        kdf,
        public_keys,
        account_key_blob,
        recovery_salt: recovery_params.salt.clone(),
        recovery_key_blob,
    };

    accounts::signup(&request).await.map_err(|e| e.message)?;
    Ok(recovery_code)
}
