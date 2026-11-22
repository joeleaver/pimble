//! Measuring on a canvas (docs/MEASURE_CONTRACT.md), the app's side: the
//! canvas's scale and the Scale panel (presets, typed, from a selected
//! element, calibrated by drawing, a picture sized to scale), the Measure
//! tool's ends snapping to element points, live readouts while drawing and
//! editing, sizes typed while drawing, and the selection's measures in its
//! toolbar group. The units, parsing and formatting are
//! `pimble_board::measure`'s; nothing here stores a measurement.
//!
//! Desktop and browser alike: this is shared `pimble-app` code.

use pimble_board::measure::{self as m, Measures, Scale, Unit, PRESETS};
use pimble_board::EditOverlay;

use super::*;
use crate::appearance::IconGlyph;

/// Which way the Scale panel sets the scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Way {
    #[default]
    Preset,
    Typed,
    /// "This is ___ long", from one selected element.
    Element,
    /// A two-point line drawn on the canvas.
    Calibrate,
    /// A selected picture sized to the canvas's scale.
    Picture,
}

impl Way {
    pub const ALL: [Way; 5] = [Way::Preset, Way::Typed, Way::Element, Way::Calibrate, Way::Picture];

    pub fn label(self) -> &'static str {
        match self {
            Way::Preset => "Preset",
            Way::Typed => "Typed",
            Way::Element => "From Selection",
            Way::Calibrate => "Calibrate",
            Way::Picture => "Picture",
        }
    }

    pub fn tip(self) -> &'static str {
        match self {
            Way::Preset => "A scale everyone uses: architectural, engineering, metric, map",
            Way::Typed => "Type a ratio (1:100) or paper = world (1 in = 10 ft)",
            Way::Element => "Say how long the selected line, or how wide the selected box, really is",
            Way::Calibrate => "Draw a line over something whose length you know, then type that length",
            Way::Picture => "Resize the selected scanned picture so its scale matches the canvas's",
        }
    }
}

/// What one selected element offers the Scale panel.
#[derive(Debug, Clone, PartialEq)]
pub struct PickedEl {
    pub id: String,
    pub kind: Kind,
    /// A line's length, board units.
    pub length: Option<f32>,
    /// A box's width and height, board units.
    pub width: Option<f32>,
    pub height: Option<f32>,
    /// A picture: its pixel size as shown (its crop's when cropped).
    pub pixels: Option<(f32, f32)>,
}

/// The selection's measures as its toolbar group shows them.
#[derive(Debug, Clone, PartialEq)]
pub struct SelMeasures {
    /// Exactly one element: its sizes can be typed.
    pub one: Option<String>,
    pub measures: Measures,
    /// Each measure as a person reads it.
    pub length: Option<String>,
    pub angle: Option<String>,
    pub width: Option<String>,
    pub height: Option<String>,
    pub perimeter: Option<String>,
    pub area: Option<String>,
    /// The kinds selected can show dimensions, an area.
    pub can_dims: bool,
    pub can_area: bool,
    /// Every one that can has them on.
    pub dims: bool,
    pub area_on: bool,
    /// Its length can be typed (a line, an arrow, a dimension); its box can.
    pub length_editable: bool,
    pub box_editable: bool,
    /// One dimension selected: how far its line sits from its points.
    pub offset: Option<String>,
}

/// A pane's measuring state, as signals.
#[derive(Clone, Copy)]
pub struct MeasureUi {
    /// The canvas's scale.
    pub scale: Signal<Option<Scale>>,
    /// The Scale panel is open.
    pub open: Signal<bool>,
    pub way: Signal<Way>,
    pub unit: Signal<Unit>,
    /// `None`: the unit's own.
    pub precision: Signal<Option<u8>>,
    /// The preset chosen, by index into `PRESETS`.
    pub preset: Signal<usize>,
    pub typed: Signal<String>,
    /// The real length typed ("This is ___ long", calibration).
    pub length: Signal<String>,
    /// From a box: its height rather than its width.
    pub use_height: Signal<bool>,
    /// Picture at scale: the scan's dots per inch and the scale it was
    /// drawn at.
    pub dpi: Signal<String>,
    pub drawn_at: Signal<String>,
    /// The calibration line's length, board units.
    pub cal_length: Signal<Option<f32>>,
    /// The one selected element, when it can set a scale.
    pub picked: Signal<Option<PickedEl>>,
    /// What the panel last said went wrong or was done.
    pub said: Signal<Option<String>>,
    /// The live readout near the pointer (logical pixels).
    pub readout: Signal<Option<(f32, f32, String)>>,
    pub selection: Signal<Option<SelMeasures>>,
    /// The sizes the selection's group offers to type, as typed.
    pub length_text: Signal<String>,
    pub width_text: Signal<String>,
    pub height_text: Signal<String>,
    pub offset_text: Signal<String>,
    /// The grid's step as the status bar says it, while the grid shows.
    pub grid_label: Signal<Option<String>>,
}

impl MeasureUi {
    pub fn new() -> Self {
        Self {
            scale: Signal::new(None),
            open: Signal::new(false),
            way: Signal::new(Way::Preset),
            unit: Signal::new(Unit::Ft),
            precision: Signal::new(None),
            preset: Signal::new(2),
            typed: Signal::new(String::new()),
            length: Signal::new(String::new()),
            use_height: Signal::new(false),
            dpi: Signal::new("300".into()),
            drawn_at: Signal::new(String::new()),
            cal_length: Signal::new(None),
            picked: Signal::new(None),
            said: Signal::new(None),
            readout: Signal::new(None),
            selection: Signal::new(None),
            length_text: Signal::new(String::new()),
            width_text: Signal::new(String::new()),
            height_text: Signal::new(String::new()),
            offset_text: Signal::new(String::new()),
            grid_label: Signal::new(None),
        }
    }
}

impl Default for MeasureUi {
    fn default() -> Self {
        Self::new()
    }
}

/// Refusal: the Measure tool used with no scale.
pub const NEEDS_SCALE: &str = "Set a scale for this canvas first: dimensions measure in its units.";
/// Refusal: Size to Scale with no scale on the canvas.
pub const PICTURE_NEEDS_SCALE: &str = "Set the canvas's scale first; the picture is sized to match it.";

/// `scale` written in `unit` (the same board units per meter).
pub fn in_unit(scale: &Scale, unit: Unit) -> Scale {
    if scale.unit == unit {
        return scale.clone();
    }
    Scale { unit, per_unit: scale.board_per_meter() * unit.meters(), label: scale.label.clone(), precision: None, off: scale.off }
}

/// What the Scale panel would set, from its fields (the one judgement the
/// panel and its tests read): a scale, or the sentence saying why not.
#[allow(clippy::too_many_arguments)]
pub fn candidate(
    way: Way,
    preset: usize,
    typed: &str,
    length: &str,
    use_height: bool,
    picked: Option<&PickedEl>,
    cal_length: Option<f32>,
    unit: Unit,
    precision: Option<u8>,
) -> Result<Scale, String> {
    let from_length = |board: Option<f32>, what: &str| -> Result<Scale, String> {
        let board = board.filter(|b| *b > 0.0).ok_or_else(|| what.to_string())?;
        if length.trim().is_empty() {
            return Err("Type how long it really is.".into());
        }
        let meters = m::parse_length(length, unit).ok_or_else(|| format!("\"{}\" is not a length Pimble can read (12' 6\", 3.5 m, 150).", length.trim()))?;
        m::scale_from_length(board as f64, meters, unit).ok_or_else(|| "A length must be more than zero.".to_string())
    };
    let scale = match way {
        Way::Preset => in_unit(&PRESETS.get(preset).ok_or("Pick a preset.")?.scale(), unit).with_label(PRESETS[preset].text),
        Way::Typed => {
            if typed.trim().is_empty() {
                return Err("Type a scale: 1:100, 1 in = 10 ft, 1/4\" = 1'.".into());
            }
            let s = m::parse_scale(typed, unit)?;
            in_unit(&s, unit).with_label(typed)
        }
        Way::Element => {
            let p = picked.ok_or("Select one line, arrow, dimension, box or picture first.")?;
            let board = match (p.length, p.width, p.height) {
                (Some(l), _, _) => Some(l),
                (None, Some(_), Some(h)) if use_height => Some(h),
                (None, Some(w), _) => Some(w),
                _ => None,
            };
            from_length(board, "That element has no length to measure.")?
        }
        Way::Calibrate => from_length(cal_length, "Draw a line on the canvas over something you know the length of.")?,
        Way::Picture => return Err("Size to Scale resizes the picture; it does not change the canvas's scale.".into()),
    };
    Ok(scale.with_precision(precision))
}

