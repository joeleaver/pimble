//! Presence in the app (docs/PRESENCE_CONTRACT.md): who else has a note open,
//! shown as initials on the pane and dots on the tree row, and this
//! window's own panes said to everyone else.
//!
//! Each pane that holds a document is one session (`{client}:{pane}`, the
//! backend fills in the client): a frame when what it holds or its focus
//! changes, one every heartbeat while it holds it, and a goodbye when it lets
//! go. What others send arrives as `PresenceFrame` events and is held until
//! its session says goodbye or goes quiet. Nothing here is stored.
//!
//! Where people are in a note's text, their carets and selections, is
//! [`text`]; where they are on a board, their pointers and selections, is
//! [`board`]. A pane that follows someone goes where they go ([`follow`]).

pub mod board;
pub mod follow;
pub mod text;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use pimble_core::{NodeId, StoreId};
use pimble_rpc::{BoardPresence, PresenceBody, PresenceFrame, PresenceWho, TextPresence, PRESENCE_HEARTBEAT_MS};
use rinch::prelude::*;

use crate::panes::{PaneId, MAX_PANES};
use crate::protocol::BackendCommand;
use crate::state::AppStore;

/// One session someone else has in a document.
#[derive(Debug, Clone, PartialEq)]
pub struct Present {
    pub session: String,
    pub who: Option<PresenceWho>,
    pub device: String,
    pub focused: bool,
    /// Where it is in the note's text.
    pub text: Option<TextPresence>,
    /// Where it is on the board.
    pub board: Option<BoardPresence>,
    /// When its caret last moved (milliseconds since the epoch): its flag
    /// names it for a while after.
    pub moved_ms: f64,
    /// When it lapses without another frame (milliseconds since the epoch).
    pub expires_ms: f64,
}

/// A person in a document, their sessions merged: what a pane's initials and
/// a tree row's dot show. The type and the components that draw it are
/// pimble-ui's, which plugins draw with too.
pub use pimble_ui::presence::{follow_hint, hover, initials, Person};

/// Who one session is, as a caret's flag shows them.
#[derive(Debug, Clone, PartialEq)]
pub struct Identity {
    /// What sessions of one person share (see [`Person::key`]).
    pub key: String,
    pub name: String,
    /// `#rrggbb`.
    pub color: String,
    /// Which of the ten author colors it is.
    pub color_index: usize,
    /// One or two letters: an agent's from its own name, not its owner's.
    pub initials: String,
    /// An AI agent (docs/PRESENCE_CONTRACT.md section 8).
    pub agent: bool,
}

/// Where a pane is in the document it holds, as its frame says it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Whereabouts {
    pub text: Option<TextPresence>,
    pub board: Option<BoardPresence>,
}

/// The least time between two frames of one pane that moved (a caret, a
/// pointer, a pan).
const COALESCE_MS: f64 = 50.0;

thread_local! {
    /// What each pane last said it holds: `(store, node, focused)`.
    static SAID: RefCell<HashMap<PaneId, (StoreId, NodeId, bool)>> = RefCell::new(HashMap::new());
    /// Where each pane last said it is.
    static SENT: RefCell<[Whereabouts; MAX_PANES]> = RefCell::new(Default::default());
    /// When each pane last said it.
    static SENT_MS: [Cell<f64>; MAX_PANES] = const { [Cell::new(0.0), Cell::new(0.0), Cell::new(0.0), Cell::new(0.0)] };
    /// A pane moved and its frame waits for the coalescing gap.
    static OWED: [Cell<bool>; MAX_PANES] = const { [Cell::new(false), Cell::new(false), Cell::new(false), Cell::new(false)] };
}

// ── This window's panes ──────────────────────────────────────────────────

/// Start saying where this window's panes are: an effect over every pane's
/// document and the focus, and the heartbeat. Called once, while the view is
/// built.
pub fn start(store: AppStore) {
    let _ = rinch::Effect::new(move || {
        let focused = store.focused_pane.get();
        let now: HashMap<PaneId, (StoreId, NodeId, bool)> = PaneId::ALL
            .into_iter()
            .filter_map(|pane| crate::pane_view::pane_document(store, pane).map(|(s, n)| (pane, (s, n, pane == focused))))
            .collect();
        untracked(|| say(store, now));
    });
    heartbeat(store);
    follow::start_watching(store);
}

