//! Watched notes (docs/NOTIFICATIONS_CONTRACT.md section 3, wave 3): a node
//! the person chose to hear about, alone or with everything under it. When
//! someone else changes one, the accounts service tells them in the Inbox
//! ("Joe: 3 changes in a note you watch") and nothing of what changed.
//!
//! The watches are the account's: the server keeps them (sealed on the
//! accounts service so every device of the person's holds the same list)
//! and keeps the documents they reach registered as nodes come and go. This
//! is only how they are chosen and shown: the explorer row's "Watch" menu,
//! the Inbox's list of what is watched, and the switch in Settings >
//! Account. Nothing is watched until the person chooses it.

use pimble_core::{NodeId, StoreId};
use pimble_rpc::WatchEntry;
use rinch::prelude::*;

use crate::protocol::BackendCommand;
use crate::state::AppStore;

/// The server's list arrived.
pub(crate) fn listed(store: AppStore, watches: Vec<WatchEntry>, on: bool) {
    store.watches.set(watches);
    store.notify_watches.set(on);
}

fn own_in(all: &[WatchEntry], store_id: StoreId, node_id: NodeId) -> Option<WatchEntry> {
    all.iter().find(|w| w.store_id == store_id && w.node_id == node_id).copied()
}

/// What the row menu's three items may do for a node, from its own watch
/// and whether any watch with a subtree in its store might reach it (the
/// explorer does not hold every ancestor, so "Unwatch" on such a node is
/// offered and the server says when nothing above reaches it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MenuState {
    pub watch: bool,
    pub watch_subtree: bool,
    pub unwatch: bool,
}

pub(crate) fn menu_state(all: &[WatchEntry], store_id: StoreId, node_id: NodeId) -> MenuState {
    let own = own_in(all, store_id, node_id);
    let a_subtree_above = all.iter().any(|w| w.store_id == store_id && w.node_id != node_id && w.subtree);
    MenuState {
        watch: own.is_none_or(|w| w.subtree),
        watch_subtree: own.is_none_or(|w| !w.subtree),
        unwatch: own.is_some() || a_subtree_above,
    }
}

/// Whether watching can be chosen at all: signed in, with notifications and
/// watched notes on; tracked.
pub(crate) fn can_watch(store: AppStore) -> bool {
    store.cloud_signed_in.get() && store.notifications_on.get() && store.notify_watches.get()
}

pub(crate) fn watch(store: AppStore, store_id: StoreId, node_id: NodeId, subtree: bool) {
    store.send(BackendCommand::WatchNode { store_id, node_id, subtree });
}

pub(crate) fn unwatch(store: AppStore, store_id: StoreId, node_id: NodeId) {
    store.send(BackendCommand::UnwatchNode { store_id, node_id });
}

/// The switch, for the account: off deletes every watch.
pub(crate) fn set_on(store: AppStore, on: bool) {
    store.notify_watches.set(on);
    if !on {
        store.watches.set(Vec::new());
    }
    store.send(BackendCommand::SetWatchesOn { on });
}

/// The Inbox's list of what is watched, each with "Unwatch".
pub(crate) fn render_watched(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    rsx! {
        div {
            class: "pimble-inbox",
            if store.cloud_signed_in.get() && store.notify_watches.get() {
                div {
                    class: "pimble-inbox__head",
                    span { class: "pimble-inbox__heading", "Watching" }
                }
                if store.watches.get().is_empty() {
                    div { class: "pimble-discussion__empty", "Nothing is watched. Choose Watch on a note in the explorer to hear when others change it." }
                }
                for entry in store.watches.get() {
                    div {
                        key: format!("{}:{}:{}", entry.store_id, entry.node_id, entry.subtree),
                        class: "pimble-inbox__row",
                        div {
                            class: "pimble-inbox__title",
                            {store.display_label(entry.store_id, entry.node_id)}
                            Button {
                                variant: "subtle",
                                size: "xs",
                                onclick: move || unwatch(store, entry.store_id, entry.node_id),
                                "Unwatch"
                            }
                        }
                        div { class: "pimble-inbox__where", {if entry.subtree { "With everything under it" } else { "This note alone" }} }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(store_id: StoreId, node_id: NodeId, subtree: bool) -> WatchEntry {
        WatchEntry { store_id, node_id, subtree }
    }

    #[test]
    fn the_menu_offers_what_would_change_something() {
        let (s, n, other) = (StoreId::new(), NodeId::new(), NodeId::new());
        let none = menu_state(&[], s, n);
        assert_eq!(none, MenuState { watch: true, watch_subtree: true, unwatch: false }, "nothing watched by default");
        let alone = menu_state(&[entry(s, n, false)], s, n);
        assert_eq!(alone, MenuState { watch: false, watch_subtree: true, unwatch: true });
        let with_subtree = menu_state(&[entry(s, n, true)], s, n);
        assert_eq!(with_subtree, MenuState { watch: true, watch_subtree: false, unwatch: true });
        let under = menu_state(&[entry(s, other, true)], s, n);
        assert!(under.unwatch, "a subtree watch above may reach it");
        let elsewhere = menu_state(&[entry(StoreId::new(), other, true), entry(s, other, false)], s, n);
        assert!(!elsewhere.unwatch, "another store's, or a watch without its subtree, does not");
    }
}
