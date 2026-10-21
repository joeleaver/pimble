//! Account > "AI Agents..." (docs/AGENTS_CONTRACT.md section 5): the AI apps
//! connected to this device's Pimble, what each may read or edit, and setting
//! one up in its app.
//!
//! An agent is made and judged by this device's own server (`createAgent`,
//! `Reach`); this window lists them, picks grants on the explorer's own tree,
//! shows a new agent's key once, and writes the app's config (Claude Desktop,
//! Claude Code, Cursor) or offers the snippet to paste. Desktop only: the
//! browser's server keeps no local agents (remote agents, wave 4, are where the
//! browser gets a view).

use std::collections::{HashMap, HashSet};

use pimble_core::{NodeId, StoreId};
use pimble_rpc::{AgentGrant, AgentInfo, AgentRole};
use rinch::prelude::*;

use crate::protocol::BackendCommand;
use crate::state::AppStore;

/// The apps Pimble can set up itself, and "another one".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientApp {
    ClaudeDesktop,
    ClaudeCode,
    Cursor,
    Other,
}

impl ClientApp {
    pub const ALL: [ClientApp; 4] = [ClientApp::ClaudeDesktop, ClientApp::ClaudeCode, ClientApp::Cursor, ClientApp::Other];

    pub fn name(self) -> &'static str {
        match self {
            ClientApp::ClaudeDesktop => "Claude Desktop",
            ClientApp::ClaudeCode => "Claude Code",
            ClientApp::Cursor => "Cursor",
            ClientApp::Other => "Another MCP app",
        }
    }

    fn value(self) -> &'static str {
        match self {
            ClientApp::ClaudeDesktop => "claude-desktop",
            ClientApp::ClaudeCode => "claude-code",
            ClientApp::Cursor => "cursor",
            ClientApp::Other => "other",
        }
    }

    fn from_value(v: &str) -> ClientApp {
        ClientApp::ALL.into_iter().find(|c| c.value() == v).unwrap_or(ClientApp::Other)
    }
}

/// A grant being chosen: a store (`None`) or a node's subtree, at a role.
pub type Choice = HashMap<(StoreId, Option<NodeId>), AgentRole>;

/// The agent being made or changed.
#[derive(Clone, Debug, PartialEq)]
pub struct Editing {
    /// `None` for a new agent.
    pub id: Option<String>,
    pub name: String,
    pub app: ClientApp,
    pub grants: Choice,
}

/// The window's state, one per window.
#[derive(Clone, Copy)]
pub struct AgentsUi {
    pub open: Signal<bool>,
    pub agents: Signal<Vec<AgentInfo>>,
    pub editing: Signal<Option<Editing>>,
    /// A just-made agent and its key, shown once, with the app it is for.
    pub created: Signal<Option<(AgentInfo, String, ClientApp)>>,
    pub error: Signal<String>,
    /// What setting up the app did, in a sentence.
    pub setup: Signal<String>,
    pub busy: Signal<bool>,
    /// The picker's expanded rows.
    pub expanded: Signal<HashSet<(StoreId, NodeId)>>,
}

impl AgentsUi {
    pub fn new() -> Self {
        Self {
            open: Signal::new(false),
            agents: Signal::new(Vec::new()),
            editing: Signal::new(None),
            created: Signal::new(None),
            error: Signal::new(String::new()),
            setup: Signal::new(String::new()),
            busy: Signal::new(false),
            expanded: Signal::new(HashSet::new()),
        }
    }
}

impl Default for AgentsUi {
    fn default() -> Self {
        Self::new()
    }
}

/// Account > "AI Agents...".
pub fn open(store: AppStore) {
    let ui = store.agents_ui;
    ui.editing.set(None);
    ui.created.set(None);
    ui.error.set(String::new());
    ui.setup.set(String::new());
    ui.busy.set(true);
    ui.open.set(true);
    store.send(BackendCommand::ListAgents);
}

