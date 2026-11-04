//! The same component rendered straight into rinch's `MockDomDocument`, and through a
//! plugin's `RemoteDocument` whose op log is applied to a second `MockDomDocument`,
//! makes the same tree; and a click on each changes them the same way
//! (docs/PLUGINS_CONTRACT.md section 5).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use pimble_plugin_sdk::abi::{
    Answer, Click, Event, EventData, HostRequest, HostState, Key, Modifiers, MountArgs, NodeAddr, NodePeople, Op, Props,
    PresentPerson, Query, GUEST_BODY, GUEST_ROOT,
};
use pimble_plugin_sdk::prelude::*;
use pimble_plugin_sdk::pimble_ui::presence::PersonChip;
use pimble_plugin_sdk::{Guest, HostLink};
use rinch_core::dom::mock::MockDomDocument;
use rinch_core::dom::{DomDocument, NodeId};

/// Applies a guest's ops to a mock document, as the host applies them to its own.
struct ApplyToMock {
    doc: Rc<RefCell<MockDomDocument>>,
    map: RefCell<HashMap<u64, NodeId>>,
    queries: RefCell<Vec<Query>>,
    requests: RefCell<Vec<HostRequest>>,
}

impl ApplyToMock {
    /// One not shared, for a view rendered straight into a mock.
    fn new_link() -> Self {
        Rc::try_unwrap(Self::new()).ok().expect("not shared yet")
    }

    fn new() -> Rc<Self> {
        let doc = Rc::new(RefCell::new(MockDomDocument::new()));
        let mut map = HashMap::new();
        map.insert(GUEST_ROOT, doc.borrow().root());
        map.insert(GUEST_BODY, doc.borrow().body());
        Rc::new(ApplyToMock { doc, map: RefCell::new(map), queries: RefCell::new(Vec::new()), requests: RefCell::new(Vec::new()) })
    }

    fn id(&self, guest: u64) -> NodeId {
        *self.map.borrow().get(&guest).unwrap_or_else(|| panic!("guest node {guest} was never created"))
    }
}

