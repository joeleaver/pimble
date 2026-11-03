//! The host's side of the op log over rinch's `MockDomDocument`: ops land under the
//! plugin's container, handler ids are translated both ways, and a guest that names
//! a node not its own is refused (docs/PLUGINS_CONTRACT.md section 5).

use std::cell::RefCell;
use std::rc::Rc;

use pimble_plugin_abi::{encode, Event, EventData, Op, GUEST_BODY, GUEST_ROOT};
use pimble_plugin_host::{Answered, Applier, ApplierOptions, Call, Failure, GuestEngine, Instance, InstanceOptions, Refusal};
use rinch_core::dom::mock::MockDomDocument;
use rinch_core::dom::{DomDocument, NodeId};
use rinch_core::events::{dispatch_event, dispatch_key_handler, has_click_handler, EventHandlerId, KeyEventData};

fn document() -> (Rc<RefCell<MockDomDocument>>, Rc<RefCell<dyn DomDocument>>, NodeId) {
    let doc = Rc::new(RefCell::new(MockDomDocument::new()));
    let container = doc.borrow_mut().create_element("div");
    let body = doc.borrow().body();
    doc.borrow_mut().append_child(body, container);
    let dyn_doc: Rc<RefCell<dyn DomDocument>> = doc.clone();
    (doc, dyn_doc, container)
}

fn element(id: u64, tag: &str) -> Op {
    Op::CreateElement { id, tag: tag.into() }
}

#[test]
fn ops_build_the_tree_under_the_container_and_handlers_are_translated() {
    let (doc, dyn_doc, container) = document();
    let seen: Rc<RefCell<Vec<Event>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    let mut applier = Applier::new(
        dyn_doc,
        container,
        Rc::new(move |event: Event| {
            let consumed = matches!(&event.data, EventData::Key(k) if k.key == "Escape");
            sink.borrow_mut().push(event);
            consumed as u32
        }),
        ApplierOptions { text_field_shim: true },
    );
    applier
        .apply(vec![
            element(2, "section"),
            Op::SetAttribute { node: 2, name: "class".into(), value: "card".into() },
            Op::CreateText { id: 3, text: "Hello".into() },
            Op::Append { parent: 2, child: 3 },
            element(4, "button"),
            Op::SetAttribute { node: 4, name: "data-rid".into(), value: "41".into() },
            Op::Append { parent: 2, child: 4 },
            element(5, "input"),
            Op::SetAttribute { node: 5, name: "data-onkeydown".into(), value: "42".into() },
            Op::InsertBefore { parent: 2, child: 5, reference: 4 },
            Op::Append { parent: GUEST_BODY, child: 2 },
        ])
        .unwrap();

    let d = doc.borrow();
    let section = applier.host_node(2).unwrap();
    assert_eq!(d.get_children(container), vec![section]);
    assert_eq!(d.get_attribute(section, "class").as_deref(), Some("card"));
    assert_eq!(d.text_content(section).as_deref(), Some("Hello"));
    let (button, input) = (applier.host_node(4).unwrap(), applier.host_node(5).unwrap());
    assert_eq!(d.get_children(section)[1..], [input, button]);

    // The host's attribute holds a host handler, never the guest's id.
    let host_click: usize = d.get_attribute(button, "data-rid").unwrap().parse().unwrap();
    assert_ne!(host_click, 41);
    // A field with key handlers and no `data-oninput` gets the desktop's no-op one.
    assert!(d.get_attribute(input, "data-oninput").is_some());
    let host_key: usize = d.get_attribute(input, "data-onkeydown").unwrap().parse().unwrap();
    drop(d);

    // rinch dispatching the host handler reaches the guest's, by its own id.
    assert!(dispatch_event(EventHandlerId(host_click)));
    assert!(matches!(seen.borrow().as_slice(), [Event { handler: 41, data: EventData::Plain(_) }]));
    // A key handler answers with the guest's "consumed".
    assert!(!dispatch_key_handler(EventHandlerId(host_key), &KeyEventData::new("a", "KeyA")));
    assert!(dispatch_key_handler(EventHandlerId(host_key), &KeyEventData::new("Escape", "Escape")));
    assert_eq!(seen.borrow().len(), 3);

    // Discarding a node lets go of its subtree's ids and handlers.
    applier.apply(vec![Op::Discard { node: 2 }]).unwrap();
    assert!(applier.host_node(4).is_none());
    assert!(!has_click_handler(EventHandlerId(host_click)));
    assert_eq!(applier.proxy_count(), 0);
    assert!(doc.borrow().get_children(container).is_empty());
}

#[test]
fn a_drag_start_that_hides_the_ghost_hides_the_hosts() {
    // A sortable list's drag start calls `suppress_drag_ghost` in the guest's
    // own rinch, which nothing paints: the guest answers the flag and the
    // host's proxy hides the window's ghost.
    let (doc, dyn_doc, container) = document();
    let mut applier = Applier::new(
        dyn_doc,
        container,
        Rc::new(|event: Event| if event.handler == 7 { pimble_plugin_abi::dispatched::HIDE_DRAG_GHOST } else { 0 }),
        ApplierOptions::default(),
    );
    applier
        .apply(vec![
            element(2, "div"),
            Op::SetAttribute { node: 2, name: "draggable".into(), value: "true".into() },
            Op::SetAttribute { node: 2, name: "data-ondragstart".into(), value: "7".into() },
            Op::SetAttribute { node: 2, name: "data-ondragend".into(), value: "8".into() },
            Op::Append { parent: GUEST_BODY, child: 2 },
        ])
        .unwrap();
    let item = applier.host_node(2).unwrap();
    let handler = |attr: &str| EventHandlerId(doc.borrow().get_attribute(item, attr).unwrap().parse().unwrap());
    // `draggable` is plain data and passes through as written.
    assert_eq!(doc.borrow().get_attribute(item, "draggable").as_deref(), Some("true"));
    rinch_core::events::reset_drag_ghost_visibility();
    assert!(dispatch_event(handler("data-ondragend")));
    assert!(rinch_core::events::is_drag_ghost_visible());
    assert!(dispatch_event(handler("data-ondragstart")));
    assert!(!rinch_core::events::is_drag_ghost_visible());
    rinch_core::events::reset_drag_ghost_visibility();
}

