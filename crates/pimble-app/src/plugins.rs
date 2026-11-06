//! Plugins in the app (docs/PLUGINS_CONTRACT.md waves 1 to 3), on the desktop and
//! in the browser alike: where plugins live on this device, what the person has
//! allowed each, the viewer a pane mounts for a metanode, and the plugin's calls
//! through the Pimble API. Only the engine and where plugins and approvals are kept
//! differ (`place` below).
//!
//! On the desktop, plugins are `.wasm` files in `<data dir>/pimble/plugins/`, put
//! there by hand (decision 3), compiled under wasmtime off the UI thread when the
//! app starts. In the browser there is no folder: the Plugins window's "Add
//! plugin..." takes a `.wasm` file the person picks, and the page keeps it in its
//! own IndexedDB (per browser, never uploaded), compiled by the page's own
//! `WebAssembly` when the app starts and when it is added. A pane opening a
//! metanode whose viewer one of them
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
//! per device, never synced), in the browser in `localStorage`
//! (`pimble.plugins.approvals`, per browser), and a plugin without approval
//! mounts nothing. "Plugins..." (the desktop's Account menu, the browser's File
//! menu) lists the plugins and takes an approval back.
//!
//! **The API.** A mounted plugin's calls are checked against its approval by the
//! host (`pimble_plugin_host::bridge`) and sent to the backend thread as
//! `BackendCommand::PluginCall`, where they are made through the window's own
//! backend (`crate::plugin_calls`: the desktop's `Pimble`, the browser's router,
//! whose encrypted stores are the page's vault), so the server or the vault
//! judges them with the person's role and the plugin is granted nothing more;
//! answers come back as
//! `BackendEvent::PluginReply` and are handed to the instance. The UI thread has
//! no second API instance.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Weak;

use pimble_api::{Addr, Forest};
use pimble_crdt::NodeDoc;
use pimble_plugin_host::abi::api::{ApiReply, Reach};
use pimble_plugin_host::abi::{HostRequest, HostState, Manifest, NodeAddr, NodePeople, PresentPerson, Props, RecipeDecl, StoreAsk};
use pimble_plugin_host::bridge::asking_sentence;
use pimble_plugin_host::library::{self, Entry, Index, Offer, Provenance};
#[cfg(not(feature = "native"))]
use pimble_plugin_host::js_engine::{Plugin, Registry, ViewerLookup};
#[cfg(feature = "native")]
use pimble_plugin_host::wasmtime_engine::{Plugin, Registry, ViewerLookup};
use pimble_plugin_host::{ApiSide, ApplierOptions, Approval, Approved, GuestEngine, Failure, HostKey, Instance, InstanceOptions};
use rinch::core::dom::{DomDocument, NodeId};
use rinch::prelude::*;

use crate::panes::PaneId;
use crate::protocol::BackendCommand;
use crate::state::AppStore;


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
    /// The stores the metanode's mounts reach that the plugin was not allowed,
    /// asked for while it runs (a mounted kanban in another store is a lane
    /// only once the plugin may read it).
    wants: Vec<pimble_core::StoreId>,
    /// "Not now" was chosen for `wants` in this pane.
    declined_wants: bool,
    /// A newer version of the plugin was installed on its own while this
    /// pane showed it: the pane keeps this one until "Restart Plugin" or the
    /// next time it mounts (docs/SETTINGS_CONTRACT.md section 3).
    newer: bool,
}

thread_local! {
    static STORE: Cell<Option<AppStore>> = const { Cell::new(None) };
    static SLOTS: RefCell<HashMap<PaneId, Slot>> = RefCell::new(HashMap::new());
    static SHOWN: RefCell<HashMap<PaneId, Shown>> = RefCell::new(HashMap::new());
    /// What each plugin was allowed, by plugin id: `approvals.json`.
    static APPROVALS: RefCell<HashMap<String, Approval>> = RefCell::new(HashMap::new());
    /// The Plugins window.
    static UI: Cell<Option<PluginsUi>> = const { Cell::new(None) };
    /// The plugins as loaded; `None` until loading ends.
    static REGISTRY: RefCell<Option<Registry>> = const { RefCell::new(None) };
    /// Host state is pushed from one effect, made once.
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    /// The plugin library's index, once it was read.
    static INDEX: RefCell<Option<Index>> = const { RefCell::new(None) };
    /// Which installed plugins came from the library: `library.json`.
    static PROVENANCE: RefCell<Provenance> = RefCell::new(Provenance::default());
    /// What a library row says about its plugin now (downloading, what came
    /// of an install), by plugin id.
    static ROW_LINES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    /// Plugins being updated on their own (wave 3): a pane showing one keeps
    /// the version it runs when the new one is swapped in.
    static QUIET: RefCell<std::collections::HashSet<String>> = RefCell::new(std::collections::HashSet::new());
    /// The library is checked for updates on a timer, started once.
    static CHECKING: Cell<bool> = const { Cell::new(false) };
}

/// Read the registry, if it has loaded.
fn with_registry<R>(f: impl FnOnce(&Registry) -> R) -> Option<R> {
    REGISTRY.with(|r| r.borrow().as_ref().map(f))
}

/// The manifest of loaded plugin `id`.
fn manifest_of(id: &str) -> Option<Manifest> {
    with_registry(|r| r.plugin(id).map(|p| p.manifest.clone())).flatten()
}

/// Load the plugins (off the UI thread, or asynchronously in the browser), and
/// the approvals; panes showing a metanode look again when it is done.
pub(crate) fn load(store: AppStore) {
    STORE.with(|s| s.set(Some(store)));
    APPROVALS.with(|a| *a.borrow_mut() = place::read_approvals());
    PROVENANCE.with(|p| *p.borrow_mut() = place::read_provenance());
    place::load();
}

/// The plugins finished loading (or changed: one was added or removed in the
/// browser).
fn loaded(registry: Registry) {
    REGISTRY.with(|r| *r.borrow_mut() = Some(registry));
    if let Some(store) = STORE.with(Cell::get) {
        // What the explorer's new-node menu offers from them.
        store.recipes.set(recipes());
        crate::metanodes::plugins_ready(store);
        if !CHECKING.with(|c| c.replace(true)) {
            schedule_update_check(FIRST_UPDATE_CHECK_MS);
        }
        if !WATCHING.with(|w| w.replace(true)) {
            watch_host_state(store);
            // A drag in this window ended: every running plugin hears it, so a
            // sortable list whose dragged item a re-render removed still ends
            // its drag (rinch #2093).
            rinch::core::on_element_drag_end(|| {
                let running: Vec<Instance> = SHOWN.with(|s| s.borrow().values().filter_map(|shown| shown.instance.clone()).collect());
                for instance in running {
                    instance.drag_ended();
                }
            });
        }
    }
    refresh_window();
    refresh_library();
}

