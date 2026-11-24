//! A kanban's column opened on its own (docs/PLUGINS_CONTRACT.md, ABI 7):
//! what a pane shows follows the column's place, and is judged again as its
//! parent arrives or changes and as the plugins load; a parent the window
//! lacks is asked for once, and only for a node a pane holds.

use super::*;
use crate::panes::PaneId;

fn column_viewer() -> pimble_plugin_abi::ChildViewerDecl {
    pimble_plugin_abi::ChildViewerDecl { id: "kanban-column".into(), name: "Kanban column".into(), of: "kanban".into(), icon: Some("layout-columns".into()) }
}

fn kanban(title: &str) -> Node {
    let mut node = Node::document(title);
    node.metadata.metanode = pimble_core::metanode::Metanode::from_stored(Some("kanban"), Some("children 2"));
    node
}

fn column_under(parent: NodeId) -> Node {
    let mut column = Node::document("Done");
    column.parent_id = Some(parent);
    column
}

fn open_in_first_pane(store: AppStore, store_id: StoreId, node_id: NodeId) {
    store.select_in_pane(PaneId::FIRST, Some(format!("node_{store_id}_{node_id}")));
}

/// The node the first pane shows as a metanode, if it shows one.
fn shown_as_metanode(store: AppStore) -> Option<NodeId> {
    untracked(|| store.pane(PaneId::FIRST).metanode.get()).map(|m| m.node_id)
}

/// How many of `commands` ask for node `node_id`.
fn asked_for(commands: &[BackendCommand], node_id: NodeId) -> usize {
    commands.iter().filter(|c| matches!(c, BackendCommand::GetNode { node_id: n, .. } if *n == node_id)).count()
}

fn loaded(events: &crossbeam_channel::Sender<BackendEvent>, store: AppStore, store_id: StoreId, node: &Node) {
    events.send(BackendEvent::NodeLoaded { store_id, node: node.clone() }).unwrap();
    pump(store);
}

/// A column opened from a link before its kanban is held shows as text and
/// asks for its parent, once however often it arrives; the kanban arriving
/// turns the pane into the column view, and the column moved out of the
/// kanban turns it back.
#[test]
fn a_pane_shows_a_column_once_its_kanban_arrives_and_stops_when_it_leaves() {
    let (store, events, commands) = store_with_events();
    store.child_viewers.set(vec![column_viewer()]);
    let store_id = StoreId::new();
    let board = kanban("Board");
    let mut column = column_under(board.id);
    open_in_first_pane(store, store_id, column.id);

    loaded(&events, store, store_id, &column);
    loaded(&events, store, store_id, &column);
    assert_eq!(shown_as_metanode(store), None, "its kanban is not held yet");
    let sent: Vec<BackendCommand> = commands.try_iter().collect();
    assert_eq!(asked_for(&sent, board.id), 1, "the parent is asked for once");

    loaded(&events, store, store_id, &board);
    assert_eq!(shown_as_metanode(store), Some(column.id), "the kanban arriving shows the column");
    let sent: Vec<BackendCommand> = commands.try_iter().collect();
    assert!(sent.iter().any(|c| matches!(c, BackendCommand::GetMetanode { node_id, .. } if *node_id == column.id)), "the column's forest is asked for");

    let archive = Node::folder("Archive");
    store.upsert_node(store_id, archive.clone());
    column.parent_id = Some(archive.id);
    loaded(&events, store, store_id, &column);
    assert_eq!(shown_as_metanode(store), None, "moved out of the kanban, it is a plain node again");
}

/// A pane restored onto a column before the plugins loaded shows it as text;
/// the plugins arriving judge every pane again and show the column.
#[test]
fn the_plugins_loading_after_a_restore_show_the_column() {
    let (store, events, _commands) = store_with_events();
    let store_id = StoreId::new();
    let board = kanban("Board");
    let column = column_under(board.id);
    store.upsert_node(store_id, board);
    open_in_first_pane(store, store_id, column.id);
    loaded(&events, store, store_id, &column);
    assert_eq!(shown_as_metanode(store), None, "no plugin provides a column viewer yet");

    store.child_viewers.set(vec![column_viewer()]);
    crate::metanodes::plugins_ready(store);
    assert_eq!(shown_as_metanode(store), Some(column.id));

    store.child_viewers.set(Vec::new());
    crate::metanodes::plugins_ready(store);
    assert_eq!(shown_as_metanode(store), None, "the plugin removed, the column is a plain node again");
}

/// Nothing is asked for a node no pane holds, nor for a share's scope root
/// on a member's replica (its parent is outside what the device may read),
/// nor for a mount.
#[test]
fn no_parent_is_asked_for_a_scope_root_a_mount_or_a_node_no_pane_holds() {
    let (store, events, commands) = store_with_events();
    store.child_viewers.set(vec![column_viewer()]);
    let mut replica = pimble_core::Store::new_local("Shared with you", "/tmp/shared.pimble".into());
    let outside = NodeId::new();
    let mut scope_root = Node::folder("Recipes");
    scope_root.parent_id = Some(outside);
    replica.root_node_id = scope_root.id;
    replica.roots = vec![scope_root.id];
    let store_id = replica.id;
    store.upsert_store(replica);

    // Edits to a row no pane holds.
    let unheld_parent = NodeId::new();
    let mut row = Node::document("Pasta");
    row.parent_id = Some(unheld_parent);
    loaded(&events, store, store_id, &row);
    loaded(&events, store, store_id, &scope_root);

    // The scope root opened in a pane.
    open_in_first_pane(store, store_id, scope_root.id);
    loaded(&events, store, store_id, &scope_root);

    // A mount opened in a pane.
    let mount_parent = NodeId::new();
    let mut mount = Node::mount(store_id, scope_root.id);
    mount.parent_id = Some(mount_parent);
    open_in_first_pane(store, store_id, mount.id);
    loaded(&events, store, store_id, &mount);

    let sent: Vec<BackendCommand> = commands.try_iter().collect();
    for parent in [unheld_parent, outside, mount_parent] {
        assert_eq!(asked_for(&sent, parent), 0, "asked for {parent:?}");
    }
}