/// The panel's preview line, or why it cannot be applied.
fn preview(ui: MeasureUi) -> Result<Scale, String> {
    let picked = ui.picked.get();
    candidate(
        ui.way.get(),
        ui.preset.get(),
        &ui.typed.get(),
        &ui.length.get(),
        ui.use_height.get(),
        picked.as_ref(),
        ui.cal_length.get(),
        ui.unit.get(),
        ui.precision.get(),
    )
}

// ── Opening, applying ───────────────────────────────────────────────────────

/// Open the Scale panel over pane `pane`'s canvas, showing the scale it has.
pub(crate) fn open_scale(store: AppStore, pane: PaneId) {
    let ui = store.pane(pane).board_ui.measure;
    let current = with_session(pane, |s| s.snapshot().scale.clone()).flatten();
    if let Some(s) = &current {
        ui.unit.set(s.unit);
        ui.precision.set(s.precision);
        if let Some(label) = &s.label {
            ui.typed.set(label.clone());
            if let Some(i) = PRESETS.iter().position(|p| p.text == label) {
                ui.preset.set(i);
                ui.way.set(Way::Preset);
            } else {
                ui.way.set(Way::Typed);
            }
        }
    }
    // A picture selected on a canvas with a scale: most likely it is to be
    // sized to it.
    if current.is_some() && untracked(|| ui.picked.get()).is_some_and(|p| p.pixels.is_some()) {
        ui.way.set(Way::Picture);
    }
    ui.said.set(None);
    ui.open.set(true);
    way_changed(store, pane);
}

/// Close the panel (its calibration line goes).
pub(crate) fn close_scale(store: AppStore, pane: PaneId) {
    let ui = store.pane(pane).board_ui.measure;
    ui.open.set(false);
    with_session(pane, |s| s.calibration = None);
    ui.cal_length.set(None);
    after_change(store, pane);
}

/// The panel's way changed: calibrating shows its line (and takes presses on
/// the canvas), anything else lets go of it.
pub(super) fn way_changed(store: AppStore, pane: PaneId) {
    let ui = store.pane(pane).board_ui.measure;
    let calibrating = untracked(|| ui.open.get() && ui.way.get() == Way::Calibrate);
    with_session(pane, |s| {
        if calibrating {
            s.calibration.get_or_insert_with(Calibration::default);
        } else {
            s.calibration = None;
        }
    });
    if !calibrating {
        ui.cal_length.set(None);
    }
    after_change(store, pane);
}

/// Set (or with `None` remove) pane `pane`'s canvas scale: one undoable edit.
pub(crate) fn set_scale(store: AppStore, pane: PaneId, scale: Option<&Scale>) -> bool {
    new_undo_step(pane);
    let done = local_edit(store, pane, |t| t.set_scale(scale)).is_some();
    new_undo_step(pane);
    after_change(store, pane);
    done
}

/// Switch measuring on or off on pane `pane`'s canvas (Joe, 2026-11-22:
/// "artists won't want it"). Off keeps the scale and hides everything that
/// measures; on brings it back, or opens the Scale panel when the canvas
/// has never had one. One undoable edit.
pub(crate) fn set_measuring(store: AppStore, pane: PaneId, on: bool) {
    let stored = with_session(pane, |s| s.snapshot().scale.clone()).flatten();
    match stored {
        Some(mut scale) if scale.off == on => {
            scale.off = !on;
            set_scale(store, pane, Some(&scale));
            if !on {
                close_scale(store, pane);
            }
        }
        Some(_) => {}
        None if on => open_scale(store, pane),
        None => close_scale(store, pane),
    }
}

/// Apply what the panel says (a scale applied measures: it is on).
fn apply(store: AppStore, pane: PaneId) {
    let ui = store.pane(pane).board_ui.measure;
    match untracked(|| preview(ui)) {
        Ok(mut scale) => {
            scale.off = false;
            if set_scale(store, pane, Some(&scale)) {
                close_scale(store, pane);
            }
        }
        Err(why) => ui.said.set(Some(why)),
    }
}

/// Size to Scale: the selected picture resized so a scan at the panel's
/// dpi, drawn at the panel's "drawn at" scale, measures true on this
/// canvas. One edit, undoable.
fn size_picture(store: AppStore, pane: PaneId) {
    let ui = store.pane(pane).board_ui.measure;
    let Some(canvas) = untracked(|| ui.scale.get()) else {
        ui.said.set(Some(PICTURE_NEEDS_SCALE.into()));
        return;
    };
    let (dpi, drawn_at) = untracked(|| (ui.dpi.get(), ui.drawn_at.get()));
    let Some(dpi) = dpi.trim().parse::<f64>().ok().filter(|d| *d > 0.0) else {
        ui.said.set(Some("Type the scan's dots per inch (300, for example).".into()));
        return;
    };
    let drawn = match m::parse_scale(&drawn_at, canvas.unit) {
        Ok(s) => s,
        Err(why) => {
            ui.said.set(Some(format!("The scale it was drawn at: {why}")));
            return;
        }
    };
    let Some(k) = m::picture_units_per_pixel(dpi, &drawn, &canvas) else { return };
    let picked = untracked(|| ui.picked.get());
    let Some(PickedEl { id, pixels: Some((pw, ph)), .. }) = picked else {
        ui.said.set(Some("Select one picture first.".into()));
        return;
    };
    let write = with_session(pane, |s| {
        let e = s.snapshot().get(&id)?;
        let Geom::Picture(p) = &e.raw.geom else { return None };
        let t = e.raw.transform;
        let (sx, sy) = (t.sx.abs().max(1e-6), t.sy.abs().max(1e-6));
        let r = p.rect.normalized();
        let to = Rect::new(r.x, r.y, (pw as f64 * k) as f32 / sx, (ph as f64 * k) as f32 / sy);
        let geom = Geom::Picture(PictureGeom { rect: to, ..p.clone() });
        let transform = pimble_board::repivot(&t, r, to);
        Some((geom, transform))
    })
    .flatten();
    let Some((geom, transform)) = write else { return };
    new_undo_step(pane);
    local_edit(store, pane, |t| {
        t.set_geom(&id, &geom)?;
        t.set_transform(&id, &transform)
    });
    new_undo_step(pane);
    ui.said.set(Some("The picture now measures true at the canvas's scale.".into()));
    after_change(store, pane);
}

// ── Measures of the selection, readouts ──────────────────────────────────

/// What `e` offers as one picked element (the Scale panel's "from
/// selection" and "picture" ways).
fn picked_of(e: &pimble_board::Element) -> Option<PickedEl> {
    let ms = m::of(e);
    let pixels = match &e.raw.geom {
        Geom::Picture(p) => Some(match p.crop {
            Some(c) => (c.w.abs(), c.h.abs()),
            None => (p.px_w as f32, p.px_h as f32),
        }),
        _ => None,
    };
    let length = matches!(e.raw.kind, Kind::Line | Kind::Arrow | Kind::Dimension | Kind::Connector).then_some(ms.length).flatten();
    (length.is_some() || ms.width.is_some()).then(|| PickedEl { id: e.raw.id.clone(), kind: e.raw.kind.clone(), length, width: ms.width, height: ms.height, pixels })
}

