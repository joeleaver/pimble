//! The other notes a sheet's formulas read (docs/SHEETS_CONTRACT.md sections
//! 6 and 7): resolved once each, so a deleted note reads `#REF!` "… was
//! deleted", one this account cannot read "You can't read …", one not here
//! yet "Loading…", and one that became another (a move out of a share) the
//! note it became; and held while read, subscribed as a pane holds a note, so
//! a peer's edit of it recomputes the sheets that read it.
//!
//! The node cache is what a read note's tables are computed from (a pane of
//! this window holding the note has a fresher copy, and that wins): a read
//! note's updates are merged into the cache here as they arrive.

use std::collections::HashSet;

use pimble_core::{LinkResolution, PimbleUrl};

use super::*;

/// Where a read note's tables come from, or why they cannot be read.
pub(super) enum Where {
    /// Read this node (the note itself, or the note it became).
    Read(NodeRef),
    Deleted,
    NoAccess,
    Missing,
    Loading,
}

struct Read {
    /// `None` while `resolveLink` is asked.
    resolution: Option<LinkResolution>,
    /// The node subscribed for it, while some sheet reads it.
    subscribed: Option<NodeRef>,
}

thread_local! {
    static READS: RefCell<HashMap<NodeRef, Read>> = RefCell::new(HashMap::new());
    /// The `resolveLink`s asked here, by URL: their answers are this module's.
    static ASKED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// How note `n` is read, asking for its resolution the first time.
pub(super) fn reach(store: AppStore, n: &NodeRef) -> Where {
    let resolution = READS.with(|r| r.borrow().get(n).map(|read| read.resolution.clone()));
    match resolution {
        None => {
            READS.with(|r| r.borrow_mut().insert(*n, Read { resolution: None, subscribed: None }));
            let url = PimbleUrl::node(n.store, n.node);
            ASKED.with(|a| a.borrow_mut().insert(url.to_string()));
            store.send(BackendCommand::ResolveLink { url, hops: 0 });
            // What is cached is shown meanwhile.
            Where::Read(*n)
        }
        Some(None) => Where::Read(*n),
        Some(Some(LinkResolution::Live { store_id, node_id })) => Where::Read(node_ref(store_id, node_id)),
        Some(Some(LinkResolution::Deleted { .. })) => Where::Deleted,
        Some(Some(LinkResolution::NoAccess)) => Where::NoAccess,
        Some(Some(LinkResolution::Missing)) => Where::Missing,
        Some(Some(LinkResolution::StoreNotHere { .. })) => Where::Loading,
    }
}

/// A `resolveLink` answer: `true` when it was asked here (the caller then
/// follows nothing).
pub(crate) fn link_resolved(store: AppStore, url: &PimbleUrl, resolution: &LinkResolution) -> bool {
    if !ASKED.with(|a| a.borrow_mut().remove(&url.to_string())) {
        return false;
    }
    let n = node_ref(url.store, url.node);
    READS.with(|r| {
        if let Some(read) = r.borrow_mut().get_mut(&n) {
            read.resolution = Some(resolution.clone());
        }
    });
    if let LinkResolution::Live { store_id, node_id } = resolution {
        if store.get_node_signal(*store_id, *node_id).is_none() {
            store.send(BackendCommand::GetNode { store_id: *store_id, node_id: *node_id });
        }
    }
    recompute_all(store);
    // Live now (or no longer): what is subscribed follows.
    sync(store);
    true
}

fn recompute_all(store: AppStore) {
    for pane in PaneId::ALL {
        if with_session(pane, |_| ()).is_some() {
            recompute(store, pane);
        }
    }
}

/// What the sheets of this window read now (each session's `reads`):
/// subscribe what is newly read, let go of what is no longer read.
pub(super) fn sync(store: AppStore) {
    let mut wanted: HashSet<NodeRef> = HashSet::new();
    for pane in PaneId::ALL {
        if let Some(reads) = with_session(pane, |s| s.reads.clone()) {
            wanted.extend(reads);
        }
    }
    // Decide first: whether a node is held already is asked of the panes and
    // of the reads as they stand (`AppStore::is_editing` counts both).
    let changes: Vec<(NodeRef, Option<NodeRef>, Option<NodeRef>)> = READS.with(|r| {
        r.borrow()
            .iter()
            .filter_map(|(n, read)| {
                let target = match &read.resolution {
                    Some(LinkResolution::Live { store_id, node_id }) => Some(node_ref(*store_id, *node_id)),
                    _ => None,
                };
                let want = if wanted.contains(n) { target } else { None };
                (read.subscribed != want).then_some((*n, read.subscribed, want))
            })
            .collect()
    });
    let mut subscribe = Vec::new();
    for (_, _, want) in &changes {
        if let Some(t) = want {
            if !store.is_editing(t.store, t.node) {
                subscribe.push(*t);
            }
        }
    }
    READS.with(|r| {
        let mut r = r.borrow_mut();
        for (n, _, want) in &changes {
            if let Some(read) = r.get_mut(n) {
                read.subscribed = *want;
            }
        }
        // A note no longer read is forgotten: read again, it is resolved
        // again (it may have been deleted or put back meanwhile).
        r.retain(|n, read| wanted.contains(n) || read.resolution.is_none());
    });
    for t in subscribe {
        store.send(BackendCommand::SubscribeNodeChanges { store_id: t.store, node_id: t.node });
        // What the cache has may be as old as its last fetch.
        store.send(BackendCommand::GetNode { store_id: t.store, node_id: t.node });
    }
    for (_, old, _) in changes {
        if let Some(t) = old {
            if !store.is_editing(t.store, t.node) {
                store.send(BackendCommand::UnsubscribeNodeChanges { store_id: t.store, node_id: t.node });
            }
        }
    }
}

/// Whether a sheet of this window reads node `node_id` and holds its
/// subscription (counted with the panes holding nodes, `AppStore::is_editing`).
pub(crate) fn holds(store_id: StoreId, node_id: NodeId) -> bool {
    let t = node_ref(store_id, node_id);
    READS.with(|r| r.borrow().values().any(|read| read.subscribed == Some(t)))
}

/// A peer's update of a node: a read note's cached copy takes it (the
/// sheets reading it recompute after, `apply_remote`).
pub(crate) fn remote(store: AppStore, store_id: StoreId, node_id: NodeId, bytes: &[u8]) {
    if !holds(store_id, node_id) {
        return;
    }
    if let Some(sig) = store.get_node_signal(store_id, node_id) {
        sig.update(|node| node.content = merged(&node.content, bytes));
    }
}

/// The connection came back: every read note is subscribed and fetched again.
pub(crate) fn reconnected(store: AppStore) {
    let held: Vec<NodeRef> = READS.with(|r| r.borrow().values().filter_map(|read| read.subscribed).collect::<HashSet<_>>().into_iter().collect());
    for t in held {
        store.send(BackendCommand::SubscribeNodeChanges { store_id: t.store, node_id: t.node });
        store.send(BackendCommand::GetNode { store_id: t.store, node_id: t.node });
    }
}
