//! Plugins in the desktop app (docs/PLUGINS_CONTRACT.md waves 1 and 2): the
//! plugins folder, what the person has allowed each plugin on this device, the
//! viewer a pane mounts for a metanode, and the plugin's calls through the
//! Pimble API.
//!
//! Plugins are `.wasm` files in `<data dir>/pimble/plugins/`, put there by hand
//! (decision 3). They are compiled and asked to describe themselves once, off the UI
//! thread, when the app starts; a pane opening a metanode whose viewer one of them
//! provides mounts that plugin's view in the pane, and every other metanode keeps
//! the outline (`crate::metanodes`). A plugin refused at load (built for another
//! ABI) leaves its viewers' metanodes on the outline with the sentence saying why;
//! a plugin stopped while it runs (too long, refused, trapped) does the same, and
//! is not started again while the pane shows that metanode.
//!
//! **Approval (decision 2, taken 2026-11-02).** A plugin's manifest asks for what
//! it needs (which stores: all, or chosen at approval; read or write). The first
//! time it would be shown, the pane shows the outline under the sentence saying
//! what it asks for and "Allow" / "Not now"; "Allow" is remembered on this device
//! in `<data dir>/pimble/plugins/approvals.json` (beside the plugins themselves:
//! per device, never synced), and a plugin without approval mounts nothing.
//! Account > "Plugins..." lists the plugins and takes an approval back.
//!
//! **The API.** A mounted plugin's calls are checked against its approval by the
//! host (`pimble_plugin_host::bridge`) and sent to the backend thread as
//! `BackendCommand::PluginCall`, where they are made through the window's own
//! `Pimble` backend (so the server judges them with the person's role, and the
//! plugin is granted nothing more); answers come back as
//! `BackendEvent::PluginReply` and are handed to the instance. The UI thread has
//! no second API instance.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Weak;
use std::sync::OnceLock;

use pimble_api::{Addr, Forest};
use pimble_crdt::NodeDoc;
use pimble_plugin_host::abi::api::ApiReply;
use pimble_plugin_host::abi::{Manifest, NodeAddr, Props, StoreAsk};
use pimble_plugin_host::bridge::asking_sentence;
use pimble_plugin_host::wasmtime_engine::{Registry, ViewerLookup, DEFAULT_BUDGET};
use pimble_plugin_host::{ApiSide, ApplierOptions, Approval, Approved, Instance, InstanceOptions};
use rinch::core::dom::{DomDocument, NodeId};
use rinch::prelude::*;

use crate::panes::PaneId;
use crate::protocol::BackendCommand;
use crate::state::AppStore;

/// Where plugins are installed on this device.
pub fn plugins_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("pimble").join("plugins")
}

/// Where this device remembers what each plugin was allowed.
fn approvals_path() -> PathBuf {
    plugins_dir().join("approvals.json")
}

/// The plugins folder as loaded at start; unset until loading ends.
static REGISTRY: OnceLock<Registry> = OnceLock::new();

/// A pane's plugin area: the element a plugin's view is applied under.
struct Slot {
    doc: Weak<RefCell<dyn DomDocument>>,
    node: NodeId,
}

/// What a pane's plugin area holds.
struct Shown {
    metanode: Addr,
    viewer: String,
    plugin_id: String,
    /// The running plugin; `None` while it waits for approval, once it stopped,
    /// or after "Not now".
    instance: Option<Instance>,
    /// The props it was last handed.
    props: Props,
    /// Why it stopped, while the pane still shows that metanode.
    stopped: Option<String>,
    /// "Not now" was chosen for this metanode in this pane.
    declined: bool,
}

thread_local! {
    static STORE: Cell<Option<AppStore>> = const { Cell::new(None) };
    static SLOTS: RefCell<HashMap<PaneId, Slot>> = RefCell::new(HashMap::new());
    static SHOWN: RefCell<HashMap<PaneId, Shown>> = RefCell::new(HashMap::new());
    /// What each plugin was allowed, by plugin id: `approvals.json`.
    static APPROVALS: RefCell<HashMap<String, Approval>> = RefCell::new(HashMap::new());
    /// The Plugins window.
    static UI: Cell<Option<PluginsUi>> = const { Cell::new(None) };
}