#[test]
fn a_guest_can_name_only_its_own_nodes() {
    let (doc, dyn_doc, container) = document();
    let mut applier = Applier::new(dyn_doc, container, Rc::new(|_| 0), ApplierOptions::default());
    // The app's nodes have no guest ids: any number the guest tries is one it never
    // created, whatever the host's own ids are.
    let outside = doc.borrow().body();
    let refused = |r: Result<(), Refusal>| r.unwrap_err().0;
    assert_eq!(refused(applier.apply(vec![Op::Append { parent: 1234, child: 1235 }])), "it named node 1234, which is not one of its own");
    assert!(refused(applier.apply(vec![Op::SetAttribute { node: 77, name: "class".into(), value: "x".into() }])).contains("node 77"));
    assert!(refused(applier.apply(vec![Op::Append { parent: GUEST_ROOT, child: GUEST_BODY }])).contains("node 0"));
    // Ids are the guest's own and are made once.
    applier.apply(vec![element(2, "div")]).unwrap();
    assert!(refused(applier.apply(vec![element(2, "span")])).contains("already exists"));
    assert!(refused(applier.apply(vec![element(GUEST_BODY, "span")])).contains("already exists"));
    // Nothing that reaches past its area: no stylesheet, no script.
    assert!(refused(applier.apply(vec![element(3, "style")])).contains("<style>"));
    // A handler id that is not a number.
    assert!(refused(applier.apply(vec![Op::SetAttribute { node: 2, name: "data-rid".into(), value: "x".into() }])).contains("handler id"));
    // Its own area stays.
    assert!(refused(applier.apply(vec![Op::Discard { node: GUEST_BODY }])).contains("own area"));
    // Nothing of the app's was touched.
    assert_eq!(doc.borrow().get_children(outside), vec![container]);
}

/// An engine that answers each call from a script, flushing ops through the
/// instance's imports as a guest does.
struct Scripted {
    key: u64,
    on_mount: Vec<Op>,
}

impl GuestEngine for Scripted {
    fn call(&mut self, call: Call<'_>) -> Result<Answered, Failure> {
        let host = pimble_plugin_host::host_calls(self.key).expect("the instance is registered");
        match call {
            Call::Mount(_) => {
                host.flush(&encode(&self.on_mount)).map_err(|r| Failure::Refused(r.0))?;
                Ok(Answered::Done)
            }
            Call::Dispatch(_) => Ok(Answered::Dispatched(0)),
            _ => Ok(Answered::Done),
        }
    }
}

fn start(on_mount: Vec<Op>) -> (Rc<RefCell<MockDomDocument>>, NodeId, Instance, Rc<RefCell<Option<String>>>) {
    let (doc, dyn_doc, container) = document();
    let instance = Instance::start("Test", dyn_doc, container, InstanceOptions { applier: ApplierOptions::default() }, |key| {
        Ok(Box::new(Scripted { key, on_mount }))
    })
    .unwrap();
    let stopped = Rc::new(RefCell::new(None));
    let sink = stopped.clone();
    instance.on_stop(move |sentence| *sink.borrow_mut() = Some(sentence.to_string()));
    (doc, container, instance, stopped)
}

#[test]
fn a_refused_op_stops_the_plugin_and_takes_down_what_it_built() {
    let (doc, container, instance, stopped) = start(vec![
        element(2, "p"),
        Op::SetAttribute { node: 2, name: "data-rid".into(), value: "1".into() },
        Op::Append { parent: GUEST_BODY, child: 2 },
        Op::SetAttribute { node: 9, name: "class".into(), value: "x".into() },
    ]);
    assert!(!instance.mount("test", &Default::default()));
    assert!(instance.is_stopped());
    assert_eq!(
        stopped.borrow().as_deref(),
        Some("The Test plugin was stopped: it named node 9, which is not one of its own.")
    );
    assert!(doc.borrow().get_children(container).is_empty(), "what it built before the refusal is gone");
    assert_eq!(instance.proxy_count(), 0);
}

#[test]
fn unmounting_clears_the_container_without_a_notice() {
    let (doc, container, instance, stopped) =
        start(vec![element(2, "p"), Op::SetAttribute { node: 2, name: "data-rid".into(), value: "1".into() }, Op::Append { parent: GUEST_BODY, child: 2 }]);
    assert!(instance.mount("test", &Default::default()));
    assert_eq!(doc.borrow().get_children(container).len(), 1);
    assert_eq!(instance.proxy_count(), 1);
    instance.unmount();
    assert!(doc.borrow().get_children(container).is_empty());
    assert_eq!(instance.proxy_count(), 0);
    assert!(stopped.borrow().is_none());
}
