//! The agent is present while it works (docs/PRESENCE_CONTRACT.md section 8):
//! after each tool call that reads or writes a node, a frame through the
//! local server says this agent is in that node, with its caret at the end
//! of what it last wrote there. There is no heartbeat and no goodbye: a frame
//! holds [`TTL_MS`], so the agent fades that long after its last call.
//!
//! One session per node (`agent:{id}:{node}`, the client being `agent:{id}`),
//! so an agent working through several notes shows in each until it leaves
//! it alone. The server stamps who it is (the account plus the agent's
//! name); the body's `device` is the MCP client app, when it said its name.
//! Nothing here is stored, and a frame that cannot be sent never fails the
//! tool call.

use std::collections::HashMap;
use std::sync::Mutex;

use pimble_client::PimbleClient;
use pimble_rpc::{BoardPresence, PresenceBody, PresenceFrame, TextPresence};

use crate::nodes::Addr;

/// How long an agent's frame holds: it is there 15 s after its last call.
pub const TTL_MS: u64 = 15_000;

pub struct Presence {
    /// The client the sessions belong to: `agent:{id}`, as edits say.
    client_id: String,
    ttl_ms: u64,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// The MCP client app's name, from its handshake.
    device: String,
    /// The last sequence number sent.
    seq: u64,
    /// Where the agent last wrote in each node: a read there keeps the caret.
    carets: HashMap<Addr, Vec<u8>>,
}

impl Presence {
    pub fn new(client_id: String) -> Self {
        Self::with_ttl(client_id, TTL_MS)
    }

    pub fn with_ttl(client_id: String, ttl_ms: u64) -> Self {
        Self { client_id, ttl_ms, inner: Mutex::new(Inner::default()) }
    }

    /// The MCP client app this agent works through ("Claude Code").
    pub fn set_device(&self, name: &str) {
        self.inner.lock().unwrap().device = name.trim().to_string();
    }

    /// The frame that says this agent is in `addr`; `caret` is where it just
    /// wrote (a sticky index into the content), `None` to keep the last one.
    pub fn frame(&self, addr: Addr, caret: Option<Vec<u8>>) -> PresenceFrame {
        self.frame_with(addr, caret, None)
    }

    /// The frame that says this agent is on a canvas in `addr`: where its
    /// pointer is and what it just drew or changed, selected.
    pub fn board_frame(&self, addr: Addr, board: BoardPresence) -> PresenceFrame {
        self.frame_with(addr, None, Some(board))
    }

    fn frame_with(&self, addr: Addr, caret: Option<Vec<u8>>, board: Option<BoardPresence>) -> PresenceFrame {
        let mut inner = self.inner.lock().unwrap();
        if let Some(caret) = caret {
            inner.carets.insert(addr, caret);
        }
        // Rising for the life of this process, and from the clock, so an
        // agent started again starts above where it was.
        let now = chrono::Utc::now().timestamp_millis().max(0) as u64;
        inner.seq = now.max(inner.seq + 1);
        // A session is in a note's text or on a canvas, never both.
        let text = if board.is_some() { None } else { inner.carets.get(&addr).map(|caret| TextPresence::new(caret, None)) };
        let body = PresenceBody { device: inner.device.clone(), focused: true, text, board };
        PresenceFrame {
            store_id: addr.store,
            doc_id: addr.node,
            session: format!("{}:{}", self.client_id, addr.node),
            seq: inner.seq,
            who: None,
            left: false,
            body: body.encode(),
            sealed: false,
            ttl_ms: self.ttl_ms,
        }
    }

    /// Say this agent is in `addr`, through `client`.
    pub async fn say(&self, client: &PimbleClient, addr: Addr, caret: Option<Vec<u8>>) {
        self.send(client, addr, self.frame(addr, caret)).await;
    }

    /// Say this agent is on the canvas in `addr`.
    pub async fn say_on_board(&self, client: &PimbleClient, addr: Addr, board: BoardPresence) {
        self.send(client, addr, self.board_frame(addr, board)).await;
    }

    async fn send(&self, client: &PimbleClient, addr: Addr, frame: PresenceFrame) {
        if let Err(e) = client.publish_presence(frame, &self.client_id).await {
            tracing::debug!("presence in {} not sent: {e}", addr.link());
        }
    }
}
