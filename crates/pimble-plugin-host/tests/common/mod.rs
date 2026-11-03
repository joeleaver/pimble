//! The plugin test harness: a real in-process Pimble server, a built plugin
//! mounted into a mock document, and a loop that makes the plugin's API calls
//! through the server as the desktop's backend thread makes them.
#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{mpsc, Arc, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use futures::StreamExt;
use pimble_api::rpc::RpcBackend;
use pimble_api::{Addr, Pimble};
use pimble_client::PimbleClient;
use pimble_core::metanode::Source;
use pimble_core::{NodeId, StoreId};
use pimble_plugin_abi::api::{ApiCall, ApiReply};
use pimble_plugin_abi::{HostRequest, NodeAddr, Props, StoreAsk};
use pimble_plugin_host::bridge::{execute, subscribe};
use pimble_plugin_host::wasmtime_engine::load_file;
use pimble_plugin_host::{ApiSide, ApplierOptions, Approval, Instance, InstanceOptions};
use pimble_server::{PimbleServer, ServerConfig};
use rinch_core::dom::mock::MockDomDocument;
use rinch_core::dom::{DomDocument, NodeId as DomId};
use rinch_core::events::{dispatch_event, dispatch_input_event, set_input_context, EventHandlerId, InputContext};

/// A plugin as `plugins/` builds it (`hello_viewer`, `kanban_viewer`), when it
/// has been built.
pub fn built_plugin(file: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../plugins/target/wasm32-wasip1/release/{file}.wasm"));
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

pub static TIMERS: Mutex<Vec<(ThreadId, u64, Instant)>> = Mutex::new(Vec::new());

pub fn arm(id: u64, delay_ms: u32) {
    let due = Instant::now() + Duration::from_millis(delay_ms as u64);
    TIMERS.lock().unwrap().push((std::thread::current().id(), id, due));
}

pub fn fire_due_timers() {
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

pub struct Fixture {
    pub rt: tokio::runtime::Runtime,
    pub _server: PimbleServer,
    pub dir: tempfile::TempDir,
    pub service: PimbleClient,
    pub url: String,
    /// The app's API: what the plugin's calls are made through.
    pub app: Arc<Pimble>,
    pub store: StoreId,
    pub board: NodeId,
    pub plan: NodeId,
    pub write: NodeId,
}

/// A server with a store "Work" whose "Board" (a metanode shown with
/// `viewer`, two levels deep) holds "Plan" and "Write it".
pub fn fixture(viewer: &str) -> Fixture {
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
        app.set_metanode(Addr::new(store, board), Some(viewer), Some(&Source::subtree(2))).await.unwrap();
        (server, service, url, app, store, board, plan, write)
    });
    Fixture { rt, _server: server, dir, service, url, app, store, board, plan, write }
}

/// The plugin mounted over the fixture, with its calls queued for the test's
/// loop to make, as the desktop queues them for its backend thread.
pub struct Running {
    pub doc: Rc<RefCell<MockDomDocument>>,
    pub container: DomId,
    pub instance: Instance,
    pub calls: Rc<RefCell<VecDeque<(u64, ApiCall)>>>,
    pub approval: Rc<RefCell<Approval>>,
    pub answers: (mpsc::Sender<(u64, ApiReply)>, mpsc::Receiver<(u64, ApiReply)>),
    /// The plugin's id and name, as the host names it on every call.
    pub plugin: (String, String),
    /// What it asked of the app (`HostRequest`), in order.
    pub requests: Rc<RefCell<Vec<HostRequest>>>,
}

/// The plugin built as `file` mounted showing `viewer` over the fixture's
/// board.
pub fn mount(f: &Fixture, approval: Approval, file: &str, viewer: &str) -> Running {
    rinch_core::set_timer_backend(arm);
    let path = built_plugin(file).expect("checked by the caller");
    let plugin = load_file(&path, Duration::from_secs(2)).expect("the plugin loads");
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
    let requests: Rc<RefCell<Vec<HostRequest>>> = Rc::default();
    let heard = requests.clone();
    instance.on_request(move |r| heard.borrow_mut().push(r));
    let props = Props { metanode: NodeAddr::from(Addr::new(f.store, f.board)), settings: None };
    assert!(instance.mount(viewer, &props));
    Running {
        doc,
        container,
        instance,
        calls,
        approval: Rc::new(RefCell::new(approval)),
        answers: mpsc::channel(),
        plugin: (plugin.manifest.id.clone(), plugin.manifest.name.clone()),
        requests,
    }
}

impl Running {
    pub fn text(&self) -> String {
        self.doc.borrow().text_content(self.container).unwrap_or_default()
    }

    /// Make the plugin's calls and hand back their answers until `done` holds.
    pub fn pump_until(&self, f: &Fixture, what: &str, done: impl Fn(&Running) -> bool) {
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
                        let (app, tx, name) = (f.app.clone(), self.answers.0.clone(), self.plugin.1.clone());
                        f.rt.spawn(async move {
                            match subscribe(app.backend(), &name, &approval, store).await {
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
                        let reply = f.rt.block_on(execute(f.app.backend(), &self.plugin.0, &self.plugin.1, &approval, call));
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

    pub fn find(&self, f: &dyn Fn(&MockDomDocument, DomId) -> bool) -> Option<DomId> {
        fn walk(doc: &MockDomDocument, node: DomId, f: &dyn Fn(&MockDomDocument, DomId) -> bool) -> Option<DomId> {
            if f(doc, node) {
                return Some(node);
            }
            doc.get_children(node).into_iter().find_map(|c| walk(doc, c, f))
        }
        walk(&self.doc.borrow(), self.container, f)
    }

    pub fn handler(&self, node: DomId, attr: &str) -> usize {
        self.doc.borrow().get_attribute(node, attr).unwrap().parse().unwrap()
    }

    /// Click the element of tag `tag` whose text is `text`.
    pub fn click(&self, tag: &str, text: &str) {
        let node = self
            .find(&|d, n| d.tag_name(n).as_deref() == Some(tag) && d.text_content(n).unwrap_or_default().trim() == text && d.get_attribute(n, "data-rid").is_some())
            .unwrap_or_else(|| panic!("no {tag} reading {text:?} in: {}", self.text()));
        assert!(dispatch_event(EventHandlerId(self.handler(node, "data-rid"))));
    }

    /// Type `text` into the field, as the desktop's input event delivers it.
    pub fn type_text(&self, text: &str) {
        let field = self.find(&|d, n| d.tag_name(n).as_deref() == Some("input")).expect("the field");
        let input = self.handler(field, "data-oninput");
        self.doc.borrow_mut().set_attribute(field, "value", text);
        set_input_context(InputContext { value: text.into(), node_id: field.0 });
        assert!(dispatch_input_event(EventHandlerId(input), text.into()));
    }
}