/// Tell everyone what changed since the last time: a goodbye for each pane
/// that let a document go, a frame for each that holds a new one or changed
/// focus.
fn say(store: AppStore, now: HashMap<PaneId, (StoreId, NodeId, bool)>) {
    let before = SAID.with(|said| said.borrow().clone());
    for (pane, (store_id, node_id, _)) in &before {
        if now.get(pane).is_none_or(|(s, n, _)| (s, n) != (store_id, node_id)) {
            store.send(BackendCommand::PublishPresence { store_id: *store_id, node_id: *node_id, session: pane_session(*pane), focused: false, left: true, text: None, board: None });
            SENT.with(|s| s.borrow_mut()[pane.index()] = Whereabouts::default());
        }
    }
    SAID.with(|said| *said.borrow_mut() = now.clone());
    // What a plugin's view said it is on belongs to the document the pane
    // held: a pane that let it go lets that go too.
    let within: Vec<PaneId> = WITHIN.with(|w| w.borrow().keys().copied().collect());
    for pane in within {
        let held = now.get(&pane).map(|(s, n, _)| (*s, *n));
        if held != WITHIN.with(|w| w.borrow().get(&pane).map(|w| w.view)) {
            say_within(store, pane, None);
        }
    }
    for (pane, held) in &now {
        if before.get(pane) != Some(held) {
            let (store_id, node_id, focused) = *held;
            let at = take_mine(store, *pane);
            store.send(BackendCommand::PublishPresence { store_id, node_id, session: pane_session(*pane), focused, left: false, text: at.text, board: at.board });
        }
    }
}

/// Where pane `pane` is: in its note's text, or on its board.
fn mine(store: AppStore, pane: PaneId) -> Whereabouts {
    Whereabouts { text: text::mine(store, pane), board: crate::boards::presence_mine(store, pane) }
}

/// Pane `pane`'s whereabouts as its next frame carries them, remembered as
/// said.
fn take_mine(store: AppStore, pane: PaneId) -> Whereabouts {
    let at = mine(store, pane);
    SENT.with(|s| s.borrow_mut()[pane.index()] = at.clone());
    SENT_MS.with(|s| s[pane.index()].set(crate::boards::now_ms()));
    at
}

/// Pane `pane` moved in its document (a caret or selection, a pointer, a
/// pan, or a note opened): a frame, coalesced, when what it says changed.
/// Read a turn later, never from inside the callback that tells.
pub(crate) fn moved(store: AppStore, pane: PaneId) {
    if OWED.with(|o| o[pane.index()].replace(true)) {
        return;
    }
    let since = crate::boards::now_ms() - SENT_MS.with(|s| s[pane.index()].get());
    let wait = (COALESCE_MS - since).max(0.0) as u32;
    rinch::core::reactive::unowned(|| {
        set_timeout(wait, move || {
            OWED.with(|o| o[pane.index()].set(false));
            if SENT.with(|s| s.borrow()[pane.index()] == mine(store, pane)) {
                return;
            }
            let Some((store_id, node_id, focused)) = SAID.with(|said| said.borrow().get(&pane).copied()) else { return };
            let at = take_mine(store, pane);
            store.send(BackendCommand::PublishPresence { store_id, node_id, session: pane_session(pane), focused, left: false, text: at.text, board: at.board });
        })
    });
}

/// Every heartbeat: say it all again, and let go of whoever went quiet.
fn heartbeat(store: AppStore) {
    let _ = set_timeout(PRESENCE_HEARTBEAT_MS as u32, move || {
        let held = SAID.with(|said| said.borrow().clone());
        for (pane, (store_id, node_id, focused)) in held {
            let at = take_mine(store, pane);
            store.send(BackendCommand::PublishPresence { store_id, node_id, session: pane_session(pane), focused, left: false, text: at.text, board: at.board });
        }
        let within = WITHIN.with(|w| w.borrow().clone());
        for (pane, w) in within {
            store.send(within_frame(pane, w.on, false));
        }
        prune(store);
        heartbeat(store);
    });
}

/// Pane `pane`'s own session: its index.
fn pane_session(pane: PaneId) -> String {
    pane.index().to_string()
}

/// The session of what a plugin's view in pane `pane` says the person is on
/// (a card they chose): beside the pane's own, under the same client, so
/// this window drops it as its own and everyone else sees the person, not a
/// second visitor.
pub(crate) fn within_session(pane: PaneId) -> String {
    format!("{}.in", pane.index())
}

/// What a plugin's view in a pane said the person is on.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Within {
    /// The document the pane holds (the metanode).
    view: (StoreId, NodeId),
    /// The node the person is on in it.
    on: (StoreId, NodeId),
}

thread_local! {
    /// What each pane's plugin view says the person is on.
    static WITHIN: RefCell<HashMap<PaneId, Within>> = RefCell::new(HashMap::new());
}

fn within_frame(pane: PaneId, (store_id, node_id): (StoreId, NodeId), left: bool) -> BackendCommand {
    // Not focused: following someone follows the pane they work in (the
    // metanode, which opens their view), not the node they are on in it.
    BackendCommand::PublishPresence { store_id, node_id, session: within_session(pane), focused: false, left, text: None, board: None }
}

