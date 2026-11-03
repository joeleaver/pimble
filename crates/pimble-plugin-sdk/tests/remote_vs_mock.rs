//! The same component rendered straight into rinch's `MockDomDocument`, and through a
//! plugin's `RemoteDocument` whose op log is applied to a second `MockDomDocument`,
//! makes the same tree; and a click on each changes them the same way
//! (docs/PLUGINS_CONTRACT.md section 5).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use pimble_plugin_sdk::abi::{
    Answer, Click, Event, EventData, ForestRow, Key, Modifiers, MountArgs, NodeAddr, Op, Props, Query, GUEST_BODY,
    GUEST_ROOT,
};
use pimble_plugin_sdk::prelude::*;
use pimble_plugin_sdk::{Guest, HostLink};
use rinch_core::dom::mock::MockDomDocument;
use rinch_core::dom::{DomDocument, NodeId};

/// Applies a guest's ops to a mock document, as the host applies them to its own.
struct ApplyToMock {
    doc: Rc<RefCell<MockDomDocument>>,
    map: RefCell<HashMap<u64, NodeId>>,
    queries: RefCell<Vec<Query>>,
}

impl ApplyToMock {
    fn new() -> Rc<Self> {
        let doc = Rc::new(RefCell::new(MockDomDocument::new()));
        let mut map = HashMap::new();
        map.insert(GUEST_ROOT, doc.borrow().root());
        map.insert(GUEST_BODY, doc.borrow().body());
        Rc::new(ApplyToMock { doc, map: RefCell::new(map), queries: RefCell::new(Vec::new()) })
    }

    fn id(&self, guest: u64) -> NodeId {
        *self.map.borrow().get(&guest).unwrap_or_else(|| panic!("guest node {guest} was never created"))
    }
}

impl HostLink for ApplyToMock {
    fn flush(&self, ops: Vec<Op>) {
        let mut doc = self.doc.borrow_mut();
        for op in ops {
            match op {
                Op::CreateElement { id, tag } => {
                    let n = doc.create_element(&tag);
                    self.map.borrow_mut().insert(id, n);
                }
                Op::CreateText { id, text } => {
                    let n = doc.create_text(&text);
                    self.map.borrow_mut().insert(id, n);
                }
                Op::CreateComment { id, text } => {
                    let n = doc.create_comment(&text);
                    self.map.borrow_mut().insert(id, n);
                }
                Op::Append { parent, child } => doc.append_child(self.id(parent), self.id(child)),
                Op::InsertBefore { parent, child, reference } => {
                    doc.insert_before(self.id(parent), self.id(child), self.id(reference))
                }
                Op::InsertAt { parent, child, index } => doc.insert_child(self.id(parent), self.id(child), index as usize),
                Op::RemoveChild { parent, child } => doc.remove_child(self.id(parent), self.id(child)),
                Op::Replace { old, new } => doc.replace_node(self.id(old), self.id(new)),
                Op::Remove { node } => doc.remove_node(self.id(node)),
                Op::Discard { node } => doc.discard_node(self.id(node)),
                Op::SetText { node, text } => doc.set_text_content(self.id(node), &text),
                Op::SetAttribute { node, name, value } => doc.set_attribute(self.id(node), &name, &value),
                Op::RemoveAttribute { node, name } => doc.remove_attribute(self.id(node), &name),
                Op::SetStyle { node, property, value } => doc.set_style(self.id(node), &property, &value),
                Op::Focus { node } => doc.focus_element(self.id(node)),
                Op::SetScrollTop { .. }
                | Op::SetScrollLeft { .. }
                | Op::SetSelection { .. }
                | Op::ScrollIntoView { .. } => {}
            }
        }
    }

    fn query(&self, query: &Query) -> Answer {
        self.queries.borrow_mut().push(query.clone());
        match query {
            Query::LiveValue { node } => {
                self.doc.borrow().live_value(self.id(*node)).map(Answer::Text).unwrap_or(Answer::Nothing)
            }
            _ => Answer::Nothing,
        }
    }
}

