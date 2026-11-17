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
//! The pure parts are tested here: a drawn rectangle or lasso as a
//! selection, and a drag as the transform it puts the pixels through.

use pimble_board::{EditOverlay, LayerKind, PixelTransform, Selection};

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
/// keeping the proportions, by 15 degrees).
pub fn dragged(kind: &DragKind, base: &PixelTransform, bounds: &Rect, start: Point, p: Point, shift: bool) -> PixelTransform {
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
        let drag = SelDrag { kind, start: p, base: sel.transform, bounds, written_at: 0.0, lifted: false };
        s.gesture = Gesture::PaintSelDrag(drag);
        Some(())
    })
    .flatten();
    if grabbed.is_some() {
        after_change(store, pane);
        return;
    }
    // A press outside keeps what was selected where it is.
    commit(store, pane);
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
                let t = dragged(&d.kind, &d.base, &d.bounds, d.start, p, shift);
                let due = now_ms() - d.written_at >= DRAG_WRITE_EVERY_MS;
                if due {
                    d.written_at = now_ms();
                }
                let sel = s.paint_sel.as_mut()?;
                sel.transform = t;
                if !due {
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
                });
            });
            after_change(store, pane);
            Ok(())
        }
        Gesture::PaintSelDrag(d) => {
            let next = with_session(pane, |s| {
                let p = s.view.screen_to_board(px, py);
                let t = dragged(&d.kind, &d.base, &d.bounds, d.start, p, s.shift);
                let sel = s.paint_sel.as_mut()?;
                sel.transform = t;
                if t == d.base {
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
    (outline.len() >= 2).then(|| EditOverlay { dashed: vec![outline], dash_phase: s.ants, ..EditOverlay::default() })
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
    let go = with_session(pane, |s| {
        let on = s.paint_sel.is_some() || matches!(s.gesture, Gesture::PaintSelect { .. });
        if on {
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
    super::points::refresh_overlay(store, pane);
    present(pane);
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
        let t = dragged(&DragKind::Move, &PixelTransform::IDENTITY, &bounds, p(10.0, 10.0), p(40.0, 15.0), false);
        assert!(near(t.apply(p(0.0, 0.0)), p(30.0, 5.0)));
        // Shift: along the axis it moved most.
        let t = dragged(&DragKind::Move, &PixelTransform::IDENTITY, &bounds, p(10.0, 10.0), p(40.0, 15.0), true);
        assert!(near(t.apply(p(0.0, 0.0)), p(30.0, 0.0)));
        // A second drag goes on from where the first left it.
        let t2 = dragged(&DragKind::Move, &t, &t.map_rect(&bounds), p(0.0, 0.0), p(0.0, 7.0), false);
        assert!(near(t2.apply(p(0.0, 0.0)), p(30.0, 7.0)));
    }

    #[test]
    fn a_handle_scales_and_shift_keeps_the_proportions() {
        let bounds = Rect::new(0.0, 0.0, 100.0, 50.0);
        // The bottom right corner to (200, 150): twice as wide, three times
        // as tall, the top left staying.
        let t = dragged(&DragKind::Scale(Handle::SE), &PixelTransform::IDENTITY, &bounds, p(100.0, 50.0), p(200.0, 150.0), false);
        assert!(near(t.apply(p(0.0, 0.0)), p(0.0, 0.0)));
        assert!(near(t.apply(p(100.0, 50.0)), p(200.0, 150.0)));
        // With Shift, one factor for both.
        let t = dragged(&DragKind::Scale(Handle::SE), &PixelTransform::IDENTITY, &bounds, p(100.0, 50.0), p(200.0, 150.0), true);
        let b = t.map_rect(&bounds);
        assert!((b.w / b.h - 2.0).abs() < 1e-3, "{b:?}");
        // The left side: the right side stays.
        let t = dragged(&DragKind::Scale(Handle::W), &PixelTransform::IDENTITY, &bounds, p(0.0, 25.0), p(50.0, 25.0), false);
        assert!(near(t.apply(p(100.0, 0.0)), p(100.0, 0.0)));
        assert!(near(t.apply(p(0.0, 0.0)), p(50.0, 0.0)));
        // A press on a handle that does not move changes nothing.
        let t = dragged(&DragKind::Scale(Handle::NE), &PixelTransform::IDENTITY, &bounds, p(100.0, 0.0), p(100.0, 0.0), false);
        assert!(near(t.apply(p(37.0, 12.0)), p(37.0, 12.0)));
    }

    #[test]
    fn the_rotate_handle_turns_about_the_center() {
        let bounds = Rect::new(0.0, 0.0, 100.0, 100.0);
        let center = bounds.center();
        // From above the center to its right: a quarter turn clockwise.
        let start = p(50.0, -20.0);
        let kind = DragKind::Rotate { center, start_angle: (start.y - center.y).atan2(start.x - center.x) };
        let t = dragged(&kind, &PixelTransform::IDENTITY, &bounds, start, p(150.0, 50.0), false);
        assert!(near(t.apply(center), center));
        assert!(near(t.apply(p(50.0, 0.0)), p(100.0, 50.0)), "{:?}", t.apply(p(50.0, 0.0)));
        // Shift snaps to 15 degrees: 40 degrees turns 45.
        let a = (-50.0f32).to_radians() - std::f32::consts::FRAC_PI_2 + 50.0f32.to_radians() + 40.0f32.to_radians();
        let to = p(center.x + 70.0 * a.cos(), center.y + 70.0 * a.sin());
        let t = dragged(&kind, &PixelTransform::IDENTITY, &bounds, start, to, true);
        let top = t.apply(p(50.0, 0.0));
        let turned = (top.y - center.y).atan2(top.x - center.x).to_degrees() + 90.0;
        assert!((turned - 45.0).abs() < 1e-2, "{turned}");
    }
}
