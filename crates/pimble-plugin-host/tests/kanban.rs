//! The kanban plugin end to end (docs/KANBAN_CONTRACT.md wave 1): the built
//! `kanban_viewer` over a real in-process server, rendering into a mock
//! document. It draws columns and cards in the tree's order, tells the host
//! which card the person is on, moves cards by its "Move to" buttons and by
//! Alt+Shift+arrows (each judged in the metanode first), renames a column in
//! place, adds and deletes a card, holds a move into another store for "Move
//! anyway", shows `plan`'s refusal, follows a move made elsewhere, and draws
//! who is on a card with the app's own chips.
#![cfg(feature = "wasm-host")]

mod common;

use common::{built_plugin, fixture, mount, Fixture, Running};
use pimble_api::Addr;
use pimble_core::plan::Refusal;
use pimble_core::{NodeId, Place, StoreId};
use pimble_plugin_abi::{HostRequest, HostState, NodeAddr, NodePeople, PresentPerson};
use pimble_plugin_host::{Approval, Approved};
use rinch_core::dom::{DomDocument, NodeId as DomId};
use rinch_core::events::{dispatch_event, dispatch_key_handler, EventHandlerId, KeyEventData};

fn children(f: &Fixture, store: StoreId, node: NodeId) -> Vec<String> {
    let (_, nodes) = f.rt.block_on(f.service.get_children(store, node)).unwrap();
    nodes.into_iter().map(|n| n.metadata.title).collect()
}

fn has_class(d: &rinch_core::dom::mock::MockDomDocument, n: DomId, class: &str) -> bool {
    d.get_attribute(n, "class").is_some_and(|c| c.split_whitespace().any(|w| w == class))
}

/// Click the card or header labelled `label` (its element with the class
/// `class` whose text starts with it).
fn choose(r: &Running, class: &str, label: &str) {
    let node = r
        .find(&|d, n| has_class(d, n, class) && d.text_content(n).unwrap_or_default().contains(label))
        .unwrap_or_else(|| panic!("no {class} {label:?} in: {}", r.text()));
    assert!(dispatch_event(EventHandlerId(r.handler(node, "data-rid"))));
}

/// A key on the element of class `class`.
fn key_on(r: &Running, class: &str, key: KeyEventData) -> bool {
    let node = r.find(&|d, n| has_class(d, n, class) && d.get_attribute(n, "data-onkeydown").is_some()).unwrap_or_else(|| panic!("no {class}"));
    dispatch_key_handler(EventHandlerId(r.handler(node, "data-onkeydown")), &key)
}

fn alt(key: &str) -> KeyEventData {
    KeyEventData::new(key, key).with_modifiers(false, true, true, false)
}

