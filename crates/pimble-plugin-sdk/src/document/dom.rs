//! `RemoteDocument`'s side of rinch's document traits: one `impl` per
//! supertrait of `DomDocument` (rinch #2999), which rinch's blanket impl joins
//! into the `dyn DomDocument` rinch renders into.

use pimble_plugin_abi::{Answer, Op, Query, GUEST_BODY, GUEST_ROOT};
use rinch::core::dom::{
    normalize_property_name, serialize_declarations, split_declarations, DomContent, DomFocus, DomLayout, DomScroll,
    DomTree, GlyphBounds, NodeId, SelectionDirection,
};
use rinch::core::reactive::ElementBounds;

use super::{direction_code, Kind, RemoteDocument};

/// The tree: identity, nodes, structure and selectors, answered from the shadow.
impl DomTree for RemoteDocument {
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

    fn set_inner_html(&mut self, _node: NodeId, _html: &str) {
        // Refused, for the reason `parse_html` is.
        crate::runtime::log_line(1, "set_inner_html is not available to a plugin; build the nodes with rsx! instead");
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
}

/// What a node holds: text, attributes and styles.
impl DomContent for RemoteDocument {
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

    fn text_content(&self, node: NodeId) -> Option<String> {
        self.text_of(node.0)
    }
}

/// Focus and text selection, which the host answers.
impl DomFocus for RemoteDocument {
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
}

/// Layout, which only the host knows.
impl DomLayout for RemoteDocument {
    fn query_caret_position(&self, _node_id: u64, _byte_offset: usize) -> Option<(f32, f32)> {
        None
    }

    fn query_glyph_bounds(&self, _node_id: u64, _byte_offset: usize) -> Option<GlyphBounds> {
        None
    }

    fn resolve_layout(&mut self, _width: f32, _height: f32) {}

    fn query_node_layout(&self, node_id: u64) -> Option<(f32, f32, f32, f32)> {
        match self.ask(Query::Layout { node: node_id }) {
            Answer::Rect { x, y, width, height } => Some((x, y, width, height)),
            _ => None,
        }
    }

    /// The painted box, asked of the host: the frame of the click context, so
    /// a drag can compare the pointer with an item it is not over. (The
    /// layout box is relative to its parent and cannot be compared.)

    fn bounding_rect(&mut self, node: NodeId) -> Option<ElementBounds> {
        match self.ask(Query::BoundingRect { node: node.0 }) {
            Answer::Rect { x, y, width, height } => Some(ElementBounds { x, y, width, height }),
            _ => None,
        }
    }

    fn viewport_size(&self) -> Option<(f32, f32)> {
        match self.ask(Query::ViewportSize) {
            Answer::Size { width, height } => Some((width, height)),
            _ => None,
        }
    }
}

/// Scroll offsets and requests, which the host answers and applies.
impl DomScroll for RemoteDocument {
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
