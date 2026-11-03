//! A plugin's side of every turn: mounting its view, handing it events and new
//! props, and taking it down, each ending with the op log flushed to the host.
//!
//! [`Guest`] is target-independent, so a native test drives it over any
//! [`HostLink`]; the wasm exports ([`crate::plugin!`]) hold one in a thread-local
//! over the host's imports.

use std::cell::RefCell;
use std::rc::Rc;

use pimble_plugin_abi::{Click, Event, EventData, Key, Modifiers, MountArgs, Props, Wheel, GUEST_BODY};
use rinch::core::dom::{DomDocument, NodeHandle, NodeId, RenderScope};
use rinch::core::events::{
    dispatch_event, dispatch_input_event, dispatch_key_handler, dispatch_scroll_event, dispatch_wheel_handler,
    set_click_context, set_input_context, set_modifier_state, ClickContext, EventHandlerId, InputContext,
    KeyEventData, KeyEventKind, ModifierState, MouseButton, ScrollEvent, WheelEvent,
};
use rinch::core::Signal;

use crate::document::{HostLink, RemoteDocument};

/// What a viewer is handed when it is mounted.
pub struct Mount {
    /// Which of the plugin's viewers the metanode names.
    pub viewer: String,
    /// What it shows. A signal: the host sends new props when the metanode's forest
    /// changes, and the view updates as it would for any signal.
    pub props: Signal<Props>,
}

/// A plugin's view: a rinch component taking the [`Mount`].
pub type ViewFn = fn(&mut RenderScope, Mount) -> NodeHandle;

struct Mounted {
    scope: RenderScope,
    node: NodeHandle,
    props: Signal<Props>,
}

/// A plugin's document and its mounted view.
pub struct Guest {
    doc: Rc<RefCell<RemoteDocument>>,
    mounted: Option<Mounted>,
}

impl Guest {
    /// A guest whose op log goes to `link`.
    pub fn new(link: Rc<dyn HostLink>) -> Self {
        Guest { doc: Rc::new(RefCell::new(RemoteDocument::new(link))), mounted: None }
    }

    /// The document, for a test to read the shadow tree.
    pub fn document(&self) -> Rc<RefCell<RemoteDocument>> {
        self.doc.clone()
    }

    fn dyn_doc(&self) -> Rc<RefCell<dyn DomDocument>> {
        self.doc.clone()
    }

    /// Send what the turn wrote.
    pub fn flush(&self) {
        self.doc.borrow().flush();
    }

    /// Render `view` into the body, in place of whatever was mounted.
    pub fn mount(&mut self, view: ViewFn, args: MountArgs) {
        self.take_down();
        let doc = self.dyn_doc();
        let mut scope = RenderScope::new(doc.clone(), NodeId(GUEST_BODY));
        let MountArgs { viewer, props } = args;
        // Built through the scope, so the props signal and every handler the view
        // registers belong to it and go with it at unmount.
        let (node, props) = scope.build(|s| {
            let props = Signal::new(props);
            (view(s, Mount { viewer, props }), props)
        });
        NodeHandle::new(NodeId(GUEST_BODY), Rc::downgrade(&doc)).append_child(&node);
        self.mounted = Some(Mounted { scope, node, props });
        self.flush();
    }

    /// The mounted view's props changed.
    pub fn props_changed(&mut self, props: Props) {
        if let Some(mounted) = &self.mounted {
            mounted.props.set(props);
        }
        self.flush();
    }

    /// Take the view down: its effects, signals and handlers are disposed and its
    /// nodes discarded, here and in the host.
    pub fn unmount(&mut self) {
        self.take_down();
        self.flush();
    }

    fn take_down(&mut self) {
        if let Some(Mounted { scope, node, .. }) = self.mounted.take() {
            scope.dispose();
            node.discard();
        }
    }

    /// Hand an event to the handler it names; answers whether a key or wheel
    /// handler consumed it.
    pub fn dispatch(&mut self, event: Event) -> bool {
        let id = EventHandlerId(event.handler as usize);
        let consumed = match event.data {
            EventData::Plain(click) => {
                set_modifier_state(modifiers(click.modifiers));
                set_click_context(click_context(&click));
                dispatch_event(id);
                false
            }
            EventData::Input { value, node } => {
                if let Some(node) = node {
                    // The host's field already holds the typed text; the shadow's
                    // `value` follows it, as the desktop's attribute does.
                    self.doc.borrow_mut().note_value(node, &value);
                }
                set_input_context(InputContext { value: value.clone(), node_id: node.unwrap_or(0) });
                dispatch_input_event(id, value);
                false
            }
            EventData::Scroll { top, left } => {
                dispatch_scroll_event(id, ScrollEvent::new(top, left));
                false
            }
            EventData::Key(key) => {
                set_modifier_state(modifiers(key.modifiers));
                dispatch_key_handler(id, &key_data(&key))
            }
            EventData::Wheel(wheel) => {
                set_modifier_state(modifiers(wheel.modifiers));
                dispatch_wheel_handler(id, &wheel_event(&wheel))
            }
        };
        self.flush();
        consumed
    }
}

fn modifiers(m: Modifiers) -> ModifierState {
    ModifierState { shift: m.shift, ctrl: m.ctrl, alt: m.alt, meta: m.meta }
}

fn click_context(click: &Click) -> ClickContext {
    ClickContext {
        mouse_x: click.mouse_x,
        mouse_y: click.mouse_y,
        element_x: click.element_x,
        element_y: click.element_y,
        element_width: click.element_width,
        element_height: click.element_height,
        viewport_width: click.viewport_width,
        viewport_height: click.viewport_height,
        button: match click.button {
            1 => MouseButton::Middle,
            2 => MouseButton::Right,
            _ => MouseButton::Left,
        },
        modifiers: modifiers(click.modifiers),
        ..ClickContext::default()
    }
}

fn key_data(key: &Key) -> KeyEventData {
    let m = key.modifiers;
    KeyEventData::new(key.key.clone(), key.code.clone())
        .with_modifiers(m.ctrl, m.shift, m.alt, m.meta)
        .with_kind(if key.up { KeyEventKind::Up } else { KeyEventKind::Down })
}

fn wheel_event(wheel: &Wheel) -> WheelEvent {
    WheelEvent::new(wheel.delta_x, wheel.delta_y, wheel.x, wheel.y).with_modifiers(modifiers(wheel.modifiers))
}

/// A line for the app's log: 0 error, 1 warning, 2 information, 3 debugging.
pub fn log_line(level: u32, line: &str) {
    #[cfg(target_arch = "wasm32")]
    crate::wasm::log(level, line);
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = level;
        eprintln!("{line}");
    }
}
