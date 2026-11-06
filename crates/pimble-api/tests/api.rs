//! The API over a real server (`RpcBackend` against an in-process
//! `PimbleServer`): naming, held documents kept current by the
//! subscription, the one write, links followed where nodes went, creation
//! under mounts, refusals before anything is sent, and a subscription made
//! again after it ends.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::AbortHandle;
use pimble_api::backend::{ChangeStream, SyncedDoc};
use pimble_api::rpc::RpcBackend;
use pimble_api::{recipes, Addr, ApiError, Backend, Change, Moved, NodeInit, Pimble};
use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, BlobId, BlobUrl, DeletedNode, LinkResolution, Node, NodeId, Place, Store, StoreId};
use pimble_rpc::{AgentGrant, AgentRole, PresenceFrame, SearchResultItem};
use pimble_server::{PimbleServer, ServerConfig};

struct Stack {
    server: PimbleServer,
    dir: tempfile::TempDir,
    url: String,
    service: PimbleClient,
}

async fn stack() -> Stack {
    let dir = tempfile::tempdir().unwrap();
    let mut server = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        credentials_path: Some(dir.path().join("credentials.json")),
        keystore_path: Some(dir.path().join("keys.json")),
        replicas_dir: Some(dir.path().join("replicas")),
        agents_path: Some(dir.path().join("agents.json")),
        ..Default::default()
    });
    server.start().await.expect("server starts");
    let url = format!("http://{}", server.addr());
    let service = PimbleClient::connect(&url).await.expect("service connects");
    Stack { server, dir, url, service }
}

/// The RPC backend, counting what it asks the server for, and able to end
/// its subscriptions as a dropped connection would.
struct Counting {
    inner: RpcBackend,
    fetches: Arc<AtomicUsize>,
    applies: Arc<AtomicUsize>,
    subs: Arc<Mutex<Vec<AbortHandle>>>,
}

#[derive(Clone)]
struct Counts {
    fetches: Arc<AtomicUsize>,
    applies: Arc<AtomicUsize>,
    subs: Arc<Mutex<Vec<AbortHandle>>>,
}

impl Counts {
    fn fetches(&self) -> usize {
        self.fetches.load(Ordering::SeqCst)
    }
    fn applies(&self) -> usize {
        self.applies.load(Ordering::SeqCst)
    }
    /// End every subscription, as a connection that went away ends them.
    fn cut(&self) {
        for h in self.subs.lock().unwrap().drain(..) {
            h.abort();
        }
    }
}

fn counting(client: PimbleClient) -> (Counting, Counts) {
    let counts = Counts { fetches: Default::default(), applies: Default::default(), subs: Default::default() };
    let backend = Counting {
        inner: RpcBackend::over(client),
        fetches: counts.fetches.clone(),
        applies: counts.applies.clone(),
        subs: counts.subs.clone(),
    };
    (backend, counts)
}

#[async_trait]
impl Backend for Counting {
    async fn list_stores(&self) -> Result<Vec<Store>, ApiError> {
        self.inner.list_stores().await
    }
    async fn fetch_one(&self, store: StoreId, node: NodeId) -> Result<Node, ApiError> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        self.inner.fetch_one(store, node).await
    }
    async fn fetch(&self, store: StoreId, nodes: Vec<NodeId>) -> Result<Vec<Node>, ApiError> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        self.inner.fetch(store, nodes).await
    }
    async fn children(&self, store: StoreId, node: NodeId) -> Result<(StoreId, Vec<Node>), ApiError> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        self.inner.children(store, node).await
    }
    async fn sync(&self, store: StoreId, have: Vec<(NodeId, Vec<u8>)>, list_unknown: bool) -> Result<(Vec<SyncedDoc>, Vec<NodeId>), ApiError> {
        self.inner.sync(store, have, list_unknown).await
    }
    async fn apply(&self, store: StoreId, node: NodeId, update: Vec<u8>, client_id: &str) -> Result<(), ApiError> {
        self.applies.fetch_add(1, Ordering::SeqCst);
        self.inner.apply(store, node, update, client_id).await
    }
    async fn create_node(&self, store: StoreId, parent: NodeId, init: NodeInit, place: Place) -> Result<NodeId, ApiError> {
        self.inner.create_node(store, parent, init, place).await
    }
    async fn create_attachment(&self, store: StoreId, host: NodeId, init: NodeInit) -> Result<NodeId, ApiError> {
        self.inner.create_attachment(store, host, init).await
    }
    async fn move_node(&self, store: StoreId, node: NodeId, parent: NodeId, place: Place) -> Result<Moved, ApiError> {
        self.inner.move_node(store, node, parent, place).await
    }
    async fn move_nodes(&self, store: StoreId, nodes: Vec<NodeId>, parent: NodeId, place: Place) -> Result<Vec<Moved>, ApiError> {
        self.inner.move_nodes(store, nodes, parent, place).await
    }
    async fn transplant(&self, from: StoreId, node: NodeId, to: StoreId, parent: NodeId, place: Place) -> Result<Moved, ApiError> {
        self.inner.transplant(from, node, to, parent, place).await
    }
    async fn delete(&self, store: StoreId, node: NodeId) -> Result<(), ApiError> {
        self.inner.delete(store, node).await
    }
    async fn undelete(&self, store: StoreId, node: NodeId) -> Result<(), ApiError> {
        self.inner.undelete(store, node).await
    }
    async fn resolve_link(&self, store: StoreId, node: NodeId) -> Result<LinkResolution, ApiError> {
        self.inner.resolve_link(store, node).await
    }
    async fn list_deleted(&self, store: StoreId) -> Result<Vec<DeletedNode>, ApiError> {
        self.inner.list_deleted(store).await
    }
    async fn list_attachments(&self, store: StoreId, host: NodeId) -> Result<Vec<Node>, ApiError> {
        self.inner.list_attachments(store, host).await
    }
    async fn search(&self, query: &str, stores: Vec<StoreId>, semantic: bool, limit: usize) -> Result<Vec<SearchResultItem>, ApiError> {
        self.inner.search(query, stores, semantic, limit).await
    }
    async fn backlinks(&self, store: StoreId, node: NodeId) -> Result<Vec<NodeId>, ApiError> {
        self.inner.backlinks(store, node).await
    }
    async fn put_blob(&self, store: StoreId, node: NodeId, mime: &str, bytes: &[u8]) -> Result<BlobUrl, ApiError> {
        self.inner.put_blob(store, node, mime, bytes).await
    }
    async fn get_blob(&self, store: StoreId, blob: BlobId) -> Result<(String, Vec<u8>), ApiError> {
        self.inner.get_blob(store, blob).await
    }
    async fn publish_presence(&self, frame: PresenceFrame, client_id: &str) -> Result<(), ApiError> {
        self.inner.publish_presence(frame, client_id).await
    }
    async fn who_am_i(&self) -> Result<(Option<String>, String), ApiError> {
        self.inner.who_am_i().await
    }
    async fn subscribe(&self, store: StoreId) -> Result<ChangeStream, ApiError> {
        let stream = self.inner.subscribe(store).await?;
        let (stream, handle) = futures::stream::abortable(stream);
        self.subs.lock().unwrap().push(handle);
        Ok(Box::pin(stream))
    }
}

async fn api(stack: &Stack) -> (Pimble, Counts) {
    let client = PimbleClient::connect(&stack.url).await.expect("connects");
    let (backend, counts) = counting(client);
    (Pimble::new(backend, "test:api"), counts)
}