impl HostLink for ApplyToMock {
    fn request(&self, request: &HostRequest) {
        self.requests.borrow_mut().push(request.clone());
    }

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
            h2 { {move || heading(&props.get())} }
            ul {
                for label in rows(&props.get()) {
                    li { key: label.clone(), {label.clone()} }
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

/// The test's props: a heading and rows, carried in the viewer's settings as
/// `heading|row,row` (a plugin reads its nodes through the API; these tests are
/// about the document, so the props carry something to draw).
fn props(title: &str, labels: &[&str]) -> Props {
    Props { metanode: NodeAddr { store: "s".into(), node: "m".into() }, settings: Some(format!("{title}|{}", labels.join(","))), remembered: None }
}

fn heading(props: &Props) -> String {
    props.settings.as_deref().unwrap_or_default().split('|').next().unwrap_or_default().to_string()
}

fn rows(props: &Props) -> Vec<String> {
    let settings = props.settings.clone().unwrap_or_default();
    let list = settings.split('|').nth(1).unwrap_or_default();
    list.split(',').filter(|l| !l.is_empty()).map(str::to_string).collect()
}

/// An API that is never answered: these tests make no calls.
struct NoHost;

impl pimble_plugin_sdk::ApiLink for NoHost {
    fn call(&self, _id: u64, _call: Vec<u8>) {}
    fn cancel(&self, _id: u64) {}
}

fn api() -> Arc<Pimble> {
    Arc::new(Pimble::new(pimble_plugin_sdk::GuestBackend::new(pimble_plugin_sdk::Bridge::new(NoHost)), "plugin:test"))
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
        let host = Host::new(Rc::new(ApplyToMock::new_link()), Signal::new(HostState::default()));
        counter(s, Mount { viewer: "test".into(), props: p, api: api(), host })
    });
    NodeHandle::new(body, Rc::downgrade(&direct_dyn)).append_child(&node);

    // Through a plugin's document, applied to another mock.
    let host = ApplyToMock::new();
    let mut guest = Guest::new(host.clone(), api());
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
    let key = |k: &str| Key { key: k.into(), code: k.into(), modifiers: Modifiers::default(), up: false, target: None };
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
    let guest = Guest::new(host.clone(), api());
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

/// A view drawing who is in the metanode with pimble-ui's chips, as a plugin
/// draws presence; a click on one asks the host to follow them.
fn present(__scope: &mut RenderScope, mount: Mount) -> NodeHandle {
    let host = mount.host.clone();
    let metanode = mount.metanode().expect("a metanode");
    let following = mount.host.clone();
    let focus = mount.host.clone();
    rsx! {
        div {
            span {
                class: "people",
                for person in host.people_in(metanode) {
                    PersonChip {
                        key: person.key.clone(),
                        person: person.clone(),
                        onfollow: {
                            let following = following.clone();
                            move |key: String| following.follow(&key)
                        },
                    }
                }
            }
            button { class: "focus", onclick: move || focus.focus(Some(metanode)), "Focus" }
        }
    }
}

#[test]
fn a_plugin_draws_presence_with_the_apps_chips_and_asks_the_host_to_follow() {
    let host = ApplyToMock::new();
    let mut guest = Guest::new(host.clone(), api());
    let metanode = NodeAddr { store: pimble_plugin_sdk::pimble_core::StoreId::new().to_string(), node: pimble_plugin_sdk::pimble_core::NodeId::new().to_string() };
    guest.mount(present, MountArgs { viewer: "test".into(), props: Props { metanode: metanode.clone(), settings: None, remembered: None } });
    let render = || {
        let mut out = String::new();
        dump(&*host.doc.borrow(), host.doc.borrow().body(), &names(), 0, &mut out);
        out
    };
    assert!(!render().contains("pimble-pane__person"));

    // The host says who is there: an agent and a person, drawn as the pane's
    // strip draws them.
    let sam = PresentPerson { key: "sub-sam".into(), name: "Sam".into(), initials: "S".into(), color: "#aa3300".into(), title: "Sam, on laptop".into(), agent: false };
    let claude = PresentPerson { key: "sub-sam/agent/Claude".into(), name: "Claude".into(), initials: "C".into(), color: "#0033aa".into(), title: "Claude, an AI agent".into(), agent: true };
    guest.host_state(HostState { dark: false, people: vec![NodePeople { node: metanode.clone(), people: vec![claude, sam] }], following: None });
    let doc = host.doc.borrow();
    let chips: Vec<NodeId> = all_with_class(&*doc, doc.body(), "pimble-pane__person");
    assert_eq!(chips.len(), 2, "{}", { drop(doc); render() });
    let classes: Vec<String> = chips.iter().map(|c| doc.get_attribute(*c, "class").unwrap()).collect();
    assert!(classes[0].contains("pimble-pane__person--agent") && !classes[1].contains("--agent"), "{classes:?}");
    assert_eq!(doc.get_attribute(chips[1], "title").as_deref(), Some("Sam, on laptop (click to follow)"));
    assert_eq!(doc.get_attribute(chips[1], "style").as_deref(), Some("background: #aa3300;"));
    drop(doc);

    // A click on Sam's chip asks to follow Sam; on the agent's, nothing.
    let remote = guest.document();
    let guest_chips = all_with_class(&*remote.borrow(), NodeId(GUEST_BODY), "pimble-pane__person");
    for chip in guest_chips {
        let handler: u64 = remote.borrow().get_attribute(chip, "data-rid").unwrap().parse().unwrap();
        guest.dispatch(Event { handler, data: EventData::Plain(Click::default()) });
    }
    let focus = all_with_class(&*remote.borrow(), NodeId(GUEST_BODY), "focus")[0];
    let handler: u64 = remote.borrow().get_attribute(focus, "data-rid").unwrap().parse().unwrap();
    guest.dispatch(Event { handler, data: EventData::Plain(Click::default()) });
    assert_eq!(
        host.requests.borrow().as_slice(),
        &[HostRequest::Follow { person: "sub-sam".into() }, HostRequest::Focus { node: Some(metanode.clone()) }]
    );

    // They leave: the chips go.
    guest.host_state(HostState::default());
    let doc = host.doc.borrow();
    assert!(all_with_class(&*doc, doc.body(), "pimble-pane__person").is_empty());
}

fn all_with_class(doc: &dyn DomDocument, node: NodeId, class: &str) -> Vec<NodeId> {
    let mut out = Vec::new();
    if doc.get_attribute(node, "class").is_some_and(|c| c.split_whitespace().any(|w| w == class)) {
        out.push(node);
    }
    for child in doc.get_children(node) {
        out.extend(all_with_class(doc, child, class));
    }
    out
}
