//! Following someone (docs/PRESENCE_CONTRACT.md section 7): a pane goes
//! where they go.
//!
//! A click on a person's initials on the pane strip, on the flag of their
//! caret in the text or on the tag of their pointer on a board makes that
//! pane follow them. While it does, the pane opens the document of their
//! focused session whenever it changes, scrolls its text to their first line
//! in view (`viewport_top`; their caret when a frame has none) and takes
//! their board's viewport. Only documents this device can read ever bring
//! frames here, so following never leads anywhere one cannot go.
//!
//! It stops when the person here types, clicks or scrolls in the pane,
//! presses Escape (a key), clicks Stop, opens something else in it or closes
//! it, and when the one followed leaves: the strip then says "Sam left" for
//! a while. Agents cannot be followed. One's own panes in this window never
//! reach `AppStore::presence` (the backend drops this client's frames), so
//! the only "you" that can be followed is you on another device.
//!
//! Where the pane goes by itself (a document opened, a scroll, a camera
//! moved) is "steered": for a moment after, a scroll in the pane is the
//! following's own and stops nothing.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use pimble_core::{NodeId, StoreId};
use pimble_rpc::{BoardPresence, BoardRect, TextPresence};
use rinch::prelude::*;

use super::Present;
use crate::panes::{PaneId, MAX_PANES};
use crate::state::{AppStore, LinkOpen};

/// How long "Sam left" stays on the strip.
const LEFT_SHOWN_MS: u32 = 3000;
/// How long someone may have no session before they count as gone: a pane
/// moving to another note says goodbye to one and hello in the next, and the
/// two frames may arrive a moment apart.
const GONE_AFTER_MS: u32 = 1500;
/// How long after the pane was steered a scroll in it is the following's own.
const STEERED_MS: f64 = 800.0;
/// How often a pane that was just steered to a document looks again for it
/// to be ready to scroll (its session started, the leader's place merged),
/// and how many times.
const SETTLE_EVERY_MS: u32 = 150;
const SETTLE_TRIES: u32 = 40;

/// What a pane is doing about following.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Follow {
    #[default]
    Off,
    On(Leader),
    /// The one followed left: their name, for the strip.
    Left(String),
}

/// The one a pane follows.
#[derive(Debug, Clone, PartialEq)]
pub struct Leader {
    /// [`super::Person::key`]: every session of theirs is a candidate.
    pub key: String,
    pub name: String,
    /// `#rrggbb`, the pane's border.
    pub color: String,
    /// The session followed now, kept while it stays focused.
    pub session: Option<String>,
    /// The document the pane was last steered to.
    pub at: Option<(StoreId, NodeId)>,
}

/// What the sessions say about where a leader is.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Step {
    /// In this document, at these places.
    Go { session: String, store_id: StoreId, node_id: NodeId, text: Option<TextPresence>, board: Option<BoardPresence> },
    /// Here, but no session of theirs has the focus (their focused pane holds
    /// nothing): the pane stays where it is.
    Stay,
    /// No session of theirs anywhere.
    Gone,
}

/// Where leader `key` is among `all` sessions: the session followed now
/// (`current`) while it keeps the focus, else the focused session that spoke
/// last (a person with several devices is followed to the one they use).
pub(crate) fn step(all: &HashMap<(StoreId, NodeId), Vec<Present>>, key: &str, current: Option<&str>) -> Step {
    let theirs: Vec<(&(StoreId, NodeId), &Present)> =
        all.iter().flat_map(|(doc, sessions)| sessions.iter().map(move |p| (doc, p))).filter(|(_, p)| super::person_key(p) == key).collect();
    if theirs.is_empty() {
        return Step::Gone;
    }
    let focused = || theirs.iter().filter(|(_, p)| p.focused);
    let chosen = focused()
        .find(|(_, p)| Some(p.session.as_str()) == current)
        .or_else(|| focused().max_by(|(_, a), (_, b)| a.expires_ms.total_cmp(&b.expires_ms).then_with(|| a.session.cmp(&b.session))));
    match chosen {
        Some(((store_id, node_id), p)) => Step::Go { session: p.session.clone(), store_id: *store_id, node_id: *node_id, text: p.text.clone(), board: p.board.clone() },
        None => Step::Stay,
    }
}

