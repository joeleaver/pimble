//! The color well's gradients (docs/DRAWING_CONTRACT.md section 2, "Fill
//! and stroke"; "Built in wave 3 (UI)"): the Solid / Linear / Radial switch,
//! the stop bar, the CSS the well's squares and the context bar's swatches
//! show a gradient with, and the gradient's two handles on the canvas.
//!
//! The pure parts (stop bar math, switching kinds, CSS) are tested here; the
//! handles' gesture is at the end.

use std::rc::Rc;

use pimble_board::tiny_skia::Transform as Affine;
use pimble_board::{EditOverlay, Gradient, GradientKind, GradientStop, Paint};

use super::*;

/// A gradient keeps at least this many stops (the stop bar's remove refuses
/// below it).
pub const MIN_STOPS: usize = 2;
/// How near a press on the stop bar must be to a stop to take it, as a
/// fraction of the bar's width.
pub const STOP_REACH: f32 = 0.04;
/// How near a press must be to a gradient handle on the canvas, logical
/// pixels.
const HANDLE_REACH: f32 = 9.0;

// ── Pure ────────────────────────────────────────────────────────────────────

/// A color as CSS (alpha kept).
pub fn css_color(c: Color) -> String {
    if c.a == 255 {
        c.to_hex()
    } else {
        format!("rgba({}, {}, {}, {:.3})", c.r, c.g, c.b, c.a as f32 / 255.0)
    }
}

/// The stops as a CSS color-stop list.
fn css_stops(g: &Gradient) -> String {
    let g = g.clone().normalized();
    g.stops.iter().map(|s| format!("{} {:.1}%", css_color(s.color), s.offset * 100.0)).collect::<Vec<_>>().join(", ")
}

/// The gradient as a CSS background, for a swatch (a square box: a linear
/// one's angle is its handles' direction in box units).
pub fn css(g: &Gradient) -> String {
    match g.kind {
        GradientKind::Linear => {
            let (dx, dy) = (g.to.x - g.from.x, g.to.y - g.from.y);
            // CSS angles: 0deg points up, 90deg right.
            let deg = if dx.abs() < 1e-6 && dy.abs() < 1e-6 { 90.0 } else { dx.atan2(-dy).to_degrees() };
            format!("linear-gradient({deg:.0}deg, {})", css_stops(g))
        }
        GradientKind::Radial => format!("radial-gradient(circle, {})", css_stops(g)),
    }
}

/// The stop bar's background: the stops left to right.
pub fn bar_css(g: &Gradient) -> String {
    format!("linear-gradient(90deg, {})", css_stops(g))
}

/// A press `x` logical pixels into a bar `width` wide, as an offset.
pub fn offset_at(x: f32, width: f32) -> f32 {
    if width <= 0.0 || !x.is_finite() {
        return 0.0;
    }
    (x / width).clamp(0.0, 1.0)
}

