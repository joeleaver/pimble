//! Account > "AI Agents..." (docs/AGENTS_CONTRACT.md section 5): the AI apps
//! connected to this device's Pimble, what each may read or edit, and setting
//! one up in its app.
//!
//! An agent is made and judged by this device's own server (`createAgent`,
//! `Reach`); this window lists them, picks grants on the explorer's own tree
//! (`picker`), shows a new agent's key once, and writes the app's config
//! (`setup`: Claude, ChatGPT / Codex, Cursor) or offers the snippet to paste.
//! Removing an agent takes its entries out of every config Pimble knows.
//! Desktop only: the browser's server keeps no local agents (remote agents,
//! wave 4, are where the browser gets a view).

// The browser keeps no agents (Settings has no AI Agents section there), so
// most of this is the desktop's alone.
#![cfg_attr(not(feature = "native"), allow(dead_code))]

pub mod client_config;
mod picker;
pub mod setup;
mod view;

use std::collections::{BTreeSet, HashMap, HashSet};

use pimble_core::{NodeId, StoreId};
use pimble_rpc::{AgentGrant, AgentInfo, AgentRole};
use rinch::prelude::*;
use uuid::Uuid;

use crate::protocol::BackendCommand;
use crate::state::AppStore;
use client_config::{Format, NewEntry};

// Settings draws the AI Agents page on the desktop only.
#[cfg(feature = "native")]
pub(crate) use view::render_page;

/// The apps Pimble can set up itself, and "another one".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientApp {
    /// The Claude app (its Chat and Code tabs) and `claude` in a terminal:
    /// one agent, one entry in each of their two config files (`setup`).
    Claude,
    /// The ChatGPT desktop app and the Codex CLI and IDE extension, which
    /// share `~/.codex/config.toml` (learn.chatgpt.com/docs/extend/mcp).
    ChatGpt,
    Cursor,
    Other,
}

impl ClientApp {
    pub const ALL: [ClientApp; 4] = [ClientApp::Claude, ClientApp::ChatGpt, ClientApp::Cursor, ClientApp::Other];

    pub fn name(self) -> &'static str {
        match self {
            ClientApp::Claude => "Claude",
            ClientApp::ChatGpt => "ChatGPT / Codex",
            ClientApp::Cursor => "Cursor",
            ClientApp::Other => "Another MCP app",
        }
    }

    fn value(self) -> &'static str {
        match self {
            ClientApp::Claude => "claude",
            ClientApp::ChatGpt => "chatgpt",
            ClientApp::Cursor => "cursor",
            ClientApp::Other => "other",
        }
    }

    fn from_value(v: &str) -> ClientApp {
        ClientApp::ALL.into_iter().find(|c| c.value() == v).unwrap_or(ClientApp::Other)
    }

    /// What adding an agent to this app does, said before it is done.
    fn add_hint(self) -> &'static str {
        match self {
            ClientApp::Claude => {
                "Claude keeps one Pimble agent: this replaces any Pimble entry already in Claude's settings, and an agent it replaces that no other app here uses is removed from Pimble."
            }
            _ => "This replaces an entry of the same name in its settings.",
        }
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
    pub agents: Signal<Vec<AgentInfo>>,
    pub editing: Signal<Option<Editing>>,
    /// A just-made agent and its key, shown once, with the app it is for.
    pub created: Signal<Option<(AgentInfo, String, ClientApp)>>,
    pub error: Signal<String>,
    /// What setting up the app did, in a sentence.
    pub setup: Signal<String>,
    /// What the last removal did to the apps' settings, under the list.
    pub notice: Signal<String>,
    /// Which apps each agent is set up in, by agent id.
    pub placed: Signal<HashMap<Uuid, Vec<ClientApp>>>,
    /// Agents asked to be removed and not yet gone from the list: their
    /// entries come out of the apps' settings once the server says so.
    pub removing: Signal<BTreeSet<Uuid>>,
    pub busy: Signal<bool>,
    /// The picker's expanded rows.
    pub expanded: Signal<HashSet<(StoreId, NodeId)>>,
}