// ── Events ───────────────────────────────────────────────────────────────

pub(crate) fn listed(store: AppStore, agents: &[AgentInfo]) {
    let ui = store.agents_ui;
    ui.busy.set(false);
    ui.agents.set(agents.to_vec());
    ui.editing.set(None);
    ui.error.set(String::new());
}

pub(crate) fn created(store: AppStore, agent: &AgentInfo, key: &str) {
    let ui = store.agents_ui;
    ui.busy.set(false);
    let app = untracked(|| ui.editing.get()).map(|e| e.app).unwrap_or(ClientApp::Other);
    ui.editing.set(None);
    ui.agents.update(|list| list.push(agent.clone()));
    ui.setup.set(String::new());
    ui.created.set(Some((agent.clone(), key.to_string(), app)));
}

pub(crate) fn failed(store: AppStore, message: &str) {
    let ui = store.agents_ui;
    ui.busy.set(false);
    ui.error.set(message.to_string());
}

// ── Grants ───────────────────────────────────────────────────────────────

/// The grants an agent's choice is saved as.
pub fn grants_of(choice: &Choice) -> Vec<AgentGrant> {
    choice.iter().map(|(&(store, node), &role)| AgentGrant { store, node, role }).collect()
}

fn choice_of(grants: &[AgentGrant], store: AppStore) -> Choice {
    grants
        .iter()
        .map(|g| {
            // A store's root node stands for the whole store in the picker.
            let node = g.node.filter(|n| store.root_node_id(g.store) != Some(*n));
            ((g.store, node), g.role)
        })
        .collect()
}

/// What a row inherits from above it: the strongest role chosen on its store
/// or on any ancestor (an agent reaches the union of its grants).
fn inherited(store: AppStore, choice: &Choice, store_id: StoreId, node: Option<NodeId>) -> Option<AgentRole> {
    let mut best = node.and_then(|_| choice.get(&(store_id, None)).copied());
    let root = store.root_node_id(store_id);
    let mut at = node.and_then(|n| store.get_node_signal(store_id, n)).and_then(|s| untracked(|| s.with(|n| n.parent_id)));
    let mut guard = 0;
    while let Some(parent) = at {
        guard += 1;
        if guard > 256 || Some(parent) == root {
            break;
        }
        if let Some(role) = choice.get(&(store_id, Some(parent))) {
            best = best.max(Some(*role));
        }
        at = store.get_node_signal(store_id, parent).and_then(|s| untracked(|| s.with(|n| n.parent_id)));
    }
    best
}

/// An agent's reach in a line: "Reads Work, edits Work/Drafts".
fn summary(store: AppStore, grants: &[AgentGrant]) -> String {
    let name = |g: &AgentGrant| {
        let store_name = store.get_store_signal(g.store).map(|s| untracked(|| s.with(|s| s.name.clone()))).unwrap_or_else(|| "a store not open here".into());
        match g.node.filter(|n| store.root_node_id(g.store) != Some(*n)) {
            Some(n) => format!("{store_name}/{}", store.display_label(g.store, n)),
            None => store_name,
        }
    };
    let reads: Vec<String> = grants.iter().filter(|g| g.role == AgentRole::Reader).map(name).collect();
    let edits: Vec<String> = grants.iter().filter(|g| g.role == AgentRole::Editor).map(name).collect();
    match (reads.is_empty(), edits.is_empty()) {
        (true, true) => "Reaches nothing yet".into(),
        (false, true) => format!("Reads {}", reads.join(", ")),
        (true, false) => format!("Edits {}", edits.join(", ")),
        (false, false) => format!("Reads {}; edits {}", reads.join(", "), edits.join(", ")),
    }
}

// ── The picker's rows ────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
struct Row {
    store: StoreId,
    /// `None` for the store's own row.
    node: Option<NodeId>,
    depth: usize,
    expanded: bool,
}

