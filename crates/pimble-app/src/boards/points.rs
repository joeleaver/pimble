//! The Pen and Direct select (docs/DRAWING_CONTRACT.md section 2, "Tools"):
//! drawing a path anchor by anchor, and editing the anchors and handles of a
//! path or a live shape.
//!
//! - **The Pen** keeps the path being drawn in the session ([`PenSession`]):
//!   a click places a corner, a press-drag a smooth anchor whose handles
//!   follow the drag (Alt moves the outgoing one alone), a click on the
//!   first anchor closes the path, Enter or Escape finishes an open one. The
//!   element is written once the path has two anchors and rewritten as it
//!   grows (about every 80 ms while a handle is dragged), so others watch it
//!   drawn; the whole path is one undo step (the session's held clock,
//!   `undo_options`).
//! - **Direct select** shows one element's points ([`PointEdit`]): anchors
//!   are pressed, shift-added and marqueed; anchors and handles drag (Alt
//!   breaks a handle's link); a double-click on a segment adds an anchor and
//!   on an anchor switches corner and smooth; Delete removes the selected
//!   anchors. Everything is worked out in the element's own space (the
//!   pointer through the inverse of its world transform). A live shape
//!   becomes a path in the same edit that first changes one of its points
//!   (`BoardTxn::convert_to_path`), and a turned or scaled path keeps the
//!   rest of itself in place as its extent changes (`pimble_board::repivot`).
//!
//! What both draw over the board (outlines, anchors, handles, the Pen's
//! segment to the pointer) is the view's [`EditOverlay`].
//!
//! The pure parts, tested here: the Pen's path ([`PenDraft`]) and what a
//! Direct select press takes ([`pick`]).

use std::sync::atomic::Ordering;

use pimble_board::path as bpath;
use pimble_board::tiny_skia::Transform as Affine;
use pimble_board::{Anchor, AnchorMode, AnchorRef, EditOverlay, HandleRef, PathGeom, SegmentHit};

use super::*;

/// How near (logical pixels) a press must be to an anchor or handle tip.
const POINT_REACH: f32 = 6.0;
/// A drag shorter than this (logical pixels) gives an anchor no handles.
const PEN_SLOP: f32 = 3.0;
/// The marks' color over the board.
const OVERLAY_COLOR: &str = "#228be6";

// ── The Pen's path (pure) ───────────────────────────────────────────────────

/// What a Pen press is placing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Placing {
    /// A new anchor, at the end.
    Anchor,
    /// The press was on the first anchor: releasing closes the path.
    Close,
    /// The press was on the last anchor: a drag gives it a new outgoing
    /// handle alone (a cusp).
    Last,
}

/// What a Pen release did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PenStep {
    Placed,
    Closed,
}

/// The Pen's path being drawn, in board units.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct PenDraft {
    pub anchors: Vec<Anchor>,
    pub closed: bool,
    /// The press under way.
    pub placing: Option<Placing>,
}

fn offset(a: Point, b: Point) -> Point {
    Point::new(b.x - a.x, b.y - a.y)
}

fn dist(a: Point, b: Point) -> f32 {
    (a.x - b.x).hypot(a.y - b.y)
}

fn is_zero(p: Point) -> bool {
    p.x.abs() < 1e-6 && p.y.abs() < 1e-6
}

impl PenDraft {
    /// A press at `p`; `reach` is how near (board units) counts as on an
    /// anchor.
    pub fn press(&mut self, p: Point, reach: f32) {
        let n = self.anchors.len();
        self.placing = Some(if n >= 2 && dist(self.anchors[0].p, p) <= reach {
            Placing::Close
        } else if n >= 1 && dist(self.anchors[n - 1].p, p) <= reach {
            Placing::Last
        } else {
            self.anchors.push(Anchor::corner(p));
            Placing::Anchor
        });
    }

    /// The press dragged to `to`: the handles follow. A drag within `slop`
    /// gives none; `break_link` (Alt) moves the dragged side alone.
    pub fn drag(&mut self, to: Point, slop: f32, break_link: bool) {
        let Some(placing) = self.placing else { return };
        let i = if placing == Placing::Close { 0 } else { self.anchors.len().saturating_sub(1) };
        let Some(a) = self.anchors.get_mut(i) else { return };
        let d = offset(a.p, to);
        let far = d.x.hypot(d.y) > slop;
        let back = Point::new(-d.x, -d.y);
        match placing {
            Placing::Anchor => {
                if !far {
                    a.h_in = Point::default();
                    a.h_out = Point::default();
                    a.mode = AnchorMode::Corner;
                } else if break_link {
                    a.h_out = d;
                    a.mode = AnchorMode::Corner;
                } else {
                    a.h_out = d;
                    a.h_in = back;
                    a.mode = AnchorMode::Symmetric;
                }
            }
            Placing::Last => {
                a.h_out = if far { d } else { Point::default() };
                a.mode = AnchorMode::Corner;
            }
            Placing::Close => {
                if !far {
                    return;
                }
                if break_link {
                    a.h_in = back;
                    a.mode = AnchorMode::Corner;
                } else {
                    a.h_out = d;
                    a.h_in = back;
                    a.mode = AnchorMode::Symmetric;
                }
            }
        }
    }

    /// The press ended.
    pub fn release(&mut self) -> Option<PenStep> {
        match self.placing.take()? {
            Placing::Close => {
                self.closed = true;
                Some(PenStep::Closed)
            }
            _ => Some(PenStep::Placed),
        }
    }