/// Whether a kind can label its sides, show its area.
pub fn takes_dims(kind: &Kind) -> bool {
    matches!(
        kind,
        Kind::Line | Kind::Arrow | Kind::Connector | Kind::Pen | Kind::Highlighter | Kind::Path | Kind::Rect | Kind::Diamond | Kind::Polygon | Kind::Star | Kind::Frame | Kind::Picture | Kind::Sticky | Kind::Card | Kind::Callout
    )
}

pub fn takes_area(kind: &Kind) -> bool {
    matches!(kind, Kind::Rect | Kind::Ellipse | Kind::Diamond | Kind::Polygon | Kind::Star | Kind::Path | Kind::Frame | Kind::Picture | Kind::Sticky | Kind::Card | Kind::Callout)
}

fn sel_measures(s: &Session, scale: &Scale) -> Option<SelMeasures> {
    let snapshot = s.snapshot();
    let els: Vec<&pimble_board::Element> = s.selected.iter().filter_map(|id| snapshot.get(id)).collect();
    if els.is_empty() {
        return None;
    }
    let first = els[0];
    let ms = m::of(first);
    let len = |v: Option<f32>| v.map(|v| scale.length(v as f64));
    let dims_kinds: Vec<&&pimble_board::Element> = els.iter().filter(|e| takes_dims(&e.raw.kind)).collect();
    let area_kinds: Vec<&&pimble_board::Element> = els.iter().filter(|e| takes_area(&e.raw.kind)).collect();
    let one = (els.len() == 1).then(|| first.raw.id.clone());
    let editable = one.is_some() && s.editable(&first.raw.id);
    Some(SelMeasures {
        one: one.clone(),
        measures: ms,
        length: len(ms.length),
        angle: ms.angle.map(|a| format!("{}°", (a * 10.0).round() / 10.0)),
        width: len(ms.width),
        height: len(ms.height),
        perimeter: len(ms.perimeter),
        area: ms.area.map(|a| scale.area(a as f64)),
        can_dims: !dims_kinds.is_empty(),
        can_area: !area_kinds.is_empty(),
        dims: !dims_kinds.is_empty() && dims_kinds.iter().all(|e| e.style.dims),
        area_on: !area_kinds.is_empty() && area_kinds.iter().all(|e| e.style.area),
        length_editable: editable && matches!((&first.raw.kind, &first.raw.geom), (Kind::Line | Kind::Arrow | Kind::Dimension, Geom::Segment { .. })),
        box_editable: editable && ms.width.is_some() && matches!(first.raw.geom, Geom::Rect(_) | Geom::Picture(_) | Geom::Card { .. } | Geom::Path(_)),
        offset: (editable && first.raw.kind == Kind::Dimension).then(|| scale.length(first.style.dim_offset.abs() as f64)),
    })
}

/// A readout of `e`'s measures that change as it is drawn or edited
/// (`14′ 6″  32°`, `12′ × 10′`).
pub fn readout_of(e: &pimble_board::Element, scale: &Scale) -> Option<String> {
    let ms = m::of(e);
    let len = |v: f32| scale.length(v as f64);
    match (ms.length, ms.angle, ms.width, ms.height) {
        (Some(l), Some(a), _, _) => Some(format!("{}  {}°", len(l), a.round())),
        (Some(l), None, _, _) => Some(len(l)),
        (None, _, Some(w), Some(h)) => Some(format!("{} × {}", len(w), len(h))),
        _ => None,
    }
}

/// After every change to pane `pane`: the scale and the selection's
/// measures for the toolbar, the readout near the pointer, the grid's step
/// for the status bar, what the Scale panel can take from the selection.
pub(super) fn publish(store: AppStore, pane: PaneId) {
    let scale_px = scale_of(pane);
    let ui = store.pane(pane).board_ui.measure;
    let grid_on = untracked(|| store.board_style.grid.get());
    let out = with_session(pane, |s| {
        // What was just drawn stays sizeable only while it alone is
        // selected.
        if s.last_drawn.as_ref().is_some_and(|d| s.selected != [d.id.clone()]) {
            s.last_drawn = None;
        }
        if s.typed.as_ref().is_some_and(|t| s.snapshot().get(&t.id).is_none()) {
            s.typed = None;
        }
        let scale = s.snapshot().measuring().cloned();
        let picked = match s.selected.as_slice() {
            [id] => s.snapshot().get(id).and_then(picked_of),
            _ => None,
        };
        let sel = scale.as_ref().and_then(|sc| sel_measures(s, sc));
        let (px, py) = (s.pointer.0 / scale_px + 16.0, s.pointer.1 / scale_px + 18.0);
        // The readout: a size being typed, else the element being drawn
        // or edited.
        let readout = if let Some(t) = &s.typed {
            let what = if line_like(&t.kind) { "Length" } else { "Size (w x h)" };
            Some((px, py, format!("{what}: {}▏  Enter applies, Escape leaves it", t.text)))
        } else if let Some(scale) = &scale {
            let id = match &s.gesture {
                Gesture::Shape { id: Some(id), .. } | Gesture::Connector { id: Some(id), .. } => Some(id.clone()),
                Gesture::Resize { bases, .. } | Gesture::Rotate { bases, .. } | Gesture::Move { bases, .. } if bases.len() == 1 => Some(bases[0].id.clone()),
                Gesture::Points(_) => s.points.as_ref().map(|p| p.id.clone()),
                _ => None,
            };
            id.and_then(|id| s.snapshot().get(&id).and_then(|e| readout_of(e, scale))).map(|text| (px, py, text))
        } else {
            None
        };
        let cal = s.calibration.as_ref().and_then(|c| c.length());
        let grid = (grid_on && s.view.grid().is_some()).then(|| scale.as_ref().map(|sc| format!("grid {}", m::grid_label(sc, super::grid_spacing(s))))).flatten();
        (scale, picked, sel, readout, cal, grid)
    });
    let Some((scale, picked, sel, readout, cal, grid)) = out else { return };
    let texts = sel.as_ref().map(|s| {
        (s.length.clone().unwrap_or_default(), s.width.clone().unwrap_or_default(), s.height.clone().unwrap_or_default(), s.offset.clone().unwrap_or_default())
    });
    if untracked(|| ui.selection.get()) != sel {
        if let Some((l, w, h, o)) = texts {
            ui.length_text.set_if_changed(l);
            ui.width_text.set_if_changed(w);
            ui.height_text.set_if_changed(h);
            ui.offset_text.set_if_changed(o);
        }
    }
    ui.scale.set_if_changed(scale);
    ui.picked.set_if_changed(picked);
    ui.selection.set_if_changed(sel);
    ui.readout.set_if_changed(readout);
    ui.cal_length.set_if_changed(cal);
    ui.grid_label.set_if_changed(grid);
}

fn line_like(kind: &Kind) -> bool {
    matches!(kind, Kind::Line | Kind::Arrow | Kind::Dimension)
}

fn box_like(kind: &Kind) -> bool {
    matches!(kind, Kind::Rect | Kind::Ellipse | Kind::Diamond | Kind::Polygon | Kind::Star | Kind::Frame)
}

// ── Editing the selection's sizes ───────────────────────────────────────────

/// Which size of the selection is typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    Length,
    Width,
    Height,
    /// A dimension's offset from its points (its side kept).
    Offset,
}

