//! Discussions (docs/DISCUSSIONS_CONTRACT.md, wave 1): threaded conversations
//! in a discussion node's own document, shown in a pane instead of the
//! editor, on the desktop and in the browser from this one module.
//!
//! A pane showing a discussion holds the node's document ([`Session`]), the
//! way a sheet pane does: it opens from the cached content, reconciles with
//! the server, takes peers' updates, and sends every edit it makes through
//! `applyEdit` as an ordinary yrs update. Every change, local or remote,
//! reads the thread again (`pimble_discussion`) and hands it to the view as
//! plain values ([`MessageView`]).
//!
//! A discussion on a node is an attachment of it (contract section 1): it is
//! in no list, so the explorer never shows it as a row. The node's pane
//! lists its discussions under its content ([`ui::render_host_panel`]), from
//! `AppStore::attachments`, which `ListAttachments` fills; the row shows how
//! many there are.

mod ui;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use pimble_core::{custom_keys, node_types, Node, NodeId, StoreId};
use pimble_crdt::NodeDoc;
use pimble_discussion::{Author, Body, Discussion, DiscussionSnapshot, Span, Stamp};
use rinch::prelude::*;

use crate::panes::{PaneId, MAX_PANES};
use crate::protocol::{BackendCommand, NodeInit};
use crate::state::{ActiveEdit, AppStore, LinkOpen};

pub(crate) use ui::{render_discussion, render_host_panel};

/// A new discussion node's icon (a Tabler icon name).
pub const DISCUSSION_ICON: &str = "messages";
/// How deep replies are indented before they are shown flat, with whom
/// they reply to (Joe, 2026-10-21).
pub const MAX_INDENT: usize = 5;

// ── What the view shows ──────────────────────────────────────────────────

/// One message of the thread, as the view draws it.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageView {
    pub id: String,
    /// Indentation, at most [`MAX_INDENT`].
    pub indent: usize,
    /// "replying to <name>": shown when the reply is deeper than the indent
    /// shows, or waits for a message not here yet.
    pub replying_to: Option<String>,
    /// Who wrote it; empty when the discussion has no authors to tell apart.
    pub author: String,
    pub agent: bool,
    /// Yours (or unauthored): Edit and Delete are offered (decision 3a).
    pub own: bool,
    /// When, as a person reads it ("5 min ago").
    pub when: String,
    /// The full time, for the tooltip.
    pub at: String,
    pub spans: Vec<Span>,
    pub deleted: bool,
    pub edited: bool,
}

/// Which message box is being typed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Composer {
    /// The one at the bottom of the thread (a new message, or a reply).
    New,
    /// The one over a message being edited.
    Edit,
    /// The topic line.
    Topic,
}

/// What a pane's discussion view shows and is typing.
#[derive(Clone, Copy)]
pub struct DiscussionUi {
    pub messages: Signal<Vec<MessageView>>,
    /// "Resolved by Alice, 5 min ago", when it is.
    pub resolved: Signal<Option<String>>,
    /// The deleted messages, newest first: (id, a preview).
    pub deleted: Signal<Vec<(String, String)>>,
    pub show_deleted: Signal<bool>,
    /// The message being typed, as Markdown.
    pub draft: Signal<String>,
    /// The message replied to: (id, its author or a preview).
    pub replying_to: Signal<Option<(String, String)>>,
    /// The message being edited, and its text.
    pub editing: Signal<Option<String>>,
    pub edit_text: Signal<String>,
    /// A refusal, in a sentence.
    pub message: Signal<Option<String>>,
    /// The emoji picker, open over one of the message boxes.
    pub emoji: Signal<Option<Composer>>,
    /// The topic as typed, before Enter commits it.
    pub topic: Signal<String>,
    /// A resolved discussion shows its first message only, until this
    /// (contract section 4): how many it holds, and whether all are shown.
    pub hidden: Signal<usize>,
    pub show_all: Signal<bool>,
}

impl DiscussionUi {
    pub fn new() -> Self {
        Self {
            messages: Signal::new(Vec::new()),
            resolved: Signal::new(None),
            deleted: Signal::new(Vec::new()),
            show_deleted: Signal::new(false),
            draft: Signal::new(String::new()),
            replying_to: Signal::new(None),
            editing: Signal::new(None),
            edit_text: Signal::new(String::new()),
            message: Signal::new(None),
            emoji: Signal::new(None),
            topic: Signal::new(String::new()),
            hidden: Signal::new(0),
            show_all: Signal::new(false),
        }
    }
}