/// Load the plugins folder off the UI thread, and the approvals on it; panes
/// showing a metanode look again when it is done.
pub(crate) fn load(store: AppStore) {
    STORE.with(|s| s.set(Some(store)));
    APPROVALS.with(|a| *a.borrow_mut() = read_approvals());
    let dir = plugins_dir();
    let spawned = std::thread::Builder::new().name("pimble-plugins".into()).spawn(move || {
        let registry = Registry::load(&dir, DEFAULT_BUDGET);
        tracing::info!(
            "plugins: {} loaded and {} refused from {}",
            registry.plugins.len(),
            registry.refused.len(),
            dir.display()
        );
        let _ = REGISTRY.set(registry);
        rinch::core::reactive::run_on_main_thread(loaded);
    });
    if let Err(e) = spawned {
        tracing::warn!("plugins: could not start loading: {e}");
    }
}

fn loaded() {
    if let Some(store) = STORE.with(Cell::get) {
        crate::metanodes::plugins_ready(store);
    }
}

fn read_approvals() -> HashMap<String, Approval> {
    let path = approvals_path();
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            // A file this app cannot read allows nothing: every plugin asks again.
            tracing::warn!("plugins: {} could not be read, so every plugin will ask again: {e}", path.display());
            HashMap::new()
        }),
        Err(_) => HashMap::new(),
    }
}

fn write_approvals() {
    let path = approvals_path();
    let json = APPROVALS.with(|a| serde_json::to_vec_pretty(&*a.borrow()));
    let written = json
        .map_err(|e| e.to_string())
        .and_then(|json| {
            std::fs::create_dir_all(plugins_dir()).map_err(|e| e.to_string())?;
            std::fs::write(&path, json).map_err(|e| e.to_string())
        });
    if let Err(e) = written {
        tracing::warn!("plugins: the approvals could not be saved to {}: {e}", path.display());
    }
}

fn approval_of(plugin_id: &str) -> Approval {
    APPROVALS.with(|a| a.borrow().get(plugin_id).cloned().unwrap_or_default())
}

/// Pane `pane`'s plugin area is `node` of `doc` (from `render_metanode`).
pub(crate) fn register_slot(pane: PaneId, doc: Weak<RefCell<dyn DomDocument>>, node: NodeId) {
    SLOTS.with(|s| s.borrow_mut().insert(pane, Slot { doc, node }));
}

/// What showing a metanode's viewer came to.
pub(crate) enum Outcome {
    /// The plugin's view is in the pane.
    Shown,
    /// It is not: the outline, under this sentence.
    Outline(String),
    /// The plugin waits for the person to allow it: the outline, under this
    /// sentence with "Allow" and "Not now".
    Ask(String),
    /// No plugin provides the viewer (or the folder is still loading): the outline
    /// with the usual line.
    Missing,
}

/// The name a person knows store `store` by.
fn store_name(store: AppStore, id: pimble_core::StoreId) -> String {
    untracked(|| store.store_data.with(|map| map.get(&id).map(|s| s.with(|s| s.name.clone()))))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "this store".to_string())
}

