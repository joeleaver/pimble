//! Paint selections (docs/DRAWING_CONTRACT.md section 3, "Select"; "Built
//! in wave 5 (UI)"): a rectangle (M) or a lasso (L) of a paint layer's
//! pixels, outlined with marching ants in the edit overlay, then moved
//! (drag inside), scaled (the handles; Shift keeps the proportions) and
//! turned (the rotate handle), with the element selection's handles.
//!
//! The pixels are lifted by one `move` operation, written on the first drag
//! (`add_move`) and changed while dragging (`set_move_transform`, about
//! every 80 ms and on release), so others watch it move. The undo clock is
//! held from that first drag until the selection ends, so a whole lift and
//! place is one undo step. Enter, a press outside, another tool or undo
//! keeps it where it is; Escape takes it back (the move goes); Delete
//! clears the selected pixels (a move with no transform).
//!
//! **Pictures placed in a paint layer** (docs/DRAWING_WAVE6_CONTRACT.md
//! section 5) are a selection too: a paste, a drop or Insert Image with a
//! paint layer picked (or a paint tool in hand) shows the picture over the
//! canvas with the same handles, fitted to the view, and nothing is written
//! while it is moved, scaled and turned. Enter, a press outside or another
//! tool commits it: the picture is stored (`PutBlob`, in pieces of at most
//! 2,048 pixels when it is larger, as Flatten tiles a layer) and laid down
//! as `pixels` operations with its rectangle and transform in one edit (one
//! undo step). Escape, Delete, undo or closing the canvas drops it.
//!
//! The pure parts are tested here: a drawn rectangle or lasso as a
//! selection, and a drag as the transform it puts the pixels through.

use pimble_board::{EditOverlay, LayerKind, OverlayPicture, PixelTransform, Selection};

use super::*;

/// How far (logical pixels) a selection must be dragged out to be one.
const MIN_PX: f32 = 3.0;
/// How often the ants march, milliseconds.
const MARCH_MS: u32 = 200;
/// How far apart (logical pixels) a lasso's points are taken.
const LASSO_STEP_PX: f32 = 2.0;

/// The sentence for a selection started where there is no paint.
pub const NO_PAINT_TO_SELECT: &str = "Selections take a paint layer's pixels, and this canvas has no painting yet.";

/// A paint selection on a pane's board.
#[derive(Debug, Clone)]
pub(super) struct PaintSel {
    /// The paint layer whose pixels it holds.
    pub layer: String,
    /// Where it was drawn, board units.
    pub selection: Selection,
    /// What its pixels are put through now (identity until dragged).
    pub transform: PixelTransform,
    /// The `move` operation, once the pixels were first dragged.
    pub move_id: Option<String>,
    /// A picture being placed (its selection is its rectangle): nothing is
    /// written until it is committed. Its `layer` is the paint layer
    /// picked when it began, or empty for the one the brush would paint
    /// into (made at commit when there is none).
    pub placing: Option<Placing>,
}

/// A picture being placed in a paint layer.
#[derive(Debug, Clone)]
pub(super) struct Placing {
    pub picture: Arc<Picture>,
    /// The picture as it came (stored as it is when it is one piece).
    pub bytes: Arc<Vec<u8>>,
}

impl PaintSel {
    /// Its box where its pixels are now (the handles' box).
    pub fn box_now(&self) -> Option<Rect> {
        self.selection.bounds().map(|b| self.transform.map_rect(&b))
    }
}

/// What a drag of the selection does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DragKind {
    /// Pressed inside: moved.
    Move,
    /// A side or corner handle: scaled.
    Scale(Handle),
    /// The rotate handle: turned about the box's center, from the angle
    /// the press made with it.
    Rotate { center: Point, start_angle: f32 },
}

/// A drag of the paint selection under way.
#[derive(Debug, Clone)]
pub(super) struct SelDrag {
    pub kind: DragKind,
    /// Where the press was, board units.
    pub start: Point,
    /// The transform when it began, and the box then.
    pub base: PixelTransform,
    pub bounds: Rect,
    /// The selection's own box, before any transform (a turned selection
    /// scales in its own frame).
    pub own: Rect,
    pub written_at: f64,
    /// This drag made the move operation (a cancel takes it away again).
    pub lifted: bool,
}

/// The selection a drag from `start` to `now` makes: a rectangle, or the
/// lasso through `lasso` (and `now`); `None` when it is smaller than `min`
/// board units either way, or encloses nothing.
pub fn selection_from(start: Point, now: Point, lasso: Option<&[Point]>, min: f32) -> Option<Selection> {
    let selection = match lasso {
        None => Selection::Rect(Rect::from_points(start, now)),
        Some(points) => {
            let mut points = points.to_vec();
            if points.last().is_none_or(|q| q.x != now.x || q.y != now.y) {
                points.push(now);
            }
            Selection::Lasso(points)
        }
    };
    let b = selection.bounds()?;
    (b.w >= min && b.h >= min).then_some(selection)
}

