//! Alpha diagnostics in the window (docs/DIAGNOSTICS_CONTRACT.md sections 4,
//! 6 and 7), the same in the desktop app and in the browser: Settings >
//! Diagnostics (what is sent, the switch, the notice shown once) and Help >
//! "Send Diagnostics..." (a report with a note, answered with a code). The
//! records themselves are each target's own: `pimble_server::diagnostics` on
//! the desktop, the web crate's in the browser, reached through [`Hooks`].

use std::sync::OnceLock;

use rinch::prelude::*;

use crate::protocol::BackendCommand;
use crate::state::AppStore;

/// What is sent, said once for the page and the notice.
pub const WHAT_IS_SENT: &str = "While Pimble is in alpha, it sends its developers what they need to see what went wrong: \
warnings and errors, how syncing and sharing go, your stores' names and where they are on disk, \
and your computer's kind (system, processor, memory). It never sends anything you wrote in a note, \
your notes' titles, your email address, or any password or key. When you are signed in, records \
carry your account's number (not your email), so your devices and both ends of a share can be read together.";

/// How the window reaches this target's diagnostics.
pub struct Hooks {
    /// Whether diagnostics are on, and whether the notice was shown.
    pub settings: fn() -> (bool, bool),
    /// Save both.
    pub save: fn(enabled: bool, noticed: bool) -> Result<(), String>,
    /// This install's number, while diagnostics record.
    pub install: fn() -> Option<String>,
    /// Whether this copy sends at all (a build that carries the key, or a
    /// page whose service takes them).
    pub available: fn() -> bool,
}

static HOOKS: OnceLock<Hooks> = OnceLock::new();

/// The browser's, set once at start before the window is built.
pub fn set_hooks(hooks: Hooks) {
    let _ = HOOKS.set(hooks);
}

#[cfg(feature = "native")]
fn native_hooks() -> Hooks {
    Hooks {
        settings: || {
            let s = pimble_server::diagnostics::settings();
            (s.enabled, s.noticed)
        },
        save: |enabled, noticed| pimble_server::diagnostics::save_settings(&pimble_server::diagnostics::Settings { enabled, noticed }).map_err(|e| e.to_string()),
        install: || pimble_server::diagnostics::ids().map(|(install, _)| install),
        available: || pimble_server::diagnostics::ids().is_some(),
    }
}

fn hooks() -> Option<&'static Hooks> {
    #[cfg(feature = "native")]
    {
        Some(HOOKS.get_or_init(native_hooks))
    }
    #[cfg(not(feature = "native"))]
    {
        HOOKS.get()
    }
}

/// At start: the notice, once, the first time this copy sends.
pub fn notice_once(store: AppStore) {
    let Some(h) = hooks() else { return };
    let (enabled, noticed) = (h.settings)();
    if noticed || !(h.available)() {
        return;
    }
    let _ = (h.save)(enabled, true);
    store.diagnostics_notice.set(true);
    crate::settings::open_at(store, crate::settings::Section::Diagnostics);
}

/// Settings > Diagnostics.
pub fn render_page(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let enabled = Signal::new(hooks().map(|h| (h.settings)().0).unwrap_or(true));
    let install = hooks().and_then(|h| (h.install)());
    let flip = move || {
        let Some(h) = hooks() else { return };
        let on = !untracked(|| enabled.get());
        if (h.save)(on, true).is_ok() {
            enabled.set(on);
        }
    };
    let when = if cfg!(feature = "native") { "the next time Pimble starts" } else { "the next time this page loads" };
    let wait = if cfg!(feature = "native") { "on this computer" } else { "in this browser" };
    rsx! {
        div {
            class: "pimble-settings__section",
            if store.diagnostics_notice.get() {
                div { class: "pimble-settings__heading", "Pimble is an alpha, and it sends diagnostics" }
            }
            if !store.diagnostics_notice.get() {
                div { class: "pimble-settings__heading", "Diagnostics" }
            }
            div { class: "pimble-settings__line", {WHAT_IS_SENT} }
            Checkbox {
                label: "Send diagnostics",
                checked_fn: move || enabled.get(),
                onchange: move || flip(),
            }
            div {
                class: "pimble-settings__note",
                {move || if enabled.get() {
                    format!("Turned off, nothing more is sent and what waits {wait} is deleted.")
                } else {
                    format!("Off: nothing is recorded or sent. Turned on, sending starts {when}.")
                }}
            }
            div {
                class: "pimble-settings__note",
                "Help > Send Diagnostics... sends a report whenever something goes wrong, on or off."
            }
            if let Some(install) = install.clone() {
                div { class: "pimble-settings__note", {format!("This install's number: {install}")} }
            }
        }
    }
}