    /// Take back the press under way (a cancelled pointer): a new anchor
    /// goes.
    pub fn cancel(&mut self) {
        if self.placing.take() == Some(Placing::Anchor) {
            self.anchors.pop();
        }
    }

    /// The path, once it has two anchors.
    pub fn geom(&self) -> Option<PathGeom> {
        (self.anchors.len() >= 2).then(|| PathGeom::from_anchors(self.anchors.clone(), self.closed))
    }
}

// ── Direct select's press (pure) ────────────────────────────────────────────

/// What a Direct select press at a point of a path takes, nearest first in
/// this order: a handle tip of a selected anchor, an anchor, a segment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Pick {
    Handle(HandleRef),
    Anchor(AnchorRef),
    Segment(SegmentHit),
    Nothing,
}

pub(super) fn pick(path: &PathGeom, selected: &[AnchorRef], p: Point, tolerance: f32) -> Pick {
    if let Some(h) = path.hit_handle(p, tolerance, Some(selected)) {
        return Pick::Handle(h);
    }
    if let Some(a) = path.hit_anchor(p, tolerance) {
        return Pick::Anchor(a);
    }
    if let Some(s) = path.hit_segment(p, tolerance) {
        return Pick::Segment(s);
    }
    Pick::Nothing
}

/// The two anchors a segment joins.
pub(super) fn segment_ends(path: &PathGeom, subpath: usize, segment: usize) -> Vec<AnchorRef> {
    let n = path.subpaths.get(subpath).map(|s| s.anchors.len()).unwrap_or(0);
    if n == 0 {
        return Vec::new();
    }
    vec![AnchorRef::new(subpath, segment % n), AnchorRef::new(subpath, (segment + 1) % n)]
}

/// The anchor selection after a press on `at`: with shift it is added, or
/// taken out when it was in (answers false then: nothing to drag); without,
/// it stays when `at` was already in it (so several drag together), else it
/// is `at` alone.
pub(super) fn press_anchor(selected: &[AnchorRef], at: AnchorRef, shift: bool) -> (Vec<AnchorRef>, bool) {
    let mut out = selected.to_vec();
    match (shift, out.iter().position(|a| *a == at)) {
        (true, Some(i)) => {
            out.remove(i);
            (out, false)
        }
        (true, None) => {
            out.push(at);
            (out, true)
        }
        (false, Some(_)) => (out, true),
        (false, None) => (vec![at], true),
    }
}

/// `p` through affine `w`.
fn apply(w: &Affine, p: Point) -> Point {
    Point::new(w.sx * p.x + w.kx * p.y + w.tx, w.ky * p.x + w.sy * p.y + w.ty)
}

/// How much `w` scales lengths, on average.
fn scale_of_affine(w: &Affine) -> f32 {
    (w.sx * w.sy - w.kx * w.ky).abs().sqrt().max(1e-6)
}

// ── State in the session ────────────────────────────────────────────────────

/// The Pen's path being drawn in a pane.
pub(super) struct PenSession {
    pub draft: PenDraft,
    /// Its element, once it has two anchors.
    pub id: Option<String>,
    pub layer: String,
    pub style: Style,
    pub written_at: f64,
}

/// The element Direct select shows the points of, and the selected anchors.
pub(super) struct PointEdit {
    pub id: String,
    pub anchors: Vec<AnchorRef>,
    /// The path as a drag has it now (drawn over the board ahead of the
    /// writes, which come about every 80 ms).
    pub working: Option<PathGeom>,
}

/// What a Direct select drag holds.
pub(super) enum Grab {
    Anchors(Vec<AnchorRef>),
    Handle(HandleRef),
}

/// A Direct select drag: the element as it was when pressed.
pub(super) struct Drag {
    id: String,
    base: PathGeom,
    /// Still a live shape: the first write turns it into a path.
    live: bool,
    transform: Transform,
    pivot: Rect,
    world: Affine,
    inv: Affine,
    grab: Grab,
    /// Where it was pressed, element space.
    start: Point,
    written_at: f64,
    moved: bool,
    /// The anchor pressed, when it was one of several selected: a click
    /// (no drag) on it selects it alone.
    clicked: Option<AnchorRef>,
}

/// An element Direct select can edit, read for an edit.
struct Target {
    path: PathGeom,
    live: bool,
    transform: Transform,
    pivot: Rect,
    world: Affine,
    inv: Affine,
}

/// Element `id`'s points: a path's own, or those a live shape (rect,
/// ellipse, diamond, polygon, star, a line without heads, a pen stroke)
/// would have as a path.
fn target_of(s: &Session, id: &str) -> Option<Target> {
    let e = s.snapshot().get(id)?;
    let (path, live) = match (&e.raw.kind, &e.raw.geom) {
        (Kind::Path, Geom::Path(p)) => (p.clone(), false),
        _ => (bpath::to_path(&e.raw.kind, &e.raw.geom, &e.raw.style, &e.raw.points)?, true),
    };
    let inv = e.world.invert()?;
    Some(Target { path, live, transform: e.raw.transform, pivot: e.pivot_bounds, world: e.world, inv })
}

/// How near counts as on a point, board units.
fn reach(s: &Session, scale: f32) -> f32 {
    POINT_REACH * scale / s.view.zoom().max(1e-3)
}

