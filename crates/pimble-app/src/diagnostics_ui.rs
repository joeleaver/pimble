//! Settings > Diagnostics (docs/DIAGNOSTICS_CONTRACT.md sections 6-7): what
//! this computer's Pimble sends while it is an alpha, the switch that turns it
//! off, and the notice shown once, at the first start that sends any. The
//! records themselves are `pimble_server::diagnostics`'.

use rinch::prelude::*;

use crate::state::AppStore;

/// What is sent, said once for the page and the notice.
pub const WHAT_IS_SENT: &str = "While Pimble is in alpha, it sends its developers what they need to see what went wrong: \
warnings and errors, how syncing and sharing go, your stores' names and where they are on disk, \
and your computer's kind (system, processor, memory). It never sends anything you wrote in a note, \
your notes' titles, your email address, or any password or key. When you are signed in, records \
carry your account's number (not your email), so your devices and both ends of a share can be read together.";

/// Whether this build sends anything at all (it carries the key).
pub fn available() -> bool {
    pimble_server::diagnostics::ids().is_some() || !pimble_server::diagnostics::settings().enabled
}

/// At start: the notice, once, the first time this computer sends.
pub fn notice_once(store: AppStore) {
    let mut settings = pimble_server::diagnostics::settings();
    if settings.noticed || pimble_server::diagnostics::ids().is_none() {
        return;
    }
    settings.noticed = true;
    let _ = pimble_server::diagnostics::save_settings(&settings);
    store.diagnostics_notice.set(true);
    crate::settings::open_at(store, crate::settings::Section::Diagnostics);
}

/// The page.
pub fn render_page(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let enabled = Signal::new(pimble_server::diagnostics::settings().enabled);
    let install = pimble_server::diagnostics::ids().map(|(install, _)| install);
    let flip = move || {
        let mut settings = pimble_server::diagnostics::settings();
        settings.enabled = !untracked(|| enabled.get());
        settings.noticed = true;
        if pimble_server::diagnostics::save_settings(&settings).is_ok() {
            enabled.set(settings.enabled);
        }
    };
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
                    "Turned off, nothing more is sent and what waits on this computer is deleted.".to_string()
                } else {
                    "Off: nothing is recorded or sent. Turned on, sending starts the next time Pimble starts.".to_string()
                }}
            }
            if let Some(install) = install.clone() {
                div { class: "pimble-settings__note", {format!("This computer's install number: {install}")} }
            }
        }
    }
}