/// Wait for `api` to hear a change about `node`.
async fn heard(api: &Pimble, node: NodeId) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match api.next_change().await {
                Some(Change::Store(c)) if pimble_api::backend::document_of(&c.kind) == Some(node) => return,
                Some(_) => continue,
                None => panic!("nothing is subscribed"),
            }
        }
    })
    .await
    .expect("the change arrives");
}

#[tokio::test]
async fn names_paths_and_ambiguity() {
    let s = stack().await;
    let (store, root) = s.service.create_store(s.dir.path().join("family.pimble"), "Family").await.unwrap();
    let home = s.service.create_node(store, Some(root), "folder", "Home").await.unwrap();
    let work = s.service.create_node(store, Some(root), "folder", "Work").await.unwrap();
    let home_budget = s.service.create_node(store, Some(home), "document", "Budget").await.unwrap();
    let work_budget = s.service.create_node(store, Some(work), "document", "Budget").await.unwrap();
    let (api, _) = api(&s).await;

    // From the store's name, a link, an id, and from anywhere.
    assert_eq!(api.resolve("Family/Home/Budget").await.unwrap(), Addr::new(store, home_budget));
    assert_eq!(api.resolve(&Addr::new(store, work_budget).link()).await.unwrap(), Addr::new(store, work_budget));
    assert_eq!(api.resolve(&work_budget.to_string()).await.unwrap(), Addr::new(store, work_budget));
    assert_eq!(api.resolve("Work/Budget").await.unwrap(), Addr::new(store, work_budget));
    assert_eq!(api.resolve("family/WORK/budget").await.unwrap(), Addr::new(store, work_budget));
    // A name that answers to several is listed, never guessed.
    match api.resolve("Budget").await {
        Err(ApiError::Ambiguous { candidates, sentence }) => {
            assert_eq!(candidates.len(), 2, "{sentence}");
            assert!(sentence.contains("Family/Home/Budget") && sentence.contains("Family/Work/Budget"), "{sentence}");
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(api.resolve("Family/Nowhere").await, Err(ApiError::NotFound { .. })));
    assert!(matches!(api.resolve("Nowhere/Budget").await, Err(ApiError::NotFound { .. })));

    assert_eq!(api.path(Addr::new(store, home_budget)).await, "Family/Home/Budget");
    assert_eq!(api.short_name(Addr::new(store, home_budget)).await, "Home/Budget");
    assert_eq!(api.ancestors(Addr::new(store, home_budget)).await.unwrap(), vec![Addr::new(store, home), Addr::new(store, root)]);
    let _ = { s }.server.stop().await;
}

#[tokio::test]
async fn walks_are_lookups_once_a_store_is_held() {
    let s = stack().await;
    let (store, root) = s.service.create_store(s.dir.path().join("work.pimble"), "Work").await.unwrap();
    let folder = s.service.create_node(store, Some(root), "folder", "Projects").await.unwrap();
    for i in 0..5 {
        s.service.create_node(store, Some(folder), "document", format!("Note {i}")).await.unwrap();
    }
    let (api, counts) = api(&s).await;
    let row = api.find_store("Work").await.unwrap();
    let first = api.walk(&row).await.unwrap();
    assert_eq!(first.len(), 7);
    assert!(first.iter().any(|w| w.path == "Work/Projects/Note 3"));
    let asked = counts.fetches();
    let again = api.walk(&row).await.unwrap();
    assert_eq!(again.len(), 7);
    assert_eq!(counts.fetches(), asked, "a second walk asks the server for nothing");

    // A node made elsewhere is heard of and held, and the next walk has it.
    let made = s.service.create_node(store, Some(folder), "document", "Made elsewhere").await.unwrap();
    heard(&api, made).await;
    let after = api.walk(&row).await.unwrap();
    assert!(after.iter().any(|w| w.addr.node == made && w.path == "Work/Projects/Made elsewhere"));
    assert_eq!(counts.fetches(), asked);
    let _ = { s }.server.stop().await;
}

#[tokio::test]
async fn the_one_write_and_documents_kept_current() {
    let s = stack().await;
    let (store, root) = s.service.create_store(s.dir.path().join("work.pimble"), "Work").await.unwrap();
    let (api, counts) = api(&s).await;

    // A recipe: the node and its first content in one create.
    let addr = api.create(Addr::new(store, root), Place::Last, recipes::document("Plans", Some("# Plans\n\nFirst")).unwrap()).await.unwrap();
    let node = api.node(addr).await.unwrap();
    assert_eq!(node.metadata.title, "Plans");
    assert_eq!(node.metadata.custom.get(pimble_core::custom_keys::EXPLICIT_TITLE), Some(&serde_json::json!(true)));
    assert_eq!(api.doc(addr).await.unwrap().markdown().unwrap().trim_end(), "# Plans\n\nFirst");

    let edited = api.edit(addr, |doc, _| doc.append_markdown("Second").map(drop)).await.unwrap();
    assert!(edited.changed && edited.caret.is_some());
    let server = s.service.get_node(store, addr.node).await.unwrap();
    assert!(pimble_crdt::NodeDoc::load(&server.content).unwrap().markdown().unwrap().contains("Second"));

    // An edit that changes nothing is not sent.
    let applies = counts.applies();
    let same = api.edit(addr, |_, _| Ok::<(), ApiError>(())).await.unwrap();
    assert!(!same.changed);
    assert_eq!(counts.applies(), applies);

    // Someone else's edit reaches what is held by the subscription alone.
    s.service.edit_node(store, addr.node, "elsewhere", |doc, _| doc.append_markdown("From elsewhere").map(drop)).await.unwrap();
    heard(&api, addr.node).await;
    let fetches = counts.fetches();
    assert!(api.doc(addr).await.unwrap().markdown().unwrap().contains("From elsewhere"));
    assert_eq!(counts.fetches(), fetches);

    // A closure's refusal sends nothing and says why.
    let refused = api.edit(addr, |doc, _| doc.replace_text("not in the note", "x").map(drop)).await;
    assert!(matches!(refused, Err(ApiError::Refused { .. })), "{refused:?}");
    assert_eq!(counts.applies(), applies);
    let _ = { s }.server.stop().await;
}

#[tokio::test]
async fn a_subscription_that_ends_is_made_again_and_catches_up() {
    let s = stack().await;
    let (store, root) = s.service.create_store(s.dir.path().join("work.pimble"), "Work").await.unwrap();
    let note = s.service.create_node(store, Some(root), "document", "Plans").await.unwrap();
    let (api, counts) = api(&s).await;
    let addr = Addr::new(store, note);
    api.node(addr).await.unwrap();
    api.listen();

    counts.cut();
    // Missed while nothing listened.
    s.service.edit_node(store, note, "elsewhere", |doc, _| doc.append_markdown("Missed").map(drop)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    // The next read finds the subscription gone, makes it again and
    // reconciles what it holds.
    assert!(api.doc(addr).await.unwrap().markdown().unwrap().contains("Missed"));
    let resynced = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match api.next_change().await {
                Some(Change::Resynced { store: st, .. }) => return st,
                Some(_) => continue,
                None => panic!("nothing is subscribed"),
            }
        }
    })
    .await
    .expect("the resync is said");
    assert_eq!(resynced, store);
    let _ = { s }.server.stop().await;
}