/// The plugins changed: one was added, replaced or removed (`changed` names
/// them by id). Every pane showing one of them stops it, the registry is
/// swapped, and each pane holding a metanode mounts again with what is loaded
/// now, so a new version takes over in place (docs/SETTINGS_CONTRACT.md
/// section 3).
///
/// A plugin updated on its own (`QUIET`) is not stopped: each pane showing it
/// keeps the version it runs and says a newer one is ready.
fn swapped(registry: Registry, changed: &[String]) {
    for id in changed {
        if QUIET.with(|q| q.borrow().contains(id)) {
            SHOWN.with(|s| {
                for shown in s.borrow_mut().values_mut().filter(|shown| &shown.plugin_id == id && shown.instance.is_some()) {
                    shown.newer = true;
                }
            });
        } else {
            stop_everywhere(id);
        }
    }
    loaded(registry);
}

/// "Restart Plugin": the pane stops the version it runs and mounts the one
/// installed now.
pub(crate) fn restart(store: AppStore, pane: PaneId) {
    hide(pane);
    crate::metanodes::refill(store, pane);
}

/// Every pane showing plugin `id` stops it.
fn stop_everywhere(id: &str) {
    let panes: Vec<PaneId> = SHOWN.with(|s| s.borrow().iter().filter(|(_, shown)| shown.plugin_id == id).map(|(p, _)| *p).collect());
    for pane in panes {
        hide(pane);
    }
}

/// The recipes every loaded plugin registers ("New Kanban"), in the folder's
/// order, each with the plugin that registered it.
fn recipes() -> Vec<crate::state::PluginRecipe> {
    with_registry(|registry| {
        registry
            .plugins
            .iter()
            .flat_map(|p| p.manifest.recipes().map(|r| crate::state::PluginRecipe { plugin: p.manifest.name.clone(), recipe: r.clone() }))
            .collect()
    })
    .unwrap_or_default()
}

/// A row's recipe item: make what `recipe` says under `parent`, through the
/// app's own API, as the person's own edit (no plugin code runs).
pub(crate) fn make(store: AppStore, store_id: pimble_core::StoreId, parent_id: pimble_core::NodeId, recipe: RecipeDecl, place: pimble_core::Place) {
    store.send(BackendCommand::MakeFromRecipe { store_id, parent_id, recipe, place });
}

// ── What the host knows, for each plugin view ─────────────────────────────────

/// Hand every mounted plugin what the app knows that it draws with, now and
/// whenever it changes: who is in which node, whom its pane follows, the
/// theme (docs/PRESENCE_CONTRACT.md "Plugins").
fn watch_host_state(store: AppStore) {
    let _ = rinch::Effect::new(move || {
        // Tracked: the sessions, the theme, and each pane's following.
        store.presence.with(|_| ());
        store.dark_mode.get();
        for pane in PaneId::ALL {
            crate::presence::follow::watch(pane).with(|_| ());
        }
        untracked(|| {
            for pane in PaneId::ALL {
                push_host_state(store, pane);
            }
        });
    });
}

/// What the plugin in pane `pane` is told: the people in every node of the
/// stores it may read, as the app draws them (`presence::people_in`, so names,
/// initials, colors and hovers are the app's own), whom the pane follows, and
/// the theme.
fn host_state(store: AppStore, pane: PaneId, approval: &Approval) -> HostState {
    let docs: Vec<(pimble_core::StoreId, pimble_core::NodeId)> = store.presence.with(|all| all.keys().copied().collect());
    let people = docs
        .into_iter()
        .filter(|(s, _)| approval.allows(*s, Reach::Read))
        .map(|(s, n)| NodePeople {
            node: NodeAddr::from(Addr::new(s, n)),
            people: crate::presence::people_in(store, s, n)
                .into_iter()
                .map(|p| PresentPerson { key: p.key, name: p.name, initials: p.initials, color: p.color, title: p.title, agent: p.agent })
                .collect(),
        })
        .filter(|n| !n.people.is_empty())
        .collect();
    let following = match crate::presence::follow::watch(pane).get() {
        crate::presence::follow::Follow::On(leader) => Some(leader.key),
        _ => None,
    };
    HostState { dark: store.dark_mode.get(), people, following }
}

fn push_host_state(store: AppStore, pane: PaneId) {
    let Some((instance, plugin_id)) = SHOWN.with(|s| s.borrow().get(&pane).and_then(|shown| Some((shown.instance.clone()?, shown.plugin_id.clone())))) else {
        return;
    };
    let state = host_state(store, pane, &approval_of(&plugin_id));
    instance.set_host_state(&state);
}

/// What the plugin in pane `pane` asked of the app.
fn request(store: AppStore, pane: PaneId, plugin_id: &str, request: HostRequest) {
    // A plugin names only nodes of stores it may read; anything else it
    // asks about is dropped, as a call outside its approval is refused.
    let approval = approval_of(plugin_id);
    let readable = |node: &NodeAddr| node.addr().filter(|a| approval.allows(a.store, Reach::Read));
    match request {
        HostRequest::Open { node } => {
            if let Some(addr) = readable(&node) {
                store.link_open.set(Some(crate::state::LinkOpen {
                    value: format!("node_{}_{}", addr.store, addr.node),
                    store_id: addr.store,
                    node_id: addr.node,
                    anchor: None,
                    pane: Some(pane),
                }));
            }
        }
        HostRequest::OpenBeside { node } => {
            if let Some(addr) = readable(&node) {
                // Out of the plugin's call: splitting writes the layout, which
                // other effects read.
                let _ = rinch::prelude::set_timeout(0, move || crate::pane_view::open_beside(store, pane, addr.store, addr.node));
            }
        }
        HostRequest::Focus { node } => {
            let on = match node {
                None => None,
                Some(node) => match readable(&node) {
                    Some(addr) => Some((addr.store, addr.node)),
                    None => return,
                },
            };
            crate::presence::say_within(store, pane, on);
        }
        HostRequest::Follow { person } => crate::presence::follow::start(store, pane, &person),
        HostRequest::Remember { value } => {
            if value.len() > pimble_plugin_abi::MAX_REMEMBERED {
                tracing::debug!("plugins: {plugin_id} asked to remember {} bytes; not kept", value.len());
                return;
            }
            let shown = SHOWN.with(|s| s.borrow().get(&pane).map(|shown| (shown.viewer.clone(), shown.metanode)));
            if let Some((viewer, metanode)) = shown {
                crate::views::set_plugin_view(plugin_id, &viewer, metanode.store, metanode.node, value);
            }
        }
    }
}

fn write_approvals() {
    let json = APPROVALS.with(|a| serde_json::to_vec_pretty(&*a.borrow()));
    match json {
        Ok(json) => place::write_approvals(&json),
        Err(e) => tracing::warn!("plugins: the approvals could not be saved: {e}"),
    }
}

