//! Discussions in a note's margin (docs/DISCUSSIONS_CONTRACT.md section 6):
//! a marker beside the line each discussion anchored in the text is on, with
//! its count and an unread dot; a press opens it beside the note.
//!
//! The pane's editor is given the anchors (rinch `set_margin_anchors`, the
//! discussions' own sticky indexes, keyed by node id) and reports their line
//! boxes (`on_margin_layout`); the markers are the editor's overlay slot, so
//! they scroll with the text and never touch the document. A discussion
//! shown inline in the pane's text is not also in its margin.

use std::cell::{Cell, RefCell};

use pimble_core::{NodeId, StoreId};
use pimble_crdt::NodeDoc;
use pimble_discussion::{Anchor, Discussion};
use rinch::prelude::*;

use crate::panes::PaneId;
use crate::rinch_editor::{MarginAnchor, MarginLine};
use crate::state::AppStore;

/// One marker: the discussions whose anchors are on one line.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Marker {
    pub top: f32,
    pub height: f32,
    pub store_id: StoreId,
    /// In the order they were made.
    pub ids: Vec<NodeId>,
    /// Their live messages, together.
    pub count: usize,
    /// The first with an unread message, which a press opens.
    pub unread: Option<NodeId>,
    pub title: String,
    /// Its place in the overlay slot: in the gutter right of the text,
    /// centred on its line.
    pub style: String,
    /// What it says: the count, when there are messages.
    pub label: String,
    pub class: &'static str,
    /// What a press opens: its first discussion with an unread message, or
    /// its first.
    pub opens: NodeId,
}

thread_local! {
    /// Bumped when an inline discussion view opens or closes, which takes it
    /// out of the margin or puts it back.
    static INLINE_SHOWN: Cell<Option<Signal<u64>>> = const { Cell::new(None) };
    /// The anchors each pane's editor was last given.
    static GIVEN: RefCell<[Vec<(NodeId, Vec<u8>)>; crate::panes::MAX_PANES]> = RefCell::new(Default::default());
    /// Other people's carets in each pane's note: anchors too, so the editor
    /// says when their lines move (docs/PRESENCE_CONTRACT.md section 4).
    static PRESENCE: RefCell<[Vec<Vec<u8>>; crate::panes::MAX_PANES]> = RefCell::new(Default::default());
}

fn inline_shown() -> Signal<u64> {
    INLINE_SHOWN.with(|cell| match cell.get() {
        Some(signal) => signal,
        None => {
            let signal = Signal::new(0);
            cell.set(Some(signal));
            signal
        }
    })
}

/// An inline discussion view opened or closed somewhere.
pub(crate) fn inline_changed() {
    let signal = inline_shown();
    untracked(|| signal.update(|v| *v += 1));
}

/// The discussions on the note pane `pane` shows that are anchored in its
/// text and not shown inline there, with their sticky indexes. Tracked.
fn anchored(store: AppStore, pane: PaneId) -> (Option<StoreId>, Vec<(NodeId, Vec<u8>)>) {
    let state = store.pane(pane);
    let Some(active) = state.active_edit.get() else { return (None, Vec::new()) };
    if !state.show_editor.get() {
        return (None, Vec::new());
    }
    inline_shown().get();
    let list = super::host_discussions(store, active.store_id, active.node_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|(id, _)| !super::inline_in(pane, active.store_id, *id))
        .filter_map(|(id, node)| {
            let doc = NodeDoc::load(&node?.content).ok()?;
            match Discussion::of_node(&doc).read().anchor {
                Some(Anchor::Text { sticky }) => Some((id, sticky)),
                _ => None,
            }
        })
        .collect();
    (Some(active.store_id), list)
}

