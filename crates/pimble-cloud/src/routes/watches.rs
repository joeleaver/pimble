//! Watches (docs/NOTIFICATIONS_CONTRACT.md section 3, wave 3): what a
//! person chose to watch, the documents their devices registered from it,
//! and the writes an owner's relay saw.
//!
//! - `GET /api/v1/me/watches`: `{ choices: { version, sealed? }, stores:
//!   [{ store_id, docs }] }`. The choices are the person's own account data,
//!   sealed under a key only their devices derive; the service never opens
//!   them. The documents are what it evaluates writes against.
//! - `PUT /api/v1/me/watches/choices { version, sealed }`: replace the
//!   choices if they are still at `version` (409 otherwise: fetch, change
//!   again, retry). Answers `{ version }`, the new one. Refused while watched
//!   notes are off.
//!   The version never goes back: turning watched notes off empties the
//!   choices and moves it on, so a device holding older ones gets 409.
//! - `PUT /api/v1/me/watches/stores/{store} { docs, version }`: every
//!   document watched in one store, computed from the choices at `version`
//!   (409 when they moved on since). The caller must hold a grant there.
//!   The hosted server is told again which documents are watched in it.
//! - `POST /api/v1/watches/writes { store_id, report, writes: [{ doc,
//!   writer, count, roots, last_ms }] }`: the writes an owner's relay face
//!   saw others make, each with the shared roots that reach its document and
//!   when it was last written. Only the owner of a relayed store may; a
//!   hosted store's writes come from the hosted server itself
//!   (`crate::watches`). A write whose writer holds no grant reaching its
//!   document (the whole store, or a share whose root is among the roots
//!   reaching it) is refused and not gathered (`refused` counts them).
//!   `report` is the batch's id, the same each time the owner's device
//!   sends it: a report taken already is answered `{ again: true }` and
//!   gathers nothing, so a report sent again because its answer was lost is
//!   not told twice.

use std::collections::{BTreeMap, BTreeSet};

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::db::GrantRow;
use crate::error::{CloudError, CloudResult};
use crate::session::AuthedUser;
use crate::state::AppState;
use crate::watches::Write;

/// The sealed choices can be no larger than this.
const MAX_CHOICES: usize = 256 * 1024;
/// Documents registered in one store, at most.
const MAX_DOCS_PER_STORE: usize = 20_000;
/// Writes in one report, at most.
const MAX_WRITES_PER_REPORT: usize = 1_000;

pub const WATCHES_OFF: &str = "watched notes are off for this account";

#[derive(Serialize, Deserialize)]
pub struct ChoicesBody {
    pub version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sealed: Option<String>,
}

pub async fn get_watches(State(state): State<AppState>, authed: AuthedUser) -> CloudResult<Json<Value>> {
    let user = &authed.user.user_uuid;
    let choices = match state.db.get_watch_choices(user).await? {
        Some((_, version, sealed)) => ChoicesBody { version, sealed: (!sealed.is_empty()).then_some(sealed) },
        None => ChoicesBody { version: 0, sealed: None },
    };
    let mut stores: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for doc in state.db.watched_docs_of(user).await? {
        stores.entry(doc.store_uuid).or_default().push(doc.doc_uuid);
    }
    let stores: Vec<Value> = stores.into_iter().map(|(store_id, mut docs)| {
        docs.sort();
        json!({ "store_id": store_id, "docs": docs })
    }).collect();
    Ok(Json(json!({ "choices": choices, "stores": stores })))
}

#[derive(Deserialize)]
pub struct PutChoices {
    pub version: u64,
    pub sealed: String,
}

pub async fn put_choices(State(state): State<AppState>, authed: AuthedUser, Json(body): Json<PutChoices>) -> CloudResult<Json<Value>> {
    let user = &authed.user;
    if user.notifications_off || user.notify_watches_off {
        return Err(CloudError::Forbidden(WATCHES_OFF.into()));
    }
    if body.sealed.len() > MAX_CHOICES {
        return Err(CloudError::BadRequest("the watches are too large".into()));
    }
    let _one_at_a_time = state.watches_lock.lock().await;
    let held = state.db.get_watch_choices(&user.user_uuid).await?;
    let held_version = held.as_ref().map(|(_, v, _)| *v).unwrap_or(0);
    if held_version != body.version {
        return Err(CloudError::Conflict("the watches changed; fetch them and change them again".into()));
    }
    let version = held_version + 1;
    state.db.put_watch_choices(&user.user_uuid, held.map(|(rid, _, _)| rid), version, &body.sealed).await?;
    Ok(Json(json!({ "version": version })))
}

#[derive(Deserialize)]
pub struct PutDocs {
    pub docs: Vec<String>,
    pub version: u64,
}