/// The transform a drag to `p` puts the selection's pixels through: the
/// one it began with, then the move, scale or turn (Shift: along one axis,
/// keeping the proportions, by 15 degrees). `bounds` is the handles' box
/// (where the pixels are now), `own` the selection's box before any
/// transform: a turned selection scales in its own frame, along its own
/// sides, so it never shears.
pub fn dragged(kind: &DragKind, base: &PixelTransform, bounds: &Rect, own: &Rect, start: Point, p: Point, shift: bool) -> PixelTransform {
    if let DragKind::Scale(handle) = kind {
        if is_turned(base) {
            return scaled_in_own_frame(*handle, base, own, start, p, shift);
        }
    }
    match kind {
        DragKind::Move => {
            let (mut dx, mut dy) = (p.x - start.x, p.y - start.y);
            if shift {
                if dx.abs() > dy.abs() {
                    dy = 0.0;
                } else {
                    dx = 0.0;
                }
            }
            base.then(&PixelTransform::translate(dx, dy))
        }
        DragKind::Scale(handle) => {
            if bounds.w <= 0.0 || bounds.h <= 0.0 {
                return *base;
            }
            let to = super::tools::resized_bounds(*handle, bounds, p, shift);
            let (sx, sy) = (to.w / bounds.w, to.h / bounds.h);
            base.then(&PixelTransform { a: sx, b: 0.0, c: 0.0, d: sy, e: to.x - bounds.x * sx, f: to.y - bounds.y * sy })
        }
        DragKind::Rotate { center, start_angle } => {
            let a = (p.y - center.y).atan2(p.x - center.x);
            base.then(&PixelTransform::rotate(super::tools::rotation_degrees(*start_angle, a, shift), *center))
        }
    }
}

/// Whether `t` turns (or shears) what it maps: its axes are no longer the
/// board's.
fn is_turned(t: &PixelTransform) -> bool {
    t.b.abs() > 1e-4 || t.c.abs() > 1e-4
}

/// A handle's direction from its box's center (y down); none for the
/// rotate handle.
fn handle_dir(h: Handle) -> Option<(f32, f32)> {
    Some(match h {
        Handle::N => (0.0, -1.0),
        Handle::NE => (1.0, -1.0),
        Handle::E => (1.0, 0.0),
        Handle::SE => (1.0, 1.0),
        Handle::S => (0.0, 1.0),
        Handle::SW => (-1.0, 1.0),
        Handle::W => (-1.0, 0.0),
        Handle::NW => (-1.0, -1.0),
        Handle::Rotate => return None,
    })
}

/// The handle nearest direction (`dx`, `dy`).
fn handle_toward(dx: f32, dy: f32) -> Handle {
    const ALL: [Handle; 8] = [Handle::E, Handle::SE, Handle::S, Handle::SW, Handle::W, Handle::NW, Handle::N, Handle::NE];
    let a = dy.atan2(dx);
    let i = ((a / std::f32::consts::FRAC_PI_4).round() as i32).rem_euclid(8) as usize;
    ALL[i]
}

/// Where handle `h` sits on box `b`.
fn handle_point(h: Handle, b: &Rect) -> Point {
    let (dx, dy) = handle_dir(h).unwrap_or((0.0, 0.0));
    let c = b.center();
    Point::new(c.x + dx * b.w / 2.0, c.y + dy * b.h / 2.0)
}

/// A scale handle of a turned selection dragged from `start` to `p`: the
/// handle of the selection's own box that faces the way the pressed one
/// does, moved by the drag as seen in the selection's own frame, its
/// opposite side staying; then the transform it had.
fn scaled_in_own_frame(handle: Handle, base: &PixelTransform, own: &Rect, start: Point, p: Point, shift: bool) -> PixelTransform {
    let Some(inv) = base.inverse() else { return *base };
    let Some((dx, dy)) = handle_dir(handle) else { return *base };
    if own.w <= 0.0 || own.h <= 0.0 {
        return *base;
    }
    // The pressed handle's direction, in the selection's own frame.
    let local = PixelTransform { e: 0.0, f: 0.0, ..inv }.apply(Point::new(dx, dy));
    let h = handle_toward(local.x, local.y);
    let (q0, q) = (inv.apply(start), inv.apply(p));
    let at = handle_point(h, own);
    let to = super::tools::resized_bounds(h, own, Point::new(at.x + q.x - q0.x, at.y + q.y - q0.y), shift);
    let (sx, sy) = (to.w / own.w, to.h / own.h);
    PixelTransform { a: sx, b: 0.0, c: 0.0, d: sy, e: to.x - own.x * sx, f: to.y - own.y * sy }.then(base)
}

/// Whether pane `pane` has a paint selection (or is drawing one).
pub(super) fn active(pane: PaneId) -> bool {
    with_session(pane, |s| s.paint_sel.is_some() || matches!(s.gesture, Gesture::PaintSelect { .. } | Gesture::PaintSelDrag(_))).unwrap_or(false)
}

/// The paint layer a selection, Smudge or Blur press at `p` works: the
/// picked one when it is a paint layer (a locked one is refused with its
/// sentence), else the topmost shown, unlocked paint layer painting there,
/// else the topmost such layer with any painting.
pub(super) fn paint_layer_at(store: AppStore, pane: PaneId, p: Point, nothing: &str) -> Result<String, String> {
    let picked = untracked(|| store.pane(pane).board_ui.layer.get());
    with_session(pane, |s| {
        let snapshot = s.snapshot();
        if let Some(layer) = picked.as_deref().and_then(|id| snapshot.layer(id)).filter(|l| l.kind == LayerKind::Paint) {
            if layer.effective_locked {
                return Err(pimble_board::locks::LAYER_LOCKED.to_string());
            }
            return Ok(layer.id.clone());
        }
        // Painting there first, else painting anywhere (a selection is
        // often started beside what it takes).
        let open = |(i, l): &(usize, &pimble_board::Layer)| l.kind == LayerKind::Paint && snapshot.paint_shown(*i) && !l.effective_locked;
        let layers = || snapshot.layers.iter().enumerate().rev().filter(open);
        layers()
            .find(|(_, l)| snapshot.paint_bounds(&l.id).is_some_and(|b| b.contains(p)))
            .or_else(|| layers().find(|(_, l)| snapshot.paint_bounds(&l.id).is_some()))
            .map(|(_, l)| l.id.clone())
            .ok_or_else(|| nothing.to_string())
    })
    .unwrap_or_else(|| Err(nothing.to_string()))
}

