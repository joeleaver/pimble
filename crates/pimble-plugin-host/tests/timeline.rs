//! The timeline plugin end to end (docs/TIMELINE_CONTRACT.md wave 1): the built
//! `timeline_viewer` over a real in-process server, rendering into a mock
//! document. It puts a span and a point in 1066 on the line by their own
//! fields, groups the nodes under a top node with no date of its own into a
//! lane, lists a node whose field is not a date below the line, opens a
//! clicked node beside it, zooms and remembers where it was, renames by a
//! double-click, and (wave 3) moves a bar by a drag, a moment by Alt+Right,
//! and makes a node by a double-click on the line.
#![cfg(feature = "wasm-host")]

mod common;

use common::{built_plugin, fixture, mount, Fixture, Running};
use pimble_api::Addr;
use pimble_core::NodeId;
use pimble_plugin_abi::{HostRequest, NodeAddr};
use pimble_plugin_host::{Approval, Approved};
use rinch_core::dom::{DomDocument, NodeId as DomId};
use rinch_core::events::{dispatch_event, set_click_context, ClickContext, EventHandlerId, KeyEventData};

fn has_class(d: &rinch_core::dom::mock::MockDomDocument, n: DomId, class: &str) -> bool {
    d.get_attribute(n, "class").is_some_and(|c| c.split_whitespace().any(|w| w == class))
}

fn set(f: &Fixture, node: NodeId, name: &'static str, value: &'static str) {
    f.rt.block_on(f.app.edit(Addr::new(f.store, node), move |doc, now| -> Result<(), pimble_api::ApiError> {
        doc.set_field(name, Some(value), now)?;
        Ok(())
    }))
    .unwrap();
}

/// The item on the line labelled `label`.
fn item(r: &Running, label: &str) -> DomId {
    r.find(&|d, n| has_class(d, n, "timeline__item") && d.text_content(n).unwrap_or_default().contains(label))
        .unwrap_or_else(|| panic!("no item {label:?} in: {}", r.text()))
}

fn key(r: &Running, k: &str) -> bool {
    let node = r.find(&|d, n| has_class(d, n, "timeline") && d.get_attribute(n, "data-onkeydown").is_some()).expect("the timeline");
    rinch_core::events::dispatch_key_handler(EventHandlerId(r.handler(node, "data-onkeydown")), &KeyEventData::new(k, k))
}

fn pointer_at(x: f32, y: f32) {
    set_click_context(ClickContext { mouse_x: x, mouse_y: y, ..ClickContext::default() });
}

fn fire(r: &Running, node: DomId, attr: &str) {
    assert!(dispatch_event(EventHandlerId(r.handler(node, attr))), "{attr}");
}

fn field_of(f: &Fixture, node: NodeId, name: &str) -> Option<String> {
    let n = f.rt.block_on(f.service.get_node(f.store, node)).unwrap();
    n.metadata.fields.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
}

fn day(text: &str) -> chrono::NaiveDate {
    chrono::NaiveDate::parse_from_str(text, "%b %-d, %Y").unwrap_or_else(|e| panic!("{text:?}: {e}"))
}

