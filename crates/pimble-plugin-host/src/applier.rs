//! Applying a plugin's op log to the app's document (docs/PLUGINS_CONTRACT.md section
//! 3.2), and answering the reads only the host can.
//!
//! Everything here goes through the host's own `dyn DomDocument` (through rinch's
//! `NodeHandle`, so focus release and rinch's own bookkeeping happen as for any
//! other write), so the same code serves the desktop document and, in wave 3, the
//! browser's.
//!
//! **The map is the sandbox.** The applier keeps, per plugin instance, the host node
//! each guest node id stands for. The guest's body is the instance's container; every
//! other id must be one the guest's own `Create*` op introduced. An op naming any
//! other id is refused, and the instance is stopped: a plugin can reach nothing in
//! the window but what it built.
//!
//! **Handlers are proxied.** rinch writes an event handler as an id in one of a closed
//! list of attributes ([`pimble_plugin_abi::HANDLER_ATTRIBUTES`]). A guest's id means
//! nothing to the host's registries, so for each one the applier registers a handler
//! of its own that hands the event, translated, to the guest's `dispatch`, and
//! writes that handler's id instead. A key or wheel handler answers with the guest's
//! "consumed".

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::cell::RefCell;

use pimble_plugin_abi::{
    dispatched, handler_kind, Answer, Click, Event, EventData, HandlerKind, Key, Modifiers, Op, Query, Wheel, FIRST_GUEST_NODE,
    GUEST_BODY,
};
use rinch_core::dom::{DomDocument, NodeHandle, NodeId, SelectionDirection};
use rinch_core::events::{
    get_click_context, get_input_context, register_handler, register_input_handler, register_key_handler,
    register_scroll_handler, register_wheel_handler, unregister_handler, ClickContext, EventHandlerId, InputCallback,
    KeyCallback, KeyEventKind, ModifierState, MouseButton, ScrollCallback, WheelCallback,
};

/// Hands an event to the guest and answers its [`pimble_plugin_abi::dispatched`]
/// flags: whether it consumed it, and what of the window's machinery its handler
/// asked for.
pub type Dispatch = Rc<dyn Fn(Event) -> u32>;

/// Why the host refused what a plugin sent. The plugin is stopped with this sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal(pub String);

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

/// Elements a plugin may not create: they reach past its area of the window (a
/// stylesheet restyles the whole app) or load something from elsewhere.
const REFUSED_TAGS: &[&str] = &["style", "link", "script", "meta", "base", "head", "html", "body", "iframe", "object", "embed"];

/// How the host's document behaves where a plugin cannot tell.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApplierOptions {
    /// The desktop backend makes an element a text field only when it carries
    /// `data-oninput`. `rsx!` adds a no-op one to an `<input>` or `<textarea>` that
    /// has only `onchange`, `onsubmit` or key handlers, but only when it compiles for
    /// a target other than wasm32, so a plugin's never does: the desktop host adds it
    /// here instead. The browser host must not (it would shadow an ancestor's
    /// `data-oninput` in the web backend's delegation).
    pub text_field_shim: bool,
}

/// One plugin instance's guest-to-host node map, its proxy handlers, and the
/// document it writes into.
pub struct Applier {
    doc: Rc<RefCell<dyn DomDocument>>,
    container: NodeId,
    map: HashMap<u64, NodeId>,
    /// Shared with the key proxies, which name the guest node a key was sent to.
    back: Rc<RefCell<HashMap<NodeId, u64>>>,
    /// The proxy registered for each guest node's handler attribute.
    handlers: HashMap<(u64, &'static str), EventHandlerId>,
    /// Fields given the desktop's no-op `data-oninput` by the host, not the guest.
    shimmed: HashSet<u64>,
    dispatch: Dispatch,
    options: ApplierOptions,
}

impl Applier {
    /// An applier writing under `container`, the instance's area of the window,
    /// which stands for the guest's body.
    pub fn new(doc: Rc<RefCell<dyn DomDocument>>, container: NodeId, dispatch: Dispatch, options: ApplierOptions) -> Self {
        let mut map = HashMap::new();
        let mut back = HashMap::new();
        map.insert(GUEST_BODY, container);
        back.insert(container, GUEST_BODY);
        Applier {
            doc,
            container,
            map,
            back: Rc::new(RefCell::new(back)),
            handlers: HashMap::new(),
            shimmed: HashSet::new(),
            dispatch,
            options,
        }
    }

