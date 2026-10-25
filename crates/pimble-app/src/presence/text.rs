//! Carets and selections in a note's text (docs/PRESENCE_CONTRACT.md
//! section 4).
//!
//! Saying where this window's panes are: a pane's caret and selection go out
//! as yrs sticky indexes into the note's content, on every move, at most one
//! frame per 50 ms (`presence::moved`), and with every frame the pane sends anyway
//! (`presence::say`, the heartbeat). A read-only pane says where it is only
//! while it has a selection.
//!
//! Showing where everyone else is: each pane resolves the indexes of the
//! sessions in its note (`collab_locate_sticky_many`). A caret is a bar in
//! the person's color with a flag, drawn over the text at `caret_rect` (the
//! window frame, like the floating controls over a picture) and read again
//! whenever something may have moved it: a frame, a margin report (the
//! carets' lines are margin anchors, so the editor says when they move), a
//! caret move or a peer's edit here, a scroll. A selection is an `Inline`
//! decoration from [`PresencePlugin`], one class per author color. A position
//! that no longer resolves is drawn at the last place it was, dimmed, until
//! the next frame.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use pimble_rpc::TextPresence;
use rinch::prelude::*;
use rinch_editor_collab::StickyAssoc;
use rinch_editor_core::state::{EditorState, Transaction};
use rinch_editor_core::{Attrs, Decoration, DecorationSet, Node as EditorNode, Plugin, PluginKey, Pos};

use crate::panes::{PaneId, MAX_PANES};
use crate::state::AppStore;

/// How long a caret's flag names the person after it moves; then initials.
const NAME_SHOWN_MS: f64 = 3000.0;
/// The margin anchors' key for carets (never a node id: the discussion
/// markers skip it).
pub(crate) const MARGIN_KEY: &str = "presence";

/// Someone else's caret as a pane draws it.
#[derive(Debug, Clone, PartialEq)]
pub struct Caret {
    pub session: String,
    /// Who it is ([`super::Person::key`]): a click on the flag follows them.
    pub key: String,
    pub class: &'static str,
    /// Its place and color, in the window frame.
    pub style: String,
    pub flag_style: String,
    /// The name for a while after it moves, then initials.
    pub label: String,
    /// Name and device, for the hover.
    pub title: String,
    /// An AI agent: a robot glyph on the flag, and no following.
    pub agent: bool,
}

/// Selections handed to a pane's [`PresencePlugin`]: `(from, to, color)`.
#[derive(Debug, Clone, Default, PartialEq)]
struct Ranges(Vec<(usize, usize, usize)>);

const PLUGIN_KEY: PluginKey = PluginKey("pimble.presence");
const META: &str = "pimble.presence";

