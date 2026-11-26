//! Events: the handler attributes the host rewrites, and what their events carry.

use serde::{Deserialize, Serialize};

/// What kind of handler an event attribute holds, and so what its event carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandlerKind {
    /// `Fn()`: a click, a press, a hover, a drag step. Carries the click context.
    Plain,
    /// `Fn(String)`: `oninput`, `onchange`.
    Input,
    /// `Fn(ScrollEvent)`.
    Scroll,
    /// `Fn(&KeyEventData) -> bool`: answered synchronously.
    Key,
    /// `Fn(&WheelEvent) -> bool`: answered synchronously.
    Wheel,
    /// Files dropped from the operating system. A plugin is given no files (no
    /// install permission covers them yet), so the host never wires these.
    Files,
}

/// rinch's closed list of event attributes (`rinch-macros` `HTML_EVENTS`: the
/// attribute an `rsx!` `on*` prop writes its handler id to), with the kind of
/// handler each holds. The host rewrites exactly these: a guest's handler id in one
/// of them becomes a host handler of the host's own that calls the guest's
/// `dispatch`. Any other attribute passes through as data.
pub const HANDLER_ATTRIBUTES: &[(&str, HandlerKind)] = &[
    ("data-rid", HandlerKind::Plain),
    ("data-oninput", HandlerKind::Input),
    ("data-onchange", HandlerKind::Input),
    ("data-onscroll", HandlerKind::Scroll),
    ("data-onfiledrop", HandlerKind::Files),
    ("data-ondropfiles", HandlerKind::Files),
    ("data-onkeydown", HandlerKind::Key),
    ("data-onkeyup", HandlerKind::Key),
    ("data-onwheel", HandlerKind::Wheel),
    ("data-onsubmit", HandlerKind::Plain),
    ("data-onmousedown", HandlerKind::Plain),
    ("data-onmouseup", HandlerKind::Plain),
    ("data-onmousemove", HandlerKind::Plain),
    ("data-onenter", HandlerKind::Plain),
    ("data-onleave", HandlerKind::Plain),
    ("data-oncontextmenu", HandlerKind::Plain),
    ("data-ondragstart", HandlerKind::Plain),
    ("data-ondragmove", HandlerKind::Plain),
    ("data-ondragenter", HandlerKind::Plain),
    ("data-ondragover", HandlerKind::Plain),
    ("data-ondragleave", HandlerKind::Plain),
    ("data-ondrop", HandlerKind::Plain),
    ("data-ondragend", HandlerKind::Plain),
    ("data-onfiledragenter", HandlerKind::Plain),
    ("data-onfiledragleave", HandlerKind::Plain),
];

// rinch's sortable lists (joeleaver/rinch#2093) run on handlers already in this
// list (`data-ondragstart`, `-ondragover`, `-ondragleave`, `-ondrop`,
// `-ondragmove`, `-ondragend`, `data-onkeydown`) and judge a drop inside the
// guest, synchronously, with what the guest holds: no new kind was needed. What
// they need of the host besides is the click context of each drag step, the
// painted box of an element ([`Query::BoundingRect`](crate::Query::BoundingRect)), the element a key was sent
// to ([`Key::target`]), the end of every drag ([`exports::DRAG_ENDED`](crate::exports::DRAG_ENDED)), and the
// drag ghost hidden for a source marked `data-drag-ghost="false"` (plain data the
// host reads itself).

/// The kind of handler attribute `name` holds, or `None` for an ordinary attribute.
pub fn handler_kind(name: &str) -> Option<HandlerKind> {
    HANDLER_ATTRIBUTES.iter().find(|(attr, _)| *attr == name).map(|(_, kind)| *kind)
}

/// The flags `dispatch` answers (ABI 4).
pub mod dispatched {
    /// A key or wheel handler consumed the event. The other bits are free for
    /// what a later handler may ask of the host's window.
    pub const CONSUMED: u32 = 1;
}

/// An event for one of the guest's handlers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// The guest's own handler id, as it wrote it into the attribute.
    pub handler: u64,
    pub data: EventData,
}

/// What an event carries, by the kind of handler.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EventData {
    Plain(Click),
    /// The field's text; `node` is the guest id of the field, when the host knew it.
    Input { value: String, node: Option<u64> },
    Scroll { top: f64, left: f64 },
    Key(Key),
    Wheel(Wheel),
}

/// Shift, Ctrl, Alt and Meta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
}

/// rinch's click context, in the host window's logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Click {
    pub mouse_x: f32,
    pub mouse_y: f32,
    pub element_x: f32,
    pub element_y: f32,
    pub element_width: f32,
    pub element_height: f32,
    pub viewport_width: f32,
    pub viewport_height: f32,
    /// 0 left, 1 middle, 2 right.
    pub button: u8,
    pub modifiers: Modifiers,
}

/// A key press or release.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Key {
    /// `KeyboardEvent.key`.
    pub key: String,
    /// `KeyboardEvent.code`.
    pub code: String,
    pub modifiers: Modifiers,
    /// A release, else a press.
    pub up: bool,
    /// The guest node the key was sent to (the focused element), when it is
    /// one of the guest's (ABI 4): an element handler hears keys typed in the
    /// fields inside it too, and this tells them apart (rinch
    /// `KeyEventData::target`).
    pub target: Option<u64>,
}

/// A wheel turn over an element.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Wheel {
    pub delta_x: f32,
    pub delta_y: f32,
    pub x: f32,
    pub y: f32,
    pub modifiers: Modifiers,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_handler_attributes_are_rinchs_closed_list() {
        assert_eq!(HANDLER_ATTRIBUTES.len(), 25);
        assert_eq!(handler_kind("data-rid"), Some(HandlerKind::Plain));
        assert_eq!(handler_kind("data-onkeydown"), Some(HandlerKind::Key));
        assert_eq!(handler_kind("class"), None);
    }
}