/// The plugin's view in pane `pane` says the person is on `node` in it, or on
/// nothing (docs/PRESENCE_CONTRACT.md "Plugins"). Everyone else then sees the
/// person on that node as they would see someone with it open: on its tree
/// row, on a pane holding it, on its card in their own view. Said again with
/// every heartbeat while it holds, and a goodbye when it changes, when the
/// view goes, or when the pane lets its document go.
pub(crate) fn say_within(store: AppStore, pane: PaneId, node: Option<(StoreId, NodeId)>) {
    let before = WITHIN.with(|w| w.borrow().get(&pane).copied());
    let view = SAID.with(|said| said.borrow().get(&pane).map(|(s, n, _)| (*s, *n)));
    let now = match (node, view) {
        (Some(on), Some(view)) => Some(Within { view, on }),
        _ => None,
    };
    if before.map(|w| w.on) == now.map(|w| w.on) {
        return;
    }
    if let Some(before) = before {
        store.send(within_frame(pane, before.on, true));
    }
    WITHIN.with(|w| match now {
        Some(within) => {
            w.borrow_mut().insert(pane, within);
        }
        None => {
            w.borrow_mut().remove(&pane);
        }
    });
    if let Some(now) = now {
        store.send(within_frame(pane, now.on, false));
    }
}

// ── Everyone else ────────────────────────────────────────────────────────

/// A frame from someone else (`BackendEvent::PresenceFrame`).
pub(crate) fn received(store: AppStore, frame: &PresenceFrame) {
    let now = crate::boards::now_ms();
    store.presence.update(|all| {
        // A session holds one document: wherever it was, it is not now.
        let mut before: Option<Present> = None;
        for ((s, d), sessions) in all.iter_mut() {
            if let Some(i) = sessions.iter().position(|p| p.session == frame.session) {
                let held = sessions.remove(i);
                if (*s, *d) == (frame.store_id, frame.doc_id) {
                    before = Some(held);
                }
            }
        }
        if !frame.left {
            let body = PresenceBody::decode(&frame.body).unwrap_or_default();
            let caret = |text: &Option<TextPresence>| text.as_ref().map(|t| (t.caret.clone(), t.anchor.clone()));
            let moved_ms = match &before {
                Some(held) if caret(&held.text) == caret(&body.text) => held.moved_ms,
                _ => now,
            };
            all.entry((frame.store_id, frame.doc_id)).or_default().push(Present {
                session: frame.session.clone(),
                who: frame.who.clone(),
                device: body.device,
                focused: body.focused,
                text: body.text,
                board: body.board,
                moved_ms,
                expires_ms: now + frame.ttl_ms as f64,
            });
        }
        all.retain(|_, sessions| !sessions.is_empty());
    });
    text::frames_changed(store);
    board::frames_changed(store);
    follow::frames_changed(store);
}

/// Drop the sessions that went quiet, and everything of a store that closed.
pub(crate) fn prune(store: AppStore) {
    let now = crate::boards::now_ms();
    let stale = store.presence.with(|all| all.values().flatten().any(|p| p.expires_ms <= now));
    if stale {
        store.presence.update(|all| {
            for sessions in all.values_mut() {
                sessions.retain(|p| p.expires_ms > now);
            }
            all.retain(|_, sessions| !sessions.is_empty());
        });
        text::frames_changed(store);
        board::frames_changed(store);
        follow::frames_changed(store);
    }
}

/// Forget a store's presence (it closed here).
pub(crate) fn forget_store(store: AppStore, store_id: StoreId) {
    store.presence.update(|all| all.retain(|(s, _), _| *s != store_id));
    text::frames_changed(store);
    board::frames_changed(store);
    follow::frames_changed(store);
}

/// The people in `(store_id, node_id)`, tracked: one per account (an agent
/// apart from its account, someone signed out per device), in a stable order.
pub fn people_in(store: AppStore, store_id: StoreId, node_id: NodeId) -> Vec<Person> {
    let sessions = store.presence.with(|all| all.get(&(store_id, node_id)).cloned().unwrap_or_default());
    if sessions.is_empty() {
        return Vec::new();
    }
    let me = crate::boards::current_author().map(|a| a.id);
    let mut people: Vec<(Person, Vec<String>)> = Vec::new();
    for p in sessions {
        let key = person_key(&p);
        if let Some((_, devices)) = people.iter_mut().find(|(person, _)| person.key == key) {
            if !devices.contains(&p.device) {
                devices.push(p.device.clone());
            }
            continue;
        }
        let who = identity(store, store_id, node_id, &p, me.as_deref());
        let person = Person { initials: who.initials, color: who.color, title: String::new(), name: who.name, key, agent: who.agent };
        people.push((person, vec![p.device.clone()]));
    }
    let mut out: Vec<Person> = people
        .into_iter()
        .map(|(mut person, devices)| {
            person.title = hover(&person.name, person.agent, &devices);
            person
        })
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.key.cmp(&b.key)));
    out
}

