//! The accounts-service stub's notices (docs/NOTIFICATIONS_CONTRACT.md):
//! notice keys, channels, push, watches and the inbox.

use super::*;

// ── Notices (docs/NOTIFICATIONS_CONTRACT.md), as the real service keeps them ──

pub async fn stub_put_notice_key(State(state): State<StubState>, headers: HeaderMap, Json(req): Json<serde_json::Value>) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut s = state.inner.lock().unwrap();
    let user_id = s.account_of(&headers).user_id.clone();
    let public = req["public_key"].as_str().unwrap_or_default().to_string();
    let envelope: KeyEnvelope = serde_json::from_value(req["envelope"].clone()).map_err(|_| StatusCode::BAD_REQUEST)?;
    let account = s.accounts.iter_mut().find(|a| a.user_id == user_id).unwrap();
    if envelope.recipient != account.account_public_keys.encryption || pimble_crypto::verify_envelope(&envelope, &account.account_public_keys.signing).is_err() {
        return Err(StatusCode::BAD_REQUEST);
    }
    match &account.notice_key {
        Some((held, _)) if *held != public => Err(StatusCode::CONFLICT),
        _ => {
            account.notice_key = Some((public.clone(), envelope));
            Ok(Json(json!({ "public_key": public })))
        }
    }
}

pub async fn stub_put_notifications(State(state): State<StubState>, headers: HeaderMap, Json(req): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let mut s = state.inner.lock().unwrap();
    let user_id = s.account_of(&headers).user_id.clone();
    let on = req["on"].as_bool();
    if let Some(a) = s.accounts.iter_mut().find(|a| a.user_id == user_id) {
        if let Some(on) = on {
            a.notifications_off = !on;
        }
        if let Some(email) = req["email"].as_bool() {
            a.email_off = !email;
        }
        if let Some(push) = req["push"].as_bool() {
            a.push_off = !push;
        }
    }
    if on == Some(false) {
        s.inbox.retain(|i| i.recipient != user_id);
    }
    Json(json!({ "on": on }))
}

pub async fn stub_post_push(State(state): State<StubState>, headers: HeaderMap, Json(req): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let mut s = state.inner.lock().unwrap();
    let user_id = s.account_of(&headers).user_id.clone();
    let endpoint = req["endpoint"].as_str().unwrap_or_default().to_string();
    s.push_subs.retain(|(_, e)| *e != endpoint);
    s.push_subs.push((user_id, endpoint));
    Json(json!({ "subscribed": true }))
}

pub async fn stub_delete_push(State(state): State<StubState>, headers: HeaderMap, Json(req): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let mut s = state.inner.lock().unwrap();
    let user_id = s.account_of(&headers).user_id.clone();
    let endpoint = req["endpoint"].as_str().unwrap_or_default().to_string();
    s.push_subs.retain(|(u, e)| !(*u == user_id && *e == endpoint));
    Json(json!({}))
}

/// `GET /api/v1/me/watches` (wave 3).
pub async fn stub_get_watches(State(state): State<StubState>, headers: HeaderMap) -> Json<serde_json::Value> {
    let s = state.inner.lock().unwrap();
    let user_id = s.account_of(&headers).user_id.clone();
    let choices = match s.watch_choices.get(&user_id) {
        Some((version, sealed)) => json!({ "version": version, "sealed": sealed }),
        None => json!({ "version": 0 }),
    };
    let stores: Vec<serde_json::Value> = s.watched.iter().filter(|((u, _), _)| *u == user_id).map(|((_, store), docs)| json!({ "store_id": store, "docs": docs })).collect();
    Json(json!({ "choices": choices, "stores": stores }))
}

/// `PUT /api/v1/me/watches/choices`: compare-and-swap on the version.
pub async fn stub_put_watch_choices(State(state): State<StubState>, headers: HeaderMap, Json(req): Json<serde_json::Value>) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut s = state.inner.lock().unwrap();
    let user_id = s.account_of(&headers).user_id.clone();
    let held = s.watch_choices.get(&user_id).map(|(v, _)| *v).unwrap_or(0);
    if req["version"].as_u64() != Some(held) {
        return Err(StatusCode::CONFLICT);
    }
    let sealed = req["sealed"].as_str().unwrap_or_default().to_string();
    s.watch_choices.insert(user_id, (held + 1, sealed));
    Ok(Json(json!({ "version": held + 1 })))
}

