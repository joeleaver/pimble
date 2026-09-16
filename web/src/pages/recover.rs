//! `/app/recover?token=` — set a new password with the recovery code.
//!
//! The server holds the account keys wrapped under two KEKs and neither of the
//! two, so this page does the only thing that can work: it opens the recovery
//! copy with the code, re-wraps the very same keys under a new password, and
//! sends the wrapped forms back. The public keys never change, so every store
//! envelope keeps working and shared stores stay readable.
//!
//! A wrong code fails here, in this browser, when the unwrap fails. Nothing is
//! sent for the server to judge.
//!
//! The page also has to be honest with someone who has lost the code, because
//! for them there is no way back in. It says so, and offers the only thing
//! left: deleting the account and everything in it, behind typing the address.

use pimble_crypto::{
    derive_recovery_kek, encode_auth_key, generate_recovery_code, unwrap_account_keys,
    wrap_account_keys, KdfParams,
};
use rinch::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::accounts::{self, RecoverCompleteRequest, RecoveryMaterial};
use crate::pages::PAGE_CSS;
use crate::route::{self, Route};
use crate::session::derive_timed;
use crate::util::{query_param, yield_to_browser};

/// The shortest password the accounts service accepts.
const MIN_PASSWORD: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Stage {
    #[default]
    Loading,
    /// The link is unknown, expired or already used.
    Invalid,
    /// The recovery code and a new password.
    Form,
    /// The new recovery code, shown once.
    NewCode,
    /// "I don't have my recovery code".
    DeadEnd,
}

#[component]
pub fn recover_page() -> NodeHandle {
    let stage = Signal::new(Stage::Loading);
    let email = Signal::new(String::new());
    let code = Signal::new(String::new());
    let password = Signal::new(String::new());
    let confirm = Signal::new(String::new());
    let error = Signal::new(String::new());
    let busy = Signal::new(false);
    let new_code = Signal::new(String::new());
    let saved = Signal::new(false);
    let typed_email = Signal::new(String::new());

    // In a signal rather than a `String`: the handlers below sit inside a
    // reactive `match`, which re-runs, and a `move` closure that swallowed a
    // `String` could only run once (rinch rule 16).
    let token = Signal::new(query_param("token").unwrap_or_default());

    // The material this token buys. Held for the life of the page, because the
    // token is one-use and asking again would spend it.
    let material: Signal<Option<RecoveryMaterial>> = Signal::new(None);

    {
        let token = untracked(|| token.get());
        spawn_local(async move {
            if token.is_empty() {
                stage.set(Stage::Invalid);
                return;
            }
            match accounts::recover_material(&token).await {
                Ok(found) => {
                    email.set(found.email.clone());
                    material.set(Some(found));
                    stage.set(Stage::Form);
                }
                Err(_) => stage.set(Stage::Invalid),
            }
        });
    }

    let submit = move || {
        {
            if untracked(|| busy.get()) {
                return;
            }
            let typed = untracked(|| code.get());
            let secret = untracked(|| password.get());
            let again = untracked(|| confirm.get());
            let Some(found) = untracked(|| material.get()) else { return };

            if typed.trim().is_empty() {
                error.set("Enter your recovery code.".to_string());
                return;
            }
            if secret.chars().count() < MIN_PASSWORD {
                error.set(format!("Use at least {MIN_PASSWORD} characters."));
                return;
            }
            if secret != again {
                error.set("The two passwords do not match.".to_string());
                return;
            }

            error.set(String::new());
            busy.set(true);
            let token = untracked(|| token.get());
            spawn_local(async move {
                // Two Argon2id runs ahead; let the button repaint as busy.
                yield_to_browser().await;
                match rewrap(&token, &found, &typed, &secret).await {
                    Ok(fresh) => {
                        new_code.set(fresh);
                        password.set(String::new());
                        confirm.set(String::new());
                        code.set(String::new());
                        stage.set(Stage::NewCode);
                    }
                    Err(message) => error.set(message),
                }
                busy.set(false);
            });
        }
    };

    let delete_account = move || {
        {
            let typed = untracked(|| typed_email.get()).trim().to_lowercase();
            let expected = untracked(|| email.get()).trim().to_lowercase();
            if expected.is_empty() || typed != expected {
                error.set("Type the account's email address to confirm.".to_string());
                return;
            }
            error.set(String::new());
            busy.set(true);
            let token = untracked(|| token.get());
            spawn_local(async move {
                match accounts::recover_delete_account(&token).await {
                    Ok(()) => route::replace_with(Route::Signup),
                    Err(e) => error.set(e.message),
                }
                busy.set(false);
            });
        }
    };

    rsx! {
        div {
            class: "pimble-page",
            style { {PAGE_CSS} }

            div {
                class: "pimble-page__card",

                h1 { class: "pimble-page__brand", "Pimble" }

                match stage.get() {
                    Stage::Loading => p { class: "pimble-page__lede", "Checking that link..." },

                    Stage::Invalid => div {
                        style: "display: flex; flex-direction: column; gap: 14px;",
                        h2 { style: "margin: 0; font-size: 16px;", "That link does not work" }
                        p { class: "pimble-page__lede",
                            "A recovery link works once and expires after an hour. Ask for \
                             a new one."
                        }
                        Button {
                            variant: "filled",
                            full_width: true,
                            onclick: move || route::go(Route::Forgot),
                            "Send another link"
                        }
                    },

                    Stage::Form => div {
                        style: "display: flex; flex-direction: column; gap: 14px;",

                        h2 { style: "margin: 0; font-size: 16px;", "Set a new password" }
                        p { class: "pimble-page__lede",
                            "Recovering " {|| email.get()}
                            ". Your recovery code is what opens your notes; it is checked \
                             here, in this browser."
                        }

                        TextInput {
                            label: "Recovery code",
                            placeholder: "XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX",
                            value_fn: move || code.get(),
                            oninput: move |value: String| code.set(value),
                        }

                        PasswordInput {
                            label: "New password",
                            description: "At least 8 characters.",
                            toggle_visibility: false,
                            value_fn: move || password.get(),
                            oninput: move |value: String| password.set(value),
                        }

                        PasswordInput {
                            label: "New password again",
                            toggle_visibility: false,
                            value_fn: move || confirm.get(),
                            oninput: move |value: String| confirm.set(value),
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
                            {|| if busy.get() { "Rewrapping your keys..." } else { "Set password" }}
                        }

                        div {
                            class: "pimble-page__footer",
                            span {
                                class: "pimble-link",
                                onclick: move || {
                                    error.set(String::new());
                                    stage.set(Stage::DeadEnd);
                                },
                                "I don't have my recovery code"
                            }
                        }
                    },

                    Stage::NewCode => div {
                        style: "display: flex; flex-direction: column; gap: 14px;",

                        h2 { style: "margin: 0; font-size: 16px;", "Your new recovery code" }
                        p { class: "pimble-page__lede",
                            "The old one no longer works. This is shown once and we cannot \
                             show it again."
                        }

                        div { class: "pimble-recovery", {|| new_code.get()} }

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
                                new_code.set(String::new());
                                route::replace_with(Route::Login);
                            },
                            "Continue to sign in"
                        }
                    },

                    Stage::DeadEnd => div {
                        style: "display: flex; flex-direction: column; gap: 14px;",

                        h2 { style: "margin: 0; font-size: 16px;", "Without the code" }
                        p { class: "pimble-page__lede",
                            "Your notes are encrypted with keys that only your recovery code \
                             and your password can open. We do not have either, so there is \
                             nothing we can do to read them for you, and no new password will \
                             bring them back."
                        }
                        p { class: "pimble-page__lede",
                            "The only thing left is to delete the account and everything in \
                             it, and start again."
                        }

                        TextInput {
                            label: "Type the account's email to confirm",
                            placeholder: {|| email.get()},
                            value_fn: move || typed_email.get(),
                            oninput: move |value: String| typed_email.set(value),
                        }

                        if !error.get().is_empty() {
                            div { class: "pimble-page__error", {|| error.get()} }
                        }

                        Button {
                            variant: "filled",
                            color: "red",
                            full_width: true,
                            disabled: {|| busy.get()},
                            onclick: delete_account,
                            "Delete this account and everything in it"
                        }

                        div {
                            class: "pimble-page__footer",
                            span {
                                class: "pimble-link",
                                onclick: move || {
                                    error.set(String::new());
                                    stage.set(Stage::Form);
                                },
                                "I found my recovery code"
                            }
                        }
                    },
                }
            }
        }
    }
}