impl AgentsUi {
    pub fn new() -> Self {
        Self {
            agents: Signal::new(Vec::new()),
            editing: Signal::new(None),
            created: Signal::new(None),
            error: Signal::new(String::new()),
            setup: Signal::new(String::new()),
            notice: Signal::new(String::new()),
            placed: Signal::new(HashMap::new()),
            removing: Signal::new(BTreeSet::new()),
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

/// Account > "AI Agents...": Settings at AI Agents.
pub fn open(store: AppStore) {
    crate::settings::open_at(store, crate::settings::Section::Agents);
}

/// Settings > AI Agents is shown: the list, asked for again.
pub(crate) fn shown(store: AppStore) {
    let ui = store.agents_ui;
    ui.editing.set(None);
    ui.created.set(None);
    ui.error.set(String::new());
    ui.setup.set(String::new());
    ui.notice.set(String::new());
    ui.busy.set(true);
    store.send(BackendCommand::ListAgents);
}

/// Settings closed: a key shown once is not shown again.
pub(crate) fn hidden(store: AppStore) {
    let ui = store.agents_ui;
    ui.created.set(None);
    ui.editing.set(None);
}

// ── Events ───────────────────────────────────────────────────────────────

pub(crate) fn listed(store: AppStore, agents: &[AgentInfo]) {
    let ui = store.agents_ui;
    ui.busy.set(false);
    let listed: HashSet<Uuid> = agents.iter().filter_map(|a| Uuid::parse_str(&a.id).ok()).collect();
    let gone: Vec<Uuid> = untracked(|| ui.removing.get()).into_iter().filter(|id| !listed.contains(id)).collect();
    if !gone.is_empty() {
        ui.removing.update(|r| r.retain(|id| listed.contains(id)));
        ui.notice.set(forget(&gone));
    }
    ui.placed.set(placements());
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
    ui.notice.set(String::new());
    ui.created.set(Some((agent.clone(), key.to_string(), app)));
}

pub(crate) fn failed(store: AppStore, message: &str) {
    let ui = store.agents_ui;
    ui.busy.set(false);
    // `removing` is left as it is: an agent whose removal failed is still
    // listed, so `listed` never takes its entries out, and another removal
    // waiting for its answer still gets its entries taken out.
    ui.error.set(message.to_string());
}

/// "Remove": the server revokes the key; its entries come out of the apps'
/// settings when the list says it is gone (`listed`).
fn remove(store: AppStore, id: &str) {
    let ui = store.agents_ui;
    if let Ok(uuid) = Uuid::parse_str(id) {
        ui.removing.update(|r| {
            r.insert(uuid);
        });
    }
    ui.notice.set(String::new());
    ui.busy.set(true);
    store.send(BackendCommand::RemoveAgent { id: id.to_string() });
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

fn last_used(agent: &AgentInfo) -> String {
    match &agent.last_used_at {
        Some(t) => format!("Last used {}", t.get(..10).unwrap_or(t)),
        None => "Not used yet".to_string(),
    }
}

/// The list's rows: each agent with the line saying where it is set up. Both
/// signals are read here, so a row is drawn again when either changes.
fn listed_rows(store: AppStore) -> Vec<(AgentInfo, String)> {
    let ui = store.agents_ui;
    let placed = ui.placed.get();
    ui.agents
        .get()
        .into_iter()
        .map(|a| {
            let line = setup::placed_line(Uuid::parse_str(&a.id).ok().and_then(|id| placed.get(&id)));
            (a, line)
        })
        .collect()
}

// ── Setting up the app ───────────────────────────────────────────────────

/// The installed `pimble-mcp`, beside this app.
fn mcp_path() -> String {
    let exe = if cfg!(windows) { "pimble-mcp.exe" } else { "pimble-mcp" };
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(exe)))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| exe.to_string())
}

/// The whole snippet to paste, in the app's own format.
pub fn snippet(app: ClientApp, agent_name: &str, key: &str) -> String {
    let name = client_config::entry_name(agent_name);
    let mcp = mcp_path();
    let format = if app == ClientApp::ChatGpt { Format::Toml } else { Format::Json };
    client_config::snippet(format, &NewEntry { name: &name, command: &mcp, key, stdio_type: false })
}

/// "Add to <app>": write the agent into the app's config, remove from Pimble
/// the agents it replaced that no app here still uses, and say what happened.
#[cfg(feature = "native")]
fn set_up(store: AppStore) {
    let ui = store.agents_ui;
    let Some((agent, key, app)) = untracked(|| ui.created.get()) else { return };
    let (Some(at), Ok(id)) = (setup::Locations::here(), Uuid::parse_str(&agent.id)) else { return };
    let (name, mcp) = (client_config::entry_name(&agent.name), mcp_path());
    let added = setup::add(&at, app, NewEntry { name: &name, command: &mcp, key: &key, stdio_type: false }, id);
    let held = setup::holders(&at);
    let agents: Vec<(Uuid, String)> = untracked(|| ui.agents.get()).into_iter().filter_map(|a| Uuid::parse_str(&a.id).ok().map(|id| (id, a.name))).collect();
    let settled = setup::settle(app, &added, &held, &agents);
    for old in &settled.remove {
        ui.removing.update(|r| {
            r.insert(*old);
        });
        store.send(BackendCommand::RemoveAgent { id: old.to_string() });
    }
    ui.placed.set(held.by_agent);
    ui.setup.set(setup::added_sentence(app, &added, &settled));
}

#[cfg(not(feature = "native"))]
fn set_up(_store: AppStore) {}

/// Which apps each agent is set up in.
#[cfg(feature = "native")]
fn placements() -> HashMap<Uuid, Vec<ClientApp>> {
    setup::Locations::here().map(|at| setup::holders(&at).by_agent).unwrap_or_default()
}

#[cfg(not(feature = "native"))]
fn placements() -> HashMap<Uuid, Vec<ClientApp>> {
    HashMap::new()
}

/// Take removed agents out of every app's settings, and say what that did.
#[cfg(feature = "native")]
fn forget(gone: &[Uuid]) -> String {
    let Some(at) = setup::Locations::here() else { return String::new() };
    let mut apps: Vec<ClientApp> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    for id in gone {
        let out = setup::forget(&at, *id);
        for file in out.changed {
            if !apps.contains(&file.app()) {
                apps.push(file.app());
            }
        }
        failed.extend(out.failed);
    }
    let mut out = Vec::new();
    if !apps.is_empty() {
        out.push(format!("Taken out of {}'s settings.", setup::app_list(&apps)));
    }
    for e in failed {
        out.push(format!("{e}: take the agent out of it yourself."));
    }
    out.join(" ")
}

#[cfg(not(feature = "native"))]
fn forget(_gone: &[Uuid]) -> String {
    String::new()
}
