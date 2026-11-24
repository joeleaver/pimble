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

mod host;
mod library_client;
mod mount;
mod window;

pub(crate) use mount::{allow, decline, hide, render_slot, reply, show, Outcome};
pub(crate) use window::{hidden, offer_install, open, render_page, shown};

use host::watch_host_state;
use library_client::{refresh_library, schedule_update_check, FIRST_UPDATE_CHECK_MS};
use window::{refresh_window, PluginsUi};

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
    /// The plugin a `pimble:settings/plugins?install=<id>` link asked about:
    /// its library row is marked and says what to press.
    static ASKED: RefCell<Option<String>> = const { RefCell::new(None) };
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
        // The child viewers they provide (a kanban's column), for the
        // children of their viewers' metanodes.
        store.child_viewers.set(child_viewers());
        // Rows of metanodes with no icon of their own show their viewer's.
        store.bump_tree_structure();
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

/// The child viewers every loaded plugin provides, in the folder's order.
fn child_viewers() -> Vec<pimble_plugin_abi::ChildViewerDecl> {
    with_registry(|registry| registry.plugins.iter().flat_map(|p| p.manifest.child_viewers().cloned()).collect()).unwrap_or_default()
}

/// A row's recipe item: make what `recipe` says under `parent`, through the
/// app's own API, as the person's own edit (no plugin code runs).
pub(crate) fn make(store: AppStore, store_id: pimble_core::StoreId, parent_id: pimble_core::NodeId, recipe: RecipeDecl, place: pimble_core::Place) {
    store.send(BackendCommand::MakeFromRecipe { store_id, parent_id, recipe, place });
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

/// The name a person knows store `store` by.
fn store_name(store: AppStore, id: pimble_core::StoreId) -> String {
    untracked(|| store.store_data.with(|map| map.get(&id).map(|s| s.with(|s| s.name.clone()))))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "this store".to_string())
}

// ── Where plugins live, per target ────────────────────────────────────────────

/// The desktop: a folder of `.wasm` files, compiled under wasmtime off the UI
/// thread and watched, so a plugin added, replaced or removed there (by
/// Settings, `scripts/plugins/install.sh` or by hand) takes effect at once;
/// approvals in a file beside them.
#[cfg(feature = "native")]
#[path = "place_desktop.rs"]
mod place;

/// The browser: plugins the person added, kept in this page's IndexedDB (per
/// browser, never uploaded: nothing is hosted unless asked), compiled by the
/// page's own `WebAssembly`; approvals in `localStorage`.
#[cfg(not(feature = "native"))]
#[path = "place_browser.rs"]
mod place;