thread_local! {
    /// A pane's carets are owed a look.
    static REDRAW_OWED: [Cell<bool>; MAX_PANES] = const { [Cell::new(false), Cell::new(false), Cell::new(false), Cell::new(false)] };
    /// A pane is waiting to turn a flag's name into initials.
    static FLAG_TIMER: [Cell<bool>; MAX_PANES] = const { [Cell::new(false), Cell::new(false), Cell::new(false), Cell::new(false)] };
    /// What each pane draws.
    static CARETS: RefCell<[Option<Signal<Vec<Caret>>>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
    /// The selections each pane's editor was last given.
    static GIVEN: RefCell<[Ranges; MAX_PANES]> = RefCell::new(Default::default());
    /// Where each session's caret last resolved in each pane: the place a
    /// caret whose position is gone is drawn, dimmed.
    static LAST: RefCell<[HashMap<String, usize>; MAX_PANES]> = RefCell::new(Default::default());
}

// ── This window's panes ──────────────────────────────────────────────────

/// Where pane `pane`'s editor is, as a frame says it: its caret and
/// selection in the note it holds, and the first position in view (what a
/// follower scrolls to); `None` when it holds none or is not collaborating.
/// A pane that may only read the note and has nothing selected says only
/// what it shows.
pub(crate) fn mine(store: AppStore, pane: PaneId) -> Option<TextPresence> {
    let state = store.pane(pane);
    let active = untracked(|| state.active_edit.get())?;
    if !untracked(|| state.show_editor.get()) {
        return None;
    }
    let handle = crate::editor::editor(pane);
    if !handle.is_collaborating() {
        return None;
    }
    let selection = handle.selection();
    let (head, anchor) = (selection.head(), selection.anchor());
    let top = viewport_top(pane, &handle).and_then(|pos| handle.collab_sticky_index_with(Pos(pos), StickyAssoc::After));
    if head == anchor && !store.node_access(active.store_id, active.node_id).allows_write() {
        return top.map(|top| TextPresence::new(&[], None).with_viewport_top(Some(&top)));
    }
    let caret = handle.collab_sticky_index_with(head, StickyAssoc::After)?;
    let anchor = if anchor == head { None } else { handle.collab_sticky_index_with(anchor, StickyAssoc::After) };
    Some(TextPresence::new(&caret, anchor.as_deref()).with_viewport_top(top.as_deref()))
}

/// The first position of pane `pane`'s note in view: on the first line whose
/// bottom is below the top of the text's box.
pub(crate) fn viewport_top(pane: PaneId, handle: &crate::rinch_editor::EditorHandle) -> Option<usize> {
    let (_, top, _, _) = text_rect(pane)?;
    let doc = handle.doc();
    let mut blocks = Vec::new();
    doc.nodes_between(0, doc.content_size(), &mut |node, pos, _| {
        if node.is_textblock() {
            blocks.push((pos + 1, pos + 1 + node.content_size()));
            return false;
        }
        true
    });
    first_in_view(&blocks, top, |pos| handle.caret_rect(Pos(pos)).map(|r| (r.y, r.height)))
}

/// The first position on a line reaching below `top`, of the textblocks
/// `blocks` (each its content's `(start, end)`, in document order), where
/// `rect` says the top and height of the line a position is drawn on, or
/// `None` for one with no geometry (a block not laid out). The editor offers
/// no hit-test for a point, so this searches by caret rectangles: over the
/// blocks for the last one starting at or above `top`, then inside it for
/// the first of its lines in view. `None` when nothing has geometry.
pub(crate) fn first_in_view(blocks: &[(usize, usize)], top: f32, rect: impl Fn(usize) -> Option<(f32, f32)>) -> Option<usize> {
    // Whether block `i`, or the first block after it with geometry, starts
    // below `top`: false up to some block and true from it on.
    let starts_below = |i: usize| -> bool {
        blocks[i..].iter().find_map(|&(start, _)| rect(start)).is_none_or(|(y, _)| y > top)
    };
    let (mut lo, mut hi) = (0, blocks.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        if starts_below(mid) {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    // `lo` is the first block that starts below; the one that holds `top`
    // is the last before it with geometry.
    let first_below = blocks[lo..].iter().find(|&&(start, _)| rect(start).is_some()).map(|&(start, _)| start);
    let Some(&(start, end)) = blocks[..lo].iter().rev().find(|&&(start, _)| rect(start).is_some()) else {
        return first_below;
    };
    let reaches = |pos: usize| rect(pos).is_some_and(|(y, h)| y + h > top);
    if !reaches(end) {
        // All of it is above: the next block, or (scrolled past the end) its end.
        return first_below.or(Some(end));
    }
    let (mut lo, mut hi) = (start, end);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if reaches(mid) {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Some(lo)
}

// ── Everyone else ────────────────────────────────────────────────────────

/// The editor plugin that draws other people's selections. Added to every
/// pane's editor before any content (adding a plugin resets history); its
/// ranges arrive in a transaction's meta and are carried through every
/// change until the next arrive.
pub struct PresencePlugin;

impl Plugin for PresencePlugin {
    fn key(&self) -> PluginKey {
        PLUGIN_KEY
    }

    fn init_state(&self, _doc: &EditorNode) -> Option<Rc<dyn std::any::Any>> {
        Some(Rc::new(Ranges::default()))
    }

    fn apply(&self, tr: &Transaction, _old: &EditorState, _new_doc: &EditorNode, prev: Option<&dyn std::any::Any>) -> Option<Rc<dyn std::any::Any>> {
        if let Some(given) = tr.get_meta(META).and_then(|m| m.downcast_ref::<Ranges>()) {
            return Some(Rc::new(given.clone()));
        }
        let prev = prev.and_then(|p| p.downcast_ref::<Ranges>()).cloned().unwrap_or_default();
        if !tr.doc_changed() {
            return Some(Rc::new(prev));
        }
        let mapping = tr.mapping();
        let mapped = prev
            .0
            .into_iter()
            .map(|(from, to, color)| (mapping.map(from, 1), mapping.map(to, -1), color))
            .filter(|(from, to, _)| from < to)
            .collect();
        Some(Rc::new(Ranges(mapped)))
    }

    fn decorations(&self, state: &EditorState) -> DecorationSet {
        let Some(ranges) = state.plugin_state::<Ranges>(PLUGIN_KEY) else { return DecorationSet::empty() };
        let size = state.doc.content_size();
        DecorationSet::new(
            ranges
                .0
                .iter()
                .filter_map(|&(from, to, color)| {
                    let (from, to) = (from.min(size), to.min(size));
                    (from < to).then(|| Decoration::inline(Pos(from), Pos(to), Attrs::new().with("class", selection_class(color))))
                })
                .collect(),
        )
    }
}

/// The class of a selection in author color `color`.
pub fn selection_class(color: usize) -> String {
    format!("pimble-presence-selection pimble-presence--c{color}")
}

/// What pane `pane` draws, tracked. Rendered once per pane.
pub(crate) fn watch(pane: PaneId) -> Signal<Vec<Caret>> {
    carets(pane)
}

fn carets(pane: PaneId) -> Signal<Vec<Caret>> {
    CARETS.with(|c| *c.borrow_mut()[pane.index()].get_or_insert_with(|| Signal::new(Vec::new())))
}

/// Frames arrived or went: every pane looks again.
pub(crate) fn frames_changed(store: AppStore) {
    for pane in PaneId::ALL {
        redraw_later(store, pane);
    }
}

/// Something may have moved the carets pane `pane` draws: they are read
/// again a turn later (after the layout the change owes), once however many
/// ask.
pub(crate) fn redraw_later(store: AppStore, pane: PaneId) {
    if REDRAW_OWED.with(|o| o[pane.index()].replace(true)) {
        return;
    }
    rinch::core::reactive::unowned(|| {
        set_timeout(0, move || {
            REDRAW_OWED.with(|o| o[pane.index()].set(false));
            redraw(store, pane);
        })
    });
}

/// One session's resolved place in a pane's note.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Placed {
    pub caret: usize,
    /// The caret's own character is gone, or it did not resolve at all:
    /// drawn where it was, dimmed.
    pub dim: bool,
    /// The selection, `from < to`.
    pub selection: Option<(usize, usize)>,
}

/// Where each of `texts` is, from what the editor resolved (`located`: the
/// caret and anchor of each, flattened in that order, `None` for an anchor
/// that is not there) and where each was last (`last`, by session). A caret
/// that does not resolve is at its last place, clamped into a document of
/// `size`, dimmed; one with no last place is not drawn. A selection needs
/// both ends.
pub(crate) fn place(texts: &[(String, bool)], located: &[Option<(usize, bool)>], last: &HashMap<String, usize>, size: usize) -> Vec<Option<Placed>> {
    let mut at = located.iter();
    texts
        .iter()
        .map(|(session, has_anchor)| {
            let caret = at.next().copied().flatten();
            let anchor = if *has_anchor { at.next().copied().flatten() } else { None };
            let (pos, dim) = match caret {
                Some((pos, displaced)) => (pos, displaced),
                None => (last.get(session).copied()?.min(size), true),
            };
            let selection = caret.zip(anchor).map(|((c, _), (a, _))| (c.min(a), c.max(a))).filter(|(from, to)| from < to);
            Some(Placed { caret: pos, dim, selection })
        })
        .collect()
}

/// Read again where everyone else is in pane `pane`'s note and draw it.
fn redraw(store: AppStore, pane: PaneId) {
    let state = store.pane(pane);
    let held = untracked(|| state.active_edit.get()).filter(|_| untracked(|| state.show_editor.get() && !state.show_board.get()));
    let handle = crate::editor::editor(pane);
    let Some(active) = held.filter(|_| handle.is_collaborating()) else {
        show(store, pane, Vec::new(), Ranges::default(), Vec::new());
        return;
    };
    let sessions: Vec<super::Present> = untracked(|| store.presence.with(|all| all.get(&(active.store_id, active.node_id)).cloned().unwrap_or_default()))
        .into_iter()
        .filter(|p| p.text.as_ref().is_some_and(TextPresence::has_caret))
        .collect();
    let mut bytes: Vec<Vec<u8>> = Vec::new();
    let mut texts: Vec<(String, bool)> = Vec::new();
    let mut stickies: Vec<Vec<u8>> = Vec::new();
    for p in &sessions {
        let text = p.text.as_ref().expect("filtered");
        let caret = text.caret_bytes().unwrap_or_default();
        stickies.push(caret.clone());
        bytes.push(caret);
        let anchor = text.anchor_bytes();
        texts.push((p.session.clone(), anchor.is_some()));
        bytes.extend(anchor);
    }
    let refs: Vec<&[u8]> = bytes.iter().map(Vec::as_slice).collect();
    let located: Vec<Option<(usize, bool)>> = handle.collab_locate_sticky_many(&refs).into_iter().map(|l| l.map(|l| (l.pos.0, l.displaced))).collect();
    let size = handle.doc().content_size();
    let placed = LAST.with(|last| place(&texts, &located, &last.borrow()[pane.index()], size));
    LAST.with(|last| {
        let last = &mut last.borrow_mut()[pane.index()];
        last.retain(|session, _| sessions.iter().any(|p| &p.session == session));
        for (p, placed) in sessions.iter().zip(&placed) {
            if let Some(placed) = placed.as_ref().filter(|placed| !placed.dim) {
                last.insert(p.session.clone(), placed.caret);
            }
        }
    });

    let now = crate::boards::now_ms();
    let me = crate::boards::current_author().map(|a| a.id);
    let clip = text_rect(pane);
    let mut drawn = Vec::new();
    let mut ranges = Ranges::default();
    let mut name_until: Option<f64> = None;
    for (p, placed) in sessions.iter().zip(placed) {
        let Some(placed) = placed else { continue };
        let who = super::identity(store, active.store_id, active.node_id, p, me.as_deref());
        if let Some((from, to)) = placed.selection {
            ranges.0.push((from, to, who.color_index));
        }
        let Some(rect) = handle.caret_rect(Pos(placed.caret)) else { continue };
        // Scrolled out of the text's box: not drawn over the strip or the
        // toolbar.
        if let Some((_, top, _, height)) = clip {
            if rect.y + rect.height < top || rect.y > top + height {
                continue;
            }
        }
        let fresh = now - p.moved_ms < NAME_SHOWN_MS;
        if fresh {
            let until = p.moved_ms + NAME_SHOWN_MS;
            name_until = Some(name_until.map_or(until, |u: f64| u.min(until)));
        }
        let title = super::hover(&who.name, who.agent, std::slice::from_ref(&p.device));
        drawn.push(Caret {
            session: p.session.clone(),
            key: who.key.clone(),
            class: if placed.dim { "pimble-presence-caret pimble-presence-caret--dim" } else { "pimble-presence-caret" },
            style: format!("left: {:.1}px; top: {:.1}px; height: {:.1}px; background: {};", rect.x - 1.0, rect.y, rect.height.max(1.0), who.color),
            flag_style: format!("background: {};", who.color),
            label: if fresh { who.name.clone() } else { who.initials.clone() },
            title,
            agent: who.agent,
        });
    }
    if let Some(until) = name_until {
        if !FLAG_TIMER.with(|t| t[pane.index()].replace(true)) {
            let wait = (until - now).max(0.0) as u32 + 20;
            rinch::core::reactive::unowned(|| {
                set_timeout(wait, move || {
                    FLAG_TIMER.with(|t| t[pane.index()].set(false));
                    redraw(store, pane);
                })
            });
        }
    }
    show(store, pane, drawn, ranges, stickies);
}

/// Hand pane `pane` what it draws: the carets, its editor's selections and
/// the carets' margin anchors, each only when it changed.
fn show(_store: AppStore, pane: PaneId, drawn: Vec<Caret>, ranges: Ranges, stickies: Vec<Vec<u8>>) {
    let signal = carets(pane);
    if untracked(|| signal.get()) != drawn {
        signal.set(drawn);
    }
    crate::discussions::margin::set_presence_anchors(pane, stickies);
    let changed = GIVEN.with(|given| {
        let slot = &mut given.borrow_mut()[pane.index()];
        if *slot == ranges {
            return false;
        }
        *slot = ranges.clone();
        true
    });
    if changed {
        crate::editor::editor(pane).update(move |state| {
            let mut tr = state.tr();
            tr.set_meta(META, ranges);
            Some(tr)
        });
    }
}

/// Pane `pane`'s text box in the window frame `caret_rect` answers in:
/// `(x, y, width, height)`, `None` when it is not known.
fn text_rect(pane: PaneId) -> Option<(f32, f32, f32, f32)> {
    #[cfg(feature = "native")]
    {
        let bounds = crate::text_boards::text_bounds(pane).map(|b| untracked(|| b.get()))?;
        (bounds.height > 0.0).then_some((bounds.x, bounds.y, bounds.width, bounds.height))
    }
    #[cfg(not(feature = "native"))]
    {
        let document = web_sys::window()?.document()?;
        let element = document.query_selector(&format!("[data-pimble-text=\"{}\"]", pane.index())).ok()??;
        let rect = element.get_bounding_client_rect();
        (rect.height() > 0.0).then_some((rect.left() as f32, rect.top() as f32, rect.width() as f32, rect.height() as f32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_caret_that_resolves_is_drawn_there_with_its_selection() {
        let texts = vec![("a:0".to_string(), true), ("b:1".to_string(), false)];
        let located = vec![Some((9, false)), Some((4, false)), Some((2, false))];
        let placed = place(&texts, &located, &HashMap::new(), 20);
        assert_eq!(placed[0], Some(Placed { caret: 9, dim: false, selection: Some((4, 9)) }));
        assert_eq!(placed[1], Some(Placed { caret: 2, dim: false, selection: None }));
    }

    #[test]
    fn a_caret_that_is_gone_is_drawn_dimmed_where_it_was() {
        let texts = vec![("a:0".to_string(), true), ("b:1".to_string(), false), ("c:2".to_string(), false)];
        // a's block was deleted: nearest place, dimmed, still a selection;
        // b's item has not arrived: its last place, clamped; c was never seen.
        let located = vec![Some((5, true)), Some((3, false)), None, None];
        let last = HashMap::from([("b:1".to_string(), 40)]);
        let placed = place(&texts, &located, &last, 12);
        assert_eq!(placed[0], Some(Placed { caret: 5, dim: true, selection: Some((3, 5)) }));
        assert_eq!(placed[1], Some(Placed { caret: 12, dim: true, selection: None }));
        assert_eq!(placed[2], None);
    }

    /// Lines 20 px tall from y = 100, a block per paragraph: positions 1..=10
    /// on two lines (1..=5 and 6..=10), 12..=14 on one, 16..=20 on one.
    fn lines(pos: usize) -> Option<(f32, f32)> {
        let line = match pos {
            1..=5 => 0,
            6..=10 => 1,
            12..=14 => 2,
            16..=20 => 3,
            _ => return None,
        };
        Some((100.0 + 20.0 * line as f32, 20.0))
    }
    const BLOCKS: [(usize, usize); 3] = [(1, 10), (12, 14), (16, 20)];

    #[test]
    fn the_first_position_in_view_is_on_the_first_line_reaching_below_the_top() {
        // Not scrolled: the start.
        assert_eq!(first_in_view(&BLOCKS, 90.0, lines), Some(1));
        // The top halfway down the first line: still that line.
        assert_eq!(first_in_view(&BLOCKS, 110.0, lines), Some(1));
        // Past the first paragraph's first line: its second line.
        assert_eq!(first_in_view(&BLOCKS, 125.0, lines), Some(6));
        // Exactly at the bottom of its second line: the next paragraph.
        assert_eq!(first_in_view(&BLOCKS, 140.0, lines), Some(12));
        assert_eq!(first_in_view(&BLOCKS, 165.0, lines), Some(16));
        // Scrolled past the end: the end.
        assert_eq!(first_in_view(&BLOCKS, 500.0, lines), Some(20));
        // Nothing laid out.
        assert_eq!(first_in_view(&BLOCKS, 0.0, |_| None), None);
    }

    #[test]
    fn blocks_with_no_geometry_are_passed_over() {
        // The first paragraph is not laid out (virtualized above the view).
        let rect = |pos: usize| if pos <= 10 { None } else { lines(pos) };
        assert_eq!(first_in_view(&BLOCKS, 130.0, rect), Some(12));
        assert_eq!(first_in_view(&BLOCKS, 150.0, rect), Some(12));
        assert_eq!(first_in_view(&BLOCKS, 170.0, rect), Some(16));
        // The last one is not laid out either.
        let rect = |pos: usize| if pos >= 16 { None } else { lines(pos) };
        assert_eq!(first_in_view(&BLOCKS, 125.0, rect), Some(6));
        assert_eq!(first_in_view(&BLOCKS, 165.0, rect), Some(14));
    }

    #[test]
    fn a_selection_needs_both_ends() {
        let texts = vec![("a:0".to_string(), true)];
        let placed = place(&texts, &[Some((7, false)), None], &HashMap::new(), 20);
        assert_eq!(placed[0], Some(Placed { caret: 7, dim: false, selection: None }));
    }

    /// The whole way a position travels: a sticky index made by one editor
    /// is found by a peer's, follows typing before it on either side, and a
    /// selection handed to the plugin is drawn where it was given and carried
    /// through the next edit.
    #[cfg(feature = "native")]
    #[test]
    fn a_peers_caret_and_selection_resolve_and_follow_edits() {
        use rinch_editor_core::selection::Selection;
        let typed = |handle: &crate::rinch_editor::EditorHandle, text: &'static str, at: usize| {
            assert!(handle.update(move |s| {
                let mut tr = s.tr();
                tr.set_selection(Selection::cursor(Pos(at)));
                tr.insert_text(text).ok()?;
                Some(tr)
            }));
        };
        let theirs = crate::rinch_editor::create_editor();
        typed(&theirs, "hello world", 1);
        let to_mine = Rc::new(RefCell::new(Vec::<Vec<u8>>::new()));
        let out = to_mine.clone();
        theirs.start_collaboration_host(move |d| out.borrow_mut().push(d)).unwrap();
        let mine = crate::rinch_editor::create_editor();
        assert!(mine.add_plugin(Rc::new(PresencePlugin)));
        let to_theirs = Rc::new(RefCell::new(Vec::<Vec<u8>>::new()));
        let out = to_theirs.clone();
        mine.start_collaboration_guest(&theirs.collab_snapshot().unwrap(), move |d| out.borrow_mut().push(d)).unwrap();

        // They select "world" (7..12), the caret at its end.
        let caret = theirs.collab_sticky_index_with(Pos(12), StickyAssoc::After).unwrap();
        let anchor = theirs.collab_sticky_index_with(Pos(7), StickyAssoc::After).unwrap();
        let body = pimble_rpc::PresenceBody { device: "d".into(), focused: true, text: Some(TextPresence::new(&caret, Some(&anchor))), board: None };
        let text = pimble_rpc::PresenceBody::decode(&body.encode()).unwrap().text.unwrap();
        let (caret, anchor) = (text.caret_bytes().unwrap(), text.anchor_bytes().unwrap());
        let found = |handle: &crate::rinch_editor::EditorHandle| -> Vec<Option<(usize, bool)>> {
            handle.collab_locate_sticky_many(&[&caret, &anchor]).into_iter().map(|l| l.map(|l| (l.pos.0, l.displaced))).collect()
        };
        assert_eq!(found(&mine), vec![Some((12, false)), Some((7, false))]);

        // Typing before it on this side, then on theirs: it moves with the words.
        typed(&mine, ">> ", 1);
        assert_eq!(found(&mine), vec![Some((15, false)), Some((10, false))]);
        typed(&theirs, "oh, ", 1);
        for d in to_mine.borrow_mut().drain(..) {
            mine.collab_receive(&d);
        }
        assert_eq!(found(&mine), vec![Some((19, false)), Some((14, false))]);

        // The plugin draws the selection given, and carries it through an edit.
        let placed = place(&[("s:0".to_string(), true)], &found(&mine), &HashMap::new(), mine.doc().content_size());
        let (from, to) = placed[0].as_ref().unwrap().selection.unwrap();
        assert!(mine.update(move |s| {
            let mut tr = s.tr();
            tr.set_meta(META, Ranges(vec![(from, to, 3)]));
            Some(tr)
        }));
        let drawn = |handle: &crate::rinch_editor::EditorHandle| -> Vec<(usize, usize, Option<String>)> {
            handle.state().decorations().iter().filter_map(|d| d.inline_class().map(|c| (d.range().0 .0, d.range().1 .0, Some(c.to_string())))).collect()
        };
        assert_eq!(drawn(&mine), vec![(14, 19, Some(selection_class(3)))]);
        typed(&mine, "!", 1);
        assert_eq!(drawn(&mine), vec![(15, 20, Some(selection_class(3)))]);
    }
}
