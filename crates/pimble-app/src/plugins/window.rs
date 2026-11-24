//! Settings > Plugins: one list of plugins, each once, from what is installed
//! here and what the library offers (docs/PLUGINS_CONTRACT.md section 7;
//! `listing` decides what each row says), with "Add from a File...".

use std::collections::HashSet;

use super::library_client::{install_from_library, read_library};
use super::listing::{self, Action, Installed, Row};
use super::mount::revoke as revoke_approval;
use super::*;

/// A row as the window draws it: the list's row, and what the plugin was
/// allowed here (Details), which "Revoke" takes back.
#[derive(Clone, PartialEq)]
pub(super) struct ShownRow {
    row: Row,
    allowed: Option<String>,
    revocable: bool,
}

/// A row ready to draw: every field is used once by the markup (a rinch `for`
/// body hands each to its own closure), and what shows or hides is a style,
/// since the row is drawn again whenever anything in it changes (its key).
#[derive(Clone, PartialEq)]
struct RowView {
    key: String,
    class: &'static str,
    title: String,
    summary: String,
    summary_style: &'static str,
    state: String,
    button: Option<(String, Action)>,
    button_style: &'static str,
    button_label: &'static str,
    toggle_id: String,
    toggle_label: &'static str,
    details_style: &'static str,
    details: Vec<String>,
    actions: Vec<(String, Action)>,
    revoke: Option<String>,
    revoke_style: &'static str,
}

const SHOW: &str = "";
const HIDE: &str = "display: none;";