/// Hold pane `pane`'s undo clock (`Some(true)`), or let it go: what is
/// written while it is held is one undo step.
fn hold_clock(pane: PaneId, hold: bool) {
    with_session(pane, |s| s.clock.store(if hold { now_ms().max(1.0) as u64 } else { 0 }, Ordering::Relaxed));
}

// ── The Pen ─────────────────────────────────────────────────────────────────

/// A Pen press at physical pixel (`px`, `py`).
pub(super) fn pen_press(store: AppStore, pane: PaneId, px: f32, py: f32) {
    if !writable_or_say(store, pane) {
        return;
    }
    let scale = scale_of(pane);
    let drawing = with_session(pane, |s| s.pen.is_some()).unwrap_or(false);
    if !drawing {
        let Some(layer) = tools::target_layer(store, pane) else { return };
        let style = tools::new_style(store, &Kind::Path);
        new_undo_step(pane);
        hold_clock(pane, true);
        with_session(pane, |s| {
            s.selected.clear();
            s.points = None;
            s.pen = Some(PenSession { draft: PenDraft::default(), id: None, layer, style, written_at: 0.0 });
        });
    }
    with_session(pane, |s| {
        let p = s.view.screen_to_board(px, py);
        let reach = reach(s, scale);
        if let Some(pen) = s.pen.as_mut() {
            pen.draft.press(p, reach);
        }
        s.gesture = Gesture::PenPlace;
    });
    write_pen(store, pane);
    after_change(store, pane);
}

/// The Pen's press dragged to physical pixel (`px`, `py`).
fn pen_drag(store: AppStore, pane: PaneId, px: f32, py: f32) {
    let scale = scale_of(pane);
    let due = with_session(pane, |s| {
        let p = s.view.screen_to_board(px, py);
        let slop = PEN_SLOP * scale / s.view.zoom().max(1e-3);
        let alt = s.alt;
        let pen = s.pen.as_mut()?;
        pen.draft.drag(p, slop, alt);
        let due = now_ms() - pen.written_at >= DRAG_WRITE_EVERY_MS;
        Some(due)
    })
    .flatten();
    match due {
        Some(true) => write_pen(store, pane),
        Some(false) => {
            refresh_overlay(store, pane);
            present(pane);
        }
        None => {}
    }
}

/// The Pen's press ended: a click on the first anchor closed the path,
/// which finishes it.
fn pen_release(store: AppStore, pane: PaneId) {
    let step = with_session(pane, |s| s.pen.as_mut().and_then(|pen| pen.draft.release())).flatten();
    write_pen(store, pane);
    if step == Some(PenStep::Closed) {
        finish_pen(store, pane);
    } else {
        after_change(store, pane);
    }
}

/// Write the Pen's path: its element made once it has two anchors, its
/// geometry after. A path whose element went (someone deleted it, an undo)
/// is let go.
fn write_pen(store: AppStore, pane: PaneId) {
    let Some((geom, id, layer, style)) = with_session(pane, |s| {
        let pen = s.pen.as_mut()?;
        let geom = pen.draft.geom()?;
        pen.written_at = now_ms();
        Some((geom, pen.id.clone(), pen.layer.clone(), pen.style.clone()))
    })
    .flatten() else {
        return;
    };
    match id {
        None => {
            let element = NewElement::path(geom).with_style(style).in_layer(layer);
            let made = local_edit(store, pane, |t| t.add_element(element));
            with_session(pane, |s| match (made, s.pen.as_mut()) {
                (Some(id), Some(pen)) => pen.id = Some(id),
                _ => s.pen = None,
            });
        }
        Some(id) => {
            if local_edit(store, pane, |t| t.set_geom(&id, &Geom::Path(geom))).is_none() {
                with_session(pane, |s| s.pen = None);
                hold_clock(pane, false);
            }
        }
    }
}

/// Finish the Pen's path (Enter, Escape, closing it, another tool): it stays
/// as written and is selected; a path of one anchor was never written and
/// goes.
pub(super) fn finish_pen(store: AppStore, pane: PaneId) {
    let Some(pen) = with_session(pane, |s| {
        if matches!(s.gesture, Gesture::PenPlace) {
            s.gesture = Gesture::Idle;
        }
        s.pen.take()
    })
    .flatten() else {
        return;
    };
    hold_clock(pane, false);
    new_undo_step(pane);
    match pen.id {
        Some(id) if with_session(pane, |s| s.snapshot().get(&id).is_some()) == Some(true) => tools::select(store, pane, vec![id]),
        _ => after_change(store, pane),
    }
}

/// Whether pane `pane` has a Pen path under way.
pub(super) fn drawing(pane: PaneId) -> bool {
    with_session(pane, |s| s.pen.is_some()).unwrap_or(false)
}

// ── Direct select ───────────────────────────────────────────────────────────

/// Whether pane `pane` shows points with anchors selected.
pub(super) fn anchors_selected(pane: PaneId) -> bool {
    with_session(pane, |s| s.points.as_ref().is_some_and(|p| !p.anchors.is_empty())).unwrap_or(false)
}

/// Whether pane `pane` shows an element's points.
pub(super) fn editing(pane: PaneId) -> bool {
    with_session(pane, |s| s.points.is_some()).unwrap_or(false)
}

/// Stop showing points (Escape, another tool).
pub(super) fn end_points(store: AppStore, pane: PaneId) {
    if with_session(pane, |s| s.points.take().is_some()) == Some(true) {
        after_change(store, pane);
    }
}

