//! A theme node in a pane (docs/THEMES_CONTRACT.md section 9): a pane
//! waiting for one (a link followed, a layout restored at start) shows it in
//! the theme editor when it arrives, the layout remembers it, and emptying
//! the pane stops the editor.

use super::*;
use crate::panes::PaneId;

#[test]
fn a_theme_arriving_for_a_pane_opens_in_the_theme_editor() {
    let (store, events, _commands) = store_with_events();
    let store_id = StoreId::new();
    let mut theme = Node::document("Reading");
    theme.node_type = pimble_core::node_types::THEME.to_string();
    store.select_in_pane(PaneId::FIRST, Some(format!("node_{store_id}_{}", theme.id)));

    events.send(BackendEvent::NodeLoaded { store_id, node: theme.clone() }).unwrap();
    pump(store);
    let pane = store.pane(PaneId::FIRST);
    assert_eq!(untracked(|| pane.theme.get()).map(|t| t.node_id), Some(theme.id));
    assert!(untracked(|| pane.show_theme.get()) && !untracked(|| pane.show_editor.get()));
    let saved = untracked(|| store.saved_panes());
    assert_eq!(saved.documents.iter().map(|d| d.node_id).collect::<Vec<_>>(), vec![theme.id], "the layout remembers the theme");

    store.clear_pane(PaneId::FIRST);
    assert!(untracked(|| pane.theme.get()).is_none() && !untracked(|| pane.show_theme.get()));
}

/// docs/THEMES_CONTRACT.md section 3.1a "Editing or making a theme shows
/// it" (Joe, 2026-11-26): a theme made (`ThemeMade`) or edited turns Themes
/// on and opens like a followed link; when its place arrives
/// (`AncestorsLoaded`, with its kind) the parent's Themes row opens
/// and the theme's row is selected with the cursor on it.
#[test]
fn a_theme_made_or_edited_is_shown_under_themes() {
    use pimble_core::internal::InternalKind;
    let (store, events, _commands) = store_with_events();
    let tree_state = UseTreeReturn::new(UseTreeOptions::default());
    let (store_id, root, theme) = (StoreId::new(), NodeId::new(), NodeId::new());
    let shown = || untracked(|| store.internal_shown[InternalKind::Theme.index()].get());
    assert!(!shown(), "hidden by default");

    events.send(BackendEvent::ThemeMade { store_id, node_id: theme }).unwrap();
    process_backend_events(store, tree_state);
    assert!(shown(), "making a theme shows Themes");
    let value = format!("node_{store_id}_{theme}");
    assert_eq!(untracked(|| store.link_open.get()).map(|open| open.value), Some(value.clone()), "it opens as a link does");

    // Opening it selected it; its place arrives.
    store.select_in_pane(store.focused(), Some(value.clone()));
    events.send(BackendEvent::AncestorsLoaded { store_id, node_id: theme, ancestors: vec![root], kind: Some(InternalKind::Theme) }).unwrap();
    process_backend_events(store, tree_state);
    let expanded = untracked(|| tree_state.expanded.get());
    let group = crate::internal_drop::row_to_open(store_id, root, "", InternalKind::Theme);
    assert!(expanded.contains(&group), "{group} opened");
    assert!(expanded.contains(&format!("store_{store_id}")));
    assert_eq!(untracked(|| tree_state.controller.cursor()), Some(value.clone()), "the cursor is on it");
    assert!(untracked(|| tree_state.selected.get()).contains(&value));

    // "Edit" in the picker on a hidden theme shows Themes too.
    store.internal_shown[InternalKind::Theme.index()].set(false);
    store.link_open.set(None);
    crate::themes::open_theme_node(store, store_id, theme);
    assert!(shown());
    assert!(untracked(|| store.link_open.get()).is_some());
}

