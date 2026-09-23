//! Server-boundary tests for `resolveLink` (docs/LINKS_CONTRACT.md
//! "Following a link"): where a link to a node leads now, through the
//! `became` a transplant leaves on every original's tombstone, judged at
//! every hop as `getNode` judges it.

use std::collections::HashMap;
use std::sync::Arc;

use jsonrpsee::Extensions;
use pimble_core::{LinkResolution, NodeId, StoreId};
use pimble_rpc::{
    CloseStoreRequest, CreateNodeRequest, CreateStoreRequest, DeleteNodeRequest, PimbleApiServer,
    ResolveLinkRequest, TransplantNodeRequest,
};
use pimble_server::{service_extensions, Grant, Principal, Role, RpcHandler};
use pimble_store::StoreManager;
use tokio::sync::RwLock;

struct Fixture {
    handler: RpcHandler,
    _dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self { handler: RpcHandler::new(Arc::new(RwLock::new(StoreManager::new()))), _dir: tempfile::tempdir().unwrap() }
    }

    async fn store(&self, name: &str) -> (StoreId, NodeId) {
        let created = self
            .handler
            .create_store(&service_extensions(), CreateStoreRequest {
                kind: Default::default(),
                store_id: None,
                path: self._dir.path().join(format!("{name}.pimble")),
                name: name.into(),
            })
            .await
            .unwrap();
        (created.store_id, created.root_node_id)
    }

    async fn node(&self, store_id: StoreId, parent: NodeId, title: &str) -> NodeId {
        self.handler
            .create_node(&service_extensions(), CreateNodeRequest {
                store_id,
                parent_id: Some(parent),
                node_type: "document".into(),
                title: title.into(),
            })
            .await
            .unwrap()
            .node_id
    }

    async fn transplant(&self, from: StoreId, node_id: NodeId, to: StoreId, parent: NodeId) -> NodeId {
        self.handler
            .transplant_node(&service_extensions(), TransplantNodeRequest {
                from_store_id: from,
                node_id,
                to_store_id: to,
                new_parent_id: parent,
                position: None,
            })
            .await
            .unwrap()
            .node_id
    }

    async fn resolve_as(&self, ext: &Extensions, store_id: StoreId, node_id: NodeId) -> LinkResolution {
        self.handler.resolve_link(ext, ResolveLinkRequest { store_id, node_id }).await.unwrap().resolution
    }

    async fn resolve(&self, store_id: StoreId, node_id: NodeId) -> LinkResolution {
        self.resolve_as(&service_extensions(), store_id, node_id).await
    }
}

fn user(grants: HashMap<StoreId, Grant>) -> Extensions {
    let mut ext = Extensions::new();
    ext.insert(Principal::User { sub: "member".into(), email: "member@example.com".into(), grants });
    ext
}

#[tokio::test]
async fn a_live_node_a_deleted_one_and_ones_that_are_not_there() {
    let fx = Fixture::new();
    let (store, root) = fx.store("A").await;
    let note = fx.node(store, root, "Note").await;
    assert_eq!(fx.resolve(store, note).await, LinkResolution::Live { store_id: store, node_id: note });

    fx.handler.delete_node(&service_extensions(), DeleteNodeRequest { store_id: store, node_id: note }).await.unwrap();
    assert_eq!(fx.resolve(store, note).await, LinkResolution::Deleted { store_id: store, node_id: note }, "a plain delete became nothing");

    assert_eq!(fx.resolve(store, NodeId::new()).await, LinkResolution::Missing);
    let elsewhere = (StoreId::new(), NodeId::new());
    assert_eq!(
        fx.resolve(elsewhere.0, elsewhere.1).await,
        LinkResolution::StoreNotHere { store_id: elsewhere.0, node_id: elsewhere.1 }
    );
}

#[tokio::test]
async fn a_link_follows_a_node_through_transplants_to_where_it_is_now() {
    let fx = Fixture::new();
    let (a, a_root) = fx.store("A").await;
    let (b, b_root) = fx.store("B").await;
    let folder = fx.node(a, a_root, "Folder").await;
    let inside = fx.node(a, folder, "Inside").await;

    let folder_in_b = fx.transplant(a, folder, b, b_root).await;
    assert_eq!(fx.resolve(a, folder).await, LinkResolution::Live { store_id: b, node_id: folder_in_b });
    // A descendant's tombstone names its own new node, not its root's.
    let LinkResolution::Live { store_id, node_id: inside_in_b } = fx.resolve(a, inside).await else { panic!("the child follows too") };
    assert_eq!(store_id, b);
    assert_ne!(inside_in_b, folder_in_b);

    // And back again: two hops.
    let folder_back = fx.transplant(b, folder_in_b, a, a_root).await;
    assert_eq!(fx.resolve(a, folder).await, LinkResolution::Live { store_id: a, node_id: folder_back });

    // Deleted where it ended: the chain ends in that tombstone.
    fx.handler.delete_node(&service_extensions(), DeleteNodeRequest { store_id: a, node_id: folder_back }).await.unwrap();
    assert_eq!(fx.resolve(a, folder).await, LinkResolution::Deleted { store_id: a, node_id: folder_back });
}