fn row_label(store: AppStore, row: Row) -> String {
    match row.node {
        Some(n) => store.display_label(row.store, n),
        None => store.get_store_signal(row.store).map(|s| untracked(|| s.with(|s| s.name.clone()))).unwrap_or_default(),
    }
}

fn rows(store: AppStore, expanded: &HashSet<(StoreId, NodeId)>) -> Vec<Row> {
    let mut out = Vec::new();
    for store_id in store.store_ids.get() {
        let Some(sig) = store.get_store_signal(store_id) else { continue };
        let root = sig.with(|s| s.root_node_id);
        let open = expanded.contains(&(store_id, root));
        out.push(Row { store: store_id, node: None, depth: 0, expanded: open });
        if open {
            push_children(store, expanded, store_id, root, 1, &mut out);
        }
    }
    out
}

fn push_children(store: AppStore, expanded: &HashSet<(StoreId, NodeId)>, store_id: StoreId, parent: NodeId, depth: usize, out: &mut Vec<Row>) {
    if depth > 24 {
        return;
    }
    // Tracked, so children that arrive after the row opened are drawn.
    let Some(children) = store.children_of.with(|m| m.get(&(store_id, parent)).copied()) else { return };
    for (child_store, child) in children.get() {
        let open = expanded.contains(&(child_store, child));
        out.push(Row { store: child_store, node: Some(child), depth, expanded: open });
        if open {
            push_children(store, expanded, child_store, child, depth + 1, out);
        }
    }
}

fn toggle_row(store: AppStore, store_id: StoreId, node: Option<NodeId>) {
    let ui = store.agents_ui;
    let key = match node {
        Some(n) => (store_id, n),
        None => match store.root_node_id(store_id) {
            Some(root) => (store_id, root),
            None => return,
        },
    };
    let opening = !untracked(|| ui.expanded.with(|e| e.contains(&key)));
    if opening && !store.has_children_loaded(key.0, key.1) {
        store.send(BackendCommand::GetChildren { store_id: key.0, node_id: key.1 });
    }
    ui.expanded.update(|e| {
        if opening {
            e.insert(key);
        } else {
            e.remove(&key);
        }
    });
}

fn set_role(store: AppStore, store_id: StoreId, node: Option<NodeId>, role: Option<AgentRole>) {
    store.agents_ui.editing.update(|e| {
        if let Some(e) = e {
            match role {
                Some(role) => {
                    e.grants.insert((store_id, node), role);
                }
                None => {
                    e.grants.remove(&(store_id, node));
                }
            }
        }
    });
}

/// A row's three role buttons: label, role, and the row they set.
fn role_buttons(store_id: StoreId, node: Option<NodeId>) -> [(&'static str, Option<AgentRole>, StoreId, Option<NodeId>); 3] {
    [
        ("None", None, store_id, node),
        ("Read", Some(AgentRole::Reader), store_id, node),
        ("Edit", Some(AgentRole::Editor), store_id, node),
    ]
}

fn role_tip(role: Option<AgentRole>) -> &'static str {
    match role {
        None => "Not chosen here",
        Some(AgentRole::Reader) => "It may read this and everything inside",
        Some(AgentRole::Editor) => "It may read and change this and everything inside",
    }
}

fn role_style(store: AppStore, store_id: StoreId, node: Option<NodeId>, role: Option<AgentRole>) -> String {
    let own = store.agents_ui.editing.get().and_then(|e| e.grants.get(&(store_id, node)).copied());
    let on = own == role;
    format!("{ROLE_BTN}{}", if on { " background: var(--rinch-primary-color); color: white; border-color: var(--rinch-primary-color);" } else { "" })
}

// ── Setting up the app ───────────────────────────────────────────────────

/// The name an agent's entry takes in an app's config: `pimble-<name>`.
pub fn entry_name(agent_name: &str) -> String {
    let slug: String = agent_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() { "pimble".into() } else { format!("pimble-{slug}") }
}