/// The one selected element's `size` set to what `text` reads as (in the
/// canvas's units): a line from its start, a box from its top-left.
pub(super) fn set_size(store: AppStore, pane: PaneId, size: Size, text: &str) {
    let ui = store.pane(pane).board_ui.measure;
    let Some(scale) = untracked(|| ui.scale.get()) else { return };
    let Some(target) = scale.parse_board(text).filter(|v| *v > 0.0) else {
        crate::events::show_notice(store, format!("\"{}\" is not a length Pimble can read (12' 6\", 3.5 m, 150).", text.trim()));
        return;
    };
    let target = target as f32;
    if size == Size::Offset {
        let write = with_session(pane, |s| {
            let [id] = s.selected.as_slice() else { return None };
            let e = s.snapshot().get(id).filter(|e| e.raw.kind == Kind::Dimension)?;
            let side = if e.style.dim_offset < 0.0 { -1.0 } else { 1.0 };
            Some((id.clone(), target * side))
        })
        .flatten();
        if let Some((id, offset)) = write {
            new_undo_step(pane);
            local_edit(store, pane, |t| t.set_style(&id, &Style { dim_offset: Some(offset), ..Style::default() }));
            new_undo_step(pane);
            after_change(store, pane);
        }
        return;
    }
    let write = with_session(pane, |s| {
        let [id] = s.selected.as_slice() else { return None };
        let e = s.snapshot().get(id)?;
        let ms = m::of(e);
        let t = e.raw.transform;
        match (size, &e.raw.geom) {
            (Size::Length, Geom::Segment { a, b }) => {
                let now = ms.length.filter(|l| *l > 1e-6)?;
                let k = target / now;
                let b2 = Point::new(a.x + (b.x - a.x) * k, a.y + (b.y - a.y) * k);
                let (old, new) = (Rect::from_points(*a, *b), Rect::from_points(*a, b2));
                Some((id.clone(), Geom::Segment { a: *a, b: b2 }, pimble_board::repivot(&t, old, new)))
            }
            (Size::Offset, _) => None,
            (Size::Width | Size::Height, geom) => {
                let now = if size == Size::Width { ms.width } else { ms.height }.filter(|v| *v > 1e-6)?;
                let k = target / now;
                let (kx, ky) = if size == Size::Width { (k, 1.0) } else { (1.0, k) };
                let old = e.pivot_bounds;
                let new = Rect::new(old.x, old.y, old.w * kx, old.h * ky);
                let map = |p: Point| Point::new(old.x + (p.x - old.x) * kx, old.y + (p.y - old.y) * ky);
                let geom = match geom {
                    Geom::Path(p) => Geom::Path(p.rescaled(&old, &new)),
                    g => g.mapped(map)?,
                };
                Some((id.clone(), geom, pimble_board::repivot(&t, old, new)))
            }
            _ => None,
        }
    })
    .flatten();
    let Some((id, geom, transform)) = write else { return };
    new_undo_step(pane);
    local_edit(store, pane, |t| {
        t.set_geom(&id, &geom)?;
        if !transform.is_identity() {
            t.set_transform(&id, &transform)?;
        }
        Ok(())
    });
    new_undo_step(pane);
    after_change(store, pane);
}

/// "Show dimensions" or "Show area" on every selected element that takes it.
pub(super) fn toggle_labels(store: AppStore, pane: PaneId, area: bool) {
    let Some(sel) = untracked(|| store.pane(pane).board_ui.measure.selection.get()) else { return };
    let on = if area { !sel.area_on } else { !sel.dims };
    let ids: Vec<String> = with_session(pane, |s| {
        s.selected
            .iter()
            .filter(|id| s.editable(id))
            .filter(|id| s.snapshot().get(id).is_some_and(|e| if area { takes_area(&e.raw.kind) } else { takes_dims(&e.raw.kind) }))
            .cloned()
            .collect()
    })
    .unwrap_or_default();
    if ids.is_empty() {
        return;
    }
    new_undo_step(pane);
    local_edit(store, pane, |t| {
        for id in &ids {
            if on {
                let style = if area { Style { area: Some(true), ..Style::default() } } else { Style { dims: Some(true), ..Style::default() } };
                t.set_style(id, &style)?;
            } else {
                t.clear_style(id, if area { pimble_board::StyleKey::Area } else { pimble_board::StyleKey::Dims })?;
            }
        }
        Ok(())
    });
    new_undo_step(pane);
    after_change(store, pane);
}

// ── Snapping the Measure tool's and the calibration's ends ──────────────────

/// How near (CSS pixels) a point must be to snap to an element's point.
const SNAP_PX: f32 = 8.0;

/// The points an element offers to snap to, board units: line ends, box
/// corners and edge midpoints, path anchors, a stroke's ends.
fn points_of(e: &pimble_board::Element) -> Vec<Point> {
    let w = e.world;
    let map = |p: Point| {
        let mut q = [pimble_board::tiny_skia::Point::from_xy(p.x, p.y)];
        w.map_points(&mut q);
        Point::new(q[0].x, q[0].y)
    };
    let boxy = |r: Rect| {
        let r = r.normalized();
        let (x0, y0, x1, y1, cx, cy) = (r.x, r.y, r.right(), r.bottom(), r.center().x, r.center().y);
        [(x0, y0), (cx, y0), (x1, y0), (x1, cy), (x1, y1), (cx, y1), (x0, y1), (x0, cy)].map(|(x, y)| map(Point::new(x, y))).to_vec()
    };
    match &e.raw.geom {
        Geom::Segment { a, b } => vec![map(*a), map(*b)],
        Geom::Rect(r) => boxy(*r),
        Geom::Picture(p) => boxy(p.rect),
        Geom::Card { rect, .. } | Geom::Callout { rect, .. } => boxy(*rect),
        Geom::Path(p) => p.subpaths.iter().flat_map(|sp| sp.anchors.iter().map(|a| map(a.p))).collect(),
        Geom::Anchor(p) => vec![map(*p)],
        Geom::Connector { .. } => e.route.map(|(a, b)| vec![a, b]).unwrap_or_default(),
        Geom::Stroke => {
            let pts: Vec<&[f32; 3]> = e.raw.points.iter().filter(|p| p[0].is_finite() && p[1].is_finite()).collect();
            match (pts.first(), pts.last()) {
                (Some(a), Some(b)) => vec![map(Point::new(a[0], a[1])), map(Point::new(b[0], b[1]))],
                _ => Vec::new(),
            }
        }
        Geom::None | Geom::Scatter => Vec::new(),
    }
}

/// `p` snapped to the nearest element point within reach (`except` left
/// out), else to the grid when snapping is on (`grid`), else as it is.
pub(super) fn snap_end(view: &BoardView, p: Point, grid: Option<f32>, css_scale: f32, except: Option<&str>) -> Point {
    let reach = SNAP_PX * css_scale / view.zoom().max(1e-6);
    let snapshot = view.snapshot();
    let near = Rect::new(p.x - reach, p.y - reach, 2.0 * reach, 2.0 * reach);
    let mut best: Option<(f32, Point)> = None;
    for i in snapshot.query(&near) {
        let e = &snapshot.elements[i];
        if !e.visible || Some(e.raw.id.as_str()) == except {
            continue;
        }
        for q in points_of(e) {
            let d = (q.x - p.x).hypot(q.y - p.y);
            if d <= reach && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, q));
            }
        }
    }
    match (best, grid.filter(|g| *g > 0.0)) {
        (Some((_, q)), _) => q,
        (None, Some(g)) => Point::new((p.x / g).round() * g, (p.y / g).round() * g),
        (None, None) => p,
    }
}

// ── Calibrating by drawing ──────────────────────────────────────────────────

/// The calibration line: its two ends once drawn, board units.
#[derive(Debug, Clone, Default)]
pub(crate) struct Calibration {
    pub ends: Option<[Point; 2]>,
}