/// Help > "Send Diagnostics...".
pub fn open_send(store: AppStore) {
    store.diagnostics_sent.set(None);
    store.diagnostics_send_open.set(true);
}

/// The open stores as this window knows them: what a report says of them.
fn stores_shape(store: AppStore) -> serde_json::Value {
    let stores: Vec<serde_json::Value> = untracked(|| {
        store.store_data.with(|map| {
            map.values()
                .map(|sig| {
                    sig.with(|s| {
                        serde_json::json!({
                            "id": s.id.to_string(),
                            "name": s.name,
                            "kind": format!("{:?}", s.kind),
                            "sync": format!("{:?}", s.sync_state),
                            "replica": s.is_replica,
                        })
                    })
                })
                .collect()
        })
    });
    serde_json::json!({ "stores": stores })
}

fn send(store: AppStore) {
    if untracked(|| store.diagnostics_sending.get()) {
        return;
    }
    store.diagnostics_sent.set(None);
    store.diagnostics_sending.set(true);
    let note = untracked(|| store.diagnostics_note.get());
    store.send(BackendCommand::SendDiagnostics { note, stores: stores_shape(store) });
}

/// What the report sends, in a sentence.
fn what_goes() -> &'static str {
    if cfg!(feature = "native") {
        "This sends Pimble's developers the log of this run and the last one (with your home folder, email addresses, keys and note titles taken out), your open stores' names and states, and what you write here. Nothing you wrote in your notes goes."
    } else {
        "This sends Pimble's developers what this page has logged since it loaded (with email addresses, keys and note titles taken out), your open stores' names and states, and what you write here. Nothing you wrote in your notes goes."
    }
}

/// The window.
pub fn render_send(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    rsx! {
        Modal {
            opened_fn: move || store.diagnostics_send_open.get(),
            onclose: move || store.diagnostics_send_open.set(false),
            title: "Send Diagnostics",
            size: "md",
            div {
                style: "display: flex; flex-direction: column; gap: 10px;",
                div { style: "font-size: 13px;", {what_goes()} }
                Textarea {
                    label: "What went wrong?",
                    placeholder: "My edits to the shared note don't reach Joe.",
                    value_fn: move || store.diagnostics_note.get(),
                    oninput: move |val: String| store.diagnostics_note.set(val),
                }
                div {
                    style: "font-size: 13px;",
                    {move || match store.diagnostics_sent.get() {
                        Some(Ok(code)) => format!("Sent. Its code is {code}: tell us that, and we can find it."),
                        Some(Err(why)) => why,
                        None if store.diagnostics_sending.get() => "Sending...".to_string(),
                        None => String::new(),
                    }}
                }
                div {
                    style: "display: flex; gap: 8px; justify-content: flex-end;",
                    Button { variant: "default", size: "xs", onclick: move || store.diagnostics_send_open.set(false), "Close" }
                    Button {
                        variant: "filled",
                        size: "xs",
                        loading: {|| store.diagnostics_sending.get()},
                        disabled: {|| store.diagnostics_sending.get()},
                        onclick: move || send(store),
                        "Send"
                    }
                }
            }
        }
    }
}