#[tokio::test]
async fn a_link_breaks_for_a_reader_who_cannot_reach_where_the_node_went() {
    let fx = Fixture::new();
    let (a, a_root) = fx.store("A").await;
    let (b, b_root) = fx.store("B").await;
    let note = fx.node(a, a_root, "Note").await;
    let moved = fx.transplant(a, note, b, b_root).await;

    let only_a = user(HashMap::from([(a, Grant::whole(Role::Reader))]));
    assert_eq!(fx.resolve_as(&only_a, a, note).await, LinkResolution::NoAccess, "the new place is in a store they hold no grant for");
    let both = user(HashMap::from([(a, Grant::whole(Role::Reader)), (b, Grant::whole(Role::Reader))]));
    assert_eq!(fx.resolve_as(&both, a, note).await, LinkResolution::Live { store_id: b, node_id: moved });
    let none = user(HashMap::new());
    assert_eq!(fx.resolve_as(&none, a, note).await, LinkResolution::NoAccess, "not even the first hop");
}

#[tokio::test]
async fn a_scoped_member_reaches_only_what_their_share_holds() {
    let fx = Fixture::new();
    let (a, a_root) = fx.store("A").await;
    let shared = fx.node(a, a_root, "Shared").await;
    let inside = fx.node(a, shared, "Inside").await;
    let private = fx.node(a, a_root, "Private").await;

    let member = user(HashMap::from([(a, Grant::Scoped(HashMap::from([(shared, Role::Editor)])))]));
    assert_eq!(fx.resolve_as(&member, a, inside).await, LinkResolution::Live { store_id: a, node_id: inside });
    assert_eq!(fx.resolve_as(&member, a, private).await, LinkResolution::NoAccess);
    assert_eq!(fx.resolve_as(&member, a, NodeId::new()).await, LinkResolution::NoAccess, "outside the share says nothing of existence");
}

#[tokio::test]
async fn this_device_opens_a_closed_store_it_knows_and_a_token_holder_does_not() {
    let fx = Fixture::new();
    let (a, a_root) = fx.store("A").await;
    let note = fx.node(a, a_root, "Note").await;
    fx.handler.close_store(&service_extensions(), CloseStoreRequest { store_id: a }).await.unwrap();

    let reader = user(HashMap::from([(a, Grant::whole(Role::Reader))]));
    assert_eq!(fx.resolve_as(&reader, a, note).await, LinkResolution::StoreNotHere { store_id: a, node_id: note });
    assert_eq!(fx.resolve(a, note).await, LinkResolution::Live { store_id: a, node_id: note }, "the registry knows it");
}

#[tokio::test]
async fn on_a_members_replica_what_it_does_not_hold_and_what_ended_are_not_reachable() {
    use pimble_core::Node;
    use pimble_rpc::OpenStoreRequest;
    use pimble_store::LocalStore;

    let fx = Fixture::new();
    let dir = fx._dir.path();
    let mut owner = LocalStore::create(dir.join("owner.pimble"), "Owner").await.unwrap();
    let root = owner.root_node_id();
    let (kept, _) = owner.create_node(Node::folder("Kept"), Some(root)).unwrap();
    let (in_kept, _) = owner.create_node(Node::document("In kept"), Some(kept)).unwrap();
    let (ended, _) = owner.create_node(Node::folder("Ended"), Some(root)).unwrap();
    let (in_ended, _) = owner.create_node(Node::document("In ended"), Some(ended)).unwrap();
    let (private, _) = owner.create_node(Node::document("Private"), Some(root)).unwrap();

    let path = dir.join("member.pimble");
    let mut replica = LocalStore::create_replica_with_scope(&path, owner.id, "Shares", root, vec![kept, ended]).await.unwrap();
    for id in [kept, in_kept, ended, in_ended] {
        replica.apply_node_update(id, &owner.tree().doc(id).unwrap().save()).unwrap();
    }
    replica.set_ended_roots(vec![ended]).await.unwrap();
    replica.flush().await.unwrap();
    drop(replica);

    fx.handler.open_store(&service_extensions(), OpenStoreRequest { path }).await.unwrap();
    let store = owner.id;
    assert_eq!(fx.resolve(store, in_kept).await, LinkResolution::Live { store_id: store, node_id: in_kept });
    assert_eq!(fx.resolve(store, private).await, LinkResolution::NoAccess, "never held here: outside the shares");
    assert_eq!(fx.resolve(store, in_ended).await, LinkResolution::NoAccess, "held, but the share ended");
}
