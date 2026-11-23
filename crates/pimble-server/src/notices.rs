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
//! Everything here is the signed-in account's own and is kept beside the
//! replicas (`<data dir>/pimble/notices/<account>.json`); nothing is sent
//! when nobody is signed in or notifications are off.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use pimble_core::mention::Mention;
use pimble_core::{NodeId, StoreId};
use pimble_crdt::NodeDoc;
use pimble_rpc::{Notice, NoticeKind, StorePeople};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::handler::RpcHandler;
use crate::keystore::SignedInAccount;

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

/// The text a mention's href starts with, as its bytes appear in a yrs
/// update that writes one: a write without them adds no mention.
const MENTION_BYTES: &[u8] = pimble_core::mention::SCHEME.as_bytes();

/// Whether an update could add a mention (the cheap test before any
/// document is read): it carries a mention's href.
pub(crate) fn may_mention(update: &[u8]) -> bool {
    update.windows(MENTION_BYTES.len()).any(|w| w == MENTION_BYTES)
}

/// One notice a write calls for, before it is addressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub kind: NoticeKind,
    pub recipient: String,
    /// What makes this notice this one within the store: the node and the
    /// mention (or the discussion and the message). The recipient is added.
    pub key: String,
    /// The node it opens: where the mention is, a discussion's host.
    pub node: NodeId,
    pub discussion: Option<NodeId>,
    pub anchor: Option<String>,
    pub snippet: String,
}

/// The notices a write calls for: `before` and `after` are the whole
/// document of `node` around it (`before` `None` for a document the write
/// made). Mentions of the writer and replies to themselves are left in; the
/// caller knows the account and leaves them out.
pub(crate) fn candidates(node: NodeId, before: Option<&[u8]>, after: &[u8]) -> Vec<Candidate> {
    let Ok(after_doc) = NodeDoc::load(after) else { return Vec::new() };
    let before_doc = before.and_then(|b| NodeDoc::load(b).ok());
    let discussion = pimble_discussion::Discussion::of_node(&after_doc);
    if discussion.exists() {
        let host = after_doc.fields().ok().and_then(|f| f.attached_to).unwrap_or(node);
        let before_snap = before_doc.as_ref().map(|d| pimble_discussion::Discussion::of_node(d).read());
        return message_candidates(node, host, before_snap.as_ref(), &discussion.read());
    }
    let before_mentions = before_doc.as_ref().map(|d| d.mentions()).unwrap_or_default();
    let added = pimble_core::mention::added(&before_mentions, &after_doc.mentions(), None);
    if added.is_empty() {
        return Vec::new();
    }
    let text = after_doc.text();
    added
        .into_iter()
        .map(|m| Candidate {
            kind: NoticeKind::Mention,
            key: format!("{node}/{}", mention_key(&m)),
            recipient: m.account.clone(),
            node,
            discussion: None,
            snippet: snippet_around(&text, &m.words()),
            anchor: m.id,
        })
        .collect()
}

fn mention_key(m: &Mention) -> String {
    match &m.id {
        Some(id) => format!("m:{id}"),
        None => format!("a:{}", m.account),
    }
}

/// A discussion's: each new message's mentions, and a reply to everyone
/// else who wrote in it and is not mentioned; an edited message's added
/// mentions.
fn message_candidates(
    discussion: NodeId,
    host: NodeId,
    before: Option<&pimble_discussion::DiscussionSnapshot>,
    after: &pimble_discussion::DiscussionSnapshot,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    for message in after.messages.iter().filter(|m| m.deleted_at.is_none()) {
        let old = before.and_then(|b| b.message(&message.id));
        let author = message.author.as_ref().map(|a| a.id.as_str());
        let snippet = clip(&message.body.text(), SNIPPET_CHARS);
        let old_mentions = old.map(|o| o.body.mentions()).unwrap_or_default();
        let added = pimble_core::mention::added(&old_mentions, &message.body.mentions(), author);
        let mentioned: HashSet<String> = message.body.mentions().into_iter().map(|m| m.account).collect();
        for m in added {
            out.push(Candidate {
                kind: NoticeKind::Mention,
                key: format!("{discussion}/{}", mention_key(&m)),
                recipient: m.account.clone(),
                node: host,
                discussion: Some(discussion),
                anchor: Some(message.id.clone()),
                snippet: snippet.clone(),
            });
        }
        if old.is_some() {
            continue;
        }
        // A new message: everyone else who wrote in the thread hears of it,
        // unless it mentions them (then the mention says it).
        let mut told = HashSet::new();
        for other in after.messages.iter().filter(|o| o.id != message.id) {
            let Some(who) = other.author.as_ref() else { continue };
            if who.is_agent() || Some(who.id.as_str()) == author || mentioned.contains(&who.id) || !told.insert(who.id.clone()) {
                continue;
            }
            out.push(Candidate {
                kind: NoticeKind::Reply,
                key: format!("{discussion}/r:{}", message.id),
                recipient: who.id.clone(),
                node: host,
                discussion: Some(discussion),
                anchor: Some(message.id.clone()),
                snippet: snippet.clone(),
            });
        }
    }
    out
}

