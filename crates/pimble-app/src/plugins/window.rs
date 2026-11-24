//! Account > Plugins...: Settings > Plugins, the plugins installed here and
//! what each was allowed, and the library's rows.

use super::library_client::{install_from_library, read_library, refresh_library};
use super::mount::revoke;
use super::*;

/// One installed plugin, as the Plugins window lists it.
#[derive(Clone, PartialEq)]
pub(super) struct PluginRow {
    id: String,
    name: String,
    /// What it provides and asks for, or why it does not run.
    line: String,
    /// What it is allowed here, or that it needs nothing.
    note: String,
    /// It was allowed something here, which "Revoke" takes back.
    revocable: bool,
    /// "Remove" takes it off this device (the browser's own store of
    /// plugins; the desktop's folder is the person's to change).
    removable: bool,
}

/// What a plugin asks for, as a row says it: "asks to read the stores you
/// choose".
pub(super) fn asks_sentence(asks: &pimble_plugin_host::abi::Permissions) -> &'static str {
    match (asks.stores, asks.write) {
        (StoreAsk::None, _) => "asks for nothing",
        (StoreAsk::All, true) => "asks to read and change all your stores",
        (StoreAsk::All, false) => "asks to read all your stores",
        (StoreAsk::Chosen, true) => "asks to read and change the stores you choose",
        (StoreAsk::Chosen, false) => "asks to read the stores you choose",
    }
}

/// One plugin the library offers, as Settings > Plugins lists it.
#[derive(Clone, PartialEq)]
pub(super) struct LibraryRow {
    pub(super) id: String,
    /// "Kanban 0.1.0".
    pub(super) title: String,
    pub(super) summary: String,
    /// "Asks to read and change the stores you choose."
    pub(super) asks: String,
    pub(super) offer: Offer,
    /// "Install" or "Update", when the row has a button.
    pub(super) button: Option<&'static str>,
    /// Downloading, or what came of the last install.
    pub(super) line: String,
    /// A link asked about this plugin.
    pub(super) asked: bool,
}

#[derive(Clone, Copy)]
pub(super) struct PluginsUi {
    pub(super) rows: Signal<Vec<PluginRow>>,
    /// What came of the last "Add plugin..." or "Remove".
    pub(super) status: Signal<String>,
    pub(super) library: Signal<Vec<LibraryRow>>,
    /// Reading the library, or why it could not be read.
    pub(super) library_line: Signal<String>,
}

/// Account > "Plugins...": Settings at Plugins.
pub(crate) fn open(store: AppStore) {
    crate::settings::open_at(store, crate::settings::Section::Plugins);
}

/// Settings > Plugins is shown: the list as it is now, and no line left from
/// last time.
pub(crate) fn shown(store: AppStore) {
    if let Some(ui) = UI.with(Cell::get) {
        ui.status.set(String::new());
        ui.rows.set(rows(store));
    }
    read_library();
}

/// Settings closes: what the rows said (an install, an update made on its
/// own) is said until then.
pub(crate) fn hidden() {
    ROW_LINES.with(|l| l.borrow_mut().clear());
    ASKED.with(|a| *a.borrow_mut() = None);
}

/// A link asked to install plugin `id` (the website's "Install in Pimble"):
/// its row in the library is marked and says what to press. Nothing is
/// installed until the person presses it.
pub(crate) fn offer_install(id: &str) {
    ASKED.with(|a| *a.borrow_mut() = Some(id.to_string()));
    refresh_library();
}

pub(super) fn refresh_window() {
    if let (Some(ui), Some(store)) = (UI.with(Cell::get), STORE.with(Cell::get)) {
        ui.rows.set(rows(store));
    }
}

fn rows(store: AppStore) -> Vec<PluginRow> {
    with_registry(|registry| rows_of(store, registry)).unwrap_or_default()
}

fn rows_of(store: AppStore, registry: &Registry) -> Vec<PluginRow> {
    let mut rows: Vec<PluginRow> = registry
        .plugins
        .iter()
        .map(|p| {
            let m = &p.manifest;
            let viewers: Vec<String> = m.viewers().map(|v| v.name.clone()).collect();
            let asks = asks_sentence(&m.asks);
            let line = format!("{} {}: the {} viewer; {asks}.", m.name, m.version, viewers.join(", "));
            let came = if PROVENANCE.with(|p| p.borrow().from_library(&m.id)) { "From the library." } else { "Added from a file." };
            let approval = approval_of(&m.id);
            let verb = if approval.write { "read and change" } else { "read" };
            let (note, revocable) = match &approval.stores {
                Approved::All => (format!("Allowed to {verb} all your stores on this device."), true),
                Approved::Only(ids) => {
                    let names = ids.iter().map(|id| store_name(store, *id)).collect::<Vec<_>>().join(", ");
                    (format!("Allowed to {verb} {names} on this device."), true)
                }
                Approved::Nothing if m.asks.asks_anything() => ("Not allowed anything yet.".to_string(), false),
                Approved::Nothing => ("It needs nothing allowed.".to_string(), false),
            };
            let note = format!("{came} {note}");
            PluginRow { id: m.id.clone(), name: m.name.clone(), line, note, revocable, removable: place::REMOVABLE }
        })
        .collect();
    for refused in &registry.refused {
        rows.push(PluginRow { id: String::new(), name: refused.file(), line: refused.sentence.clone(), note: String::new(), revocable: false, removable: false });
    }
    rows
}