/// Open the recovery copy of the account keys and wrap the same keys again:
/// once under the new password, once under a brand-new recovery code.
///
/// Returns the new code, to show once. The keys themselves never leave this
/// function unwrapped, and the account's public keys are untouched, which is
/// what keeps every store envelope valid.
async fn rewrap(
    token: &str,
    material: &RecoveryMaterial,
    code: &str,
    new_password: &str,
) -> Result<String, String> {
    let recovery_kek = derive_recovery_kek(code, &material.kdf)
        .map_err(|e| format!("Deriving from that code failed: {e}"))?;
    let keys = unwrap_account_keys(&material.recovery_key_blob, &recovery_kek)
        .map_err(|_| "That recovery code does not open this account.".to_string())?;

    // The keys that came out must be the account's. A server that served
    // somebody else's blob, or a tampered one, is caught here rather than after
    // the new password is set and every store has stopped opening.
    if keys.public_keys() != material.public_keys {
        return Err("The recovered keys do not match this account.".to_string());
    }

    let kdf = KdfParams::generate();
    let derived = derive_timed(new_password, &kdf).map_err(|e| e.to_string())?;
    let account_key_blob = wrap_account_keys(&keys, &derived.kek)
        .map_err(|e| format!("Wrapping your keys failed: {e}"))?;

    // A used code is a spent code: recovery mints a new one rather than
    // leaving the old one able to open the account again.
    let fresh_code = generate_recovery_code();
    let recovery_params = KdfParams::generate();
    let fresh_kek = derive_recovery_kek(&fresh_code, &recovery_params)
        .map_err(|e| format!("Deriving the new recovery key failed: {e}"))?;
    let recovery_key_blob = wrap_account_keys(&keys, &fresh_kek)
        .map_err(|e| format!("Wrapping your recovery copy failed: {e}"))?;

    accounts::recover_complete(
        token,
        &RecoverCompleteRequest {
            auth_key: encode_auth_key(&derived.auth_key),
            kdf,
            account_key_blob,
            recovery_salt: recovery_params.salt.clone(),
            recovery_key_blob,
        },
    )
    .await
    .map_err(|e| e.message)?;

    Ok(fresh_code)
}