#[tokio::test]
async fn a_moved_link_is_followed_where_the_node_went() {
    let s = stack().await;
    let (a, a_root) = s.service.create_store(s.dir.path().join("a.pimble"), "A").await.unwrap();
    let (b, b_root) = s.service.create_store(s.dir.path().join("b.pimble"), "B").await.unwrap();
    let note = s.service.create_node(a, Some(a_root), "document", "Traveller").await.unwrap();
    let (api, _) = api(&s).await;
    let old = Addr::new(a, note);
    let moved = api.move_node(old, Addr::new(b, b_root), Place::Last).await.unwrap();
    assert!(moved.new_link && moved.addr.store == b);
    // The old link leads to where it went (`became`), not to a tombstone.
    assert_eq!(api.resolve(&old.link()).await.unwrap(), moved.addr);
    assert_eq!(api.path(moved.addr).await, "B/Traveller");
    let _ = { s }.server.stop().await;
}

#[tokio::test]
async fn whatever_is_made_under_a_mount_goes_to_its_source() {
    let s = stack().await;
    let (home, home_root) = s.service.create_store(s.dir.path().join("home.pimble"), "Home").await.unwrap();
    let (lib, lib_root) = s.service.create_store(s.dir.path().join("lib.pimble"), "Library").await.unwrap();
    let shelf = s.service.create_node(lib, Some(lib_root), "folder", "Shelf").await.unwrap();
    let (mount, _) = s.service.create_mount(home, home_root, lib, shelf, Some("Shelf here".into())).await.unwrap();
    let (api, _) = api(&s).await;
    let under = Addr::new(home, mount);

    let doc = api.create(under, Place::Last, recipes::document("Book", None).unwrap()).await.unwrap();
    assert_eq!(doc.store, lib);
    let topic = recipes::discussion("About the shelf", None, None).unwrap();
    let standalone = api.create(under, Place::Last, topic).await.unwrap();
    assert_eq!(standalone.store, lib);
    let listed: Vec<Addr> = api.children(under).await.unwrap().into_iter().map(|(a, _)| a).collect();
    assert_eq!(listed, vec![doc, standalone]);
    // And by path, through the mount.
    assert_eq!(api.resolve("Home/Shelf here/Book").await.unwrap(), doc);
    let node = api.node(standalone).await.unwrap();
    assert_eq!(node.metadata.custom.get(pimble_core::custom_keys::ICON), Some(&serde_json::json!(recipes::DISCUSSION_ICON)));
    let _ = { s }.server.stop().await;
}

#[tokio::test]
async fn a_reader_is_refused_before_anything_is_sent() {
    let s = stack().await;
    let (store, root) = s.service.create_store(s.dir.path().join("work.pimble"), "Work").await.unwrap();
    let note = s.service.create_node(store, Some(root), "document", "Plans").await.unwrap();
    let created = s.service.create_agent("Reader", vec![AgentGrant { store, node: None, role: AgentRole::Reader }]).await.unwrap();
    let agent = PimbleClient::connect_with_auth(&s.url, &AuthMethod::Bearer { token: created.key.clone() }).await.unwrap();
    let (backend, counts) = counting(agent);
    let api = Pimble::new(backend, format!("agent:{}", created.agent.id));
    let refused = api.edit(Addr::new(store, note), |doc, _| doc.append_markdown("No").map(drop)).await;
    assert_eq!(refused.unwrap_err(), ApiError::ReadOnly);
    assert_eq!(counts.applies(), 0);
    // Its identity is asked once and kept.
    let me = api.me().await.expect("an agent has an identity");
    assert_eq!(me.name, "Reader");
    let _ = { s }.server.stop().await;
}

/// What `pimble_core::plan` reads, answered from held documents: a node's
/// parent and children, its label, a mount and its source, a share marker,
/// and the stores' facts.
#[tokio::test]
async fn the_tree_view_reads_held_documents() {
    use pimble_api::plan;
    let s = stack().await;
    let (home, home_root) = s.service.create_store(s.dir.path().join("home.pimble"), "Home").await.unwrap();
    let (lib, lib_root) = s.service.create_store(s.dir.path().join("lib.pimble"), "Library").await.unwrap();
    let shelf = s.service.create_node(lib, Some(lib_root), "folder", "Shelf").await.unwrap();
    let (mount, _) = s.service.create_mount(home, home_root, lib, shelf, Some("Shelf here".into())).await.unwrap();
    let (api, _) = api(&s).await;
    let first = recipes::document("", Some("A first line that is longer than the label")).unwrap();
    let first = api.create(Addr::new(home, home_root), Place::Last, first).await.unwrap();
    let titled = api.create(Addr::new(home, home_root), Place::Last, recipes::document("Plans", Some("Not the label")).unwrap()).await.unwrap();

    // A node not held is not known, never a guess. (The two just made may
    // be held already: a node made is held from the change that announces
    // it, once that is heard.)
    let never = NodeId::new();
    assert_eq!(api.view(|v| v.parent(home, never)), None);
    assert_eq!(api.view(|v| v.label(home, never)), plan::UNTITLED);

    api.hold_store(home).await.unwrap();
    api.stores().await.unwrap();
    api.view(|v| {
        assert_eq!(v.parent(home, titled.node), Some(home_root));
        assert_eq!(v.children(home, home_root), Some(vec![mount, first.node, titled.node]));
        assert_eq!(v.label(home, titled.node), "Plans");
        assert_eq!(v.label(home, first.node), "A first line that is long…");
        assert!(v.is_mount(home, mount) && !v.is_mount(home, titled.node));
        assert_eq!(v.mount_source(home, mount), Some((lib, shelf)));
        let facts = v.store(home).expect("an open store");
        assert_eq!((facts.name.as_str(), facts.root, facts.linked), ("Home", home_root, false));
        assert_eq!(plan::access(v, home, titled.node), pimble_core::StoreAccess::Full);
        assert_eq!(plan::ancestors(v, home, titled.node), vec![titled.node, home_root]);
    });

    // A share marker on a node is read from its document.
    let marker = pimble_core::ShareMarker { v: 1, key_id: uuid::Uuid::new_v4(), url: "https://pimble.app".into(), name: "Family".into() };
    api.edit(titled, |doc, at| doc.set_custom(pimble_core::custom_keys::SHARE, &serde_json::to_value(&marker).unwrap(), at)).await.unwrap();
    assert!(api.view(|v| v.is_shared(home, titled.node)));
    let _ = { s }.server.stop().await;
}

