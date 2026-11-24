//! Which viewer shows a node, and the icon that comes with it: a metanode is
//! shown by its own viewer, and a node whose parent is a metanode may be shown
//! by a child viewer of the parent's (a kanban's column, opened on its own,
//! by the kanban's column viewer: docs/PLUGINS_CONTRACT.md, ABI 7). Nothing
//! is stored for a child viewer: it follows from where the node is.

use pimble_core::{Node, StoreId};
use pimble_plugin_abi::ChildViewerDecl;
use rinch::prelude::*;

use super::AppStore;

impl AppStore {
    /// The icon a viewer's metanodes show when they have none of their own:
    /// the one its plugin's recipe makes them with ("layout-kanban"), so a
    /// node turned into a kanban looks like one made with New > Kanban.
    pub fn viewer_icon(&self, viewer: &str) -> Option<String> {
        untracked(|| self.recipes.with(|rs| rs.iter().find(|r| r.recipe.viewer == viewer).and_then(|r| r.recipe.icon.clone())))
    }

    /// The child viewer that shows `node` (of store `store_id`) because of
    /// where it is: its parent is a metanode, and an installed plugin provides
    /// a child viewer for the parent's viewer. A metanode of its own is shown
    /// by its own viewer, and a query metanode's children are not what it
    /// shows, so neither has one. `None` too while the parent is not held:
    /// the parent arriving judges the panes holding its children again
    /// (`crate::events`, `take_node`).
    pub fn child_viewer(&self, store_id: StoreId, node: &Node) -> Option<ChildViewerDecl> {
        if node.metadata.metanode.is_some() {
            return None;
        }
        let parent = node.parent_id?;
        let parent = untracked(|| self.node_data.with(|map| map.get(&(store_id, parent)).copied()))?;
        let viewer = untracked(|| parent.with(|p| p.metadata.metanode.as_ref().filter(|m| !m.is_query()).map(|m| m.viewer.clone())))?;
        untracked(|| self.child_viewers.with(|all| all.iter().find(|c| c.of == viewer).cloned()))
    }

    /// The icon `node` (of store `store_id`) shows in its row and its pane's
    /// strip: its own, else its viewer's (`viewer_icon`), else its child
    /// viewer's (`child_viewer`), else its type's.
    pub fn node_icon(&self, store_id: StoreId, node: &Node) -> String {
        if node.metadata.icon().is_none() {
            if let Some(m) = node.metadata.metanode.as_ref().filter(|m| !m.is_query()) {
                if let Some(icon) = self.viewer_icon(&m.viewer) {
                    return icon;
                }
            }
            if let Some(icon) = self.child_viewer(store_id, node).and_then(|c| c.icon) {
                return icon;
            }
        }
        pimble_ui::appearance::node_icon(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_fixtures::store_with_a_child;

    fn column_viewer() -> ChildViewerDecl {
        ChildViewerDecl { id: "kanban-column".into(), name: "Kanban column".into(), of: "kanban".into(), icon: Some("layout-columns".into()) }
    }

    /// A child of a kanban is shown by the kanban's column viewer once its
    /// plugin is known, with that viewer's icon; nothing is stored on it, so
    /// moving it out or turning the parent into something else ends it, and
    /// a node of its own viewer or a chosen icon wins.
    #[test]
    fn a_kanbans_child_is_shown_by_its_column_viewer() {
        let (app, store_id, child_id) = store_with_a_child();
        let kanban = Node::document("Board");
        let parent_id = kanban.id;
        app.upsert_node(store_id, kanban);
        let mut column = app.get_node_signal(store_id, child_id).map(|sig| untracked(|| sig.get())).expect("the child is held");
        column.parent_id = Some(parent_id);
        app.upsert_node(store_id, column.clone());
        let set_parent_viewer = |viewer: Option<&str>| {
            let sig = app.get_node_signal(store_id, parent_id).expect("the parent is held");
            sig.update(|p| p.metadata.metanode = viewer.and_then(|v| pimble_core::metanode::Metanode::from_stored(Some(v), Some("children 2"))));
        };
        set_parent_viewer(Some("kanban"));
        assert_eq!(app.child_viewer(store_id, &column), None, "no plugin provides one yet");
        assert_eq!(app.node_icon(store_id, &column), "file");
        let before = app.row_snapshot(store_id, child_id);
        app.child_viewers.set(vec![column_viewer()]);
        assert_eq!(app.child_viewer(store_id, &column).map(|c| c.id), Some("kanban-column".to_string()));
        assert_eq!(app.node_icon(store_id, &column), "layout-columns");
        assert_ne!(app.row_snapshot(store_id, child_id), before, "the row re-renders once the child viewer is known");

        let mut chosen = column.clone();
        chosen.metadata.set_icon(Some("star".into()));
        assert_eq!(app.node_icon(store_id, &chosen), "star");
        let mut own = column.clone();
        own.metadata.metanode = pimble_core::metanode::Metanode::from_stored(Some("kanban"), Some("children 2"));
        assert_eq!(app.child_viewer(store_id, &own), None, "a lane is a kanban of its own");
        let mut elsewhere = column.clone();
        elsewhere.parent_id = None;
        assert_eq!(app.child_viewer(store_id, &elsewhere), None);

        set_parent_viewer(Some("gallery"));
        assert_eq!(app.child_viewer(store_id, &column), None, "no child viewer for that viewer");
        set_parent_viewer(None);
        assert_eq!(app.child_viewer(store_id, &column), None, "a plain parent gives none");
    }
}
