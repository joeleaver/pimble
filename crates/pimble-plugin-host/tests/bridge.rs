//! The API bridge end to end (docs/PLUGINS_CONTRACT.md wave 2): the built `hello`
//! plugin, rendering into a mock document, runs `pimble_api::Pimble` over the
//! guest SDK's backend; every call it makes crosses the host's approval check and
//! is made through a real in-process Pimble server, as the app's backend makes it.
//! The plugin reads its metanode's forest, renames a node (attributed to the
//! plugin), moves one, gets `plan`'s refusal for a move into itself, and is
//! refused a write once its approval is read-only.
#![cfg(feature = "wasm-host")]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{mpsc, Arc, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use futures::StreamExt;
use pimble_api::rpc::RpcBackend;
use pimble_api::{Addr, Change, Pimble};
use pimble_client::PimbleClient;
use pimble_core::metanode::Source;
use pimble_core::plan::Refusal;
use pimble_core::{NodeId, StoreId};
use pimble_plugin_abi::api::{ApiCall, ApiReply};
use pimble_plugin_abi::{NodeAddr, Props, StoreAsk};
use pimble_plugin_host::bridge::{execute, subscribe};
use pimble_plugin_host::wasmtime_engine::load_file;
use pimble_plugin_host::{ApiSide, ApplierOptions, Approval, Approved, Instance, InstanceOptions};
use pimble_server::{PimbleServer, ServerConfig};
use rinch_core::dom::mock::MockDomDocument;
use rinch_core::dom::{DomDocument, NodeId as DomId};
use rinch_core::events::{dispatch_event, dispatch_input_event, dispatch_key_handler, set_input_context, EventHandlerId, InputContext, KeyEventData};

/// The hello plugin as `plugins/` builds it, when it has been built.
fn hello_wasm() -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/target/wasm32-wasip1/release/hello_viewer.wasm");
    if path.is_file() {
        Some(path)
    } else {
        eprintln!(
            "skipped: build the plugins first (cd plugins && cargo build --release --target wasm32-wasip1); no {}",
            path.display()
        );
        None
    }
}

// ── Timers ─────────────────────────────────────────────────────────────────────
// The host runs a guest's tasks and timers through rinch's `set_timeout`; there
// is no rinch event loop in a test, so its timers are kept here and fired by the
// test's own loop, on the thread that armed them.

static TIMERS: Mutex<Vec<(ThreadId, u64, Instant)>> = Mutex::new(Vec::new());

fn arm(id: u64, delay_ms: u32) {
    let due = Instant::now() + Duration::from_millis(delay_ms as u64);
    TIMERS.lock().unwrap().push((std::thread::current().id(), id, due));
}

fn fire_due_timers() {
    let me = std::thread::current().id();
    let now = Instant::now();
    let due: Vec<u64> = {
        let mut timers = TIMERS.lock().unwrap();
        let (due, kept): (Vec<_>, Vec<_>) = timers.drain(..).partition(|(t, _, at)| *t == me && *at <= now);
        *timers = kept;
        due.into_iter().map(|(_, id, _)| id).collect()
    };
    for id in due {
        rinch_core::fire_timeout(id);
    }
}

// ── The stack ──────────────────────────────────────────────────────────────────

struct Fixture {
    rt: tokio::runtime::Runtime,
    _server: PimbleServer,
    _dir: tempfile::TempDir,
    service: PimbleClient,
    url: String,
    /// The app's API: what the plugin's calls are made through.
    app: Arc<Pimble>,
    store: StoreId,
    board: NodeId,
    plan: NodeId,
    write: NodeId,
}

fn fixture() -> Fixture {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (server, service, url, app, store, board, plan, write) = rt.block_on(async {
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
        let service = PimbleClient::connect(&url).await.unwrap();
        let (store, root) = service.create_store(dir.path().join("work.pimble"), "Work").await.unwrap();
        let board = service.create_node(store, Some(root), "folder", "Board").await.unwrap();
        let plan = service.create_node(store, Some(board), "document", "Plan").await.unwrap();
        let write = service.create_node(store, Some(board), "document", "Write it").await.unwrap();
        let app = Arc::new(Pimble::new(RpcBackend::over(PimbleClient::connect(&url).await.unwrap()), "app"));
        app.set_metanode(Addr::new(store, board), Some("hello"), Some(&Source::subtree(2))).await.unwrap();
        (server, service, url, app, store, board, plan, write)
    });
    Fixture { rt, _server: server, _dir: dir, service, url, app, store, board, plan, write }
}