/// `PUT /api/v1/me/watches/stores/{store}`: the store's documents, whole,
/// from the choices at `version`.
pub async fn stub_put_watched_docs(State(state): State<StubState>, headers: HeaderMap, AxPath(store_id): AxPath<String>, Json(req): Json<serde_json::Value>) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut s = state.inner.lock().unwrap();
    let user_id = s.account_of(&headers).user_id.clone();
    let held = s.watch_choices.get(&user_id).map(|(v, _)| *v).unwrap_or(0);
    if req["version"].as_u64() != Some(held) {
        return Err(StatusCode::CONFLICT);
    }
    let docs: Vec<String> = req["docs"].as_array().map(|d| d.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
    s.watched.insert((user_id, store_id), docs);
    Ok(Json(json!({})))
}

impl StubInner {
    /// Whether `user_id` reaches the scope `root` of `store_id`.
    pub fn reaches(&self, user_id: &str, store_id: &str, root: &Option<String>) -> bool {
        self.stores.iter().any(|row| row.user_id == user_id && row.store_id == store_id && (row.root.is_none() || (root.is_some() && row.root == *root)))
    }
}

pub async fn stub_post_notice(
    State(state): State<StubState>,
    AxPath(recipient): AxPath<String>,
    headers: HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut s = state.inner.lock().unwrap();
    let from = s.account_of(&headers).user_id.clone();
    let store_id = req["store_id"].as_str().unwrap_or_default().to_string();
    let root = req["root"].as_str().map(str::to_string);
    if !s.reaches(&from, &store_id, &root) || !s.reaches(&recipient, &store_id, &root) {
        return Err(StatusCode::FORBIDDEN);
    }
    s.posted.push((from.clone(), recipient.clone()));
    s.kinds.push(req["kind"].as_str().unwrap_or_default().to_string());
    if s.accounts.iter().any(|a| a.user_id == recipient && a.notifications_off) {
        return Ok(Json(json!({ "id": null, "queued": false })));
    }
    s.next_seq += 1;
    let item = StubInboxItem { seq: s.next_seq, id: Uuid::new_v4().to_string(), recipient, from, store_id, root, sealed: req["sealed"].as_str().unwrap_or_default().to_string() };
    let id = item.id.clone();
    s.inbox.push(item);
    drop(s);
    state.inbox_wake.notify_waiters();
    Ok(Json(json!({ "id": id, "queued": true })))
}

fn stub_items_after(s: &StubInner, me: &str, after: u64) -> Vec<StubInboxItem> {
    s.inbox.iter().filter(|i| i.recipient == me && i.seq > after).cloned().collect()
}

pub async fn stub_drain_inbox(State(state): State<StubState>, headers: HeaderMap, Query(query): Query<HashMap<String, String>>) -> Json<serde_json::Value> {
    let after: u64 = query.get("after").and_then(|a| a.parse().ok()).unwrap_or(0);
    let wait: u64 = query.get("wait").and_then(|a| a.parse().ok()).unwrap_or(0).min(25);
    let me = state.inner.lock().unwrap().account_of(&headers).user_id.clone();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(wait);
    let items = loop {
        let notified = state.inbox_wake.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let items = stub_items_after(&state.inner.lock().unwrap(), &me, after);
        if !items.is_empty() || tokio::time::Instant::now() >= deadline {
            break items;
        }
        let _ = tokio::time::timeout_at(deadline, notified).await;
    };
    let cursor = items.iter().map(|i| i.seq).max().unwrap_or(after);
    let items: Vec<_> = items
        .iter()
        .map(|i| json!({ "id": i.id, "seq": i.seq, "from": i.from, "store_id": i.store_id, "root": i.root, "sealed": i.sealed, "at": chrono::Utc::now().to_rfc3339() }))
        .collect();
    Json(json!({ "items": items, "cursor": cursor }))
}

pub async fn stub_ack_inbox(State(state): State<StubState>, headers: HeaderMap, Json(req): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let mut s = state.inner.lock().unwrap();
    let me = s.account_of(&headers).user_id.clone();
    let ids: Vec<String> = req["ids"].as_array().cloned().unwrap_or_default().into_iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
    s.inbox.retain(|i| !(i.recipient == me && ids.contains(&i.id)));
    Json(json!({}))
}

pub async fn stub_clear_inbox(State(state): State<StubState>, headers: HeaderMap) -> Json<serde_json::Value> {
    let mut s = state.inner.lock().unwrap();
    let me = s.account_of(&headers).user_id.clone();
    s.inbox.retain(|i| i.recipient != me);
    Json(json!({}))
}
