//! What the host knows, and what it does for a plugin (ABI 3).

use serde::{Deserialize, Serialize};
use crate::NodeAddr;

/// What the host knows that a plugin draws with and that changes on its own,
/// sent whole whenever any of it changes (`pimble_host_state`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HostState {
    /// The app shows its dark theme: colors are drawn for it
    /// (`pimble_ui::appearance::display_color`).
    pub dark: bool,
    /// Who else is in which node, for every node someone is in, in the stores
    /// the plugin may read (docs/PRESENCE_CONTRACT.md "Plugins"): resolved
    /// as the app resolves them, names, initials and colors included, so a
    /// plugin draws them with `pimble_ui::presence` exactly as the app does.
    pub people: Vec<NodePeople>,
    /// The key of the person the plugin's pane follows, when it follows
    /// someone: the plugin shows where they are in its view (a kanban
    /// scrolls to the card they are on).
    pub following: Option<String>,
}

/// The people in one node.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NodePeople {
    pub node: NodeAddr,
    pub people: Vec<PresentPerson>,
}

/// A person in a node, as the app draws them (`pimble_ui::presence::Person`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PresentPerson {
    pub key: String,
    pub name: String,
    pub initials: String,
    pub color: String,
    pub title: String,
    pub agent: bool,
}

impl HostState {
    /// The people in `node`.
    pub fn people_in(&self, node: &NodeAddr) -> &[PresentPerson] {
        self.people.iter().find(|p| &p.node == node).map_or(&[], |p| p.people.as_slice())
    }
}

/// Something only the app can do for the person using a plugin
/// (`pimble_host`). The host may decline any of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HostRequest {
    /// Open `node` in the plugin's pane, as a click on a row of the outline
    /// does.
    Open { node: NodeAddr },
    /// The person is on `node` in the plugin's view (a card they chose), or
    /// on nothing in it: the app says so to everyone else as part of the
    /// pane's presence, so they show on that node as they would in a native
    /// view. Never document data.
    Focus { node: Option<NodeAddr> },
    /// Follow the person with this key in the plugin's pane, as a click on
    /// their initials on the pane's strip does.
    Follow { person: String },
    /// Remember `value` (the viewer's own text, at most [`MAX_REMEMBERED`](crate::MAX_REMEMBERED)
    /// bytes) as how the viewer looks on its metanode on this device; the
    /// next mount's [`Props::remembered`](crate::Props::remembered). Not document data.
    Remember { value: String },
    /// Open `node` beside the plugin's pane (ABI 6): in the pane the app keeps
    /// beside it, split off to the right the first time, while the plugin's pane
    /// keeps the focus. Where there is no room for another pane it opens in
    /// another pane shown, or in the plugin's own when that is the only one.
    OpenBeside { node: NodeAddr },
}