impl Default for DiscussionUi {
    fn default() -> Self {
        Self::new()
    }
}

// ── The pane's document ─────────────────────────────────────────────────

struct Session {
    store_id: StoreId,
    node_id: NodeId,
    doc: NodeDoc,
    discussion: Discussion,
}

thread_local! {
    static SESSIONS: RefCell<[Option<Session>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
    static APP: Cell<Option<AppStore>> = const { Cell::new(None) };
    /// What a create asked for once it is made: open the new discussion,
    /// from this pane.
    static OPEN_AFTER: RefCell<HashMap<u64, PaneId>> = RefCell::new(HashMap::new());
    static NEXT_REQUEST: Cell<u64> = const { Cell::new(0xD15C_0000) };
}

fn with_session<R>(pane: PaneId, f: impl FnOnce(&mut Session) -> R) -> Option<R> {
    SESSIONS.with(|sessions| sessions.borrow_mut()[pane.index()].as_mut().map(f))
}

fn panes_holding(store_id: StoreId, node_id: NodeId) -> Vec<PaneId> {
    PaneId::ALL
        .into_iter()
        .filter(|pane| with_session(*pane, |s| (s.store_id, s.node_id) == (store_id, node_id)).unwrap_or(false))
        .collect()
}

/// The node pane `pane`'s discussion view holds, if any.
pub(crate) fn held(pane: PaneId) -> Option<(StoreId, NodeId)> {
    with_session(pane, |s| (s.store_id, s.node_id))
}

fn origin(pane: PaneId) -> String {
    format!("pimble-discussion-pane-{}", pane.index())
}

/// Who discussion messages are stamped as: the signed-in account (contract
/// section 3), the same identity board markup uses, or nobody.
fn author() -> Option<Author> {
    crate::boards::current_author().map(|a| Author::new(a.id, a.name))
}

/// Show discussion node `node_id` in pane `pane`.
pub(crate) fn start(store: AppStore, pane: PaneId, store_id: StoreId, node_id: NodeId, content: &[u8]) {
    crate::editor::stop_editing(store, pane);
    crate::boards::stop(store, pane);
    crate::sheets::stop(store, pane);
    stop(store, pane);
    APP.with(|app| app.set(Some(store)));
    let first = !store.is_editing(store_id, node_id);
    // A pane opening a discussion another pane holds starts from that copy.
    let sibling = panes_holding(store_id, node_id).into_iter().find_map(|other| with_session(other, |s| s.doc.save()));
    let bytes = sibling.as_deref().unwrap_or(content);
    let doc = match NodeDoc::load(bytes) {
        Ok(doc) => doc,
        Err(e) => {
            tracing::warn!("discussion {node_id} could not be read ({e}); showing it empty until the server's copy arrives");
            NodeDoc::new()
        }
    };
    let discussion = Discussion::of_node(&doc).with_origin(origin(pane));
    SESSIONS.with(|sessions| sessions.borrow_mut()[pane.index()] = Some(Session { store_id, node_id, doc, discussion }));
    let state = store.pane(pane);
    let ui = state.discussion_ui;
    ui.draft.set(String::new());
    ui.replying_to.set(None);
    ui.editing.set(None);
    ui.message.set(None);
    ui.emoji.set(None);
    ui.show_deleted.set(false);
    ui.show_all.set(false);
    state.discussion.set(Some(ActiveEdit { store_id, node_id }));
    if first {
        store.send(BackendCommand::SubscribeNodeChanges { store_id, node_id });
    }
    request_reconcile(store, pane);
    recompute(store, pane);
}

/// Stop showing pane `pane`'s discussion, if it shows one: the node cache
/// gets the document as this pane last had it, and the last pane holding
/// the node lets go of its subscription.
pub(crate) fn stop(store: AppStore, pane: PaneId) {
    let Some(session) = SESSIONS.with(|sessions| sessions.borrow_mut()[pane.index()].take()) else { return };
    end_composing();
    if let Some(sig) = store.get_node_signal(session.store_id, session.node_id) {
        let bytes = session.doc.save();
        sig.update(|node| node.content = bytes);
    }
    let state = store.pane(pane);
    state.discussion_ui.messages.set(Vec::new());
    state.discussion.set(None);
    if !store.is_editing(session.store_id, session.node_id) {
        store.send(BackendCommand::UnsubscribeNodeChanges { store_id: session.store_id, node_id: session.node_id });
    }
}

fn encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Ask the server for what it has beyond pane `pane`'s copy.
fn request_reconcile(store: AppStore, pane: PaneId) {
    if let Some((store_id, node_id, state_vector)) = with_session(pane, |s| (s.store_id, s.node_id, s.doc.state_vector())) {
        store.send(BackendCommand::ReconcileNodeContent { store_id, node_id, state_vector });
    }
}

/// Every pane holding the node asks again (the connection came back).
pub(crate) fn request_reconcile_all(store: AppStore, store_id: StoreId, node_id: NodeId) {
    for pane in panes_holding(store_id, node_id) {
        request_reconcile(store, pane);
    }
}

/// The server's answer to a reconcile: merged into every pane holding the
/// node, then what the server lacks goes back.
pub(crate) fn apply_reconcile(store: AppStore, store_id: StoreId, node_id: NodeId, diff: &[u8], server_state_vector: &[u8]) {
    let writable = store.node_access(store_id, node_id).allows_write();
    for pane in panes_holding(store_id, node_id) {
        let ours = with_session(pane, |s| {
            if !diff.is_empty() {
                if let Err(e) = s.doc.apply_update(diff) {
                    tracing::warn!("a discussion's reconcile did not apply: {e}");
                }
            }
            if writable {
                s.doc.diff_if_peer_lacks_it(server_state_vector, diff).ok().flatten()
            } else {
                None
            }
        })
        .flatten();
        if let Some(ours) = ours {
            store.send(BackendCommand::BroadcastChanges { store_id, node_id, changes: encode(&ours) });
        }
        refresh_cache(store, pane);
        recompute(store, pane);
    }
}

/// A peer's update of a node: applied in every pane holding it.
pub(crate) fn apply_remote(store: AppStore, store_id: StoreId, node_id: NodeId, bytes: &[u8]) {
    for pane in panes_holding(store_id, node_id) {
        apply_to(store, pane, bytes);
    }
}

fn apply_to(store: AppStore, pane: PaneId, bytes: &[u8]) {
    let applied = with_session(pane, |s| {
        if let Err(e) = s.doc.apply_update(bytes) {
            tracing::warn!("a discussion update did not apply: {e}");
        }
    });
    if applied.is_some() {
        refresh_cache(store, pane);
        recompute(store, pane);
    }
}

/// The node cache takes pane `pane`'s copy of its discussion: what this
/// window shows of it elsewhere (its host's list) reads the cache, and no
/// refetch comes for a node a pane holds.
fn refresh_cache(store: AppStore, pane: PaneId) {
    let Some((store_id, node_id, bytes)) = with_session(pane, |s| (s.store_id, s.node_id, s.doc.save())) else { return };
    if let Some(sig) = store.get_node_signal(store_id, node_id) {
        sig.update(|node| node.content = bytes);
    }
}

/// Send an update this pane made: to the server through `applyEdit`, and to
/// every other pane of this window holding the discussion, a turn later.
fn send_update(store: AppStore, from: PaneId, store_id: StoreId, node_id: NodeId, bytes: Vec<u8>) {
    store.send(BackendCommand::BroadcastChanges { store_id, node_id, changes: encode(&bytes) });
    let others: Vec<PaneId> = panes_holding(store_id, node_id).into_iter().filter(|p| *p != from).collect();
    if others.is_empty() {
        return;
    }
    let deliver = move || {
        for other in others {
            apply_to(store, other, &bytes);
        }
    };
    #[cfg(test)]
    deliver();
    // Armed by no component: the button that made the edit may be rebuilt
    // by the edit itself (Delete), which would cancel it.
    #[cfg(not(test))]
    rinch::core::reactive::unowned(|| set_timeout(0, deliver));
}

fn say(store: AppStore, pane: PaneId, sentence: impl Into<String>) {
    store.pane(pane).discussion_ui.message.set(Some(sentence.into()));
}

/// One write of pane `pane`'s discussion, sent once it is made. `false`
/// (with the reason said) when it was refused.
fn local_edit(store: AppStore, pane: PaneId, f: impl FnOnce(&mut pimble_discussion::DiscussionTxn<'_, '_>) -> pimble_discussion::doc::Result<()>) -> bool {
    let Some((store_id, node_id)) = held(pane) else { return false };
    if !store.node_access(store_id, node_id).allows_write() {
        say(store, pane, pimble_core::StoreAccess::READ_ONLY_REFUSAL);
        return false;
    }
    let Some(edited) = with_session(pane, |s| s.discussion.edit(f)) else { return false };
    let ok = match edited.result {
        Ok(()) => {
            store.pane(pane).discussion_ui.message.set(None);
            true
        }
        Err(e) => {
            say(store, pane, e.to_string());
            false
        }
    };
    if let Some(update) = edited.update {
        send_update(store, pane, store_id, node_id, update);
        // No echo of our own edit refreshes the cache either.
        refresh_cache(store, pane);
    }
    recompute(store, pane);
    ok
}

// ── Reading the thread ──────────────────────────────────────────────────

/// "just now", "5 min ago", "3 h ago", "Oct 21": how long ago `at` was.
pub(crate) fn ago(at: &str, now_ms: f64) -> String {
    let Ok(then) = chrono::DateTime::parse_from_rfc3339(at) else { return String::new() };
    let seconds = ((now_ms - then.timestamp_millis() as f64) / 1000.0).max(0.0) as i64;
    match seconds {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{} min ago", seconds / 60),
        3600..=86_399 => format!("{} h ago", seconds / 3600),
        _ => then.format("%b %-d, %Y").to_string(),
    }
}

/// The thread as the view shows it, from a snapshot.
pub(crate) fn views_of(snap: &DiscussionSnapshot, me: Option<&Author>, now_ms: f64) -> Vec<MessageView> {
    let named = !snap.authors().is_empty();
    let name_of = |id: &str| -> String {
        snap.message(id)
            .map(|m| match &m.author {
                Some(a) if !a.name.is_empty() => a.name.clone(),
                _ => preview(&m.body, 24),
            })
            .unwrap_or_default()
    };
    snap.thread()
        .into_iter()
        .map(|item| {
            let m = &snap.messages[item.index];
            let indent = item.depth.min(MAX_INDENT);
            let replying_to = if item.waiting {
                Some("a message not here yet".to_string())
            } else if item.depth > MAX_INDENT {
                m.reply_to.as_deref().map(name_of)
            } else {
                None
            };
            let own = match (&m.author, me) {
                (None, _) => true,
                (Some(a), Some(me)) => a.id == me.id,
                (Some(_), None) => false,
            };
            MessageView {
                id: m.id.clone(),
                indent,
                replying_to,
                author: if named { m.author.as_ref().map(|a| a.name.clone()).unwrap_or_else(|| "Someone".into()) } else { String::new() },
                agent: m.author.as_ref().is_some_and(Author::is_agent),
                own,
                when: ago(&m.created, now_ms),
                at: m.created.clone(),
                spans: m.body.spans.clone(),
                deleted: m.deleted_at.is_some(),
                edited: m.edited.is_some(),
            }
        })
        .collect()
}

/// The first `n` characters of a body, on one line.
fn preview(body: &Body, n: usize) -> String {
    let text = body.text().replace('\n', " ");
    if text.chars().count() > n {
        text.chars().take(n).collect::<String>() + "…"
    } else {
        text
    }
}

/// Read pane `pane`'s discussion again into its view.
pub(crate) fn recompute(store: AppStore, pane: PaneId) {
    let Some(snap) = with_session(pane, |s| s.discussion.read()) else { return };
    let now = crate::boards::now_ms();
    let me = author();
    let ui = store.pane(pane).discussion_ui;
    let mut views = views_of(&snap, me.as_ref(), now);
    let collapsed = snap.resolved.is_some() && !untracked(|| ui.show_all.get()) && views.len() > 1;
    ui.hidden.set(if collapsed { views.len() - 1 } else { 0 });
    if collapsed {
        views.truncate(1);
    }
    ui.messages.set(views);
    ui.resolved.set(snap.resolved.as_ref().map(|r| {
        let by = r.by.as_ref().map(|a| format!(" by {}", a.name)).unwrap_or_default();
        format!("Resolved{by}, {}", ago(&r.at, now))
    }));
    ui.deleted.set(snap.deleted().into_iter().map(|m| (m.id.clone(), preview(&m.body, 60))).collect());
}

// ── Writing ─────────────────────────────────────────────────────────────

/// Send pane `pane`'s draft: a new message, or a reply to the message it
/// replies to.
pub(crate) fn post(store: AppStore, pane: PaneId) {
    let ui = store.pane(pane).discussion_ui;
    let draft = untracked(|| ui.draft.get());
    let body = Body::from_markdown(&draft);
    if body.is_blank() {
        return;
    }
    let reply_to = untracked(|| ui.replying_to.get()).map(|(id, _)| id);
    let me = author();
    let now = crate::boards::now_stamp();
    let posted = local_edit(store, pane, |t| t.post(Stamp::new(me.as_ref(), &now), reply_to.as_deref(), &body).map(|_| ()));
    if posted {
        ui.draft.set(String::new());
        ui.replying_to.set(None);
        ui.emoji.set(None);
    }
}

/// Reply to message `id`: the message box at the bottom says so.
pub(crate) fn reply_to(store: AppStore, pane: PaneId, id: &str) {
    let ui = store.pane(pane).discussion_ui;
    let who = untracked(|| ui.messages.get())
        .into_iter()
        .find(|m| m.id == id)
        .map(|m| if m.author.is_empty() { spans_preview(&m.spans) } else { m.author.clone() })
        .unwrap_or_default();
    ui.replying_to.set(Some((id.to_string(), who)));
    ui::focus_composer(pane, Composer::New);
}

fn spans_preview(spans: &[Span]) -> String {
    preview(&Body { spans: spans.to_vec() }, 24)
}

/// Start editing message `id`: its text as Markdown in the edit box.
pub(crate) fn begin_edit(store: AppStore, pane: PaneId, id: &str) {
    let Some(body) = with_session(pane, |s| s.discussion.read().message(id).map(|m| m.body.clone())).flatten() else { return };
    let ui = store.pane(pane).discussion_ui;
    ui.edit_text.set(body.to_markdown());
    ui.editing.set(Some(id.to_string()));
    ui::focus_composer(pane, Composer::Edit);
}

pub(crate) fn save_edit(store: AppStore, pane: PaneId) {
    let ui = store.pane(pane).discussion_ui;
    let Some(id) = untracked(|| ui.editing.get()) else { return };
    let body = Body::from_markdown(&untracked(|| ui.edit_text.get()));
    let me = author();
    let now = crate::boards::now_stamp();
    if local_edit(store, pane, |t| t.edit_message(Stamp::new(me.as_ref(), &now), &id, &body)) {
        ui.editing.set(None);
        ui.emoji.set(None);
    }
}

pub(crate) fn cancel_edit(store: AppStore, pane: PaneId) {
    let ui = store.pane(pane).discussion_ui;
    ui.editing.set(None);
    ui.emoji.set(None);
}

pub(crate) fn delete_message(store: AppStore, pane: PaneId, id: &str) {
    let me = author();
    let now = crate::boards::now_stamp();
    let id = id.to_string();
    local_edit(store, pane, |t| t.delete_message(Stamp::new(me.as_ref(), &now), &id));
}

pub(crate) fn restore_message(store: AppStore, pane: PaneId, id: &str) {
    let id = id.to_string();
    local_edit(store, pane, |t| t.restore_message(&id));
}

/// Show every message of a resolved discussion.
pub(crate) fn show_all(store: AppStore, pane: PaneId) {
    store.pane(pane).discussion_ui.show_all.set(true);
    recompute(store, pane);
}

pub(crate) fn resolve(store: AppStore, pane: PaneId, reopen: bool) {
    store.pane(pane).discussion_ui.show_all.set(false);
    let me = author();
    let now = crate::boards::now_stamp();
    local_edit(store, pane, |t| if reopen { t.reopen() } else { t.resolve(Stamp::new(me.as_ref(), &now)) });
}

/// The topic, as typed: the discussion node's title.
pub(crate) fn set_topic(store: AppStore, pane: PaneId, text: &str) {
    let Some((store_id, node_id)) = held(pane) else { return };
    store.send(BackendCommand::RenameNode { store_id, node_id, title: text.trim().to_string() });
}

/// The discussion node's title, for the topic field.
pub(crate) fn topic_of(store: AppStore, pane: PaneId) -> String {
    let Some((store_id, node_id)) = store.pane(pane).discussion.get().map(|e| (e.store_id, e.node_id)) else { return String::new() };
    let sig = store.node_data.with(|map| map.get(&(store_id, node_id)).copied());
    sig.map(|s| s.with(|n| n.metadata.title.clone())).unwrap_or_default()
}

// ── The message boxes' keys ─────────────────────────────────────────────

thread_local! {
    /// The message box being typed in: its pane and which one.
    static COMPOSING: Cell<Option<(PaneId, Composer)>> = const { Cell::new(None) };
    /// A press landed in a message box this turn (heard before the pane's).
    static PRESS_IN_BOX: Cell<bool> = const { Cell::new(false) };
}

/// A press in one of pane `pane`'s message boxes: Enter sends (or saves),
/// Shift+Enter starts a new line, Escape cancels. Plain inputs hear Enter
/// only through the keyboard interceptor (rinch has no keydown on them yet,
/// joeleaver/rinch#1678), so the box takes it until a press lands elsewhere.
pub(crate) fn begin_composing(store: AppStore, pane: PaneId, which: Composer) {
    PRESS_IN_BOX.with(|p| p.set(true));
    COMPOSING.with(|c| c.set(Some((pane, which))));
    // Owned by no component: a click on Edit rebuilds the message it is in,
    // and an interceptor owned by that button would go with it.
    rinch::core::reactive::unowned(|| rinch::core::set_keyboard_interceptor(move |data| {
        if data.key != "Enter" && data.key != "Escape" {
            return false;
        }
        let Some((p, which)) = COMPOSING.with(Cell::get) else { return false };
        if data.key == "Enter" && data.shift {
            return false;
        }
        if data.kind != rinch::core::KeyEventKind::Down {
            return true;
        }
        let escape = data.key == "Escape";
        let _ = set_timeout(0, move || match (which, escape) {
            (Composer::New, false) => post(store, p),
            (Composer::New, true) => {
                let ui = store.pane(p).discussion_ui;
                ui.replying_to.set(None);
                ui.emoji.set(None);
            }
            (Composer::Edit, false) => save_edit(store, p),
            (Composer::Edit, true) => cancel_edit(store, p),
            (Composer::Topic, false) => {
                let text = untracked(|| store.pane(p).discussion_ui.topic.get());
                set_topic(store, p, &text);
                end_composing();
            }
            (Composer::Topic, true) => {
                store.pane(p).discussion_ui.topic.set(untracked(|| topic_of(store, p)));
                end_composing();
            }
        });
        true
    }));
}

/// A press anywhere in a pane: one outside the message boxes ends typing in
/// them, so the next Enter goes where the press went.
pub(crate) fn pane_pressed() {
    if PRESS_IN_BOX.with(|p| p.replace(false)) {
        return;
    }
    end_composing();
}

fn end_composing() {
    if COMPOSING.with(|c| c.take()).is_some() {
        rinch::core::clear_keyboard_interceptor();
    }
}

// ── Making one ──────────────────────────────────────────────────────────

fn next_request() -> u64 {
    NEXT_REQUEST.with(|n| {
        let r = n.get();
        n.set(r + 1);
        r
    })
}

fn init(request: u64) -> NodeInit {
    NodeInit {
        node_type: node_types::DISCUSSION.to_string(),
        content: pimble_discussion::new_content(None),
        custom: vec![
            (custom_keys::ICON.to_string(), serde_json::json!(DISCUSSION_ICON)),
            (custom_keys::EXPLICIT_TITLE.to_string(), serde_json::json!(true)),
        ],
        request: Some(request),
    }
}

/// A row's "New Discussion": a discussion node of its own under `parent_id`,
/// opened in the focused pane once it is made.
pub(crate) fn new_discussion(store: AppStore, store_id: StoreId, parent_id: NodeId) {
    let request = next_request();
    OPEN_AFTER.with(|o| o.borrow_mut().insert(request, store.focused()));
    store.send(BackendCommand::CreateNode { store_id, parent_id: Some(parent_id), title: String::new(), init: Some(init(request)) });
}

/// "Start a Discussion" on node `host_id`: an attachment of it, opened beside
/// `from` (split to the right when there is room) once it is made.
pub(crate) fn start_on(store: AppStore, from: PaneId, store_id: StoreId, host_id: NodeId) {
    if !store.node_access(store_id, host_id).allows_write() {
        store.notice.set(pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string());
        return;
    }
    let request = next_request();
    OPEN_AFTER.with(|o| o.borrow_mut().insert(request, from));
    store.send(BackendCommand::CreateAttachment { store_id, host_id, title: String::new(), init: init(request) });
}

/// Start a discussion on the node the focused pane shows (Edit menu).
pub(crate) fn start_on_focused(store: AppStore) {
    let pane = store.focused();
    let held = untracked(|| {
        let p = store.pane(pane);
        p.active_edit.get().or_else(|| p.board.get()).or_else(|| p.sheet.get())
    });
    match held {
        Some(edit) => start_on(store, pane, edit.store_id, edit.node_id),
        None => store.notice.set("Open a note to start a discussion on it.".to_string()),
    }
}

/// Open discussion `node_id` from pane `from`: in a new pane to its right
/// when there is room for one, in `from` otherwise.
pub(crate) fn open_beside(store: AppStore, from: PaneId, store_id: StoreId, node_id: NodeId) {
    let pane = untracked(|| {
        // A pane already showing it takes the focus instead.
        PaneId::ALL.into_iter().find(|p| store.pane(*p).discussion.get().is_some_and(|e| (e.store_id, e.node_id) == (store_id, node_id)) && store.tiling.with(|t| t.rect_of(*p).is_some()))
    });
    if let Some(pane) = pane {
        crate::pane_view::focus_pane(store, pane);
        return;
    }
    // Another pane shown that holds nothing, or another discussion, takes it;
    // otherwise `from` is split, and with no room left `from` shows it.
    let reuse = untracked(|| {
        PaneId::ALL.into_iter().find(|p| {
            let state = store.pane(*p);
            *p != from
                && store.tiling.with(|t| t.rect_of(*p).is_some())
                && (state.discussion.get().is_some()
                    || (!state.show_editor.get() && !state.show_board.get() && !state.show_sheet.get() && !state.show_discussion.get()))
        })
    });
    let target = reuse.or_else(|| crate::pane_view::split_pane(store, from, crate::panes::Direction::Right)).unwrap_or(from);
    store.link_open.set(Some(LinkOpen { value: format!("node_{store_id}_{node_id}"), store_id, node_id, anchor: None, pane: Some(target) }));
}

/// A create this module asked for was made: open it.
pub(crate) fn created(store: AppStore, request: u64, store_id: StoreId, node_id: NodeId, host: Option<NodeId>) {
    let Some(from) = OPEN_AFTER.with(|o| o.borrow_mut().remove(&request)) else { return };
    match host {
        Some(host) => {
            store.send(BackendCommand::ListAttachments { store_id, node_id: host });
            store.send(BackendCommand::GetNode { store_id, node_id: host });
            open_beside(store, from, store_id, node_id);
        }
        None => store.link_open.set(Some(LinkOpen { value: format!("node_{store_id}_{node_id}"), store_id, node_id, anchor: None, pane: Some(from) })),
    }
}

// ── A node's discussions ────────────────────────────────────────────────

/// `ListAttachments` answered: the discussions go in the node cache and the
/// host's list.
pub(crate) fn attachments_listed(store: AppStore, store_id: StoreId, host: NodeId, nodes: Vec<Node>) {
    let ids: Vec<NodeId> = nodes.iter().filter(|n| n.node_type == node_types::DISCUSSION).map(|n| n.id).collect();
    for node in nodes {
        store.upsert_node(store_id, node);
    }
    store.attachments.update(|map| {
        map.insert((store_id, host), ids);
    });
}

/// An attachment of `host` came, went or came back: its list is asked for
/// again where this window shows it, and the host's count.
pub(crate) fn attachment_changed(store: AppStore, store_id: StoreId, host: NodeId, node_id: NodeId, live: bool) {
    let listed = untracked(|| store.attachments.with(|map| map.contains_key(&(store_id, host))));
    if listed {
        store.send(BackendCommand::ListAttachments { store_id, node_id: host });
    }
    store.send(BackendCommand::GetNode { store_id, node_id: host });
    if !live {
        // A pane showing a deleted discussion shows nothing now.
        for pane in store.panes_discussing(store_id, node_id) {
            stop(store, pane);
            store.clear_pane(pane);
        }
    }
}

/// The discussions on node `(store_id, host)` this window knows of, tracked,
/// with each one's node (from the cache).
pub(crate) fn host_discussions(store: AppStore, store_id: StoreId, host: NodeId) -> Option<Vec<(NodeId, Option<Node>)>> {
    let ids = store.attachments.with(|map| map.get(&(store_id, host)).cloned())?;
    Some(
        ids.into_iter()
            .map(|id| {
                let sig = store.node_data.with(|map| map.get(&(store_id, id)).copied());
                (id, sig.map(|s| s.get()))
            })
            .collect(),
    )
}

/// One line about a discussion for its host's list.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscussionRow {
    pub store_id: StoreId,
    pub node_id: NodeId,
    pub topic: String,
    pub count: usize,
    /// "Alice: the latest message…", "No messages yet".
    pub latest: String,
    pub when: String,
    pub resolved: bool,
}