impl Calibration {
    pub fn length(&self) -> Option<f32> {
        self.ends.map(|[a, b]| (b.x - a.x).hypot(b.y - a.y)).filter(|l| *l > 0.0)
    }
}

/// A press while calibrating: an end near it is taken up, elsewhere a new
/// line starts. Answers whether it took the press.
pub(super) fn calibrate_press(store: AppStore, pane: PaneId, px: f32, py: f32) -> bool {
    let grid = super::tools::snap_grid(store, pane);
    let css = scale_of(pane);
    let took = with_session(pane, |s| {
        let cal = s.calibration.as_ref()?;
        let p = s.view.screen_to_board(px, py);
        let reach = 10.0 * css / s.view.zoom().max(1e-6);
        let near = cal.ends.and_then(|ends| (0..2).find(|i| (ends[*i].x - p.x).hypot(ends[*i].y - p.y) <= reach));
        match near {
            Some(i) => s.gesture = Gesture::Calibrate(i),
            None => {
                let a = snap_end(&s.view, p, grid, css, None);
                s.calibration = Some(Calibration { ends: Some([a, a]) });
                s.gesture = Gesture::Calibrate(1);
            }
        }
        Some(())
    })
    .flatten()
    .is_some();
    if took {
        after_change(store, pane);
    }
    took
}

/// The pointer moved while an end is held.
pub(super) fn calibrate_moved(store: AppStore, pane: PaneId, px: f32, py: f32) -> bool {
    let grid = super::tools::snap_grid(store, pane);
    let css = scale_of(pane);
    let moved = with_session(pane, |s| {
        let Gesture::Calibrate(i) = s.gesture else { return false };
        let p = s.view.screen_to_board(px, py);
        let q = snap_end(&s.view, p, grid, css, None);
        if let Some(ends) = s.calibration.as_mut().and_then(|c| c.ends.as_mut()) {
            ends[i] = q;
        }
        true
    })
    .unwrap_or(false);
    if moved {
        after_change(store, pane);
    }
    moved
}

/// The overlay the calibration line is drawn as: the line and its two ends.
pub(super) fn overlay(s: &Session) -> Option<EditOverlay> {
    let [a, b] = s.calibration.as_ref()?.ends?;
    let mut o = EditOverlay::default();
    o.curves = vec![[a, a, b, b]];
    o.anchors = vec![(a, true), (b, true)];
    Some(o)
}

// ── Typed sizes ─────────────────────────────────────────────────────────────

/// A line or box this pane just drew (`start` where the press was, `end`
/// where it was let go).
#[derive(Debug, Clone)]
pub(crate) struct Drawn {
    pub id: String,
    pub kind: Kind,
    pub start: Point,
    pub end: Point,
}

/// A size being typed for what was just drawn.
#[derive(Debug, Clone)]
pub(crate) struct Typed {
    pub id: String,
    pub kind: Kind,
    pub start: Point,
    pub end: Point,
    pub text: String,
}

/// The geometry a typed size gives what was drawn from `start` toward
/// `end`: a line's length (and `@ angle`, drafting degrees) along the drawn
/// direction, a box's `w x h` (one number: a square) away from the start
/// the way it was drawn. `read` turns a length typed into board units.
pub fn typed_geom(kind: &Kind, start: Point, end: Point, text: &str, read: impl Fn(&str) -> Option<f64>) -> Option<Geom> {
    if line_like(kind) {
        let (len_text, angle) = match text.split_once('@') {
            Some((l, a)) => (l, Some(a.trim().trim_end_matches('°').parse::<f32>().ok()?)),
            None => (text, None),
        };
        let len = read(len_text.trim()).filter(|v| *v > 0.0)? as f32;
        let (dx, dy) = match angle {
            Some(a) => (a.to_radians().cos(), -a.to_radians().sin()),
            None => {
                let (dx, dy) = (end.x - start.x, end.y - start.y);
                let l = dx.hypot(dy);
                if l > 1e-6 {
                    (dx / l, dy / l)
                } else {
                    (1.0, 0.0)
                }
            }
        };
        return Some(Geom::Segment { a: start, b: Point::new(start.x + dx * len, start.y + dy * len) });
    }
    if box_like(kind) {
        let parts: Vec<&str> = text.split(['x', 'X', '×', '*', ',']).map(str::trim).filter(|p| !p.is_empty()).collect();
        let (w, h) = match parts.as_slice() {
            [one] => {
                let v = read(one).filter(|v| *v > 0.0)? as f32;
                (v, v)
            }
            [w, h] => (read(w).filter(|v| *v > 0.0)? as f32, read(h).filter(|v| *v > 0.0)? as f32),
            _ => return None,
        };
        let sx = if end.x < start.x { -1.0 } else { 1.0 };
        let sy = if end.y < start.y { -1.0 } else { 1.0 };
        return Some(Geom::Rect(Rect::from_points(start, Point::new(start.x + sx * w, start.y + sy * h))));
    }
    None
}

/// A character a typed size may hold.
fn typeable(c: char) -> bool {
    c.is_ascii_alphanumeric() || " .,/'\"@-x×*°′″".contains(c)
}

/// Whether a key is a typed size's (claimed from the rest of the app).
pub(super) fn claims_key(pane: PaneId, key: &SurfaceKeyData) -> bool {
    if key.ctrl || key.meta || key.alt {
        return false;
    }
    with_session(pane, |s| {
        if s.typed.is_some() {
            return matches!(key.key.as_str(), "Enter" | "Escape" | "Backspace") || key.key.chars().count() == 1;
        }
        let starts = key.key.len() == 1 && key.key.chars().all(|c| c.is_ascii_digit() || c == '.');
        starts && can_type(s)
    })
    .unwrap_or(false)
}

/// What a typed size would size: the element being drawn now, or the one
/// just drawn while it alone is selected.
fn can_type(s: &Session) -> bool {
    match &s.gesture {
        Gesture::Shape { kind, id: Some(_), .. } => line_like(kind) || box_like(kind),
        Gesture::Idle => s.last_drawn.as_ref().is_some_and(|d| s.selected == [d.id.clone()]),
        _ => false,
    }
}