#[test]
fn the_timeline_places_spans_points_and_lanes_opens_zooms_and_renames() {
    if built_plugin("timeline_viewer").is_none() {
        return;
    }
    // "Board" (two levels deep) holds "Plan" and "Write it".
    let f = fixture("timeline");
    let (store, board, plan, write) = (f.store, f.board, f.plan, f.write);
    let (draft, hastings, odd) = f.rt.block_on(async {
        let d = f.service.create_node(store, Some(write), "document", "Draft").await.unwrap();
        let h = f.service.create_node(store, Some(board), "document", "Hastings").await.unwrap();
        let o = f.service.create_node(store, Some(board), "document", "Odd").await.unwrap();
        (d, h, o)
    });
    set(&f, plan, "start", "Oct 1 2026");
    set(&f, plan, "end", "Oct 20 2026");
    set(&f, draft, "date", "Nov 3 2026");
    set(&f, hastings, "date", "October 14, 1066");
    set(&f, odd, "start", "whenever");
    let running = mount(&f, Approval { stores: Approved::All, write: true }, "timeline_viewer", "timeline");
    let r = &running;
    r.pump_until(&f, "the line", |r| r.text().contains("Hastings") && r.text().contains("Draft"));

    // Plan is a span, Hastings a point; Write it has no date and holds one,
    // so it is a lane, not an item.
    let d = r.doc.borrow();
    assert!(has_class(&d, item(r, "Plan"), "timeline__item--span"));
    assert!(has_class(&d, item(r, "Hastings"), "timeline__item--point"));
    assert!(d.get_attribute(item(r, "Hastings"), "title").is_some_and(|t| t.contains("Oct 14, 1066")));
    assert!(r.find(&|d, n| has_class(d, n, "timeline__lane") && d.text_content(n).unwrap_or_default().contains("Write it")).is_some(), "{}", r.text());
    assert!(r.find(&|d, n| has_class(d, n, "timeline__item") && d.text_content(n).unwrap_or_default().contains("Write it")).is_none());
    drop(d);
    // Odd's start is not a date: below the line, saying so.
    let unread = r.find(&|d, n| has_class(d, n, "timeline__unread")).expect("the list below the line");
    let said = r.doc.borrow().text_content(unread).unwrap_or_default();
    assert!(said.contains("Odd") && said.contains("\u{201c}whenever\u{201d} is not a date in the Gregorian calendar"), "{said}");
    // Fitted to what is on the line: the axis spans centuries.
    assert!(r.find(&|d, n| has_class(d, n, "timeline__tick")).is_some());

    // A click opens the node beside the timeline.
    assert!(dispatch_event(EventHandlerId(r.handler(item(r, "Plan"), "data-rid"))));
    r.pump_until(&f, "the open beside", |r| r.requests.borrow().contains(&HostRequest::OpenBeside { node: NodeAddr::from(Addr::new(store, plan)) }));

    // + zooms in, and once the gestures pause the view is remembered.
    let before = r.requests.borrow().iter().filter(|q| matches!(q, HostRequest::Remember { .. })).count();
    assert!(key(r, "+"));
    r.pump_until(&f, "the view remembered", |r| r.requests.borrow().iter().filter(|q| matches!(q, HostRequest::Remember { .. })).count() > before);

    // A double-click on Hastings renames it (a click on another first, so
    // the earlier click is not this one's first half).
    assert!(dispatch_event(EventHandlerId(r.handler(item(r, "Draft"), "data-rid"))));
    assert!(dispatch_event(EventHandlerId(r.handler(item(r, "Hastings"), "data-rid"))));
    assert!(dispatch_event(EventHandlerId(r.handler(item(r, "Hastings"), "data-rid"))));
    r.pump_until(&f, "the name field", |r| r.find(&|d, n| d.tag_name(n).as_deref() == Some("input")).is_some());
    r.type_text("Battle of Hastings");
    let field = r.find(&|d, n| d.tag_name(n).as_deref() == Some("input")).unwrap();
    assert!(rinch_core::events::dispatch_key_handler(EventHandlerId(r.handler(field, "data-onkeydown")), &KeyEventData::new("Enter", "Enter")));
    r.pump_until(&f, "the rename", |_| f.rt.block_on(f.service.get_node(store, hastings)).unwrap().metadata.title == "Battle of Hastings");

    // Wave 3. Plan dragged later: both its moments move by whole days, its
    // length kept, written as typed (to the day) in one edit; the click that
    // ends the drag opens nothing.
    let panel = r.find(&|d, n| has_class(d, n, "timeline__panel")).expect("the panel");
    let opened = r.requests.borrow().len();
    // The desktop's order: the press, its click, the moves, the release (a
    // browser clicks after the release instead).
    pointer_at(100.0, 60.0);
    fire(r, item(r, "Plan"), "data-onmousedown");
    assert!(dispatch_event(EventHandlerId(r.handler(item(r, "Plan"), "data-rid"))));
    pointer_at(160.0, 60.0);
    fire(r, panel, "data-onmousemove");
    fire(r, panel, "data-onmouseup");
    r.pump_until(&f, "the drag written", |_| field_of(&f, plan, "start").as_deref() != Some("Oct 1 2026"));
    let (s, e) = (day(&field_of(&f, plan, "start").unwrap()), day(&field_of(&f, plan, "end").unwrap()));
    assert!(s > day("Oct 1, 2026"), "{s}");
    assert_eq!((e - s).num_days(), 19);
    assert!(!r.requests.borrow()[opened..].iter().any(|q| matches!(q, HostRequest::OpenBeside { .. } | HostRequest::Open { .. })), "a drag opens nothing");
    // A press let go where it was is a click: Draft opens beside on the release.
    pointer_at(300.0, 60.0);
    fire(r, item(r, "Draft"), "data-onmousedown");
    assert!(dispatch_event(EventHandlerId(r.handler(item(r, "Draft"), "data-rid"))));
    assert!(!r.requests.borrow()[opened..].iter().any(|q| matches!(q, HostRequest::OpenBeside { .. })), "not before the release");
    fire(r, panel, "data-onmouseup");
    r.pump_until(&f, "Draft open beside", |r| r.requests.borrow().contains(&HostRequest::OpenBeside { node: NodeAddr::from(Addr::new(store, draft)) }));

    // Alt+Right moves the chosen moment (Draft, to the day) a day later.
    let mut alt = KeyEventData::new("ArrowRight", "ArrowRight");
    alt.alt = true;
    let root = r.find(&|d, n| has_class(d, n, "timeline") && d.get_attribute(n, "data-onkeydown").is_some()).unwrap();
    assert!(rinch_core::events::dispatch_key_handler(EventHandlerId(r.handler(root, "data-onkeydown")), &alt));
    r.pump_until(&f, "the nudge", |_| field_of(&f, draft, "date").as_deref() == Some("Nov 4, 2026"));

    // A double-click on the line's background makes a node at that moment
    // and names it in place.
    let before = f.rt.block_on(f.service.get_children(store, board)).unwrap().1.len();
    pointer_at(400.0, 50.0);
    fire(r, panel, "data-rid");
    fire(r, panel, "data-rid");
    r.pump_until(&f, "the new node's name field", |r| r.find(&|d, n| d.tag_name(n).as_deref() == Some("input")).is_some());
    r.type_text("Coronation");
    let field_node = r.find(&|d, n| d.tag_name(n).as_deref() == Some("input")).unwrap();
    assert!(rinch_core::events::dispatch_key_handler(EventHandlerId(r.handler(field_node, "data-onkeydown")), &KeyEventData::new("Enter", "Enter")));
    r.pump_until(&f, "the node made", |r| r.text().contains("Coronation"));
    let children = f.rt.block_on(f.service.get_children(store, board)).unwrap().1;
    assert_eq!(children.len(), before + 1);
    let made = children.iter().find(|n| n.metadata.title == "Coronation").expect("Coronation under the timeline");
    assert!(made.metadata.fields.iter().any(|(k, v)| k == "date" && !v.is_empty()), "{:?}", made.metadata.fields);
    r.instance.unmount();
}