/// About [`SNIPPET_CHARS`] of `text` around the first `needle`, on one line,
/// with an ellipsis where it was cut.
pub(crate) fn snippet_around(text: &str, needle: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() <= SNIPPET_CHARS {
        return flat;
    }
    let at = flat.find(needle).map(|byte| flat[..byte].chars().count()).unwrap_or(0);
    let start = at.saturating_sub(SNIPPET_CHARS / 3).min(chars.len() - SNIPPET_CHARS);
    let end = (start + SNIPPET_CHARS).min(chars.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    out
}

fn clip(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut out: String = flat.chars().take(max).collect();
    out.push('…');
    out
}

// ── What is kept ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Outgoing {
    recipient: String,
    notice: Notice,
    queued_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Held {
    /// The accounts service's id of the item, what an acknowledgement names.
    item: String,
    notice: Notice,
    received_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Kept {
    account: String,
    #[serde(default = "yes")]
    on: bool,
    /// What was notified: key, when.
    #[serde(default)]
    sent: BTreeMap<String, i64>,
    #[serde(default)]
    outbox: Vec<Outgoing>,
    /// What arrived, oldest first.
    #[serde(default)]
    held: Vec<Held>,
    /// Items read here and not yet acknowledged to the service.
    #[serde(default)]
    acks: Vec<String>,
    /// Wave 2: the account's email and push switches, as the service last
    /// said them.
    #[serde(default = "yes")]
    email: bool,
    #[serde(default = "yes")]
    push: bool,
    /// Wave 2: the service's VAPID public key, none when it sends no pushes.
    #[serde(default)]
    push_key: Option<String>,
    /// Wave 2: the push subscriptions this device registered for the
    /// account, taken back when it signs out.
    #[serde(default)]
    push_endpoints: Vec<String>,
}

fn yes() -> bool {
    true
}

impl Kept {
    fn new(account: &str) -> Self {
        Self {
            account: account.to_string(),
            on: true,
            sent: BTreeMap::new(),
            outbox: Vec::new(),
            held: Vec::new(),
            acks: Vec::new(),
            email: true,
            push: true,
            push_key: None,
            push_endpoints: Vec::new(),
        }
    }

    fn prune(&mut self, now: i64) {
        self.sent.retain(|_, at| now - *at < SENT_KEPT_MS);
        if self.sent.len() > SENT_MAX {
            let mut ats: Vec<i64> = self.sent.values().copied().collect();
            ats.sort_unstable();
            let cut = ats[self.sent.len() - SENT_MAX];
            self.sent.retain(|_, at| *at >= cut);
        }
        self.outbox.retain(|o| now - o.queued_ms < OUTBOX_TTL_MS);
        self.held.retain(|h| now - h.received_ms < HELD_KEPT_MS);
        if self.held.len() > HELD_MAX {
            let drop = self.held.len() - HELD_MAX;
            self.held.drain(..drop);
        }
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The people of the share a node is in that name `recipient`, innermost
/// first: the scope and its root (`None` for the whole store).
fn scope_for<'a>(people: &'a StorePeople, chain: &[NodeId], recipient: &str) -> Option<&'a pimble_rpc::ScopePeople> {
    let depth = |root: Option<NodeId>| match root {
        Some(root) => chain.iter().position(|n| *n == root),
        None => Some(chain.len()),
    };
    let mut scopes: Vec<(usize, &pimble_rpc::ScopePeople)> = people.scopes.iter().filter_map(|s| depth(s.root).map(|d| (d, s))).collect();
    scopes.sort_by_key(|(d, _)| *d);
    scopes.into_iter().map(|(_, s)| s).find(|s| s.people.iter().any(|p| p.user_id == recipient))
}

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
        let task = handler.runtime().spawn(run(handler.clone()));
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
            (pimble_rpc::ListNoticesResponse { notices, on: kept.on, email: kept.email, push: kept.push, push_key: kept.push_key.clone() }, false)
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
        crate::cloud::put_notifications(handler.http(), &account.url, &account.session, Some(on), None, None).await.map_err(|e| e.to_string())?;
        self.with(handler, &account.user_id, |kept| {
            kept.on = on;
            if !on {
                kept.outbox.clear();
                kept.acks.clear();
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
        crate::cloud::put_notifications(handler.http(), &account.url, &account.session, None, email, push).await.map_err(|e| e.to_string())?;
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

    /// Send what waits: acknowledgements, then each notice in the outbox,
    /// addressed through the share its node is in and sealed to the
    /// recipient's notice key. One that cannot be addressed yet (the
    /// recipient has no notice key yet, the service cannot be reached)
    /// stays; one nobody can be told (the recipient left the share, the
    /// service refused it) goes.
    async fn flush(&self, handler: &RpcHandler, account: &SignedInAccount) {
        let me = account.user_id.as_str();
        let (acks, outbox) = self.with(handler, me, |kept| ((kept.acks.clone(), kept.outbox.clone()), false)).await;
        if !acks.is_empty() && crate::cloud::ack_inbox(handler.http(), &account.url, &account.session, &acks).await.is_ok() {
            self.with(handler, me, |kept| {
                kept.acks.retain(|a| !acks.contains(a));
                ((), true)
            })
            .await;
        }
        if outbox.is_empty() {
            return;
        }
        let mut done: Vec<String> = Vec::new();
        let mut refreshed: HashSet<StoreId> = HashSet::new();
        for out in outbox {
            let store_id = out.notice.store;
            let chain = chain_of(handler, store_id, out.notice.discussion.unwrap_or(out.notice.node)).await;
            let mut people = crate::people::cached(handler, store_id).await;
            let addressed = |people: &StorePeople| scope_for(people, &chain, &out.recipient).and_then(|s| s.people.iter().find(|p| p.user_id == out.recipient).and_then(|p| p.notice_key.clone()).map(|k| (s.clone(), k)));
            let mut target = addressed(&people);
            if target.is_none() && refreshed.insert(store_id) {
                people = crate::people::refresh(handler, store_id).await;
                target = addressed(&people);
            }
            let Some((scope, notice_key)) = target else {
                if scope_for(&people, &chain, &out.recipient).is_none() && refreshed.contains(&store_id) && !people.scopes.is_empty() {
                    debug!("Notices: {} cannot open that note; nobody is told", out.recipient);
                    done.push(out.notice.id.clone());
                }
                continue;
            };
            let mut notice = out.notice.clone();
            notice.from_name = if scope.my_name.trim().is_empty() { account.email.split('@').next().unwrap_or("someone").to_string() } else { scope.my_name.clone() };
            let Ok(plain) = serde_json::to_vec(&notice) else { continue };
            let sealed = match pimble_crypto::seal_notice(&plain, &notice_key, &out.recipient) {
                Ok(sealed) => sealed,
                Err(e) => {
                    warn!("Notices: could not seal a notice to {}: {}", out.recipient, e);
                    done.push(out.notice.id.clone());
                    continue;
                }
            };
            let root = scope.root.map(|r| r.to_string());
            let kind = match notice.kind {
                NoticeKind::Mention => "mention",
                NoticeKind::Reply => "reply",
            };
            match crate::cloud::post_notice(handler.http(), &account.url, &account.session, &out.recipient, &store_id.to_string(), root.as_deref(), &sealed, kind).await {
                Ok(_) => done.push(out.notice.id.clone()),
                Err(e) => match e.status() {
                    Some(400 | 403 | 404) => {
                        debug!("Notices: the service refused a notice to {}: {}", out.recipient, e);
                        done.push(out.notice.id.clone());
                    }
                    Some(401) => {
                        handler.keystore().mark_session_expired();
                        break;
                    }
                    _ => {
                        debug!("Notices: not sent yet: {}", e);
                        break;
                    }
                },
            }
        }
        if !done.is_empty() {
            self.with(handler, me, |kept| {
                kept.outbox.retain(|o| !done.contains(&o.notice.id));
                ((), true)
            })
            .await;
        }
    }

    /// Take what a drain brought: open each item new here, keep it and tell
    /// every window. An item that does not open, or whose notice names
    /// another sender than the one the service authenticated, is
    /// acknowledged away unread.
    async fn take(&self, handler: &RpcHandler, account: &SignedInAccount, key: &pimble_crypto::NoticeKey, items: Vec<crate::cloud::InboxItem>) {
        let me = account.user_id.as_str();
        let mut opened: Vec<(String, Notice)> = Vec::new();
        let mut refused: Vec<String> = Vec::new();
        for item in items {
            let notice = pimble_crypto::open_notice(&item.sealed, key, me).ok().and_then(|plain| serde_json::from_slice::<Notice>(&plain).ok());
            match notice {
                Some(notice) if notice.from == item.from => opened.push((item.id, notice)),
                _ => {
                    warn!("Notices: an item from {} did not open as a notice from them; dropped", item.from);
                    refused.push(item.id);
                }
            }
        }
        let now = now_ms();
        let fresh = self
            .with(handler, me, |kept| {
                let mut fresh = Vec::new();
                for (item, notice) in opened {
                    if kept.held.iter().any(|h| h.item == item || h.notice.id == notice.id) {
                        continue;
                    }
                    kept.held.push(Held { item, notice: notice.clone(), received_ms: now });
                    fresh.push(notice);
                }
                kept.acks.extend(refused.iter().cloned());
                let changed = !fresh.is_empty() || !refused.is_empty();
                (fresh, changed)
            })
            .await;
        for notice in fresh {
            info!("Notices: a {:?} from {} arrived", notice.kind, notice.from_name);
            let _ = self.received.send(notice);
        }
        if !refused.is_empty() {
            self.work.notify_one();
        }
    }
}

/// The node and its ancestors, nearest first (a discussion's host stands
/// for its parent): what says which shares a node is in.
async fn chain_of(handler: &RpcHandler, store_id: StoreId, node: NodeId) -> Vec<NodeId> {
    let manager = handler.store_manager_handle();
    let manager = manager.read().await;
    let mut chain = vec![node];
    let Ok(tree) = manager.tree(store_id) else { return chain };
    let mut at = node;
    while chain.len() < 1000 {
        let Some(fields) = tree.doc(at).and_then(|d| d.fields().ok()) else { break };
        let Some(up) = fields.parent_id.or(fields.attached_to) else { break };
        if chain.contains(&up) {
            break;
        }
        chain.push(up);
        at = up;
    }
    chain
}

/// This device holds the account's notice key: from the accounts service
/// when one of the account's devices made it, else made here and
/// published (docs/NOTIFICATIONS_CONTRACT.md section 2: an existing account
/// gets one from its next signed-in device).
pub(crate) async fn ensure_notice_key(handler: &RpcHandler, account: &SignedInAccount) -> Result<pimble_crypto::NoticeKey, String> {
    if let Some(key) = handler.keystore().notice_key().await {
        return Ok(key);
    }
    let fetch = || async {
        let keys = crate::cloud::me_keys(handler.http(), &account.url, &account.session).await.map_err(|e| e.to_string())?;
        match keys.notice_key {
            Some(view) => pimble_crypto::unwrap_notice_key(&view.envelope, &account.keys, &account.user_id, &view.public_key).map(Some).map_err(|e| format!("the account's notice key did not open: {e}")),
            None => Ok(None),
        }
    };
    let key = match fetch().await? {
        Some(key) => key,
        None => {
            let key = pimble_crypto::NoticeKey::generate();
            let envelope = pimble_crypto::wrap_notice_key(&key, &account.keys, &account.user_id).map_err(|e| e.to_string())?;
            if crate::cloud::put_notice_key(handler.http(), &account.url, &account.session, &key.public(), &envelope).await.map_err(|e| e.to_string())? {
                info!("Notices: made this account's notice key");
                key
            } else {
                // Another device of the account's made one a moment ago.
                fetch().await?.ok_or("the account's notice key went missing")?
            }
        }
    };
    handler.keystore().set_notice_key(&account.user_id, &key).await.map_err(|e| e.to_string())?;
    Ok(key)
}

/// The work while an account is signed in and notifications are on: the
/// outbox, and the inbox's long poll.
async fn run(handler: RpcHandler) {
    let Some(account) = handler.keystore().account().await else { return };
    if handler.keystore().session_expired() {
        return;
    }
    let notices = handler.notices();
    let rt = handler.runtime().clone();
    // The switch is the account's: whatever another device set.
    if let Ok(me) = crate::cloud::me(handler.http(), &account.url, &account.session).await {
        notices
            .with(&handler, &account.user_id, |kept| {
                let changed = kept.on != me.notifications || kept.email != me.notification_email || kept.push != me.notification_push || kept.push_key != me.push_key;
                kept.on = me.notifications;
                kept.email = me.notification_email;
                kept.push = me.notification_push;
                kept.push_key = me.push_key.clone();
                ((), changed)
            })
            .await;
    }
    if !notices.with(&handler, &account.user_id, |kept| (kept.on, false)).await {
        return;
    }
    let mut backoff = Duration::from_secs(2);
    let key = loop {
        match ensure_notice_key(&handler, &account).await {
            Ok(key) => break key,
            Err(e) => {
                debug!("Notices: no notice key yet: {}", e);
                rt.sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(300));
            }
        }
    };
    handler.keystore().publish_notice_key().await;

    let sending = async {
        loop {
            notices.flush(&handler, &account).await;
            let waiting = notices.with(&handler, &account.user_id, |kept| (!kept.outbox.is_empty() || !kept.acks.is_empty(), false)).await;
            let work = notices.work.notified();
            if waiting {
                tokio::select! {
                    _ = work => {}
                    _ = rt.sleep(RETRY_EVERY) => {}
                }
            } else {
                work.await;
            }
        }
    };
    let receiving = async {
        // From the start at every run (each item is taken once by id), then
        // from the cursor the service answers.
        let mut cursor = 0u64;
        let mut backoff = Duration::from_secs(1);
        loop {
            match crate::cloud::drain_inbox(handler.http(), &account.url, &account.session, cursor, DRAIN_WAIT_SECS).await {
                Ok(body) => {
                    backoff = Duration::from_secs(1);
                    cursor = body.cursor.max(cursor);
                    if !body.items.is_empty() {
                        notices.take(&handler, &account, &key, body.items).await;
                    }
                }
                Err(e) if e.status() == Some(401) => {
                    handler.keystore().mark_session_expired();
                    handler.cloud_changed();
                    return;
                }
                Err(e) => {
                    debug!("Notices: the inbox could not be drained: {}", e);
                    rt.sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
            }
        }
    };
    tokio::select! {
        _ = sending => {}
        _ = receiving => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_discussion::{Author, Body, Stamp};

    fn note_with(text_md: &str) -> Vec<u8> {
        let mut doc = NodeDoc::from_plain_text("").unwrap();
        doc.write_markdown(text_md).expect("markdown applies");
        doc.save()
    }

    #[test]
    fn a_mention_added_to_a_note_is_a_notice_once_and_a_removal_or_an_unchanged_one_is_not() {
        let node = NodeId::new();
        let before = note_with("Hello there.");
        let after = note_with("Hello there. [@Will](pimble-person:will?m=abc123) can you look?");
        let got = candidates(node, Some(&before), &after);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!((got[0].kind, got[0].recipient.as_str(), got[0].anchor.as_deref()), (NoticeKind::Mention, "will", Some("abc123")));
        assert_eq!(got[0].key, format!("{node}/m:abc123"));
        assert!(got[0].snippet.contains("@Will can you look?"), "{}", got[0].snippet);
        assert!(candidates(node, Some(&after), &after).is_empty(), "nothing new");
        assert!(candidates(node, Some(&after), &before).is_empty(), "taking it out tells nobody");
        assert_eq!(candidates(node, None, &after).len(), 1, "a new note's mentions count");
        assert!(may_mention(&after) && !may_mention(&before));
    }

    #[test]
    fn a_new_message_tells_the_mentioned_and_everyone_else_who_wrote_in_the_thread() {
        use yrs::updates::decoder::Decode;
        use yrs::Transact;
        let (alice, bob, carol) = (Author::new("alice", "Alice"), Author::new("bob", "Bob"), Author::new("carol", "Carol"));
        let robot = Author::new("agent:1", "Claude");
        let d = pimble_discussion::Discussion::standalone();
        d.doc().transact_mut().apply_update(yrs::Update::decode_v1(&pimble_discussion::new_content(None)).unwrap()).unwrap();
        let first = d.edit(|t| t.post(Stamp::new(Some(&alice), "2026-11-23T10:00:00.000Z"), None, &Body::plain("first"))).into_result().unwrap().0;
        d.edit(|t| t.post(Stamp::new(Some(&bob), "2026-11-23T10:00:01.000Z"), Some(&first), &Body::plain("second"))).into_result().unwrap();
        d.edit(|t| t.post(Stamp::new(Some(&robot), "2026-11-23T10:00:02.000Z"), None, &Body::plain("an agent's"))).into_result().unwrap();
        let before = snapshot_bytes(&d);
        let body = Body::from_markdown("[@Bob](pimble-person:bob?m=m1) look at this");
        let new = d.edit(|t| t.post(Stamp::new(Some(&carol), "2026-11-23T10:00:03.000Z"), None, &body)).into_result().unwrap().0;
        let after = snapshot_bytes(&d);
        let node = NodeId::new();
        let mut got = candidates(node, Some(&before), &after);
        got.sort_by(|a, b| a.recipient.cmp(&b.recipient));
        let who: Vec<(&str, NoticeKind)> = got.iter().map(|c| (c.recipient.as_str(), c.kind)).collect();
        assert_eq!(who, vec![("alice", NoticeKind::Reply), ("bob", NoticeKind::Mention)], "Bob is mentioned, Alice wrote in it, the agent is nobody's inbox");
        assert!(got.iter().all(|c| c.anchor.as_deref() == Some(new.as_str()) && c.discussion == Some(node) && c.node == node));
        assert_eq!(got[0].snippet, "@Bob look at this");
        assert!(candidates(node, Some(&after), &after).is_empty());
    }

    fn snapshot_bytes(d: &pimble_discussion::Discussion) -> Vec<u8> {
        use yrs::{ReadTxn, StateVector, Transact};
        d.doc().transact().encode_state_as_update_v1(&StateVector::default())
    }

    #[test]
    fn a_snippet_is_about_120_characters_around_the_mention() {
        let long = format!("{} @Will here {}", "a".repeat(300), "b".repeat(300));
        let s = snippet_around(&long, "@Will");
        assert!(s.contains("@Will here"), "{s}");
        assert!(s.starts_with('…') && s.ends_with('…'));
        assert!(s.chars().count() <= SNIPPET_CHARS + 2);
        assert_eq!(snippet_around("short\n\ntext", "x"), "short text");
    }

    #[test]
    fn the_innermost_share_that_names_the_recipient_addresses_the_notice() {
        let (note, folder) = (NodeId::new(), NodeId::new());
        let person = |id: &str| pimble_rpc::Person { user_id: id.into(), name: id.into(), notice_key: Some("k".into()) };
        let scope = |root, ids: &[&str]| pimble_rpc::ScopePeople { root, people: ids.iter().map(|i| person(i)).collect(), my_name: String::new(), my_name_chosen: true, share_name: String::new() };
        let people = StorePeople { scopes: vec![scope(None, &["joe", "ann"]), scope(Some(folder), &["joe", "will"])] };
        assert_eq!(scope_for(&people, &[note, folder], "will").map(|s| s.root), Some(Some(folder)));
        assert_eq!(scope_for(&people, &[note, folder], "ann").map(|s| s.root), Some(None), "the whole store's");
        assert!(scope_for(&people, &[note], "will").is_none(), "Will cannot open a note outside his share");
    }

    #[test]
    fn what_is_kept_is_pruned() {
        let mut kept = Kept::new("me");
        let now = now_ms();
        kept.sent.insert("old".into(), now - SENT_KEPT_MS - 1);
        kept.sent.insert("new".into(), now);
        let notice = Notice { id: "n".into(), kind: NoticeKind::Mention, from: "a".into(), from_name: "A".into(), store: StoreId::new(), node: NodeId::new(), discussion: None, anchor: None, snippet: String::new(), at: String::new() };
        kept.outbox.push(Outgoing { recipient: "r".into(), notice: notice.clone(), queued_ms: now - OUTBOX_TTL_MS - 1 });
        kept.outbox.push(Outgoing { recipient: "r".into(), notice, queued_ms: now });
        kept.prune(now);
        assert_eq!(kept.sent.keys().collect::<Vec<_>>(), vec!["new"]);
        assert_eq!(kept.outbox.len(), 1, "a notice older than a day is dropped rather than sent late");
    }
}