/// A Direct select press at physical pixel (`px`, `py`).
pub(super) fn press(store: AppStore, pane: PaneId, px: f32, py: f32) {
    let scale = scale_of(pane);
    let writable = with_session(pane, |s| store.node_access(s.store_id, s.node_id).allows_write()).unwrap_or(false);
    let mut locked = false;
    with_session(pane, |s| {
        let p = s.view.screen_to_board(px, py);
        let tol = reach(s, scale);
        let shift = s.shift;
        // On the element shown: its handles, anchors and segments.
        let shown = s.points.as_ref().map(|pe| pe.id.clone()).filter(|id| s.editable(id));
        if let Some(id) = shown {
            if let Some(t) = target_of(s, &id) {
                let q = apply(&t.inv, p);
                let tol_e = tol / scale_of_affine(&t.world);
                let selected = s.points.as_ref().map(|pe| pe.anchors.clone()).unwrap_or_default();
                let picked = pick(&t.path, &selected, q, tol_e);
                let grab = match picked {
                    Pick::Handle(h) => Some((Grab::Handle(h), None)),
                    Pick::Anchor(a) => {
                        let was_in = selected.contains(&a);
                        let (now, drag) = press_anchor(&selected, a, shift);
                        if let Some(pe) = s.points.as_mut() {
                            pe.anchors = now.clone();
                        }
                        drag.then(|| (Grab::Anchors(now), (was_in && !shift).then_some(a)))
                    }
                    Pick::Segment(hit) => {
                        let ends = segment_ends(&t.path, hit.subpath, hit.segment);
                        let now = if shift {
                            let mut all = selected.clone();
                            all.extend(ends.iter().filter(|a| !selected.contains(a)));
                            all
                        } else {
                            ends
                        };
                        if let Some(pe) = s.points.as_mut() {
                            pe.anchors = now.clone();
                        }
                        Some((Grab::Anchors(now), None))
                    }
                    Pick::Nothing => None,
                };
                // Off the element, another may be picked below.
                if picked != Pick::Nothing {
                    if let (Some((grab, clicked)), true) = (grab, writable) {
                        s.gesture = Gesture::Points(Drag {
                            id,
                            base: t.path,
                            live: t.live,
                            transform: t.transform,
                            pivot: t.pivot,
                            world: t.world,
                            inv: t.inv,
                            grab,
                            start: q,
                            written_at: 0.0,
                            moved: false,
                            clicked,
                        });
                    }
                    return;
                }
            }
        }
        // Another element: show its points (and take the anchor pressed).
        let hit = super::tools::hit_at(s, p).map(|e| e.raw.id.clone());
        if let Some(id) = hit {
            if let Some(t) = target_of(s, &id) {
                if !s.editable(&id) {
                    locked = true;
                    return;
                }
                let q = apply(&t.inv, p);
                let tol_e = tol / scale_of_affine(&t.world);
                let anchor = t.path.hit_anchor(q, tol_e);
                s.selected = vec![id.clone()];
                s.points = Some(PointEdit { id: id.clone(), anchors: anchor.into_iter().collect(), working: None });
                if let (Some(a), true) = (anchor, writable) {
                    s.gesture = Gesture::Points(Drag {
                        id,
                        base: t.path,
                        live: t.live,
                        transform: t.transform,
                        pivot: t.pivot,
                        world: t.world,
                        inv: t.inv,
                        grab: Grab::Anchors(vec![a]),
                        start: q,
                        written_at: 0.0,
                        moved: false,
                        clicked: None,
                    });
                }
                return;
            }
        }
        // Empty board: a marquee over anchors.
        let base = if shift { s.points.as_ref().map(|pe| pe.anchors.clone()).unwrap_or_default() } else { Vec::new() };
        if let Some(pe) = s.points.as_mut() {
            pe.anchors = base.clone();
        }
        s.gesture = Gesture::PointMarquee { start: (px, py), now: (px, py), base };
    });
    if locked {
        crate::events::show_notice(store, LOCKED.to_string());
    }
    if with_session(pane, |s| matches!(s.gesture, Gesture::Points(_))) == Some(true) {
        new_undo_step(pane);
        hold_clock(pane, true);
    }
    tools::load_selection_style(store, pane);
    after_change(store, pane);
}

/// Anchors of path `path` (element space, drawn through `world`) inside
/// board rectangle `rect`.
fn anchors_in(path: &PathGeom, world: &Affine, rect: &Rect) -> Vec<AnchorRef> {
    path.anchor_refs()
        .into_iter()
        .filter(|r| path.anchor(*r).is_some_and(|a| rect.contains(apply(world, a.p))))
        .collect()
}

/// A Direct select gesture moved to physical pixel (`px`, `py`), or the
/// Pen's press dragged; answers whether it was one of theirs.
pub(super) fn moved(store: AppStore, pane: PaneId, px: f32, py: f32) -> bool {
    let kind = with_session(pane, |s| match s.gesture {
        Gesture::PenPlace => 1,
        Gesture::Points(_) => 2,
        Gesture::PointMarquee { .. } => 3,
        _ => 0,
    })
    .unwrap_or(0);
    match kind {
        1 => pen_drag(store, pane, px, py),
        2 => drag_to(store, pane, px, py, false),
        3 => {
            with_session(pane, |s| {
                let Gesture::PointMarquee { start, now, base } = &mut s.gesture else { return };
                *now = (px, py);
                let a = s.view.screen_to_board(start.0.min(px), start.1.min(py));
                let b = s.view.screen_to_board(start.0.max(px), start.1.max(py));
                let rect = Rect::new(a.x, a.y, b.x - a.x, b.y - a.y);
                let base = base.clone();
                let Some(id) = s.points.as_ref().map(|pe| pe.id.clone()) else { return };
                let Some(t) = target_of(s, &id) else { return };
                let mut chosen = base;
                for r in anchors_in(&t.path, &t.world, &rect) {
                    if !chosen.contains(&r) {
                        chosen.push(r);
                    }
                }
                if let Some(pe) = s.points.as_mut() {
                    pe.anchors = chosen;
                }
            });
            after_change(store, pane);
        }
        _ => return false,
    }
    true
}