/// A press with a selection tool at physical pixel (`px`, `py`): on the
/// selection's handles or inside it, a drag of its pixels; elsewhere the
/// selection is kept where it is and a new one starts.
pub(super) fn press(store: AppStore, pane: PaneId, tool: Tool, px: f32, py: f32) {
    if !writable_or_say(store, pane) {
        return;
    }
    let scale = scale_of(pane);
    let grabbed = with_session(pane, |s| {
        let p = s.view.screen_to_board(px, py);
        let sel = s.paint_sel.as_ref()?;
        let bounds = sel.box_now()?;
        let kind = match super::tools::handle_of_box(s.to_screen(&bounds), px, py, scale) {
            Some(Handle::Rotate) => {
                let center = bounds.center();
                DragKind::Rotate { center, start_angle: (p.y - center.y).atan2(p.x - center.x) }
            }
            Some(handle) => DragKind::Scale(handle),
            None if bounds.contains(p) => DragKind::Move,
            None => return None,
        };
        let own = sel.selection.bounds().unwrap_or(bounds);
        let drag = SelDrag { kind, start: p, base: sel.transform, bounds, own, written_at: 0.0, lifted: false };
        s.gesture = Gesture::PaintSelDrag(drag);
        Some(())
    })
    .flatten();
    if grabbed.is_some() {
        after_change(store, pane);
        return;
    }
    // A press outside keeps what was selected where it is (a picture being
    // placed is laid down, and the press does nothing more).
    let was_placing = placing(pane);
    commit(store, pane);
    if was_placing {
        return;
    }
    let Some(p) = with_session(pane, |s| s.view.screen_to_board(px, py)) else { return };
    match paint_layer_at(store, pane, p, NO_PAINT_TO_SELECT) {
        Ok(layer) => {
            with_session(pane, |s| {
                s.selected.clear();
                s.gesture = Gesture::PaintSelect { layer, start: p, now: p, lasso: (tool == Tool::SelectLasso).then(|| vec![p]) };
            });
            march(store, pane);
        }
        Err(sentence) => crate::events::show_notice(store, sentence),
    }
    after_change(store, pane);
}

/// What a pointer move during a selection gesture writes.
enum Next {
    Redraw,
    Write(String, PixelTransform),
    Lift(String, Selection, PixelTransform),
}

/// The pointer moved: a selection being drawn follows it, a drag moves the
/// pixels (the first write lifts them). Answers whether it was a selection
/// gesture.
pub(super) fn moved(store: AppStore, pane: PaneId, px: f32, py: f32) -> bool {
    let scale = scale_of(pane);
    let next = with_session(pane, |s| {
        let p = s.view.screen_to_board(px, py);
        let shift = s.shift;
        let step = LASSO_STEP_PX * scale / s.view.zoom().max(1e-3);
        match &mut s.gesture {
            Gesture::PaintSelect { now, lasso, .. } => {
                *now = p;
                if let Some(points) = lasso {
                    if points.last().is_none_or(|q| (q.x - p.x).hypot(q.y - p.y) >= step) {
                        points.push(p);
                    }
                }
                Some(Next::Redraw)
            }
            Gesture::PaintSelDrag(d) => {
                let t = dragged(&d.kind, &d.base, &d.bounds, &d.own, d.start, p, shift);
                let due = now_ms() - d.written_at >= DRAG_WRITE_EVERY_MS;
                if due {
                    d.written_at = now_ms();
                }
                let sel = s.paint_sel.as_mut()?;
                sel.transform = t;
                // A picture being placed is not written until committed.
                if !due || sel.placing.is_some() {
                    return Some(Next::Redraw);
                }
                Some(match &sel.move_id {
                    Some(id) => Next::Write(id.clone(), t),
                    None => Next::Lift(sel.layer.clone(), sel.selection.clone(), t),
                })
            }
            _ => None,
        }
    })
    .flatten();
    let Some(next) = next else { return false };
    write(store, pane, next);
    true
}

fn write(store: AppStore, pane: PaneId, next: Next) {
    match next {
        Next::Redraw => after_change(store, pane),
        Next::Write(id, t) => {
            local_edit(store, pane, |txn| txn.set_move_transform(&id, Some(t)));
        }
        Next::Lift(layer, selection, t) => lift(store, pane, &layer, selection, Some(t)),
    }
}