/// The component under test: a heading, a reactive list, a branch, a component
/// (rinch's `Button`) whose click counts, and a field.
fn counter(__scope: &mut RenderScope, mount: Mount) -> NodeHandle {
    let props = mount.props;
    let clicks = Signal::new(0u32);
    let typed = Signal::new(String::new());
    rsx! {
        div {
            class: "counter",
            style: "padding: 4px;",
            h2 { {move || props.get().title} }
            ul {
                for row in props.get().forest {
                    li { key: row.addr.node.clone(), {row.label.clone()} }
                }
            }
            if clicks.get() > 1 {
                p { class: "many", "More than one click" }
            }
            Button {
                onclick: move || clicks.update(|n| *n += 1),
                {move || format!("Clicked {}", clicks.get())}
            }
            input {
                class: "field",
                value: {move || typed.get()},
                oninput: move |v: String| typed.set(v),
                onkeydown: move |k: &KeyEventData| k.key == "Escape",
            }
            p { class: "echo", {move || format!("typed {}", typed.get())} }
        }
    }
}

fn props(title: &str, labels: &[&str]) -> Props {
    Props {
        metanode: NodeAddr { store: "s".into(), node: "m".into() },
        title: title.into(),
        forest: labels
            .iter()
            .map(|label| ForestRow {
                addr: NodeAddr { store: "s".into(), node: label.to_string() },
                parent: None,
                depth: 1,
                label: label.to_string(),
                node_type: "document".into(),
            })
            .collect(),
        settings: None,
    }
}

/// A handler attribute's value is an id that differs between two renders.
fn is_handler(name: &str) -> bool {
    pimble_plugin_sdk::abi::handler_kind(name).is_some()
}

/// The tree under `node` as text, reading `doc` only through `DomDocument`: tags,
/// text, and the attributes `names` lists (a `DomDocument` cannot enumerate them).
fn dump(doc: &dyn DomDocument, node: NodeId, names: &[String], depth: usize, out: &mut String) {
    let pad = "  ".repeat(depth);
    match doc.node_type(node) {
        Some(1) => {
            let tag = doc.tag_name(node).unwrap_or_default();
            let attrs: String = names
                .iter()
                .filter_map(|name| {
                    doc.get_attribute(node, name).map(|v| {
                        let v = if is_handler(name) { "<handler>".to_string() } else { v };
                        format!(" {name}=\"{v}\"")
                    })
                })
                .collect();
            out.push_str(&format!("{pad}<{tag}{attrs}>\n"));
            for child in doc.get_children(node) {
                dump(doc, child, names, depth + 1, out);
            }
        }
        Some(3) => out.push_str(&format!("{pad}\"{}\"\n", doc.text_content(node).unwrap_or_default())),
        _ => {}
    }
}

const NAMES: &[&str] = &[
    "class", "style", "type", "value", "disabled", "data-rid", "data-oninput", "data-onkeydown", "role", "aria-label",
    "tabindex", "data-size", "data-variant",
];

fn names() -> Vec<String> {
    NAMES.iter().map(|s| s.to_string()).collect()
}

fn first_with(doc: &dyn DomDocument, node: NodeId, attr: &str) -> Option<NodeId> {
    if doc.get_attribute(node, attr).is_some() {
        return Some(node);
    }
    doc.get_children(node).into_iter().find_map(|c| first_with(doc, c, attr))
}