/// The path a drag makes of its base with the pointer at board point `p`.
fn dragged(d: &Drag, p: Point, alt: bool) -> PathGeom {
    let q = apply(&d.inv, p);
    let mut geom = d.base.clone();
    match &d.grab {
        Grab::Anchors(refs) => geom.move_anchors(refs, q.x - d.start.x, q.y - d.start.y),
        Grab::Handle(h) => geom.move_handle(*h, q, alt),
    }
    geom
}

/// What writing `geom` over a drag's element takes.
struct PointWrite {
    id: String,
    live: bool,
    transform: Transform,
    pivot: Rect,
    geom: PathGeom,
}

/// Write a path's new geometry: a live shape turned into a path first, in
/// the same edit; its transform kept so the rest of it stays in place.
/// Answers whether it was written.
fn write_points(store: AppStore, pane: PaneId, w: &PointWrite) -> bool {
    let to = w.geom.bounds().unwrap_or(w.pivot);
    let transform = pimble_board::repivot(&w.transform, w.pivot, to);
    local_edit(store, pane, |t| {
        if w.live {
            t.convert_to_path(&w.id)?;
        }
        t.set_geom(&w.id, &Geom::Path(w.geom.clone()))?;
        if !w.transform.is_identity() || !transform.is_identity() {
            t.set_transform(&w.id, &transform)?;
        }
        Ok(())
    })
    .is_some()
}

/// The drag at physical pixel (`px`, `py`): the working path follows at
/// once, the document about every 80 ms (and always when `last`).
fn drag_to(store: AppStore, pane: PaneId, px: f32, py: f32, last: bool) {
    let scale = scale_of(pane);
    let write = with_session(pane, |s| {
        let p = s.view.screen_to_board(px, py);
        let alt = s.alt;
        let slop = CLICK_SLOP_PX * scale / s.view.zoom().max(1e-3);
        let Gesture::Points(d) = &mut s.gesture else { return None };
        let q = apply(&d.inv, p);
        if !d.moved && dist(apply(&d.world, q), apply(&d.world, d.start)) <= slop {
            return None;
        }
        d.moved = true;
        let geom = dragged(d, p, alt);
        let due = last || now_ms() - d.written_at >= DRAG_WRITE_EVERY_MS;
        if due {
            d.written_at = now_ms();
        }
        let w = due.then(|| PointWrite { id: d.id.clone(), live: d.live, transform: d.transform, pivot: d.pivot, geom: geom.clone() });
        if let Some(pe) = s.points.as_mut() {
            pe.working = Some(geom);
        }
        Some(w)
    })
    .flatten();
    match write {
        Some(Some(w)) => {
            if write_points(store, pane, &w) {
                with_session(pane, |s| {
                    if let Gesture::Points(d) = &mut s.gesture {
                        d.live = false;
                    }
                });
            }
        }
        Some(None) => {
            refresh_overlay(store, pane);
            present(pane);
        }
        None => {}
    }
}

/// A click shorter than this (logical pixels) moves nothing.
const CLICK_SLOP_PX: f32 = 3.0;

/// A Direct select or Pen gesture ended (`gesture`, taken out of the
/// session) at physical pixel (`px`, `py`); answers whether it was one of
/// theirs.
pub(super) fn released(store: AppStore, pane: PaneId, gesture: Gesture, px: f32, py: f32) -> Result<(), Gesture> {
    match gesture {
        Gesture::PenPlace => pen_release(store, pane),
        Gesture::Points(d) => {
            let clicked = d.clicked;
            let moved = d.moved;
            with_session(pane, |s| s.gesture = Gesture::Points(d));
            if moved {
                drag_to(store, pane, px, py, true);
            }
            with_session(pane, |s| {
                s.gesture = Gesture::Idle;
                if let Some(pe) = s.points.as_mut() {
                    pe.working = None;
                    if let (false, Some(a)) = (moved, clicked) {
                        pe.anchors = vec![a];
                    }
                }
            });
            hold_clock(pane, false);
            new_undo_step(pane);
            after_change(store, pane);
        }
        Gesture::PointMarquee { start, now, .. } => {
            let scale = scale_of(pane);
            let click = (start.0 - now.0).abs() <= CLICK_SLOP_PX * scale && (start.1 - now.1).abs() <= CLICK_SLOP_PX * scale;
            with_session(pane, |s| {
                let shift = s.shift;
                if click {
                    // A click on empty board lets go of the points.
                    if !shift {
                        s.points = None;
                        s.selected.clear();
                    }
                    return;
                }
                if s.points.is_some() {
                    return;
                }
                // Nothing shown yet: the topmost element with anchors inside
                // the marquee shows them.
                let a = s.view.screen_to_board(start.0.min(now.0), start.1.min(now.1));
                let b = s.view.screen_to_board(start.0.max(now.0), start.1.max(now.1));
                let rect = Rect::new(a.x, a.y, b.x - a.x, b.y - a.y);
                let snapshot = s.snapshot();
                let ids: Vec<String> = snapshot
                    .query(&rect)
                    .into_iter()
                    .rev()
                    .map(|i| &snapshot.elements[i])
                    .filter(|e| e.visible && !snapshot.in_locked_layer(e) && snapshot.editable(&e.raw.id))
                    .map(|e| e.raw.id.clone())
                    .collect();
                for id in ids {
                    let Some(t) = target_of(s, &id) else { continue };
                    let inside = anchors_in(&t.path, &t.world, &rect);
                    if !inside.is_empty() {
                        s.selected = vec![id.clone()];
                        s.points = Some(PointEdit { id, anchors: inside, working: None });
                        break;
                    }
                }
            });
            tools::load_selection_style(store, pane);
            after_change(store, pane);
        }
        other => return Err(other),
    }
    Ok(())
}

