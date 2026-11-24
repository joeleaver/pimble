//! The picker of "What it can reach": the explorer's own tree, a row per store
//! and node, each with None / Read / Edit.

use std::collections::HashSet;

use pimble_core::{NodeId, StoreId};
use pimble_rpc::AgentRole;
use rinch::prelude::*;

use super::inherited;
use crate::protocol::BackendCommand;
use crate::state::AppStore;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Row {
    pub(super) store: StoreId,
    /// `None` for the store's own row.
    pub(super) node: Option<NodeId>,
    pub(super) depth: usize,
    pub(super) expanded: bool,
}

pub(super) fn row_label(store: AppStore, row: Row) -> String {
    match row.node {
        Some(n) => store.display_label(row.store, n),
        None => store.get_store_signal(row.store).map(|s| untracked(|| s.with(|s| s.name.clone()))).unwrap_or_default(),
    }
}

pub(super) fn rows(store: AppStore, expanded: &HashSet<(StoreId, NodeId)>) -> Vec<Row> {
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

pub(super) fn toggle_row(store: AppStore, store_id: StoreId, node: Option<NodeId>) {
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

pub(super) fn set_role(store: AppStore, store_id: StoreId, node: Option<NodeId>, role: Option<AgentRole>) {
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
pub(super) fn role_buttons(store_id: StoreId, node: Option<NodeId>) -> [(&'static str, Option<AgentRole>, StoreId, Option<NodeId>); 3] {
    [
        ("None", None, store_id, node),
        ("Read", Some(AgentRole::Reader), store_id, node),
        ("Edit", Some(AgentRole::Editor), store_id, node),
    ]
}

pub(super) fn role_tip(role: Option<AgentRole>) -> &'static str {
    match role {
        None => "Not chosen here",
        Some(AgentRole::Reader) => "It may read this and everything inside",
        Some(AgentRole::Editor) => "It may read and change this and everything inside",
    }
}

pub(super) fn role_style(store: AppStore, store_id: StoreId, node: Option<NodeId>, role: Option<AgentRole>) -> String {
    let own = store.agents_ui.editing.get().and_then(|e| e.grants.get(&(store_id, node)).copied());
    let on = own == role;
    format!("{ROLE_BTN}{}", if on { " background: var(--rinch-primary-color); color: white; border-color: var(--rinch-primary-color);" } else { "" })
}

/// "reads, from above" on a row nothing is chosen on that inherits a role.
pub(super) fn from_above(store: AppStore, store_id: StoreId, node: Option<NodeId>) -> String {
    let Some(e) = store.agents_ui.editing.get() else { return String::new() };
    match (e.grants.get(&(store_id, node)), inherited(store, &e.grants, store_id, node)) {
        (None, Some(AgentRole::Reader)) => "reads, from above".into(),
        (None, Some(AgentRole::Editor)) => "edits, from above".into(),
        _ => String::new(),
    }
}

pub(super) fn chevron(row: Row) -> &'static str {
    if row.expanded { "▾" } else { "▸" }
}

const ROLE_BTN: &str = "padding: 2px 8px; border-radius: 4px; font-size: 12px; cursor: pointer; border: 1px solid var(--rinch-color-border);";