/// Approvals as remembered: a record this app cannot read allows nothing, and
/// every plugin asks again.
fn parse_approvals(bytes: &[u8]) -> HashMap<String, Approval> {
    serde_json::from_slice(bytes).unwrap_or_else(|e| {
        tracing::warn!("plugins: the approvals could not be read, so every plugin will ask again: {e}");
        HashMap::new()
    })
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
    /// It is, and the metanode shows mounts into stores it was not allowed:
    /// the view, under this sentence asking for them with "Allow" and "Not
    /// now".
    ShownAsking(String),
    /// It is, and a newer version was installed since it mounted: the view,
    /// under this sentence with "Restart Plugin".
    ShownNewer(String),
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
    let outcome = show_viewer(store, pane, viewer, forest);
    if !matches!(outcome, Outcome::Shown) {
        return outcome;
    }
    // Shown: ask for the stores its mounts reach that the plugin may not read,
    // as it asked for the metanode's own.
    let Some(plugin_id) = SHOWN.with(|s| s.borrow().get(&pane).map(|shown| shown.plugin_id.clone())) else { return outcome };
    let Some(manifest) = manifest_of(&plugin_id) else { return outcome };
    let approval = approval_of(&plugin_id);
    let wants: Vec<pimble_core::StoreId> = mounted_stores(forest).into_iter().filter(|s| !approval.covers(&manifest.asks, *s)).collect();
    let declined = SHOWN.with(|s| {
        let mut s = s.borrow_mut();
        let shown = s.get_mut(&pane)?;
        shown.wants = wants.clone();
        Some(shown.declined_wants)
    });
    if wants.is_empty() || declined != Some(false) {
        let newer = SHOWN.with(|s| s.borrow().get(&pane).is_some_and(|shown| shown.newer));
        if newer {
            return Outcome::ShownNewer(format!("A newer version of {} is ready.", manifest.name));
        }
        return outcome;
    }
    let names: Vec<String> = wants.iter().map(|s| store_name(store, *s)).collect();
    Outcome::ShownAsking(asking_sentence(&manifest.name, &manifest.asks, &names.join(" and ")))
}

/// The stores other than the metanode's that its forest reaches through
/// mounts, in the forest's order.
fn mounted_stores(forest: &Forest) -> Vec<pimble_core::StoreId> {
    let mut stores = Vec::new();
    for entry in &forest.entries {
        let reached = entry.node.mount_ref().map(|m| m.source_store).into_iter().chain(Some(entry.addr.store));
        for store in reached {
            if store != forest.metanode.store && !stores.contains(&store) {
                stores.push(store);
            }
        }
    }
    stores
}

/// [`show`] without asking for the mounts' stores.
fn show_viewer(store: AppStore, pane: PaneId, viewer: &str, forest: &Forest) -> Outcome {
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
            let name = manifest_of(&name).map_or(name, |m| m.name);
            return Outcome::Outline(format!("Shown as an outline: the {name} plugin was not allowed here."));
        }
        // Waiting for approval: judged again below (it may have been allowed
        // just now).
    }
    hide(pane);
    let found = with_registry(|registry| match registry.viewer(viewer) {
        ViewerLookup::Ready(plugin) => Ok(plugin.clone()),
        ViewerLookup::Refused(sentence) => Err(Outcome::Outline(sentence.to_string())),
        ViewerLookup::Missing => Err(Outcome::Missing),
    });
    let plugin = match found {
        Some(Ok(plugin)) => plugin,
        Some(Err(outcome)) => return outcome,
        None => return Outcome::Missing,
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
        wants: Vec::new(),
        declined_wants: false,
        newer: false,
    };
    if manifest.asks.asks_anything() && !approval.covers(&manifest.asks, forest.metanode.store) {
        SHOWN.with(|s| s.borrow_mut().insert(pane, shown(None, None)));
        return Outcome::Ask(asking_sentence(&manifest.name, &manifest.asks, &store_name(store, forest.metanode.store)));
    }
    let Some((doc, node)) = SLOTS.with(|s| s.borrow().get(&pane).and_then(|slot| Some((slot.doc.upgrade()?, slot.node)))) else {
        return Outcome::Missing;
    };
    if !place::ready(&plugin, store) {
        return Outcome::Outline(format!("The {} plugin is starting.", manifest.name));
    }
    // The desktop backend makes an element a text field only when it carries
    // `data-oninput`, which a plugin's `rsx!` leaves out; the browser's must
    // not get one (it would shadow an ancestor's in rinch-web's delegation).
    let options = InstanceOptions { applier: ApplierOptions { text_field_shim: cfg!(feature = "native") } };
    let started = Instance::start(&manifest.name, doc, node, options, |key| place::instantiate(&plugin, key));
    let instance = match started {
        Ok(instance) => instance,
        Err(sentence) => {
            SHOWN.with(|s| s.borrow_mut().insert(pane, shown(None, Some(sentence.clone()))));
            return Outcome::Outline(sentence);
        }
    };
    instance.set_api(api_side(store, &instance, &manifest, approval));
    instance.on_stop(move |sentence| stopped(store, pane, sentence));
    let plugin_id = manifest.id.clone();
    instance.on_request(move |r| request(store, pane, &plugin_id, r));
    SHOWN.with(|s| s.borrow_mut().insert(pane, shown(Some(instance.clone()), None)));
    // What this device remembers of the view is handed at mount only: the
    // props kept for comparing are the settings, so remembering never sends
    // new props.
    let remembered = crate::views::plugin_view(&manifest.id, viewer, forest.metanode.store, forest.metanode.node);
    if instance.mount(viewer, &Props { remembered, ..props.clone() }) {
        push_host_state(store, pane);
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
    let Some(manifest) = manifest_of(&plugin_id) else {
        return;
    };
    // The metanode's store, and while the plugin runs, the stores its mounts
    // reach that it asked for.
    let (wants, running) = SHOWN.with(|s| s.borrow().get(&pane).map(|shown| (shown.wants.clone(), shown.instance.is_some()))).unwrap_or_default();
    let mut grown = approval_of(&plugin_id).grown(&manifest.asks, metanode.store);
    for store in &wants {
        grown = grown.grown(&manifest.asks, *store);
    }
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
    // A running plugin read its metanode without them: it starts again, so
    // it reads what they let it (a mounted kanban becomes a lane).
    if running && !wants.is_empty() {
        hide(pane);
    }
    crate::metanodes::refill(store, pane);
    refresh_window();
}

