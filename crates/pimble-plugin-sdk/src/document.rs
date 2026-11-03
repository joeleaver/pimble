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
use rinch::core::dom::{
    next_doc_key, normalize_property_name, serialize_declarations, split_declarations, DomDocument, GlyphBounds, NodeId,
    SelectionDirection,
};
use rinch::core::reactive::ElementBounds;

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

impl DomDocument for RemoteDocument {
    fn doc_key(&self) -> u64 {
        self.doc_key
    }

    fn create_element(&mut self, tag: &str) -> NodeId {
        let id = self.mint(Kind::Element(tag.to_string()), "");
        self.push(Op::CreateElement { id, tag: tag.to_string() });
        NodeId(id)
    }

    fn create_text(&mut self, text: &str) -> NodeId {
        let id = self.mint(Kind::Text, text);
        self.push(Op::CreateText { id, text: text.to_string() });
        NodeId(id)
    }

    fn create_comment(&mut self, text: &str) -> NodeId {
        let id = self.mint(Kind::Comment, text);
        self.push(Op::CreateComment { id, text: text.to_string() });
        NodeId(id)
    }

    fn append_child(&mut self, parent: NodeId, child: NodeId) {
        let (parent, child) = (parent.0, child.0);
        if !self.is_live(parent) || !self.is_live(child) {
            return;
        }
        self.detach(child);
        self.nodes.get_mut(&parent).expect("live").children.push(child);
        self.nodes.get_mut(&child).expect("live").parent = Some(parent);
        self.push(Op::Append { parent, child });
    }

    fn remove_child(&mut self, parent: NodeId, child: NodeId) {
        let (parent, child) = (parent.0, child.0);
        let listed = self.nodes.get(&parent).is_some_and(|p| p.children.contains(&child));
        if !listed {
            return;
        }
        self.detach(child);
        self.push(Op::RemoveChild { parent, child });
    }

    fn insert_before(&mut self, parent: NodeId, child: NodeId, reference: NodeId) {
        let (parent, child, reference) = (parent.0, child.0, reference.0);
        if !self.is_live(parent) || !self.is_live(child) {
            return;
        }
        self.detach(child);
        let Some(pos) = self.nodes.get(&parent).and_then(|p| p.children.iter().position(|&c| c == reference)) else {
            // A reference the parent does not list places nothing, as on the mock
            // and in a browser (which throws); the child stays detached.
            self.push(Op::Remove { node: child });
            return;
        };
        self.nodes.get_mut(&parent).expect("live").children.insert(pos, child);
        self.nodes.get_mut(&child).expect("live").parent = Some(parent);
        self.push(Op::InsertBefore { parent, child, reference });
    }

    fn insert_child(&mut self, parent: NodeId, child: NodeId, index: usize) {
        let (parent, child) = (parent.0, child.0);
        if !self.is_live(parent) || !self.is_live(child) {
            return;
        }
        self.detach(child);
        let p = self.nodes.get_mut(&parent).expect("live");
        let index = index.min(p.children.len());
        p.children.insert(index, child);
        let next = p.children.get(index + 1).copied();
        self.nodes.get_mut(&child).expect("live").parent = Some(parent);
        // Sent as a place next to a sibling rather than an index: the host's list
        // can hold a node the guest never named (the text node an element's
        // `set_text_content` makes on the desktop), and an index would count it.
        match next {
            Some(reference) => self.push(Op::InsertBefore { parent, child, reference }),
            None => self.push(Op::Append { parent, child }),
        }
    }

    fn replace_node(&mut self, old: NodeId, new: NodeId) {
        let (old, new) = (old.0, new.0);
        if old == new || !self.is_live(old) || !self.is_live(new) {
            return;
        }
        let Some(parent) = self.nodes.get(&old).and_then(|n| n.parent) else { return };
        self.detach(new);
        if let Some(p) = self.nodes.get_mut(&parent) {
            if let Some(pos) = p.children.iter().position(|&c| c == old) {
                p.children[pos] = new;
            }
        }
        self.nodes.get_mut(&new).expect("live").parent = Some(parent);
        self.nodes.get_mut(&old).expect("live").parent = None;
        self.push(Op::Replace { old, new });
    }

    fn remove_node(&mut self, node: NodeId) {
        if !self.is_live(node.0) {
            return;
        }
        self.detach(node.0);
        self.push(Op::Remove { node: node.0 });
    }

    fn discard_node(&mut self, node: NodeId) {
        if !self.is_live(node.0) || node.0 == GUEST_ROOT || node.0 == GUEST_BODY {
            return;
        }
        self.detach(node.0);
        self.forget_subtree(node.0);
        self.push(Op::Discard { node: node.0 });
    }

