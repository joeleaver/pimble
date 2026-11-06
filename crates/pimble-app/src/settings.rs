//! File > "Settings..." (docs/SETTINGS_CONTRACT.md section 2): one window with
//! sections down the left and the chosen section's page on the right. It holds
//! what had windows of their own (Account, AI Agents, Plugins) and the theme,
//! and where keeping the app current stands. Every page is rendered once and
//! shown while its section is chosen, so a page keeps what was typed in it
//! while another is looked at.

use rinch::prelude::*;

use crate::state::AppStore;

/// A section of the Settings window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    General,
    Account,
    Plugins,
    Agents,
    Updates,
}

impl Section {
    /// The sections this build has, in order: the browser keeps no agents and
    /// is always the version pimble.app serves.
    pub fn all() -> &'static [Section] {
        if cfg!(feature = "native") {
            &[Section::General, Section::Account, Section::Plugins, Section::Agents, Section::Updates]
        } else {
            &[Section::General, Section::Account, Section::Plugins]
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Section::General => "General",
            Section::Account => "Account",
            Section::Plugins => "Plugins",
            Section::Agents => "AI Agents",
            Section::Updates => "Updates",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Section::General => "general",
            Section::Account => "account",
            Section::Plugins => "plugins",
            Section::Agents => "agents",
            Section::Updates => "updates",
        }
    }

    fn from_key(key: &str) -> Option<Section> {
        Section::all().iter().copied().find(|s| s.key() == key)
    }
}

/// File > "Settings..." (Ctrl+,): the section chosen last time.
pub fn open(store: AppStore) {
    let last = crate::persistence::load_settings_section().and_then(|k| Section::from_key(&k)).unwrap_or(Section::General);
    open_at(store, last);
}

/// Open the window at `section`, as the old menu items ("Account...",
/// "Plugins...", "AI Agents...") and the status bar do.
pub fn open_at(store: AppStore, section: Section) {
    choose(store, section);
}

/// Show `section`'s page, getting it ready the way its own window used to be
/// when it opened.
fn choose(store: AppStore, section: Section) {
    match section {
        Section::Plugins => crate::plugins::shown(store),
        #[cfg(feature = "native")]
        Section::Agents => crate::agents::shown(store),
        Section::Account => crate::app::account_shown(store),
        _ => {}
    }
    store.settings.set(Some(section));
    crate::persistence::save_settings_section(section.key());
}

/// The window closes: what each page held only while it was open goes (a
/// typed password, an agent's key shown once).
fn close(store: AppStore) {
    store.settings.set(None);
    crate::app::account_hidden(store);
    #[cfg(feature = "native")]
    crate::agents::hidden(store);
}

/// Whether the window is open at `section`.
pub fn showing(store: AppStore, section: Section) -> bool {
    store.settings.get() == Some(section)
}

/// The window, made once. `account` is the Account page (it lives in
/// `app.rs`, beside the sign-in it sends).
pub fn render(__scope: &mut RenderScope, store: AppStore, account: NodeHandle) -> NodeHandle {
    let general = render_general(__scope, store);
    let plugins = crate::plugins::render_page(__scope, store);
    let nav = rsx! {
        div {
            class: "pimble-settings__nav",
            for section in Section::all().iter().copied() {
                div {
                    key: section.key(),
                    class: {move || if store.settings.get() == Some(section) { "pimble-settings__tab pimble-settings__tab--on" } else { "pimble-settings__tab" }},
                    onclick: move || choose(store, section),
                    {section.title()}
                }
            }
        }
    };
    #[cfg(feature = "native")]
    let pages = {
        let agents = crate::agents::render_page(__scope, store);
        let updates = render_updates(__scope, store);
        rsx! {
            div {
                class: "pimble-settings__page",
                div { style: {move || page_style(store, Section::General)}, {general} }
                div { style: {move || page_style(store, Section::Account)}, {account} }
                div { style: {move || page_style(store, Section::Plugins)}, {plugins} }
                div { style: {move || page_style(store, Section::Agents)}, {agents} }
                div { style: {move || page_style(store, Section::Updates)}, {updates} }
            }
        }
    };
    #[cfg(not(feature = "native"))]
    let pages = rsx! {
        div {
            class: "pimble-settings__page",
            div { style: {move || page_style(store, Section::General)}, {general} }
            div { style: {move || page_style(store, Section::Account)}, {account} }
            div { style: {move || page_style(store, Section::Plugins)}, {plugins} }
        }
    };
    rsx! {
        Modal {
            opened_fn: move || store.settings.get().is_some(),
            onclose: move || close(store),
            title: "Settings",
            size: "xl",
            div {
                class: "pimble-settings",
                {nav}
                {pages}
            }
        }
    }
}

/// A page shows while its section is chosen.
fn page_style(store: AppStore, section: Section) -> &'static str {
    if store.settings.get() == Some(section) { "display: block;" } else { "display: none;" }
}

fn choice_class(on: bool) -> &'static str {
    if on { "pimble-settings__choice pimble-settings__choice--on" } else { "pimble-settings__choice" }
}

/// General: the theme.
fn render_general(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let pick = move |dark: bool| {
        move || {
            if untracked(|| store.dark_mode.get()) != dark {
                crate::app::toggle_dark_mode(store);
            }
        }
    };
    rsx! {
        div {
            class: "pimble-settings__section",
            div { class: "pimble-settings__heading", "Theme" }
            div {
                class: "pimble-settings__choices",
                div { class: {move || choice_class(store.dark_mode.get())}, onclick: pick(true), "Dark" }
                div { class: {move || choice_class(!store.dark_mode.get())}, onclick: pick(false), "Light" }
            }
            div { class: "pimble-settings__note", "View > Toggle Dark Mode switches it too." }
        }
    }
}

/// Updates: this version and where keeping it current stands, as Help > About
/// says it (docs/UPDATE_CONTRACT.md).
#[cfg(feature = "native")]
fn render_updates(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    use crate::state::UpdateStatus;
    let can_check = move || !matches!(store.update_status.get(), UpdateStatus::NotInstalled | UpdateStatus::Checking | UpdateStatus::Ready { .. });
    let ready = move || matches!(store.update_status.get(), UpdateStatus::Ready { .. });
    rsx! {
        div {
            class: "pimble-settings__section",
            div { class: "pimble-settings__heading", {format!("Pimble {}", crate::about::VERSION)} }
            div { class: "pimble-settings__line", {move || crate::about::update_line(&store.update_status.get())} }
            div {
                class: "pimble-settings__buttons",
                if can_check() {
                    Button { variant: "default", size: "xs", onclick: move || crate::update::check_now(), "Check Now" }
                }
                if ready() {
                    Button { variant: "filled", size: "xs", onclick: move || crate::update::restart_to_update(store), "Restart to Update" }
                }
            }
        }
    }
}