/// Whether session `p` can be followed: not an agent's.
pub(crate) fn followable(p: &Present) -> bool {
    p.who.as_ref().is_none_or(|w| w.agent.is_none())
}

/// What a leader's text part asks a follower to scroll to: their first
/// position in view, or (from a sender that says none) their caret.
pub(crate) fn text_target(text: &TextPresence) -> Option<Vec<u8>> {
    text.viewport_top_bytes().or_else(|| text.caret_bytes().filter(|_| text.has_caret()))
}

thread_local! {
    static FOLLOW: RefCell<[Option<Signal<Follow>>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
    /// Until when (milliseconds since the epoch) a scroll in a pane is the
    /// following's own.
    static STEERED_UNTIL: [Cell<f64>; MAX_PANES] = const { [Cell::new(0.0), Cell::new(0.0), Cell::new(0.0), Cell::new(0.0)] };
    /// What each pane last took from its leader: the text place and the
    /// board viewport, so a heartbeat saying the same moves nothing.
    static TAKEN: RefCell<[(Option<Vec<u8>>, Option<BoardRect>); MAX_PANES]> = RefCell::new(Default::default());
    /// A pane is waiting to see whether its leader is gone.
    static GONE_CHECK: [Cell<bool>; MAX_PANES] = const { [Cell::new(false), Cell::new(false), Cell::new(false), Cell::new(false)] };
    /// A pane is waiting for its document to be ready to scroll.
    static SETTLING: [Cell<bool>; MAX_PANES] = const { [Cell::new(false), Cell::new(false), Cell::new(false), Cell::new(false)] };
}

/// What pane `pane` is doing about following, tracked.
pub fn watch(pane: PaneId) -> Signal<Follow> {
    FOLLOW.with(|f| *f.borrow_mut()[pane.index()].get_or_insert_with(|| Signal::new(Follow::Off)))
}

fn leader(pane: PaneId) -> Option<Leader> {
    match untracked(|| watch(pane).get()) {
        Follow::On(leader) => Some(leader),
        _ => None,
    }
}

/// Watch every pane's document: something else opened in a following pane
/// (the tree, a link, a search hit) stops it. Called once, while the view
/// is built.
pub(crate) fn start_watching(store: AppStore) {
    for pane in PaneId::ALL {
        let _ = rinch::Effect::new(move || {
            let Some(doc) = crate::pane_view::pane_document(store, pane) else { return };
            untracked(|| {
                if let Some(leader) = leader(pane) {
                    if leader.at != Some(doc) {
                        stop(pane);
                    }
                }
            });
        });
    }
}

/// Make pane `pane` follow the person `key` names. An agent cannot be
/// followed; someone with no session here is not followed either.
pub(crate) fn start(store: AppStore, pane: PaneId, key: &str) {
    let all = untracked(|| store.presence.get());
    let Some(((store_id, node_id), p)) = all.iter().flat_map(|(doc, sessions)| sessions.iter().map(move |p| (doc, p))).find(|(_, p)| super::person_key(p) == key) else {
        return;
    };
    if !followable(p) {
        crate::events::show_notice(store, "An agent cannot be followed.".to_string());
        return;
    }
    let me = crate::boards::current_author().map(|a| a.id);
    let who = super::identity(store, *store_id, *node_id, p, me.as_deref());
    let at = untracked(|| crate::pane_view::pane_document(store, pane));
    TAKEN.with(|t| t.borrow_mut()[pane.index()] = Default::default());
    watch(pane).set(Follow::On(Leader { key: key.to_string(), name: who.name, color: who.color, session: None, at }));
    advance(store, pane);
}

/// Stop pane `pane` following (Stop, a key, a click in it, a document
/// opened in it by hand, the pane closed). A "left" notice goes too.
pub(crate) fn stop(pane: PaneId) {
    let signal = watch(pane);
    if untracked(|| signal.get()) != Follow::Off {
        signal.set(Follow::Off);
    }
}

/// The pane scrolled: the person here did it, unless the following did.
pub(crate) fn scrolled(pane: PaneId) {
    if crate::boards::now_ms() >= STEERED_UNTIL.with(|s| s[pane.index()].get()) {
        if leader(pane).is_some() {
            stop(pane);
        }
    }
}

fn steer(pane: PaneId) {
    STEERED_UNTIL.with(|s| s[pane.index()].set(crate::boards::now_ms() + STEERED_MS));
}

/// Frames arrived or went: every following pane goes where its leader is.
pub(crate) fn frames_changed(store: AppStore) {
    for pane in PaneId::ALL {
        if leader(pane).is_some() {
            advance(store, pane);
        }
    }
}

/// Take pane `pane` where its leader is now.
fn advance(store: AppStore, pane: PaneId) {
    let Some(mut leader) = leader(pane) else { return };
    let all = untracked(|| store.presence.get());
    match step(&all, &leader.key, leader.session.as_deref()) {
        Step::Gone => gone_later(store, pane),
        Step::Stay => {}
        Step::Go { session, store_id, node_id, text, board } => {
            let here = untracked(|| crate::pane_view::pane_document(store, pane));
            let moved = leader.at != Some((store_id, node_id));
            if leader.session.as_deref() != Some(session.as_str()) || moved {
                leader.session = Some(session);
                leader.at = Some((store_id, node_id));
                watch(pane).set(Follow::On(leader));
            }
            if here != Some((store_id, node_id)) {
                if moved {
                    open(store, pane, store_id, node_id);
                }
                settle_later(store, pane, 0);
                return;
            }
            if !take_place(store, pane, text.as_ref(), board.as_ref()) {
                settle_later(store, pane, 0);
            }
        }
    }
}

/// Open the leader's document in pane `pane`, the way a followed link opens.
fn open(store: AppStore, pane: PaneId, store_id: StoreId, node_id: NodeId) {
    steer(pane);
    TAKEN.with(|t| t.borrow_mut()[pane.index()] = Default::default());
    store.link_open.set(Some(LinkOpen { value: format!("node_{store_id}_{node_id}"), store_id, node_id, anchor: None, pane: Some(pane) }));
}

/// Go where the leader is in the document pane `pane` shows: their text
/// place, their board viewport. Answers false when the pane is not ready
/// (its session not started, the place not merged here yet).
fn take_place(store: AppStore, pane: PaneId, text: Option<&TextPresence>, board: Option<&BoardPresence>) -> bool {
    let state = store.pane(pane);
    let mut ready = true;
    if let Some(board) = board.filter(|b| b.markup.is_none()) {
        if untracked(|| state.show_board.get()) {
            let taken = TAKEN.with(|t| t.borrow()[pane.index()].1.clone());
            if taken.as_ref() != Some(&board.viewport) {
                steer(pane);
                if crate::boards::take_viewport(store, pane, &board.viewport) {
                    TAKEN.with(|t| t.borrow_mut()[pane.index()].1 = Some(board.viewport.clone()));
                } else {
                    ready = false;
                }
            }
        }
    }
    if let Some(target) = text.and_then(text_target) {
        let shown = untracked(|| state.show_editor.get() && !state.show_board.get());
        let handle = crate::editor::editor(pane);
        if shown {
            let taken = TAKEN.with(|t| t.borrow()[pane.index()].0.clone());
            if taken.as_ref() != Some(&target) {
                match handle.is_collaborating().then(|| handle.collab_locate_sticky_many(&[&target]).into_iter().next().flatten()).flatten() {
                    Some(located) => {
                        steer(pane);
                        let pos = located.pos;
                        handle.scroll_into_view_aligned(pos, pos, crate::rinch_editor::ScrollAlign::Fraction(0.0));
                        TAKEN.with(|t| t.borrow_mut()[pane.index()].0 = Some(target));
                    }
                    None => ready = false,
                }
            }
        }
    }
    ready
}

/// Look again a little later for pane `pane`'s document to be ready to take
/// its leader's place, once however many ask.
fn settle_later(store: AppStore, pane: PaneId, tries: u32) {
    if tries == 0 && SETTLING.with(|s| s[pane.index()].replace(true)) {
        return;
    }
    rinch::core::reactive::unowned(|| {
        set_timeout(SETTLE_EVERY_MS, move || {
            let Some(leader) = leader(pane) else {
                SETTLING.with(|s| s[pane.index()].set(false));
                return;
            };
            steer(pane);
            let all = untracked(|| store.presence.get());
            let done = match step(&all, &leader.key, leader.session.as_deref()) {
                Step::Go { store_id, node_id, text, board, .. } => {
                    untracked(|| crate::pane_view::pane_document(store, pane)) == Some((store_id, node_id)) && take_place(store, pane, text.as_ref(), board.as_ref())
                }
                _ => true,
            };
            if done || tries + 1 >= SETTLE_TRIES {
                SETTLING.with(|s| s[pane.index()].set(false));
            } else {
                settle_later(store, pane, tries + 1);
            }
        })
    });
}

/// The leader has no session: if they still have none in a moment, they
/// left, and the strip says so for a while.
fn gone_later(store: AppStore, pane: PaneId) {
    if GONE_CHECK.with(|g| g[pane.index()].replace(true)) {
        return;
    }
    rinch::core::reactive::unowned(|| {
        set_timeout(GONE_AFTER_MS, move || {
            GONE_CHECK.with(|g| g[pane.index()].set(false));
            let Some(leader) = leader(pane) else { return };
            let all = untracked(|| store.presence.get());
            if step(&all, &leader.key, leader.session.as_deref()) != Step::Gone {
                return;
            }
            let name = leader.name.clone();
            watch(pane).set(Follow::Left(name.clone()));
            rinch::core::reactive::unowned(|| {
                set_timeout(LEFT_SHOWN_MS, move || {
                    if untracked(|| watch(pane).get()) == Follow::Left(name) {
                        watch(pane).set(Follow::Off);
                    }
                })
            });
        })
    });
}

/// What pane `pane`'s strip says about following, tracked: "Following Sam",
/// "Sam left", or nothing.
pub fn strip_text(pane: PaneId) -> Option<String> {
    match watch(pane).get() {
        Follow::Off => None,
        Follow::On(leader) => Some(format!("Following {}", leader.name)),
        Follow::Left(name) => Some(format!("{name} left")),
    }
}

/// The color of pane `pane`'s border while it follows someone, tracked.
pub fn border_color(pane: PaneId) -> Option<String> {
    match watch(pane).get() {
        Follow::On(leader) => Some(leader.color),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_rpc::PresenceWho;

    fn present(session: &str, sub: &str, focused: bool, expires_ms: f64) -> Present {
        Present {
            session: session.to_string(),
            who: Some(PresenceWho { sub: sub.to_string(), agent: None }),
            device: "laptop".to_string(),
            focused,
            text: None,
            board: None,
            moved_ms: 0.0,
            expires_ms,
        }
    }

    fn docs() -> (StoreId, NodeId, NodeId) {
        (StoreId::new(), NodeId::new(), NodeId::new())
    }

    #[test]
    fn the_focused_session_is_where_they_are() {
        let (s, a, b) = docs();
        let all = HashMap::from([
            ((s, a), vec![present("sam1:0", "sam", false, 10.0), present("ann1:0", "ann", true, 50.0)]),
            ((s, b), vec![present("sam1:1", "sam", true, 10.0)]),
        ]);
        match step(&all, "sam", None) {
            Step::Go { session, node_id, .. } => {
                assert_eq!(session, "sam1:1");
                assert_eq!(node_id, b);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn of_two_focused_devices_the_one_followed_is_kept_then_the_latest() {
        let (s, a, b) = docs();
        let all = HashMap::from([((s, a), vec![present("laptop:0", "sam", true, 10.0)]), ((s, b), vec![present("web:0", "sam", true, 90.0)])]);
        // A new follow goes to the one that spoke last.
        assert!(matches!(step(&all, "sam", None), Step::Go { session, .. } if session == "web:0"));
        // One already followed is kept while it keeps the focus.
        assert!(matches!(step(&all, "sam", Some("laptop:0")), Step::Go { session, .. } if session == "laptop:0"));
    }

    #[test]
    fn no_focused_session_stays_and_no_session_is_gone() {
        let (s, a, _) = docs();
        let all = HashMap::from([((s, a), vec![present("sam1:0", "sam", false, 10.0)])]);
        assert_eq!(step(&all, "sam", Some("sam1:0")), Step::Stay);
        assert_eq!(step(&all, "ann", None), Step::Gone);
        assert_eq!(step(&HashMap::new(), "sam", None), Step::Gone);
    }

    #[test]
    fn a_session_that_moved_to_another_note_takes_the_pane_there() {
        let (s, a, b) = docs();
        let mut all = HashMap::from([((s, a), vec![present("sam1:0", "sam", true, 10.0)])]);
        assert!(matches!(step(&all, "sam", None), Step::Go { node_id, .. } if node_id == a));
        all.clear();
        all.insert((s, b), vec![present("sam1:0", "sam", true, 20.0)]);
        assert!(matches!(step(&all, "sam", Some("sam1:0")), Step::Go { node_id, .. } if node_id == b));
    }

    #[test]
    fn agents_cannot_be_followed() {
        let mut agent = present("agent:x:n", "sam", true, 10.0);
        agent.who = Some(PresenceWho { sub: "sam".into(), agent: Some("Claude".into()) });
        assert!(!followable(&agent));
        assert!(followable(&present("sam1:0", "sam", true, 10.0)));
        // Signed out: followed by device.
        let mut anon = present("x:0", "", true, 10.0);
        anon.who = None;
        assert!(followable(&anon));
    }

    #[test]
    fn the_text_target_is_the_first_line_in_view_or_the_caret() {
        let with_top = TextPresence::new(&[1, 2], None).with_viewport_top(Some(&[9]));
        assert_eq!(text_target(&with_top), Some(vec![9]));
        let caret_only = TextPresence::new(&[1, 2], None);
        assert_eq!(text_target(&caret_only), Some(vec![1, 2]));
        let nothing = TextPresence::new(&[], None);
        assert_eq!(text_target(&nothing), None);
    }

    /// The whole machine on an app store: following opens the leader's
    /// focused document in the pane, moves with them to the next, and a
    /// stop (a key, a click, Stop) ends it; an agent is refused.
    #[test]
    fn following_opens_their_document_and_stops_when_told() {
        let store = AppStore::new();
        let pane = PaneId::ALL[1];
        let (s, a, b) = docs();
        store.presence.set(HashMap::from([((s, a), vec![present("sam1:0", "sam", true, 10.0)])]));
        start(store, pane, "sam");
        let Follow::On(leader) = watch(pane).get() else { panic!("following") };
        assert_eq!(leader.session.as_deref(), Some("sam1:0"));
        assert_eq!(leader.at, Some((s, a)));
        let open = store.link_open.get().expect("their note opens");
        assert_eq!((open.store_id, open.node_id, open.pane), (s, a, Some(pane)));
        store.link_open.set(None);

        // They open another note: the pane goes there.
        store.presence.set(HashMap::from([((s, b), vec![present("sam1:0", "sam", true, 20.0)])]));
        frames_changed(store);
        assert_eq!(store.link_open.get().map(|o| o.node_id), Some(b));
        store.link_open.set(None);

        // A scroll the following made stops nothing; one a moment later does.
        scrolled(pane);
        assert!(matches!(watch(pane).get(), Follow::On(_)));
        STEERED_UNTIL.with(|s| s[pane.index()].set(0.0));
        scrolled(pane);
        assert_eq!(watch(pane).get(), Follow::Off);

        // A key or a click (`stop`) ends it too, and nothing opens after.
        start(store, pane, "sam");
        store.link_open.set(None);
        stop(pane);
        assert_eq!(watch(pane).get(), Follow::Off);
        frames_changed(store);
        assert_eq!(store.link_open.get(), None);

        // An agent is not followed.
        let mut agent = present("agent:c:n", "sam", true, 10.0);
        agent.who = Some(PresenceWho { sub: "sam".into(), agent: Some("Claude".into()) });
        store.presence.set(HashMap::from([((s, a), vec![agent])]));
        start(store, pane, "sam/agent/Claude");
        assert_eq!(watch(pane).get(), Follow::Off);
    }
}