/// "Not now" in pane `pane`: the outline, until the pane shows something else.
pub(crate) fn decline(store: AppStore, pane: PaneId) {
    SHOWN.with(|s| {
        if let Some(shown) = s.borrow_mut().get_mut(&pane) {
            // A running plugin asking for its mounts' stores keeps running
            // without them; one waiting for its first approval is not shown.
            if shown.instance.is_some() {
                shown.declined_wants = true;
            } else {
                shown.declined = true;
            }
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
    crate::presence::say_within(store, pane, None);
    SHOWN.with(|s| {
        if let Some(shown) = s.borrow_mut().get_mut(&pane) {
            shown.instance = None;
            shown.stopped = Some(sentence.to_string());
        }
    });
    crate::metanodes::plugin_stopped(store, pane, sentence);
}

/// Take down whatever plugin pane `pane` shows; what it said the person was
/// on goes with it.
pub(crate) fn hide(pane: PaneId) {
    let shown = SHOWN.with(|s| s.borrow_mut().remove(&pane));
    if let Some(Shown { instance: Some(instance), .. }) = shown {
        instance.unmount();
    }
    if let Some(store) = STORE.with(Cell::get) {
        crate::presence::say_within(store, pane, None);
    }
}

/// What a viewer is handed: the metanode, and the viewer's settings on it (its
/// `data` under the viewer's id). The forest is the plugin's to read.
fn props(forest: &Forest, viewer: &str) -> Props {
    let settings = NodeDoc::load(&forest.node.content)
        .ok()
        .and_then(|doc| doc.data_json().get(viewer).cloned())
        .map(|value| value.to_string());
    Props { metanode: NodeAddr::from(forest.metanode), settings, remembered: None }
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
fn asks_sentence(asks: &pimble_plugin_host::abi::Permissions) -> &'static str {
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
struct LibraryRow {
    id: String,
    /// "Kanban 0.1.0".
    title: String,
    summary: String,
    /// "Asks to read and change the stores you choose."
    asks: String,
    offer: Offer,
    /// "Install" or "Update", when the row has a button.
    button: Option<&'static str>,
    /// Downloading, or what came of the last install.
    line: String,
}

#[derive(Clone, Copy)]
struct PluginsUi {
    rows: Signal<Vec<PluginRow>>,
    /// What came of the last "Add plugin..." or "Remove".
    status: Signal<String>,
    library: Signal<Vec<LibraryRow>>,
    /// Reading the library, or why it could not be read.
    library_line: Signal<String>,
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
}

fn refresh_window() {
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
                        key: format!("{} {} {:?} {}", row.id, row.title, row.offer, row.line),
                        class: "pimble-plugins__row",
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

// ── The library (docs/SETTINGS_CONTRACT.md section 4) ─────────────────────────

/// Where the library's index (`None`) or one of its files is.
fn library_url(file: Option<&str>) -> String {
    let base = place::library_base();
    match file {
        Some(file) => format!("{}/{file}", base.trim_end_matches('/')),
        None => base,
    }
}

/// Read the index again, as Settings > Plugins is shown.
fn read_library() {
    if INDEX.with(|i| i.borrow().is_none()) {
        library_said("Looking at the library...".to_string());
    }
    place::fetch(library_url(None), |fetched| {
        let read = fetched.and_then(|bytes| Index::parse(&bytes));
        match read {
            Ok(index) => {
                let empty = index.plugins.is_empty();
                INDEX.with(|i| *i.borrow_mut() = Some(index));
                library_said(if empty { "The library has no plugins yet.".to_string() } else { String::new() });
            }
            Err(e) => {
                tracing::warn!("plugins: the library could not be read: {e}");
                let known = INDEX.with(|i| i.borrow().is_some());
                library_said(if known { String::new() } else { format!("The library could not be reached ({e}).") });
            }
        }
        refresh_library();
    });
}

fn library_said(line: String) {
    if let Some(ui) = UI.with(Cell::get) {
        ui.library_line.set(line);
    }
}

fn library_rows() -> Vec<LibraryRow> {
    let Some(index) = INDEX.with(|i| i.borrow().clone()) else { return Vec::new() };
    index
        .plugins
        .iter()
        .map(|entry| {
            let installed = manifest_of(&entry.id).map(|m| m.version);
            let from_library = PROVENANCE.with(|p| p.borrow().from_library(&entry.id));
            let offer = library::offer(entry, installed.as_deref(), from_library);
            let mut asks = asks_sentence(&entry.asks).to_string();
            if let Some(first) = asks.get_mut(..1) {
                first.make_ascii_uppercase();
            }
            let line = ROW_LINES.with(|l| l.borrow().get(&entry.id).cloned()).unwrap_or_else(|| match &offer {
                Offer::Update { from } => format!("You have {from}."),
                _ => String::new(),
            });
            let button = match offer {
                Offer::Install => Some("Install"),
                Offer::Update { .. } => Some("Update"),
                _ => None,
            };
            LibraryRow { button, id: entry.id.clone(), title: format!("{} {}", entry.name, entry.version), summary: entry.summary.clone(), asks: format!("{asks}."), offer, line }
        })
        .collect()
}

fn refresh_library() {
    if let Some(ui) = UI.with(Cell::get) {
        ui.library.set(library_rows());
    }
}

fn row_said(id: &str, line: String) {
    ROW_LINES.with(|l| l.borrow_mut().insert(id.to_string(), line));
    refresh_library();
}

/// "Install" or "Update".
fn install_from_library(id: &str) {
    let Some(entry) = INDEX.with(|i| i.borrow().as_ref().and_then(|index| index.plugins.iter().find(|e| e.id == id).cloned())) else {
        return;
    };
    row_said(&entry.id, format!("Downloading {}...", entry.name));
    fetch_and_install(entry);
}

/// The plugin and its signature are downloaded, checked against what the
/// index lists and the key this build carries, and only then installed. A
/// plugin that does not verify is never written anywhere.
fn fetch_and_install(entry: Entry) {
    place::fetch(library_url(Some(&entry.wasm)), move |wasm| {
        let sig_url = library_url(Some(&entry.sig));
        place::fetch(sig_url, move |sig| {
            let (wasm, sig) = match (wasm, sig) {
                (Ok(wasm), Ok(sig)) => (wasm, sig),
                (Err(e), _) | (_, Err(e)) => return install_failed(&entry, format!("The download did not finish ({e}).")),
            };
            if let Err(e) = library::verify_download(&entry, crate::RELEASE_KEY, &wasm, &sig) {
                tracing::warn!("plugins: {} from the library refused: {e}", entry.id);
                return install_failed(&entry, library::NOT_VERIFIED.to_string());
            }
            if !QUIET.with(|q| q.borrow().contains(&entry.id)) {
                row_said(&entry.id, format!("Installing {}...", entry.name));
            }
            place::install(entry, wasm);
        });
    });
}

/// A plugin from the library is installed: remember where it came from.
fn installed_from_library(entry: &Entry) {
    let quiet = QUIET.with(|q| q.borrow_mut().remove(&entry.id));
    PROVENANCE.with(|p| p.borrow_mut().plugins.insert(entry.id.clone(), entry.version.clone()));
    write_provenance();
    let line = if quiet { format!("{} updated to {}.", entry.name, entry.version) } else { format!("Installed {} {}.", entry.name, entry.version) };
    tracing::info!("plugins: {line}");
    row_said(&entry.id, line);
    refresh_window();
    if quiet {
        // Panes showing it say a newer version is ready.
        if let Some(store) = STORE.with(Cell::get) {
            crate::metanodes::plugins_ready(store);
        }
    }
}

/// An install from the library did not happen: the row says why. One made on
/// its own is tried again at the next check.
fn install_failed(entry: &Entry, line: String) {
    let quiet = QUIET.with(|q| q.borrow_mut().remove(&entry.id));
    if quiet {
        tracing::warn!("plugins: updating {} to {} did not happen: {line}", entry.id, entry.version);
    }
    row_said(&entry.id, line);
}

// ── Updates on their own (docs/SETTINGS_CONTRACT.md section 5) ───────────────

/// The first look at the library after the app starts, then one every six
/// hours.
const FIRST_UPDATE_CHECK_MS: u32 = 20_000;
const UPDATE_CHECK_EVERY_MS: u32 = 6 * 60 * 60 * 1000;

fn schedule_update_check(after_ms: u32) {
    rinch::core::reactive::unowned(|| {
        let _ = rinch::core::set_timeout(after_ms, || {
            check_for_updates();
            schedule_update_check(UPDATE_CHECK_EVERY_MS);
        });
    });
}

/// Read the index, and install every newer version of a plugin that came from
/// the library and that this Pimble can load, without asking. A plugin added
/// from a file is never touched; what each was allowed carries over, and a
/// new version that asks for more asks the way a first use does.
fn check_for_updates() {
    place::fetch(library_url(None), |fetched| {
        let index = match fetched.and_then(|bytes| Index::parse(&bytes)) {
            Ok(index) => index,
            Err(e) => return tracing::info!("plugins: no update check this time: {e}"),
        };
        INDEX.with(|i| *i.borrow_mut() = Some(index.clone()));
        let due = updates_due(&index, |id| manifest_of(id).map(|m| m.version), |id| PROVENANCE.with(|p| p.borrow().from_library(id)));
        refresh_library();
        for entry in due {
            if !QUIET.with(|q| q.borrow_mut().insert(entry.id.clone())) {
                continue;
            }
            tracing::info!("plugins: updating {} to {}", entry.id, entry.version);
            fetch_and_install(entry);
        }
    });
}

/// The entries of `index` that are updates to install on their own: newer
/// than the version installed, of a plugin installed from the library, for
/// this Pimble's plugin ABI.
fn updates_due(index: &Index, installed: impl Fn(&str) -> Option<String>, from_library: impl Fn(&str) -> bool) -> Vec<Entry> {
    index
        .plugins
        .iter()
        .filter(|entry| {
            let have = installed(&entry.id);
            matches!(library::offer(entry, have.as_deref(), from_library(&entry.id)), Offer::Update { .. })
        })
        .cloned()
        .collect()
}

/// Plugin `id` was removed, or added from a file over the library's: the
/// library no longer looks after it.
fn forget_library(id: &str) {
    let had = PROVENANCE.with(|p| p.borrow_mut().plugins.remove(id).is_some());
    if had {
        write_provenance();
    }
    ROW_LINES.with(|l| l.borrow_mut().remove(id));
    refresh_library();
}

fn write_provenance() {
    let json = PROVENANCE.with(|p| p.borrow().to_json());
    place::write_provenance(&json);
}

/// Say what came of an add or a remove, in the Plugins window.
fn say(line: String) {
    if let Some(ui) = UI.with(Cell::get) {
        ui.status.set(line);
    }
}

// ── Where plugins live, per target ────────────────────────────────────────────

/// The desktop: a folder of `.wasm` files, compiled under wasmtime off the UI
/// thread and watched, so a plugin added, replaced or removed there (by
/// Settings, `scripts/plugins/install.sh` or by hand) takes effect at once;
/// approvals in a file beside them.
#[cfg(feature = "native")]
mod place {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::{Duration, SystemTime};

    use super::*;
    use pimble_plugin_host::wasmtime_engine::{load_bytes, load_file, DEFAULT_BUDGET};

    pub const CAN_ADD: bool = true;
    pub const REMOVABLE: bool = true;

    pub static WHERE: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        format!("Plugins are .wasm files in {}. One added there, replaced or removed takes effect at once. Each asks once on this device before it reads or changes anything.", plugins_dir().display())
    });

    type Loaded = Result<Plugin, (Option<Manifest>, String)>;

    /// What a file was when it was loaded: its length and modified time.
    type Stamp = (u64, Option<SystemTime>);

    /// What each file in the folder loaded as, and the file as it was then: a
    /// file unchanged since is not compiled again.
    static CACHE: Mutex<Option<HashMap<PathBuf, (Stamp, Loaded)>>> = Mutex::new(None);

    fn stamp(path: &Path) -> Option<Stamp> {
        let meta = std::fs::metadata(path).ok()?;
        Some((meta.len(), meta.modified().ok()))
    }

    fn id_of(loaded: &Loaded) -> Option<String> {
        match loaded {
            Ok(plugin) => Some(plugin.manifest.id.clone()),
            Err((Some(manifest), _)) => Some(manifest.id.clone()),
            Err((None, _)) => None,
        }
    }

    /// Every `.wasm` file in `dir`, each compiled again only when its file
    /// changed, and the ids whose plugin changed: new, replaced or gone.
    fn scan(dir: &Path) -> (Registry, Vec<String>) {
        let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        let cache = guard.get_or_insert_with(HashMap::new);
        let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|entries| entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "wasm")).collect())
            .unwrap_or_default();
        paths.sort();
        let mut changed: Vec<String> = Vec::new();
        let mut note = |id: Option<String>| {
            if let Some(id) = id {
                if !changed.contains(&id) {
                    changed.push(id);
                }
            }
        };
        let mut registry = Registry::default();
        for path in &paths {
            let now = stamp(path);
            let same = now.is_some() && cache.get(path).is_some_and(|(was, _)| Some(*was) == now);
            if !same {
                if let Some((_, old)) = cache.get(path) {
                    note(id_of(old));
                }
                let loaded = load_file(path, DEFAULT_BUDGET);
                note(id_of(&loaded));
                cache.insert(path.clone(), (now.unwrap_or((0, None)), loaded));
            }
            let (_, loaded) = &cache[path];
            registry.take(path.clone(), loaded.clone());
        }
        let gone: Vec<PathBuf> = cache.keys().filter(|p| !paths.contains(p)).cloned().collect();
        for path in gone {
            if let Some((_, old)) = cache.remove(&path) {
                note(id_of(&old));
            }
        }
        (registry, changed)
    }

    /// Look at the folder again and hand what changed to the window.
    fn rescan(dir: &Path) {
        let (registry, changed) = scan(dir);
        if changed.is_empty() {
            return;
        }
        tracing::info!("plugins: {} changed in {}", changed.join(", "), dir.display());
        rinch::core::reactive::run_on_main_thread(move || swapped(registry, &changed));
    }

    /// Watch the folder for as long as the app runs: a burst of events (a copy
    /// writes in several steps) is one rescan once it has been quiet for half
    /// a second.
    fn watch(dir: PathBuf) {
        use notify::Watcher;
        let _ = std::fs::create_dir_all(&dir);
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let mut watcher = match notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if event.is_ok_and(|e| e.paths.iter().any(|p| p.extension().is_some_and(|x| x == "wasm"))) {
                let _ = tx.send(());
            }
        }) {
            Ok(w) => w,
            Err(e) => return tracing::warn!("plugins: the folder cannot be watched ({e}); a change there shows at the next start"),
        };
        if let Err(e) = watcher.watch(&dir, notify::RecursiveMode::NonRecursive) {
            return tracing::warn!("plugins: {} cannot be watched ({e}); a change there shows at the next start", dir.display());
        }
        while rx.recv().is_ok() {
            while rx.recv_timeout(Duration::from_millis(500)).is_ok() {}
            rescan(&dir);
        }
    }

    /// Where plugins are installed on this device.
    pub fn plugins_dir() -> PathBuf {
        dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("pimble").join("plugins")
    }

    fn approvals_path() -> PathBuf {
        plugins_dir().join("approvals.json")
    }

    fn provenance_path() -> PathBuf {
        plugins_dir().join("library.json")
    }

    pub fn read_provenance() -> Provenance {
        std::fs::read(provenance_path()).map(|bytes| Provenance::parse(&bytes)).unwrap_or_default()
    }

    pub fn write_provenance(json: &[u8]) {
        let path = provenance_path();
        let written = std::fs::create_dir_all(plugins_dir()).and_then(|_| std::fs::write(&path, json));
        if let Err(e) = written {
            tracing::warn!("plugins: {} could not be saved: {e}", path.display());
        }
    }

    /// pimble.app's library, or another service's (`PIMBLE_LIBRARY_URL`, for
    /// the local stack).
    pub fn library_base() -> String {
        std::env::var("PIMBLE_LIBRARY_URL").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| "https://pimble.app/api/v1/plugins".to_string())
    }

    /// Fetch `url` off the UI thread; `done` hears the bytes on it.
    pub fn fetch(url: String, done: impl FnOnce(Result<Vec<u8>, String>) + Send + 'static) {
        let spawned = std::thread::Builder::new().name("pimble-library".into()).spawn(move || {
            let fetched = (|| {
                let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(120)).build().map_err(|e| e.to_string())?;
                let resp = client.get(&url).send().map_err(|e| e.to_string())?;
                if !resp.status().is_success() {
                    return Err(format!("HTTP {}", resp.status().as_u16()));
                }
                resp.bytes().map(|b| b.to_vec()).map_err(|e| e.to_string())
            })();
            rinch::core::reactive::run_on_main_thread(move || done(fetched));
        });
        if let Err(e) = spawned {
            tracing::warn!("plugins: could not start a download: {e}");
        }
    }

    /// A verified download from the library: loaded (it must be the plugin
    /// the index listed) and put in the folder as "Add from a file..." puts one.
    pub fn install(entry: Entry, bytes: Vec<u8>) {
        let dir = plugins_dir();
        let spawned = std::thread::Builder::new().name("pimble-plugin-install".into()).spawn(move || {
            let put = load_bytes(Path::new(&entry.wasm), &bytes, DEFAULT_BUDGET)
                .map_err(|(_, sentence)| sentence)
                .and_then(|plugin| library::matches(&entry, &plugin.manifest).map(|_| plugin))
                .and_then(|plugin| put(&dir, &bytes, &plugin.manifest));
            match put {
                Ok(()) => {
                    rescan(&dir);
                    rinch::core::reactive::run_on_main_thread(move || installed_from_library(&entry));
                }
                Err(e) => rinch::core::reactive::run_on_main_thread(move || install_failed(&entry, e)),
            }
        });
        if let Err(e) = spawned {
            tracing::warn!("plugins: could not start installing: {e}");
        }
    }

    pub fn read_approvals() -> HashMap<String, Approval> {
        std::fs::read(approvals_path()).map(|bytes| parse_approvals(&bytes)).unwrap_or_default()
    }

    pub fn write_approvals(json: &[u8]) {
        let path = approvals_path();
        let written = std::fs::create_dir_all(plugins_dir()).and_then(|_| std::fs::write(&path, json));
        if let Err(e) = written {
            tracing::warn!("plugins: the approvals could not be saved to {}: {e}", path.display());
        }
    }

    pub fn load() {
        let dir = plugins_dir();
        let spawned = std::thread::Builder::new().name("pimble-plugins".into()).spawn(move || {
            let (registry, changed) = scan(&dir);
            tracing::info!("plugins: {} loaded and {} refused from {}", registry.plugins.len(), registry.refused.len(), dir.display());
            rinch::core::reactive::run_on_main_thread(move || swapped(registry, &changed));
            watch(dir);
        });
        if let Err(e) = spawned {
            tracing::warn!("plugins: could not start loading: {e}");
        }
    }

    /// A wasmtime instance is made on the spot.
    pub fn ready(_plugin: &Plugin, _store: AppStore) -> bool {
        true
    }

    pub fn instantiate(plugin: &Plugin, key: HostKey) -> Result<Box<dyn GuestEngine>, Failure> {
        plugin.instantiate(key, DEFAULT_BUDGET)
    }

    /// "Add from a file...": a `.wasm` the person picks, checked (it must
    /// load), then copied into the folder: over the file that holds the same
    /// plugin now, so a new version replaces the old, else as `<id>.wasm`.
    pub fn pick() {
        let Some(picked) = rinch::dialogs::open_file().set_title("Add Plugin").add_filter("Pimble plugins", &["wasm"]).pick_file() else {
            return;
        };
        let dir = plugins_dir();
        let spawned = std::thread::Builder::new().name("pimble-plugin-add".into()).spawn(move || {
            let (id, line) = add(&dir, &picked);
            rinch::core::reactive::run_on_main_thread(move || {
                if let Some(id) = id {
                    forget_library(&id);
                }
                say(line);
            });
        });
        if let Err(e) = spawned {
            tracing::warn!("plugins: could not start adding: {e}");
        }
    }

    /// The plugin added (by id, when one was) and the sentence saying so.
    fn add(dir: &Path, picked: &Path) -> (Option<String>, String) {
        let file = picked.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let bytes = match std::fs::read(picked) {
            Ok(bytes) => bytes,
            Err(e) => return (None, format!("{file} could not be read: {e}.")),
        };
        let plugin = match load_bytes(picked, &bytes, DEFAULT_BUDGET) {
            Ok(plugin) => plugin,
            Err((_, sentence)) => return (None, sentence),
        };
        let m = &plugin.manifest;
        if let Err(e) = put(dir, &bytes, m) {
            return (None, e);
        }
        rescan(dir);
        (Some(m.id.clone()), format!("Added the {} plugin {}.", m.name, m.version))
    }

    /// Write `bytes`, a plugin that loaded as `manifest`, into the folder:
    /// over the file that holds the same plugin now, so a new version replaces
    /// the old, else as `<id>.wasm`; beside it and renamed over it, so the
    /// watcher never reads half a plugin.
    fn put(dir: &Path, bytes: &[u8], manifest: &Manifest) -> Result<(), String> {
        let id = &manifest.id;
        let holding = CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|cache| {
            cache.iter().find(|(_, (_, loaded))| id_of(loaded).as_deref() == Some(id.as_str())).map(|(path, _)| path.clone())
        });
        let dest = holding.unwrap_or_else(|| dir.join(format!("{id}.wasm")));
        if std::fs::read(&dest).is_ok_and(|have| have == bytes) {
            return Ok(());
        }
        let partial = dest.with_extension("wasm.partial");
        let written = std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&partial, bytes)).and_then(|_| std::fs::rename(&partial, &dest));
        written.map_err(|e| {
            let _ = std::fs::remove_file(&partial);
            format!("The {} plugin could not be put in {}: {e}.", manifest.name, dir.display())
        })
    }

    /// "Remove": the plugin's file leaves the folder, with what it was allowed.
    pub fn remove(id: &str) {
        let Some(path) = with_registry(|r| r.plugin(id).map(|p| p.path.clone())).flatten() else { return };
        let name = manifest_of(id).map_or(id.to_string(), |m| m.name);
        if let Err(e) = std::fs::remove_file(&path) {
            return say(format!("The {name} plugin could not be removed from {}: {e}.", path.display()));
        }
        APPROVALS.with(|a| a.borrow_mut().remove(id));
        super::write_approvals();
        forget_library(id);
        let dir = plugins_dir();
        let _ = std::thread::Builder::new().name("pimble-plugin-remove".into()).spawn(move || rescan(&dir));
        say(format!("Removed the {name} plugin."));
    }
}

