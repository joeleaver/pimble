//! Notices (docs/NOTIFICATIONS_CONTRACT.md sections 2, 4 and 6): the
//! account's notice key, the switch, and the inbox of sealed notices.
//!
//! - `PUT /api/v1/me/notice-key { public_key, envelope }`: the account's
//!   notice key, made by its first signed-in device. The envelope must be
//!   the account's own (signed by it, sealed to it, for this account's notice
//!   key). A second, different key is refused (409): the device that lost
//!   the race fetches the first one from `GET /me/keys`.
//! - `PUT /api/v1/me/notifications { on }`: the switch. Off drops everything
//!   queued for the account and refuses new notices to it.
//! - `POST /api/v1/inbox/{recipient} { store_id, root, sealed }`: a sender
//!   queues a notice. Both accounts must reach that share (the store whole,
//!   or that root); a sender may queue [`SENDS_PER_WINDOW`] per
//!   [`SEND_WINDOW`].
//! - `GET /api/v1/inbox?after=&wait=`: the caller's items after the cursor
//!   (a row id; `0` for everything), waiting up to `wait` seconds (at most
//!   [`MAX_WAIT`]) for one when there are none: a running device's
//!   long poll. Undelivered items older than a day are dropped first, and
//!   delivered ones seven days after their first delivery.
//! - `POST /api/v1/inbox/ack { ids }`: the caller's items, read; deleted.
//! - `DELETE /api/v1/inbox`: everything queued for the caller.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::db::InboxItemRow;
use crate::error::{CloudError, CloudResult};
use crate::session::AuthedUser;
use crate::state::AppState;

use super::stores::{normalize_root, require_live_store};

/// "a notice older than a day is dropped rather than delivered late".
pub const UNDELIVERED_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// "the service deletes delivered items after seven days".
pub const DELIVERED_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// The longest a drain waits for an item.
pub const MAX_WAIT: Duration = Duration::from_secs(25);
/// A sealed notice is small: a snippet and a few ids.
const MAX_SEALED_LEN: usize = 16 * 1024;
/// How many notices one sender may queue in a window, across recipients.
pub const SENDS_PER_WINDOW: usize = 60;
pub const SEND_WINDOW: Duration = Duration::from_secs(60);

// ── The notice key ────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct PutNoticeKey {
    pub public_key: String,
    pub envelope: pimble_crypto::KeyEnvelope,
}

pub async fn put_notice_key(State(state): State<AppState>, authed: AuthedUser, Json(body): Json<PutNoticeKey>) -> CloudResult<Json<Value>> {
    let user = &authed.user;
    let public = user.public_keys().ok_or_else(|| CloudError::Internal("an account with no keys".into()))?;
    let decoded = base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, body.public_key.trim()).map_err(|_| CloudError::BadRequest("public_key is not base64url".into()))?;
    if decoded.len() != 32 {
        return Err(CloudError::BadRequest("public_key is not an X25519 key".into()));
    }
    if body.envelope.context != pimble_crypto::notice_key_context(&user.user_uuid) || body.envelope.recipient != public.encryption {
        return Err(CloudError::BadRequest("the envelope is not this account's notice key".into()));
    }
    pimble_crypto::verify_envelope(&body.envelope, &public.signing).map_err(|_| CloudError::BadRequest("the envelope's signature is not this account's".into()))?;
    if !user.notice_public_key.is_empty() {
        if user.notice_public_key == body.public_key.trim() {
            return Ok(Json(json!({ "public_key": user.notice_public_key })));
        }
        return Err(CloudError::Conflict("this account has a notice key already".into()));
    }
    let envelope = serde_json::to_string(&body.envelope).map_err(|e| CloudError::Internal(e.to_string()))?;
    state.db.set_notice_key(user.rid, body.public_key.trim(), &envelope).await?;
    Ok(Json(json!({ "public_key": body.public_key.trim() })))
}

// ── The switch ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct PutNotifications {
    pub on: bool,
}

pub async fn put_notifications(State(state): State<AppState>, authed: AuthedUser, Json(body): Json<PutNotifications>) -> CloudResult<Json<Value>> {
    state.db.set_notifications_off(authed.user.rid, !body.on).await?;
    if !body.on {
        for item in state.db.inbox_of(&authed.user.user_uuid).await? {
            state.db.delete_inbox_item(item.rid).await?;
        }
    }
    Ok(Json(json!({ "on": body.on })))
}

// ── Sending ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct PostNotice {
    pub store_id: String,
    /// The share's node, or empty (absent) for the whole store.
    #[serde(default)]
    pub root: Option<String>,
    pub sealed: String,
}

/// Whether `user_rid` reaches the scope `root` of the store: a whole-store
/// grant reaches every scope, a scoped grant its own.
async fn reaches(state: &AppState, user_rid: u64, store_rid: u64, root: &str) -> CloudResult<bool> {
    Ok(state.db.grants_for_user_and_store(user_rid, store_rid).await?.iter().any(|g| g.is_whole_store() || (!root.is_empty() && g.root == root)))
}

