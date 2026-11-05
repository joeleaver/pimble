//! The todo list plugin end to end (docs/TODO_CONTRACT.md wave 1): the built
//! `todo_viewer` over a real in-process server, rendering into a mock
//! document. It lists the subtree in the tree's order, checks an item off as a
//! timestamp in its `done` field and unchecks it, opens a clicked item beside
//! the list, renames by a double-click, makes the next item with Enter and
//! takes an empty one back, indents and outdents with Tab, and deletes.
#![cfg(feature = "wasm-host")]

mod common;

use common::{built_plugin, fixture, mount, Fixture, Running};
use pimble_api::Addr;
use pimble_core::{NodeId, StoreId};
use pimble_plugin_abi::{HostRequest, NodeAddr};
use pimble_plugin_host::{Approval, Approved};
use rinch_core::dom::{DomDocument, NodeId as DomId};
use rinch_core::events::{dispatch_event, EventHandlerId, KeyEventData};

fn children(f: &Fixture, store: StoreId, node: NodeId) -> Vec<String> {
    let (_, nodes) = f.rt.block_on(f.service.get_children(store, node)).unwrap();
    nodes.into_iter().map(|n| n.metadata.title).collect()
}

fn done_field(f: &Fixture, store: StoreId, node: NodeId) -> Option<String> {
    let node = f.rt.block_on(f.service.get_node(store, node)).unwrap();
    node.metadata.fields.into_iter().find(|(n, _)| n == "done").map(|(_, v)| v)
}

fn has_class(d: &rinch_core::dom::mock::MockDomDocument, n: DomId, class: &str) -> bool {
    d.get_attribute(n, "class").is_some_and(|c| c.split_whitespace().any(|w| w == class))
}

/// The row of the item labelled `label`.
fn row(r: &Running, label: &str) -> DomId {
    r.find(&|d, n| has_class(d, n, "todo__item") && d.text_content(n).unwrap_or_default().contains(label))
        .unwrap_or_else(|| panic!("no item {label:?} in: {}", r.text()))
}

/// Click the item labelled `label`.
fn click(r: &Running, label: &str) {
    assert!(dispatch_event(EventHandlerId(r.handler(row(r, label), "data-rid"))));
}

/// A key on the list.
fn key(r: &Running, key: KeyEventData) -> bool {
    let node = r.find(&|d, n| has_class(d, n, "todo") && d.get_attribute(n, "data-onkeydown").is_some()).expect("the list");
    rinch_core::events::dispatch_key_handler(EventHandlerId(r.handler(node, "data-onkeydown")), &key)
}

fn plain(k: &str) -> KeyEventData {
    KeyEventData::new(k, k)
}

fn input(r: &Running) -> Option<DomId> {
    r.find(&|d, n| d.tag_name(n).as_deref() == Some("input"))
}

fn field_key(r: &Running, k: &str) -> bool {
    let field = input(r).expect("a name field");
    rinch_core::events::dispatch_key_handler(EventHandlerId(r.handler(field, "data-onkeydown")), &plain(k))
}