/// The browser: plugins the person added, kept in this page's IndexedDB (per
/// browser, never uploaded: nothing is hosted unless asked), compiled by the
/// page's own `WebAssembly`; approvals in `localStorage`.
#[cfg(not(feature = "native"))]
mod place {
    use super::*;
    use pimble_plugin_host::js_engine::{load_bytes, DEFAULT_BUDGET};
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;

    pub const CAN_ADD: bool = true;
    pub const REMOVABLE: bool = true;
    pub const WHERE: &str = "Plugins added here are kept in this browser only, never uploaded. Each asks once in this browser before it reads or changes anything.";

    const APPROVALS_KEY: &str = "pimble.plugins.approvals";

    // The page's own store of plugins: one IndexedDB object store, a
    // plugin's module bytes under its id.
    #[wasm_bindgen(inline_js = r#"
const DB = 'pimble-plugins', STORE = 'plugins';
function open() {
  return new Promise((ok, fail) => {
    const r = indexedDB.open(DB, 1);
    r.onupgradeneeded = () => r.result.createObjectStore(STORE);
    r.onsuccess = () => ok(r.result);
    r.onerror = () => fail(r.error);
  });
}
function done(tx) {
  return new Promise((ok, fail) => {
    tx.oncomplete = () => ok();
    tx.onerror = () => fail(tx.error);
    tx.onabort = () => fail(tx.error);
  });
}
export async function pluginsList() {
  const db = await open();
  const tx = db.transaction(STORE, 'readonly');
  const out = [];
  await new Promise((ok, fail) => {
    const c = tx.objectStore(STORE).openCursor();
    c.onsuccess = () => {
      const cur = c.result;
      if (cur) { out.push([String(cur.key), new Uint8Array(cur.value)]); cur.continue(); } else { ok(); }
    };
    c.onerror = () => fail(c.error);
  });
  db.close();
  return out;
}
export async function pluginsPut(id, bytes) {
  const db = await open();
  const tx = db.transaction(STORE, 'readwrite');
  tx.objectStore(STORE).put(bytes.slice().buffer, id);
  await done(tx);
  db.close();
}
export async function libraryFetch(url) {
  let r;
  try { r = await fetch(url, { credentials: 'omit' }); } catch (e) { throw String(e); }
  if (!r.ok) throw 'HTTP ' + r.status;
  return new Uint8Array(await r.arrayBuffer());
}
export async function pluginsDelete(id) {
  const db = await open();
  const tx = db.transaction(STORE, 'readwrite');
  tx.objectStore(STORE).delete(id);
  await done(tx);
  db.close();
}
"#)]
    extern "C" {
        #[wasm_bindgen(js_name = pluginsList, catch)]
        async fn plugins_list() -> Result<JsValue, JsValue>;
        #[wasm_bindgen(js_name = pluginsPut, catch)]
        async fn plugins_put(id: &str, bytes: &[u8]) -> Result<JsValue, JsValue>;
        #[wasm_bindgen(js_name = pluginsDelete, catch)]
        async fn plugins_delete(id: &str) -> Result<JsValue, JsValue>;
        #[wasm_bindgen(js_name = libraryFetch, catch)]
        async fn library_fetch(url: &str) -> Result<JsValue, JsValue>;
    }