/// The plugin mounted over the fixture, with its calls queued for the test's
/// loop to make, as the desktop queues them for its backend thread.
struct Running {
    doc: Rc<RefCell<MockDomDocument>>,
    container: DomId,
    instance: Instance,
    calls: Rc<RefCell<VecDeque<(u64, ApiCall)>>>,
    approval: Rc<RefCell<Approval>>,
    answers: (mpsc::Sender<(u64, ApiReply)>, mpsc::Receiver<(u64, ApiReply)>),
}

fn mount(f: &Fixture, approval: Approval) -> Running {
    rinch_core::set_timer_backend(arm);
    let path = hello_wasm().expect("checked by the caller");
    let plugin = load_file(&path, Duration::from_secs(2)).expect("the hello plugin loads");
    assert_eq!(plugin.manifest.asks.stores, StoreAsk::Chosen);
    assert!(plugin.manifest.asks.write);
    let doc = Rc::new(RefCell::new(MockDomDocument::new()));
    let container = doc.borrow_mut().create_element("div");
    let body = doc.borrow().body();
    doc.borrow_mut().append_child(body, container);
    let dyn_doc: Rc<RefCell<dyn DomDocument>> = doc.clone();
    let instance = Instance::start(
        &plugin.manifest.name,
        dyn_doc,
        container,
        InstanceOptions { applier: ApplierOptions { text_field_shim: true } },
        |key| plugin.instantiate(key, Duration::from_secs(2)),
    )
    .expect("the plugin instantiates");
    instance.on_stop(|sentence| panic!("the plugin stopped: {sentence}"));
    let calls: Rc<RefCell<VecDeque<(u64, ApiCall)>>> = Rc::default();
    let queue = calls.clone();
    instance.set_api(ApiSide {
        plugin_id: plugin.manifest.id.clone(),
        approval: approval.clone(),
        forward: Box::new(move |id, call| queue.borrow_mut().push_back((id, call))),
        cancel: Box::new(|_| {}),
        forget: Box::new(|| {}),
    });
    let props = Props { metanode: NodeAddr::from(Addr::new(f.store, f.board)), settings: None };
    assert!(instance.mount("hello", &props));
    Running { doc, container, instance, calls, approval: Rc::new(RefCell::new(approval)), answers: mpsc::channel() }
}

impl Running {
    fn text(&self) -> String {
        self.doc.borrow().text_content(self.container).unwrap_or_default()
    }

