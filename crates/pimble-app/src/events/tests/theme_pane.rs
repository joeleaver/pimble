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