/// The installed `pimble-mcp`, beside this app.
fn mcp_path() -> String {
    let exe = if cfg!(windows) { "pimble-mcp.exe" } else { "pimble-mcp" };
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(exe)))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| exe.to_string())
}

/// The `mcpServers` entry for an agent.
pub fn entry_json(app: ClientApp, mcp: &str, key: &str) -> serde_json::Value {
    let mut entry = serde_json::json!({ "command": mcp, "env": { "PIMBLE_AGENT_KEY": key } });
    if app == ClientApp::ClaudeCode {
        entry["type"] = serde_json::json!("stdio");
    }
    entry
}

/// The whole snippet to paste, for an app Pimble does not set up itself.
pub fn snippet(app: ClientApp, agent_name: &str, key: &str) -> String {
    let config = serde_json::json!({ "mcpServers": { entry_name(agent_name): entry_json(app, &mcp_path(), key) } });
    serde_json::to_string_pretty(&config).unwrap_or_default()
}

/// Where an app keeps its MCP servers.
#[cfg(feature = "native")]
fn config_path(app: ClientApp) -> Option<std::path::PathBuf> {
    match app {
        // `config_dir` is ~/.config, %APPDATA% and ~/Library/Application Support.
        ClientApp::ClaudeDesktop => dirs::config_dir().map(|d| d.join("Claude").join("claude_desktop_config.json")),
        ClientApp::ClaudeCode => dirs::home_dir().map(|h| h.join(".claude.json")),
        ClientApp::Cursor => dirs::home_dir().map(|h| h.join(".cursor").join("mcp.json")),
        ClientApp::Other => None,
    }
}

/// Add an agent's entry to the app's config file at `path`, touching nothing
/// else: the file is read, the entry merged in under `mcpServers`, the old
/// file kept beside it as `.pimble-backup`, and the result written in one
/// rename. A file that does not parse is left alone.
pub fn write_entry(path: &std::path::Path, entry_name: &str, entry: serde_json::Value) -> Result<(), String> {
    let mut config = match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => serde_json::json!({}),
        Ok(text) => {
            let parsed: serde_json::Value = serde_json::from_str(&text).map_err(|_| format!("{} is not JSON Pimble can read, so it was left alone", path.display()))?;
            std::fs::write(backup_path(path), &text).map_err(|e| format!("Could not keep a backup of {}: {e}", path.display()))?;
            parsed
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(format!("Could not read {}: {e}", path.display())),
    };
    let Some(object) = config.as_object_mut() else {
        return Err(format!("{} is not a settings object, so it was left alone", path.display()));
    };
    let servers = object.entry("mcpServers").or_insert_with(|| serde_json::json!({}));
    let Some(servers) = servers.as_object_mut() else {
        return Err(format!("{}'s mcpServers is not a list of servers, so it was left alone", path.display()));
    };
    servers.insert(entry_name.to_string(), entry);
    let text = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("Could not make {}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("pimble-tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("Could not write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("Could not write {}: {e}", path.display()))
}

fn backup_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".pimble-backup");
    std::path::PathBuf::from(name)
}

/// "Add to <app>": write the agent into the app's config, and say what
/// happened.
#[cfg(feature = "native")]
fn set_up(store: AppStore) {
    let ui = store.agents_ui;
    let Some((agent, key, app)) = untracked(|| ui.created.get()) else { return };
    let Some(path) = config_path(app) else { return };
    let sentence = match write_entry(&path, &entry_name(&agent.name), entry_json(app, &mcp_path(), &key)) {
        Ok(()) => format!("Added to {}. Restart {} to connect it.", app.name(), app.name()),
        Err(e) => format!("{e}. Copy the setup below into {} instead.", app.name()),
    };
    ui.setup.set(sentence);
}

#[cfg(not(feature = "native"))]
fn set_up(_store: AppStore) {}

// ── The window ───────────────────────────────────────────────────────────

