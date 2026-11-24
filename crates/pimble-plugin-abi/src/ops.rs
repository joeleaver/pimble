//! The DOM op log: the writes a guest made, in order.

use serde::{Deserialize, Serialize};

/// One DOM write a guest made, in the order it made them. Node ids are the guest's
/// own ([`GUEST_BODY`](crate::GUEST_BODY) and the ids its `Create*` ops introduced); the host maps them
/// to its nodes and refuses any other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Op {
    CreateElement { id: u64, tag: String },
    CreateText { id: u64, text: String },
    CreateComment { id: u64, text: String },
    Append { parent: u64, child: u64 },
    InsertBefore { parent: u64, child: u64, reference: u64 },
    InsertAt { parent: u64, child: u64, index: u32 },
    RemoveChild { parent: u64, child: u64 },
    Replace { old: u64, new: u64 },
    /// Detached, still re-insertable.
    Remove { node: u64 },
    /// Detached for good with its subtree: the host lets go of the ids.
    Discard { node: u64 },
    SetText { node: u64, text: String },
    SetAttribute { node: u64, name: String, value: String },
    RemoveAttribute { node: u64, name: String },
    SetStyle { node: u64, property: String, value: String },
    SetScrollTop { node: u64, value: f64 },
    SetScrollLeft { node: u64, value: f64 },
    Focus { node: u64 },
    /// `direction`: 0 forward, 1 backward, 2 none.
    SetSelection { node: u64, start: u32, end: u32, direction: u8 },
    ScrollIntoView { node: u64 },
}
