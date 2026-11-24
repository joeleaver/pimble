//! Notices and watches (docs/NOTIFICATIONS_CONTRACT.md): what one person's
//! device tells another, the switches, browsers' push subscriptions, and the
//! notes a person watches.

use pimble_core::{NodeId, StoreId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    /// Someone mentioned you, in a note's text or in a discussion.
    Mention,
    /// Someone wrote in a discussion you wrote in.
    Reply,
    /// Someone else changed a note you watch (section 3, wave 3): told by the
    /// accounts service, which knows only the document, the writer and how
    /// many changes; this device fills in the rest.
    Watch,
}

impl NoticeKind {
    /// The word the accounts service is told beside a sealed notice (wave
    /// 2), what its email and push say.
    pub fn word(self) -> &'static str {
        match self {
            NoticeKind::Mention => "mention",
            NoticeKind::Reply => "reply",
            NoticeKind::Watch => "watch",
        }
    }
}

/// What one person's device tells another that it wrote to them (section 2),
/// sealed to the recipient's notice key on the way, opened by the
/// recipient's own server. A watch notice is sealed by the accounts service
/// from what it knows: the store, the document, the writer, how many.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    /// Made by the sender; what read state names it by.
    pub id: String,
    pub kind: NoticeKind,
    /// The sender's account (for a watch notice, the writer's).
    pub from: String,
    /// The name the sender goes by in that share.
    pub from_name: String,
    pub store: StoreId,
    /// The node the mention is in, or the discussion's host (the discussion
    /// itself for a standalone one); for a watch notice, the node changed.
    pub node: NodeId,
    /// The discussion, for a mention or reply in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discussion: Option<NodeId>,
    /// Where it points: the mention's id in a note's text, the message's id
    /// in a discussion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    /// About 120 characters around the mention, or of the message; empty for
    /// a watch notice.
    pub snippet: String,
    /// When it was written, RFC 3339 (a watch notice: the last change).
    pub at: String,
    /// For a watch notice: how many changes it stands for.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub count: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// The notices this device has received and still keeps, newest first, and
/// whether notifications are on for the account.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListNoticesResponse {
    pub notices: Vec<Notice>,
    pub on: bool,
    /// Wave 2 (section 4): whether an item none of the account's devices saw
    /// within fifteen minutes is emailed.
    #[serde(default)]
    pub email: bool,
    /// Wave 2: whether the browsers the account subscribed are pushed.
    #[serde(default)]
    pub push: bool,
    /// Wave 2: the accounts service's VAPID public key (base64url), what a
    /// browser subscribes with; none when the service sends no pushes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_key: Option<String>,
    /// Wave 3 (section 6): whether changes to watched notes are told.
    #[serde(default)]
    pub watches: bool,
}

/// Notices the person opened: acknowledged to the accounts service, so it
/// keeps them no longer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcknowledgeNoticesRequest {
    pub ids: Vec<String>,
}

/// The switches (section 6), for the whole account; `None` leaves one as it
/// is.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetNotificationsRequest {
    /// Everything: off stops sending and receiving and deletes what is
    /// queued, and the registered watches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<bool>,
    /// Wave 2: the email fallback alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<bool>,
    /// Wave 2: web push alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<bool>,
    /// Wave 3: watched notes alone. Off deletes every watch the account
    /// chose and registered, and what was gathered for them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watches: Option<bool>,
}

/// A browser's push subscription (wave 2), as `PushSubscription.toJSON()`
/// gives it: handed to the accounts service under the signed-in account, and
/// remembered here so signing out takes it back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterPushRequest {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnregisterPushRequest {
    pub endpoint: String,
}

// ── Watches (section 3, wave 3) ─────────────────────────────────────────────

/// One watch the person chose: a node, alone or with everything under it.
/// The person's own account data, kept by the accounts service sealed under
/// a key only their own devices derive, so every device of theirs holds the
/// same list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WatchEntry {
    pub store_id: StoreId,
    pub node_id: NodeId,
    /// With its subtree: every node under it as nodes come and go.
    #[serde(default)]
    pub subtree: bool,
}

/// Watch a node (again, to change `subtree`). `Service`-only. Answers the
/// account's watches as they are now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchNodeRequest {
    pub store_id: StoreId,
    pub node_id: NodeId,
    #[serde(default)]
    pub subtree: bool,
}

/// Stop watching a node: the watch on it, or else the watch with its
/// subtree that reaches it from nearest above (what "Unwatch" on a watched
/// change means). `Service`-only. Answers the watches as they are now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnwatchNodeRequest {
    pub store_id: StoreId,
    pub node_id: NodeId,
}

/// The watches the signed-in account chose, and whether watched notes are
/// on.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListWatchesResponse {
    pub watches: Vec<WatchEntry>,
    #[serde(default)]
    pub on: bool,
}

/// A write a signed-in person (a token's `sub`) made to a node's document
/// on this server (section 3): what the accounts service gathers watches
/// from. `subscribeWrites` (`Service`-only) carries them, gathered over a
/// couple of seconds per (store, document, writer)
/// (`pimble_core::watch::Window::REPORT`), so someone typing is a handful of
/// these, not hundreds. Only a server that verifies tokens (the hosted
/// server, an owner's relay face) has any to tell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteSeen {
    pub store_id: StoreId,
    pub doc_id: NodeId,
    /// The writer's account (`sub`).
    pub writer: String,
    /// How many appends or edits.
    pub count: u32,
}
