//! The desktop engine (wasmtime) with real modules: a guest built for another ABI is
//! refused at load, a guest that loops is interrupted at its budget and stopped
//! with a notice, a guest naming a node not its own is stopped, and the built
//! `hello` plugin renders into a mock document and takes a click, typing and a key
//! (docs/PLUGINS_CONTRACT.md section 5).
#![cfg(feature = "wasm-host")]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use pimble_plugin_abi::{encode, ForestRow, Manifest, NodeAddr, Op, Props, Provision, SourceShape, ViewerDecl, ABI_VERSION, GUEST_BODY};
use pimble_plugin_host::wasmtime_engine::{load_bytes, load_file, Plugin, Registry, ViewerLookup};
use pimble_plugin_host::{ApplierOptions, Instance, InstanceOptions};
use rinch_core::dom::mock::MockDomDocument;
use rinch_core::dom::{DomDocument, NodeId};
use rinch_core::events::{
    dispatch_event, dispatch_input_event, dispatch_key_handler, set_input_context, EventHandlerId, InputContext, KeyEventData,
};

fn manifest(abi: u32, viewer: &str) -> Manifest {
    Manifest {
        id: format!("{viewer}-test"),
        name: format!("{viewer} test"),
        version: "0.0.1".into(),
        abi,
        rinch: "test".into(),
        provides: vec![Provision::Viewer(ViewerDecl { id: viewer.into(), name: viewer.into(), takes: vec![SourceShape::Any] })],
    }
}

/// A WAT data string holding `bytes`.
fn data(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("\\{b:02x}")).collect()
}