/// The selection's pixels lifted: the move operation, in an undo step held
/// open until the selection ends. A refusal (a lock that arrived) ends the
/// selection.
fn lift(store: AppStore, pane: PaneId, layer: &str, selection: Selection, t: Option<PixelTransform>) {
    new_undo_step(pane);
    super::points::hold_clock(pane, true);
    let id = local_edit_said(store, pane, |txn| txn.add_move(layer, selection, t));
    match id {
        Some(id) => {
            with_session(pane, |s| {
                if let Some(sel) = s.paint_sel.as_mut() {
                    sel.move_id = Some(id);
                }
                if let Gesture::PaintSelDrag(d) = &mut s.gesture {
                    d.lifted = true;
                }
            });
        }
        None => {
            super::points::hold_clock(pane, false);
            with_session(pane, |s| {
                s.paint_sel = None;
                if matches!(s.gesture, Gesture::PaintSelDrag(_)) {
                    s.gesture = Gesture::Idle;
                }
            });
            after_change(store, pane);
        }
    }
}

/// The press of a selection gesture was released at (`px`, `py`): a drawn
/// selection becomes the selection; a drag puts the pixels where it ended.
pub(super) fn released(store: AppStore, pane: PaneId, gesture: Gesture, px: f32, py: f32) -> Result<(), Gesture> {
    match gesture {
        Gesture::PaintSelect { layer, start, lasso, .. } => {
            let scale = scale_of(pane);
            with_session(pane, |s| {
                let now = s.view.screen_to_board(px, py);
                let min = MIN_PX * scale / s.view.zoom().max(1e-3);
                s.paint_sel = selection_from(start, now, lasso.as_deref(), min).map(|selection| PaintSel {
                    layer,
                    selection,
                    transform: PixelTransform::IDENTITY,
                    move_id: None,
                    placing: None,
                });
            });
            after_change(store, pane);
            Ok(())
        }
        Gesture::PaintSelDrag(d) => {
            let next = with_session(pane, |s| {
                let p = s.view.screen_to_board(px, py);
                let t = dragged(&d.kind, &d.base, &d.bounds, &d.own, d.start, p, s.shift);
                let sel = s.paint_sel.as_mut()?;
                sel.transform = t;
                if t == d.base || sel.placing.is_some() {
                    return None;
                }
                Some(match &sel.move_id {
                    Some(id) => Next::Write(id.clone(), t),
                    None => Next::Lift(sel.layer.clone(), sel.selection.clone(), t),
                })
            })
            .flatten();
            if let Some(next) = next {
                write(store, pane, next);
            }
            after_change(store, pane);
            Ok(())
        }
        other => Err(other),
    }
}

/// A selection gesture taken back (a cancelled pointer, the first finger
/// of a pinch): a selection being drawn is not made; a drag puts the
/// pixels back where it found them (a move it made goes).
pub(super) fn cancelled(store: AppStore, pane: PaneId, gesture: Gesture) -> Result<(), Gesture> {
    match gesture {
        Gesture::PaintSelect { .. } => {
            after_change(store, pane);
            Ok(())
        }
        Gesture::PaintSelDrag(d) => {
            let undo = with_session(pane, |s| {
                let sel = s.paint_sel.as_mut()?;
                sel.transform = d.base;
                let id = sel.move_id.clone()?;
                if d.lifted {
                    sel.move_id = None;
                    Some((id, true))
                } else {
                    Some((id, false))
                }
            })
            .flatten();
            match undo {
                Some((id, true)) => {
                    local_edit(store, pane, |t| t.delete_paint_op(&id));
                    super::points::hold_clock(pane, false);
                    new_undo_step(pane);
                }
                Some((id, false)) => {
                    let base = d.base;
                    local_edit(store, pane, |t| t.set_move_transform(&id, Some(base)));
                }
                None => {}
            }
            after_change(store, pane);
            Ok(())
        }
        other => Err(other),
    }
}

/// The selection ends with its pixels where they are (Enter, a press
/// outside, another tool, undo, the pane closing): the undo step it held
/// open closes.
pub(super) fn commit(store: AppStore, pane: PaneId) {
    let had = with_session(pane, |s| {
        if matches!(s.gesture, Gesture::PaintSelect { .. } | Gesture::PaintSelDrag(_)) {
            s.gesture = Gesture::Idle;
        }
        s.paint_sel.take()
    })
    .flatten();
    if let Some(sel) = had {
        if sel.move_id.is_some() {
            super::points::hold_clock(pane, false);
            new_undo_step(pane);
        }
        if sel.placing.is_some() {
            store_placed(store, pane, sel);
        }
        after_change(store, pane);
    }
}

/// Escape: the selection ends and its pixels go back where they were (the
/// move operation goes).
pub(super) fn cancel(store: AppStore, pane: PaneId) {
    let had = with_session(pane, |s| {
        s.gesture = Gesture::Idle;
        s.paint_sel.take()
    })
    .flatten();
    let Some(sel) = had else { return };
    if let Some(id) = sel.move_id {
        local_edit(store, pane, |t| t.delete_paint_op(&id));
        super::points::hold_clock(pane, false);
        new_undo_step(pane);
    }
    after_change(store, pane);
}