/// `plan_move` holds what it judges and answers the app's refusals and
/// sentences; `move_nodes` sends what it planned.
#[tokio::test]
async fn moves_are_planned_over_held_documents_and_sent_as_planned() {
    use pimble_api::plan::{self, MoveStep};
    let s = stack().await;
    let (home, home_root) = s.service.create_store(s.dir.path().join("home.pimble"), "Home").await.unwrap();
    let (lib, lib_root) = s.service.create_store(s.dir.path().join("lib.pimble"), "Library").await.unwrap();
    let trips = s.service.create_node(home, Some(home_root), "folder", "Trips").await.unwrap();
    let moab = s.service.create_node(home, Some(trips), "document", "Moab").await.unwrap();
    let zion = s.service.create_node(home, Some(trips), "document", "Zion").await.unwrap();
    let inbox = s.service.create_node(home, Some(home_root), "folder", "Inbox").await.unwrap();
    let (api, _) = api(&s).await;
    let a = |n| Addr::new(home, n);

    // Into itself, or beside a store, or a store's root: refused before
    // anything is sent, with the explorer's sentences.
    let refused = api.plan_move(&[a(trips)], a(moab), Place::Last).await.unwrap_err();
    assert_eq!(refused.sentence(), plan::INTO_ITSELF);
    let refused = api.plan_move(&[a(moab)], a(trips), Place::After(home_root)).await.unwrap_err();
    assert_eq!(refused.sentence(), plan::BESIDE_A_STORE);
    let refused = api.plan_move(&[a(home_root)], a(trips), Place::Last).await.unwrap_err();
    assert_eq!(refused.sentence(), plan::NOT_A_NODE);

    // Where it already is: nothing to send.
    let stays = api.plan_move(&[a(zion)], a(trips), Place::Last).await.unwrap();
    assert!(stays.stays() && stays.line == plan::WHERE_IT_IS, "{stays:?}");

    // Two nodes of one store: one `moveNodes`, said as the strip says it.
    let plan = api.plan_move(&[a(moab), a(zion)], a(inbox), Place::First).await.unwrap();
    assert_eq!(plan.line, "2 items into Inbox, first");
    assert!(matches!(plan.steps.as_slice(), [MoveStep::MoveMany { nodes, .. }] if nodes == &vec![moab, zion]));
    assert_eq!(plan.warning, None);
    let moved = api.move_nodes(&plan).await.unwrap();
    assert_eq!(moved.iter().map(|m| m.addr).collect::<Vec<_>>(), vec![a(moab), a(zion)]);
    let listed: Vec<Addr> = s.service.get_children(home, inbox).await.unwrap().1.into_iter().map(|n| a(n.id)).collect();
    assert_eq!(listed, vec![a(moab), a(zion)]);

    // Into another store: a transplant, and the warning that says so.
    let plan = api.plan_move(&[a(moab)], Addr::new(lib, lib_root), Place::Last).await.unwrap();
    assert_eq!(plan.line, "Into Library");
    assert!(plan.warning.as_deref().is_some_and(|w| w.starts_with("A new node in Library")), "{:?}", plan.warning);
    assert!(matches!(plan.steps.as_slice(), [MoveStep::Transplant { .. }]));
    let moved = api.move_nodes(&plan).await.unwrap();
    assert!(moved[0].new_link && moved[0].addr.store == lib);

    // Out of a share: said before it happens.
    let marker = pimble_core::ShareMarker { v: 1, key_id: uuid::Uuid::new_v4(), url: "https://pimble.app".into(), name: "Family".into() };
    api.edit(a(inbox), |doc, at| doc.set_custom(pimble_core::custom_keys::SHARE, &serde_json::to_value(&marker).unwrap(), at)).await.unwrap();
    let plan = api.plan_move(&[a(zion)], a(trips), Place::Last).await.unwrap();
    assert_eq!(plan.warning.as_deref(), Some(plan::leaves_share_warning(1, "Inbox").as_str()));

    // A mount whose store is not open here has nowhere to put anything.
    let (mount, _) = s.service.create_mount(home, home_root, lib, lib_root, Some("Library here".into())).await.unwrap();
    // Closed since the stores were listed.
    s.service.close_store(lib).await.unwrap();
    let refused = api.plan_move(&[a(trips)], a(mount), Place::Last).await.unwrap_err();
    assert_eq!(refused.sentence(), plan::MOUNT_NOT_HERE);
    assert_eq!(api.create_parent(a(mount)).await.unwrap_err().sentence(), plan::MOUNT_NOT_HERE);
    let _ = { s }.server.stop().await;
}

/// A reader is refused a move and a delete before anything is sent.
#[tokio::test]
async fn a_reader_cannot_plan_a_move_or_a_delete() {
    let s = stack().await;
    let (store, root) = s.service.create_store(s.dir.path().join("work.pimble"), "Work").await.unwrap();
    let folder = s.service.create_node(store, Some(root), "folder", "Projects").await.unwrap();
    let note = s.service.create_node(store, Some(root), "document", "Plans").await.unwrap();
    let created = s.service.create_agent("Reader", vec![AgentGrant { store, node: None, role: AgentRole::Reader }]).await.unwrap();
    let agent = PimbleClient::connect_with_auth(&s.url, &AuthMethod::Bearer { token: created.key.clone() }).await.unwrap();
    let api = Pimble::new(RpcBackend::over(agent), "agent:reader");
    let (note, folder) = (Addr::new(store, note), Addr::new(store, folder));
    assert_eq!(api.plan_move(&[note], folder, Place::Last).await.unwrap_err(), ApiError::ReadOnly);
    assert_eq!(api.move_node(note, folder, Place::Last).await.unwrap_err(), ApiError::ReadOnly);
    assert_eq!(api.can_delete(&[note]).await.unwrap_err(), ApiError::ReadOnly);
    assert_eq!(api.create_parent(folder).await.unwrap_err(), ApiError::ReadOnly);
    let _ = { s }.server.stop().await;
}

/// Wait until `api` has heard a change of every kind `wanted` names, and
/// answer the rows and lists they touched, as a client shows them.
async fn heard_all(api: &Pimble, store: StoreId, mut wanted: Vec<fn(&pimble_rpc::StoreChangeKind) -> bool>) -> pimble_api::Affected {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut affected = pimble_api::Affected::default();
        while !wanted.is_empty() {
            match api.next_change().await {
                Some(Change::Store(c)) if c.store == store => {
                    affected = affected.and(pimble_api::Affected::of(&c.kind));
                    wanted.retain(|w| !w(&c.kind));
                }
                Some(_) => continue,
                None => panic!("nothing is subscribed"),
            }
        }
        affected
    })
    .await
    .expect("the changes arrive")
}

/// What another device does to a store's tree is shown from the documents
/// the API holds once their changes are merged (docs/API_CONTRACT.md wave
/// 5): a node made there is held from its announcement, a rename and a move
/// read as they are now, and nothing is fetched to show any of it.
#[tokio::test]
async fn another_devices_changes_are_shown_from_held_documents() {
    use pimble_rpc::StoreChangeKind as K;
    let s = stack().await;
    let (store, root) = s.service.create_store(s.dir.path().join("work.pimble"), "Work").await.unwrap();
    let folder = s.service.create_node(store, Some(root), "folder", "Folder").await.unwrap();
    let (api, counts) = api(&s).await;
    api.listing(Addr::new(store, root)).await.unwrap();
    api.listing(Addr::new(store, folder)).await.unwrap();
    api.listen();
    let fetches = counts.fetches();

    // Made elsewhere.
    let made = s.service.create_node(store, Some(root), "document", "Made elsewhere").await.unwrap();
    let affected = heard_all(&api, store, vec![|k| matches!(k, K::NodeCreated { .. }), |k| matches!(k, K::TreeStructure { .. })]).await;
    let view = api.shown(store, &affected).await;
    let list = view.lists.iter().find(|l| l.parent == root).expect("the root's list is shown");
    assert_eq!(list.children.iter().map(|n| n.id).collect::<Vec<_>>(), vec![folder, made]);
    assert!(view.nodes.iter().any(|n| n.id == made && n.metadata.title == "Made elsewhere"));

    // Renamed elsewhere.
    s.service.edit_node(store, made, "elsewhere", |doc, at| doc.set_title("Renamed", at)).await.unwrap();
    let affected = heard_all(&api, store, vec![|k| matches!(k, K::MetadataUpdated { .. })]).await;
    let view = api.shown(store, &affected).await;
    assert_eq!(view.nodes.iter().find(|n| n.id == made).map(|n| n.metadata.title.as_str()), Some("Renamed"));

    // Moved elsewhere: both lists, as they are now.
    s.service.move_node(store, made, folder, Place::Last).await.unwrap();
    let affected = heard_all(&api, store, vec![|k| matches!(k, K::NodeMoved { .. })]).await;
    // The lists' own documents may still be on their way; what they say is
    // the same once they are merged.
    let _ = tokio::time::timeout(Duration::from_millis(300), heard_all(&api, store, vec![|k| matches!(k, K::TreeStructure { .. }), |k| matches!(k, K::TreeStructure { .. })])).await;
    let view = api.shown(store, &affected).await;
    let ids = |parent: NodeId| view.lists.iter().find(|l| l.parent == parent).map(|l| l.children.iter().map(|n| n.id).collect::<Vec<_>>());
    assert_eq!(ids(root), Some(vec![folder]));
    assert_eq!(ids(folder), Some(vec![made]));

    assert_eq!(counts.fetches(), fetches, "nothing was fetched to show any of it");
    let _ = { s }.server.stop().await;
}