    /// The host node guest node `guest` stands for, if it is one of the guest's.
    pub fn host_node(&self, guest: u64) -> Option<NodeId> {
        self.map.get(&guest).copied()
    }

    /// The guest node host node `host` stands for, if it is one.
    pub fn guest_node(&self, host: NodeId) -> Option<u64> {
        self.back.borrow().get(&host).copied()
    }

    /// How many host handlers stand for the guest's.
    pub fn proxy_count(&self) -> usize {
        self.handlers.len()
    }

    fn handle(&self, node: NodeId) -> NodeHandle {
        NodeHandle::new(node, Rc::downgrade(&self.doc))
    }

    fn host(&self, guest: u64) -> Result<NodeHandle, Refusal> {
        self.map
            .get(&guest)
            .map(|&n| self.handle(n))
            .ok_or_else(|| Refusal(format!("it named node {guest}, which is not one of its own")))
    }

    fn fresh(&self, id: u64) -> Result<(), Refusal> {
        if id < FIRST_GUEST_NODE || self.map.contains_key(&id) {
            return Err(Refusal(format!("it created node {id} over a node that already exists")));
        }
        Ok(())
    }

    fn adopt(&mut self, guest: u64, host: NodeId) {
        self.map.insert(guest, host);
        self.back.borrow_mut().insert(host, guest);
    }

    /// Apply `ops` in order. Stops at the first refused op; the caller stops the
    /// instance then, so what was applied before it is taken down with the rest.
    pub fn apply(&mut self, ops: Vec<Op>) -> Result<(), Refusal> {
        for op in ops {
            self.apply_one(op)?;
        }
        Ok(())
    }

    fn apply_one(&mut self, op: Op) -> Result<(), Refusal> {
        match op {
            Op::CreateElement { id, tag } => {
                self.fresh(id)?;
                if REFUSED_TAGS.contains(&tag.to_ascii_lowercase().as_str()) {
                    return Err(Refusal(format!("it created a <{tag}> element, which a plugin may not")));
                }
                let host = self.doc.borrow_mut().create_element(&tag);
                self.adopt(id, host);
            }
            Op::CreateText { id, text } => {
                self.fresh(id)?;
                let host = self.doc.borrow_mut().create_text(&text);
                self.adopt(id, host);
            }
            Op::CreateComment { id, text } => {
                self.fresh(id)?;
                let host = self.doc.borrow_mut().create_comment(&text);
                self.adopt(id, host);
            }
            Op::Append { parent, child } => {
                let (parent, child) = (self.host(parent)?, self.host(child)?);
                parent.append_child(&child);
            }
            Op::InsertBefore { parent, child, reference } => {
                let (parent, child, reference) = (self.host(parent)?, self.host(child)?, self.host(reference)?);
                parent.insert_before(&child, &reference);
            }
            Op::InsertAt { parent, child, index } => {
                let (parent, child) = (self.host(parent)?, self.host(child)?);
                let siblings: Vec<NodeHandle> =
                    parent.children().into_iter().filter(|c| c.node_id() != child.node_id()).collect();
                match siblings.get(index as usize) {
                    Some(reference) => parent.insert_before(&child, reference),
                    None => parent.append_child(&child),
                }
            }
            Op::RemoveChild { parent, child } => {
                let (parent, child) = (self.host(parent)?, self.host(child)?);
                parent.remove_child(&child);
            }
            Op::Replace { old, new } => {
                let (old, new) = (self.host(old)?, self.host(new)?);
                old.replace_with(&new);
            }
            Op::Remove { node } => {
                if node == GUEST_BODY {
                    return Err(Refusal("it removed its own area of the window".into()));
                }
                self.host(node)?.remove();
            }
            Op::Discard { node } => {
                if node == GUEST_BODY {
                    return Err(Refusal("it discarded its own area of the window".into()));
                }
                let host = self.host(node)?;
                self.forget_subtree(host.node_id());
                host.discard();
            }
            Op::SetText { node, text } => self.host(node)?.set_text(&text),
            Op::SetAttribute { node, name, value } => self.set_attribute(node, &name, &value)?,
            Op::RemoveAttribute { node, name } => {
                let host = self.host(node)?;
                if let Some((attr, _)) = pimble_plugin_abi::HANDLER_ATTRIBUTES.iter().find(|(a, _)| *a == name) {
                    if let Some(old) = self.handlers.remove(&(node, *attr)) {
                        unregister_handler(old);
                    }
                }
                host.remove_attribute(&name);
            }
            Op::SetStyle { node, property, value } => self.host(node)?.set_style(&property, &value),
            Op::SetScrollTop { node, value } => self.host(node)?.set_scroll_top(value),
            Op::SetScrollLeft { node, value } => self.host(node)?.set_scroll_left(value),
            Op::Focus { node } => self.host(node)?.focus(),
            Op::SetSelection { node, start, end, direction } => self.host(node)?.set_selection_range(
                start as usize,
                end as usize,
                match direction {
                    1 => SelectionDirection::Backward,
                    2 => SelectionDirection::None,
                    _ => SelectionDirection::Forward,
                },
            ),
            Op::ScrollIntoView { node } => self.host(node)?.scroll_into_view(),
        }
        Ok(())
    }