/// Delete or Backspace: the selected pixels are cleared (a move that puts
/// nothing down), in the selection's undo step, and the selection ends.
pub(super) fn clear(store: AppStore, pane: PaneId) {
    let had = with_session(pane, |s| {
        s.gesture = Gesture::Idle;
        s.paint_sel.take()
    })
    .flatten();
    let Some(sel) = had else { return };
    if sel.placing.is_some() {
        // A picture not placed yet: nothing to clear, it goes.
        after_change(store, pane);
        return;
    }
    match sel.move_id {
        Some(id) => {
            local_edit_said(store, pane, |t| t.set_move_transform(&id, None));
        }
        None => {
            new_undo_step(pane);
            super::points::hold_clock(pane, true);
            local_edit_said(store, pane, |t| t.add_move(&sel.layer, sel.selection.clone(), None));
        }
    }
    super::points::hold_clock(pane, false);
    new_undo_step(pane);
    after_change(store, pane);
}

/// The marching ants over pane `pane`'s board: the selection being drawn,
/// or the selection where its pixels are now.
pub(super) fn overlay(s: &Session) -> Option<EditOverlay> {
    let outline = match &s.gesture {
        Gesture::PaintSelect { start, now, lasso: None, .. } => Selection::Rect(Rect::from_points(*start, *now)).outline(),
        Gesture::PaintSelect { now, lasso: Some(points), .. } => {
            let mut points = points.clone();
            points.push(*now);
            points
        }
        _ => {
            let sel = s.paint_sel.as_ref()?;
            sel.selection.outline_through(&sel.transform)
        }
    };
    let picture = s.paint_sel.as_ref().and_then(|sel| {
        let placing = sel.placing.as_ref()?;
        Some(OverlayPicture { picture: placing.picture.clone(), rect: sel.selection.bounds()?, transform: sel.transform })
    });
    (outline.len() >= 2).then(|| EditOverlay { dashed: vec![outline], dash_phase: s.ants, picture, ..EditOverlay::default() })
}

// ── Pictures placed in a paint layer ────────────────────────────────────────

/// `PutBlob` request ids of placed pictures' pieces start here, clear of
/// image nodes' (`1 << 40`) and flattened layers' (`1 << 41`).
const PLACE_BASE: u64 = 1 << 42;

/// What the layers panel says while a placed picture is stored.
const PLACING_WORDS: &str = "Placing the picture...";

/// More than one picture for a paint layer.
const ONE_AT_A_TIME: &str = "A paint layer takes one picture at a time, so the first was taken.";

/// Whether pane `pane` is placing a picture.
pub(super) fn placing(pane: PaneId) -> bool {
    with_session(pane, |s| s.paint_sel.as_ref().is_some_and(|sel| sel.placing.is_some())).unwrap_or(false)
}

/// Where a picture put on pane `pane`'s board goes as pixels: `Some` when
/// the layers panel's pick is a paint layer (its id) or a paint tool is in
/// hand (empty: the layer the brush would paint into); `None` when it is a
/// picture element as before.
pub(super) fn paints_into(store: AppStore, pane: PaneId) -> Option<String> {
    let picked = untracked(|| store.pane(pane).board_ui.layer.get());
    let paint = with_session(pane, |s| picked.as_deref().and_then(|id| s.snapshot().layer(id)).filter(|l| l.kind == LayerKind::Paint).map(|l| l.id.clone())).flatten();
    if paint.is_some() {
        return paint;
    }
    let tool = untracked(|| store.board_tool.get());
    (tool.paints() || tool.selects_paint() || matches!(tool, Tool::Fill | Tool::Eyedropper)).then(String::new)
}

/// Pictures put on pane `pane`'s board while it paints into `layer`
/// ([`paints_into`]): the first is decoded (off the UI thread on the
/// desktop) and shown for placing, fitted to the view, centered where it
/// was dropped or on the view.
pub(super) fn place_pictures(store: AppStore, pane: PaneId, at: Option<(f32, f32)>, layer: String, mut pictures: Vec<(Option<String>, Vec<u8>)>) {
    if pictures.is_empty() || !writable_or_say(store, pane) {
        return;
    }
    if pictures.len() > 1 {
        crate::events::show_notice(store, ONE_AT_A_TIME.to_string());
    }
    let Some(held) = with_session(pane, |s| (s.store_id, s.node_id)) else { return };
    let (_, bytes) = pictures.swap_remove(0);
    let work = move || -> Result<(Arc<Picture>, Vec<u8>), String> {
        let picture = Picture::decode(&bytes).map_err(|_| NOT_A_PICTURE.to_string())?;
        Ok((Arc::new(picture), bytes))
    };
    #[cfg(all(feature = "native", not(test)))]
    std::thread::spawn(move || {
        let result = work();
        run_on_main_thread(move || begin_placing(store, pane, held, at, layer, result));
    });
    #[cfg(any(not(feature = "native"), test))]
    begin_placing(store, pane, held, at, layer, work());
}

/// The picture was decoded: it shows over the canvas with the selection's
/// handles. A selection or placement under way is kept where it is first.
fn begin_placing(store: AppStore, pane: PaneId, held: (StoreId, NodeId), at: Option<(f32, f32)>, layer: String, decoded: Result<(Arc<Picture>, Vec<u8>), String>) {
    let (picture, bytes) = match decoded {
        Ok(d) => d,
        Err(sentence) => {
            crate::events::show_notice(store, sentence);
            return;
        }
    };
    if with_session(pane, |s| (s.store_id, s.node_id)) != Some(held) {
        return;
    }
    commit(store, pane);
    with_session(pane, |s| {
        let (w, h) = s.view.size();
        let center = match at {
            Some((x, y)) => s.view.screen_to_board(x, y),
            None => s.view.center(),
        };
        let a = s.view.screen_to_board(0.0, 0.0);
        let b = s.view.screen_to_board(w as f32, h as f32);
        let view = Rect::new(a.x.min(b.x), a.y.min(b.y), (b.x - a.x).abs(), (b.y - a.y).abs());
        // Large pictures are fitted to the view at first.
        let rect = super::lay_out(&[(picture.width(), picture.height())], center, view)[0];
        s.selected.clear();
        s.paint_sel = Some(PaintSel {
            layer,
            selection: Selection::Rect(rect),
            transform: PixelTransform::IDENTITY,
            move_id: None,
            placing: Some(Placing { picture, bytes: Arc::new(bytes) }),
        });
    });
    after_change(store, pane);
}