/// A metanode's source (docs/METANODES_CONTRACT.md wave 1): a forest in the
/// tree's own order, read from held documents, told apart from changes that
/// cannot touch it, and judged edits that say when a node leaves the view.
#[tokio::test]
async fn a_metanode_source_answers_a_forest_in_the_trees_order() {
    use pimble_api::MetanodeEdit;
    use pimble_core::metanode::{leaves_metanode_warning, Source};
    use pimble_rpc::StoreChangeKind as K;
    let s = stack().await;
    let (store, root) = s.service.create_store(s.dir.path().join("work.pimble"), "Work").await.unwrap();
    let board = s.service.create_node(store, Some(root), "folder", "Board").await.unwrap();
    let todo = s.service.create_node(store, Some(board), "folder", "To do").await.unwrap();
    let doing = s.service.create_node(store, Some(board), "folder", "Doing").await.unwrap();
    let done = s.service.create_node(store, Some(board), "folder", "Done").await.unwrap();
    let a = s.service.create_node(store, Some(todo), "document", "A").await.unwrap();
    let b = s.service.create_node(store, Some(todo), "document", "B").await.unwrap();
    let c = s.service.create_node(store, Some(doing), "document", "C").await.unwrap();
    let deep = s.service.create_node(store, Some(a), "document", "Below A").await.unwrap();
    let elsewhere = s.service.create_node(store, Some(root), "folder", "Elsewhere").await.unwrap();
    let (api, counts) = api(&s).await;
    let at = |n| Addr::new(store, n);

    // A node with no viewer is read as its children.
    let plain = api.metanode_source(at(board)).await.unwrap();
    assert!(plain.spec.is_none());
    assert_eq!(plain.entries.iter().map(|e| e.addr.node).collect::<Vec<_>>(), vec![todo, doing, done]);

    // Set as a view two levels deep: columns and cards, in order, nothing deeper.
    let edited = api.set_metanode(at(board), Some("kanban"), Some(&Source::subtree(2))).await.unwrap();
    assert!(edited.changed);
    let forest = api.metanode_source(at(board)).await.unwrap();
    assert_eq!(forest.spec.as_ref().map(|v| v.viewer.as_str()), Some("kanban"));
    let order: Vec<(NodeId, u32)> = forest.entries.iter().map(|e| (e.addr.node, e.depth)).collect();
    assert_eq!(order, vec![(todo, 1), (a, 2), (b, 2), (doing, 1), (c, 2), (done, 1)]);
    assert_eq!(forest.tops().map(|e| e.addr.node).collect::<Vec<_>>(), vec![todo, doing, done]);
    assert_eq!(forest.get(at(todo)).unwrap().children, vec![at(a), at(b)]);
    assert_eq!(forest.get(at(a)).unwrap().parent, Some(at(todo)));
    assert!(!forest.contains(at(deep)), "a third level is not in a two-level source");
    assert!(forest.lists_children_of(at(done)) && !forest.lists_children_of(at(a)));
    assert_eq!(forest.not_understood(), None);
    // The node reads as a view for every client.
    let read = s.service.get_node(store, board).await.unwrap();
    assert_eq!(read.metadata.metanode.map(|v| v.source), Some(Source::subtree(2)));

    // Read again: the documents are held, nothing is fetched.
    let fetches = counts.fetches();
    api.metanode_source(at(board)).await.unwrap();
    assert_eq!(counts.fetches(), fetches, "a held forest is read without asking the server");

    // Live: a reorder elsewhere is a change the forest hears, and the
    // forest read again keeps the tree's new order.
    api.listen();
    s.service.move_node(store, b, todo, Place::First).await.unwrap();
    let touched = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match api.next_change().await {
                Some(change @ Change::Store(_)) => {
                    if forest.touched_by(&change) {
                        return true;
                    }
                }
                Some(_) => continue,
                None => panic!("nothing is subscribed"),
            }
        }
    })
    .await
    .expect("the move is heard");
    assert!(touched);
    let again = api.metanode_source(at(board)).await.unwrap();
    assert_eq!(again.get(at(todo)).unwrap().children, vec![at(b), at(a)]);

    // A change outside the view's lists and nodes does not touch it.
    let forest = again;
    let made = s.service.create_node(store, Some(elsewhere), "document", "Unrelated").await.unwrap();
    // (What is still arriving of the reorder is skipped: only the
    // creation's own change is judged.)
    let affected = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match api.next_change().await {
                Some(Change::Store(c)) if matches!(&c.kind, K::NodeCreated { node_id, .. } if *node_id == made) => {
                    return pimble_api::Affected::of(&c.kind);
                }
                Some(_) => continue,
                None => panic!("nothing is subscribed"),
            }
        }
    })
    .await
    .expect("the creation is heard");
    assert!(!forest.touched(store, &affected), "{affected:?}");
    // And one in a list it read does.
    assert!(forest.touched(store, &pimble_api::Affected::rows(vec![done])));

    // Judged: within the view no warning, out of it the leaves-the-view
    // warning, on top of plan's own refusals.
    let judged = api.judge_in_metanode(at(board), MetanodeEdit::Move { nodes: vec![at(c)], target: at(done), place: Place::Last }).await.unwrap();
    assert!(judged.plan.is_some() && judged.warnings.is_empty(), "{:?}", judged.warnings);
    let judged = api.judge_in_metanode(at(board), MetanodeEdit::Move { nodes: vec![at(c)], target: at(elsewhere), place: Place::Last }).await.unwrap();
    assert_eq!(judged.warnings, vec![leaves_metanode_warning(1)]);
    assert_eq!(judged.warnings[0], "This won't be in this view any more.");
    // Under a card: three levels down, out of a two-level view.
    let judged = api.judge_in_metanode(at(board), MetanodeEdit::Move { nodes: vec![at(c)], target: at(a), place: Place::Last }).await.unwrap();
    assert_eq!(judged.warnings, vec![leaves_metanode_warning(1)]);
    let refused = api.judge_in_metanode(at(board), MetanodeEdit::Move { nodes: vec![at(todo)], target: at(a), place: Place::Last }).await.unwrap_err();
    assert_eq!(refused.sentence(), pimble_api::plan::INTO_ITSELF);
    let judged = api.judge_in_metanode(at(board), MetanodeEdit::Delete { nodes: vec![at(a), at(b)] }).await.unwrap();
    assert_eq!(judged.warnings, vec!["These won't be in this view any more.".to_string()]);
    // Nothing was sent by judging.
    assert_eq!(s.service.get_children(store, doing).await.unwrap().1.len(), 1);

    // Back to an outline: the viewer goes, the source stays for next time.
    api.set_metanode(at(board), None, None).await.unwrap();
    let read = api.node(at(board)).await.unwrap();
    assert!(read.metadata.metanode.is_none());
    api.set_metanode(at(board), Some("kanban"), None).await.unwrap();
    assert_eq!(api.metanode_source(at(board)).await.unwrap().source, Source::subtree(2));
    let _ = { s }.server.stop().await;
}

