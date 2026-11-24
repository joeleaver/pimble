//! The accounts service's notices and watches (docs/NOTIFICATIONS_CONTRACT.md):
//! the notice key, the switches, push subscriptions, the inbox, and the
//! watches an account registers.

use pimble_crypto::KeyEnvelope;
use pimble_host::http::{Http, Method};
use serde::{Deserialize, Serialize};

use super::{endpoint, finish, get, json_of, post, put, send, MeKeysResponse, Result};

/// `PUT /api/v1/me/notice-key`: `Ok(false)` when the account already has a
/// different one (another device made it first).
pub async fn put_notice_key(http: &dyn Http, base_url: &str, session: &str, public_key: &str, envelope: &KeyEnvelope) -> Result<bool> {
    let body = serde_json::json!({ "public_key": public_key, "envelope": envelope });
    match put::<_, serde_json::Value>(http, &endpoint(base_url, "/api/v1/me/notice-key"), session, &body).await {
        Ok(_) => Ok(true),
        Err(e) if e.status() == Some(409) => Ok(false),
        Err(e) => Err(e),
    }
}

/// The account's switches: everything (`on`), wave 2's channels (`email`,
/// `push`) and wave 3's watched notes (`watches`); `None` leaves one as it
/// is.
pub async fn put_notifications(http: &dyn Http, base_url: &str, session: &str, switches: &Switches) -> Result<()> {
    let mut body = serde_json::Map::new();
    for (name, value) in [("on", switches.on), ("email", switches.email), ("push", switches.push), ("watches", switches.watches)] {
        if let Some(value) = value {
            body.insert(name.into(), value.into());
        }
    }
    let _: serde_json::Value = put(http, &endpoint(base_url, "/api/v1/me/notifications"), session, &body).await?;
    Ok(())
}

/// The switches `put_notifications` sets; `None` leaves one as it is.
#[derive(Debug, Clone, Copy, Default)]
pub struct Switches {
    pub on: Option<bool>,
    pub email: Option<bool>,
    pub push: Option<bool>,
    pub watches: Option<bool>,
}

/// A browser's push subscription, kept by the accounts service (wave 2).
pub async fn push_subscribe(http: &dyn Http, base_url: &str, session: &str, endpoint_url: &str, p256dh: &str, auth: &str) -> Result<()> {
    let body = serde_json::json!({ "endpoint": endpoint_url, "keys": { "p256dh": p256dh, "auth": auth } });
    let _: serde_json::Value = post(http, &endpoint(base_url, "/api/v1/me/push"), Some(session), &body).await?;
    Ok(())
}

pub async fn push_unsubscribe(http: &dyn Http, base_url: &str, session: &str, endpoint_url: &str) -> Result<()> {
    let url = endpoint(base_url, "/api/v1/me/push");
    let _: serde_json::Value = finish(&url, send(http, Method::Delete, &url, Some(session), Some(json_of(&serde_json::json!({ "endpoint": endpoint_url })))).await?)?;
    Ok(())
}

/// What `POST /api/v1/inbox/{recipient}` answers: whether it was kept (it is
/// not when the recipient turned notifications off).
#[derive(Debug, Deserialize)]
pub struct PostedNotice {
    #[serde(default)]
    pub queued: bool,
}

/// `kind` (wave 2) is the one thing an email or a push says about the notice
/// besides who and where: `"mention"` or `"reply"` (a `"watch"` notice is
/// queued by the service itself, never posted).
pub async fn post_notice(http: &dyn Http, base_url: &str, session: &str, recipient: &str, store_id: &str, root: Option<&str>, sealed: &str, kind: &str) -> Result<PostedNotice> {
    let body = serde_json::json!({ "store_id": store_id, "root": root, "sealed": sealed, "kind": kind });
    post(http, &endpoint(base_url, &format!("/api/v1/inbox/{recipient}")), Some(session), &body).await
}

#[derive(Debug, Clone, Deserialize)]
pub struct InboxItem {
    pub id: String,
    #[allow(dead_code)]
    pub seq: u64,
    pub from: String,
    #[allow(dead_code)]
    pub store_id: String,
    #[allow(dead_code)]
    #[serde(default)]
    pub root: Option<String>,
    pub sealed: String,
    #[allow(dead_code)]
    pub at: String,
}