#[test]
fn the_kanban_shows_moves_renames_adds_deletes_and_shows_who_is_there() {
    if built_plugin("kanban_viewer").is_none() {
        return;
    }
    let f = fixture("kanban");
    let store = f.store;
    // Columns "Plan" and "Write it"; cards under them.
    let (alpha, bravo, charlie) = f.rt.block_on(async {
        let a = f.service.create_node(store, Some(f.plan), "document", "Alpha").await.unwrap();
        let b = f.service.create_node(store, Some(f.plan), "document", "Bravo").await.unwrap();
        let c = f.service.create_node(store, Some(f.write), "document", "Charlie").await.unwrap();
        // A subtask of Alpha.
        f.service.create_node(store, Some(a), "document", "Step one").await.unwrap();
        (a, b, c)
    });
    let running = mount(&f, Approval { stores: Approved::All, write: true }, "kanban_viewer", "kanban");
    let r = &running;
    r.pump_until(&f, "the board", |r| r.text().contains("Charlie"));
    let text = r.text();
    assert!(text.find("Plan").unwrap() < text.find("Write it").unwrap(), "{text}");
    assert!(text.find("Alpha").unwrap() < text.find("Bravo").unwrap(), "{text}");
    assert!(!text.contains("Step one"), "a subtask is a count on its card, not a card: {text}");
    assert!(r.find(&|d, n| has_class(d, n, "kanban__subtasks") && d.text_content(n).unwrap_or_default().trim() == "1").is_some(), "{text}");

    // Choosing a card tells the host the person is on it.
    choose(r, "kanban__card", "Alpha");
    r.pump_until(&f, "the focus", |r| r.requests.borrow().contains(&HostRequest::Focus { node: Some(NodeAddr::from(Addr::new(store, alpha))) }));

    // "Move to ▸ Write it": the bottom of that column.
    r.click("button", "Write it");
    r.pump_until(&f, "the move", |_| children(&f, store, f.write) == ["Charlie", "Alpha"]);
    r.pump_until(&f, "the board after the move", |r| {
        let t = r.text();
        t.find("Charlie").unwrap() < t.find("Alpha").unwrap()
    });

    // Alt+Shift+Up: above Charlie; Alt+Shift+Left: back into Plan at the same height.
    assert!(key_on(r, "kanban", alt("ArrowUp")));
    r.pump_until(&f, "the move up", |_| children(&f, store, f.write) == ["Alpha", "Charlie"]);
    r.pump_until(&f, "the board after the move up", |r| {
        let t = r.text();
        t.find("Alpha").unwrap() < t.find("Charlie").unwrap()
    });
    assert!(key_on(r, "kanban", alt("ArrowLeft")));
    r.pump_until(&f, "the move left", |_| children(&f, store, f.plan) == ["Alpha", "Bravo"]);

    // Open: the host opens it in the pane.
    r.pump_until(&f, "the actions", |r| r.text().contains("Open"));
    r.click("button", "Open");
    r.pump_until(&f, "the open", |r| r.requests.borrow().contains(&HostRequest::Open { node: NodeAddr::from(Addr::new(store, alpha)) }));

    // Rename the column in place: the one write, with the explicit title.
    choose(r, "kanban__header", "Plan");
    r.click("button", "Rename");
    r.pump_until(&f, "the name field", |r| r.find(&|d, n| d.tag_name(n).as_deref() == Some("input")).is_some());
    r.type_text("Planning");
    assert!(key_on(r, "kanban__field", KeyEventData::new("Enter", "Enter")));
    r.pump_until(&f, "the rename", |_| f.rt.block_on(f.service.get_node(store, f.plan)).unwrap().metadata.title == "Planning");
    let column = f.rt.block_on(f.service.get_node(store, f.plan)).unwrap();
    assert_eq!(column.metadata.custom.get(pimble_core::custom_keys::EXPLICIT_TITLE), Some(&serde_json::Value::Bool(true)));

    // "+ Add card": a create under the column, last, then named in place.
    r.pump_until(&f, "the renamed header", |r| r.text().contains("Planning"));
    let add = r.find(&|d, n| has_class(d, n, "kanban__add-card")).expect("+ Add card");
    assert!(dispatch_event(EventHandlerId(r.handler(add, "data-rid"))));
    r.pump_until(&f, "the new card", |_| children(&f, store, f.plan).len() == 3);
    r.pump_until(&f, "its name field", |r| r.find(&|d, n| d.tag_name(n).as_deref() == Some("input")).is_some());
    r.type_text("Delta");
    assert!(key_on(r, "kanban__field", KeyEventData::new("Enter", "Enter")));
    r.pump_until(&f, "the new card's name", |_| children(&f, store, f.plan) == ["Alpha", "Bravo", "Delta"]);

    // Delete it: asked of `plan`, then a tombstone.
    r.pump_until(&f, "Delta on the board", |r| r.text().contains("Delta"));
    choose(r, "kanban__card", "Delta");
    r.pump_until(&f, "its actions", |r| r.text().contains("Delete"));
    r.click("button", "Delete");
    r.pump_until(&f, "the delete", |_| children(&f, store, f.plan) == ["Alpha", "Bravo"]);
    r.pump_until(&f, "the line", |r| r.text().contains("Deleted \u{201c}Delta\u{201d}."));

    // A move made elsewhere shows live.
    f.rt.block_on(f.service.move_node(store, charlie, f.plan, Place::First)).unwrap();
    r.pump_until(&f, "the move from elsewhere", |r| {
        let t = r.text();
        t.find("Charlie").unwrap() < t.find("Alpha").unwrap()
    });

    // Who is on a card, drawn with the app's chips; the pane follows Sam, so
    // Sam's card is edged in Sam's color; a click on the chip follows.
    let sam = PresentPerson { key: "sub-sam".into(), name: "Sam".into(), initials: "S".into(), color: "#aa3300".into(), title: "Sam, on laptop".into(), agent: false };
    let claude = PresentPerson { key: "sub-sam/agent/Claude".into(), name: "Claude".into(), initials: "C".into(), color: "#0033aa".into(), title: "Claude, an AI agent".into(), agent: true };
    let on_bravo = NodeAddr::from(Addr::new(store, bravo));
    assert!(r.instance.set_host_state(&HostState { dark: false, people: vec![NodePeople { node: on_bravo, people: vec![sam, claude] }], following: Some("sub-sam".into()) }));
    r.pump_until(&f, "the chips", |r| r.find(&|d, n| has_class(d, n, "pimble-pane__person--agent")).is_some());
    let card = r.find(&|d, n| has_class(d, n, "kanban__card") && d.text_content(n).unwrap_or_default().contains("Bravo")).unwrap();
    assert!(r.doc.borrow().get_attribute(card, "style").unwrap().contains("2px solid #aa3300"));
    let chip = r
        .find(&|d, n| has_class(d, n, "pimble-pane__person") && !has_class(d, n, "pimble-pane__person--agent"))
        .expect("Sam's chip");
    assert_eq!(r.doc.borrow().get_attribute(chip, "title").as_deref(), Some("Sam, on laptop (click to follow)"));
    assert!(dispatch_event(EventHandlerId(r.handler(chip, "data-rid"))));
    r.pump_until(&f, "the follow", |r| r.requests.borrow().contains(&HostRequest::Follow { person: "sub-sam".into() }));

    // A column that is another store's: a move into it is a new node there,
    // which waits for "Move anyway".
    let (other, mount_column) = f.rt.block_on(async {
        let (other, other_root) = f.service.create_store(f.dir.path().join("home.pimble"), "Home").await.unwrap();
        let (mount, _) = f.service.create_mount(store, f.board, other, other_root, Some("Home".into())).await.unwrap();
        (other, mount)
    });
    r.pump_until(&f, "the mounted column", |r| r.text().contains("Home"));
    choose(r, "kanban__card", "Alpha");
    r.pump_until(&f, "Alpha's actions", |r| r.find(&|d, n| d.tag_name(n).as_deref() == Some("button") && d.text_content(n).unwrap_or_default().trim() == "Home").is_some());
    r.click("button", "Home");
    r.pump_until(&f, "the warning", |r| r.text().contains("Move anyway"));
    r.click("button", "Cancel");
    r.pump_until(&f, "the warning gone", |r| !r.text().contains("Move anyway"));
    assert_eq!(children(&f, store, f.plan), ["Charlie", "Alpha", "Bravo"]);

    // With that store gone from this computer (closed, and its folder
    // removed, so the mount cannot open it again), the explorer's refusal,
    // and nothing sent.
    f.rt.block_on(f.service.close_store(other)).unwrap();
    std::fs::remove_dir_all(f.dir.path().join("home.pimble")).unwrap();
    r.click("button", "Home");
    r.pump_until(&f, "the refusal", |r| r.text().contains(Refusal::MountNotHere.sentence()));
    assert_eq!(children(&f, store, f.plan), ["Charlie", "Alpha", "Bravo"]);
    let _ = mount_column;

    r.instance.unmount();
    assert!(r.doc.borrow().get_children(r.container).is_empty());
}

#[test]
fn a_kanban_allowed_only_to_read_is_refused_its_writes() {
    if built_plugin("kanban_viewer").is_none() {
        return;
    }
    let f = fixture("kanban");
    let store = f.store;
    f.rt.block_on(f.service.create_node(store, Some(f.plan), "document", "Alpha")).unwrap();
    // The person may change the store, the plugin only read it: the board
    // shows the gestures (they are the person's), and the host refuses the
    // plugin's write with its sentence before it leaves. (A board the person
    // may only read, a reader's share, shows none: `Node::access` hides
    // them, checked by hand.)
    let running = mount(&f, Approval { stores: Approved::Only(vec![store]), write: false }, "kanban_viewer", "kanban");
    let r = &running;
    r.pump_until(&f, "the board", |r| r.text().contains("Alpha"));
    choose(r, "kanban__card", "Alpha");
    r.pump_until(&f, "the actions", |r| r.text().contains("Write it"));
    r.click("button", "Write it");
    r.pump_until(&f, "the refusal", |r| r.text().contains("The Kanban plugin was allowed to read this store, not to change it."));
    assert_eq!(children(&f, store, f.plan), ["Alpha"]);
    r.instance.unmount();
}
