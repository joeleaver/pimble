//! Reads only the host can answer.

use serde::{Deserialize, Serialize};

/// A read the guest's shadow tree cannot answer. The guest flushes its op log
/// before asking, so the host's document holds everything the guest wrote.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Query {
    /// The text a form control holds now (`DomDocument::live_value`).
    LiveValue { node: u64 },
    /// The layout box relative to its parent, `(x, y, width, height)`
    /// (`query_node_layout`).
    Layout { node: u64 },
    /// The scroll offsets and sizes.
    Scroll { node: u64 },
    /// The focused node, when it is one of the guest's.
    ActiveElement,
    /// The window's size.
    ViewportSize,
    /// A text control's selection.
    TextSelection { node: u64 },
    /// The box the element is painted in, in the window's logical pixels: the
    /// frame of the click context's pointer and element box
    /// (`DomDocument::bounding_rect`, ABI 4). A drag compares the pointer
    /// with the boxes of items it is not over; the parent-relative `Layout`
    /// cannot be compared with it.
    BoundingRect { node: u64 },
}

/// The host's answer to a [`Query`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Answer {
    /// Nothing to say: the node is not the guest's, or the backend cannot tell.
    Nothing,
    Text(String),
    Rect { x: f32, y: f32, width: f32, height: f32 },
    Scroll { top: f64, left: f64, height: f64, width: f64, client_height: f64, client_width: f64 },
    Node(u64),
    Size { width: f32, height: f32 },
    /// `direction`: 0 forward, 1 backward, 2 none.
    Selection { start: u32, end: u32, direction: u8 },
}