/// A metanode's source reaches through a mount to its source store's nodes.
#[tokio::test]
async fn a_metanode_source_reads_through_mounts() {
    let s = stack().await;
    let (home, home_root) = s.service.create_store(s.dir.path().join("home.pimble"), "Home").await.unwrap();
    let (lib, lib_root) = s.service.create_store(s.dir.path().join("lib.pimble"), "Library").await.unwrap();
    let book = s.service.create_node(lib, Some(lib_root), "document", "Book").await.unwrap();
    let shelf = s.service.create_node(home, Some(home_root), "folder", "Shelf").await.unwrap();
    let (mount, _) = s.service.create_mount(home, shelf, lib, lib_root, Some("Library here".into())).await.unwrap();
    let (api, _) = api(&s).await;
    api.set_metanode(Addr::new(home, shelf), Some("kanban"), Some(&pimble_core::metanode::Source::subtree(2))).await.unwrap();
    let forest = api.metanode_source(Addr::new(home, shelf)).await.unwrap();
    let order: Vec<Addr> = forest.entries.iter().map(|e| e.addr).collect();
    assert_eq!(order, vec![Addr::new(home, mount), Addr::new(lib, book)]);
    assert_eq!(forest.get(Addr::new(lib, book)).unwrap().parent, Some(Addr::new(home, mount)));
    // A node put in the mount's source lands in the view.
    assert!(forest.lists_children_of(Addr::new(lib, lib_root)));
    let _ = { s }.server.stop().await;
}