/// A hand-written guest: it describes itself as `manifest`, and its mount runs
/// `mount_body` (WAT instructions), with `ops` at address 4096 for it to flush.
fn guest(manifest: &Manifest, mount_body: &str, ops: &[Op]) -> Vec<u8> {
    let described = encode(manifest);
    let ops = encode(&ops.to_vec());
    let packed = (1024u64 << 32) | described.len() as u64;
    let wat = format!(
        r#"(module
  (import "pimble" "pimble_flush" (func $flush (param i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "{described}")
  (data (i32.const 4096) "{ops_data}")
  (global $bump (mut i32) (i32.const 16384))
  (func (export "pimble_alloc") (param $len i32) (result i32) (local $p i32)
    global.get $bump local.set $p
    global.get $bump local.get $len i32.add global.set $bump
    local.get $p)
  (func (export "pimble_free") (param i32 i32))
  (func (export "pimble_describe") (result i64) i64.const {packed})
  (func (export "pimble_mount") (param i32 i32) {mount_body})
  (func (export "pimble_dispatch") (param i32 i32) (result i32) i32.const 0)
  (func (export "pimble_props_changed") (param i32 i32))
  (func (export "pimble_unmount"))
  (func (export "pimble_fire_timeout") (param i64))
  (func (export "pimble_poll_task") (param i64))
  (func (export "pimble_drain_polls") (result i64) i64.const -1)
)"#,
        described = data(&described),
        ops_data = data(&ops),
        mount_body = mount_body.replace("OPS_LEN", &ops.len().to_string()),
    );
    wat.into_bytes()
}

fn document() -> (Rc<RefCell<MockDomDocument>>, Rc<RefCell<dyn DomDocument>>, NodeId) {
    let doc = Rc::new(RefCell::new(MockDomDocument::new()));
    let container = doc.borrow_mut().create_element("div");
    let body = doc.borrow().body();
    doc.borrow_mut().append_child(body, container);
    let dyn_doc: Rc<RefCell<dyn DomDocument>> = doc.clone();
    (doc, dyn_doc, container)
}

fn start(plugin: &Plugin, budget: Duration) -> (Rc<RefCell<MockDomDocument>>, NodeId, Instance, Rc<RefCell<Option<String>>>) {
    let (doc, dyn_doc, container) = document();
    let instance = Instance::start(
        &plugin.manifest.name,
        dyn_doc,
        container,
        InstanceOptions { applier: ApplierOptions { text_field_shim: true } },
        |key| plugin.instantiate(key, budget),
    )
    .expect("the plugin instantiates");
    let stopped = Rc::new(RefCell::new(None));
    let sink = stopped.clone();
    instance.on_stop(move |sentence| *sink.borrow_mut() = Some(sentence.to_string()));
    (doc, container, instance, stopped)
}

#[test]
fn a_plugin_built_for_another_abi_is_refused_at_load() {
    let module = guest(&manifest(ABI_VERSION + 1, "old"), "", &[]);
    let Err((described, sentence)) = load_bytes(Path::new("old.wasm"), &module, Duration::from_secs(1)) else {
        panic!("a plugin for another ABI loads");
    };
    assert_eq!(
        sentence,
        format!("The old test plugin was built for plugin ABI {}, and this Pimble runs ABI {ABI_VERSION} plugins.", ABI_VERSION + 1)
    );
    assert_eq!(described.map(|m| m.id), Some("old-test".to_string()));
    // Not a module at all.
    let Err((None, sentence)) = load_bytes(Path::new("junk.wasm"), b"\0asm junk", Duration::from_secs(1)) else {
        panic!("junk loads");
    };
    assert!(sentence.starts_with("The plugin junk.wasm is not a WebAssembly module"), "{sentence}");

    // In a folder, the refused plugin's viewer says why it is missing.
    let dir = std::env::temp_dir().join(format!("pimble-plugin-host-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("old.wasm"), &module).unwrap();
    std::fs::write(dir.join("good.wasm"), guest(&manifest(ABI_VERSION, "good"), "", &[])).unwrap();
    let registry = Registry::load(&dir, Duration::from_secs(1));
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(matches!(registry.viewer("good"), ViewerLookup::Ready(p) if p.manifest.id == "good-test"));
    assert!(matches!(registry.viewer("old"), ViewerLookup::Refused(s) if s.contains("plugin ABI")));
    assert!(matches!(registry.viewer("kanban"), ViewerLookup::Missing));
}

#[test]
fn a_plugin_that_runs_too_long_is_stopped_with_a_notice() {
    let module = guest(&manifest(ABI_VERSION, "stuck"), "(loop $spin br $spin)", &[]);
    let plugin = load_bytes(Path::new("stuck.wasm"), &module, Duration::from_millis(200)).unwrap();
    let (doc, container, instance, stopped) = start(&plugin, Duration::from_millis(200));
    let began = Instant::now();
    assert!(!instance.mount("stuck", &Props::default()));
    let took = began.elapsed();
    assert!(took >= Duration::from_millis(150) && took < Duration::from_secs(5), "interrupted after {took:?}");
    assert!(instance.is_stopped());
    assert_eq!(stopped.borrow().as_deref(), Some("The stuck test plugin was stopped: it ran too long without answering."));
    assert!(doc.borrow().get_children(container).is_empty());
    // A stopped plugin takes nothing more.
    assert!(!instance.props_changed(&Props::default()));
}

#[test]
fn a_plugin_naming_a_node_not_its_own_is_stopped() {
    let ops = vec![
        Op::CreateElement { id: 2, tag: "p".into() },
        Op::Append { parent: GUEST_BODY, child: 2 },
        Op::Append { parent: 0, child: 2 },
    ];
    let module = guest(&manifest(ABI_VERSION, "rogue"), "i32.const 4096 i32.const OPS_LEN call $flush", &ops);
    let plugin = load_bytes(Path::new("rogue.wasm"), &module, Duration::from_secs(1)).unwrap();
    let (doc, container, instance, stopped) = start(&plugin, Duration::from_secs(1));
    assert!(!instance.mount("rogue", &Props::default()));
    assert_eq!(stopped.borrow().as_deref(), Some("The rogue test plugin was stopped: it named node 0, which is not one of its own."));
    assert!(doc.borrow().get_children(container).is_empty());
}

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

fn find(doc: &MockDomDocument, node: NodeId, f: &dyn Fn(&MockDomDocument, NodeId) -> bool) -> Option<NodeId> {
    if f(doc, node) {
        return Some(node);
    }
    doc.get_children(node).into_iter().find_map(|c| find(doc, c, f))
}

fn handler(doc: &MockDomDocument, node: NodeId, attr: &str) -> usize {
    doc.get_attribute(node, attr).unwrap().parse().unwrap()
}

#[test]
fn the_hello_plugin_renders_and_takes_a_click_typing_and_a_key() {
    let Some(path) = hello_wasm() else { return };
    let plugin = load_file(&path, Duration::from_secs(2)).expect("the hello plugin loads");
    assert_eq!(plugin.manifest.id, "hello-viewer");
    assert!(plugin.manifest.viewers().any(|v| v.id == "hello"));
    let (doc, container, instance, stopped) = start(&plugin, Duration::from_secs(2));
    let row = |node: &str, label: &str, depth: u32| ForestRow {
        addr: NodeAddr { store: "s".into(), node: node.into() },
        parent: None,
        depth,
        label: label.into(),
        node_type: "document".into(),
    };
    let props = Props {
        metanode: NodeAddr { store: "s".into(), node: "m".into() },
        title: "Work".into(),
        forest: vec![row("a", "Plan", 1), row("b", "Write it", 2)],
        settings: None,
    };
    assert!(instance.mount("hello", &props), "{:?}", stopped.borrow());
    let text = || doc.borrow().text_content(container).unwrap_or_default();
    assert!(text().contains("Hello from a plugin: Work"), "{}", text());
    assert!(text().contains("Plan") && text().contains("Write it"));
    assert!(text().contains("Click me"));

    // A click on the button, as the desktop dispatches it: by the host handler id
    // in the host's attribute.
    let button = find(&doc.borrow(), container, &|d, n| d.get_attribute(n, "data-rid").is_some() && d.tag_name(n).as_deref() == Some("button"))
        .expect("the button");
    let click = handler(&doc.borrow(), button, "data-rid");
    assert!(dispatch_event(EventHandlerId(click)));
    assert!(text().contains("Clicked once"), "{}", text());
    assert!(dispatch_event(EventHandlerId(click)));
    assert!(text().contains("Clicked 2 times"), "{}", text());

    // Typing into the field: the text comes back as an input event, and the guest's
    // view echoes it.
    let field = find(&doc.borrow(), container, &|d, n| d.tag_name(n).as_deref() == Some("input")).expect("the field");
    let input = handler(&doc.borrow(), field, "data-oninput");
    doc.borrow_mut().set_attribute(field, "value", "hey");
    set_input_context(InputContext { value: "hey".into(), node_id: field.0 });
    assert!(dispatch_input_event(EventHandlerId(input), "hey".into()));
    assert!(text().contains("You typed: hey"), "{}", text());

    // A key, offered to the handler on the field's wrapper: not consumed.
    let wrapper = find(&doc.borrow(), container, &|d, n| d.get_attribute(n, "data-onkeydown").is_some()).expect("the key handler");
    let key = handler(&doc.borrow(), wrapper, "data-onkeydown");
    assert!(!dispatch_key_handler(EventHandlerId(key), &KeyEventData::new("x", "KeyX")));
    assert!(text().contains("Last key: x"), "{}", text());

    // New props: the forest changes in place.
    let mut next = props.clone();
    next.forest = vec![row("c", "Ship", 1)];
    assert!(instance.props_changed(&next));
    assert!(text().contains("Ship") && !text().contains("Plan"), "{}", text());

    instance.unmount();
    assert!(doc.borrow().get_children(container).is_empty());
    assert_eq!(instance.proxy_count(), 0);
    assert!(stopped.borrow().is_none());
}