/// Pane `pane`'s margin: the anchors kept in step with its note's
/// discussions, and the markers drawn from where the editor says their lines
/// are. Rendered once per pane; the returned signal is what the overlay slot
/// draws.
pub(crate) fn watch(store: AppStore, pane: PaneId) -> impl Fn() -> Vec<Marker> + Copy + 'static {
    let lines: Signal<Vec<MarginLine>> = Signal::new(Vec::new());
    let right: Signal<f32> = Signal::new(0.0);
    let held: Signal<Option<StoreId>> = Signal::new(None);
    let editor = crate::editor::editor(pane);
    editor.on_margin_layout(move |reported: &[MarginLine]| {
        let key = crate::presence::text::MARGIN_KEY;
        if reported.iter().any(|line| line.key == key) || PRESENCE.with(|p| !p.borrow()[pane.index()].is_empty()) {
            crate::presence::text::redraw_later(store, pane);
        }
        let reported: Vec<MarginLine> = reported.iter().filter(|line| line.key != key).cloned().collect();
        if untracked(|| lines.get()) != reported {
            lines.set(reported);
        }
        right.set(crate::editor::editor(pane).margin_text_right().unwrap_or(0.0));
    });
    let _ = rinch::Effect::new(move || {
        let (store_id, list) = anchored(store, pane);
        if untracked(|| held.get()) != store_id {
            held.set(store_id);
        }
        let changed = GIVEN.with(|given| {
            let mut given = given.borrow_mut();
            let slot = &mut given[pane.index()];
            if *slot == list {
                return false;
            }
            *slot = list.clone();
            true
        });
        if changed {
            if list.is_empty() {
                lines.set(Vec::new());
            }
            untracked(|| give(pane));
        }
    });
    move || markers(store, held.get(), &lines.get(), right.get())
}

/// Pane `pane`'s editor's anchors: its discussions' and the carets of
/// whoever else is in its note.
fn give(pane: PaneId) {
    let mut anchors: Vec<MarginAnchor> = GIVEN.with(|given| given.borrow()[pane.index()].iter().map(|(id, sticky)| MarginAnchor::sticky(id.to_string(), sticky.clone())).collect());
    PRESENCE.with(|p| anchors.extend(p.borrow()[pane.index()].iter().map(|sticky| MarginAnchor::sticky(crate::presence::text::MARGIN_KEY, sticky.clone()))));
    crate::editor::editor(pane).set_margin_anchors(anchors);
}

/// The carets of others in pane `pane`'s note changed.
pub(crate) fn set_presence_anchors(pane: PaneId, stickies: Vec<Vec<u8>>) {
    let changed = PRESENCE.with(|p| {
        let slot = &mut p.borrow_mut()[pane.index()];
        if *slot == stickies {
            return false;
        }
        *slot = stickies;
        true
    });
    if changed {
        give(pane);
    }
}

/// One marker per line the reported anchors are on, top to bottom.
fn markers(store: AppStore, store_id: Option<StoreId>, lines: &[MarginLine], right: f32) -> Vec<Marker> {
    let Some(store_id) = store_id else { return Vec::new() };
    store.read_version.get();
    let mut out: Vec<Marker> = Vec::new();
    for line in lines {
        let Ok(id) = NodeId::parse(&line.key) else { continue };
        let Some(node) = store.get_node_signal(store_id, id).map(|sig| sig.get()) else { continue };
        let count = NodeDoc::load(&node.content).map(|doc| Discussion::of_node(&doc).read().live_count()).unwrap_or(0);
        let unread = super::unread_of(store_id, id, &node) > 0;
        let title = super::row_of(store_id, id, Some(&node), 0.0).topic;
        match out.iter_mut().find(|m| (m.top - line.top).abs() < 0.5) {
            Some(marker) => {
                marker.ids.push(id);
                marker.count += count;
                if unread && marker.unread.is_none() {
                    marker.unread = Some(id);
                }
                marker.title = format!("{} discussions", marker.ids.len());
            }
            None => out.push(Marker { top: line.top, height: line.height, store_id, ids: vec![id], count, unread: unread.then_some(id), title, style: String::new(), label: String::new(), class: "", opens: id }),
        }
    }
    out.sort_by(|a, b| a.top.total_cmp(&b.top));
    for marker in &mut out {
        const SIZE: f32 = 18.0;
        marker.opens = marker.unread.unwrap_or(marker.ids[0]);
        marker.class = if marker.unread.is_some() { "pimble-margin-marker pimble-margin-marker--unread" } else { "pimble-margin-marker" };
        marker.label = if marker.count > 0 { marker.count.to_string() } else { String::new() };
        marker.style = format!("left: {:.1}px; top: {:.1}px;", right + 6.0, marker.top + (marker.height - SIZE).max(0.0) / 2.0);
    }
    out
}
