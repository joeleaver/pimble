//! The browser engine (`js_engine`) with real modules under the page's own
//! WebAssembly: a guest built for another ABI is refused at load, a guest's ops
//! reach the document through the same imports the desktop links, a guest that
//! ran past its budget is stopped when its turn returns (decision 5), and a guest
//! naming a node not its own is stopped from inside the import.
//!
//! Run with `wasm-bindgen-test-runner` (node):
//! `CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner cargo test
//! -p pimble-plugin-host --features js-host --target wasm32-unknown-unknown --test js`.
#![cfg(all(target_arch = "wasm32", feature = "js-host"))]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use pimble_plugin_abi::{encode, Manifest, Op, Props, Provision, SourceShape, ViewerDecl, ABI_VERSION, GUEST_BODY};
use pimble_plugin_host::js_engine::{load_bytes, Plugin};
use pimble_plugin_host::{ApplierOptions, Instance, InstanceOptions};
use rinch_core::dom::mock::MockDomDocument;
use rinch_core::dom::{DomDocument, NodeId};
use wasm_bindgen_test::wasm_bindgen_test;

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

fn data(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("\\{b:02x}")).collect()
}

/// A hand-written guest, as `tests/wasm.rs` makes them, with the preview1
/// imports a real plugin has (so the shim is linked) and binary, not text.
fn guest(manifest: &Manifest, mount_body: &str, ops: &[Op]) -> Vec<u8> {
    let described = encode(manifest);
    let ops = encode(&ops.to_vec());
    let packed = (1024u64 << 32) | described.len() as u64;
    let wat = format!(
        r#"(module
  (import "pimble" "pimble_flush" (func $flush (param i32 i32)))
  (import "wasi_snapshot_preview1" "clock_time_get" (func $clock (param i32 i64 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))
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
    wat::parse_str(wat).expect("the test guest is valid WAT")
}

fn start(plugin: &Plugin) -> (Rc<RefCell<MockDomDocument>>, NodeId, Instance, Rc<RefCell<Option<String>>>) {
    let doc = Rc::new(RefCell::new(MockDomDocument::new()));
    let container = doc.borrow_mut().create_element("div");
    let body = doc.borrow().body();
    doc.borrow_mut().append_child(body, container);
    let dyn_doc: Rc<RefCell<dyn DomDocument>> = doc.clone();
    let instance = Instance::start(
        &plugin.manifest.name,
        dyn_doc,
        container,
        InstanceOptions { applier: ApplierOptions { text_field_shim: false } },
        |key| plugin.instantiate(key),
    )
    .expect("the plugin instantiates");
    let stopped = Rc::new(RefCell::new(None));
    let sink = stopped.clone();
    instance.on_stop(move |sentence| *sink.borrow_mut() = Some(sentence.to_string()));
    (doc, container, instance, stopped)
}

#[wasm_bindgen_test]
async fn a_plugin_built_for_another_abi_is_refused_at_load() {
    let module = guest(&manifest(ABI_VERSION + 1, "old"), "", &[]);
    let Err((described, sentence)) = load_bytes("old.wasm", &module, Duration::from_secs(1)).await else {
        panic!("a plugin for another ABI loads");
    };
    assert_eq!(
        sentence,
        format!("The old test plugin was built for plugin ABI {}, and this Pimble runs ABI {ABI_VERSION} plugins.", ABI_VERSION + 1)
    );
    assert_eq!(described.map(|m| m.id), Some("old-test".to_string()));
    let Err((None, sentence)) = load_bytes("junk.wasm", b"\0asm junk", Duration::from_secs(1)).await else {
        panic!("junk loads");
    };
    assert!(sentence.starts_with("The plugin junk.wasm is not a WebAssembly module"), "{sentence}");
}

#[wasm_bindgen_test]
async fn a_guests_ops_reach_the_document_and_the_next_instance_is_made_ready() {
    let ops = vec![Op::CreateElement { id: 2, tag: "p".into() }, Op::Append { parent: GUEST_BODY, child: 2 }];
    let module = guest(&manifest(ABI_VERSION, "good"), "i32.const 4096 i32.const OPS_LEN call $flush", &ops);
    let plugin = load_bytes("good.wasm", &module, Duration::from_secs(1)).await.unwrap();
    assert!(plugin.is_ready(), "the instance that described it is ready to mount");
    let (doc, container, instance, stopped) = start(&plugin);
    assert!(instance.mount("good", &Props::default()));
    assert_eq!(stopped.borrow().as_deref(), None);
    let children = doc.borrow().get_children(container);
    assert_eq!(children.len(), 1);
    assert_eq!(doc.borrow().tag_name(children[0]).as_deref(), Some("p"));
    // Another is being made ready behind it.
    let (ready, waited) = futures_channel();
    plugin.warm(move || ready(()));
    waited.await;
    assert!(plugin.is_ready());
}

#[wasm_bindgen_test]
async fn a_plugin_that_ran_too_long_is_stopped_when_its_turn_returns() {
    // About thirty million turns of a counter: far past a 1 ms budget.
    let spin = "(local $n i32) i32.const 30000000 local.set $n (loop $spin local.get $n i32.const 1 i32.sub local.tee $n br_if $spin)";
    let module = guest(&manifest(ABI_VERSION, "slow"), spin, &[]);
    let plugin = load_bytes("slow.wasm", &module, Duration::from_millis(1)).await.unwrap();
    let (doc, container, instance, stopped) = start(&plugin);
    assert!(!instance.mount("slow", &Props::default()));
    assert!(instance.is_stopped());
    assert_eq!(stopped.borrow().as_deref(), Some("The slow test plugin was stopped: it ran too long without answering."));
    assert!(doc.borrow().get_children(container).is_empty());
}

#[wasm_bindgen_test]
async fn a_plugin_naming_a_node_not_its_own_is_stopped() {
    let ops = vec![
        Op::CreateElement { id: 2, tag: "p".into() },
        Op::Append { parent: GUEST_BODY, child: 2 },
        Op::Append { parent: 0, child: 2 },
    ];
    let module = guest(&manifest(ABI_VERSION, "rogue"), "i32.const 4096 i32.const OPS_LEN call $flush", &ops);
    let plugin = load_bytes("rogue.wasm", &module, Duration::from_secs(1)).await.unwrap();
    let (doc, container, instance, stopped) = start(&plugin);
    assert!(!instance.mount("rogue", &Props::default()));
    assert_eq!(stopped.borrow().as_deref(), Some("The rogue test plugin was stopped: it named node 0, which is not one of its own."));
    assert!(doc.borrow().get_children(container).is_empty());
}

/// A one-shot callback and the future that waits for it.
fn futures_channel() -> (impl FnOnce(()), impl std::future::Future<Output = ()>) {
    let (tx, rx) = futures_oneshot();
    (move |v| tx(v), rx)
}

fn futures_oneshot() -> (impl FnOnce(()), impl std::future::Future<Output = ()>) {
    let promise_resolve: Rc<RefCell<Option<js_sys::Function>>> = Rc::default();
    let keep = promise_resolve.clone();
    let promise = js_sys::Promise::new(&mut |resolve, _| *keep.borrow_mut() = Some(resolve));
    let fire = move |_: ()| {
        if let Some(resolve) = promise_resolve.borrow_mut().take() {
            let _ = resolve.call0(&wasm_bindgen::JsValue::UNDEFINED);
        }
    };
    (fire, async move {
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    })
}
