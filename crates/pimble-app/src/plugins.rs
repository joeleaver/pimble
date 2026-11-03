//! Plugins in the desktop app (docs/PLUGINS_CONTRACT.md wave 1): the plugins folder
//! and the viewer a pane mounts for a metanode.
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
//! Wave 1's props are the metanode's address and its source's forest; the plugin
//! reads and writes through the Pimble API from wave 2.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Weak;
use std::sync::OnceLock;

use pimble_api::{Addr, Forest};
use pimble_plugin_host::abi::{ForestRow, NodeAddr, Props};
use pimble_plugin_host::wasmtime_engine::{Registry, ViewerLookup, DEFAULT_BUDGET};
use pimble_plugin_host::{ApplierOptions, Instance, InstanceOptions};
use rinch::core::dom::{DomDocument, NodeId};
use rinch::prelude::*;

use crate::panes::PaneId;
use crate::state::{display_label_from_node, AppStore};

/// Where plugins are installed on this device.
pub fn plugins_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("pimble").join("plugins")
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
    /// The running plugin; `None` once it stopped.
    instance: Option<Instance>,
    /// Why it stopped, while the pane still shows that metanode.
    stopped: Option<String>,
}

thread_local! {
    static STORE: Cell<Option<AppStore>> = const { Cell::new(None) };
    static SLOTS: RefCell<HashMap<PaneId, Slot>> = RefCell::new(HashMap::new());
    static SHOWN: RefCell<HashMap<PaneId, Shown>> = RefCell::new(HashMap::new());
}

/// Load the plugins folder off the UI thread; panes showing a metanode look again
/// when it is done.
pub(crate) fn load(store: AppStore) {
    STORE.with(|s| s.set(Some(store)));
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
    /// No plugin provides the viewer (or the folder is still loading): the outline
    /// with the usual line.
    Missing,
}

/// Show `forest`'s metanode in pane `pane` with the plugin providing `viewer`:
/// mount it, or hand the one already mounted its new props.
pub(crate) fn show(store: AppStore, pane: PaneId, viewer: &str, forest: &Forest) -> Outcome {
    let props = props(forest);
    let same = SHOWN.with(|s| {
        s.borrow().get(&pane).is_some_and(|shown| shown.metanode == forest.metanode && shown.viewer == viewer)
    });
    if same {
        let (instance, stopped) = SHOWN.with(|s| {
            let s = s.borrow();
            let shown = s.get(&pane).expect("checked above");
            (shown.instance.clone(), shown.stopped.clone())
        });
        if let Some(sentence) = stopped {
            return Outcome::Outline(sentence);
        }
        if let Some(instance) = instance {
            return if instance.props_changed(&props) { Outcome::Shown } else { stopped_sentence(pane) };
        }
    }
    hide(pane);
    let Some(registry) = REGISTRY.get() else { return Outcome::Missing };
    let plugin = match registry.viewer(viewer) {
        ViewerLookup::Ready(plugin) => plugin,
        ViewerLookup::Refused(sentence) => return Outcome::Outline(sentence.to_string()),
        ViewerLookup::Missing => return Outcome::Missing,
    };
    let Some((doc, node)) = SLOTS.with(|s| s.borrow().get(&pane).and_then(|slot| Some((slot.doc.upgrade()?, slot.node)))) else {
        return Outcome::Missing;
    };
    let name = plugin.manifest.name.clone();
    let options = InstanceOptions { applier: ApplierOptions { text_field_shim: true } };
    let started = Instance::start(&name, doc, node, options, |key| plugin.instantiate(key, DEFAULT_BUDGET));
    let instance = match started {
        Ok(instance) => instance,
        Err(sentence) => {
            remember(pane, forest.metanode, viewer, None, Some(sentence.clone()));
            return Outcome::Outline(sentence);
        }
    };
    instance.on_stop(move |sentence| stopped(store, pane, sentence));
    remember(pane, forest.metanode, viewer, Some(instance.clone()), None);
    if instance.mount(viewer, &props) {
        Outcome::Shown
    } else {
        stopped_sentence(pane)
    }
}

fn remember(pane: PaneId, metanode: Addr, viewer: &str, instance: Option<Instance>, stopped: Option<String>) {
    SHOWN.with(|s| s.borrow_mut().insert(pane, Shown { metanode, viewer: viewer.to_string(), instance, stopped }));
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

fn addr(a: Addr) -> NodeAddr {
    NodeAddr { store: a.store.to_string(), node: a.node.to_string() }
}

/// Wave 1's props: the metanode and its source's forest, as the outline shows it.
fn props(forest: &Forest) -> Props {
    Props {
        metanode: addr(forest.metanode),
        title: display_label_from_node(&forest.node),
        forest: forest
            .entries
            .iter()
            .map(|e| ForestRow {
                addr: addr(e.addr),
                parent: e.parent.map(addr),
                depth: e.depth,
                label: display_label_from_node(&e.node),
                node_type: e.node.node_type.clone(),
            })
            .collect(),
        settings: None,
    }
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