    fn set_attribute(&mut self, node: u64, name: &str, value: &str) -> Result<(), Refusal> {
        let host = self.host(node)?;
        let Some(&(attr, kind)) = pimble_plugin_abi::HANDLER_ATTRIBUTES.iter().find(|(a, _)| *a == name) else {
            host.set_attribute(name, value);
            return Ok(());
        };
        let guest_handler: u64 = value
            .parse()
            .map_err(|_| Refusal(format!("it wrote {value:?} as a handler id in {name}")))?;
        if let Some(old) = self.handlers.remove(&(node, attr)) {
            unregister_handler(old);
        }
        if attr == "data-oninput" {
            self.shimmed.remove(&node);
        }
        let Some(proxy) = self.proxy(node, host.node_id(), attr, kind, guest_handler) else {
            // Files from the operating system: no install permission covers them
            // yet, so the attribute is not written and nothing is dropped on it.
            tracing::debug!("plugin: {name} is not wired for plugins");
            return Ok(());
        };
        self.handlers.insert((node, attr), proxy);
        host.set_attribute(attr, &proxy.0.to_string());
        if self.options.text_field_shim && kind != HandlerKind::Plain && attr != "data-oninput" {
            self.shim_text_field(node, &host);
        }
        Ok(())
    }

    /// The desktop's no-op `data-oninput` on a field that has none (see
    /// [`ApplierOptions::text_field_shim`]).
    fn shim_text_field(&mut self, node: u64, host: &NodeHandle) {
        let field = matches!(host.tag_name().as_deref(), Some("input" | "textarea"));
        if !field || self.handlers.contains_key(&(node, "data-oninput")) || self.shimmed.contains(&node) {
            return;
        }
        let noop = rinch_core::reactive::unowned(|| register_input_handler(InputCallback::new(|_: String| {})));
        self.handlers.insert((node, "data-oninput"), noop);
        self.shimmed.insert(node);
        host.set_attribute("data-oninput", &noop.0.to_string());
    }