/// Settings > Plugins: each plugin installed, what it asks for, what it was
/// allowed here, "Revoke" and "Remove"; and "Add from a file...".
pub(crate) fn render_page(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let ui = PluginsUi {
        rows: Signal::new(Vec::new()),
        status: Signal::new(String::new()),
        library: Signal::new(Vec::new()),
        library_line: Signal::new(String::new()),
    };
    UI.with(|u| u.set(Some(ui)));
    rsx! {
            div {
                class: "pimble-plugins",
                style: "display: flex; flex-direction: column; gap: 12px;",
                div {
                    style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                    {place::WHERE.to_string()}
                }
                if ui.rows.with(|r| r.is_empty()) {
                    div { style: "font-size: 13px;", "No plugins are installed." }
                }
                for row in ui.rows.get() {
                    div {
                        key: format!("{} {} {} {}", row.id, row.name, row.note, row.revocable),
                        class: "pimble-plugins__row",
                        style: "display: flex; gap: 12px; align-items: center;",
                        div {
                            style: "flex: 1; display: flex; flex-direction: column; gap: 2px;",
                            div { style: "font-size: 13px;", {row.line.clone()} }
                            div {
                                style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                                {row.note.clone()}
                            }
                        }
                        if row.removable {
                            Button {
                                variant: "default",
                                size: "xs",
                                onclick: {
                                    let id = row.id.clone();
                                    move || place::remove(&id)
                                },
                                "Remove"
                            }
                        }
                        if row.revocable {
                            Button {
                                variant: "default",
                                size: "xs",
                                onclick: {
                                    let id = row.id.clone();
                                    move || revoke(store, &id)
                                },
                                "Revoke"
                            }
                        }
                    }
                }
                if !ui.status.get().is_empty() {
                    div { style: "font-size: 12px;", {move || ui.status.get()} }
                }
                div {
                    style: "display: flex; justify-content: flex-start; gap: 8px;",
                    if place::CAN_ADD {
                        Button { variant: "default", size: "xs", onclick: move || place::pick(), "Add from a File..." }
                    }
                }
                div { class: "pimble-settings__heading", style: "margin-top: 8px;", "Library" }
                div {
                    style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                    "Plugins from pimble.app, signed by Pimble and checked before they are installed."
                }
                if !ui.library_line.get().is_empty() {
                    div { style: "font-size: 13px;", {move || ui.library_line.get()} }
                }
                for row in ui.library.get() {
                    div {
                        key: format!("{} {} {:?} {} {}", row.id, row.title, row.offer, row.line, row.asked),
                        class: if row.asked { "pimble-plugins__row pimble-plugins__row--asked" } else { "pimble-plugins__row" },
                        style: "display: flex; gap: 12px; align-items: center;",
                        div {
                            style: "flex: 1; display: flex; flex-direction: column; gap: 2px;",
                            div { style: "font-size: 13px; font-weight: 600;", {row.title.clone()} }
                            div { style: "font-size: 13px;", {row.summary.clone()} }
                            div { style: "font-size: 12px; color: var(--rinch-color-dimmed);", {row.asks.clone()} }
                            if !row.line.is_empty() {
                                div { style: "font-size: 12px;", {row.line.clone()} }
                            }
                        }
                        if row.button.is_some() {
                            Button {
                                variant: "filled",
                                size: "xs",
                                onclick: {
                                    let id = row.id.clone();
                                    move || install_from_library(&id)
                                },
                                {row.button.unwrap_or_default()}
                            }
                        }
                        if row.button.is_none() {
                            div {
                                style: "font-size: 12px; color: var(--rinch-color-dimmed); white-space: nowrap;",
                                {offer_label(&row.offer)}
                            }
                        }
                    }
                }
            }
    }
}

/// What a library row says instead of a button.
fn offer_label(offer: &Offer) -> &'static str {
    match offer {
        Offer::Installed => "Installed",
        Offer::FromFile => "Added from a file",
        Offer::NeedsNewer => "Needs a newer Pimble",
        Offer::Install | Offer::Update { .. } => "",
    }
}

/// Say what came of an add or a remove, in the Plugins window.
pub(super) fn say(line: String) {
    if let Some(ui) = UI.with(Cell::get) {
        ui.status.set(line);
    }
}
