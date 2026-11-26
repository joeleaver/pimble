//! Who can open one note (`whoCanOpen`): what an AI agent may learn of a
//! store's shares (docs/MCP_CONTRACT.md "Who can open a note"). The whole
//! list per share, with what it takes to send someone a notice, is
//! `cloudPeople`, which is the server operator's alone.

use pimble_core::{NodeId, StoreId};
use serde::{Deserialize, Serialize};

/// Ask who can open a node (`whoCanOpen`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhoCanOpenRequest {
    pub store_id: StoreId,
    pub node_id: NodeId,
}

/// The members of the shares a node is in, as this device last learned them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhoCanOpenResponse {
    /// Each person once, by the name they go by in the innermost share that
    /// names them.
    pub people: Vec<Opener>,
    /// The node is in a share (or a Pimble Cloud store) whose members this
    /// device does not have (not learned yet, or a share that ended), so
    /// `people` may leave some out.
    #[serde(default)]
    pub incomplete: bool,
}

/// One person who can open a node: the account a mention names, and the name
/// they go by in the share. Nothing else of theirs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Opener {
    pub user_id: String,
    pub name: String,
}