    fn is_retired(&self, node: NodeId) -> bool {
        !self.nodes.contains_key(&node.0)
    }

    fn set_text_content(&mut self, node: NodeId, text: &str) {
        let Some(n) = self.nodes.get_mut(&node.0) else { return };
        n.text = text.to_string();
        let orphans = match n.kind {
            Kind::Element(_) => std::mem::take(&mut n.children),
            Kind::Text | Kind::Comment => Vec::new(),
        };
        for orphan in orphans {
            if let Some(o) = self.nodes.get_mut(&orphan) {
                o.parent = None;
            }
        }
        self.push(Op::SetText { node: node.0, text: text.to_string() });
    }

    fn set_attribute(&mut self, node: NodeId, name: &str, value: &str) {
        let Some(n) = self.nodes.get_mut(&node.0) else { return };
        n.set(name, value);
        self.push(Op::SetAttribute { node: node.0, name: name.to_string(), value: value.to_string() });
    }

    fn remove_attribute(&mut self, node: NodeId, name: &str) {
        let Some(n) = self.nodes.get_mut(&node.0) else { return };
        n.attributes.retain(|(k, _)| k != name);
        self.push(Op::RemoveAttribute { node: node.0, name: name.to_string() });
    }

    fn get_attribute(&self, node: NodeId, name: &str) -> Option<String> {
        self.nodes.get(&node.0)?.attribute(name).map(str::to_string)
    }

    fn live_value(&self, node: NodeId) -> Option<String> {
        match self.ask(Query::LiveValue { node: node.0 }) {
            Answer::Text(text) => Some(text),
            _ => None,
        }
    }

    fn set_style(&mut self, node: NodeId, property: &str, value: &str) {
        let Some(n) = self.nodes.get_mut(&node.0) else { return };
        // The shadow's `style` attribute follows the write the way CSSOM's
        // `setProperty` does, so a read of it agrees with the host's.
        let mut decls = split_declarations(n.attribute("style").unwrap_or(""));
        let name = normalize_property_name(property);
        match decls.iter_mut().find(|(k, _)| k.as_str() == &*name) {
            Some(slot) => slot.1 = value.to_string(),
            None => decls.push((name.into_owned(), value.to_string())),
        }
        let style = serialize_declarations(&decls);
        n.set("style", &style);
        self.push(Op::SetStyle { node: node.0, property: property.to_string(), value: value.to_string() });
    }

    fn mark_dirty(&mut self, _node: NodeId) {}

    fn take_dirty_nodes(&mut self) -> Vec<NodeId> {
        Vec::new()
    }

    fn root(&self) -> NodeId {
        NodeId(GUEST_ROOT)
    }

    fn body(&self) -> NodeId {
        NodeId(GUEST_BODY)
    }

    fn query_selector(&self, selector: &str) -> Option<NodeId> {
        self.query_selector_all(selector).into_iter().next()
    }

    fn query_selector_all(&self, selector: &str) -> Vec<NodeId> {
        let mut out = Vec::new();
        if let Some(parsed) = crate::selector::Selector::parse(selector) {
            if self.matches(GUEST_BODY, &parsed) {
                out.push(NodeId(GUEST_BODY));
            }
        }
        out.extend(self.select_under(GUEST_BODY, selector));
        out
    }

    fn query_selector_all_in(&self, scope: NodeId, selector: &str) -> Vec<NodeId> {
        self.select_under(scope.0, selector)
    }

    fn get_children(&self, node: NodeId) -> Vec<NodeId> {
        self.nodes.get(&node.0).map(|n| n.children.iter().copied().map(NodeId).collect()).unwrap_or_default()
    }

    fn is_text_node(&self, node: NodeId) -> bool {
        matches!(self.nodes.get(&node.0).map(|n| &n.kind), Some(Kind::Text))
    }

    fn parent_node(&self, node: NodeId) -> Option<NodeId> {
        self.nodes.get(&node.0)?.parent.map(NodeId)
    }

    fn next_sibling(&self, node: NodeId) -> Option<NodeId> {
        let parent = self.nodes.get(&node.0)?.parent?;
        let siblings = &self.nodes.get(&parent)?.children;
        let pos = siblings.iter().position(|&c| c == node.0)?;
        siblings.get(pos + 1).copied().map(NodeId)
    }

    fn parse_html(&mut self, _html: &str) -> Option<NodeId> {
        // Refused: markup parsed from a string is not something the op log can
        // describe node by node, and a plugin builds its DOM through rinch anyway.
        crate::runtime::log_line(1, "parse_html is not available to a plugin; build the nodes with rsx! instead");
        None
    }

    fn set_scroll_top(&mut self, node: NodeId, scroll_top: f64) {
        if self.is_live(node.0) {
            self.push(Op::SetScrollTop { node: node.0, value: scroll_top });
        }
    }