    const PROVENANCE_KEY: &str = "pimble.plugins.library";

    pub fn read_provenance() -> Provenance {
        storage().and_then(|s| s.get_item(PROVENANCE_KEY).ok().flatten()).map(|json| Provenance::parse(json.as_bytes())).unwrap_or_default()
    }

    pub fn write_provenance(json: &[u8]) {
        let saved = storage().map(|s| s.set_item(PROVENANCE_KEY, &String::from_utf8_lossy(json)));
        if !matches!(saved, Some(Ok(()))) {
            tracing::warn!("plugins: where the plugins came from could not be saved in this browser");
        }
    }

    /// The library of the service this page came from.
    pub fn library_base() -> String {
        "/api/v1/plugins".to_string()
    }

    pub fn fetch(url: String, done: impl FnOnce(Result<Vec<u8>, String>) + 'static) {
        wasm_bindgen_futures::spawn_local(async move {
            let fetched = match library_fetch(&url).await {
                Ok(bytes) => Ok(js_sys::Uint8Array::new(&bytes).to_vec()),
                Err(e) => Err(e.as_string().unwrap_or_else(|| format!("{e:?}"))),
            };
            done(fetched);
        });
    }

    /// A verified download from the library, kept as "Add from a file..."
    /// keeps one, once it loads as the plugin the index listed.
    pub fn install(entry: Entry, bytes: Vec<u8>) {
        wasm_bindgen_futures::spawn_local(async move {
            match put(&entry.wasm, &bytes, Some(&entry)).await {
                Ok(_) => installed_from_library(&entry),
                Err(e) => install_failed(&entry, e),
            }
        });
    }