/// The platform took the pointer, or Escape: a Pen press is taken back, a
/// drag's points go back where they were, a marquee leaves the anchors it
/// started from. Answers `Err` with a gesture that is not theirs.
pub(super) fn cancelled(store: AppStore, pane: PaneId, gesture: Gesture) -> Result<(), Gesture> {
    match gesture {
        Gesture::PenPlace => {
            let gone = with_session(pane, |s| {
                let pen = s.pen.as_mut()?;
                pen.draft.cancel();
                (pen.draft.anchors.len() < 2).then(|| pen.id.take()).flatten()
            })
            .flatten();
            match gone {
                Some(id) => {
                    local_edit(store, pane, |t| t.delete_element(&id));
                }
                None => write_pen(store, pane),
            }
            after_change(store, pane);
        }
        Gesture::Points(d) => {
            if d.moved && !d.live {
                let w = PointWrite { id: d.id.clone(), live: false, transform: d.transform, pivot: d.pivot, geom: d.base.clone() };
                write_points(store, pane, &w);
            }
            with_session(pane, |s| {
                if let Some(pe) = s.points.as_mut() {
                    pe.working = None;
                }
            });
            hold_clock(pane, false);
            after_change(store, pane);
        }
        Gesture::PointMarquee { base, .. } => {
            with_session(pane, |s| {
                if let Some(pe) = s.points.as_mut() {
                    pe.anchors = base;
                }
            });
            after_change(store, pane);
        }
        other => return Err(other),
    }
    Ok(())
}

/// One edit of the shown element's path, made by `f` on its points (the
/// selected anchors in hand); `f` answers false to write nothing. An
/// emptied path deletes the element.
fn edit_points(store: AppStore, pane: PaneId, f: impl FnOnce(&mut PathGeom, &mut Vec<AnchorRef>) -> bool) {
    let plan = with_session(pane, |s| {
        let pe = s.points.as_ref()?;
        if !s.editable(&pe.id) {
            return None;
        }
        let t = target_of(s, &pe.id)?;
        let mut geom = t.path.clone();
        let mut anchors = pe.anchors.clone();
        if !f(&mut geom, &mut anchors) {
            return None;
        }
        if let Some(pe) = s.points.as_mut() {
            pe.anchors = anchors;
        }
        Some(PointWrite { id: pe_id(s), live: t.live, transform: t.transform, pivot: t.pivot, geom })
    })
    .flatten();
    let Some(w) = plan else { return };
    new_undo_step(pane);
    if w.geom.is_empty() {
        local_edit(store, pane, |t| t.delete_element(&w.id));
        with_session(pane, |s| {
            s.points = None;
            s.selected.clear();
        });
        after_change(store, pane);
    } else {
        write_points(store, pane, &w);
    }
}

fn pe_id(s: &Session) -> String {
    s.points.as_ref().map(|p| p.id.clone()).unwrap_or_default()
}

/// Delete or Backspace: the selected anchors go (an emptied path deletes
/// its element).
pub(super) fn delete_anchors(store: AppStore, pane: PaneId) {
    edit_points(store, pane, |geom, anchors| {
        if anchors.is_empty() {
            return false;
        }
        geom.delete_anchors(anchors);
        anchors.clear();
        true
    });
}

/// A Direct select double-click at physical pixel (`px`, `py`): on an anchor
/// it switches corner and smooth, on a segment it adds an anchor there.
/// Answers whether it did something.
pub(super) fn double_click(store: AppStore, pane: PaneId, px: f32, py: f32) -> bool {
    let scale = scale_of(pane);
    let found = with_session(pane, |s| {
        let id = s.points.as_ref()?.id.clone();
        let t = target_of(s, &id)?;
        let p = s.view.screen_to_board(px, py);
        let q = apply(&t.inv, p);
        let tol = reach(s, scale) / scale_of_affine(&t.world);
        match pick(&t.path, &[], q, tol) {
            Pick::Anchor(a) => Some(Pick::Anchor(a)),
            Pick::Segment(hit) => Some(Pick::Segment(hit)),
            _ => None,
        }
    })
    .flatten();
    let Some(found) = found else { return false };
    if !writable_or_say(store, pane) {
        return true;
    }
    // The press before took the gesture (a drag that did not move).
    with_session(pane, |s| s.gesture = Gesture::Idle);
    hold_clock(pane, false);
    edit_points(store, pane, |geom, anchors| match found {
        Pick::Anchor(a) => {
            *anchors = vec![a];
            geom.toggle_smooth(a).is_some()
        }
        Pick::Segment(hit) => match geom.insert_anchor(hit.subpath, hit.segment, hit.t) {
            Some(a) => {
                // Anchors after it in its subpath move along by one.
                for r in anchors.iter_mut() {
                    if r.subpath == a.subpath && r.index >= a.index {
                        r.index += 1;
                    }
                }
                *anchors = vec![a];
                true
            }
            None => false,
        },
        _ => false,
    });
    true
}