    /// A host handler standing for the guest's `guest_handler` on guest node
    /// `node` (host node `host`), or `None` for a kind plugins are not given.
    fn proxy(&self, node: u64, host: NodeId, attr: &'static str, kind: HandlerKind, guest_handler: u64) -> Option<EventHandlerId> {
        let dispatch = self.dispatch.clone();
        let (doc, back) = (Rc::downgrade(&self.doc), self.back.clone());
        let send = move |data: EventData| dispatch(Event { handler: guest_handler, data });
        // Unowned: the proxy lives as long as the guest's attribute does, and the
        // applier unregisters it then. Registered under whatever app scope happens
        // to be running (an effect that mounted the plugin), it would be freed when
        // that scope re-ran.
        rinch_core::reactive::unowned(|| match kind {
            HandlerKind::Plain => Some(register_handler(Rc::new(move || {
                // A drag source that draws its own feedback (a sortable list's
                // item) says so with `data-drag-ghost="false"` on itself or an
                // ancestor; the window's ghost is hidden before the guest hears
                // the start. A rinch whose runtime reads the attribute itself
                // makes this a no-op; the guest's own `suppress_drag_ghost`
                // reaches nothing that paints.
                if attr == "data-ondragstart" && doc.upgrade().is_some_and(|d| ghost_refused(&*d.borrow(), host)) {
                    rinch_core::events::suppress_drag_ghost();
                }
                send(EventData::Plain(click(&get_click_context())));
            }))),
            HandlerKind::Input => Some(register_input_handler(InputCallback::new(move |value: String| {
                // The field the text came from, when it is this node (it is on the
                // desktop, where a field carries its own `data-oninput`).
                let at = get_input_context().node_id;
                let node = (at == host.0).then_some(node);
                send(EventData::Input { value, node });
            }))),
            HandlerKind::Scroll => Some(register_scroll_handler(ScrollCallback::new(move |e| {
                send(EventData::Scroll { top: e.scroll_top, left: e.scroll_left });
            }))),
            HandlerKind::Key => Some(register_key_handler(KeyCallback::new(move |k| {
                send(EventData::Key(Key {
                    key: k.key.clone(),
                    code: k.code.clone(),
                    modifiers: Modifiers { shift: k.shift, ctrl: k.ctrl, alt: k.alt, meta: k.meta },
                    up: k.kind == KeyEventKind::Up,
                    target: key_target(&doc, &back),
                })) & dispatched::CONSUMED
                    != 0
            }))),
            HandlerKind::Wheel => Some(register_wheel_handler(WheelCallback::new(move |w| {
                send(EventData::Wheel(Wheel {
                    delta_x: w.delta_x,
                    delta_y: w.delta_y,
                    x: w.x,
                    y: w.y,
                    modifiers: modifiers(w.modifiers),
                })) & dispatched::CONSUMED
                    != 0
            }))),
            HandlerKind::Files => None,
        })
    }