/// A placed picture whose pieces are being stored. Their `PutBlob` request
/// ids are `first .. first + pieces`.
struct PendingPlace {
    pane: PaneId,
    held: (StoreId, NodeId),
    layer: String,
    transform: PixelTransform,
    /// Each piece's rectangle, and its URL once stored.
    rects: Vec<Rect>,
    srcs: Vec<Option<String>>,
    first: u64,
    /// One piece stored as it came: this window's picture cache gets the
    /// decoded picture (a picture in pieces is fetched as any is).
    whole: Option<(Arc<Picture>, Arc<Vec<u8>>)>,
}

thread_local! {
    static PLACES: RefCell<Vec<PendingPlace>> = const { RefCell::new(Vec::new()) };
    static NEXT_PLACE: Cell<u64> = const { Cell::new(PLACE_BASE) };
}

/// A placement committed: its pieces (the picture as it came, or a grid
/// of tiles past 2,048 pixels on a side, cut off the UI thread on the
/// desktop) go to be stored; nothing is written until they all are.
fn store_placed(store: AppStore, pane: PaneId, sel: PaintSel) {
    let Some(placing) = sel.placing else { return };
    let Some(rect) = sel.selection.bounds() else { return };
    let Some(held) = with_session(pane, |s| (s.store_id, s.node_id)) else { return };
    let ui = store.pane(pane).board_ui;
    ui.rasterizing.set(Some(PLACING_WORDS));
    let (layer, transform) = (sel.layer, sel.transform);
    let picture = placing.picture.clone();
    let work = move || -> Result<Vec<(Vec<u8>, Rect)>, String> {
        let pieces = pimble_board::picture_pieces(&picture, rect).map_err(|e| e.to_string())?;
        Ok(pieces.into_iter().map(|p| (p.bytes, p.rect)).collect())
    };
    if !pimble_board::needs_pieces(placing.picture.width(), placing.picture.height()) {
        let pieces = vec![((*placing.bytes).clone(), rect)];
        send_pieces(store, PendingPlace { pane, held, layer, transform, rects: Vec::new(), srcs: Vec::new(), first: 0, whole: Some((placing.picture, placing.bytes)) }, pieces);
        return;
    }
    let pending = PendingPlace { pane, held, layer, transform, rects: Vec::new(), srcs: Vec::new(), first: 0, whole: None };
    #[cfg(all(feature = "native", not(test)))]
    std::thread::spawn(move || {
        let result = work();
        run_on_main_thread(move || cut(store, pending, result));
    });
    #[cfg(any(not(feature = "native"), test))]
    cut(store, pending, work());
}

fn cut(store: AppStore, pending: PendingPlace, result: Result<Vec<(Vec<u8>, Rect)>, String>) {
    match result {
        Ok(pieces) => send_pieces(store, pending, pieces),
        Err(sentence) => {
            store.pane(pending.pane).board_ui.rasterizing.set(None);
            crate::events::show_notice(store, sentence);
        }
    }
}

fn send_pieces(store: AppStore, mut pending: PendingPlace, pieces: Vec<(Vec<u8>, Rect)>) {
    let first = NEXT_PLACE.with(|n| n.replace(n.get() + pieces.len() as u64));
    pending.first = first;
    pending.rects = pieces.iter().map(|(_, r)| *r).collect();
    pending.srcs = vec![None; pieces.len()];
    let held = pending.held;
    PLACES.with(|p| p.borrow_mut().push(pending));
    for (i, (bytes, _)) in pieces.into_iter().enumerate() {
        store.send(BackendCommand::PutBlob { store_id: held.0, node_id: held.1, bytes, request_id: first + i as u64 });
    }
}

