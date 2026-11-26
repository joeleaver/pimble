//! What the host knows, for each plugin view (who is where, whom its pane
//! follows, the theme), and what a view asks of the app.

use super::*;

/// Hand every mounted plugin what the app knows that it draws with, now and
/// whenever it changes: who is in which node, whom its pane follows, the
/// theme (docs/PRESENCE_CONTRACT.md "Plugins").
pub(super) fn watch_host_state(store: AppStore) {
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

pub(super) fn push_host_state(store: AppStore, pane: PaneId) {
    let Some((instance, plugin_id)) = SHOWN.with(|s| s.borrow().get(&pane).and_then(|shown| Some((shown.instance.clone()?, shown.plugin_id.clone())))) else {
        return;
    };
    let state = host_state(store, pane, &approval_of(&plugin_id));
    instance.set_host_state(&state);
}

/// What the plugin in pane `pane` asked of the app.
pub(super) fn request(store: AppStore, pane: PaneId, plugin_id: &str, request: HostRequest) {
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
