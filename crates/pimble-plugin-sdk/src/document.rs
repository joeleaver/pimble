//! `RemoteDocument`: the document a plugin renders into (docs/PLUGINS_CONTRACT.md
//! section 3.1).
//!
//! rinch renders into any `dyn DomDocument`. A plugin's document is not the window's:
//! it keeps a shadow of the tree the plugin built (tags, attributes, children, text),
//! so every structural read rinch makes (children, parents, attributes, text) is
//! answered here without leaving the guest, and it records every write as an [`Op`]
//! in an ordered log. The log goes to the host at the end of every guest turn, and
//! before any read only the host can answer (a live value, a layout box, the focus,
//! a scroll offset), which the host answers from its own document once it holds
//! everything the plugin wrote.
//!
//! Node ids are the guest's own. The host maps each to a node of its own under the
//! plugin's area and refuses any id the plugin did not create, so a plugin can name
//! nothing outside what it built.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use pimble_plugin_abi::{Answer, Op, Query, FIRST_GUEST_NODE, GUEST_BODY, GUEST_ROOT};
use rinch::core::dom::{next_doc_key, NodeId, SelectionDirection};

mod dom;

/// Where a document's op log goes, and who answers the reads it cannot.
///
/// In a plugin it is the host, through the wasm imports. A test can stand anything
/// there: a recorder, or an applier onto another document.
pub trait HostLink {
    /// Apply these writes now, in order.
    fn flush(&self, ops: Vec<Op>);
    /// Answer a read only the host can.
    fn query(&self, query: &Query) -> Answer;
    /// Ask the app to do something for the person (open a node in the pane,
    /// say where they are, follow someone). A host that cannot drops it.
    fn request(&self, _request: &pimble_plugin_abi::HostRequest) {}
}

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Element(String),
    Text,
    Comment,
}

#[derive(Debug, Clone)]
struct Shadow {
    kind: Kind,
    /// A text or comment node's text; on an element, text written over it with
    /// `set_text_content`, which replaced its children.
    text: String,
    /// In the order they were first set, so a dump is stable.
    attributes: Vec<(String, String)>,
    children: Vec<u64>,
    parent: Option<u64>,
}

impl Shadow {
    fn new(kind: Kind, text: &str) -> Self {
        Shadow { kind, text: text.to_string(), attributes: Vec::new(), children: Vec::new(), parent: None }
    }

    fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    fn set(&mut self, name: &str, value: &str) {
        match self.attributes.iter_mut().find(|(k, _)| k == name) {
            Some(slot) => slot.1 = value.to_string(),
            None => self.attributes.push((name.to_string(), value.to_string())),
        }
    }
}

/// A plugin's document: a shadow tree answering structural reads and an op log of
/// writes for the host.
pub struct RemoteDocument {
    doc_key: u64,
    next_id: u64,
    nodes: HashMap<u64, Shadow>,
    /// Writes not yet sent. A `RefCell` because reads that only the host answers
    /// take `&self` and must send what is pending first.
    log: RefCell<Vec<Op>>,
    link: Rc<dyn HostLink>,
    /// How many ops have gone to the host, for a test to see that a read flushed.
    sent: Cell<usize>,
}

impl RemoteDocument {
    /// A document with an `<html>` root ([`GUEST_ROOT`]) and a `<body>`
    /// ([`GUEST_BODY`]), the plugin's area of the window.
    pub fn new(link: Rc<dyn HostLink>) -> Self {
        let mut nodes = HashMap::new();
        let mut root = Shadow::new(Kind::Element("html".into()), "");
        root.children.push(GUEST_BODY);
        let mut body = Shadow::new(Kind::Element("body".into()), "");
        body.parent = Some(GUEST_ROOT);
        nodes.insert(GUEST_ROOT, root);
        nodes.insert(GUEST_BODY, body);
        RemoteDocument {
            doc_key: next_doc_key(),
            next_id: FIRST_GUEST_NODE,
            nodes,
            log: RefCell::new(Vec::new()),
            link,
            sent: Cell::new(0),
        }
    }

    /// Send every pending write to the host. Called at the end of every guest turn
    /// and before every read only the host answers.
    pub fn flush(&self) {
        let ops = std::mem::take(&mut *self.log.borrow_mut());
        if !ops.is_empty() {
            self.sent.set(self.sent.get() + ops.len());
            self.link.flush(ops);
        }
    }

    /// Ops sent to the host so far.
    pub fn ops_sent(&self) -> usize {
        self.sent.get()
    }

    /// Ops written and not sent yet.
    pub fn ops_pending(&self) -> usize {
        self.log.borrow().len()
    }

