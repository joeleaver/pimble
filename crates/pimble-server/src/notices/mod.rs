//! Notices (docs/NOTIFICATIONS_CONTRACT.md, wave 1): telling the people
//! you mention, and the people you answer, that you wrote to them; and
//! hearing it when someone writes to you.
//!
//! **Sending.** Every write a person or their agent makes on this server
//! passes `applyEdit` (or `createNode`/`createAttachment` with content), and
//! only those: an update arriving over a sync or vault link was written on
//! another device, which told whom it had to. The handler hands this module
//! the document before and after such a write ([`candidates`]); a mention
//! of an account added by it, or a new message in a discussion others wrote
//! in, becomes a notice. What was notified is remembered per (store, node,
//! mention or message, recipient) in this account's own file, so undo and
//! redo, a restart or the same edit arriving twice never notify again,
//! while the same mention pasted into another node does. A notice waits in
//! the outbox until it is sealed to the recipient's notice key and the
//! accounts service has queued it; one older than a day is dropped rather
//! than sent late.
//!
//! **Receiving.** While an account is signed in and notifications are on,
//! one task drains the account's inbox: at once, then a long poll (the
//! accounts service answers as soon as a notice is queued, or after
//! [`DRAIN_WAIT_SECS`]), so a running device hears within a moment. Each
//! item is opened with the notice key, checked against the sender the
//! service authenticated, kept here, and handed to every window listening
//! (`subscribeNotices`), which shows it. Opening one in the app acknowledges
//! it, and the service forgets it.
//!
//! **Watching** (section 3, wave 3, `watches`): the notes the person chose
//! to watch are their own account data, sealed for their devices; this
//! device registers the documents they reach with the accounts service as
//! opaque ids, and keeps that set in step as nodes come and go. The service
//! tells them of others' writes as notices of kind `watch`, which arrive
//! here like any other.
//!
//! Everything here is the signed-in account's own and is kept beside the
//! replicas (`<data dir>/pimble/notices/<account>.json`); nothing is sent
//! when nobody is signed in or notifications are off.


use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use pimble_core::StoreId;
use pimble_rpc::Notice;
use tracing::{debug, info, warn};

use crate::handler::RpcHandler;

mod detect;
mod kept;
mod run;
mod watches;

pub(crate) use run::chain_of;

pub(crate) use detect::{candidates, may_mention, Candidate};
use kept::{now_ms, Kept, Outgoing};

/// How long a drain waits for a notice before it asks again (under the
/// desktop's 15 s request timeout).
pub const DRAIN_WAIT_SECS: u64 = 10;
/// "a notice older than a day is dropped rather than delivered late".
const OUTBOX_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// About how much of the text a notice carries.
pub const SNIPPET_CHARS: usize = 120;
/// What was notified is remembered this long, and at most this many.
const SENT_KEPT_MS: i64 = 180 * 24 * 60 * 60 * 1000;
const SENT_MAX: usize = 20_000;
/// The notices kept for the Inbox: the newest this many, this young.
const HELD_MAX: usize = 300;
const HELD_KEPT_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// How often the outbox is tried again while something waits in it.
const RETRY_EVERY: Duration = Duration::from_secs(30);


// ── The module ────────────────────────────────────────────────────────────

/// This server's notices: one per [`RpcHandler`], `Arc`-shared.
pub struct Notices {
    dir: PathBuf,
    kept: tokio::sync::Mutex<Option<Kept>>,
    received: tokio::sync::broadcast::Sender<Notice>,
    work: Arc<tokio::sync::Notify>,
    task: std::sync::Mutex<Option<pimble_host::Task>>,
}