#[test]
fn a_component_renders_the_same_tree_through_a_remote_document() {
    // Straight into a mock.
    let direct = Rc::new(RefCell::new(MockDomDocument::new()));
    let body = direct.borrow().body();
    let direct_dyn: Rc<RefCell<dyn DomDocument>> = direct.clone();
    let mut scope = RenderScope::new(direct_dyn.clone(), body);
    let node = scope.build(|s| {
        let p = Signal::new(props("Work", &["Plan", "Do"]));
        counter(s, Mount { viewer: "test".into(), props: p })
    });
    NodeHandle::new(body, Rc::downgrade(&direct_dyn)).append_child(&node);

    // Through a plugin's document, applied to another mock.
    let host = ApplyToMock::new();
    let mut guest = Guest::new(host.clone());
    guest.mount(counter, MountArgs { viewer: "test".into(), props: props("Work", &["Plan", "Do"]) });
    let remote = guest.document();
    assert_eq!(remote.borrow().ops_pending(), 0, "a turn ends with the log flushed");

    let render = |doc: &dyn DomDocument, body: NodeId| {
        let mut out = String::new();
        dump(doc, body, &names(), 0, &mut out);
        out
    };
    let want = render(&*direct.borrow(), body);
    let shadow = render(&*remote.borrow(), NodeId(GUEST_BODY));
    let applied = render(&*host.doc.borrow(), host.doc.borrow().body());
    assert!(want.contains("Clicked 0") && want.contains("Plan"), "{want}");
    assert_eq!(shadow, want, "the shadow tree is the tree rinch built");
    assert_eq!(applied, want, "the host builds the same tree from the op log");

    // A click on each: the direct one through rinch's dispatch, the plugin's as the
    // host would send it, by the guest handler id in the shadow's attribute.
    let button = first_with(&*direct.borrow(), body, "data-rid").expect("a clickable button");
    let id: usize = direct.borrow().get_attribute(button, "data-rid").unwrap().parse().unwrap();
    rinch_core::events::dispatch_event(rinch_core::events::EventHandlerId(id));
    rinch_core::events::dispatch_event(rinch_core::events::EventHandlerId(id));
    let guest_button = first_with(&*remote.borrow(), NodeId(GUEST_BODY), "data-rid").expect("the same button");
    let handler: u64 = remote.borrow().get_attribute(guest_button, "data-rid").unwrap().parse().unwrap();
    for _ in 0..2 {
        let consumed = guest.dispatch(Event { handler, data: EventData::Plain(Click::default()) });
        assert!(!consumed);
    }
    let want = render(&*direct.borrow(), body);
    assert!(want.contains("Clicked 2") && want.contains("More than one click"), "{want}");
    assert_eq!(render(&*remote.borrow(), NodeId(GUEST_BODY)), want);
    assert_eq!(render(&*host.doc.borrow(), host.doc.borrow().body()), want);

    // New props reach the view as a signal write.
    guest.props_changed(props("Home", &["Garden"]));
    let shown = render(&*host.doc.borrow(), host.doc.borrow().body());
    assert!(shown.contains("Home") && shown.contains("Garden") && !shown.contains("Plan"), "{shown}");
    assert_eq!(render(&*remote.borrow(), NodeId(GUEST_BODY)), shown);

    // Typing: the input event carries the text and the field's id; a key handler
    // answers whether it consumed the key.
    let field = first_with(&*remote.borrow(), NodeId(GUEST_BODY), "data-oninput").expect("the field");
    let input: u64 = remote.borrow().get_attribute(field, "data-oninput").unwrap().parse().unwrap();
    guest.dispatch(Event { handler: input, data: EventData::Input { value: "hi".into(), node: Some(field.0) } });
    let keys: u64 = remote.borrow().get_attribute(field, "data-onkeydown").unwrap().parse().unwrap();
    let key = |k: &str| Key { key: k.into(), code: k.into(), modifiers: Modifiers::default(), up: false };
    assert!(!guest.dispatch(Event { handler: keys, data: EventData::Key(key("a")) }));
    assert!(guest.dispatch(Event { handler: keys, data: EventData::Key(key("Escape")) }));
    let shown = render(&*host.doc.borrow(), host.doc.borrow().body());
    assert!(shown.contains("typed hi"), "{shown}");

    // Unmounting takes everything the view built out of the host's body.
    guest.unmount();
    assert!(host.doc.borrow().get_children(host.doc.borrow().body()).is_empty());
    assert!(remote.borrow().get_children(NodeId(GUEST_BODY)).is_empty());
}

#[test]
fn a_read_only_the_host_answers_sends_the_pending_writes_first() {
    let host = ApplyToMock::new();
    let guest = Guest::new(host.clone());
    let doc = guest.document();
    let field = doc.borrow_mut().create_element("input");
    doc.borrow_mut().set_attribute(field, "value", "draft");
    doc.borrow_mut().append_child(NodeId(GUEST_BODY), field);
    assert_eq!(doc.borrow().ops_pending(), 3);
    // The host's mock holds the field only once the log is applied, so the answer
    // proves the order.
    assert_eq!(doc.borrow().live_value(field).as_deref(), Some("draft"));
    assert_eq!(doc.borrow().ops_pending(), 0);
    assert_eq!(host.queries.borrow().as_slice(), &[Query::LiveValue { node: field.0 }]);
    // Markup from a string is refused.
    assert!(doc.borrow_mut().parse_html("<b>x</b>").is_none());
    // Structural reads stay in the guest.
    assert_eq!(doc.borrow().parent_node(field), Some(NodeId(GUEST_BODY)));
    assert_eq!(doc.borrow().query_selector("body > input").map(|n| n.0), Some(field.0));
    assert_eq!(host.queries.borrow().len(), 1);
}