    /// Let go of the ids and proxies of host node `host` and everything under it.
    fn forget_subtree(&mut self, host: NodeId) {
        let children = self.doc.borrow().get_children(host);
        for child in children {
            self.forget_subtree(child);
        }
        let gone = self.back.borrow_mut().remove(&host);
        if let Some(guest) = gone {
            self.map.remove(&guest);
            self.shimmed.remove(&guest);
            let gone: Vec<(u64, &'static str)> = self.handlers.keys().filter(|(n, _)| *n == guest).copied().collect();
            for key in gone {
                if let Some(id) = self.handlers.remove(&key) {
                    unregister_handler(id);
                }
            }
        }
    }

    /// Take down everything the guest built: every proxy is unregistered, every node
    /// it put in the container is discarded, and the map forgets every id.
    pub fn clear(&mut self) {
        for (_, id) in self.handlers.drain() {
            unregister_handler(id);
        }
        let container = self.handle(self.container);
        for child in container.children() {
            child.discard();
        }
        self.map.retain(|&guest, _| guest == GUEST_BODY);
        self.back.borrow_mut().retain(|_, &mut guest| guest == GUEST_BODY);
        self.shimmed.clear();
    }

    /// Answer a read the guest's shadow cannot. A node that is not the guest's
    /// answers [`Answer::Nothing`]: a read reaches nothing, so it is not refused.
    pub fn answer(&self, query: &Query) -> Answer {
        if let Query::BoundingRect { node: guest } = *query {
            // rinch-dom may shape a font probe to measure an inline element,
            // so this read borrows the document mutably.
            let Some(n) = self.map.get(&guest).copied() else { return Answer::Nothing };
            return match self.doc.borrow_mut().bounding_rect(n) {
                Some(b) => Answer::Rect { x: b.x, y: b.y, width: b.width, height: b.height },
                None => Answer::Nothing,
            };
        }
        let doc = self.doc.borrow();
        let node = |guest: u64| self.map.get(&guest).copied();
        match *query {
            Query::LiveValue { node: guest } => {
                node(guest).and_then(|n| doc.live_value(n)).map(Answer::Text).unwrap_or(Answer::Nothing)
            }
            Query::Layout { node: guest } => node(guest)
                .and_then(|n| doc.query_node_layout(n.0))
                .map(|(x, y, width, height)| Answer::Rect { x, y, width, height })
                .unwrap_or(Answer::Nothing),
            Query::Scroll { node: guest } => match node(guest) {
                Some(n) => Answer::Scroll {
                    top: doc.scroll_top(n),
                    left: doc.scroll_left(n),
                    height: doc.scroll_height(n),
                    width: doc.scroll_width(n),
                    client_height: doc.client_height(n),
                    client_width: doc.client_width(n),
                },
                None => Answer::Nothing,
            },
            Query::ActiveElement => {
                doc.active_element().and_then(|n| self.back.borrow().get(&n).copied()).map(Answer::Node).unwrap_or(Answer::Nothing)
            }
            Query::ViewportSize => {
                doc.viewport_size().map(|(width, height)| Answer::Size { width, height }).unwrap_or(Answer::Nothing)
            }
            Query::TextSelection { node: guest } => node(guest)
                .and_then(|n| doc.text_selection_range(n))
                .map(|(start, end, direction)| Answer::Selection {
                    start: start as u32,
                    end: end as u32,
                    direction: match direction {
                        SelectionDirection::Forward => 0,
                        SelectionDirection::Backward => 1,
                        SelectionDirection::None => 2,
                    },
                })
                .unwrap_or(Answer::Nothing),
            Query::BoundingRect { .. } => unreachable!("answered above"),
        }
    }
}

impl Drop for Applier {
    fn drop(&mut self) {
        for (_, id) in self.handlers.drain() {
            unregister_handler(id);
        }
    }
}

/// The guest node a key is sent to: the document's focused element, when it is
/// one of the guest's. (rinch's `KeyEventData::target` names it on a rinch that
/// has it; on the desktop the two are the same element.)
fn key_target(doc: &std::rc::Weak<RefCell<dyn DomDocument>>, back: &Rc<RefCell<HashMap<NodeId, u64>>>) -> Option<u64> {
    let focused = doc.upgrade()?.borrow().active_element()?;
    back.borrow().get(&focused).copied()
}

/// Whether the drag source `node`, or an element above it, is marked
/// `data-drag-ghost="false"`.
fn ghost_refused(doc: &dyn DomDocument, node: NodeId) -> bool {
    let mut at = Some(node);
    while let Some(n) = at {
        if doc.get_attribute(n, "data-drag-ghost").as_deref() == Some("false") {
            return true;
        }
        at = doc.parent_node(n);
    }
    false
}

/// Whether `name` is one of rinch's handler attributes.
pub fn is_handler_attribute(name: &str) -> bool {
    handler_kind(name).is_some()
}

fn modifiers(m: ModifierState) -> Modifiers {
    Modifiers { shift: m.shift, ctrl: m.ctrl, alt: m.alt, meta: m.meta }
}

fn click(c: &ClickContext) -> Click {
    Click {
        mouse_x: c.mouse_x,
        mouse_y: c.mouse_y,
        element_x: c.element_x,
        element_y: c.element_y,
        element_width: c.element_width,
        element_height: c.element_height,
        viewport_width: c.viewport_width,
        viewport_height: c.viewport_height,
        button: match c.button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
        },
        modifiers: modifiers(c.modifiers),
    }
}
