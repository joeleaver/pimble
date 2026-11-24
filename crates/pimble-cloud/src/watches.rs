//! Watches, evaluated here (docs/NOTIFICATIONS_CONTRACT.md section 3, wave
//! 3).
//!
//! **Writes in.** The hosted Pimble server tells this service every write a
//! signed-in person makes there (`subscribeWrites`: which store and
//! document, which account, how many, gathered over a couple of seconds),
//! and an owner's device posts what its relay face saw
//! (`POST /api/v1/watches/writes`). Neither says anything of what was
//! written. [`record_writes`] gathers each one for every person watching
//! that document other than its writer, who is still in the store and has
//! watched notes on: one `WatchBatch` per (watcher, document, writer), kept
//! in the database so a deploy loses none.
//!
//! **Notices out.** [`watch_pass`] queues one notice for each batch that is
//! due (`pimble_core::watch::Window::NOTICE`: five quiet minutes, or an hour
//! after the first write), sealed to the watcher's notice key like any
//! notice, with no title or text: the store, the document, the writer and
//! how many changes. The watcher's devices fill in the rest from the notes
//! they hold. Its push and email say "Joe made 3 changes in a note you
//! watch in Garden". [`spawn`] runs the pass and follows the hosted server
//! for the life of the process; tests call [`watch_pass`] with a clock of
//! their own.

use std::time::Duration;

use pimble_core::watch::Window;
use pimble_core::{AuthMethod, NodeId, StoreId};
use pimble_rpc::{Notice, NoticeKind};

use crate::db::{InboxItemRow, UserRow, WatchBatchRow};
use crate::error::CloudResult;
use crate::state::AppState;

/// How often [`watch_pass`] looks for batches that are due.
pub const WATCH_PASS_EVERY: Duration = Duration::from_secs(30);

/// One write reported, of `count` appends or edits.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Write {
    pub doc: String,
    pub writer: String,
    #[serde(default = "one")]
    pub count: u64,
}

fn one() -> u64 {
    1
}

/// The scope a watcher holds `store_rid` through: `""` for a whole-store
/// grant, else a shared root; `None` when they hold nothing there any more.
async fn scope_of(state: &AppState, watcher: &UserRow, store_rid: u64) -> CloudResult<Option<String>> {
    let grants = state.db.grants_for_user_and_store(watcher.rid, store_rid).await?;
    Ok(grants.iter().find(|g| g.is_whole_store()).or_else(|| grants.first()).map(|g| g.root.clone()))
}

fn watching(user: &UserRow) -> bool {
    !user.notifications_off && !user.notify_watches_off
}

/// Gather `writes` to `store_uuid` at `now_ms` for everyone watching them.
/// Answers how many (watcher, write) pairs were gathered.
pub async fn record_writes(state: &AppState, store_uuid: &str, writes: &[Write], now_ms: i64) -> CloudResult<usize> {
    let Some(store) = state.db.find_hosted_store(store_uuid).await? else { return Ok(0) };
    if store.deleted {
        return Ok(0);
    }
    let mut gathered = 0;
    for write in writes {
        for watched in state.db.watchers_of(store_uuid, &write.doc).await? {
            if watched.user_uuid == write.writer {
                continue;
            }
            let Some(watcher) = state.db.find_user_by_uuid(&watched.user_uuid).await? else { continue };
            if !watching(&watcher) || scope_of(state, &watcher, store.rid).await?.is_none() {
                continue;
            }
            state.db.gather_watch_write(&watcher.user_uuid, store_uuid, &write.doc, &write.writer, write.count, now_ms).await?;
            gathered += 1;
        }
    }
    Ok(gathered)
}

/// One look over every batch as of `now_ms`: each due one becomes a notice
/// in its watcher's inbox (or nothing, when the watcher turned watches off,
/// left the store or has no notice key yet) and is gone. Answers how many
/// notices were queued.
pub async fn watch_pass(state: &AppState, now_ms: i64) -> CloudResult<usize> {
    let mut queued = 0;
    for batch in state.db.watch_batches().await? {
        if !batch.gathered().is_due(Window::NOTICE, now_ms) {
            continue;
        }
        // Gone before it is told, so a pass that fails half way never tells
        // it twice.
        state.db.delete_watch_batch(batch.rid).await?;
        if queue_notice(state, &batch).await? {
            queued += 1;
        }
    }
    Ok(queued)
}

