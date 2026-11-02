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
                Some(Change::Resynced { store: st }) => return st,
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

    // Nothing is held yet: not known, never a guess.
    assert_eq!(api.view(|v| v.parent(home, titled.node)), None);
    assert_eq!(api.view(|v| v.label(home, titled.node)), plan::UNTITLED);

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