#[test]
fn the_todo_list_checks_opens_renames_adds_indents_and_deletes() {
    if built_plugin("todo_viewer").is_none() {
        return;
    }
    // The fixture's "Board" holds "Plan" and "Write it"; a sub-todo of Plan.
    let f = fixture("todo");
    let (store, board, plan, write) = (f.store, f.board, f.plan, f.write);
    let step = f.rt.block_on(f.service.create_node(store, Some(plan), "document", "Step one")).unwrap();
    let running = mount(&f, Approval { stores: Approved::All, write: true }, "todo_viewer", "todo");
    let r = &running;
    r.pump_until(&f, "the list", |r| r.text().contains("Write it") && r.text().contains("Step one"));
    let text = r.text();
    assert!(text.find("Plan").unwrap() < text.find("Step one").unwrap(), "{text}");
    assert!(text.find("Step one").unwrap() < text.find("Write it").unwrap(), "{text}");
    assert!(text.contains("0 of 3 done"), "{text}");
    assert!(text.contains("0/1"), "Plan shows its progress: {text}");
    assert!(!text.contains("Rename") && !text.contains("Open"), "no buttons on rows: {text}");

    // A click opens the item beside the list.
    click(r, "Write it");
    r.pump_until(&f, "the open beside", |r| r.requests.borrow().contains(&HostRequest::OpenBeside { node: NodeAddr::from(Addr::new(store, write)) }));

    // Space checks the chosen item off: a timestamp in its `done` field; again
    // takes it back.
    assert!(key(r, plain(" ")));
    r.pump_until(&f, "the check", |_| done_field(&f, store, write).is_some());
    let when = done_field(&f, store, write).unwrap();
    assert!(when.contains('T') && when.len() >= 20, "an RFC 3339 timestamp: {when}");
    r.pump_until(&f, "the count", |r| r.text().contains("1 of 3 done"));
    assert!(key(r, plain(" ")));
    r.pump_until(&f, "the uncheck", |_| done_field(&f, store, write).is_none());

    // The checkbox checks Step one, and Plan's progress follows (once the
    // list shows the uncheck).
    r.pump_until(&f, "the list after the uncheck", |r| r.text().contains("0 of 3 done"));
    let check = r
        .find(&|d, n| has_class(d, n, "todo__check") && d.parent_node(n).is_some_and(|p| d.text_content(p).unwrap_or_default().contains("Step one")))
        .expect("Step one's checkbox");
    assert!(dispatch_event(EventHandlerId(r.handler(check, "data-rid"))));
    r.pump_until(&f, "Step one checked", |_| done_field(&f, store, step).is_some());
    r.pump_until(&f, "Plan's progress", |r| r.text().contains("1/1"));

    // A double-click renames; Enter keeps the name and starts the next item.
    // (A click on another item first, so the earlier click on Write it, a
    // moment ago in a test, does not make this the double-click's second.)
    click(r, "Plan");
    click(r, "Write it");
    click(r, "Write it");
    r.pump_until(&f, "the name field", |r| input(r).is_some());
    r.type_text("Writing");
    assert!(field_key(r, "Enter"));
    r.pump_until(&f, "the rename", |_| children(&f, store, board).first().map(String::as_str) == Some("Plan") && children(&f, store, board).get(1).map(String::as_str) == Some("Writing"));
    r.pump_until(&f, "the next item", |_| children(&f, store, board).len() == 3);
    r.pump_until(&f, "its name field", |r| input(r).is_some());
    // Enter on the new item left empty takes it back.
    assert!(field_key(r, "Enter"));
    r.pump_until(&f, "the empty item gone", |_| children(&f, store, board) == ["Plan", "Writing"]);

    // Tab indents the chosen item under the one above, Shift+Tab outdents it.
    r.pump_until(&f, "the list after the rename", |r| r.text().contains("Writing"));
    click(r, "Plan");
    click(r, "Writing");
    assert!(key(r, plain("Tab")));
    r.pump_until(&f, "the indent", |_| children(&f, store, plan) == ["Step one", "Writing"]);
    r.pump_until(&f, "the list after the indent", |r| r.text().contains("1/2"));
    assert!(key(r, KeyEventData::new("Tab", "Tab").with_modifiers(false, true, false, false)));
    r.pump_until(&f, "the outdent", |_| children(&f, store, board) == ["Plan", "Writing"]);

    // Delete deletes the chosen item.
    r.pump_until(&f, "the list after the outdent", |r| r.text().contains("0/1") || r.text().contains("1/1"));
    assert!(key(r, plain("Delete")));
    r.pump_until(&f, "the delete", |_| children(&f, store, board) == ["Plan"]);
    r.pump_until(&f, "the line", |r| r.text().contains("Deleted \u{201c}Writing\u{201d}."));
    r.instance.unmount();
}

/// The element of the sortable row keyed `key`.
fn sortable(r: &Running, key: &str) -> DomId {
    r.find(&|d, n| d.get_attribute(n, "data-sortable-key").as_deref() == Some(key))
        .unwrap_or_else(|| panic!("no sortable {key} in: {}", r.text()))
}

fn key_of(store: StoreId, node: NodeId) -> String {
    format!("{store}/{node}")
}

/// The pointer at `(150, py)` over a row whose box is `(100, 100, 240, 40)`:
/// `py` under 120 is its upper half (a drop before it), over 120 its lower.
fn over(py: f32) {
    rinch_core::events::set_click_context(rinch_core::events::ClickContext {
        mouse_x: 150.0,
        mouse_y: py,
        element_x: 100.0,
        element_y: 100.0,
        element_width: 240.0,
        element_height: 40.0,
        ..rinch_core::events::ClickContext::default()
    });
}