/// Show `forest`'s metanode in pane `pane` with the plugin providing `viewer`:
/// ask for approval, mount it, or hand the one already mounted its new props.
pub(crate) fn show(store: AppStore, pane: PaneId, viewer: &str, forest: &Forest) -> Outcome {
    let props = props(forest, viewer);
    let same = SHOWN.with(|s| s.borrow().get(&pane).is_some_and(|shown| shown.metanode == forest.metanode && shown.viewer == viewer));
    if same {
        let (instance, stopped, declined, last) = SHOWN.with(|s| {
            let s = s.borrow();
            let shown = s.get(&pane).expect("checked above");
            (shown.instance.clone(), shown.stopped.clone(), shown.declined, shown.props.clone())
        });
        if let Some(sentence) = stopped {
            return Outcome::Outline(sentence);
        }
        if let Some(instance) = instance {
            // A plugin reads its metanode's nodes through the API, which keeps
            // them live: new props are only for new settings.
            if props == last {
                return Outcome::Shown;
            }
            SHOWN.with(|s| s.borrow_mut().get_mut(&pane).map(|shown| shown.props = props.clone()));
            return if instance.props_changed(&props) { Outcome::Shown } else { stopped_sentence(pane) };
        }
        if declined {
            let name = SHOWN.with(|s| s.borrow().get(&pane).map(|shown| shown.plugin_id.clone())).unwrap_or_default();
            let name = REGISTRY.get().and_then(|r| r.plugins.iter().find(|p| p.manifest.id == name)).map_or(name, |p| p.manifest.name.clone());
            return Outcome::Outline(format!("Shown as an outline: the {name} plugin was not allowed here."));
        }
        // Waiting for approval: judged again below (it may have been allowed
        // just now).
    }
    hide(pane);
    let Some(registry) = REGISTRY.get() else { return Outcome::Missing };
    let plugin = match registry.viewer(viewer) {
        ViewerLookup::Ready(plugin) => plugin,
        ViewerLookup::Refused(sentence) => return Outcome::Outline(sentence.to_string()),
        ViewerLookup::Missing => return Outcome::Missing,
    };
    let manifest = plugin.manifest.clone();
    let approval = approval_of(&manifest.id);
    let shown = |instance: Option<Instance>, stopped: Option<String>| Shown {
        metanode: forest.metanode,
        viewer: viewer.to_string(),
        plugin_id: manifest.id.clone(),
        instance,
        props: props.clone(),
        stopped,
        declined: false,
    };
    if manifest.asks.asks_anything() && !approval.covers(&manifest.asks, forest.metanode.store) {
        SHOWN.with(|s| s.borrow_mut().insert(pane, shown(None, None)));
        return Outcome::Ask(asking_sentence(&manifest.name, &manifest.asks, &store_name(store, forest.metanode.store)));
    }
    let Some((doc, node)) = SLOTS.with(|s| s.borrow().get(&pane).and_then(|slot| Some((slot.doc.upgrade()?, slot.node)))) else {
        return Outcome::Missing;
    };
    let options = InstanceOptions { applier: ApplierOptions { text_field_shim: true } };
    let started = Instance::start(&manifest.name, doc, node, options, |key| plugin.instantiate(key, DEFAULT_BUDGET));
    let instance = match started {
        Ok(instance) => instance,
        Err(sentence) => {
            SHOWN.with(|s| s.borrow_mut().insert(pane, shown(None, Some(sentence.clone()))));
            return Outcome::Outline(sentence);
        }
    };
    instance.set_api(api_side(store, &instance, &manifest, approval));
    instance.on_stop(move |sentence| stopped(store, pane, sentence));
    SHOWN.with(|s| s.borrow_mut().insert(pane, shown(Some(instance.clone()), None)));
    if instance.mount(viewer, &props) {
        Outcome::Shown
    } else {
        stopped_sentence(pane)
    }
}

/// An instance's way to the API: its calls go to the backend thread with the
/// approval as it stands when each is made, and what it leaves in flight goes
/// when it stops.
fn api_side(store: AppStore, instance: &Instance, manifest: &Manifest, approval: Approval) -> ApiSide {
    let key = instance.key();
    let (plugin_id, plugin_name) = (manifest.id.clone(), manifest.name.clone());
    ApiSide {
        plugin_id: plugin_id.clone(),
        approval,
        forward: Box::new(move |call_id, call| {
            store.send(BackendCommand::PluginCall {
                instance: key,
                plugin_id: plugin_id.clone(),
                plugin_name: plugin_name.clone(),
                approval: approval_of(&plugin_id),
                call_id,
                call,
            });
        }),
        cancel: Box::new(move |call_id| store.send(BackendCommand::PluginCancel { instance: key, call_id })),
        forget: Box::new(move || store.send(BackendCommand::PluginForget { instance: key })),
    }
}

/// The answer to call `call_id` of instance `instance` (`BackendEvent::PluginReply`).
pub(crate) fn reply(instance: u64, call_id: u64, reply: ApiReply) {
    let found = SHOWN.with(|s| s.borrow().values().filter_map(|shown| shown.instance.clone()).find(|i| i.key() == instance));
    if let Some(instance) = found {
        instance.resolve(call_id, reply);
    }
}