/// A key for a typed size: digits start one, characters add to it, Enter
/// applies it, Escape leaves the drawn size. Answers whether it took the
/// key.
pub(super) fn key_down(store: AppStore, pane: PaneId, key: &SurfaceKeyData) -> bool {
    if !claims_key(pane, key) {
        return false;
    }
    let ui = store.pane(pane).board_ui.measure;
    let mut applied: Option<(String, Kind, Point, Point, String)> = None;
    let mut started_from_gesture: Option<(String, Kind, Point, Point)> = None;
    with_session(pane, |s| {
        if s.typed.is_none() {
            // Typing starts: what is being drawn stops following the pointer
            // where it is now.
            let from = match std::mem::replace(&mut s.gesture, Gesture::Idle) {
                Gesture::Shape { kind, start, last, id: Some(id), .. } => {
                    started_from_gesture = Some((id.clone(), kind.clone(), start, last));
                    Some((id, kind, start, last))
                }
                other => {
                    s.gesture = other;
                    s.last_drawn.as_ref().map(|d| (d.id.clone(), d.kind.clone(), d.start, d.end))
                }
            };
            let Some((id, kind, start, end)) = from else { return };
            s.typed = Some(Typed { id, kind, start, end, text: String::new() });
        }
        let Some(t) = s.typed.as_mut() else { return };
        match key.key.as_str() {
            "Escape" => s.typed = None,
            "Backspace" => {
                t.text.pop();
            }
            "Enter" => {
                let t = s.typed.take().unwrap();
                applied = Some((t.id, t.kind, t.start, t.end, t.text));
            }
            k => {
                if let Some(c) = k.chars().next().filter(|c| typeable(*c)) {
                    t.text.push(c);
                }
            }
        }
    });
    // The drag that typing ended: its element is written where it was and
    // selected, as a release would.
    if let Some((id, kind, start, end)) = started_from_gesture {
        let geom = if line_like(&kind) { Geom::Segment { a: start, b: end } } else { Geom::Rect(Rect::from_points(start, end)) };
        local_edit(store, pane, |t| t.set_geom(&id, &geom));
        with_session(pane, |s| {
            s.selected = vec![id.clone()];
            s.last_drawn = Some(Drawn { id: id.clone(), kind, start, end });
        });
    }
    if let Some((id, kind, start, end, text)) = applied {
        let scale = untracked(|| ui.scale.get());
        let read = |t: &str| match &scale {
            Some(sc) => sc.parse_board(t),
            None => t.trim().parse::<f64>().ok(),
        };
        match typed_geom(&kind, start, end, &text, read) {
            Some(geom) => {
                local_edit(store, pane, |t| t.set_geom(&id, &geom));
                let end = match &geom {
                    Geom::Segment { b, .. } => *b,
                    Geom::Rect(r) => Point::new(if end.x < start.x { r.x } else { r.right() }, if end.y < start.y { r.y } else { r.bottom() }),
                    _ => end,
                };
                with_session(pane, |s| s.last_drawn = Some(Drawn { id, kind, start, end }));
            }
            None if !text.trim().is_empty() => {
                let shape = if line_like(&kind) { "a length (12' 6\", 3.5 m; @ 30 for an angle)" } else { "a width and height (12' x 10')" };
                crate::events::show_notice(store, format!("\"{}\" is not {shape}.", text.trim()));
            }
            None => {}
        }
    }
    after_change(store, pane);
    true
}

/// Remember what a drag just drew (its release): typing digits straight
/// after sizes it.
pub(super) fn drawn(pane: PaneId, id: &str, kind: &Kind, start: Point, end: Point) {
    if line_like(kind) || box_like(kind) {
        with_session(pane, |s| s.last_drawn = Some(Drawn { id: id.to_string(), kind: kind.clone(), start, end }));
    }
}

// ── The Scale panel ─────────────────────────────────────────────────────────

/// The Scale panel, floating over the top of pane `pane`'s canvas: it is
/// not modal, so a calibration line can be drawn and its ends dragged
/// while it is open.
pub(super) fn render_panel(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let ui = store.pane(pane).board_ui.measure;
    let shown = move |on: bool| if on { "" } else { "display: none;" };
    let ways: Vec<NodeHandle> = Way::ALL
        .into_iter()
        .map(|way| {
            rsx! {
                div {
                    title: way.tip(),
                    class: {move || if ui.way.get() == way { "pimble-board-chip pimble-board-chip--active" } else { "pimble-board-chip" }},
                    onclick: move || {
                        ui.way.set(way);
                        ui.said.set(None);
                        way_changed(store, pane);
                    },
                    {way.label()}
                }
            }
        })
        .collect();
    let units: Vec<SelectOption> = Unit::ALL.iter().map(|u| SelectOption::new(u.as_str(), u.label())).collect();
    let presets: Vec<SelectOption> = PRESETS.iter().enumerate().map(|(i, p)| SelectOption::new(i.to_string(), format!("{}: {}", p.group, p.text))).collect();
    let metric_precisions: Vec<SelectOption> = std::iter::once(SelectOption::new("auto", "The unit's own"))
        .chain([0u8, 1, 2, 3].iter().map(|p| SelectOption::new(p.to_string(), Unit::M.precision_label(*p))))
        .collect();
    let imperial_precisions: Vec<SelectOption> = std::iter::once(SelectOption::new("auto", "The unit's own"))
        .chain([1u8, 2, 4, 8, 16].iter().map(|p| SelectOption::new(p.to_string(), Unit::In.precision_label(*p))))
        .collect();
    let precision_value = move || ui.precision.get().map(|p| p.to_string()).unwrap_or_else(|| "auto".into());
    let set_precision = move |v: String| ui.precision.set(v.parse::<u8>().ok());
    let metric = move || ui.unit.get().is_metric() || ui.unit.get() == Unit::Mi;
    let says = move || match preview(ui) {
        Ok(s) => s.describe(),
        Err(why) => why,
    };
    let good = move || preview(ui).is_ok();
    let picked_word = move || match ui.picked.get() {
        Some(p) if p.length.is_some() => format!("The selected {} is", kind_name(&p.kind)),
        Some(p) => format!("The selected {} is", kind_name(&p.kind)),
        None => "Select one line, arrow, dimension, box or picture.".to_string(),
    };
    let boxy = move || ui.picked.get().is_some_and(|p| p.length.is_none() && p.width.is_some());
    rsx! {
        div {
            class: "pimble-float pimble-board__scale",
            style: {move || shown(ui.open.get())},
            onmousedown: move || {
                crate::pane_view::press_in_view();
                super::rail::keys_to(store, pane, super::rail::Keys::Chrome);
            },
            onmouseenter: move || super::tools::pointer_off(store, pane),
            div {
                class: "pimble-board__scale-head",
                span { class: "pimble-board__grow", "Scale" }
                div {
                    class: "pimble-board-tool",
                    title: "Close",
                    onclick: move || close_scale(store, pane),
                    IconGlyph { name: TablerIcon::X.name().to_string() }
                }
            }
            div { class: "pimble-board__scale-ways", {ways} }
            div {
                class: "pimble-board__scale-way",
                style: {move || shown(ui.way.get() == Way::Preset)},
                Select {
                    label: "Preset",
                    size: "xs",
                    value_fn: move || ui.preset.get().to_string(),
                    onchange: move |v: String| {
                        if let Ok(i) = v.parse::<usize>() {
                            ui.preset.set(i);
                            if let Some(p) = PRESETS.get(i) {
                                ui.unit.set(p.unit);
                            }
                        }
                    },
                    data: presets,
                }
            }
            div {
                class: "pimble-board__scale-way",
                style: {move || shown(ui.way.get() == Way::Typed)},
                TextInput {
                    label: "Scale",
                    size: "xs",
                    placeholder: "1:100, 1 in = 10 ft, 1/4\" = 1'",
                    value_fn: move || ui.typed.get(),
                    oninput: move |v: String| {
                        // An equation names the unit it is shown in.
                        if let Ok(s) = m::parse_scale(&v, untracked(|| ui.unit.get())) {
                            if v.contains('=') {
                                ui.unit.set(s.unit);
                            }
                        }
                        ui.typed.set(v);
                    },
                    onsubmit: move || apply(store, pane),
                }
            }
            div {
                class: "pimble-board__scale-way",
                style: {move || shown(matches!(ui.way.get(), Way::Element | Way::Calibrate))},
                div {
                    class: "pimble-board__scale-note",
                    style: {move || shown(ui.way.get() == Way::Element)},
                    {move || picked_word()}
                }
                div {
                    class: "pimble-board__scale-note",
                    style: {move || shown(ui.way.get() == Way::Calibrate)},
                    {move || match ui.cal_length.get() {
                        Some(l) => format!("The line drawn is {} board units; drag its ends to adjust. It really is", (l * 10.0).round() / 10.0),
                        None => "Drag on the canvas over something whose length you know (ends snap to points).".to_string(),
                    }}
                }
                div {
                    class: "pimble-board__row",
                    style: {move || shown(ui.way.get() == Way::Element && boxy())},
                    div {
                        title: "Its width is the length typed",
                        class: {move || if !ui.use_height.get() { "pimble-board-chip pimble-board-chip--active" } else { "pimble-board-chip" }},
                        onclick: move || ui.use_height.set(false),
                        "Width"
                    }
                    div {
                        title: "Its height is the length typed",
                        class: {move || if ui.use_height.get() { "pimble-board-chip pimble-board-chip--active" } else { "pimble-board-chip" }},
                        onclick: move || ui.use_height.set(true),
                        "Height"
                    }
                }
                TextInput {
                    label: "Long",
                    size: "xs",
                    placeholder: "12' 6\", 3.5 m",
                    value_fn: move || ui.length.get(),
                    oninput: move |v: String| ui.length.set(v),
                    onsubmit: move || apply(store, pane),
                }
            }
            div {
                class: "pimble-board__scale-way",
                style: {move || shown(ui.way.get() == Way::Picture)},
                div {
                    class: "pimble-board__scale-note",
                    {move || match (ui.picked.get().and_then(|p| p.pixels), ui.scale.get()) {
                        (None, _) => "Select one scanned picture.".to_string(),
                        (Some(_), None) => PICTURE_NEEDS_SCALE.to_string(),
                        (Some((w, h)), Some(s)) => format!("{w:.0} × {h:.0} pixels, sized to this canvas's {}.", s.name()),
                    }}
                }
                TextInput {
                    label: "Scanned at (dots per inch)",
                    size: "xs",
                    value_fn: move || ui.dpi.get(),
                    oninput: move |v: String| ui.dpi.set(v),
                }
                TextInput {
                    label: "Drawn at",
                    size: "xs",
                    placeholder: "1/4\" = 1', 1:100",
                    value_fn: move || ui.drawn_at.get(),
                    oninput: move |v: String| ui.drawn_at.set(v),
                    onsubmit: move || size_picture(store, pane),
                }
            }
            div {
                class: "pimble-board__scale-way",
                style: {move || shown(ui.way.get() != Way::Picture)},
                div {
                    class: "pimble-board__row",
                    Select {
                        label: "Unit",
                        size: "xs",
                        value_fn: move || ui.unit.get().as_str().to_string(),
                        onchange: move |v: String| {
                            if let Some(u) = Unit::parse(&v) {
                                ui.unit.set(u);
                                ui.precision.set(None);
                            }
                        },
                        data: units,
                    }
                    if metric() {
                        Select {
                            label: "Precision",
                            size: "xs",
                            value_fn: precision_value,
                            onchange: set_precision,
                            data: metric_precisions.clone(),
                        }
                    }
                    if !metric() {
                        Select {
                            label: "Precision",
                            size: "xs",
                            value_fn: precision_value,
                            onchange: set_precision,
                            data: imperial_precisions.clone(),
                        }
                    }
                }
                div {
                    class: {move || if good() { "pimble-board__scale-preview" } else { "pimble-board__scale-preview pimble-board__scale-preview--bad" }},
                    {move || says()}
                }
            }
            div {
                class: "pimble-board__scale-note",
                style: {move || shown(ui.said.get().is_some())},
                {move || ui.said.get().unwrap_or_default()}
            }
            div {
                class: "pimble-board__scale-actions",
                div {
                    class: "pimble-board-chip",
                    title: "Take the scale away: nothing shows measurements",
                    style: {move || shown(ui.scale.get().is_some() && ui.way.get() != Way::Picture)},
                    onclick: move || {
                        if set_scale(store, pane, None) {
                            close_scale(store, pane);
                        }
                    },
                    "Remove Scale"
                }
                span { class: "pimble-board__grow" }
                div {
                    class: "pimble-board-chip",
                    title: "Close without changing anything",
                    onclick: move || close_scale(store, pane),
                    "Cancel"
                }
                div {
                    class: "pimble-board-chip pimble-board-chip--primary",
                    title: "Resize the selected picture so it measures true at the canvas's scale (one undoable edit)",
                    style: {move || shown(ui.way.get() == Way::Picture)},
                    onclick: move || size_picture(store, pane),
                    "Size to Scale"
                }
                div {
                    class: "pimble-board-chip pimble-board-chip--primary",
                    title: "Set this scale for the canvas (one undoable edit; everyone sees it)",
                    style: {move || shown(ui.way.get() != Way::Picture)},
                    onclick: move || apply(store, pane),
                    "Apply"
                }
            }
        }
    }
}