fn fire(r: &Running, node: DomId, attr: &str) {
    assert!(dispatch_event(EventHandlerId(r.handler(node, attr))), "{attr}");
}

/// Drag the row keyed `from` over the row keyed `to` at `py`, and drop it.
fn drag(r: &Running, from: &str, to: &str, py: f32) {
    let row = sortable(r, from);
    fire(r, row, "data-ondragstart");
    let target = sortable(r, to);
    over(py);
    fire(r, target, "data-ondragover");
    fire(r, row, "data-ondragmove");
    fire(r, target, "data-ondrop");
    fire(r, row, "data-ondragend");
}

#[test]
fn rows_drag_beside_and_into_an_open_row_and_a_drop_into_its_own_sub_todos_is_refused() {
    if built_plugin("todo_viewer").is_none() {
        return;
    }
    let f = fixture("todo");
    let (store, board, plan, write) = (f.store, f.board, f.plan, f.write);
    let step = f.rt.block_on(f.service.create_node(store, Some(plan), "document", "Step one")).unwrap();
    let running = mount(&f, Approval { stores: Approved::All, write: true }, "todo_viewer", "todo");
    let r = &running;
    r.pump_until(&f, "the list", |r| r.text().contains("Write it") && r.text().contains("Step one"));

    // Write it over the upper half of Plan: first in the list.
    drag(r, &key_of(store, write), &key_of(store, plan), 105.0);
    r.pump_until(&f, "the drop at the top", |_| children(&f, store, board) == ["Write it", "Plan"]);
    r.pump_until(&f, "the list after it", |r| { let t = r.text(); t.find("Write it").unwrap() < t.find("Plan").unwrap() });

    // Over the lower half of Plan, whose sub-todo shows below it: Plan's first sub-todo.
    drag(r, &key_of(store, write), &key_of(store, plan), 135.0);
    r.pump_until(&f, "the drop into Plan", |_| children(&f, store, plan) == ["Write it", "Step one"]);
    r.pump_until(&f, "Plan's progress", |r| r.text().contains("0/2"));

    // Plan over the lower half of its own sub-todo: refused at the caret with
    // `plan`'s sentence, and nothing is sent.
    let row = sortable(r, &key_of(store, plan));
    fire(r, row, "data-ondragstart");
    let target = sortable(r, &key_of(store, step));
    over(135.0);
    fire(r, target, "data-ondragover");
    let reason = r.find(&|d, n| has_class(d, n, "rinch-sortable__reason--shown")).expect("the reason shown at the caret");
    assert_eq!(r.doc.borrow().text_content(reason).as_deref(), Some("A node can't go inside itself."));
    fire(r, target, "data-ondrop");
    fire(r, row, "data-ondragend");
    assert!(r.calls.borrow().iter().all(|(_, c)| !format!("{c:?}").contains("Move")), "{:?}", r.calls.borrow());
    assert_eq!(children(&f, store, board), ["Plan"]);
    r.instance.unmount();
}

#[test]
fn a_due_date_reads_as_a_date_and_a_past_one_is_overdue_until_done() {
    if built_plugin("todo_viewer").is_none() {
        return;
    }
    let f = fixture("todo");
    let (store, plan, write) = (f.store, f.plan, f.write);
    let set = |node: NodeId, name: &'static str, value: &'static str| {
        f.rt.block_on(f.app.edit(Addr::new(store, node), move |doc, now| -> Result<(), pimble_api::ApiError> {
            doc.set_field(name, Some(value), now)?;
            Ok(())
        }))
        .unwrap();
    };
    set(write, "due", "2020-01-01");
    set(plan, "due", "whenever");
    let running = mount(&f, Approval { stores: Approved::All, write: true }, "todo_viewer", "todo");
    let r = &running;
    r.pump_until(&f, "the due dates", |r| r.text().contains("Jan 1, 2020") && r.text().contains("whenever"));
    assert!(r.find(&|d, n| has_class(d, n, "todo__due--overdue")).is_some(), "a past date is overdue");
    // Checked off, it is not overdue any more.
    set(write, "done", "2026-11-05T12:00:00Z");
    r.pump_until(&f, "not overdue", |r| r.find(&|d, n| has_class(d, n, "todo__due--overdue")).is_none());
    r.instance.unmount();
}