impl Notices {
    /// Kept under `dir` (`<data dir>/pimble/notices`).
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, kept: tokio::sync::Mutex::new(None), received: tokio::sync::broadcast::channel(64).0, work: Arc::new(tokio::sync::Notify::new()), task: std::sync::Mutex::new(None) }
    }

    /// Where a server whose replicas live in `replicas` keeps its notices.
    pub fn beside(replicas: &Path) -> PathBuf {
        replicas.parent().map(|p| p.join("notices")).unwrap_or_else(|| PathBuf::from("notices"))
    }

    fn file_of(&self, account: &str) -> PathBuf {
        let safe: String = account.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
        self.dir.join(format!("{safe}.json"))
    }

    /// Hear each notice as it arrives.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Notice> {
        self.received.subscribe()
    }

    /// Run `f` over what is kept for `account`, read from the disk the first
    /// time (another account's is put away first), and write it after when
    /// `f` says it changed something.
    async fn with<R>(&self, handler: &RpcHandler, account: &str, f: impl FnOnce(&mut Kept) -> (R, bool)) -> R {
        let mut guard = self.kept.lock().await;
        if guard.as_ref().is_none_or(|k| k.account != account) {
            let path = self.file_of(account);
            let read = match handler.disk().read_to_string(&path).await {
                Ok(json) => serde_json::from_str::<Kept>(&json).ok().filter(|k| k.account == account),
                Err(_) => None,
            };
            *guard = Some(read.unwrap_or_else(|| Kept::new(account)));
        }
        let kept = guard.as_mut().expect("loaded above");
        let (answer, changed) = f(kept);
        if changed {
            kept.prune(now_ms());
            let path = self.file_of(account);
            match serde_json::to_vec(&*kept) {
                Ok(bytes) => {
                    let _ = handler.disk().create_dir_all(&self.dir).await;
                    if let Err(e) = handler.disk().write_private_atomic(&path, &bytes).await {
                        warn!("Notices: could not write {}: {}", path.display(), e);
                    }
                }
                Err(e) => warn!("Notices: {}", e),
            }
        }
        answer
    }

    /// Start (or start again) the work for whoever is signed in now: after
    /// the server starts, a sign-in or out, and the switch.
    pub fn restart(&self, handler: &RpcHandler) {
        let task = handler.runtime().spawn(run::run(handler.clone()));
        if let Some(old) = self.task.lock().unwrap_or_else(|e| e.into_inner()).replace(task) {
            old.abort();
        }
    }

    pub fn stop(&self) {
        if let Some(old) = self.task.lock().unwrap_or_else(|e| e.into_inner()).take() {
            old.abort();
        }
    }

    /// The notices a write in `store_id` called for: kept and sent, each
    /// once ever. Nothing when nobody is signed in or notifications are off.
    pub(crate) async fn queue(&self, handler: &RpcHandler, store_id: StoreId, candidates: Vec<Candidate>) {
        let Some(account) = handler.keystore().account().await else { return };
        let me = account.user_id.clone();
        let now = now_ms();
        let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let queued = self
            .with(handler, &me, |kept| {
                if !kept.on {
                    return (0, false);
                }
                let mut n = 0;
                for c in candidates {
                    if c.recipient == me || c.recipient.starts_with(crate::agents::AGENT_SUB_PREFIX) {
                        continue;
                    }
                    let key = format!("{store_id}/{}/{}", c.key, c.recipient);
                    if kept.sent.contains_key(&key) {
                        continue;
                    }
                    kept.sent.insert(key, now);
                    let notice = Notice {
                        id: uuid::Uuid::new_v4().to_string(),
                        kind: c.kind,
                        from: me.clone(),
                        from_name: String::new(),
                        store: store_id,
                        node: c.node,
                        discussion: c.discussion,
                        anchor: c.anchor,
                        snippet: c.snippet,
                        at: at.clone(),
                        count: 0,
                    };
                    kept.outbox.push(Outgoing { recipient: c.recipient, notice, queued_ms: now });
                    n += 1;
                }
                (n, n > 0)
            })
            .await;
        if queued > 0 {
            debug!("Notices: {} queued for store {}", queued, store_id);
            self.work.notify_one();
        }
    }

    /// The notices kept for the Inbox, newest first, and whether they are on.
    pub(crate) async fn list(&self, handler: &RpcHandler) -> pimble_rpc::ListNoticesResponse {
        let Some(account) = handler.keystore().account().await else { return pimble_rpc::ListNoticesResponse::default() };
        self.with(handler, &account.user_id, |kept| {
            let notices = kept.held.iter().rev().map(|h| h.notice.clone()).collect();
            (pimble_rpc::ListNoticesResponse { notices, on: kept.on, email: kept.email, push: kept.push, push_key: kept.push_key.clone(), watches: kept.on && kept.watches_on }, false)
        })
        .await
    }

    /// The person opened these: the service is told, now or when it can be.
    pub(crate) async fn acknowledge(&self, handler: &RpcHandler, ids: &[String]) {
        let Some(account) = handler.keystore().account().await else { return };
        let wanted: HashSet<&str> = ids.iter().map(String::as_str).collect();
        let any = self
            .with(handler, &account.user_id, |kept| {
                let items: Vec<String> = kept.held.iter().filter(|h| wanted.contains(h.notice.id.as_str())).map(|h| h.item.clone()).collect();
                let mut changed = false;
                for item in items {
                    if !kept.acks.contains(&item) {
                        kept.acks.push(item);
                        changed = true;
                    }
                }
                (changed, changed)
            })
            .await;
        if any {
            self.work.notify_one();
        }
    }

    /// The switch, for the account: the service is told first (off deletes
    /// what is queued there), then this device stops or starts.
    pub(crate) async fn set_on(&self, handler: &RpcHandler, on: bool) -> Result<(), String> {
        let account = handler.keystore().account().await.ok_or("Nobody is signed in.")?;
        let switches = crate::cloud::Switches { on: Some(on), ..Default::default() };
        crate::cloud::put_notifications(handler.http(), &account.url, &account.session, &switches).await.map_err(|e| e.to_string())?;
        self.with(handler, &account.user_id, |kept| {
            kept.on = on;
            if !on {
                kept.outbox.clear();
                kept.acks.clear();
                // The service deleted the account's watches with the rest.
                kept.forget_watches();
            }
            ((), true)
        })
        .await;
        info!("Notifications turned {}", if on { "on" } else { "off" });
        self.restart(handler);
        Ok(())
    }

    /// Wave 2's switches, for the account: email, web push.
    pub(crate) async fn set_channels(&self, handler: &RpcHandler, email: Option<bool>, push: Option<bool>) -> Result<(), String> {
        if email.is_none() && push.is_none() {
            return Ok(());
        }
        let account = handler.keystore().account().await.ok_or("Nobody is signed in.")?;
        let switches = crate::cloud::Switches { email, push, ..Default::default() };
        crate::cloud::put_notifications(handler.http(), &account.url, &account.session, &switches).await.map_err(|e| e.to_string())?;
        self.with(handler, &account.user_id, |kept| {
            kept.email = email.unwrap_or(kept.email);
            kept.push = push.unwrap_or(kept.push);
            ((), true)
        })
        .await;
        Ok(())
    }

    /// A browser subscribed to web push: the service keeps the subscription
    /// under the account, and this device remembers it to take it back at
    /// sign-out.
    pub(crate) async fn register_push(&self, handler: &RpcHandler, endpoint: &str, p256dh: &str, auth: &str) -> Result<(), String> {
        let account = handler.keystore().account().await.ok_or("Nobody is signed in.")?;
        crate::cloud::push_subscribe(handler.http(), &account.url, &account.session, endpoint, p256dh, auth).await.map_err(|e| e.to_string())?;
        self.with(handler, &account.user_id, |kept| {
            if !kept.push_endpoints.iter().any(|e| e == endpoint) {
                kept.push_endpoints.push(endpoint.to_string());
            }
            ((), true)
        })
        .await;
        info!("Notices: this browser takes pushes");
        Ok(())
    }

    pub(crate) async fn unregister_push(&self, handler: &RpcHandler, endpoint: &str) -> Result<(), String> {
        let account = handler.keystore().account().await.ok_or("Nobody is signed in.")?;
        crate::cloud::push_unsubscribe(handler.http(), &account.url, &account.session, endpoint).await.map_err(|e| e.to_string())?;
        self.with(handler, &account.user_id, |kept| {
            kept.push_endpoints.retain(|e| e != endpoint);
            ((), true)
        })
        .await;
        Ok(())
    }

    /// The account is signing out of this device: every push subscription it
    /// registered is taken back first, while the session still works, so no
    /// push for that account reaches this browser afterwards. Best effort: a
    /// service out of reach keeps them until a push to them fails.
    pub(crate) async fn signing_out(&self, handler: &RpcHandler) {
        let Some(account) = handler.keystore().account().await else { return };
        let endpoints = self.with(handler, &account.user_id, |kept| (std::mem::take(&mut kept.push_endpoints), true)).await;
        for endpoint in endpoints {
            if let Err(e) = crate::cloud::push_unsubscribe(handler.http(), &account.url, &account.session, &endpoint).await {
                debug!("Notices: a push subscription was not taken back: {}", e);
            }
        }
    }
}
