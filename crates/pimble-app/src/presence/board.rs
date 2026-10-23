//! Pointers and selections on boards (docs/PRESENCE_CONTRACT.md section 5).
//!
//! Saying where this window's panes are: a board pane's frame carries its
//! pointer (none once it leaves the surface), the ids it has selected and
//! what it shows of the board, all in board units, so they mean the same at
//! every zoom and window size. `boards::presence_mine` reads them; a pointer
//! move, a pan or zoom and every change of the selection ask for a frame
//! (`presence::moved`, at most one per 50 ms).
//!
//! Showing where everyone else is: each board pane places the pointers and
//! selections of the sessions on its board with its own camera
//! (`board_to_screen`, divided by the pixel scale, as the discussion pins
//! are), again whenever its camera or the board changes and whenever a frame
//! arrives or lapses. A pointer is an arrow in the person's color with their
//! name; a selection is an outline in that color beside one's own.

use pimble_board::{Point, Rect, View};
use pimble_core::{NodeId, StoreId};
use pimble_rpc::{BoardPoint, BoardPresence, BoardRect};

use crate::panes::PaneId;
use crate::state::AppStore;

/// The most selected ids a frame carries: a select-all of a big board would
/// otherwise make every pointer frame kilobytes long, twenty times a second.
pub(crate) const MAX_SELECTION: usize = 50;

/// Someone else's pointer as a board pane draws it (logical pixels over the
/// surface).
#[derive(Debug, Clone, PartialEq)]
pub struct PointerView {
    pub session: String,
    /// Its place and color.
    pub style: String,
    /// The name tag's color.
    pub tag_style: String,
    pub name: String,
    /// Name and device, for the hover.
    pub title: String,
}

/// Someone else's selected element as a board pane outlines it.
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineView {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub color: String,
}

/// What a board pane draws of everyone else.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Others {
    pub pointers: Vec<PointerView>,
    pub outlines: Vec<OutlineView>,
}

/// Where this pane is on its board, as a frame says it: the pointer at
/// physical pixel `pointer` of the surface when it is over it, the
/// selection, what `view` shows of a `size` surface, and the picture's board
/// id when it is markup in a note's text.
pub(crate) fn whereabouts(view: &View, size: (u32, u32), pointer: Option<(f32, f32)>, selection: &[String], markup: Option<&str>) -> BoardPresence {
    let viewport = view.board_rect(size.0, size.1);
    BoardPresence {
        pointer: pointer.map(|(px, py)| {
            let p = view.to_board(px, py);
            BoardPoint { x: p.x, y: p.y }
        }),
        selection: selection.iter().take(MAX_SELECTION).cloned().collect(),
        viewport: BoardRect { x: viewport.x, y: viewport.y, w: viewport.w, h: viewport.h },
        markup: markup.map(str::to_string),
    }
}

/// Where board point `p` is over a `size` surface (physical pixels) that
/// `view` shows, in logical pixels (`scale` physical to one): `None` off
/// the surface.
pub(crate) fn on_screen(view: &View, size: (u32, u32), scale: f32, p: BoardPoint) -> Option<(f32, f32)> {
    let (x, y) = view.to_pixels(Point::new(p.x, p.y));
    let inside = x >= 0.0 && y >= 0.0 && x <= size.0 as f32 && y <= size.1 as f32;
    inside.then(|| (x / scale, y / scale))
}

/// A board rectangle in logical pixels over the surface.
pub(crate) fn rect_on_screen(view: &View, scale: f32, r: &Rect) -> (f32, f32, f32, f32) {
    let (x0, y0) = view.to_pixels(Point::new(r.x, r.y));
    let (x1, y1) = view.to_pixels(Point::new(r.right(), r.bottom()));
    (x0 / scale, y0 / scale, (x1 - x0) / scale, (y1 - y0) / scale)
}

/// The sessions on board `(store_id, node_id)` (`markup`: the picture in
/// its text being marked up, or the node's own board): each session's id,
/// where it is, who it is, and its hover.
pub(crate) fn sessions_on(store: AppStore, store_id: StoreId, node_id: NodeId, markup: Option<&str>) -> Vec<(String, BoardPresence, super::Identity, String)> {
    let sessions = rinch::prelude::untracked(|| store.presence.with(|all| all.get(&(store_id, node_id)).cloned().unwrap_or_default()));
    let me = crate::boards::current_author().map(|a| a.id);
    sessions
        .into_iter()
        .filter_map(|p| {
            let board = p.board.clone().filter(|b| b.markup.as_deref() == markup)?;
            let who = super::identity(store, store_id, node_id, &p, me.as_deref());
            let title = if p.device.is_empty() { who.name.clone() } else { format!("{}, on {}", who.name, p.device) };
            Some((p.session, board, who, title))
        })
        .collect()
}

/// Frames arrived or went: every board pane draws everyone again.
pub(crate) fn frames_changed(store: AppStore) {
    for pane in PaneId::ALL {
        crate::boards::draw_others(store, pane);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pointer said from one camera is drawn at the same board point by
    /// another: board units are what travel, whatever each pane's zoom, pan
    /// and pixel scale.
    #[test]
    fn a_pointer_lands_on_the_same_board_point_at_another_camera() {
        // Theirs: zoom 2, the board's (100, 50) at the top left.
        let theirs = View::new(100.0, 50.0, 2.0);
        let said = whereabouts(&theirs, (800, 600), Some((200.0, 100.0)), &["e1".to_string()], None);
        let pointer = said.pointer.unwrap();
        assert_eq!((pointer.x, pointer.y), (200.0, 100.0));
        assert_eq!(said.viewport, BoardRect { x: 100.0, y: 50.0, w: 400.0, h: 300.0 });
        assert_eq!(said.selection, vec!["e1".to_string()]);

        // Mine: zoom 0.5 from the origin, on a screen of pixel scale 2.
        let mine = View::new(0.0, 0.0, 0.5);
        assert_eq!(on_screen(&mine, (1000, 1000), 2.0, pointer), Some((50.0, 25.0)));
        // Panned so the point is left of the surface: not drawn.
        let panned = View::new(300.0, 0.0, 0.5);
        assert_eq!(on_screen(&panned, (1000, 1000), 2.0, pointer), None);
        // An element's bounds follow the same mapping.
        assert_eq!(rect_on_screen(&mine, 2.0, &Rect::new(200.0, 100.0, 40.0, 20.0)), (50.0, 25.0, 10.0, 5.0));
    }

    #[test]
    fn a_pointer_off_the_surface_says_none_and_a_big_selection_is_cut() {
        let view = View::new(0.0, 0.0, 1.0);
        let all: Vec<String> = (0..500).map(|i| format!("e{i}")).collect();
        let said = whereabouts(&view, (10, 10), None, &all, Some("b1"));
        assert_eq!(said.pointer, None);
        assert_eq!(said.markup.as_deref(), Some("b1"));
        assert_eq!(said.selection.len(), MAX_SELECTION);
        assert_eq!(said.selection[0], "e0");
    }
}