/// A query metanode (docs/QUERY_METANODES_CONTRACT.md waves A and B): its own
/// children first, then the nodes its match finds, each condition kind,
/// orders, the limit, where it looks, what it never finds, the refusal a
/// found node's place gives, and the changes that make it read again.
#[tokio::test]
async fn a_query_metanode_finds_nodes_after_its_own_children() {
    use pimble_api::MetanodeEdit;
    use pimble_core::metanode::{node_ref, Condition as C, FieldOp, Order, Query, Source, Within, NEWER_SOURCE};
    let s = stack().await;
    let (p, root) = s.service.create_store(s.dir.path().join("personal.pimble"), "Personal").await.unwrap();
    let (o, o_root) = s.service.create_store(s.dir.path().join("other.pimble"), "Other").await.unwrap();
    let mk = |parent, ty: &'static str, title: &'static str| s.service.create_node(p, Some(parent), ty, title);
    let projects = mk(root, "folder", "Projects").await.unwrap();
    let alpha = mk(projects, "document", "Alpha").await.unwrap();
    let beta = mk(projects, "document", "Beta").await.unwrap();
    let gamma = mk(projects, "document", "Gamma").await.unwrap();
    let gone = mk(projects, "document", "Gone").await.unwrap();
    let target = mk(root, "document", "Target").await.unwrap();
    let sketch = mk(root, "board", "Sketch").await.unwrap();
    let lists = mk(root, "folder", "Lists").await.unwrap();
    let waiting = mk(lists, "folder", "Waiting").await.unwrap();
    let own = mk(waiting, "document", "Own").await.unwrap();
    let mine = mk(waiting, "document", "Mine").await.unwrap();
    let kanban = mk(root, "folder", "Board").await.unwrap();
    let elsewhere = s.service.create_node(o, Some(o_root), "document", "Elsewhere").await.unwrap();
    let target_link = pimble_core::PimbleUrl::node(p, target).to_string();

    // (node, tags, fields, text, created)
    let setup: Vec<(StoreId, NodeId, Vec<&str>, Vec<(&str, &str)>, String, &str)> = vec![
        (p, alpha, vec!["waiting"], vec![("Due", "2026-11-06"), ("Cost", "$1,200")], "Send the invoice to Bob".into(), "2026-01-01T00:00:00.000Z"),
        (p, beta, vec!["waiting", "done"], vec![("Due", "2026-11-20")], "An invoice draft".into(), "2026-02-01T00:00:00.000Z"),
        (p, gamma, vec!["#Waiting"], vec![], format!("See [the target]({target_link})"), "2026-03-01T00:00:00.000Z"),
        (p, sketch, vec!["waiting"], vec![], String::new(), "2026-04-01T00:00:00.000Z"),
        (p, gone, vec!["waiting"], vec![], String::new(), "2026-05-01T00:00:00.000Z"),
        (p, lists, vec!["waiting"], vec![], String::new(), "2026-05-01T00:00:00.000Z"),
        (p, waiting, vec!["waiting"], vec![], String::new(), "2026-05-01T00:00:00.000Z"),
        (p, own, vec!["waiting"], vec![], String::new(), "2026-05-01T00:00:00.000Z"),
        (o, elsewhere, vec!["waiting"], vec![], String::new(), "2026-05-01T00:00:00.000Z"),
    ];
    for (store, node, tags, fields, text, created) in setup {
        s.service
            .edit_node(store, node, "setup", |doc, at| {
                doc.set_tags(&tags.iter().map(|t| t.to_string()).collect::<Vec<_>>(), at)?;
                for (name, value) in &fields {
                    doc.set_field(name, Some(*value), at)?;
                }
                if !text.is_empty() {
                    doc.append_markdown(&text)?;
                }
                doc.set_timestamps(created, created)
            })
            .await
            .unwrap();
    }
    s.service.delete_node(p, gone).await.unwrap();

    let (api, _) = api(&s).await;
    api.set_metanode(Addr::new(p, kanban), Some("kanban"), None).await.unwrap();
    let at = Addr::new(p, waiting);
    let query = |all: Vec<C>, within: Option<Within>, order: Order, descending: bool, limit: u32| Source {
        query: Query::Match { within, all, order, descending, limit },
        later: Default::default(),
    };
    let tag = |t: &str| C::Tag { tag: t.into() };
    let finds = |all: Vec<C>| query(all, None, Order::Title, false, 50);
    let api = &api;
    let found_with = |source: Source| async move {
        api.set_metanode(at, Some("list"), Some(&source)).await.unwrap();
        let forest = api.metanode_source(at).await.unwrap();
        forest.found().map(|e| e.addr.node).collect::<Vec<_>>()
    };

    // Own children first, in order; then the finds by title. A node that is
    // both shows once, as its own. Never the metanode, what is above it, a
    // deleted node, or another store's node.
    api.set_metanode(at, Some("list"), Some(&finds(vec![tag("waiting")]))).await.unwrap();
    let forest = api.metanode_source(at).await.unwrap();
    let rows: Vec<(NodeId, bool)> = forest.entries.iter().map(|e| (e.addr.node, e.found)).collect();
    assert_eq!(rows, vec![(own, false), (mine, false), (alpha, true), (beta, true), (gamma, true), (sketch, true)]);
    assert!(forest.entries.iter().all(|e| e.depth == 1 && e.parent.is_none() && e.children.is_empty()));
    assert_eq!(forest.reason.as_deref(), Some("it is tagged #waiting"));
    assert!(forest.lists_children_of(at), "a drop onto the metanode makes an own child");
    assert_eq!(
        api.describe_source(at, &forest.source).await,
        "the nodes tagged waiting in Personal, by title, at most 50"
    );

    // Each condition kind.
    assert_eq!(found_with(finds(vec![tag("waiting"), C::NoTag { no_tag: "done".into() }])).await, vec![alpha, gamma, sketch]);
    assert_eq!(found_with(finds(vec![C::Text { text: "invoice".into() }])).await, vec![alpha, beta]);
    assert_eq!(found_with(finds(vec![C::Text { text: "INVOICE bob".into() }])).await, vec![alpha]);
    assert_eq!(found_with(finds(vec![C::Text { text: "target".into() }])).await, vec![gamma, target], "title and text");
    let field = |f: &str, is, v: Option<&str>| C::Field { field: f.into(), is, value: v.map(Into::into) };
    assert_eq!(found_with(finds(vec![field("Due", FieldOp::Less, Some("2026-11-10"))])).await, vec![alpha]);
    assert_eq!(found_with(finds(vec![field("due", FieldOp::AtLeast, Some("2026-11-06"))])).await, vec![alpha, beta]);
    assert_eq!(found_with(finds(vec![field("Due", FieldOp::Set, None)])).await, vec![alpha, beta]);
    assert_eq!(found_with(finds(vec![tag("waiting"), field("Due", FieldOp::Empty, None)])).await, vec![gamma, sketch]);
    assert_eq!(found_with(finds(vec![field("Cost", FieldOp::More, Some("1000"))])).await, vec![alpha]);
    assert_eq!(found_with(finds(vec![field("Cost", FieldOp::Equal, Some("1200"))])).await, vec![alpha]);
    assert_eq!(found_with(finds(vec![C::LinksTo { links_to: node_ref(p, target) }])).await, vec![gamma]);
    assert_eq!(found_with(finds(vec![C::LinksTo { links_to: target_link.clone() }])).await, vec![gamma]);
    assert_eq!(found_with(finds(vec![C::Type { node_type: "canvas".into() }])).await, vec![sketch]);
    assert_eq!(found_with(finds(vec![C::Type { node_type: "kanban".into() }])).await, vec![kanban]);
    assert_eq!(found_with(finds(vec![tag("waiting"), C::Type { node_type: "document".into() }])).await, vec![alpha, beta, gamma]);
    // A condition still being filled in finds nothing (a new query metanode's
    // first line), with or beside others.
    assert_eq!(found_with(finds(vec![tag(" ")])).await, Vec::<NodeId>::new());
    assert_eq!(found_with(finds(vec![tag("waiting"), C::Text { text: String::new() }])).await, Vec::<NodeId>::new());
    assert_eq!(found_with(finds(vec![field("Due", FieldOp::Less, None)])).await, Vec::<NodeId>::new());

    // Orders, descending, and the limit.
    assert_eq!(found_with(query(vec![tag("waiting")], None, Order::Created, true, 50)).await, vec![sketch, gamma, beta, alpha]);
    assert_eq!(found_with(query(vec![tag("waiting")], None, Order::Created, true, 2)).await, vec![sketch, gamma]);
    assert_eq!(found_with(query(vec![tag("waiting")], None, Order::Modified, false, 50)).await, vec![alpha, beta, gamma, sketch]);
    assert_eq!(found_with(query(vec![tag("waiting")], None, Order::Title, true, 50)).await, vec![sketch, gamma, beta, alpha]);
    // A field order: the values in order, then the nodes without one, by title.
    assert_eq!(
        found_with(query(vec![tag("waiting")], None, Order::Field("Due".into()), true, 50)).await,
        vec![beta, alpha, gamma, sketch]
    );

    // Where it looks.
    let under = Within::Under(node_ref(p, projects));
    assert_eq!(found_with(query(vec![tag("waiting")], Some(under), Order::Title, false, 50)).await, vec![alpha, beta, gamma]);
    assert_eq!(found_with(query(vec![tag("waiting")], Some(Within::Store(o)), Order::Title, false, 50)).await, vec![elsewhere]);
    let everywhere = found_with(query(vec![tag("waiting")], Some(Within::Everywhere), Order::Title, false, 50)).await;
    assert_eq!(everywhere, vec![alpha, beta, elsewhere, gamma, sketch]);

    // A condition from a newer Pimble: own children only, and a sentence.
    let mut newer = finds(vec![tag("waiting")]);
    if let Query::Match { all, .. } = &mut newer.query {
        all.push(C::Later(serde_json::json!({ "near": "home" })));
    }
    api.set_metanode(at, Some("list"), Some(&newer)).await.unwrap();
    let forest = api.metanode_source(at).await.unwrap();
    assert_eq!(forest.not_understood(), Some(NEWER_SOURCE));
    assert_eq!(forest.entries.iter().map(|e| e.addr.node).collect::<Vec<_>>(), vec![own, mine]);

    // A found node's place is not changed from here, and says why.
    api.set_metanode(at, Some("list"), Some(&finds(vec![tag("waiting")]))).await.unwrap();
    let forest = api.metanode_source(at).await.unwrap();
    let sentence = "Shown here because it is tagged #waiting. It lives in Personal / Projects.";
    assert_eq!(api.found_refusal(&forest, Addr::new(p, alpha)).await.as_deref(), Some(sentence));
    assert_eq!(api.found_refusal(&forest, Addr::new(p, own)).await, None, "an own child is ordinary");
    let refused = api.judge_in_metanode(at, MetanodeEdit::Delete { nodes: vec![Addr::new(p, alpha)] }).await.unwrap_err();
    assert_eq!(refused.sentence(), sentence);
    let into = MetanodeEdit::Move { nodes: vec![Addr::new(p, mine)], target: Addr::new(p, alpha), place: Place::Last };
    assert_eq!(api.judge_in_metanode(at, into).await.unwrap_err().sentence(), sentence);
    let own_move = MetanodeEdit::Move { nodes: vec![Addr::new(p, mine)], target: at, place: Place::First };
    assert!(api.judge_in_metanode(at, own_move).await.is_ok());

    // Live: a tag added anywhere in the store touches it; another store's
    // change does not, and a content edit only when the match reads content.
    api.listen();
    s.service.edit_node(p, target, "elsewhere", |doc, at| doc.set_tags(&["waiting".to_string()], at)).await.unwrap();
    let touched = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match api.next_change().await {
                Some(change @ Change::Store(_)) if forest.touched_by(&change) => return true,
                Some(_) => continue,
                None => panic!("nothing is subscribed"),
            }
        }
    })
    .await
    .expect("the tag is heard");
    assert!(touched);
    assert!(api.metanode_source(at).await.unwrap().is_found(Addr::new(p, target)));
    assert!(!forest.touched(o, &pimble_api::Affected::rows(vec![elsewhere])));
    let content = |node| {
        Change::Store(pimble_api::StoreChange {
            store: p,
            kind: pimble_rpc::StoreChangeKind::ContentUpdated { node_id: node },
            source: None,
            update: None,
        })
    };
    assert!(!forest.touched_by(&content(projects)), "a tag match does not read content");
    api.set_metanode(at, Some("list"), Some(&finds(vec![C::Text { text: "x".into() }]))).await.unwrap();
    let by_text = api.metanode_source(at).await.unwrap();
    assert!(by_text.touched_by(&content(projects)));
    let _ = { s }.server.stop().await;
}