    fn storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok().flatten()
    }

    pub fn read_approvals() -> HashMap<String, Approval> {
        storage().and_then(|s| s.get_item(APPROVALS_KEY).ok().flatten()).map(|json| parse_approvals(json.as_bytes())).unwrap_or_default()
    }

    pub fn write_approvals(json: &[u8]) {
        let saved = storage().map(|s| s.set_item(APPROVALS_KEY, &String::from_utf8_lossy(json)));
        if !matches!(saved, Some(Ok(()))) {
            tracing::warn!("plugins: the approvals could not be saved in this browser");
        }
    }

    /// Every plugin kept in this browser, compiled and described.
    pub fn load() {
        wasm_bindgen_futures::spawn_local(async {
            let mut registry = Registry::default();
            match plugins_list().await {
                Ok(list) => {
                    for entry in js_sys::Array::from(&list).iter() {
                        let entry = js_sys::Array::from(&entry);
                        let id = entry.get(0).as_string().unwrap_or_default();
                        let bytes = js_sys::Uint8Array::new(&entry.get(1)).to_vec();
                        let loaded = load_bytes(&format!("{id}.wasm"), &bytes, DEFAULT_BUDGET).await;
                        registry.take(format!("{id}.wasm").into(), loaded);
                    }
                }
                Err(e) => tracing::warn!("plugins: this browser's plugins could not be read: {e:?}"),
            }
            tracing::info!("plugins: {} loaded and {} refused in this browser", registry.plugins.len(), registry.refused.len());
            loaded(registry);
        });
    }

    /// Whether an instance is ready to mount now; if not, one is made and every
    /// pane showing a metanode looks again once it is.
    pub fn ready(plugin: &Plugin, store: AppStore) -> bool {
        // One that could not be made is taken anyway, so the pane says why.
        if plugin.is_ready() || plugin.is_broken() {
            return true;
        }
        plugin.warm(move || crate::metanodes::plugins_ready(store));
        false
    }

    pub fn instantiate(plugin: &Plugin, key: HostKey) -> Result<Box<dyn GuestEngine>, Failure> {
        plugin.instantiate(key)
    }

    /// "Add plugin...": a `.wasm` file the person picks, compiled and described
    /// here; one this app runs is kept in this browser, replacing an earlier
    /// version of the same plugin, and one it does not is refused with its
    /// sentence and not kept.
    pub fn pick() {
        const INPUT_ID: &str = "pimble-plugin-input";
        let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
        if let Some(old) = document.get_element_by_id(INPUT_ID) {
            old.remove();
        }
        let Some(input) = document.create_element("input").ok().and_then(|el| el.dyn_into::<web_sys::HtmlInputElement>().ok()) else {
            return;
        };
        input.set_id(INPUT_ID);
        input.set_type("file");
        input.set_accept(".wasm,application/wasm");
        input.set_hidden(true);
        let Some(body) = document.body() else { return };
        if body.append_child(&input).is_err() {
            return;
        }
        let picked = input.clone();
        let on_change = wasm_bindgen::closure::Closure::once_into_js(move |_: web_sys::Event| {
            let file = picked.files().and_then(|files| files.get(0));
            picked.remove();
            let Some(file) = file else { return };
            wasm_bindgen_futures::spawn_local(async move {
                let name = file.name();
                say(format!("Adding {name}..."));
                let bytes = match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                    Ok(buffer) => js_sys::Uint8Array::new(&buffer).to_vec(),
                    Err(_) => return say(format!("{name} could not be read.")),
                };
                add(&name, &bytes).await;
            });
        });
        if input.add_event_listener_with_callback("change", on_change.unchecked_ref()).is_err() {
            return;
        }
        input.click();
    }

    async fn add(name: &str, bytes: &[u8]) {
        match put(name, bytes, None).await {
            Ok((id, line)) => {
                forget_library(&id);
                say(line);
            }
            Err(e) => say(e),
        }
    }

    /// Compile and describe `bytes`; one this app runs (and, from the
    /// library, the one `entry` lists) is kept in this browser, replacing an
    /// earlier version of the same plugin, and swapped in.
    async fn put(name: &str, bytes: &[u8], entry: Option<&Entry>) -> Result<(String, String), String> {
        let plugin = load_bytes(name, bytes, DEFAULT_BUDGET).await.map_err(|(_, sentence)| sentence)?;
        if let Some(entry) = entry {
            library::matches(entry, &plugin.manifest)?;
        }
        let id = plugin.manifest.id.clone();
        if let Err(e) = plugins_put(&id, bytes).await {
            tracing::warn!("plugins: {id} could not be kept: {e:?}");
            return Err(format!("The {} plugin could not be kept in this browser.", plugin.manifest.name));
        }
        // An earlier version of the same plugin goes, with any pane showing it.
        let mut registry = REGISTRY.with(|r| r.borrow().clone()).unwrap_or_default();
        registry.plugins.retain(|p| p.manifest.id != id);
        registry.refused.retain(|r| r.file() != format!("{id}.wasm"));
        let line = format!("Added the {} plugin {}.", plugin.manifest.name, plugin.manifest.version);
        registry.take(format!("{id}.wasm").into(), Ok(plugin));
        swapped(registry, std::slice::from_ref(&id));
        Ok((id, line))
    }

    /// "Remove": the plugin leaves this browser, with what it was allowed.
    pub fn remove(id: &str) {
        let id = id.to_string();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = plugins_delete(&id).await {
                tracing::warn!("plugins: {id} could not be removed: {e:?}");
                return say("The plugin could not be removed from this browser.".to_string());
            }
            let name = manifest_of(&id).map_or(id.clone(), |m| m.name);
            APPROVALS.with(|a| a.borrow_mut().remove(&id));
            write_approvals_now();
            forget_library(&id);
            let mut registry = REGISTRY.with(|r| r.borrow().clone()).unwrap_or_default();
            registry.plugins.retain(|p| p.manifest.id != id);
            swapped(registry, std::slice::from_ref(&id));
            say(format!("Removed the {name} plugin from this browser."));
        });
    }

    fn write_approvals_now() {
        super::write_approvals();
    }
}