// ── The overlay ─────────────────────────────────────────────────────────────

/// Draw what the Pen or Direct select shows over pane `pane`'s board (or
/// nothing), as things are now. The caller presents.
pub(super) fn refresh_overlay(store: AppStore, pane: PaneId) {
    let scale = scale_of(pane);
    let tool = untracked(|| store.board_tool.get());
    let color = Color::parse(OVERLAY_COLOR).unwrap_or(EditOverlay::default().color);
    with_session(pane, |s| {
        // Points of an element that went are no longer shown.
        if s.points.as_ref().is_some_and(|pe| s.snapshot().get(&pe.id).is_none()) {
            s.points = None;
        }
        let overlay = if let Some(pen) = s.pen.as_ref().filter(|_| tool == Tool::Pen) {
            pen_overlay(s, pen, scale)
        } else if let Some(pe) = s.points.as_ref().filter(|_| tool == Tool::DirectSelect) {
            points_overlay(s, pe, scale)
        } else {
            None
        };
        let overlay = overlay.map(|o| EditOverlay { color, scale, ..o });
        s.view.set_overlay(overlay);
    });
}

fn pen_overlay(s: &Session, pen: &PenSession, scale: f32) -> Option<EditOverlay> {
    let d = &pen.draft;
    let last = d.anchors.last()?;
    let mut o = EditOverlay::default();
    let geom = PathGeom::from_anchors(d.anchors.clone(), d.closed);
    o.curves = geom.segments().map(|(_, _, c)| c).collect();
    let n = d.anchors.len();
    o.anchors = d.anchors.iter().enumerate().map(|(i, a)| (a.p, i + 1 == n)).collect();
    let shown = match d.placing {
        Some(Placing::Close) => &d.anchors[0],
        _ => last,
    };
    for h in [shown.h_in, shown.h_out] {
        if !is_zero(h) {
            let tip = Point::new(shown.p.x + h.x, shown.p.y + h.y);
            o.arms.push((shown.p, tip));
            o.tips.push(tip);
        }
    }
    if d.placing.is_none() && s.over {
        let p = s.view.screen_to_board(s.pointer.0, s.pointer.1);
        o.pending = Some([last.p, last.out_point(), p, p]);
        if n >= 2 && dist(d.anchors[0].p, p) <= reach(s, scale) {
            o.hot = Some(d.anchors[0].p);
        }
    }
    Some(o)
}

fn points_overlay(s: &Session, pe: &PointEdit, _scale: f32) -> Option<EditOverlay> {
    let t = target_of(s, &pe.id)?;
    let path = pe.working.as_ref().unwrap_or(&t.path);
    let w = |p: Point| apply(&t.world, p);
    let mut o = EditOverlay::default();
    o.curves = path.segments().map(|(_, _, c)| [w(c[0]), w(c[1]), w(c[2]), w(c[3])]).collect();
    for r in path.anchor_refs() {
        let Some(a) = path.anchor(r) else { continue };
        let selected = pe.anchors.contains(&r);
        o.anchors.push((w(a.p), selected));
        if selected {
            for h in [a.h_in, a.h_out] {
                if !is_zero(h) {
                    let tip = w(Point::new(a.p.x + h.x, a.p.y + h.y));
                    o.arms.push((w(a.p), tip));
                    o.tips.push(tip);
                }
            }
        }
    }
    Some(o)
}

/// The tool changed: a Pen path under way is finished, Direct select's
/// points are let go of.
pub(super) fn tool_changed(store: AppStore, pane: PaneId) {
    let tool = untracked(|| store.board_tool.get());
    if tool != Tool::Pen && drawing(pane) {
        finish_pen(store, pane);
    }
    if tool != Tool::DirectSelect {
        end_points(store, pane);
    }
    if with_session(pane, |_| ()).is_some() {
        refresh_overlay(store, pane);
        present(pane);
    }
}