/// The answer to a placed picture's piece's `PutBlob`; answers whether it
/// was one. Once every piece is stored the picture is laid down in one edit
/// (one undo step): the paint layer (made when needed) and a `pixels`
/// operation per piece, with the placement's transform. A piece that could
/// not be stored ends it with its sentence and nothing written.
pub(super) fn placed_stored(store: AppStore, request_id: u64, result: &Result<StoredBlob, String>) -> bool {
    let at = PLACES.with(|p| p.borrow().iter().position(|p| (p.first..p.first + p.srcs.len() as u64).contains(&request_id)));
    let Some(at) = at else { return false };
    let blob = match result {
        Err(sentence) => {
            let pending = PLACES.with(|p| p.borrow_mut().remove(at));
            store.pane(pending.pane).board_ui.rasterizing.set(None);
            crate::events::show_notice(store, sentence.clone());
            return true;
        }
        Ok(blob) => blob,
    };
    if let Some(notice) = &blob.notice {
        crate::events::show_notice(store, notice.clone());
    }
    let done = PLACES.with(|p| {
        let mut p = p.borrow_mut();
        let index = (request_id - p[at].first) as usize;
        p[at].srcs[index] = Some(blob.url.to_string());
        p[at].srcs.iter().all(Option::is_some)
    });
    if !done {
        return true;
    }
    let PendingPlace { pane, held, layer, transform, rects, srcs, whole, .. } = PLACES.with(|p| p.borrow_mut().remove(at));
    store.pane(pane).board_ui.rasterizing.set(None);
    if with_session(pane, |s| (s.store_id, s.node_id)) != Some(held) {
        crate::events::show_notice(store, "The canvas was closed before its picture was stored, so nothing changed.".to_string());
        return true;
    }
    let pieces: Vec<(String, Rect)> = srcs.into_iter().flatten().zip(rects).collect();
    if let (Some((picture, bytes)), [(src, _)]) = (whole, pieces.as_slice()) {
        if blob.size == Some((picture.width(), picture.height())) {
            super::seed_picture(src, picture, (*bytes).clone());
        }
    }
    let picked = untracked(|| store.pane(pane).board_ui.layer.get());
    new_undo_step(pane);
    let laid = local_edit_said(store, pane, |t| {
        let into = if layer.is_empty() { t.paint_layer_for(picked.as_deref())? } else { layer.clone() };
        t.add_placed_pixels(&into, &pieces, Some(transform))?;
        Ok(into)
    });
    new_undo_step(pane);
    if let Some(into) = laid {
        store.pane(pane).board_ui.layer.set_if_changed(Some(into));
        super::request_wanted_pictures();
    }
    true
}

/// A picture being placed goes without being laid down (undo, the canvas
/// closing); answers whether there was one.
pub(super) fn drop_placing(store: AppStore, pane: PaneId) -> bool {
    let had = with_session(pane, |s| {
        if !s.paint_sel.as_ref().is_some_and(|sel| sel.placing.is_some()) {
            return false;
        }
        if matches!(s.gesture, Gesture::PaintSelDrag(_)) {
            s.gesture = Gesture::Idle;
        }
        s.paint_sel = None;
        true
    })
    .unwrap_or(false);
    if had {
        after_change(store, pane);
    }
    had
}

thread_local! {
    static MARCHING: Cell<[bool; MAX_PANES]> = const { Cell::new([false; MAX_PANES]) };
}

/// Set the ants of pane `pane` marching, while it has a selection.
fn march(store: AppStore, pane: PaneId) {
    let already = MARCHING.with(|m| {
        let mut all = m.get();
        let was = all[pane.index()];
        all[pane.index()] = true;
        m.set(all);
        was
    });
    if !already {
        let _ = set_timeout(MARCH_MS, move || step(store, pane));
    }
}