/// What sessions of one person share: their account, an agent apart from
/// its account, the client (one window of one device) for someone signed
/// out, so a signed-out window's pane and what its plugin view says it is on
/// (docs/PRESENCE_CONTRACT.md "Plugins") are one person in one color.
pub(crate) fn person_key(p: &Present) -> String {
    match &p.who {
        Some(PresenceWho { sub, agent: Some(agent) }) => format!("{sub}/agent/{agent}"),
        Some(PresenceWho { sub, agent: None }) if !sub.is_empty() => sub.clone(),
        _ => client_of(&p.session).to_string(),
    }
}

/// The client a session belongs to: everything before its last `:`
/// (`{client}:{pane}`, `{client}:{pane}.in`).
fn client_of(session: &str) -> &str {
    session.rsplit_once(':').map_or(session, |(client, _)| client)
}

/// Who session `p` in `(store_id, node_id)` is: their name there and their
/// color (`author_color` of the account, so the same on every device).
pub fn identity(store: AppStore, store_id: StoreId, node_id: NodeId, p: &Present, me: Option<&str>) -> Identity {
    let key = person_key(p);
    let name = name_of(store, store_id, node_id, p, me);
    let color_key = match &p.who {
        Some(w) if w.agent.is_none() && !w.sub.is_empty() => w.sub.clone(),
        _ => key.clone(),
    };
    let c = pimble_board::author_color(&color_key);
    let agent = p.who.as_ref().and_then(|w| w.agent.as_deref());
    Identity {
        key,
        initials: initials(agent.unwrap_or(&name)),
        name,
        color: format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b),
        color_index: pimble_board::author_color_index(&color_key),
        agent: agent.is_some(),
    }
}

/// What someone is called here (docs/PRESENCE_CONTRACT.md section 3): the
/// name they go by in the note's share, "you" on another device of one's
/// own, an agent by its name, someone signed out by their device.
fn name_of(store: AppStore, store_id: StoreId, node_id: NodeId, p: &Present, me: Option<&str>) -> String {
    match &p.who {
        Some(PresenceWho { sub, agent: Some(agent) }) => {
            if me == Some(sub.as_str()) || sub.is_empty() {
                agent.clone()
            } else {
                let owner = crate::shares::Names::of(store, store_id, node_id).name(sub);
                owner.map(|o| format!("{agent} ({o}'s agent)")).unwrap_or_else(|| agent.clone())
            }
        }
        Some(PresenceWho { sub, agent: None }) if me == Some(sub.as_str()) => "You".to_string(),
        Some(PresenceWho { sub, agent: None }) if !sub.is_empty() => {
            crate::shares::Names::of(store, store_id, node_id).name(sub).unwrap_or_else(|| "Someone".to_string())
        }
        _ if !p.device.is_empty() => p.device.clone(),
        _ => "Someone".to_string(),
    }
}

/// The device label a frame carries: the computer's name, or "Web".
pub fn device_label() -> String {
    #[cfg(feature = "native")]
    {
        let from_env = std::env::var("HOSTNAME").ok().or_else(|| std::env::var("COMPUTERNAME").ok());
        let from_file = || std::fs::read_to_string("/etc/hostname").ok().map(|s| s.trim().to_string());
        from_env.filter(|s| !s.is_empty()).or_else(from_file).filter(|s| !s.is_empty()).unwrap_or_else(|| "a computer".to_string())
    }
    #[cfg(not(feature = "native"))]
    {
        "Web".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present(session: &str, who: Option<PresenceWho>) -> Present {
        Present { session: session.into(), who, device: "laptop".into(), focused: false, text: None, board: None, moved_ms: 0.0, expires_ms: 0.0 }
    }

    #[test]
    fn a_pane_and_what_its_plugin_view_is_on_are_one_person() {
        // Signed out: the pane's session and its plugin view's are one client.
        let pane = present("c1:0", None);
        let within = present(&format!("c1:{}", within_session(PaneId::ALL[0])), None);
        assert_eq!(within.session, "c1:0.in");
        assert_eq!(person_key(&pane), person_key(&within));
        assert_ne!(person_key(&pane), person_key(&present("c2:0", None)));
        // Signed in: the account, whatever the session.
        let sam = Some(PresenceWho { sub: "sam".into(), agent: None });
        assert_eq!(person_key(&present("c1:0.in", sam.clone())), "sam");
        // An agent apart from its account.
        let agent = Some(PresenceWho { sub: "sam".into(), agent: Some("Claude".into()) });
        assert_eq!(person_key(&present("agent:7:n", agent)), "sam/agent/Claude");
    }
}