/// The stop nearest offset `t`, when one is within `reach`.
pub fn stop_near(g: &Gradient, t: f32, reach: f32) -> Option<usize> {
    g.stops
        .iter()
        .enumerate()
        .map(|(i, s)| (i, (s.offset - t).abs()))
        .filter(|(_, d)| *d <= reach)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// A stop added at `t`, in the color the gradient has there, in order (after
/// any stop at the same offset). Answers the gradient and the new stop's
/// index.
pub fn add_stop(g: &Gradient, t: f32) -> (Gradient, usize) {
    let t = t.clamp(0.0, 1.0);
    let mut g = g.clone().normalized();
    let color = g.color_at(t);
    let at = g.stops.iter().position(|s| s.offset > t).unwrap_or(g.stops.len());
    g.stops.insert(at, GradientStop::new(t, color));
    (g, at)
}

/// Stop `i` moved to `t`, the stops kept in order. Answers the gradient and
/// where the stop is now.
pub fn move_stop(g: &Gradient, i: usize, t: f32) -> (Gradient, usize) {
    let mut g = g.clone();
    if i >= g.stops.len() {
        return (g, i);
    }
    let mut stop = g.stops.remove(i);
    let (old, t) = (stop.offset, t.clamp(0.0, 1.0));
    stop.offset = t;
    // Stops it passes go to its other side; one at its new offset stays on
    // the side it came from.
    let at = if t >= old {
        g.stops.iter().position(|s| s.offset >= t)
    } else {
        g.stops.iter().position(|s| s.offset > t)
    }
    .unwrap_or(g.stops.len());
    g.stops.insert(at, stop);
    (g, at)
}

/// Stop `i` taken out: `None` when that would leave fewer than
/// [`MIN_STOPS`].
pub fn remove_stop(g: &Gradient, i: usize) -> Option<Gradient> {
    if g.stops.len() <= MIN_STOPS || i >= g.stops.len() {
        return None;
    }
    let mut g = g.clone();
    g.stops.remove(i);
    Some(g)
}

/// Stop `i` in `color`.
pub fn with_stop_color(g: &Gradient, i: usize, color: Color) -> Gradient {
    let mut g = g.clone();
    if let Some(s) = g.stops.get_mut(i) {
        s.color = color;
    }
    g
}

/// What the well's switch makes of the paint: `None` (Solid) drops the
/// gradient; a kind keeps the stops of the gradient there is (its handles
/// the new kind's defaults when the kind changes), or starts the kind's
/// default from `color`.
pub fn switched(current: Option<&Gradient>, kind: Option<GradientKind>, color: Color) -> Option<Gradient> {
    let kind = kind?;
    let fresh = match kind {
        GradientKind::Linear => Gradient::default_linear(color),
        GradientKind::Radial => Gradient::default_radial(color),
    };
    match current {
        Some(g) if g.kind == kind => Some(g.clone()),
        Some(g) => Some(Gradient { stops: g.stops.clone(), ..fresh }),
        None => Some(fresh),
    }
}

/// A gradient handle in box units, mapped to board units: through `bbox`
/// (the element's box, [`Gradient::box_for`] of its pivot bounds) and its
/// world transform.
pub fn handle_on_board(world: &Affine, bbox: Rect, p: Point) -> Point {
    let q = Point::new(bbox.x + p.x * bbox.w, bbox.y + p.y * bbox.h);
    Point::new(world.sx * q.x + world.kx * q.y + world.tx, world.ky * q.x + world.sy * q.y + world.ty)
}

/// A board point back in box units (`inverse` is the world transform's).
pub fn handle_in_box(inverse: &Affine, bbox: Rect, p: Point) -> Point {
    let q = Point::new(inverse.sx * p.x + inverse.kx * p.y + inverse.tx, inverse.ky * p.x + inverse.sy * p.y + inverse.ty);
    let (w, h) = (if bbox.w.abs() < 1e-6 { 1.0 } else { bbox.w }, if bbox.h.abs() < 1e-6 { 1.0 } else { bbox.h });
    Point::new((q.x - bbox.x) / w, (q.y - bbox.y) / h)
}

// ── The well ────────────────────────────────────────────────────────────────

/// The front square's gradient signal.
pub(super) fn front_signal(st: StyleUi) -> Signal<Option<Gradient>> {
    if untracked(|| st.front.get()) == rail::Well::Fill { st.fill_gradient } else { st.stroke_gradient }
}

/// The gradient of square `well`, tracked.
pub(super) fn of_well(st: StyleUi, well: rail::Well) -> Option<Gradient> {
    match well {
        rail::Well::Fill => st.fill_gradient.get(),
        rail::Well::Stroke => st.stroke_gradient.get(),
    }
}

/// The front square's gradient becomes `g` (with its stop `stop` picked):
/// new elements take it, the selection gets it.
pub(super) fn set_front(store: AppStore, pane: PaneId, g: Option<Gradient>, stop: usize) {
    let st = store.board_style;
    let well = untracked(|| st.front.get());
    let sig = front_signal(st);
    sig.set(g.clone());
    st.stop.set(stop);
    let paint = match (g, well) {
        (Some(g), _) => Paint::Gradient(g),
        (None, rail::Well::Fill) => match untracked(|| st.fill.get()).as_deref().and_then(Color::parse) {
            Some(c) => Paint::Solid(c),
            None => Paint::None,
        },
        (None, rail::Well::Stroke) => match Color::parse(&untracked(|| st.stroke.get())) {
            Some(c) => Paint::Solid(c),
            None => return,
        },
    };
    tools::apply_paint(store, pane, well, &paint);
}

/// The switch: Solid (`None`), Linear or Radial for the front square.
pub(super) fn switch_to(store: AppStore, pane: PaneId, kind: Option<GradientKind>) {
    let st = store.board_style;
    let well = untracked(|| st.front.get());
    let color = match well {
        rail::Well::Fill => untracked(|| st.fill.get()).unwrap_or_else(|| untracked(|| st.stroke.get())),
        rail::Well::Stroke => untracked(|| st.stroke.get()),
    };
    let color = Color::parse(&color).unwrap_or(Color::BLACK);
    let current = untracked(|| front_signal(st).get());
    if current.as_ref().map(|g| g.kind) == kind {
        return;
    }
    if kind.is_some() && well == rail::Well::Fill && untracked(|| st.fill.get()).is_none() {
        st.fill.set(Some(color.to_hex()));
    }
    let g = switched(current.as_ref(), kind, color);
    // The picked stop stays picked when the stops are kept.
    let stop = match (&current, &g) {
        (Some(_), Some(g)) => untracked(|| st.stop.get()).min(g.stops.len().saturating_sub(1)),
        _ => 0,
    };
    set_front(store, pane, g, stop);
}

/// A press on the stop bar at `t`: a stop near it is picked and dragged, or
/// a new one is added there and dragged. The drag writes about every 80 ms
/// and at its end, as one undo step.
pub(super) fn press_bar(store: AppStore, pane: PaneId, t: f32) {
    let st = store.board_style;
    let sig = front_signal(st);
    let Some(g) = untracked(|| sig.get()) else { return };
    let (g, i) = match stop_near(&g, t, STOP_REACH) {
        Some(i) => (g, i),
        None => add_stop(&g, t),
    };
    sig.set(Some(g.clone()));
    st.stop.set(i);
    new_undo_step(pane);
    points::hold_clock(pane, true);
    tools::apply_paint_quietly(store, pane, untracked(|| st.front.get()), &Paint::Gradient(g));
    let written = Rc::new(Cell::new(now_ms()));
    let moved = written.clone();
    let end = move |_: f32, _: f32| {
        let st = store.board_style;
        if let Some(g) = untracked(|| front_signal(st).get()) {
            tools::apply_paint_quietly(store, pane, untracked(|| st.front.get()), &Paint::Gradient(g));
        }
        points::hold_clock(pane, false);
    };
    rinch::core::Drag::percent()
        .on_move(move |x, _| {
            let st = store.board_style;
            let sig = front_signal(st);
            let Some(g) = untracked(|| sig.get()) else { return };
            let (g, i) = move_stop(&g, untracked(|| st.stop.get()), x);
            sig.set(Some(g.clone()));
            st.stop.set(i);
            if now_ms() - moved.get() >= DRAG_WRITE_EVERY_MS {
                moved.set(now_ms());
                tools::apply_paint_quietly(store, pane, untracked(|| st.front.get()), &Paint::Gradient(g));
            }
        })
        .on_end(end)
        .on_cancel(end)
        .start();
}

/// The picked stop removed (at least [`MIN_STOPS`] stay).
pub(super) fn remove_picked(store: AppStore, pane: PaneId) {
    let st = store.board_style;
    let Some(g) = untracked(|| front_signal(st).get()) else { return };
    let i = untracked(|| st.stop.get());
    match remove_stop(&g, i) {
        Some(g) => {
            let next = i.min(g.stops.len() - 1);
            set_front(store, pane, Some(g), next);
        }
        None => crate::events::show_notice(store, "A gradient keeps at least two colors.".to_string()),
    }
}

/// A color picked while the front square has a gradient: the picked stop
/// takes it.
pub(super) fn color_picked(store: AppStore, pane: PaneId, hex: &str) -> bool {
    let st = store.board_style;
    let Some(g) = untracked(|| front_signal(st).get()) else { return false };
    let Some(c) = Color::parse(hex) else { return true };
    let i = untracked(|| st.stop.get()).min(g.stops.len().saturating_sub(1));
    set_front(store, pane, Some(with_stop_color(&g, i, c)), i);
    st.recent.update(|r| *r = rail::remember_recent(r, &Color { a: 255, ..c }.to_hex()));
    true
}

// ── The handles on the canvas ───────────────────────────────────────────────

/// The element whose gradient handles show, its gradient, and which square
/// it is: one element selected, the well's picker open, the front square a
/// gradient the element has.
pub(super) struct Shown {
    pub id: String,
    pub fill: bool,
    pub gradient: Gradient,
    pub bbox: Rect,
    pub world: Affine,
}

/// Whether the gradient's handles take presses with `tool` (the Pen and
/// Direct select have their own points, the Hand pans, the eraser erases).
pub(super) fn handles_with(tool: Tool) -> bool {
    !matches!(tool, Tool::Pen | Tool::DirectSelect | Tool::Hand | Tool::Eraser | Tool::Brush | Tool::Highlighter | Tool::Eyedropper)
}

/// What [`Shown`] the session has now, judged from what the well shows.
pub(super) fn shown(store: AppStore, pane: PaneId, s: &Session) -> Option<Shown> {
    let ui = store.pane(pane).board_ui;
    if untracked(|| ui.popup.get()) != Popup::Well {
        return None;
    }
    let [id] = s.selected.as_slice() else { return None };
    let fill = untracked(|| store.board_style.front.get()) == rail::Well::Fill;
    let e = s.snapshot().get(id)?;
    let gradient = if fill { e.style.fill_gradient.clone() } else { e.style.stroke_gradient.clone() }?;
    // A drag under way draws its own.
    let gradient = match &s.gesture {
        Gesture::GradientHandle(d) if d.id == *id => d.working.clone(),
        _ => gradient,
    };
    Some(Shown { id: id.clone(), fill, gradient, bbox: Gradient::box_for(e.pivot_bounds), world: e.world })
}

/// The overlay of the handles: an arm from `from` to `to`, `from` drawn as
/// an anchor, `to` as a handle tip.
pub(super) fn overlay(sh: &Shown) -> EditOverlay {
    let from = handle_on_board(&sh.world, sh.bbox, sh.gradient.from);
    let to = handle_on_board(&sh.world, sh.bbox, sh.gradient.to);
    EditOverlay { arms: vec![(from, to)], anchors: vec![(from, true)], tips: vec![to], ..EditOverlay::default() }
}

/// A drag of one gradient handle.
pub(super) struct HandleDrag {
    pub id: String,
    pub fill: bool,
    /// 0: `from`, 1: `to`.
    pub end: usize,
    pub base: Gradient,
    pub working: Gradient,
    pub bbox: Rect,
    pub inverse: Affine,
    pub written_at: f64,
}

/// A press at physical pixel (`px`, `py`) on a gradient handle starts its
/// drag; answers whether it did.
pub(super) fn press(store: AppStore, pane: PaneId, px: f32, py: f32) -> bool {
    let scale = scale_of(pane);
    let writable = with_session(pane, |s| store.node_access(s.store_id, s.node_id).allows_write()).unwrap_or(false);
    if !writable {
        return false;
    }
    let started = with_session(pane, |s| {
        let sh = shown(store, pane, s)?;
        if !s.editable(&sh.id) {
            return None;
        }
        let p = s.view.screen_to_board(px, py);
        let reach = HANDLE_REACH * scale / s.view.zoom().max(1e-3);
        let from = handle_on_board(&sh.world, sh.bbox, sh.gradient.from);
        let to = handle_on_board(&sh.world, sh.bbox, sh.gradient.to);
        let d = |q: Point| (q.x - p.x).hypot(q.y - p.y);
        // The end handle first: on a short gradient it sits over the start.
        let end = if d(to) <= reach {
            1
        } else if d(from) <= reach {
            0
        } else {
            return None;
        };
        let inverse = sh.world.invert()?;
        s.gesture = Gesture::GradientHandle(HandleDrag {
            id: sh.id,
            fill: sh.fill,
            end,
            base: sh.gradient.clone(),
            working: sh.gradient,
            bbox: sh.bbox,
            inverse,
            written_at: 0.0,
        });
        Some(())
    })
    .flatten()
    .is_some();
    if started {
        new_undo_step(pane);
        points::hold_clock(pane, true);
    }
    started
}

/// The handle dragged to physical pixel (`px`, `py`): the overlay follows at
/// once, the document about every 80 ms (always when `last`).
fn drag_to(store: AppStore, pane: PaneId, px: f32, py: f32, last: bool) {
    let write = with_session(pane, |s| {
        let p = s.view.screen_to_board(px, py);
        let Gesture::GradientHandle(d) = &mut s.gesture else { return None };
        let q = handle_in_box(&d.inverse, d.bbox, p);
        let mut g = d.base.clone();
        if d.end == 0 {
            // The start drags the whole gradient along with it.
            let (dx, dy) = (q.x - g.from.x, q.y - g.from.y);
            g.from = q;
            g.to = Point::new(g.to.x + dx, g.to.y + dy);
        } else {
            g.to = q;
        }
        if !g.is_valid() {
            return None;
        }
        d.working = g.clone();
        let due = last || now_ms() - d.written_at >= DRAG_WRITE_EVERY_MS;
        if due {
            d.written_at = now_ms();
        }
        Some(due.then(|| (d.id.clone(), d.fill, g)))
    })
    .flatten();
    match write {
        Some(Some((id, fill, g))) => {
            let paint = Paint::Gradient(g.clone());
            local_edit(store, pane, |t| if fill { t.set_fill_paint(&id, &paint) } else { t.set_stroke_paint(&id, &paint) });
            let st = store.board_style;
            let sig = if fill { st.fill_gradient } else { st.stroke_gradient };
            sig.set_if_changed(Some(g));
        }
        Some(None) => {
            points::refresh_overlay(store, pane);
            present(pane);
        }
        None => {}
    }
}

/// A gradient handle gesture moved; answers whether it was one.
pub(super) fn moved(store: AppStore, pane: PaneId, px: f32, py: f32) -> bool {
    if !with_session(pane, |s| matches!(s.gesture, Gesture::GradientHandle(_))).unwrap_or(false) {
        return false;
    }
    drag_to(store, pane, px, py, false);
    true
}

/// A gradient handle gesture ended (taken out of the session); `Err` hands
/// back a gesture that is not one.
pub(super) fn released(store: AppStore, pane: PaneId, gesture: Gesture, px: f32, py: f32) -> Result<(), Gesture> {
    match gesture {
        Gesture::GradientHandle(d) => {
            with_session(pane, |s| s.gesture = Gesture::GradientHandle(d));
            drag_to(store, pane, px, py, true);
            with_session(pane, |s| s.gesture = Gesture::Idle);
            points::hold_clock(pane, false);
            after_change(store, pane);
            Ok(())
        }
        other => Err(other),
    }
}

/// A gradient handle gesture cancelled: the gradient goes back as it was.
pub(super) fn cancelled(store: AppStore, pane: PaneId, gesture: Gesture) -> Result<(), Gesture> {
    match gesture {
        Gesture::GradientHandle(d) => {
            let paint = Paint::Gradient(d.base.clone());
            local_edit(store, pane, |t| if d.fill { t.set_fill_paint(&d.id, &paint) } else { t.set_stroke_paint(&d.id, &paint) });
            points::hold_clock(pane, false);
            after_change(store, pane);
            Ok(())
        }
        other => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g3() -> Gradient {
        Gradient::linear(
            Point::new(0.0, 0.5),
            Point::new(1.0, 0.5),
            vec![
                GradientStop::new(0.0, Color::rgb(255, 0, 0)),
                GradientStop::new(0.5, Color::rgb(0, 255, 0)),
                GradientStop::new(1.0, Color::rgb(0, 0, 255)),
            ],
        )
    }

    fn offsets(g: &Gradient) -> Vec<f32> {
        g.stops.iter().map(|s| s.offset).collect()
    }

    #[test]
    fn a_press_on_the_bar_is_an_offset() {
        assert_eq!(offset_at(50.0, 200.0), 0.25);
        assert_eq!(offset_at(-10.0, 200.0), 0.0);
        assert_eq!(offset_at(250.0, 200.0), 1.0);
        assert_eq!(offset_at(10.0, 0.0), 0.0);
        assert_eq!(offset_at(f32::NAN, 100.0), 0.0);
    }

    #[test]
    fn a_stop_is_found_near_its_offset() {
        let g = g3();
        assert_eq!(stop_near(&g, 0.52, STOP_REACH), Some(1));
        assert_eq!(stop_near(&g, 0.98, STOP_REACH), Some(2));
        assert_eq!(stop_near(&g, 0.3, STOP_REACH), None);
    }

    #[test]
    fn adding_a_stop_keeps_the_order_and_the_color_there() {
        let (g, i) = add_stop(&g3(), 0.25);
        assert_eq!(i, 1);
        assert_eq!(offsets(&g), vec![0.0, 0.25, 0.5, 1.0]);
        // Halfway from red to green.
        assert_eq!(g.stops[1].color, g3().color_at(0.25));
        let (g, i) = add_stop(&g, 1.0);
        assert_eq!(i, 4, "after a stop at the same offset");
        assert_eq!(offsets(&g), vec![0.0, 0.25, 0.5, 1.0, 1.0]);
        let (_, i) = add_stop(&g3(), -3.0);
        assert_eq!(i, 1, "clamped to 0, after the stop there");
    }

    #[test]
    fn moving_a_stop_past_others_keeps_them_in_order() {
        let red = Color::rgb(255, 0, 0);
        // The first stop dragged past the middle one.
        let (g, i) = move_stop(&g3(), 0, 0.75);
        assert_eq!(offsets(&g), vec![0.5, 0.75, 1.0]);
        assert_eq!(i, 1);
        assert_eq!(g.stops[1].color, red);
        // And back before it.
        let (g, i) = move_stop(&g, 1, 0.1);
        assert_eq!(offsets(&g), vec![0.1, 0.5, 1.0]);
        assert_eq!(i, 0);
        assert_eq!(g.stops[0].color, red);
        // Within its neighbours it stays where it is.
        let (g, i) = move_stop(&g3(), 1, 0.6);
        assert_eq!((offsets(&g), i), (vec![0.0, 0.6, 1.0], 1));
        // Clamped to the bar.
        let (g, i) = move_stop(&g3(), 1, 1.7);
        assert_eq!((offsets(&g), i), (vec![0.0, 1.0, 1.0], 1));
        // Out of range: unchanged.
        assert_eq!(move_stop(&g3(), 9, 0.2).0, g3());
    }

    #[test]
    fn removing_keeps_at_least_two_stops() {
        let g = remove_stop(&g3(), 1).unwrap();
        assert_eq!(offsets(&g), vec![0.0, 1.0]);
        assert!(remove_stop(&g, 0).is_none());
        assert!(remove_stop(&g3(), 7).is_none());
    }

    #[test]
    fn the_switch_keeps_stops_and_starts_from_the_color() {
        let red = Color::rgb(255, 0, 0);
        assert_eq!(switched(Some(&g3()), None, red), None);
        assert_eq!(switched(None, Some(GradientKind::Linear), red), Some(Gradient::default_linear(red)));
        assert_eq!(switched(None, Some(GradientKind::Radial), red), Some(Gradient::default_radial(red)));
        let radial = switched(Some(&g3()), Some(GradientKind::Radial), red).unwrap();
        assert_eq!(radial.kind, GradientKind::Radial);
        assert_eq!(radial.stops, g3().stops);
        assert_eq!((radial.from, radial.to), (Gradient::default_radial(red).from, Gradient::default_radial(red).to));
        assert_eq!(switched(Some(&g3()), Some(GradientKind::Linear), red), Some(g3()));
    }

    #[test]
    fn swatches_show_a_gradient_as_css() {
        assert_eq!(css(&g3()), "linear-gradient(90deg, #ff0000 0.0%, #00ff00 50.0%, #0000ff 100.0%)");
        let down = Gradient { from: Point::new(0.5, 0.0), to: Point::new(0.5, 1.0), ..g3() };
        assert!(css(&down).starts_with("linear-gradient(180deg"));
        let fade = Gradient::default_radial(Color::rgb(0, 0, 255));
        assert_eq!(css(&fade), "radial-gradient(circle, #0000ff 0.0%, rgba(0, 0, 255, 0.000) 100.0%)");
        assert!(bar_css(&down).starts_with("linear-gradient(90deg"));
    }

    #[test]
    fn handles_map_through_the_box_and_back() {
        let world = Affine::from_row(2.0, 0.0, 0.0, 3.0, 10.0, 20.0);
        let bbox = Rect::new(5.0, 5.0, 100.0, 50.0);
        let p = handle_on_board(&world, bbox, Point::new(1.0, 0.5));
        assert_eq!(p, Point::new(10.0 + 2.0 * 105.0, 20.0 + 3.0 * 30.0));
        let back = handle_in_box(&world.invert().unwrap(), bbox, p);
        assert!((back.x - 1.0).abs() < 1e-4 && (back.y - 0.5).abs() < 1e-4);
    }
}