pub async fn post_notice(State(state): State<AppState>, authed: AuthedUser, Path(recipient): Path<String>, Json(body): Json<PostNotice>) -> CloudResult<Json<Value>> {
    if body.sealed.is_empty() || body.sealed.len() > MAX_SEALED_LEN {
        return Err(CloudError::BadRequest("a sealed notice is between 1 byte and 16 KiB".into()));
    }
    let store = require_live_store(&state, &body.store_id).await?;
    let root = normalize_root(body.root.as_deref().unwrap_or(""))?;
    let to = state.db.find_user_by_uuid(&recipient).await?.ok_or_else(|| CloudError::NotFound("no such account".into()))?;
    if to.user_uuid == authed.user.user_uuid {
        return Err(CloudError::BadRequest("nobody is notified of their own words".into()));
    }
    // Nobody outside a share can reach its members' inboxes, and nobody can
    // notify a person through a share that person is not in.
    if !reaches(&state, authed.user.rid, store.rid, &root).await? || !reaches(&state, to.rid, store.rid, &root).await? {
        return Err(CloudError::Forbidden("the sender and the recipient are not both in this share".into()));
    }
    if !state.inbox_quota.try_acquire(&authed.user.user_uuid) {
        return Err(CloudError::RateLimited("too many notices; slow down".into()));
    }
    // Turned off: nothing is kept for them. Answered as done, so the sender
    // stops trying; `queued` says it was dropped.
    if to.notifications_off {
        return Ok(Json(json!({ "id": null, "queued": false })));
    }
    let item = state.db.create_inbox_item(&to.user_uuid, &authed.user.user_uuid, &store.store_id, &root, &body.sealed).await?;
    state.wake_inbox(&to.user_uuid);
    Ok(Json(json!({ "id": item.item_uuid, "queued": true })))
}

// ── Draining ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct DrainQuery {
    #[serde(default)]
    pub after: u64,
    #[serde(default)]
    pub wait: u64,
}

#[derive(Serialize)]
pub struct InboxItemView {
    pub id: String,
    /// Orders the caller's items: the next drain's `after`.
    pub seq: u64,
    /// The sender, as this service authenticated them.
    pub from: String,
    pub store_id: String,
    pub root: Option<String>,
    pub sealed: String,
    /// When it was queued, RFC 3339.
    pub at: String,
}

#[derive(Serialize)]
pub struct InboxResponse {
    pub items: Vec<InboxItemView>,
    /// The highest `seq` answered, or `after` when nothing was.
    pub cursor: u64,
}

/// Drop what has waited too long, and answer the rest after `after`.
async fn pending(state: &AppState, recipient: &str, after: u64) -> CloudResult<Vec<InboxItemRow>> {
    let now = chrono::Utc::now().timestamp_millis();
    let mut out = Vec::new();
    for item in state.db.inbox_of(recipient).await? {
        let stale = if item.delivered { now - item.delivered_at_ms > DELIVERED_TTL_MS } else { now - item.created_at_ms > UNDELIVERED_TTL_MS };
        if stale {
            state.db.delete_inbox_item(item.rid).await?;
        } else if item.rid > after {
            out.push(item);
        }
    }
    Ok(out)
}

pub async fn drain(State(state): State<AppState>, authed: AuthedUser, Query(query): Query<DrainQuery>) -> CloudResult<Json<InboxResponse>> {
    let me = authed.user.user_uuid.clone();
    let wait = Duration::from_secs(query.wait).min(MAX_WAIT);
    let wake = state.inbox_waker(&me);
    // Armed before looking, so an item queued between the look and the wait
    // still wakes it.
    let notified = wake.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    let mut items = pending(&state, &me, query.after).await?;
    if items.is_empty() && !wait.is_zero() {
        let _ = tokio::time::timeout(wait, notified).await;
        items = pending(&state, &me, query.after).await?;
    }
    for item in items.iter().filter(|i| !i.delivered) {
        state.db.mark_inbox_item_delivered(item.rid).await?;
    }
    let cursor = items.iter().map(|i| i.rid).max().unwrap_or(query.after);
    let items = items
        .into_iter()
        .map(|i| InboxItemView {
            id: i.item_uuid,
            seq: i.rid,
            from: i.sender_uuid,
            store_id: i.store_uuid,
            root: if i.root.is_empty() { None } else { Some(i.root) },
            sealed: i.sealed,
            at: chrono::DateTime::from_timestamp_millis(i.created_at_ms).unwrap_or_default().to_rfc3339(),
        })
        .collect();
    Ok(Json(InboxResponse { items, cursor }))
}

#[derive(Deserialize)]
pub struct AckBody {
    pub ids: Vec<String>,
}

pub async fn ack(State(state): State<AppState>, authed: AuthedUser, Json(body): Json<AckBody>) -> CloudResult<Json<Value>> {
    let wanted: std::collections::HashSet<&str> = body.ids.iter().map(String::as_str).collect();
    let mut removed = 0;
    for item in state.db.inbox_of(&authed.user.user_uuid).await? {
        if wanted.contains(item.item_uuid.as_str()) {
            state.db.delete_inbox_item(item.rid).await?;
            removed += 1;
        }
    }
    Ok(Json(json!({ "acknowledged": removed })))
}

pub async fn clear(State(state): State<AppState>, authed: AuthedUser) -> CloudResult<Json<Value>> {
    for item in state.db.inbox_of(&authed.user.user_uuid).await? {
        state.db.delete_inbox_item(item.rid).await?;
    }
    Ok(Json(json!({})))
}

/// One waker per recipient whose device is waiting (or was).
pub type InboxWakers = std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Notify>>>;