    /// Make the plugin's calls and hand back their answers until `done` holds.
    fn pump_until(&self, f: &Fixture, what: &str, done: impl Fn(&Running) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if done(self) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}; the plugin shows: {}", self.text());
            fire_due_timers();
            loop {
                let next = self.calls.borrow_mut().pop_front();
                let Some((id, call)) = next else { break };
                let approval = self.approval.borrow().clone();
                match call {
                    ApiCall::Subscribe { store } => {
                        let (app, tx) = (f.app.clone(), self.answers.0.clone());
                        f.rt.spawn(async move {
                            match subscribe(app.backend(), "Hello viewer", &approval, store).await {
                                Ok(mut changes) => {
                                    let _ = tx.send((id, ApiReply::Subscribed));
                                    while let Some(change) = changes.next().await {
                                        let _ = tx.send((id, ApiReply::Change { change: change.into() }));
                                    }
                                    let _ = tx.send((id, ApiReply::Ended));
                                }
                                Err(e) => {
                                    let _ = tx.send((id, ApiReply::error(e)));
                                }
                            }
                        });
                    }
                    call => {
                        let reply = f.rt.block_on(execute(f.app.backend(), "hello-viewer", "Hello viewer", &approval, call));
                        self.instance.resolve(id, reply);
                    }
                }
            }
            while let Ok((id, reply)) = self.answers.1.try_recv() {
                self.instance.resolve(id, reply);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn find(&self, f: &dyn Fn(&MockDomDocument, DomId) -> bool) -> Option<DomId> {
        fn walk(doc: &MockDomDocument, node: DomId, f: &dyn Fn(&MockDomDocument, DomId) -> bool) -> Option<DomId> {
            if f(doc, node) {
                return Some(node);
            }
            doc.get_children(node).into_iter().find_map(|c| walk(doc, c, f))
        }
        walk(&self.doc.borrow(), self.container, f)
    }

    fn handler(&self, node: DomId, attr: &str) -> usize {
        self.doc.borrow().get_attribute(node, attr).unwrap().parse().unwrap()
    }

    /// Click the element of tag `tag` whose text is `text`.
    fn click(&self, tag: &str, text: &str) {
        let node = self
            .find(&|d, n| d.tag_name(n).as_deref() == Some(tag) && d.text_content(n).unwrap_or_default().trim() == text && d.get_attribute(n, "data-rid").is_some())
            .unwrap_or_else(|| panic!("no {tag} reading {text:?} in: {}", self.text()));
        assert!(dispatch_event(EventHandlerId(self.handler(node, "data-rid"))));
    }

    /// Type `text` into the field, as the desktop's input event delivers it.
    fn type_text(&self, text: &str) {
        let field = self.find(&|d, n| d.tag_name(n).as_deref() == Some("input")).expect("the field");
        let input = self.handler(field, "data-oninput");
        self.doc.borrow_mut().set_attribute(field, "value", text);
        set_input_context(InputContext { value: text.into(), node_id: field.0 });
        assert!(dispatch_input_event(EventHandlerId(input), text.into()));
    }
}

#[test]
fn the_hello_plugin_reads_renames_and_moves_through_the_api() {
    if hello_wasm().is_none() {
        return;
    }
    let f = fixture();
    let store = f.store;
    let observer = f.rt.block_on(async {
        let observer = Pimble::new(RpcBackend::over(PimbleClient::connect(&f.url).await.unwrap()), "observer");
        observer.watch(store).await.unwrap();
        observer
    });
    let running = mount(&f, Approval { stores: Approved::Only(vec![store]), write: true });

    // The forest, read by the plugin through `Pimble::metanode_source`.
    running.pump_until(&f, "the forest", |r| r.text().contains("Write it"));
    let text = running.text();
    assert!(text.contains("Hello from a plugin: Board") && text.contains("Plan") && text.contains("It shows 2 nodes"), "{text}");

    // A key reaches the plugin's handler; typing reaches its field.
    let wrapper = running.find(&|d, n| d.get_attribute(n, "data-onkeydown").is_some()).expect("the key handler");
    assert!(!dispatch_key_handler(EventHandlerId(running.handler(wrapper, "data-onkeydown")), &KeyEventData::new("x", "KeyX")));
    assert!(running.text().contains("Last key: x"));

    // Rename "Plan" through the one write: the server holds the new title, the
    // edit is attributed to the plugin, and the plugin's own view shows it.
    running.click("li", "Plan");
    running.type_text("Planned");
    running.click("button", "Rename");
    running.pump_until(&f, "the rename", |r| r.text().contains("Renamed."));
    let renamed = f.rt.block_on(f.service.get_node(store, f.plan)).unwrap();
    assert_eq!(renamed.metadata.title, "Planned");
    let source = f.rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(Change::Store(change)) = observer.next_change().await {
                    if change.source.as_deref().is_some_and(|s| s.starts_with("plugin:")) {
                        return change.source;
                    }
                }
            }
        })
        .await
        .expect("the rename is heard")
    });
    assert_eq!(source.as_deref(), Some("plugin:hello-viewer"));
    running.pump_until(&f, "the renamed row", |r| r.text().contains("Planned"));

    // Move "Write it" to the top: the server's order changes, and so does the
    // plugin's, from the change stream.
    running.click("li", "Write it");
    running.click("button", "Move to top");
    running.pump_until(&f, "the move", |r| r.text().contains("Moved to the top."));
    let (_, children) = f.rt.block_on(f.service.get_children(store, f.board)).unwrap();
    assert_eq!(children.iter().map(|n| n.id).collect::<Vec<_>>(), vec![f.write, f.plan]);
    let text = running.text();
    assert!(text.find("Write it").unwrap() < text.find("Planned").unwrap(), "{text}");

    // A move into itself is refused by `plan`, in the plugin, with the explorer's
    // sentence; nothing is sent.
    running.click("button", "Move into itself");
    running.pump_until(&f, "the refusal", |r| r.text().contains(Refusal::IntoItself.sentence()));
    let (_, children) = f.rt.block_on(f.service.get_children(store, f.board)).unwrap();
    assert_eq!(children.len(), 2);

    // Read-only from now on: the host refuses the write before it leaves.
    let read_only = Approval { stores: Approved::Only(vec![store]), write: false };
    running.instance.set_approval(read_only.clone());
    *running.approval.borrow_mut() = read_only;
    running.type_text("Not allowed");
    running.click("button", "Rename");
    running.pump_until(&f, "the read-only refusal", |r| {
        r.text().contains("The Hello viewer plugin was allowed to read this store, not to change it.")
    });
    let unchanged = f.rt.block_on(f.service.get_node(store, f.write)).unwrap();
    assert_eq!(unchanged.metadata.title, "Write it");

    running.instance.unmount();
    assert!(running.doc.borrow().get_children(running.container).is_empty());
}

#[test]
fn a_plugin_without_an_approval_reads_nothing() {
    if hello_wasm().is_none() {
        return;
    }
    let f = fixture();
    let running = mount(&f, Approval::default());
    running.pump_until(&f, "the refusal", |r| r.text().contains("The Hello viewer plugin was not allowed to use that store."));
    assert!(!running.text().contains("Plan"));
    running.instance.unmount();
}