/// "Allow" in pane `pane`: the plugin it asks for is allowed what it asks, for
/// the store of the metanode shown there, remembered on this device; the pane
/// shows it.
pub(crate) fn allow(store: AppStore, pane: PaneId) {
    let Some((plugin_id, metanode)) = SHOWN.with(|s| s.borrow().get(&pane).map(|shown| (shown.plugin_id.clone(), shown.metanode))) else {
        return;
    };
    let Some(manifest) = REGISTRY.get().and_then(|r| r.plugins.iter().find(|p| p.manifest.id == plugin_id)).map(|p| p.manifest.clone()) else {
        return;
    };
    let grown = approval_of(&plugin_id).grown(&manifest.asks, metanode.store);
    APPROVALS.with(|a| a.borrow_mut().insert(plugin_id.clone(), grown.clone()));
    write_approvals();
    tracing::info!("plugins: {} allowed {:?}", plugin_id, grown);
    // Instances already running elsewhere check their calls against the new
    // approval from now on.
    SHOWN.with(|s| {
        for shown in s.borrow().values().filter(|shown| shown.plugin_id == plugin_id) {
            if let Some(instance) = &shown.instance {
                instance.set_approval(grown.clone());
            }
        }
    });
    crate::metanodes::refill(store, pane);
    refresh_window();
}

/// "Not now" in pane `pane`: the outline, until the pane shows something else.
pub(crate) fn decline(store: AppStore, pane: PaneId) {
    SHOWN.with(|s| {
        if let Some(shown) = s.borrow_mut().get_mut(&pane) {
            shown.declined = true;
        }
    });
    crate::metanodes::refill(store, pane);
}

/// Take back what plugin `plugin_id` was allowed: every pane showing it stops
/// it and asks again.
pub(crate) fn revoke(store: AppStore, plugin_id: &str) {
    APPROVALS.with(|a| a.borrow_mut().remove(plugin_id));
    write_approvals();
    tracing::info!("plugins: {plugin_id}'s approval taken back");
    let panes: Vec<PaneId> = SHOWN.with(|s| s.borrow().iter().filter(|(_, shown)| shown.plugin_id == plugin_id).map(|(p, _)| *p).collect());
    for pane in panes {
        hide(pane);
        crate::metanodes::refill(store, pane);
    }
    refresh_window();
}

fn stopped_sentence(pane: PaneId) -> Outcome {
    let sentence = SHOWN.with(|s| s.borrow().get(&pane).and_then(|shown| shown.stopped.clone()));
    Outcome::Outline(sentence.unwrap_or_else(|| "The plugin stopped.".to_string()))
}

/// The plugin in `pane` stopped by itself: the pane falls back to the outline with
/// the sentence, and the plugin is not started again for this metanode.
fn stopped(store: AppStore, pane: PaneId, sentence: &str) {
    SHOWN.with(|s| {
        if let Some(shown) = s.borrow_mut().get_mut(&pane) {
            shown.instance = None;
            shown.stopped = Some(sentence.to_string());
        }
    });
    crate::metanodes::plugin_stopped(store, pane, sentence);
}

/// Take down whatever plugin pane `pane` shows.
pub(crate) fn hide(pane: PaneId) {
    let shown = SHOWN.with(|s| s.borrow_mut().remove(&pane));
    if let Some(Shown { instance: Some(instance), .. }) = shown {
        instance.unmount();
    }
}

/// What a viewer is handed: the metanode, and the viewer's settings on it (its
/// `data` under the viewer's id). The forest is the plugin's to read.
fn props(forest: &Forest, viewer: &str) -> Props {
    let settings = NodeDoc::load(&forest.node.content)
        .ok()
        .and_then(|doc| doc.data_json().get(viewer).cloned())
        .map(|value| value.to_string());
    Props { metanode: NodeAddr::from(forest.metanode), settings }
}

/// The element a pane's plugin view is applied under: shown while a plugin view
/// is, and registered as the pane's slot.
pub(crate) fn render_slot(__scope: &mut RenderScope, pane: PaneId, shown: Signal<bool>) -> NodeHandle {
    let slot = __scope.create_element("div");
    slot.set_attribute("class", "pimble-metanode__plugin");
    register_slot(pane, __scope.doc_weak(), slot.node_id());
    let styled = slot.clone();
    __scope.create_effect(move || {
        let style = if shown.get() { "" } else { "display: none;" };
        styled.set_attribute("style", style);
    });
    slot
}

// ── Account > Plugins... ──────────────────────────────────────────────────────

