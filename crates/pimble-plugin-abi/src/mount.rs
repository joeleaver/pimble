//! Mounting: what a viewer is handed when the host mounts it.

use serde::{Deserialize, Serialize};

/// What `mount` is handed: which of the plugin's viewers, and what it shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MountArgs {
    pub viewer: String,
    pub props: Props,
}

/// What a viewer is handed: the metanode it shows and the viewer's settings on
/// it. The viewer reads the metanode's source (`Pimble::metanode_source`) and
/// everything else through the Pimble API, which keeps it live; the props change
/// only when the settings do.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Props {
    /// The metanode.
    pub metanode: NodeAddr,
    /// The viewer's shared settings on the node (its `data` under the viewer's
    /// id), as JSON, when it has any.
    pub settings: Option<String>,
    /// What this device remembers of how the viewer last looked on this
    /// metanode, as the viewer handed it to [`HostRequest::Remember`](crate::HostRequest::Remember): one
    /// person's view on one device (lanes collapsed, columns folded), never
    /// in a store and never synced, like the explorer's expanded rows.
    pub remembered: Option<String>,
}

/// The most a viewer may ask the host to remember for one metanode, in bytes.
pub const MAX_REMEMBERED: usize = 16 * 1024;

/// A node by its store and its id, as text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct NodeAddr {
    pub store: String,
    pub node: String,
}

impl NodeAddr {
    /// The API's address, when both ids parse.
    pub fn addr(&self) -> Option<pimble_api::Addr> {
        let store = pimble_core::StoreId::parse(&self.store).ok()?;
        let node = pimble_core::NodeId::parse(&self.node).ok()?;
        Some(pimble_api::Addr::new(store, node))
    }
}

impl From<pimble_api::Addr> for NodeAddr {
    fn from(a: pimble_api::Addr) -> Self {
        NodeAddr { store: a.store.to_string(), node: a.node.to_string() }
    }
}
