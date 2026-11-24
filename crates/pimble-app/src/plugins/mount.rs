//! A pane's plugin view: showing a metanode's viewer in a pane, asking for
//! approval, handing a running view its props and its API answers, and taking
//! it down when it stops or the pane shows something else.

use super::host::{push_host_state, request};
use super::window::refresh_window;
use super::*;

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
    if is_off(&manifest.id) {
        return Outcome::Outline(turned_off_sentence(&manifest.name));
    }
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