/// What points at a node (docs/PROPERTIES_CONTRACT.md section 5, wave 3):
/// the query Metanodes that find it, in its store and from another store
/// looking everywhere, as their evaluation does (a limit that cuts it off,
/// a condition that does not hold, a Metanode looking in its own other
/// store); the nodes whose text links to it, from the store's index, a
/// table's link cell not counted and a deleted linker left out; and both
/// following changes, the walks kept until a store changes.
#[tokio::test]
async fn what_points_at_a_node() {
    use pimble_core::metanode::{Condition as C, Order, Query, Source, Within};
    let s = stack().await;
    let (p, root) = s.service.create_store(s.dir.path().join("personal.pimble"), "Personal").await.unwrap();
    let (o, o_root) = s.service.create_store(s.dir.path().join("other.pimble"), "Other").await.unwrap();
    let mk = |store, parent, title: &'static str| s.service.create_node(store, Some(parent), "document", title);
    let target = mk(p, root, "Target").await.unwrap();
    let first = mk(p, root, "Aardvark").await.unwrap();
    let linker = mk(p, root, "Linker").await.unwrap();
    let gone = mk(p, root, "Gone linker").await.unwrap();
    let plain = mk(p, root, "Plain").await.unwrap();
    let [waiting, done, cut] = [mk(p, root, "Waiting").await.unwrap(), mk(p, root, "Done").await.unwrap(), mk(p, root, "Cut off").await.unwrap()];
    let [everywhere, local] = [mk(o, o_root, "Everywhere").await.unwrap(), mk(o, o_root, "Only here").await.unwrap()];
    let link = pimble_core::PimbleUrl::node(p, target).to_string();
    for (node, tags, text) in [(target, vec!["waiting"], String::new()), (first, vec!["waiting"], String::new()), (linker, vec![], format!("See [the target]({link})")), (gone, vec![], format!("Also [here]({link})")), (plain, vec![], "No link".to_string())] {
        s.service
            .edit_node(p, node, "setup", |doc, at| {
                doc.set_tags(&tags.iter().map(|t| t.to_string()).collect::<Vec<_>>(), at)?;
                if !text.is_empty() {
                    doc.append_markdown(&text)?;
                }
                Ok(())
            })
            .await
            .unwrap();
    }
    s.service.delete_node(p, gone).await.unwrap();

    let (api, _) = api(&s).await;
    let source = |tag: &str, within: Option<Within>, limit: u32| Source {
        query: Query::Match { within, all: vec![C::Tag { tag: tag.into() }], order: Order::Title, descending: false, limit },
        later: Default::default(),
    };
    for (store, node, src) in [
        (p, waiting, source("waiting", None, 50)),
        (p, done, source("done", None, 50)),
        // "Aardvark" comes first by title and takes the one place.
        (p, cut, source("waiting", None, 1)),
        (o, everywhere, source("waiting", Some(Within::Everywhere), 50)),
        (o, local, source("waiting", None, 50)),
    ] {
        api.set_metanode(Addr::new(store, node), Some("list"), Some(&src)).await.unwrap();
    }
    let t = Addr::new(p, target);
    let names = |found: Vec<pimble_api::ShownIn>| found.iter().map(|f| f.metanode.node).collect::<Vec<_>>();
    assert_eq!(names(api.shown_in(t).await.unwrap()), vec![everywhere, waiting], "by name; not Done, Cut off or Only here");
    assert_eq!(names(api.shown_in(t).await.unwrap()), vec![everywhere, waiting], "asked again, the same (from what was kept)");
    assert!(api.shown_in(Addr::new(p, plain)).await.unwrap().is_empty());

    // A change to the node: what finds it follows.
    api.edit(t, |doc, at| doc.set_tags(&["done".to_string()], at)).await.unwrap();
    assert_eq!(names(api.shown_in(t).await.unwrap()), vec![done]);
    // A change to a Metanode elsewhere, made by another client.
    s.service
        .edit_node(o, local, "elsewhere", |doc, at| doc.set_metanode_source(Some(&source("done", Some(Within::Store(p)), 50)), at))
        .await
        .unwrap();
    heard(&api, local).await;
    assert_eq!(names(api.shown_in(t).await.unwrap()), vec![done, local]);

    // Links here: from the index, which takes a moment.
    let mut links = Vec::new();
    for _ in 0..60 {
        links = api.backlinks(t).await.unwrap().into_iter().map(|l| l.addr.node).collect::<Vec<_>>();
        if !links.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert_eq!(links, vec![linker], "the deleted linker is left out");
    assert!(api.backlinks(Addr::new(p, plain)).await.unwrap().is_empty());
    assert!(api.rows_pointing_here(t).await.unwrap().is_empty(), "no table links to it");
}

/// Splitting a note at a place in its text, then joining it back
/// (docs/SPLIT_CONTRACT.md): the new note lands right below with the text
/// from there on, the first note keeps its children, and a join puts the
/// text, the children and the links of the note below into this one.
#[tokio::test]
async fn a_note_splits_in_two_and_joins_back() {
    let s = stack().await;
    let (store, root) = s.service.create_store(s.dir.path().join("book.pimble"), "Book").await.unwrap();
    let (api, _) = api(&s).await;
    let chapter = api.create(Addr::new(store, root), Place::Last, recipes::document("Chapter", Some("First part.\n\nSecond part.\n\nThird part.")).unwrap()).await.unwrap();
    let after = api.create(Addr::new(store, root), Place::Last, recipes::document("After", None).unwrap()).await.unwrap();
    let child = api.create(chapter, Place::Last, recipes::document("A child", None).unwrap()).await.unwrap();

    let at = api.doc(chapter).await.unwrap().sticky_at_quote("Second").unwrap();
    let split = api.split_node(chapter, at, Some("Part two".into())).await.unwrap();
    assert!(split.kept_delta.is_some());
    assert_eq!(api.doc(chapter).await.unwrap().markdown().unwrap().trim(), "First part.");
    assert_eq!(api.doc(split.new).await.unwrap().markdown().unwrap().trim(), "Second part.\n\nThird part.");
    assert_eq!(api.node(split.new).await.unwrap().metadata.title, "Part two");
    assert_eq!(api.node(split.new).await.unwrap().metadata.custom.get(pimble_core::custom_keys::EXPLICIT_TITLE), Some(&serde_json::json!(true)), "named, not titled from its text");
    let order: Vec<NodeId> = api.children(Addr::new(store, root)).await.unwrap().into_iter().map(|(a, _)| a.node).collect();
    assert_eq!(order, vec![chapter.node, split.new.node, after.node], "right below it");
    assert_eq!(api.children(chapter).await.unwrap().len(), 1, "its children stay");

    // A grandchild under the new note goes with it when it is joined back.
    let under = api.create(split.new, Place::Last, recipes::document("Under part two", None).unwrap()).await.unwrap();
    let joined = api.join_next(chapter).await.unwrap();
    assert_eq!(joined.gone, split.new);
    assert_eq!(api.doc(chapter).await.unwrap().markdown().unwrap().trim(), "First part.\n\nSecond part.\n\nThird part.");
    let kids: Vec<NodeId> = api.children(chapter).await.unwrap().into_iter().map(|(a, _)| a.node).collect();
    assert_eq!(kids, vec![child.node, under.node]);
    assert_eq!(api.resolve(&split.new.link()).await.unwrap(), chapter, "a link to the note joined in leads here");

    // Nothing below the last note.
    assert!(api.join_next(after).await.unwrap_err().to_string().contains("no note below"));
    let _ = { s }.server.stop().await;
}
