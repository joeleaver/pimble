//! `/app/forgot` — ask for a recovery link.
//!
//! The answer never varies: a reply that said "no account here" would tell a
//! stranger who has one. The page instead says plainly what recovery costs,
//! because the honest sentence is the useful one. The account keys are wrapped
//! under the password's KEK and the recovery code's, and the server holds
//! neither, so a link on its own opens nothing.

use rinch::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::accounts;
use crate::pages::PAGE_CSS;
use crate::route::{self, Route};

#[component]
pub fn forgot_page() -> NodeHandle {
    let email = Signal::new(String::new());
    let error = Signal::new(String::new());
    let sent = Signal::new(false);
    let busy = Signal::new(false);

    let submit = move || {
        if untracked(|| busy.get()) {
            return;
        }
        let address = untracked(|| email.get()).trim().to_string();
        if address.is_empty() || !address.contains('@') {
            error.set("Enter your email address.".to_string());
            return;
        }
        error.set(String::new());
        busy.set(true);
        spawn_local(async move {
            match accounts::recover_start(&address).await {
                // The same answer either way; only a failure to reach the
                // service at all is worth reporting.
                Ok(()) => sent.set(true),
                Err(e) => error.set(e.message),
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

                match sent.get() {
                    false => div {
                        style: "display: flex; flex-direction: column; gap: 14px;",

                        h2 { style: "margin: 0; font-size: 16px;", "Forgot your password" }

                        p { class: "pimble-page__lede",
                            "Recovery needs the recovery code you saved at signup. Without \
                             it, the notes in your account cannot be decrypted by anyone, \
                             including us."
                        }

                        TextInput {
                            label: "Email",
                            input_type: "email",
                            placeholder: "you@example.com",
                            value_fn: move || email.get(),
                            oninput: move |value: String| email.set(value),
                            onsubmit: submit,
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
                            "Send a recovery link"
                        }
                    },

                    true => div {
                        style: "display: flex; flex-direction: column; gap: 14px;",

                        h2 { style: "margin: 0; font-size: 16px;", "Check your inbox" }
                        p { class: "pimble-page__lede",
                            "If that address has an account, we sent a link. It works once \
                             and expires in an hour."
                        }
                        p { class: "pimble-page__lede",
                            "You will need your recovery code to finish."
                        }
                    },
                }

                div {
                    class: "pimble-page__footer",
                    span {
                        class: "pimble-link",
                        onclick: move || route::go(Route::Login),
                        "Back to sign in"
                    }
                }
            }
        }
    }
}
