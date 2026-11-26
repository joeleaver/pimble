//! The stores the state tests start from.

use super::*;

/// A store with one child under its root, and the child's canonical pair.
pub(crate) fn store_with_a_child() -> (AppStore, StoreId, NodeId) {
    let app = AppStore::new();
    let mut store = Store::new_local("Notes", "/tmp/notes.pimble".into());
    let root = Node::folder("Notes");
    store.root_node_id = root.id;
    let child = Node::document("Recipes");
    let (store_id, child_id) = (store.id, child.id);
    app.upsert_store(store);
    app.upsert_node(store_id, root.clone());
    app.upsert_node(store_id, child);
    app.set_children(store_id, root.id, vec![(store_id, child_id)]);
    (app, store_id, child_id)
}

pub(super) fn member(email: &str, role: pimble_rpc::MemberRole) -> pimble_rpc::ShareMember {
    pimble_rpc::ShareMember { email: email.to_string(), role, status: pimble_rpc::ShareMemberStatus::Active, name: String::new() }
}

pub(super) fn marker(name: &str) -> pimble_core::ShareMarker {
    pimble_core::ShareMarker {
        v: pimble_core::ShareMarker::VERSION,
        key_id: uuid::Uuid::new_v4(),
        url: "https://pimble.app".to_string(),
        name: name.to_string(),
    }
}

/// A replica of two shares, with a document under each, parents and
/// lists as the server sends them.
pub(super) fn a_replica_of_two_shares() -> (AppStore, StoreId, [NodeId; 2], [NodeId; 2]) {
    let app = AppStore::new();
    let mut store = Store::new_local("Shared by ann@example.com", "/tmp/anns.pimble".into());
    let (mut recipes, mut trips) = (Node::folder("Recipes"), Node::folder("Trips"));
    let (mut pasta, mut rome) = (Node::document("Pasta"), Node::document("Rome"));
    pasta.parent_id = Some(recipes.id);
    rome.parent_id = Some(trips.id);
    recipes.children = vec![pasta.id];
    trips.children = vec![rome.id];
    store.root_node_id = recipes.id;
    store.roots = vec![recipes.id, trips.id];
    store.shared_by = Some("ann@example.com".to_string());
    store.is_replica = true;
    let store_id = store.id;
    app.upsert_store(store);
    app.set_children(store_id, recipes.id, vec![(store_id, pasta.id)]);
    app.set_children(store_id, trips.id, vec![(store_id, rome.id)]);
    let ids = ([recipes.id, trips.id], [pasta.id, rome.id]);
    for node in [recipes, trips, pasta, rome] {
        app.upsert_node(store_id, node);
    }
    (app, store_id, ids.0, ids.1)
}

/// The nodes of [`a_store_with_themes`].
pub(crate) struct WithThemes {
    pub store_id: StoreId,
    pub root: NodeId,
    pub recipes: NodeId,
    pub book: NodeId,
    pub code: NodeId,
    /// A folder holding the theme `inner`.
    pub folder: NodeId,
    pub inner: NodeId,
}

/// A store whose root holds `Book` (a theme), `Recipes`, `Code` (a theme)
/// and the folder `Shared`, which holds the theme `Inner`.
pub(crate) fn a_store_with_themes() -> (AppStore, WithThemes) {
    let (app, store_id, recipes) = store_with_a_child();
    let root = app.root_node_id(store_id).unwrap();
    let theme = |name: &str| {
        let mut node = Node::document(name);
        node.node_type = pimble_core::node_types::THEME.to_string();
        node
    };
    let (book, code, folder, mut inner) = (theme("Book"), theme("Code"), Node::folder("Shared"), theme("Inner"));
    let ids = WithThemes { store_id, root, recipes, book: book.id, code: code.id, folder: folder.id, inner: inner.id };
    inner.parent_id = Some(ids.folder);
    for mut n in [book, code, folder] {
        n.parent_id = Some(root);
        app.upsert_node(store_id, n);
    }
    app.upsert_node(store_id, inner);
    app.set_children(store_id, root, vec![(store_id, ids.book), (store_id, recipes), (store_id, ids.code), (store_id, ids.folder)]);
    app.set_children(store_id, ids.folder, vec![(store_id, ids.inner)]);
    (app, ids)
}
