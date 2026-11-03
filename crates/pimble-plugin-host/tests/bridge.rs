//! The API bridge end to end (docs/PLUGINS_CONTRACT.md wave 2): the built `hello`
//! plugin, rendering into a mock document, runs `pimble_api::Pimble` over the
//! guest SDK's backend; every call it makes crosses the host's approval check and
//! is made through a real in-process Pimble server, as the app's backend makes it.
//! The plugin reads its metanode's forest, renames a node (attributed to the
//! plugin), moves one, gets `plan`'s refusal for a move into itself, and is
//! refused a write once its approval is read-only.
#![cfg(feature = "wasm-host")]

mod common;

use std::time::Duration;

use common::{built_plugin, fixture, mount};
use pimble_api::rpc::RpcBackend;
use pimble_api::{Change, Pimble};
use pimble_client::PimbleClient;
use pimble_core::plan::Refusal;
use pimble_plugin_host::{Approval, Approved};
use rinch_core::dom::DomDocument;
use rinch_core::events::{dispatch_key_handler, EventHandlerId, KeyEventData};

fn hello_wasm() -> Option<std::path::PathBuf> {
    built_plugin("hello_viewer")
}

#[test]
fn the_hello_plugin_reads_renames_and_moves_through_the_api() {
    if hello_wasm().is_none() {
        return;
    }
    let f = fixture("hello");
    let store = f.store;
    let observer = f.rt.block_on(async {
        let observer = Pimble::new(RpcBackend::over(PimbleClient::connect(&f.url).await.unwrap()), "observer");
        observer.watch(store).await.unwrap();
        observer
    });
    let running = mount(&f, Approval { stores: Approved::Only(vec![store]), write: true }, "hello_viewer", "hello");

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
    let f = fixture("hello");
    let running = mount(&f, Approval::default(), "hello_viewer", "hello");
    running.pump_until(&f, "the refusal", |r| r.text().contains("The Hello viewer plugin was not allowed to use that store."));
    assert!(!running.text().contains("Plan"));
    running.instance.unmount();
}