async fn queue_notice(state: &AppState, batch: &WatchBatchRow) -> CloudResult<bool> {
    let Some(watcher) = state.db.find_user_by_uuid(&batch.watcher_uuid).await? else { return Ok(false) };
    if !watching(&watcher) || watcher.notice_public_key.is_empty() {
        return Ok(false);
    }
    let Some(store) = state.db.find_hosted_store(&batch.store_uuid).await? else { return Ok(false) };
    if store.deleted {
        return Ok(false);
    }
    let Some(root) = scope_of(state, &watcher, store.rid).await? else { return Ok(false) };
    let (Ok(store_id), Ok(node)) = (StoreId::parse(&batch.store_uuid), NodeId::parse(&batch.doc_uuid)) else { return Ok(false) };
    let item_uuid = uuid::Uuid::new_v4().to_string();
    // The names as the watcher knows them: the writer's in the watcher's
    // share, and the share's.
    let mut item = InboxItemRow {
        rid: 0,
        item_uuid: item_uuid.clone(),
        recipient_uuid: watcher.user_uuid.clone(),
        sender_uuid: batch.writer_uuid.clone(),
        store_uuid: batch.store_uuid.clone(),
        root: root.clone(),
        sealed: String::new(),
        created_at_ms: batch.last_at_ms,
        delivered: false,
        delivered_at_ms: 0,
        kind: NoticeKind::Watch.word().to_string(),
        emailed: false,
        count: batch.count,
    };
    let (from_name, _share) = crate::routes::inbox::names_for(state, &item).await?;
    let notice = Notice {
        id: item_uuid.clone(),
        kind: NoticeKind::Watch,
        from: batch.writer_uuid.clone(),
        from_name,
        store: store_id,
        node,
        discussion: None,
        anchor: None,
        snippet: String::new(),
        at: chrono::DateTime::from_timestamp_millis(batch.last_at_ms).unwrap_or_default().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        count: u32::try_from(batch.count).unwrap_or(u32::MAX),
    };
    let plain = serde_json::to_vec(&notice).map_err(|e| crate::error::CloudError::Internal(e.to_string()))?;
    let sealed = match pimble_crypto::seal_notice(&plain, &watcher.notice_public_key, &watcher.user_uuid) {
        Ok(sealed) => sealed,
        Err(e) => {
            tracing::warn!(error = %e, "a watch notice could not be sealed to its watcher's notice key");
            return Ok(false);
        }
    };
    item = state.db.create_watch_item_at(&item_uuid, &watcher.user_uuid, &batch.writer_uuid, &batch.store_uuid, &root, &sealed, batch.count, chrono::Utc::now().timestamp_millis()).await?;
    state.wake_inbox(&watcher.user_uuid);
    if state.vapid.is_some() && !watcher.notify_push_off {
        let state = state.clone();
        tokio::spawn(async move {
            crate::notify::push_item(&state, &watcher, &item).await;
        });
    }
    Ok(true)
}

/// Follow the hosted server's writes for as long as it runs, connecting
/// again (with backoff) whenever the connection ends. A write made while
/// this service is not connected is not told: watches are best effort.
async fn follow_hosted_writes(state: AppState) {
    let auth = match &state.config.pimble_server_token {
        Some(token) => AuthMethod::Bearer { token: token.clone() },
        None => AuthMethod::None,
    };
    let mut backoff = Duration::from_secs(1);
    loop {
        match pimble_client::PimbleClient::connect_with_auth(&state.config.pimble_server_url, &auth).await {
            Ok(client) => match client.subscribe_writes().await {
                Ok(mut writes) => {
                    backoff = Duration::from_secs(1);
                    while let Some(Ok(seen)) = writes.next().await {
                        let write = Write { doc: seen.doc_id.to_string(), writer: seen.writer, count: u64::from(seen.count) };
                        if let Err(e) = record_writes(&state, &seen.store_id.to_string(), &[write], chrono::Utc::now().timestamp_millis()).await {
                            tracing::warn!(error = %e, "a write was not gathered for watches");
                        }
                    }
                    tracing::info!("the hosted server's writes ended; following them again");
                }
                Err(e) => tracing::warn!(error = %e, "the hosted server's writes could not be followed"),
            },
            Err(e) => tracing::debug!(error = %e, "the hosted server is not reachable for its writes"),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

/// Watches' jobs, for the life of the process: following the hosted
/// server's writes, and [`watch_pass`] every [`WATCH_PASS_EVERY`]. Started
/// by `build_router` (the service), never by the tests.
pub fn spawn(state: AppState) {
    tokio::spawn(follow_hosted_writes(state.clone()));
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(WATCH_PASS_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if let Err(e) = watch_pass(&state, chrono::Utc::now().timestamp_millis()).await {
                tracing::warn!(error = %e, "the watch pass failed");
            }
        }
    });
}