/// One installed plugin, as the Plugins window lists it.
#[derive(Clone, PartialEq)]
struct PluginRow {
    id: String,
    name: String,
    /// What it provides and asks for, or why it does not run.
    line: String,
    /// What it is allowed, when anything.
    allowed: Option<String>,
}

#[derive(Clone, Copy)]
struct PluginsUi {
    open: Signal<bool>,
    rows: Signal<Vec<PluginRow>>,
}

/// Open Account > "Plugins...".
pub(crate) fn open(store: AppStore) {
    if let Some(ui) = UI.with(Cell::get) {
        ui.rows.set(rows(store));
        ui.open.set(true);
    }
}

fn refresh_window() {
    if let (Some(ui), Some(store)) = (UI.with(Cell::get), STORE.with(Cell::get)) {
        ui.rows.set(rows(store));
    }
}

fn rows(store: AppStore) -> Vec<PluginRow> {
    let Some(registry) = REGISTRY.get() else { return Vec::new() };
    let mut rows: Vec<PluginRow> = registry
        .plugins
        .iter()
        .map(|p| {
            let m = &p.manifest;
            let viewers: Vec<String> = m.viewers().map(|v| v.name.clone()).collect();
            let asks = match (m.asks.stores, m.asks.write) {
                (StoreAsk::None, _) => "asks for nothing".to_string(),
                (StoreAsk::All, true) => "asks to read and change all your stores".to_string(),
                (StoreAsk::All, false) => "asks to read all your stores".to_string(),
                (StoreAsk::Chosen, true) => "asks to read and change the stores you choose".to_string(),
                (StoreAsk::Chosen, false) => "asks to read the stores you choose".to_string(),
            };
            let line = format!("{} {}: the {} viewer; {asks}.", m.name, m.version, viewers.join(", "));
            let approval = approval_of(&m.id);
            let allowed = match &approval.stores {
                Approved::Nothing => None,
                stores => {
                    let what = match stores {
                        Approved::All => "all your stores".to_string(),
                        Approved::Only(ids) => ids.iter().map(|id| store_name(store, *id)).collect::<Vec<_>>().join(", "),
                        Approved::Nothing => unreachable!(),
                    };
                    let verb = if approval.write { "read and change" } else { "read" };
                    Some(format!("Allowed to {verb} {what} on this device."))
                }
            };
            PluginRow { id: m.id.clone(), name: m.name.clone(), line, allowed }
        })
        .collect();
    for refused in &registry.refused {
        let file = refused.path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        rows.push(PluginRow { id: String::new(), name: file, line: refused.sentence.clone(), allowed: None });
    }
    rows
}

/// The Plugins window: each plugin in the folder, what it asks for, what it was
/// allowed here, and "Revoke".
pub(crate) fn render_modal(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let ui = PluginsUi { open: Signal::new(false), rows: Signal::new(Vec::new()) };
    UI.with(|u| u.set(Some(ui)));
    rsx! {
        Modal {
            opened_fn: move || ui.open.get(),
            onclose: move || ui.open.set(false),
            title: "Plugins",
            size: "md",
            div {
                class: "pimble-plugins",
                style: "display: flex; flex-direction: column; gap: 12px;",
                div {
                    style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                    {format!("Plugins are .wasm files in {}. Each asks once on this device before it reads or changes anything.", plugins_dir().display())}
                }
                if ui.rows.with(|r| r.is_empty()) {
                    div { style: "font-size: 13px;", "No plugins are installed." }
                }
                for row in ui.rows.get() {
                    div {
                        key: format!("{} {} {:?}", row.id, row.name, row.allowed),
                        class: "pimble-plugins__row",
                        style: "display: flex; gap: 12px; align-items: center;",
                        div {
                            style: "flex: 1; display: flex; flex-direction: column; gap: 2px;",
                            div { style: "font-size: 13px;", {row.line.clone()} }
                            div {
                                style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                                {row.allowed.clone().unwrap_or_else(|| if row.id.is_empty() { String::new() } else { "Not allowed anything yet.".to_string() })}
                            }
                        }
                        if row.allowed.is_some() {
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
                div {
                    style: "display: flex; justify-content: flex-end;",
                    Button { variant: "default", size: "xs", onclick: move || ui.open.set(false), "Close" }
                }
            }
        }
    }
}
