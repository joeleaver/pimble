//! The desktop engine (wasmtime) with real modules: a guest built for another ABI is
//! refused at load, a guest that loops is interrupted at its budget and stopped
//! with a notice, and a guest naming a node not its own is stopped
//! (docs/PLUGINS_CONTRACT.md section 5). The built `hello` plugin is `bridge.rs`'s.
#![cfg(feature = "wasm-host")]

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use pimble_plugin_abi::{encode, Manifest, Op, Props, Provision, SourceShape, ViewerDecl, ABI_VERSION, GUEST_BODY};
use pimble_plugin_host::wasmtime_engine::{load_bytes, Plugin, ViewerLookup};
use pimble_plugin_host::{ApplierOptions, Instance, InstanceOptions};
use rinch_core::dom::mock::MockDomDocument;
use rinch_core::dom::{DomDocument, NodeId};

fn manifest(abi: u32, viewer: &str) -> Manifest {
    Manifest {
        id: format!("{viewer}-test"),
        name: format!("{viewer} test"),
        version: "0.0.1".into(),
        abi,
        rinch: "test".into(),
        provides: vec![Provision::Viewer(ViewerDecl { id: viewer.into(), name: viewer.into(), takes: vec![SourceShape::Any] })],
        asks: Default::default(),
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
  (func (export "pimble_resolve") (param i32 i32))
  (func (export "pimble_host_state") (param i32 i32))
  (func (export "pimble_drag_ended"))
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
    let registry = pimble_plugin_host::wasmtime_engine::load_dir(&dir, Duration::from_secs(1));
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