    fn set_scroll_left(&mut self, node: NodeId, scroll_left: f64) {
        if self.is_live(node.0) {
            self.push(Op::SetScrollLeft { node: node.0, value: scroll_left });
        }
    }

    fn set_inner_html(&mut self, _node: NodeId, _html: &str) {
        // Refused, for the reason `parse_html` is.
        crate::runtime::log_line(1, "set_inner_html is not available to a plugin; build the nodes with rsx! instead");
    }

    fn query_caret_position(&self, _node_id: u64, _byte_offset: usize) -> Option<(f32, f32)> {
        None
    }

    fn query_glyph_bounds(&self, _node_id: u64, _byte_offset: usize) -> Option<GlyphBounds> {
        None
    }

    fn focus_element(&mut self, node: NodeId) {
        if self.is_live(node.0) {
            self.push(Op::Focus { node: node.0 });
        }
    }

    fn set_selection_range(&mut self, node: NodeId, start: usize, end: usize, direction: SelectionDirection) {
        if self.is_live(node.0) {
            self.push(Op::SetSelection {
                node: node.0,
                start: start as u32,
                end: end as u32,
                direction: direction_code(direction),
            });
        }
    }

    fn text_selection_range(&self, node: NodeId) -> Option<(usize, usize, SelectionDirection)> {
        match self.ask(Query::TextSelection { node: node.0 }) {
            Answer::Selection { start, end, direction } => Some((
                start as usize,
                end as usize,
                match direction {
                    1 => SelectionDirection::Backward,
                    2 => SelectionDirection::None,
                    _ => SelectionDirection::Forward,
                },
            )),
            _ => None,
        }
    }

    fn active_element(&self) -> Option<NodeId> {
        match self.ask(Query::ActiveElement) {
            Answer::Node(id) => Some(NodeId(id)),
            _ => None,
        }
    }

    fn resolve_layout(&mut self, _width: f32, _height: f32) {}

    fn query_node_layout(&self, node_id: u64) -> Option<(f32, f32, f32, f32)> {
        match self.ask(Query::Layout { node: node_id }) {
            Answer::Rect { x, y, width, height } => Some((x, y, width, height)),
            _ => None,
        }
    }

    fn bounding_rect(&mut self, node: NodeId) -> Option<ElementBounds> {
        self.query_node_layout(node.0).map(|(x, y, width, height)| ElementBounds { x, y, width, height })
    }

    fn viewport_size(&self) -> Option<(f32, f32)> {
        match self.ask(Query::ViewportSize) {
            Answer::Size { width, height } => Some((width, height)),
            _ => None,
        }
    }

    fn tag_name(&self, node: NodeId) -> Option<String> {
        match &self.nodes.get(&node.0)?.kind {
            Kind::Element(tag) => Some(tag.clone()),
            _ => None,
        }
    }

    fn node_type(&self, node: NodeId) -> Option<u16> {
        Some(match self.nodes.get(&node.0)?.kind {
            Kind::Element(_) => 1,
            Kind::Text => 3,
            Kind::Comment => 8,
        })
    }

    fn text_content(&self, node: NodeId) -> Option<String> {
        self.text_of(node.0)
    }

    fn scroll_top(&self, node: NodeId) -> f64 {
        match self.ask(Query::Scroll { node: node.0 }) {
            Answer::Scroll { top, .. } => top,
            _ => 0.0,
        }
    }

    fn scroll_left(&self, node: NodeId) -> f64 {
        match self.ask(Query::Scroll { node: node.0 }) {
            Answer::Scroll { left, .. } => left,
            _ => 0.0,
        }
    }

    fn scroll_height(&self, node: NodeId) -> f64 {
        match self.ask(Query::Scroll { node: node.0 }) {
            Answer::Scroll { height, .. } => height,
            _ => 0.0,
        }
    }

    fn scroll_width(&self, node: NodeId) -> f64 {
        match self.ask(Query::Scroll { node: node.0 }) {
            Answer::Scroll { width, .. } => width,
            _ => 0.0,
        }
    }

    fn client_height(&self, node: NodeId) -> f64 {
        match self.ask(Query::Scroll { node: node.0 }) {
            Answer::Scroll { client_height, .. } => client_height,
            _ => 0.0,
        }
    }

    fn client_width(&self, node: NodeId) -> f64 {
        match self.ask(Query::Scroll { node: node.0 }) {
            Answer::Scroll { client_width, .. } => client_width,
            _ => 0.0,
        }
    }

    fn request_scroll_into_view(&mut self, node: NodeId) {
        if self.is_live(node.0) {
            self.push(Op::ScrollIntoView { node: node.0 });
        }
    }
}