pub(crate) fn row_of(store_id: StoreId, node_id: NodeId, node: Option<&Node>, now_ms: f64) -> DiscussionRow {
    let snap = node.and_then(|n| NodeDoc::load(&n.content).ok()).map(|doc| Discussion::of_node(&doc).read()).unwrap_or_default();
    let topic = node.map(|n| n.metadata.title.trim().to_string()).unwrap_or_default();
    let latest = snap.latest();
    let first = snap.messages.iter().find(|m| m.deleted_at.is_none());
    let topic = if topic.is_empty() { first.map(|m| preview(&m.body, 48)).filter(|t| !t.is_empty()).unwrap_or_else(|| "Discussion".into()) } else { topic };
    DiscussionRow {
        store_id,
        node_id,
        topic,
        count: snap.live_count(),
        latest: match latest {
            Some(m) => match &m.author {
                Some(a) if !a.name.is_empty() => format!("{}: {}", a.name, preview(&m.body, 80)),
                _ => preview(&m.body, 80),
            },
            None => "No messages yet".to_string(),
        },
        when: latest.map(|m| ago(&m.created, now_ms)).unwrap_or_default(),
        resolved: snap.resolved.is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-10-21T12:00:00.000Z";

    fn now_ms() -> f64 {
        chrono::DateTime::parse_from_rfc3339(NOW).unwrap().timestamp_millis() as f64
    }

    #[test]
    fn ago_reads_like_a_person() {
        assert_eq!(ago("2026-10-21T11:59:30.000Z", now_ms()), "just now");
        assert_eq!(ago("2026-10-21T11:55:00.000Z", now_ms()), "5 min ago");
        assert_eq!(ago("2026-10-21T09:00:00.000Z", now_ms()), "3 h ago");
        assert_eq!(ago("2026-10-01T09:00:00.000Z", now_ms()), "Oct 1, 2026");
    }

    #[test]
    fn deep_replies_are_shown_flat_with_whom_they_answer() {
        let d = Discussion::standalone();
        {
            use yrs::updates::decoder::Decode;
            use yrs::Transact;
            d.doc().transact_mut().apply_update(yrs::Update::decode_v1(&pimble_discussion::new_content(None)).unwrap()).unwrap();
        }
        let alice = Author::new("a", "Alice");
        let bob = Author::new("b", "Bob");
        let mut parent: Option<String> = None;
        for i in 0..8 {
            let who = if i % 2 == 0 { &alice } else { &bob };
            let at = format!("2026-10-21T11:0{i}:00.000Z");
            let id = d.edit(|t| t.post(Stamp::new(Some(who), &at), parent.as_deref(), &Body::plain(format!("m{i}")))).into_result().unwrap().0;
            parent = Some(id);
        }
        let views = views_of(&d.read(), Some(&alice), now_ms());
        assert_eq!(views.iter().map(|v| v.indent).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4, 5, 5, 5]);
        assert_eq!(views[6].replying_to.as_deref(), Some("Bob"));
        assert!(views[0].own && !views[1].own, "Alice's are hers, Bob's are not");
        assert_eq!(views[1].author, "Bob");
    }

    #[test]
    fn a_row_names_the_latest_message() {
        let mut node = Node::new(node_types::DISCUSSION);
        let d = Discussion::standalone();
        {
            use yrs::updates::decoder::Decode;
            use yrs::Transact;
            d.doc().transact_mut().apply_update(yrs::Update::decode_v1(&pimble_discussion::new_content(None)).unwrap()).unwrap();
        }
        let mut doc = NodeDoc::new();
        doc.apply_update(&pimble_discussion::new_content(None)).unwrap();
        let alice = Author::new("a", "Alice");
        Discussion::of_node(&doc).edit(|t| t.post(Stamp::new(Some(&alice), "2026-10-21T11:55:00.000Z"), None, &Body::plain("first thoughts"))).into_result().unwrap();
        node.content = doc.save();
        let row = row_of(StoreId::new(), node.id, Some(&node), now_ms());
        assert_eq!(row.topic, "first thoughts", "an empty topic shows the first message");
        assert_eq!(row.count, 1);
        assert_eq!(row.latest, "Alice: first thoughts");
        assert_eq!(row.when, "5 min ago");
    }
}