#[cfg(test)]
mod update_tests {
    use super::*;
    use pimble_plugin_host::abi::{Permissions, ABI_VERSION};

    fn entry(id: &str, version: &str, abi: u32) -> Entry {
        Entry {
            id: id.into(),
            name: id.into(),
            version: version.into(),
            abi,
            author: "Pimble".into(),
            summary: String::new(),
            asks: Permissions::default(),
            wasm: format!("{id}-{version}.wasm"),
            sig: format!("{id}-{version}.wasm.sig"),
            shot: None,
            bytes: 1,
            sha256: String::new(),
        }
    }

    #[test]
    fn only_newer_library_plugins_this_pimble_runs_update_on_their_own() {
        let index = Index {
            v: 1,
            plugins: vec![
                entry("kanban-viewer", "0.2.0", ABI_VERSION),
                entry("todo-viewer", "0.2.0", ABI_VERSION),
                entry("timeline-viewer", "0.2.0", ABI_VERSION + 1),
                entry("calendar-viewer", "0.2.0", ABI_VERSION),
                entry("stuck-viewer", "0.1.0", ABI_VERSION),
            ],
        };
        let installed = |id: &str| match id {
            "kanban-viewer" | "todo-viewer" | "timeline-viewer" => Some("0.1.0".to_string()),
            "stuck-viewer" => Some("0.1.0".to_string()),
            _ => None,
        };
        // todo was added from a file; calendar is not installed; timeline's new
        // version needs a newer Pimble; stuck is current.
        let from_library = |id: &str| id != "todo-viewer";
        let due: Vec<String> = updates_due(&index, installed, from_library).into_iter().map(|e| e.id).collect();
        assert_eq!(due, vec!["kanban-viewer".to_string()]);
    }
}