fn last_used(agent: &AgentInfo) -> String {
    match &agent.last_used_at {
        Some(t) => format!("Last used {}", t.get(..10).unwrap_or(t)),
        None => "Not used yet".to_string(),
    }
}

/// "reads, from above" on a row nothing is chosen on that inherits a role.
fn from_above(store: AppStore, store_id: StoreId, node: Option<NodeId>) -> String {
    let Some(e) = store.agents_ui.editing.get() else { return String::new() };
    match (e.grants.get(&(store_id, node)), inherited(store, &e.grants, store_id, node)) {
        (None, Some(AgentRole::Reader)) => "reads, from above".into(),
        (None, Some(AgentRole::Editor)) => "edits, from above".into(),
        _ => String::new(),
    }
}

fn chevron(row: Row) -> &'static str {
    if row.expanded { "▾" } else { "▸" }
}

const ROLE_BTN: &str = "padding: 2px 8px; border-radius: 4px; font-size: 12px; cursor: pointer; border: 1px solid var(--rinch-color-border);";

pub(crate) fn render_modal(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let ui = store.agents_ui;
    let listing = move || ui.editing.get().is_none() && ui.created.get().is_none();
    let save = move || {
        let Some(e) = untracked(|| ui.editing.get()) else { return };
        if e.name.trim().is_empty() {
            ui.error.set("Name the agent: the app it is for, or what it does.".into());
            return;
        }
        if e.grants.is_empty() {
            ui.error.set("Choose what it may read or edit: nothing is chosen yet.".into());
            return;
        }
        ui.error.set(String::new());
        ui.busy.set(true);
        let grants = grants_of(&e.grants);
        store.send(match e.id {
            Some(id) => BackendCommand::UpdateAgent { id, name: e.name.trim().to_string(), grants },
            None => BackendCommand::CreateAgent { name: e.name.trim().to_string(), grants },
        });
    };

    rsx! {
        Modal {
            opened_fn: move || ui.open.get(),
            onclose: move || {
                ui.open.set(false);
                ui.created.set(None);
                ui.editing.set(None);
            },
            title: "AI Agents",
            size: "lg",

            div {
                class: "pimble-agents",

                // ── The list ──
                div {
                    style: {move || if listing() { "display: flex; flex-direction: column; gap: 10px;" } else { "display: none;" }},
                    div {
                        class: "pimble-agents__intro",
                        "Apps like Claude Desktop connect to Pimble as agents. Each one reads or edits only the parts of your notes you choose, on this computer."
                    }
                    for agent in ui.agents.get() {
                        div {
                            key: agent.id.clone(),
                            class: "pimble-agents__row",
                            div {
                                style: "flex: 1; min-width: 0;",
                                div { class: "pimble-agents__name", {agent.name.clone()} }
                                div { class: "pimble-agents__reach", {summary(store, &agent.grants)} }
                                div {
                                    class: "pimble-agents__reach",
                                    {last_used(&agent)}
                                }
                            }
                            Button {
                                size: "xs",
                                variant: "light",
                                onclick: {
                                    let agent = agent.clone();
                                    move || {
                                        ui.error.set(String::new());
                                        ui.editing.set(Some(Editing {
                                            id: Some(agent.id.clone()),
                                            name: agent.name.clone(),
                                            app: ClientApp::Other,
                                            grants: choice_of(&agent.grants, store),
                                        }));
                                    }
                                },
                                "Edit"
                            }
                            Button {
                                size: "xs",
                                variant: "subtle",
                                color: "red",
                                onclick: {
                                    let id = agent.id.clone();
                                    move || {
                                        ui.busy.set(true);
                                        store.send(BackendCommand::RemoveAgent { id: id.clone() });
                                    }
                                },
                                "Remove"
                            }
                        }
                    }
                    div {
                        style: {move || if ui.agents.get().is_empty() && !ui.busy.get() { "font-size: 12px; color: var(--rinch-color-dimmed);" } else { "display: none;" }},
                        "No agents yet."
                    }
                    Button {
                        size: "sm",
                        variant: "filled",
                        onclick: move || {
                            ui.error.set(String::new());
                            ui.editing.set(Some(Editing { id: None, name: ClientApp::ClaudeDesktop.name().to_string(), app: ClientApp::ClaudeDesktop, grants: Choice::new() }));
                        },
                        "New Agent"
                    }
                }

                // ── Making or changing one ──
                div {
                    style: {move || if ui.editing.get().is_some() { "display: flex; flex-direction: column; gap: 10px;" } else { "display: none;" }},
                    TextInput {
                        label: "Name",
                        placeholder: "Claude Desktop",
                        value_fn: move || ui.editing.get().map(|e| e.name).unwrap_or_default(),
                        oninput: move |val: String| ui.editing.update(|e| {
                            if let Some(e) = e {
                                e.name = val;
                            }
                        }),
                    }
                    div {
                        style: {move || if ui.editing.get().is_some_and(|e| e.id.is_none()) { "" } else { "display: none;" }},
                        Select {
                            label: "For",
                            value_fn: move || ui.editing.get().map(|e| e.app.value().to_string()).unwrap_or_default(),
                            onchange: move |val: String| ui.editing.update(|e| {
                                if let Some(e) = e {
                                    e.app = ClientApp::from_value(&val);
                                    if e.name.is_empty() || ClientApp::ALL.iter().any(|a| a.name() == e.name) {
                                        e.name = e.app.name().to_string();
                                    }
                                }
                            }),
                            data: {|| ClientApp::ALL.iter().map(|a| SelectOption::new(a.value(), a.name())).collect::<Vec<_>>()},
                        }
                    }
                    div { class: "pimble-agents__label", "What it can reach" }
                    div {
                        class: "pimble-agents__hint",
                        "A choice covers everything inside; a row inside can only add to it. Nothing is chosen to start with."
                    }
                    div {
                        class: "pimble-agents__picker",
                        for row in rows(store, &ui.expanded.get()) {
                            div {
                                key: format!("{}:{:?}", row.store, row.node),
                                class: "pimble-agents__pick",
                                style: format!("padding-left: {}px;", 4 + row.depth * 16),
                                div {
                                    class: "pimble-agents__chevron",
                                    onclick: move || toggle_row(store, row.store, row.node),
                                    {chevron(row)}
                                }
                                div {
                                    style: "flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                    {row_label(store, row)}
                                }
                                div {
                                    class: "pimble-agents__from",
                                    {move || from_above(store, row.store, row.node)}
                                }
                                for (label, role, rs, rn) in role_buttons(row.store, row.node) {
                                    div {
                                        key: label,
                                        title: role_tip(role),
                                        style: move || role_style(store, rs, rn, role),
                                        onclick: move || set_role(store, rs, rn, role),
                                        {label}
                                    }
                                }
                            }
                        }
                    }
                    div {
                        style: "display: flex; gap: 8px; justify-content: flex-end;",
                        Button {
                            size: "sm",
                            variant: "subtle",
                            onclick: move || {
                                ui.error.set(String::new());
                                ui.editing.set(None);
                            },
                            "Cancel"
                        }
                        Button {
                            size: "sm",
                            variant: "filled",
                            loading: {|| ui.busy.get()},
                            onclick: save,
                            {move || if ui.editing.get().is_some_and(|e| e.id.is_some()) { "Save" } else { "Make Agent" }}
                        }
                    }
                }

                // ── A new agent, set up ──
                div {
                    style: {move || if ui.created.get().is_some() { "display: flex; flex-direction: column; gap: 10px;" } else { "display: none;" }},
                    div {
                        class: "pimble-agents__name",
                        {move || ui.created.get().map(|(a, _, _)| format!("{} is ready.", a.name)).unwrap_or_default()}
                    }
                    div {
                        class: "pimble-agents__hint",
                        "Its key is shown this once. Pimble keeps only a fingerprint of it; if it is lost, remove the agent and make another."
                    }
                    div {
                        style: {move || match ui.created.get() {
                            Some((_, _, app)) if app != ClientApp::Other && cfg!(feature = "native") => "",
                            _ => "display: none;",
                        }},
                        Button {
                            size: "sm",
                            variant: "filled",
                            onclick: move || set_up(store),
                            {move || ui.created.get().map(|(_, _, app)| format!("Add to {}", app.name())).unwrap_or_default()}
                        }
                    }
                    div {
                        style: {move || if ui.setup.get().is_empty() { "display: none;" } else { "font-size: 13px;" }},
                        {move || ui.setup.get()}
                    }
                    div { class: "pimble-agents__label", "Or add it to the app's MCP settings yourself:" }
                    pre {
                        class: "pimble-agents__snippet",
                        {move || ui.created.get().map(|(a, key, app)| snippet(app, &a.name, &key)).unwrap_or_default()}
                    }
                    div {
                        style: "display: flex; gap: 8px; justify-content: flex-end;",
                        Button {
                            size: "sm",
                            variant: "light",
                            onclick: move || {
                                if let Some((a, key, app)) = untracked(|| ui.created.get()) {
                                    let _ = crate::links::copy_text(&snippet(app, &a.name, &key));
                                    ui.setup.set("Copied.".into());
                                }
                            },
                            "Copy Setup"
                        }
                        Button {
                            size: "sm",
                            variant: "filled",
                            onclick: move || {
                                ui.created.set(None);
                                ui.setup.set(String::new());
                            },
                            "Done"
                        }
                    }
                }

                div {
                    style: {move || if ui.error.get().is_empty() { "display: none;" } else { "color: var(--rinch-color-red-6); font-size: 12px; margin-top: 8px;" }},
                    {move || ui.error.get()}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_joins_an_apps_config_and_leaves_the_rest_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude_desktop_config.json");
        std::fs::write(&path, r#"{ "theme": "dark", "mcpServers": { "other": { "command": "x" } } }"#).unwrap();
        write_entry(&path, "pimble-claude", entry_json(ClientApp::ClaudeDesktop, "/opt/pimble-mcp", "pmbl_agent_k")).unwrap();
        let config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config["theme"], "dark");
        assert_eq!(config["mcpServers"]["other"]["command"], "x");
        assert_eq!(config["mcpServers"]["pimble-claude"]["command"], "/opt/pimble-mcp");
        assert_eq!(config["mcpServers"]["pimble-claude"]["env"]["PIMBLE_AGENT_KEY"], "pmbl_agent_k");
        assert!(config["mcpServers"]["pimble-claude"].get("type").is_none());
        let backup = std::fs::read_to_string(backup_path(&path)).unwrap();
        assert!(backup.contains("\"theme\""), "the old file is kept");
    }

    #[test]
    fn a_missing_config_is_made_and_a_broken_one_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("mcp.json");
        write_entry(&path, "pimble-x", entry_json(ClientApp::ClaudeCode, "m", "k")).unwrap();
        let config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config["mcpServers"]["pimble-x"]["type"], "stdio");

        let broken = dir.path().join("broken.json");
        std::fs::write(&broken, "{ not json").unwrap();
        assert!(write_entry(&broken, "pimble-x", entry_json(ClientApp::Cursor, "m", "k")).is_err());
        assert_eq!(std::fs::read_to_string(&broken).unwrap(), "{ not json", "untouched");
    }

    #[test]
    fn entry_names_are_tidy() {
        assert_eq!(entry_name("Claude Desktop"), "pimble-claude-desktop");
        assert_eq!(entry_name("  Work GPT!! "), "pimble-work-gpt");
        assert_eq!(entry_name("✨"), "pimble");
    }
}