/// The pointer moved with nothing pressed: the Pen's segment to it follows.
pub(super) fn hovered(store: AppStore, pane: PaneId) {
    if drawing(pane) {
        refresh_overlay(store, pane);
        present(pane);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn the_pen_places_corners_with_clicks_and_curves_with_drags() {
        let mut d = PenDraft::default();
        d.press(p(0.0, 0.0), 5.0);
        assert_eq!(d.release(), Some(PenStep::Placed));
        assert!(d.geom().is_none(), "one anchor is no path yet");
        // A press-drag: symmetric handles following the drag.
        d.press(p(100.0, 0.0), 5.0);
        d.drag(p(130.0, 40.0), 3.0, false);
        let a = d.anchors[1];
        assert_eq!(a.h_out, p(30.0, 40.0));
        assert_eq!(a.h_in, p(-30.0, -40.0));
        assert_eq!(a.mode, AnchorMode::Symmetric);
        // Alt breaks it: the outgoing handle moves alone.
        d.drag(p(100.0, 50.0), 3.0, true);
        let a = d.anchors[1];
        assert_eq!(a.h_out, p(0.0, 50.0));
        assert_eq!(a.h_in, p(-30.0, -40.0));
        assert_eq!(a.mode, AnchorMode::Corner);
        d.release();
        // A drag that comes back within the slop is a click: a corner.
        d.press(p(100.0, 100.0), 5.0);
        d.drag(p(101.0, 101.0), 3.0, false);
        assert_eq!(d.anchors[2], Anchor::corner(p(100.0, 100.0)));
        d.release();
        let geom = d.geom().unwrap();
        assert_eq!(geom.anchor_count(), 3);
        assert!(!geom.subpaths[0].closed);
    }

    #[test]
    fn a_click_on_the_first_anchor_closes_the_path() {
        let mut d = PenDraft::default();
        d.press(p(0.0, 0.0), 5.0);
        d.release();
        // With one anchor, a click on it is a click on the last: no close.
        d.press(p(1.0, 1.0), 5.0);
        assert_eq!(d.placing, Some(Placing::Last));
        d.release();
        assert_eq!(d.anchors.len(), 1);
        d.press(p(100.0, 0.0), 5.0);
        d.release();
        d.press(p(50.0, 80.0), 5.0);
        d.release();
        d.press(p(2.0, -2.0), 5.0);
        assert_eq!(d.placing, Some(Placing::Close));
        // Dragging there shapes the first anchor's handles.
        d.drag(p(0.0, 20.0), 3.0, false);
        assert_eq!(d.anchors[0].h_out, p(0.0, 20.0));
        assert_eq!(d.anchors[0].h_in, p(0.0, -20.0));
        assert_eq!(d.release(), Some(PenStep::Closed));
        let geom = d.geom().unwrap();
        assert_eq!(geom.anchor_count(), 3, "closing adds no anchor");
        assert!(geom.subpaths[0].closed);
    }

    #[test]
    fn a_press_on_the_last_anchor_makes_a_cusp_and_a_cancel_takes_the_anchor_back() {
        let mut d = PenDraft::default();
        d.press(p(0.0, 0.0), 5.0);
        d.drag(p(0.0, 30.0), 3.0, false);
        d.release();
        d.press(p(0.0, 0.0), 5.0);
        d.drag(p(40.0, 0.0), 3.0, false);
        let a = d.anchors[0];
        assert_eq!(a.h_in, p(0.0, -30.0), "its incoming handle stays");
        assert_eq!(a.h_out, p(40.0, 0.0));
        assert_eq!(a.mode, AnchorMode::Corner);
        d.release();
        d.press(p(90.0, 0.0), 5.0);
        d.cancel();
        assert_eq!(d.anchors.len(), 1);
        assert_eq!(d.placing, None);
    }

    fn square() -> PathGeom {
        PathGeom::polyline(&[p(0.0, 0.0), p(100.0, 0.0), p(100.0, 100.0), p(0.0, 100.0)], true)
    }

    #[test]
    fn a_direct_select_press_takes_handles_then_anchors_then_segments() {
        let mut path = square();
        let a0 = AnchorRef::new(0, 0);
        path.set_mode(a0, AnchorMode::Smooth);
        let tip = path.anchor(a0).unwrap().out_point();
        // A handle shows (and is taken) only on a selected anchor.
        assert!(!matches!(pick(&path, &[], tip, 4.0), Pick::Handle(_)));
        assert!(matches!(pick(&path, &[a0], tip, 4.0), Pick::Handle(h) if h.anchor == a0));
        assert_eq!(pick(&path, &[], p(99.0, 1.0), 4.0), Pick::Anchor(AnchorRef::new(0, 1)));
        let Pick::Segment(hit) = pick(&path, &[], p(100.0, 50.0), 4.0) else { panic!("a segment") };
        assert_eq!((hit.subpath, hit.segment), (0, 1));
        assert_eq!(segment_ends(&path, 0, 3), vec![AnchorRef::new(0, 3), AnchorRef::new(0, 0)], "the closing segment");
        assert_eq!(pick(&path, &[], p(50.0, 50.0), 4.0), Pick::Nothing);
    }

    #[test]
    fn shift_adds_anchors_and_a_press_on_a_selected_one_keeps_the_rest() {
        let (a, b) = (AnchorRef::new(0, 0), AnchorRef::new(0, 1));
        assert_eq!(press_anchor(&[], a, false), (vec![a], true));
        assert_eq!(press_anchor(&[a], b, true), (vec![a, b], true));
        assert_eq!(press_anchor(&[a, b], b, false), (vec![a, b], true), "several drag together");
        assert_eq!(press_anchor(&[a, b], a, true), (vec![b], false), "shift takes it back out");
        assert_eq!(press_anchor(&[a], b, false), (vec![b], true));
    }

    #[test]
    fn anchors_are_found_through_the_element_transform() {
        let path = square();
        // Turned a quarter turn and moved: (100, 0) lands at (200, 100).
        let world = Affine::from_row(0.0, 1.0, -1.0, 0.0, 200.0, 0.0);
        assert_eq!(apply(&world, p(100.0, 0.0)), p(200.0, 100.0));
        let inv = world.invert().unwrap();
        let back = apply(&inv, p(200.0, 100.0));
        assert!((back.x - 100.0).abs() < 1e-4 && back.y.abs() < 1e-4);
        let inside = anchors_in(&path, &world, &Rect::new(190.0, 90.0, 20.0, 20.0));
        assert_eq!(inside, vec![AnchorRef::new(0, 1)]);
    }
}