fn kind_name(kind: &Kind) -> &'static str {
    match kind {
        Kind::Line => "line",
        Kind::Arrow => "arrow",
        Kind::Dimension => "dimension",
        Kind::Connector => "connector",
        Kind::Picture => "picture",
        Kind::Rect => "rectangle",
        _ => "element",
    }
}

/// The readout near the pointer, over the canvas.
pub(super) fn render_readout(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let ui = store.pane(pane).board_ui.measure;
    rsx! {
        div {
            class: "pimble-board__readout",
            style: {move || match ui.readout.get() {
                Some((x, y, _)) => format!("left: {x:.1}px; top: {y:.1}px;"),
                None => "display: none;".to_string(),
            }},
            {move || ui.readout.get().map(|(_, _, t)| t).unwrap_or_default()}
        }
    }
}

// ── The selection's group ───────────────────────────────────────────────────

/// The selection's measuring controls for its toolbar group (with a scale):
/// Show Dimensions and Show Area switches, and Measures, a picker with its
/// sizes to type and its perimeter and area. And the picker.
pub(super) fn render_controls(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> (NodeHandle, NodeHandle) {
    let state = store.pane(pane);
    let ui = state.board_ui;
    let mu = ui.measure;
    let shown = move |on: bool| if on { "" } else { "display: none;" };
    let sel = move || mu.selection.get();
    let dims_btn = rsx! {
        div {
            title: {move || if sel().is_some_and(|s| s.dims) { "Show Dimensions: on (its sides labeled in the canvas's units)" } else { "Show Dimensions: off" }},
            class: {move || if sel().is_some_and(|s| s.dims) { "pimble-board-tool pimble-board-tool--active" } else { "pimble-board-tool" }},
            style: {move || shown(sel().is_some_and(|s| s.can_dims))},
            onclick: move || toggle_labels(store, pane, false),
            IconGlyph { name: TablerIcon::Dimensions.name().to_string() }
        }
    };
    let area_btn = rsx! {
        div {
            title: {move || if sel().is_some_and(|s| s.area_on) { "Show Area: on (its area at its center)" } else { "Show Area: off" }},
            class: {move || if sel().is_some_and(|s| s.area_on) { "pimble-board-tool pimble-board-tool--active" } else { "pimble-board-tool" }},
            style: {move || shown(sel().is_some_and(|s| s.can_area))},
            onclick: move || toggle_labels(store, pane, true),
            IconGlyph { name: TablerIcon::SquareDot.name().to_string() }
        }
    };
    let measures_tip = move || match sel() {
        Some(s) => {
            let parts: Vec<String> = [
                s.length.map(|v| format!("length {v}")),
                s.angle.map(|v| format!("angle {v}")),
                s.width.map(|v| format!("width {v}")),
                s.height.map(|v| format!("height {v}")),
                s.perimeter.map(|v| format!("perimeter {v}")),
                s.area.map(|v| format!("area {v}")),
            ]
            .into_iter()
            .flatten()
            .collect();
            if parts.is_empty() { "Measures: none for this".to_string() } else { format!("Measures: {}", parts.join(", ")) }
        }
        None => "Measures".to_string(),
    };
    let measures_face = rsx! { span { class: "pimble-board__face", IconGlyph { name: TablerIcon::Ruler3.name().to_string() } } };
    let measures_btn = super::rail::picker_button(__scope, ui, measures_tip, Popup::Measures, "", measures_face);
    let bar = rsx! {
        div {
            class: "pimble-board__ctx-group pimble-board__ctx-sep",
            style: {move || shown(sel().is_some())},
            {dims_btn}
            {area_btn}
            div { class: "pimble-board__ctx-group", style: {move || shown(sel().is_some_and(|s| s.one.is_some()))}, {measures_btn} }
        }
    };
    let row = |__scope: &mut RenderScope, label: &'static str, value: Box<dyn Fn() -> Option<String>>| {
        let value = std::rc::Rc::new(value);
        let v2 = value.clone();
        rsx! {
            div {
                class: "pimble-board__row",
                style: {|| if v2().is_some() { "" } else { "display: none;" }},
                span { class: "pimble-board__ctx-label", {label} }
                span { class: "pimble-board__measure-value", {|| value().unwrap_or_default()} }
            }
        }
    };
    let field = |__scope: &mut RenderScope, label: &'static str, text: Signal<String>, size: Size, on: Box<dyn Fn() -> bool>| {
        rsx! {
            div {
                class: "pimble-board__row",
                style: {|| if on() { "" } else { "display: none;" }},
                span { class: "pimble-board__ctx-label", {label} }
                TextInput {
                    size: "xs",
                    value_fn: move || text.get(),
                    oninput: move |v: String| text.set(v),
                    onsubmit: move || set_size(store, pane, size, &untracked(|| text.get())),
                }
            }
        }
    };
    let length_field = field(__scope, "Length", mu.length_text, Size::Length, Box::new(move || sel().is_some_and(|s| s.length_editable)));
    let width_field = field(__scope, "Width", mu.width_text, Size::Width, Box::new(move || sel().is_some_and(|s| s.box_editable)));
    let height_field = field(__scope, "Height", mu.height_text, Size::Height, Box::new(move || sel().is_some_and(|s| s.box_editable)));
    let offset_field = field(__scope, "Offset", mu.offset_text, Size::Offset, Box::new(move || sel().is_some_and(|s| s.offset.is_some())));
    let length_row = row(__scope, "Length", Box::new(move || sel().filter(|s| !s.length_editable).and_then(|s| s.length)));
    let angle_row = row(__scope, "Angle", Box::new(move || sel().and_then(|s| s.angle)));
    let width_row = row(__scope, "Width", Box::new(move || sel().filter(|s| !s.box_editable).and_then(|s| s.width)));
    let height_row = row(__scope, "Height", Box::new(move || sel().filter(|s| !s.box_editable).and_then(|s| s.height)));
    let perimeter_row = row(__scope, "Perimeter", Box::new(move || sel().and_then(|s| s.perimeter)));
    let area_row = row(__scope, "Area", Box::new(move || sel().and_then(|s| s.area)));
    let popup = rsx! {
        div {
            class: "pimble-float pimble-board__options pimble-board__options--rows pimble-board__measures",
            style: {move || shown(ui.popup.get() == Popup::Measures && sel().is_some())},
            {length_field}
            {length_row}
            {angle_row}
            {width_field}
            {height_field}
            {offset_field}
            {width_row}
            {height_row}
            {perimeter_row}
            {area_row}
            div { class: "pimble-board__scale-note", "Type a size and press Enter (12' 6\", 3.5 m)." }
        }
    };
    (bar, popup)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feet() -> Scale {
        m::parse_scale("1 in = 10 ft", Unit::Ft).unwrap()
    }

    #[test]
    fn the_panel_judges_each_way() {
        let line = PickedEl { id: "a".into(), kind: Kind::Line, length: Some(240.0), width: None, height: None, pixels: None };
        let boxed = PickedEl { id: "b".into(), kind: Kind::Rect, length: None, width: Some(96.0), height: Some(48.0), pixels: None };
        let c = |way, typed: &str, length: &str, use_height, picked: Option<&PickedEl>, cal| candidate(way, 2, typed, length, use_height, picked, cal, Unit::Ft, None);
        // Preset 2 is 1/4" = 1': a foot is 24 units.
        let s = c(Way::Preset, "", "", false, None, None).unwrap();
        assert!((s.per_unit - 24.0).abs() < 1e-9 && s.label.as_deref() == Some("1/4\" = 1'"));
        let s = c(Way::Typed, "1 in = 10 ft", "", false, None, None).unwrap();
        assert!((s.per_unit - 9.6).abs() < 1e-9);
        assert!(c(Way::Typed, "nonsense", "", false, None, None).is_err());
        assert!(c(Way::Typed, "", "", false, None, None).is_err());
        // The selected line is 10 feet: 24 units a foot.
        let s = c(Way::Element, "", "10'", false, Some(&line), None).unwrap();
        assert!((s.per_unit - 24.0).abs() < 1e-9);
        // A box: its height, 48 units, is 4 feet: 12 a foot.
        let s = c(Way::Element, "", "4 ft", true, Some(&boxed), None).unwrap();
        assert!((s.per_unit - 12.0).abs() < 1e-9);
        assert!(c(Way::Element, "", "4 ft", false, None, None).is_err(), "nothing selected");
        assert!(c(Way::Element, "", "", false, Some(&line), None).is_err(), "no length typed");
        assert!(c(Way::Element, "", "long", false, Some(&line), None).is_err(), "unreadable");
        let s = c(Way::Calibrate, "", "5 ft", false, None, Some(120.0)).unwrap();
        assert!((s.per_unit - 24.0).abs() < 1e-9);
        assert!(c(Way::Calibrate, "", "5 ft", false, None, None).is_err(), "no line drawn");
        // The unit picker re-expresses a preset in its unit.
        let s = candidate(Way::Preset, 2, "", "", false, None, None, Unit::In, Some(16)).unwrap();
        assert_eq!(s.unit, Unit::In);
        assert!((s.per_unit - 2.0).abs() < 1e-9, "24 units a foot is 2 an inch");
        assert_eq!(s.precision, Some(16));
    }

    #[test]
    fn typed_sizes_follow_the_drawn_direction() {
        let s = feet();
        let read = |t: &str| s.parse_board(t);
        let (a, b) = (Point::new(10.0, 10.0), Point::new(10.0, -50.0));
        let Some(Geom::Segment { a: a2, b: b2 }) = typed_geom(&Kind::Line, a, b, "12'6\"", read) else { panic!() };
        assert_eq!(a2, a);
        assert!((b2.x - 10.0).abs() < 1e-3 && (b2.y - (10.0 - 120.0)).abs() < 1e-3, "{b2:?}: up, 12.5 ft is 120 units");
        let Some(Geom::Segment { b: b3, .. }) = typed_geom(&Kind::Line, a, b, "10' @ 0", read) else { panic!() };
        assert!((b3.x - 106.0).abs() < 1e-3 && (b3.y - 10.0).abs() < 1e-3, "{b3:?}: east by the angle");
        // A box drawn up and to the left keeps going that way.
        let Some(Geom::Rect(r)) = typed_geom(&Kind::Rect, a, Point::new(0.0, 0.0), "10' x 5'", read) else { panic!() };
        assert!((r.x - (10.0 - 96.0)).abs() < 1e-3 && (r.y - (10.0 - 48.0)).abs() < 1e-3 && (r.w - 96.0).abs() < 1e-3 && (r.h - 48.0).abs() < 1e-3, "{r:?}");
        let Some(Geom::Rect(sq)) = typed_geom(&Kind::Rect, a, Point::new(20.0, 20.0), "10'", read) else { panic!() };
        assert!((sq.w - 96.0).abs() < 1e-3 && (sq.h - 96.0).abs() < 1e-3);
        assert!(typed_geom(&Kind::Rect, a, b, "big", read).is_none());
        assert!(typed_geom(&Kind::Text, a, b, "10'", read).is_none());
    }

    #[test]
    fn a_scale_reads_in_another_unit() {
        let s = in_unit(&feet(), Unit::In);
        assert_eq!(s.unit, Unit::In);
        assert!((s.per_unit - 0.8).abs() < 1e-9);
        assert_eq!(s.length(9.6), "12″");
    }
}