fn step(store: AppStore, pane: PaneId) {
    // Another pane has the person's attention: the ants stand still (and
    // nothing is drawn) until this one has it again.
    let focused = untracked(|| store.focused_pane.get()) == pane;
    let go = with_session(pane, |s| {
        // A picture being placed shows still ants: marching would redraw
        // the picture under them five times a second.
        let on = s.paint_sel.as_ref().is_some_and(|sel| sel.placing.is_none()) || matches!(s.gesture, Gesture::PaintSelect { .. });
        if on && focused {
            s.ants = (s.ants + 1.0) % 64.0;
        }
        on
    })
    .unwrap_or(false);
    if !go {
        MARCHING.with(|m| {
            let mut all = m.get();
            all[pane.index()] = false;
            m.set(all);
        });
        return;
    }
    if focused {
        // Only the overlay changed: the view redraws just under the ants
        // (`BoardView::frame`), not the whole frame.
        super::points::refresh_overlay(store, pane);
        present(pane);
    }
    let _ = set_timeout(MARCH_MS, move || step(store, pane));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

    fn near(a: Point, b: Point) -> bool {
        (a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3
    }

    #[test]
    fn a_drag_makes_a_rectangle_or_a_lasso() {
        // Dragged either way, the same rectangle.
        assert_eq!(selection_from(p(10.0, 20.0), p(0.0, 0.0), None, 1.0), Some(Selection::Rect(Rect::new(0.0, 0.0, 10.0, 20.0))));
        // Too small to be one.
        assert_eq!(selection_from(p(0.0, 0.0), p(0.5, 30.0), None, 1.0), None);
        // A lasso closes through where the pointer was let go.
        let lasso = selection_from(p(0.0, 0.0), p(0.0, 10.0), Some(&[p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0)]), 1.0).unwrap();
        assert_eq!(lasso, Selection::Lasso(vec![p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0), p(0.0, 10.0)]));
        assert!(lasso.contains(p(5.0, 5.0)));
        // A lasso along a line encloses nothing.
        assert_eq!(selection_from(p(0.0, 0.0), p(20.0, 0.0), Some(&[p(0.0, 0.0), p(10.0, 0.0)]), 1.0), None);
    }

    #[test]
    fn a_drag_inside_moves_the_pixels() {
        let bounds = Rect::new(0.0, 0.0, 100.0, 50.0);
        let t = dragged(&DragKind::Move, &PixelTransform::IDENTITY, &bounds, &bounds, p(10.0, 10.0), p(40.0, 15.0), false);
        assert!(near(t.apply(p(0.0, 0.0)), p(30.0, 5.0)));
        // Shift: along the axis it moved most.
        let t = dragged(&DragKind::Move, &PixelTransform::IDENTITY, &bounds, &bounds, p(10.0, 10.0), p(40.0, 15.0), true);
        assert!(near(t.apply(p(0.0, 0.0)), p(30.0, 0.0)));
        // A second drag goes on from where the first left it.
        let t2 = dragged(&DragKind::Move, &t, &t.map_rect(&bounds), &bounds, p(0.0, 0.0), p(0.0, 7.0), false);
        assert!(near(t2.apply(p(0.0, 0.0)), p(30.0, 7.0)));
    }

    #[test]
    fn a_handle_scales_and_shift_keeps_the_proportions() {
        let bounds = Rect::new(0.0, 0.0, 100.0, 50.0);
        // The bottom right corner to (200, 150): twice as wide, three times
        // as tall, the top left staying.
        let t = dragged(&DragKind::Scale(Handle::SE), &PixelTransform::IDENTITY, &bounds, &bounds, p(100.0, 50.0), p(200.0, 150.0), false);
        assert!(near(t.apply(p(0.0, 0.0)), p(0.0, 0.0)));
        assert!(near(t.apply(p(100.0, 50.0)), p(200.0, 150.0)));
        // With Shift, one factor for both.
        let t = dragged(&DragKind::Scale(Handle::SE), &PixelTransform::IDENTITY, &bounds, &bounds, p(100.0, 50.0), p(200.0, 150.0), true);
        let b = t.map_rect(&bounds);
        assert!((b.w / b.h - 2.0).abs() < 1e-3, "{b:?}");
        // The left side: the right side stays.
        let t = dragged(&DragKind::Scale(Handle::W), &PixelTransform::IDENTITY, &bounds, &bounds, p(0.0, 25.0), p(50.0, 25.0), false);
        assert!(near(t.apply(p(100.0, 0.0)), p(100.0, 0.0)));
        assert!(near(t.apply(p(0.0, 0.0)), p(50.0, 0.0)));
        // A press on a handle that does not move changes nothing.
        let t = dragged(&DragKind::Scale(Handle::NE), &PixelTransform::IDENTITY, &bounds, &bounds, p(100.0, 0.0), p(100.0, 0.0), false);
        assert!(near(t.apply(p(37.0, 12.0)), p(37.0, 12.0)));
    }

    #[test]
    fn a_turned_selection_scales_along_its_own_sides_without_shearing() {
        let own = Rect::new(0.0, 0.0, 100.0, 50.0);
        let turned = PixelTransform::rotate(30.0, own.center());
        let bounds = turned.map_rect(&own);
        // The box's right handle, dragged 40 units along the selection's own
        // x axis (30 degrees down the board).
        let (c, s) = (30.0f32.to_radians().cos(), 30.0f32.to_radians().sin());
        let start = p(bounds.right(), bounds.center().y);
        let to = p(start.x + 40.0 * c, start.y + 40.0 * s);
        let t = dragged(&DragKind::Scale(Handle::E), &turned, &bounds, &own, start, to, false);
        // Still a turn and a scale: the mapped sides stay at right angles.
        let (ex, ey) = (t.apply(p(1.0, 0.0)), t.apply(p(0.0, 1.0)));
        let o = t.apply(p(0.0, 0.0));
        let (ux, uy) = ((ex.x - o.x, ex.y - o.y), (ey.x - o.x, ey.y - o.y));
        assert!((ux.0 * uy.0 + ux.1 * uy.1).abs() < 1e-3, "no shear: {ux:?} {uy:?}");
        // Wider along its own side by the drag, as tall as before.
        assert!(((ux.0.hypot(ux.1)) - 1.4).abs() < 1e-3, "{ux:?}");
        assert!(((uy.0.hypot(uy.1)) - 1.0).abs() < 1e-3, "{uy:?}");
        // Its left side stays where it was.
        assert!(near(t.apply(p(0.0, 25.0)), turned.apply(p(0.0, 25.0))));
    }

    #[test]
    fn the_rotate_handle_turns_about_the_center() {
        let bounds = Rect::new(0.0, 0.0, 100.0, 100.0);
        let center = bounds.center();
        // From above the center to its right: a quarter turn clockwise.
        let start = p(50.0, -20.0);
        let kind = DragKind::Rotate { center, start_angle: (start.y - center.y).atan2(start.x - center.x) };
        let t = dragged(&kind, &PixelTransform::IDENTITY, &bounds, &bounds, start, p(150.0, 50.0), false);
        assert!(near(t.apply(center), center));
        assert!(near(t.apply(p(50.0, 0.0)), p(100.0, 50.0)), "{:?}", t.apply(p(50.0, 0.0)));
        // Shift snaps to 15 degrees: 40 degrees turns 45.
        let a = (-50.0f32).to_radians() - std::f32::consts::FRAC_PI_2 + 50.0f32.to_radians() + 40.0f32.to_radians();
        let to = p(center.x + 70.0 * a.cos(), center.y + 70.0 * a.sin());
        let t = dragged(&kind, &PixelTransform::IDENTITY, &bounds, &bounds, start, to, true);
        let top = t.apply(p(50.0, 0.0));
        let turned = (top.y - center.y).atan2(top.x - center.x).to_degrees() + 90.0;
        assert!((turned - 45.0).abs() < 1e-2, "{turned}");
    }
}