    /// What the host said a form control holds, kept in the shadow with no op: the
    /// host's document already holds it (an input event carries the typed text).
    pub fn note_value(&mut self, node: u64, value: &str) {
        if let Some(n) = self.nodes.get_mut(&node) {
            n.set("value", value);
        }
    }

    /// The shadow tree under `node` as indented text: tags with their attributes
    /// sorted by name, and text. `normalize` rewrites an attribute's value (a test
    /// blanks handler ids, which differ between two renders).
    pub fn dump(&self, node: u64, normalize: &dyn Fn(&str, &str) -> String) -> String {
        let mut out = String::new();
        self.dump_into(node, 0, normalize, &mut out);
        out
    }

    fn dump_into(&self, node: u64, depth: usize, normalize: &dyn Fn(&str, &str) -> String, out: &mut String) {
        let Some(n) = self.nodes.get(&node) else { return };
        let pad = "  ".repeat(depth);
        match &n.kind {
            Kind::Element(tag) => {
                let mut attrs: Vec<(String, String)> =
                    n.attributes.iter().map(|(k, v)| (k.clone(), normalize(k, v))).collect();
                attrs.sort();
                let attrs: String = attrs.iter().map(|(k, v)| format!(" {k}=\"{v}\"")).collect();
                out.push_str(&format!("{pad}<{tag}{attrs}>\n"));
                if !n.text.is_empty() {
                    out.push_str(&format!("{pad}  \"{}\"\n", n.text));
                }
                for &child in &n.children {
                    self.dump_into(child, depth + 1, normalize, out);
                }
            }
            Kind::Text => out.push_str(&format!("{pad}\"{}\"\n", n.text)),
            Kind::Comment => {}
        }
    }

    fn is_live(&self, id: u64) -> bool {
        self.nodes.contains_key(&id)
    }

    fn push(&self, op: Op) {
        self.log.borrow_mut().push(op);
    }

    fn mint(&mut self, kind: Kind, text: &str) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.nodes.insert(id, Shadow::new(kind, text));
        id
    }

    /// Unlist `child` from whatever parent lists it, as a move does first.
    fn detach(&mut self, child: u64) {
        let old = self.nodes.get(&child).and_then(|n| n.parent);
        if let Some(parent) = old {
            if let Some(p) = self.nodes.get_mut(&parent) {
                p.children.retain(|&c| c != child);
            }
        }
        if let Some(n) = self.nodes.get_mut(&child) {
            n.parent = None;
        }
    }

    fn forget_subtree(&mut self, node: u64) {
        let children = self.nodes.get(&node).map(|n| n.children.clone()).unwrap_or_default();
        for child in children {
            self.forget_subtree(child);
        }
        self.nodes.remove(&node);
    }

    fn text_of(&self, node: u64) -> Option<String> {
        let n = self.nodes.get(&node)?;
        Some(match n.kind {
            Kind::Text | Kind::Comment => n.text.clone(),
            Kind::Element(_) => {
                let mut out = n.text.clone();
                for &child in &n.children {
                    if let Some(t) = self.text_of(child) {
                        out.push_str(&t);
                    }
                }
                out
            }
        })
    }

    /// Ask the host, with everything written so far applied there first.
    fn ask(&self, query: Query) -> Answer {
        self.flush();
        self.link.query(&query)
    }

    fn matches(&self, node: u64, selector: &crate::selector::Selector) -> bool {
        selector.matches(node, &|id| self.element_view(id))
    }

    fn element_view(&self, id: u64) -> Option<crate::selector::ElementView<'_>> {
        let n = self.nodes.get(&id)?;
        match &n.kind {
            Kind::Element(tag) => Some(crate::selector::ElementView {
                tag,
                attributes: &n.attributes,
                parent: n.parent,
            }),
            _ => None,
        }
    }

    fn elements_under(&self, node: u64, out: &mut Vec<u64>) {
        if let Some(n) = self.nodes.get(&node) {
            for &child in &n.children {
                if matches!(self.nodes.get(&child).map(|c| &c.kind), Some(Kind::Element(_))) {
                    out.push(child);
                }
                self.elements_under(child, out);
            }
        }
    }

    fn select_under(&self, scope: u64, selector: &str) -> Vec<NodeId> {
        let Some(selector) = crate::selector::Selector::parse(selector) else { return Vec::new() };
        let mut all = Vec::new();
        self.elements_under(scope, &mut all);
        all.into_iter().filter(|&id| self.matches(id, &selector)).map(NodeId).collect()
    }
}

fn direction_code(direction: SelectionDirection) -> u8 {
    match direction {
        SelectionDirection::Forward => 0,
        SelectionDirection::Backward => 1,
        SelectionDirection::None => 2,
    }
}
