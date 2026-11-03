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
use rinch_core::events::{dispatch_event, dispatch_key_handler, set_click_context, ClickContext, EventHandlerId, KeyEventData};

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

/// A node's key in the board's sortable lists (`board::key_of`).
fn key(store: StoreId, node: NodeId) -> String {
    format!("{store}/{node}")
}

/// The element of the sortable item keyed `key`.
fn sortable(r: &Running, key: &str) -> DomId {
    r.find(&|d, n| d.get_attribute(n, "data-sortable-key").as_deref() == Some(key))
        .unwrap_or_else(|| panic!("no sortable {key} in: {}", r.text()))
}

/// The first element under `node` (or `node`) of class `class`.
fn under(r: &Running, node: DomId, class: &str) -> Option<DomId> {
    fn walk(d: &rinch_core::dom::mock::MockDomDocument, n: DomId, class: &str) -> Option<DomId> {
        if has_class(d, n, class) {
            return Some(n);
        }
        d.get_children(n).into_iter().find_map(|c| walk(d, c, class))
    }
    walk(&r.doc.borrow(), node, class)
}

/// The runtime's click context for a drag step over a box at `(x, y, w, h)`
/// with the pointer at `(px, py)`, as the desktop sets it for `data-ondragover`.
fn over(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32) {
    set_click_context(ClickContext {
        mouse_x: px,
        mouse_y: py,
        element_x: x,
        element_y: y,
        element_width: w,
        element_height: h,
        ..ClickContext::default()
    });
}

/// Fire the host handler `attr` of `node`, as the drag runtime does.
fn fire(r: &Running, node: DomId, attr: &str) {
    assert!(dispatch_event(EventHandlerId(r.handler(node, attr))), "{attr}");
}

#[test]
fn cards_and_columns_drag_through_the_host_and_a_refused_drop_says_why() {
    if built_plugin("kanban_viewer").is_none() {
        return;
    }
    let f = fixture("kanban");
    let store = f.store;
    let (alpha, bravo, charlie) = f.rt.block_on(async {
        let a = f.service.create_node(store, Some(f.plan), "document", "Alpha").await.unwrap();
        let b = f.service.create_node(store, Some(f.plan), "document", "Bravo").await.unwrap();
        let c = f.service.create_node(store, Some(f.write), "document", "Charlie").await.unwrap();
        (a, b, c)
    });
    let running = mount(&f, Approval { stores: Approved::All, write: true }, "kanban_viewer", "kanban");
    let r = &running;
    r.pump_until(&f, "the board", |r| r.text().contains("Charlie"));

    // Alpha dragged over the lower half of Charlie, in the other column: it
    // lands after Charlie, one moveNode.
    let card = sortable(r, &key(store, alpha));
    rinch_core::events::reset_drag_ghost_visibility();
    fire(r, card, "data-ondragstart");
    // The guest's drag start hid the window's ghost through the host.
    assert!(!rinch_core::events::is_drag_ghost_visible());
    rinch_core::events::reset_drag_ghost_visibility();
    assert!(has_class(&r.doc.borrow(), card, "rinch-sortable__item--dragging"));
    let target = sortable(r, &key(store, charlie));
    over(150.0, 135.0, 100.0, 100.0, 240.0, 40.0);
    fire(r, target, "data-ondragover");
    assert!(has_class(&r.doc.borrow(), target, "rinch-sortable__item--drop-after"), "the caret after Charlie");
    fire(r, card, "data-ondragmove");
    fire(r, target, "data-ondrop");
    fire(r, card, "data-ondragend");
    r.pump_until(&f, "the card's drop", |_| children(&f, store, f.write) == ["Charlie", "Alpha"]);
    assert_eq!(children(&f, store, f.plan), ["Bravo"]);
    r.pump_until(&f, "the board after the drop", |r| {
        let t = r.text();
        t.find("Charlie").unwrap() < t.find("Alpha").unwrap()
    });

    // The "Write it" column dragged by its header over the left half of
    // "Plan": first on the board, a move of the column node.
    let column = sortable(r, &key(store, f.write));
    assert!(r.find(&|d, n| has_class(d, n, "rinch-sortable__handle") && d.get_attribute(n, "draggable").as_deref() == Some("true")).is_some());
    fire(r, column, "data-ondragstart");
    let plan_column = sortable(r, &key(store, f.plan));
    over(110.0, 50.0, 100.0, 40.0, 260.0, 400.0);
    fire(r, plan_column, "data-ondragover");
    assert!(has_class(&r.doc.borrow(), plan_column, "rinch-sortable__item--drop-before"));
    fire(r, plan_column, "data-ondrop");
    fire(r, column, "data-ondragend");
    r.pump_until(&f, "the column's drop", |_| children(&f, store, f.board) == ["Write it", "Plan"]);
    rinch_core::events::reset_drag_ghost_visibility();

    // A column mounted from another store, whose store is then gone from
    // this computer: a card dragged into it is refused at the caret with
    // `plan`'s sentence, and the drop sends nothing.
    let other = f.rt.block_on(async {
        let (other, other_root) = f.service.create_store(f.dir.path().join("home.pimble"), "Home").await.unwrap();
        f.service.create_mount(store, f.board, other, other_root, Some("Home".into())).await.unwrap();
        other
    });
    r.pump_until(&f, "the mounted column", |r| r.text().contains("Home"));
    f.rt.block_on(f.service.close_store(other)).unwrap();
    std::fs::remove_dir_all(f.dir.path().join("home.pimble")).unwrap();
    // A change elsewhere reads the board, and its stores, again.
    f.rt.block_on(f.service.move_node(store, bravo, f.write, Place::First)).unwrap();
    r.pump_until(&f, "the board read again", |r| {
        let t = r.text();
        t.find("Bravo").unwrap() < t.find("Charlie").unwrap()
    });
    let home = r
        .find(&|d, n| has_class(d, n, "kanban__column") && d.text_content(n).unwrap_or_default().contains("Home"))
        .expect("the Home column");
    let card = sortable(r, &key(store, charlie));
    fire(r, card, "data-ondragstart");
    let list = under(r, home, "kanban__cards").expect("the Home column's cards");
    over(700.0, 80.0, 650.0, 60.0, 240.0, 28.0);
    fire(r, list, "data-ondragover");
    let reason = under(r, list, "rinch-sortable__reason").expect("the list's reason slot");
    assert!(has_class(&r.doc.borrow(), list, "rinch-sortable--refused"));
    assert!(has_class(&r.doc.borrow(), reason, "rinch-sortable__reason--shown"));
    assert_eq!(r.doc.borrow().text_content(reason).as_deref(), Some(Refusal::MountNotHere.sentence()));
    fire(r, list, "data-ondrop");
    fire(r, card, "data-ondragend");
    // Nothing was asked of the server: no call waits, and the lists stand.
    assert!(r.calls.borrow().iter().all(|(_, c)| !format!("{c:?}").contains("Move")), "{:?}", r.calls.borrow());
    r.pump_until(&f, "the caret gone", |r| r.find(&|d, n| has_class(d, n, "rinch-sortable__reason--shown")).is_none());
    assert_eq!(children(&f, store, f.write), ["Bravo", "Charlie", "Alpha"]);
    r.instance.unmount();
}
