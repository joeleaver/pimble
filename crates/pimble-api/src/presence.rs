//! Presence frames (docs/PRESENCE_CONTRACT.md): one builder for every client
//! that says where it is, so the session naming, the rising sequence and
//! the hold time are written once. The app names a session after a pane;
//! an agent after the node it works in ([`Whereabouts`]).

use std::collections::HashMap;
use std::sync::Mutex;

use pimble_core::{NodeId, StoreId};
use pimble_rpc::{BoardPresence, PresenceBody, PresenceFrame, TextPresence};

use crate::Addr;

/// How long an agent's frame holds: it is there 15 s after its last call
/// (docs/PRESENCE_CONTRACT.md section 8). There is no heartbeat and no
/// goodbye, so it fades that long after it stops.
pub const AGENT_TTL_MS: u64 = 15_000;

/// The frames one client sends.
pub struct PresenceFrames {
    client_id: String,
    ttl_ms: u64,
    seq: Mutex<u64>,
}

impl PresenceFrames {
    pub fn new(client_id: impl Into<String>, ttl_ms: u64) -> Self {
        Self { client_id: client_id.into(), ttl_ms, seq: Mutex::new(0) }
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// The frame session `session` of this client says about `node`: where
    /// it is (`body`), or that it left (`None`). The session is
    /// `<client id>:<session>`; the sequence rises for the life of the
    /// process, from the clock, so a client started again starts above where
    /// it was.
    pub fn frame(&self, store: StoreId, node: NodeId, session: &str, body: Option<PresenceBody>) -> PresenceFrame {
        let seq = {
            let mut seq = self.seq.lock().unwrap();
            let now = chrono::Utc::now().timestamp_millis().max(0) as u64;
            *seq = now.max(*seq + 1);
            *seq
        };
        PresenceFrame {
            store_id: store,
            doc_id: node,
            session: format!("{}:{session}", self.client_id),
            seq,
            who: None,
            left: body.is_none(),
            body: body.map(|b| b.encode()).unwrap_or_default(),
            sealed: false,
            ttl_ms: self.ttl_ms,
        }
    }
}

/// Where a client that works node by node (an agent) last was: a session per
/// node, its caret where it last wrote there, so a read keeps it, and the
/// device it works through (the MCP client app's name).
#[derive(Default)]
pub struct Whereabouts {
    device: Mutex<String>,
    carets: Mutex<HashMap<Addr, Vec<u8>>>,
}

impl Whereabouts {
    pub fn set_device(&self, name: &str) {
        *self.device.lock().unwrap() = name.trim().to_string();
    }

    /// The frame that says this client is in `addr`'s text, its caret where
    /// it just wrote (`None` keeps the last one), or on a canvas there.
    pub fn frame(&self, frames: &PresenceFrames, addr: Addr, caret: Option<Vec<u8>>, board: Option<BoardPresence>) -> PresenceFrame {
        let text = {
            let mut carets = self.carets.lock().unwrap();
            if let Some(caret) = caret {
                carets.insert(addr, caret);
            }
            // A session is in a note's text or on a canvas, never both.
            if board.is_some() {
                None
            } else {
                carets.get(&addr).map(|caret| TextPresence::new(caret, None))
            }
        };
        let device = self.device.lock().unwrap().clone();
        let body = PresenceBody { device, focused: true, text, board };
        frames.frame(addr.store, addr.node, &addr.node.to_string(), Some(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_rise_and_name_their_session() {
        let frames = PresenceFrames::new("agent:7", 300);
        let addr = Addr { store: StoreId::new(), node: NodeId::new() };
        let at = Whereabouts::default();
        at.set_device(" Claude Code ");
        let first = at.frame(&frames, addr, Some(vec![1, 2]), None);
        let second = at.frame(&frames, addr, None, None);
        assert!(second.seq > first.seq);
        assert_eq!(first.session, format!("agent:7:{}", addr.node));
        assert_eq!(first.ttl_ms, 300);
        // A read keeps the caret where it wrote.
        let body = PresenceBody::decode(&second.body).unwrap();
        assert_eq!(body.device, "Claude Code");
        assert_eq!(body.text.unwrap().caret_bytes().unwrap(), vec![1, 2]);
        let left = frames.frame(addr.store, addr.node, "0", None);
        assert!(left.left && left.body.is_empty());
    }
}