/// A followed link's reveal of an ordinary node opens no group and leaves
/// the cursor where it is.
#[test]
fn an_ordinary_reveal_opens_no_group() {
    let (store, events, _commands) = store_with_events();
    let tree_state = UseTreeReturn::new(UseTreeOptions::default());
    let (store_id, root, note) = (StoreId::new(), NodeId::new(), NodeId::new());
    let value = format!("node_{store_id}_{note}");
    store.select_in_pane(store.focused(), Some(value.clone()));
    events.send(BackendEvent::AncestorsLoaded { store_id, node_id: note, ancestors: vec![root], kind: None }).unwrap();
    process_backend_events(store, tree_state);
    assert!(untracked(|| tree_state.expanded.get()).iter().all(|v| !crate::state::is_internal_group(v)));
    assert_eq!(untracked(|| tree_state.controller.cursor()), None);
}

/// docs/TREE_CONTRACT.md "Internal groups": once a theme moves, the group
/// it shows in is opened: the one dropped into, or under the row dropped
/// on (a mount's own Themes for a drop on a mount row). A drop into a group
/// that this move does not answer (refused, or a move made elsewhere) opens
/// nothing of it.
#[test]
fn a_moved_theme_is_seen_in_its_group() {
    use crate::state::{internal_group_value, set_last_drop_target_value, test_fixtures::a_store_with_themes};
    use pimble_core::internal::InternalKind;
    let (store, t) = a_store_with_themes();
    let (cmd_tx, _cmd_rx) = crossbeam_channel::bounded::<BackendCommand>(64);
    let (events, event_rx) = crossbeam_channel::bounded::<BackendEvent>(64);
    store.backend.set(Some(BackendHandle { cmd_tx, event_rx }));
    let s = t.store_id;
    let mount = Node::document("Mounted");
    let m = mount.id;
    store.upsert_node(s, mount);
    let source = pimble_core::MountRef { source_store: s, source_node: t.folder, source_path: None, source_remote: None };
    store.set_mount_state(s, m, pimble_core::MountState::Live, source);
    store.internal_shown[InternalKind::Theme.index()].set(true);
    let moved = |node_id, new_parent_id| BackendEvent::NodeMoved { store_id: s, node_id, old_parent_id: t.root, new_parent_id };
    let opened = |tree_state: UseTreeReturn| untracked(|| tree_state.expanded.get());

    // Into the folder's Themes: the folder and its group open.
    let tree_state = UseTreeReturn::new(UseTreeOptions::default());
    let folder_themes = internal_group_value(s, t.folder, InternalKind::Theme, "");
    untracked(|| store.build_tree_data_structural());
    set_last_drop_target_value(folder_themes.clone());
    events.send(moved(t.book, t.folder)).unwrap();
    process_backend_events(store, tree_state);
    let open = opened(tree_state);
    assert!(open.contains(&folder_themes) && open.contains(&format!("node_{s}_{}", t.folder)), "{open:?}");

    // On the mount row: the mount and the mount's own Themes open.
    let tree_state = UseTreeReturn::new(UseTreeOptions::default());
    let mount_row = format!("node_{s}_{m}");
    set_last_drop_target_value(mount_row.clone());
    events.send(moved(t.code, t.folder)).unwrap();
    process_backend_events(store, tree_state);
    let open = opened(tree_state);
    let mount_themes = internal_group_value(s, m, InternalKind::Theme, &format!("/{s}_{m}"));
    assert!(open.contains(&mount_row) && open.contains(&mount_themes), "{open:?}");

    // A group drop left behind by a move into somewhere else opens nothing
    // of that group.
    let tree_state = UseTreeReturn::new(UseTreeOptions::default());
    set_last_drop_target_value(folder_themes.clone());
    events.send(moved(t.inner, t.root)).unwrap();
    process_backend_events(store, tree_state);
    let open = opened(tree_state);
    assert!(!open.contains(&folder_themes), "{open:?}");
    assert!(open.contains(&internal_group_value(s, t.root, InternalKind::Theme, "")), "the root's Themes, where it went");
}