#[derive(Debug, Deserialize)]
pub struct InboxBody {
    pub items: Vec<InboxItem>,
    pub cursor: u64,
}

/// `GET /api/v1/inbox?after=&wait=`: waits up to `wait` seconds for an item.
pub async fn drain_inbox(http: &dyn Http, base_url: &str, session: &str, after: u64, wait: u64) -> Result<InboxBody> {
    get(http, &endpoint(base_url, &format!("/api/v1/inbox?after={after}&wait={wait}")), Some(session)).await
}

pub async fn ack_inbox(http: &dyn Http, base_url: &str, session: &str, ids: &[String]) -> Result<()> {
    let _: serde_json::Value = post(http, &endpoint(base_url, "/api/v1/inbox/ack"), Some(session), &serde_json::json!({ "ids": ids })).await?;
    Ok(())
}

pub async fn me_keys(http: &dyn Http, base_url: &str, session: &str) -> Result<MeKeysResponse> {
    get(http, &endpoint(base_url, "/api/v1/me/keys"), Some(session)).await
}


// ── Watches (docs/NOTIFICATIONS_CONTRACT.md section 3, wave 3) ──────────────

/// The watches the account chose, sealed under a key only its own devices
/// derive, with the version a change must name.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WatchChoices {
    /// 0 before any device wrote them.
    pub version: u64,
    #[serde(default)]
    pub sealed: Option<String>,
}

/// The document ids registered for one store.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WatchedStore {
    pub store_id: String,
    pub docs: Vec<String>,
}

/// `GET /api/v1/me/watches`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WatchesBody {
    pub choices: WatchChoices,
    #[serde(default)]
    pub stores: Vec<WatchedStore>,
}

pub async fn get_watches(http: &dyn Http, base_url: &str, session: &str) -> Result<WatchesBody> {
    get(http, &endpoint(base_url, "/api/v1/me/watches"), Some(session)).await
}

#[derive(Debug, Deserialize)]
struct Versioned {
    version: u64,
}

/// `PUT /api/v1/me/watches/choices`: replace the sealed choices, if they are
/// still at `version`. Answers the new version, or `None` when another
/// device changed them first (409).
pub async fn put_watch_choices(http: &dyn Http, base_url: &str, session: &str, version: u64, sealed: &str) -> Result<Option<u64>> {
    let body = serde_json::json!({ "version": version, "sealed": sealed });
    match put::<_, Versioned>(http, &endpoint(base_url, "/api/v1/me/watches/choices"), session, &body).await {
        Ok(v) => Ok(Some(v.version)),
        Err(e) if e.status() == Some(409) => Ok(None),
        Err(e) => Err(e),
    }
}

/// `PUT /api/v1/me/watches/stores/{store}`: the documents watched in one
/// store, all of them, computed from choices at `version`. `Ok(false)` when
/// the choices moved on since (409): read them again first.
pub async fn put_watched_docs(http: &dyn Http, base_url: &str, session: &str, store_id: &str, docs: &[String], version: u64) -> Result<bool> {
    let body = serde_json::json!({ "docs": docs, "version": version });
    match put::<_, serde_json::Value>(http, &endpoint(base_url, &format!("/api/v1/me/watches/stores/{store_id}")), session, &body).await {
        Ok(_) => Ok(true),
        Err(e) if e.status() == Some(409) => Ok(false),
        Err(e) => Err(e),
    }
}

/// One gathered write, as the relay's owner reports it.
#[derive(Debug, Clone, Serialize)]
pub struct ReportedWrite {
    pub doc: String,
    pub writer: String,
    pub count: u32,
}

/// `POST /api/v1/watches/writes`: what an owner's relay face saw others
/// write to the store it relays (only the owner of a relayed store may).
pub async fn post_relay_writes(http: &dyn Http, base_url: &str, session: &str, store_id: &str, writes: &[ReportedWrite]) -> Result<()> {
    let body = serde_json::json!({ "store_id": store_id, "writes": writes });
    let _: serde_json::Value = post(http, &endpoint(base_url, "/api/v1/watches/writes"), Some(session), &body).await?;
    Ok(())
}