impl RowView {
    fn of(shown: ShownRow, open: bool) -> RowView {
        let key = format!("{:?} {:?} {} {open}", shown.row, shown.allowed, shown.revocable);
        let row = shown.row;
        let mut details = row.details.clone();
        details.extend(shown.allowed);
        RowView {
            key,
            class: if row.asked { "pimble-plugins__row pimble-plugins__row--asked" } else { "pimble-plugins__row" },
            summary_style: if row.summary.is_empty() { HIDE } else { SHOW },
            title: row.title,
            summary: row.summary,
            state: row.state,
            button_style: if row.button.is_some() { SHOW } else { HIDE },
            button_label: row.button.map(Action::label).unwrap_or_default(),
            button: row.button.map(|a| (row.id.clone(), a)),
            toggle_id: row.id.clone(),
            toggle_label: if open { "Hide Details" } else { "Details" },
            details_style: if open { SHOW } else { HIDE },
            details,
            actions: row.more.iter().map(|a| (row.id.clone(), *a)).collect(),
            revoke_style: if shown.revocable { SHOW } else { HIDE },
            revoke: shown.revocable.then(|| row.id.clone()),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct PluginsUi {
    pub(super) rows: Signal<Vec<ShownRow>>,
    /// What came of the last "Add from a File..." or "Remove".
    pub(super) status: Signal<String>,
    /// Reading the library, or why it could not be read.
    pub(super) library_line: Signal<String>,
    /// Only the installed plugins are listed.
    installed_only: Signal<bool>,
    /// The rows whose Details are open, by plugin id.
    open: Signal<HashSet<String>>,
}

/// Account > "Plugins...": Settings at Plugins.
pub(crate) fn open(store: AppStore) {
    crate::settings::open_at(store, crate::settings::Section::Plugins);
}

/// Settings > Plugins is shown: the list as it is now, no line left from
/// last time, and the library read again (which also updates what it can).
pub(crate) fn shown(_store: AppStore) {
    if let Some(ui) = UI.with(Cell::get) {
        ui.status.set(String::new());
    }
    refresh_window();
    read_library();
}

/// Settings closes: what the rows said (an install, an update made on its
/// own) is said until then.
pub(crate) fn hidden() {
    ROW_LINES.with(|l| l.borrow_mut().clear());
    ASKED.with(|a| *a.borrow_mut() = None);
    if let Some(ui) = UI.with(Cell::get) {
        ui.open.set(HashSet::new());
    }
}

/// A link asked to install plugin `id` (the website's "Install in Pimble"):
/// its row is marked and says what to press. Nothing is installed until the
/// person presses it.
pub(crate) fn offer_install(id: &str) {
    ASKED.with(|a| *a.borrow_mut() = Some(id.to_string()));
    if let Some(ui) = UI.with(Cell::get) {
        ui.installed_only.set(false);
    }
    refresh_window();
}

/// Draw the list again from what is installed, the library's index and what
/// each row says now.
pub(super) fn refresh_window() {
    if let (Some(ui), Some(store)) = (UI.with(Cell::get), STORE.with(Cell::get)) {
        ui.rows.set(rows(store));
    }
}

/// What is installed here, as the list reads it.
pub(super) fn installed() -> Vec<Installed> {
    with_registry(|registry| {
        let running = registry.plugins.iter().map(|p| Installed::running(&p.manifest, file_name(&p.path)));
        running.chain(registry.refused.iter().map(Installed::refused)).collect()
    })
    .unwrap_or_default()
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
}

fn rows(store: AppStore) -> Vec<ShownRow> {
    let installed = installed();
    let index = INDEX.with(|i| i.borrow().clone());
    let provenance = PROVENANCE.with(|p| p.borrow().clone());
    let lines = ROW_LINES.with(|l| l.borrow().clone());
    let asked = ASKED.with(|a| a.borrow().clone());
    let inputs = listing::Inputs { installed: &installed, index: index.as_ref(), provenance: &provenance, lines: &lines, asked: asked.as_deref() };
    listing::rows(&inputs)
        .into_iter()
        .map(|row| {
            let (allowed, revocable) = match manifest_of(&row.id) {
                Some(m) => allowed_sentence(store, &m),
                None => (None, false),
            };
            ShownRow { row, allowed, revocable }
        })
        .collect()
}

/// What plugin `m` was allowed on this device, and whether "Revoke" has
/// anything to take back.
fn allowed_sentence(store: AppStore, m: &Manifest) -> (Option<String>, bool) {
    let approval = approval_of(&m.id);
    let verb = if approval.write { "read and change" } else { "read" };
    match &approval.stores {
        Approved::All => (Some(format!("Allowed to {verb} all your stores on this device.")), true),
        Approved::Only(ids) => {
            let names = ids.iter().map(|id| store_name(store, *id)).collect::<Vec<_>>().join(", ");
            (Some(format!("Allowed to {verb} {names} on this device.")), true)
        }
        Approved::Nothing if m.asks.asks_anything() => (Some("Not allowed anything yet: it asks the first time it is shown.".to_string()), false),
        Approved::Nothing => (Some("It needs nothing allowed.".to_string()), false),
    }
}

/// A row's button was pressed.
fn act(id: &str, action: Action) {
    match action {
        Action::Install | Action::Update | Action::UseLibrary => install_from_library(id),
        Action::TurnOn => set_off(id, false),
        Action::TurnOff => set_off(id, true),
        Action::Remove => place::remove(id),
    }
}

fn toggle_open(ui: PluginsUi, id: &str) {
    ui.open.update(|open| {
        if !open.remove(id) {
            open.insert(id.to_string());
        }
    });
}

/// Settings > Plugins.
pub(crate) fn render_page(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let ui = PluginsUi {
        rows: Signal::new(Vec::new()),
        status: Signal::new(String::new()),
        library_line: Signal::new(String::new()),
        installed_only: Signal::new(false),
        open: Signal::new(HashSet::new()),
    };
    UI.with(|u| u.set(Some(ui)));
    let shown_rows = move || {
        let only = ui.installed_only.get();
        let open = ui.open.get();
        ui.rows.get().into_iter().filter(|r| !only || r.row.installed).map(|r| {
            let is_open = open.contains(&r.row.id);
            RowView::of(r, is_open)
        }).collect::<Vec<_>>()
    };
    rsx! {
        div {
            class: "pimble-plugins",
            style: "display: flex; flex-direction: column; gap: 12px;",
            div {
                style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                {place::WHERE.to_string()}
                " Plugins from pimble.app are signed by Pimble, checked before they are installed, and kept up to date on their own."
            }
            div {
                style: "display: flex; gap: 6px; align-items: center;",
                Button {
                    variant: {move || if ui.installed_only.get() { "default" } else { "filled" }},
                    size: "xs",
                    onclick: move || ui.installed_only.set(false),
                    "All"
                }
                Button {
                    variant: {move || if ui.installed_only.get() { "filled" } else { "default" }},
                    size: "xs",
                    onclick: move || ui.installed_only.set(true),
                    "Installed"
                }
                div { style: "flex: 1;" }
                if place::CAN_ADD {
                    Button { variant: "default", size: "xs", onclick: move || place::pick(), "Add from a File..." }
                }
            }
            if !ui.library_line.get().is_empty() {
                div { style: "font-size: 13px;", {move || ui.library_line.get()} }
            }
            if !ui.status.get().is_empty() {
                div { style: "font-size: 12px;", {move || ui.status.get()} }
            }
            if shown_rows().is_empty() {
                div { style: "font-size: 13px;", "No plugins are installed." }
            }
            for view in shown_rows() {
                div {
                    key: view.key.clone(),
                    class: view.class,
                    style: "display: flex; flex-direction: column; gap: 6px;",
                    div {
                        style: "display: flex; gap: 12px; align-items: center;",
                        div {
                            style: "flex: 1; display: flex; flex-direction: column; gap: 2px;",
                            div { style: "font-size: 13px; font-weight: 600;", {view.title.clone()} }
                            div { style: format!("font-size: 13px; {}", view.summary_style), {view.summary.clone()} }
                            div { style: "font-size: 12px; color: var(--rinch-color-dimmed);", {view.state.clone()} }
                        }
                        div {
                            style: view.button_style,
                            Button {
                                variant: "filled",
                                size: "xs",
                                onclick: {
                                    let button = view.button.clone();
                                    move || if let Some((id, action)) = &button { act(id, *action) }
                                },
                                {view.button_label}
                            }
                        }
                        Button {
                            variant: "subtle",
                            size: "xs",
                            onclick: {
                                let id = view.toggle_id.clone();
                                move || toggle_open(ui, &id)
                            },
                            {view.toggle_label}
                        }
                    }
                    div {
                        class: "pimble-plugins__details",
                        style: format!("display: flex; flex-direction: column; gap: 4px; padding-left: 12px; font-size: 12px; color: var(--rinch-color-dimmed); {}", view.details_style),
                        for line in view.details.clone() {
                            div { key: line.clone(), {line.clone()} }
                        }
                        div {
                            style: "display: flex; gap: 6px; margin-top: 2px;",
                            for (id, action) in view.actions.clone() {
                                Button {
                                    key: action.label().to_string(),
                                    variant: "default",
                                    size: "xs",
                                    onclick: {
                                        let id = id.clone();
                                        move || act(&id, action)
                                    },
                                    {action.label()}
                                }
                            }
                            div {
                                style: view.revoke_style,
                                Button {
                                    variant: "default",
                                    size: "xs",
                                    onclick: {
                                        let revoke = view.revoke.clone();
                                        move || if let Some(id) = &revoke { revoke_approval(store, id) }
                                    },
                                    "Revoke"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Say what came of an add or a remove, in the Plugins window.
pub(super) fn say(line: String) {
    if let Some(ui) = UI.with(Cell::get) {
        ui.status.set(line);
    }
}