pub async fn put_store_docs(State(state): State<AppState>, authed: AuthedUser, Path(store_id): Path<String>, Json(body): Json<PutDocs>) -> CloudResult<Json<Value>> {
    let user = &authed.user;
    if user.notifications_off || user.notify_watches_off {
        return Err(CloudError::Forbidden(WATCHES_OFF.into()));
    }
    if body.docs.len() > MAX_DOCS_PER_STORE {
        return Err(CloudError::BadRequest("too many documents watched in one store".into()));
    }
    let mut wanted = BTreeSet::new();
    for doc in &body.docs {
        let id = uuid::Uuid::parse_str(doc).map_err(|_| CloudError::BadRequest(format!("{doc} is not a document id")))?;
        wanted.insert(id.to_string());
    }
    let store = super::stores::require_live_store(&state, &store_id).await?;
    let store_uuid = store.store_id.clone();
    if state.db.grants_for_user_and_store(user.rid, store.rid).await?.is_empty() {
        return Err(CloudError::Forbidden("not a member of this store".into()));
    }
    let _one_at_a_time = state.watches_lock.lock().await;
    let held_version = state.db.get_watch_choices(&user.user_uuid).await?.map(|(_, v, _)| v).unwrap_or(0);
    if held_version != body.version {
        return Err(CloudError::Conflict("the watches changed since these documents were computed".into()));
    }
    let mut held = BTreeMap::new();
    for doc in state.db.watched_docs_of(&user.user_uuid).await?.into_iter().filter(|d| d.store_uuid == store_uuid) {
        if held.contains_key(&doc.doc_uuid) {
            // A duplicate row: one is enough.
            state.db.delete_watched_doc(doc.rid).await?;
        } else {
            held.insert(doc.doc_uuid.clone(), doc.rid);
        }
    }
    for (doc, rid) in &held {
        if !wanted.contains(doc) {
            state.db.delete_watched_doc(*rid).await?;
        }
    }
    for doc in &wanted {
        if !held.contains_key(doc) {
            state.db.create_watched_doc(&user.user_uuid, &store_uuid, doc).await?;
        }
    }
    state.watch_interest.changed([store_uuid]);
    Ok(Json(json!({ "docs": wanted.len() })))
}

#[derive(Deserialize)]
pub struct PostWrites {
    pub store_id: String,
    /// The batch's id, the same each time it is sent.
    pub report: String,
    pub writes: Vec<Write>,
}

pub async fn post_relay_writes(State(state): State<AppState>, authed: AuthedUser, Json(body): Json<PostWrites>) -> CloudResult<Json<Value>> {
    if body.writes.len() > MAX_WRITES_PER_REPORT {
        return Err(CloudError::BadRequest("too many writes in one report".into()));
    }
    let report = uuid::Uuid::parse_str(&body.report).map_err(|_| CloudError::BadRequest("a report is named by an id".into()))?.to_string();
    let store = super::stores::require_live_store(&state, &body.store_id).await?;
    if !store.is_relayed() {
        return Err(CloudError::BadRequest("a hosted store's writes are told by the hosted server".into()));
    }
    if !super::stores::is_store_owner(&state, &store, authed.user.rid).await? {
        return Err(CloudError::Forbidden("only the store's owner reports its relay's writes".into()));
    }
    for write in &body.writes {
        let ids = std::iter::once(&write.doc).chain(std::iter::once(&write.writer)).chain(&write.roots);
        if ids.map(|id| uuid::Uuid::parse_str(id)).any(|parsed| parsed.is_err()) {
            return Err(CloudError::BadRequest("a write names a document, a writer and the roots reaching it by id".into()));
        }
    }
    // One report at a time, so the same one sent twice at once is taken once.
    let _one_at_a_time = state.relay_reports_lock.lock().await;
    if state.db.relay_report_taken(&report).await? {
        return Ok(Json(json!({ "gathered": 0, "refused": 0, "again": true })));
    }
    // The owner names the writer, so the writer must be someone the store
    // was shared with, through a grant that reaches the document written:
    // nobody else's name goes on a notice, and a member is never named for a
    // write outside what they hold.
    let mut grants_of: BTreeMap<String, Vec<GrantRow>> = BTreeMap::new();
    let mut taken = Vec::with_capacity(body.writes.len());
    let mut refused = 0usize;
    for write in body.writes {
        let writer = uuid::Uuid::parse_str(&write.writer).map(|u| u.to_string()).unwrap_or_default();
        if !grants_of.contains_key(&writer) {
            let grants = match state.db.find_user_by_uuid(&writer).await? {
                Some(user) => state.db.grants_for_user_and_store(user.rid, store.rid).await?,
                None => Vec::new(),
            };
            grants_of.insert(writer.clone(), grants);
        }
        let reached: BTreeSet<String> = write.roots.iter().map(|r| crate::watches::canonical(r)).collect();
        if crate::watches::told_through(&grants_of[&writer], &reached).is_some() {
            taken.push(crate::watches::Write { writer, ..write });
        } else {
            refused += 1;
        }
    }
    let now = chrono::Utc::now().timestamp_millis();
    let gathered = crate::watches::record_writes(&state, &store.store_id, &taken, now).await?;
    // Remembered once gathered: a report that failed half way is gathered
    // again when it is sent again, and a write gathered twice at the same
    // moment adds no spell of editing.
    state.db.remember_relay_report(&report, &store.store_id, now).await?;
    Ok(Json(json!({ "gathered": gathered, "refused": refused })))
}
