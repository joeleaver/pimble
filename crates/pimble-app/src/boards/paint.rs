//! Painting on a canvas (docs/DRAWING_CONTRACT.md section 3, "Built in wave
//! 4 (UI)"): the Brush with its presets, the Highlighter (the brush's
//! highlighter preset, always into a multiply "Highlights" layer), the
//! Eraser in paint layers, the Eyedropper (and Alt held with a brush), `[`
//! and `]`, the brush cursor, the tile cache paint layers draw from, and the
//! layers panel's thumbnails. Wave 5 (docs/DRAWING_CONTRACT.md, "Built in
//! wave 5 (UI)"): the Fill, and Smudge and Blur, which stream as a stroke
//! does.
//!
//! A stroke follows the model's recommended flow: on the press one edit
//! with the layer (`paint_layer_for` or `highlights_layer`) and
//! `begin_stroke`, sent at once; each pointer event appends its point to the
//! document here at once (the tile work is only the new dabs) and the
//! updates go out merged about every 100 ms; the release ends the stroke.
//! The undo clock is held from the press to the release, so a stroke is one
//! undo step.
//!
//! The pure parts are tested here: the presets the picker offers, the
//! pressure a pointer paints with, `[`/`]` steps, the brush cursor's circle,
//! a thumbnail's box.

use std::sync::OnceLock;

use pimble_board::brush::{self, BrushPoint, BrushSettings, Preset, PRESETS};
use pimble_board::{EditOverlay, LayerKind, NewFill, NewRetouch, NewStroke, PaintBody, PaintCache, Retouch, RgbaImage};
use rinch::render_surface::SurfacePointerKind;

use super::rail::Well;
use super::*;

// ── Settings ────────────────────────────────────────────────────────────────

/// The eraser's first size, board units.
pub const ERASER_SIZE: f32 = 24.0;
/// Smudge's and Blur's first size.
pub const RETOUCH_SIZE: f32 = 30.0;
/// The Fill's tolerances (the context bar's chips): a label and the value.
pub const TOLERANCES: [(&str, f32); 6] = [("0%", 0.0), ("5%", 0.05), ("15%", 0.15), ("30%", 0.3), ("50%", 0.5), ("80%", 0.8)];
/// Smudge's and Blur's strengths.
pub const STRENGTHS: [(&str, f32); 5] = [("10%", 0.1), ("25%", 0.25), ("50%", 0.5), ("75%", 0.75), ("100%", 1.0)];

/// A chip's label for `value` among `choices` (the nearest one's, so a
/// value set elsewhere still shows), as a percent when none is near.
pub fn choice_label(choices: &[(&'static str, f32)], value: f32) -> String {
    choices
        .iter()
        .find(|(_, v)| (v - value).abs() < 0.005)
        .map(|(l, _)| l.to_string())
        .unwrap_or_else(|| format!("{}%", (value * 100.0).round()))
}

/// The brush sizes the size picker offers, board units.
pub const SIZE_RANGE: (f32, f32) = (1.0, 250.0);

/// Each preset's settings as the context bar has changed them (the
/// session's, like every tool option), in [`PRESETS`] order.
pub fn default_brushes() -> Vec<BrushSettings> {
    PRESETS.iter().map(|p| p.settings).collect()
}

/// The presets the context bar's picker offers: name and label, in menu
/// order (the highlighter last; picking it picks the Highlighter tool).
pub fn preset_choices() -> Vec<(&'static str, &'static str)> {
    PRESETS.iter().map(|p| (p.name, p.label)).collect()
}

/// The preset `tool` paints with: the highlighter's own, or the brush's
/// picked one.
pub fn preset_for(tool: Tool, brush_name: &str) -> &'static Preset {
    match tool {
        Tool::Highlighter => &brush::HIGHLIGHTER,
        _ => brush::preset(brush_name).filter(|p| p.name != brush::HIGHLIGHTER.name).unwrap_or(&brush::ROUND),
    }
}

/// The tool a preset picked in the context bar makes current: the
/// highlighter preset is the Highlighter, any other the Brush.
pub fn tool_for_preset(name: &str) -> Tool {
    if name == brush::HIGHLIGHTER.name {
        Tool::Highlighter
    } else {
        Tool::Brush
    }
}

fn index_of(preset: &Preset) -> usize {
    PRESETS.iter().position(|p| p.name == preset.name).unwrap_or(0)
}

/// The preset the current tool paints with (tracked), `None` for a tool
/// that does not paint with one.
pub(super) fn current_preset(store: AppStore) -> Option<&'static Preset> {
    let tool = store.board_tool.get();
    matches!(tool, Tool::Brush | Tool::Highlighter).then(|| preset_for(tool, &store.board_style.brush.get()))
}

/// The settings the current tool paints with (tracked): the preset's as the
/// context bar left them; the eraser's are the round brush at its own size.
pub(super) fn current_settings(store: AppStore) -> Option<BrushSettings> {
    let st = store.board_style;
    match store.board_tool.get() {
        Tool::Eraser => Some(brush::ROUND.settings.with_size(st.eraser_size.get())),
        // Smudge and Blur work with the round brush at their own size.
        Tool::Smudge | Tool::Blur => Some(brush::ROUND.settings.with_size(st.retouch_size.get())),
        _ => {
            let preset = current_preset(store)?;
            Some(st.brushes.get().get(index_of(preset)).copied().unwrap_or(preset.settings))
        }
    }
}

/// Change the current tool's settings (the context bar, `[` and `]`).
pub(super) fn change_settings(store: AppStore, f: impl FnOnce(&mut BrushSettings)) {
    let st = store.board_style;
    let tool = untracked(|| store.board_tool.get());
    if tool == Tool::Eraser {
        let mut s = brush::ROUND.settings.with_size(untracked(|| st.eraser_size.get()));
        f(&mut s);
        st.eraser_size.set(s.size.clamp(brush::MIN_SIZE, brush::MAX_SIZE));
        return;
    }
    if matches!(tool, Tool::Smudge | Tool::Blur) {
        let mut s = brush::ROUND.settings.with_size(untracked(|| st.retouch_size.get()));
        f(&mut s);
        st.retouch_size.set(s.size.clamp(brush::MIN_SIZE, brush::MAX_SIZE));
        return;
    }
    let Some(preset) = untracked(|| current_preset(store)) else { return };
    let at = index_of(preset);
    st.brushes.update(|all| {
        if all.len() != PRESETS.len() {
            *all = default_brushes();
        }
        let mut s = all[at];
        f(&mut s);
        all[at] = s.sanitized();
    });
}

/// Pick preset `name` from the context bar: the Brush with it, or the
/// Highlighter.
pub(super) fn pick_preset(store: AppStore, name: &'static str) {
    let tool = tool_for_preset(name);
    if tool == Tool::Brush {
        store.board_style.brush.set(name);
    }
    store.board_tool.set(tool);
}

/// The next brush size for `[` (smaller) and `]` (larger): about a fifth
/// either way, at least a whole unit, within the brush's limits.
pub fn next_size(size: f32, larger: bool) -> f32 {
    let next = if larger { (size * 1.2).max(size + 1.0) } else { (size / 1.2).min(size - 1.0) };
    ((next * 2.0).round() / 2.0).clamp(brush::MIN_SIZE, brush::MAX_SIZE)
}

/// `[` or `]` with a paint tool: the brush (or the eraser) a size smaller or
/// larger; the cursor follows.
pub(super) fn step_size(store: AppStore, pane: PaneId, larger: bool) {
    change_settings(store, |s| s.size = next_size(s.size, larger));
    super::points::refresh_overlay(store, pane);
    present(pane);
}

/// The color the current paint tool paints with (tracked): the color
/// well's front square (the eyedropper picks into it), a gradient's first
/// color; the highlighter left on the marks' red paints its own yellow
/// (red multiplied over a picture reads as a dark smear).
pub(super) fn brush_color(store: AppStore) -> Color {
    let st = store.board_style;
    let front = match st.front.get() {
        Well::Fill => st.fill_gradient.get().and_then(|g| g.stops.first().map(|s| s.color)).or_else(|| st.fill.get().and_then(|c| Color::parse(&c))),
        Well::Stroke => None,
    };
    let color = front
        .or_else(|| st.stroke_gradient.get().and_then(|g| g.stops.first().map(|s| s.color)))
        .or_else(|| Color::parse(&st.stroke.get()))
        .unwrap_or(Color::BLACK);
    if store.board_tool.get() == Tool::Highlighter && color.to_hex() == DEFAULT_COLOR {
        return brush::HIGHLIGHTER.color;
    }
    color
}

/// The pressure a pointer paints with (the model's rule: 1 for an input
/// without pressure, so a mouse paints at the brush's size): a pen's as
/// reported; a mouse's and a finger's 1 (a touch screen without force
/// sensing reports 0.5 whatever the touch). `None` for a pen's reading of
/// nothing (a moment without pressure mid-stroke keeps the last).
pub fn brush_pressure(kind: SurfacePointerKind, pressure: f32) -> Option<f32> {
    match kind {
        SurfacePointerKind::Pen | SurfacePointerKind::Eraser => (pressure.is_finite() && pressure > 0.0).then(|| pressure.clamp(0.0, 1.0)),
        _ => Some(1.0),
    }
}

// ── The cache paint layers draw from ────────────────────────────────────────

/// Bytes of tiles kept in memory (the model's default, Joe's 256 MB).
pub const MEMORY_BUDGET: usize = 256 << 20;
/// What the desktop's disk cache is pruned to when the app starts.
#[cfg(all(feature = "native", not(test)))]
pub const DISK_BUDGET: u64 = 512 << 20;

/// The one tile cache of this app (docs/DRAWING_CONTRACT.md, "Rendering and
/// its cache"): every board pane, the navigator, exports and pictures in
/// text draw paint layers from it, so a painting shown in two places is
/// replayed once and a picture in text keeps its tiles between renders.
/// Tiles are keyed by their layer and every operation touching them, so
/// sharing it between boards is safe. The desktop keeps tiles on disk too,
/// under the user's cache folder (`pimble/tiles`), pruned to 512 MB the
/// first time it is made; the browser keeps them in memory only.
pub(crate) fn shared_cache() -> Arc<PaintCache> {
    static CACHE: OnceLock<Arc<PaintCache>> = OnceLock::new();
    CACHE.get_or_init(make_cache).clone()
}

#[cfg(all(feature = "native", not(test)))]
fn make_cache() -> Arc<PaintCache> {
    let Some(dir) = dirs::cache_dir().map(|d| d.join("pimble").join("tiles")) else {
        return PaintCache::with_store(None, MEMORY_BUDGET);
    };
    let store = Arc::new(pimble_board::DirTileStore::new(dir.clone()));
    let pruning = store.clone();
    std::thread::spawn(move || {
        let removed = pruning.prune(DISK_BUDGET);
        if removed > 0 {
            tracing::info!("the paint tile cache in {} dropped {removed} old tiles", dir.display());
        }
    });
    PaintCache::with_store(Some(store), MEMORY_BUDGET)
}

#[cfg(any(not(feature = "native"), test))]
fn make_cache() -> Arc<PaintCache> {
    PaintCache::with_store(None, MEMORY_BUDGET)
}

// ── A stroke ────────────────────────────────────────────────────────────────

/// A paint stroke being drawn: its operation, and the point last appended.
#[derive(Debug, Clone)]
pub(super) struct Stroke {
    pub id: String,
    pub last: BrushPoint,
}

/// Where a press with the eraser erases: elements of a vector layer (as
/// before paint), or with an erasing stroke in this paint layer.
enum EraseIn {
    Elements,
    Paint(String),
}

/// The picture a highlighter press lands on (pictures in locked layers
/// included: a picture's layer is locked), else the one being marked up.
fn picture_at(s: &Session, p: Point) -> Option<String> {
    let snapshot = s.snapshot();
    // The topmost (elements are in drawing order).
    let hit = snapshot
        .query(&Rect::new(p.x, p.y, 0.0, 0.0))
        .into_iter()
        .filter(|i| {
            let e = &snapshot.elements[*i];
            e.raw.kind.is_picture() && e.bounds.contains(p)
        })
        .max();
    if let Some(i) = hit {
        return Some(snapshot.elements[i].raw.id.clone());
    }
    if s.markup.is_some() {
        return snapshot.elements.iter().find(|e| e.raw.kind.is_picture()).map(|e| e.raw.id.clone());
    }
    None
}

/// What the eraser does at `p`: the picked layer's kind decides (a paint
/// layer is erased with a stroke, a vector layer loses whole elements);
/// with nothing picked, an element under the press is erased, else the
/// topmost shown paint layer painting there.
fn erase_in(store: AppStore, pane: PaneId, p: Point) -> Result<EraseIn, String> {
    let picked = untracked(|| store.pane(pane).board_ui.layer.get());
    with_session(pane, |s| {
        let snapshot = s.snapshot();
        if let Some(layer) = picked.as_deref().and_then(|id| snapshot.layer(id)) {
            return match layer.kind {
                LayerKind::Paint if layer.effective_locked => Err(pimble_board::locks::LAYER_LOCKED.to_string()),
                LayerKind::Paint => Ok(EraseIn::Paint(layer.id.clone())),
                _ => Ok(EraseIn::Elements),
            };
        }
        if super::tools::hit_at(s, p).is_some() {
            return Ok(EraseIn::Elements);
        }
        let painted = snapshot
            .layers
            .iter()
            .enumerate()
            .rev()
            .find(|(i, l)| l.kind == LayerKind::Paint && snapshot.paint_shown(*i) && !l.effective_locked && snapshot.paint_bounds(&l.id).is_some_and(|b| b.contains(p)));
        Ok(match painted {
            Some((_, l)) => EraseIn::Paint(l.id.clone()),
            None => EraseIn::Elements,
        })
    })
    .unwrap_or(Ok(EraseIn::Elements))
}

/// The effect a retouching tool works.
fn retouch_effect(tool: Tool) -> Option<Retouch> {
    match tool {
        Tool::Smudge => Some(Retouch::Smudge),
        Tool::Blur => Some(Retouch::Blur),
        _ => None,
    }
}

/// Smudge's and Blur's sentence where there is no paint to work.
pub const NO_PAINT_HERE: &str = "Smudge and Blur work on paint, and this canvas has no painting yet.";

/// A Fill press at board point `p` (docs/DRAWING_CONTRACT.md section 3):
/// the color well's front square poured into the paint layer the brush
/// would paint into ("Paint N" made when needed, and picked), at the
/// context bar's tolerance, contiguous or everywhere, clipped to what this
/// device sees together with the layer's painting (`fill_clip`). One undo
/// step, the layer made with it included.
pub(super) fn fill(store: AppStore, pane: PaneId, p: Point) {
    if !writable_or_say(store, pane) {
        return;
    }
    let st = store.board_style;
    let (color, tolerance, contiguous) = untracked(|| (brush_color(store), st.tolerance.get(), st.contiguous.get()));
    let picked = untracked(|| store.pane(pane).board_ui.layer.get());
    new_undo_step(pane);
    super::points::hold_clock(pane, true);
    let layer = local_edit_said(store, pane, |t| t.paint_layer_for(picked.as_deref()));
    if let Some(layer) = layer {
        let clip = with_session(pane, |s| pimble_board::fill_clip(s.snapshot(), &layer, p, Some(s.visible_rect())));
        if let Some(clip) = clip {
            let fill = NewFill::new(layer.clone(), p, color, clip).with_tolerance(tolerance);
            let fill = if contiguous { fill } else { fill.everywhere() };
            local_edit_said(store, pane, |t| t.add_fill(fill));
        }
        if picked.as_deref() != Some(layer.as_str()) {
            store.pane(pane).board_ui.layer.set(Some(layer));
        }
    }
    super::points::hold_clock(pane, false);
    new_undo_step(pane);
    with_session(pane, |s| s.selected.clear());
    after_change(store, pane);
}

/// A press with a paint tool at board point `p`: Alt held with a brush
/// picks a color; the eraser on a vector layer starts erasing elements
/// (answers `false`, the caller's way); otherwise a stroke begins.
/// Answers whether it handled the press.
pub(super) fn press(store: AppStore, pane: PaneId, tool: Tool, p: Point, px: f32, py: f32) -> bool {
    let alt = with_session(pane, |s| s.alt).unwrap_or(false);
    if alt && matches!(tool, Tool::Brush | Tool::Highlighter) {
        with_session(pane, |s| s.gesture = Gesture::Eyedrop);
        eyedrop(store, pane, px, py);
        return true;
    }
    if !writable_or_say(store, pane) {
        return true;
    }
    let erase = match tool {
        Tool::Eraser => match erase_in(store, pane, p) {
            Ok(EraseIn::Elements) => return false,
            Ok(EraseIn::Paint(layer)) => Some(layer),
            Err(sentence) => {
                crate::events::show_notice(store, sentence);
                return true;
            }
        },
        Tool::Smudge | Tool::Blur => match super::paint_select::paint_layer_at(store, pane, p, NO_PAINT_HERE) {
            Ok(layer) => Some(layer),
            Err(sentence) => {
                crate::events::show_notice(store, sentence);
                return true;
            }
        },
        _ => None,
    };
    let pressure = with_session(pane, |s| s.paint_pressure).unwrap_or(1.0);
    let point: BrushPoint = [p.x, p.y, pressure, 0.0];
    let st = store.board_style;
    let (preset, settings, color, strength) = untracked(|| {
        let preset = match tool {
            Tool::Smudge | Tool::Blur => &brush::ROUND,
            _ => preset_for(tool, &st.brush.get()),
        };
        (preset, current_settings(store).unwrap_or(preset.settings), brush_color(store), st.strength.get())
    });
    let picked = untracked(|| store.pane(pane).board_ui.layer.get());
    let picture = (tool == Tool::Highlighter).then(|| with_session(pane, |s| picture_at(s, p))).flatten().flatten();
    // A new stroke is a new undo step, and everything until the release
    // joins it.
    new_undo_step(pane);
    super::points::hold_clock(pane, true);
    let begun = local_edit_said(store, pane, |t| {
        let layer = match (&erase, tool) {
            (Some(layer), _) => layer.clone(),
            (None, Tool::Highlighter) => t.highlights_layer(picture.as_deref())?,
            (None, _) => t.paint_layer_for(picked.as_deref())?,
        };
        if let Some(effect) = retouch_effect(tool) {
            let retouch = NewRetouch::new(layer.clone(), effect, preset).with_settings(settings).with_strength(strength).with_points(vec![point]);
            return Ok((t.begin_retouch(retouch)?, layer));
        }
        let stroke = NewStroke::new(layer.clone(), preset, color).with_settings(settings).with_points(vec![point]);
        let stroke = if tool == Tool::Eraser { stroke.erasing() } else { stroke };
        Ok((t.begin_stroke(stroke)?, layer))
    });
    let Some((id, layer)) = begun else {
        super::points::hold_clock(pane, false);
        return true;
    };
    // The brush picks the layer it painted into (the highlighter leaves the
    // pick alone: its "Highlights" is its own).
    if tool == Tool::Brush && picked.as_deref() != Some(layer.as_str()) {
        store.pane(pane).board_ui.layer.set(Some(layer));
    }
    with_session(pane, |s| {
        s.selected.clear();
        s.gesture = Gesture::Paint(Stroke { id, last: point });
    });
    after_change(store, pane);
    true
}

/// The pointer moved during a stroke: its point is appended at once, sent
/// with the others about every 100 ms.
pub(super) fn moved(store: AppStore, pane: PaneId, id: String, point: BrushPoint) {
    held_edit(store, pane, |t| t.append_stroke(&id, &[point]));
    flush(store, pane, false);
    after_change(store, pane);
}

/// The stroke was released at `point` (appended when the pointer moved
/// since): it ends, everything still held goes out, and the undo step it
/// was closes.
pub(super) fn released(store: AppStore, pane: PaneId, stroke: Stroke, point: BrushPoint) {
    let moved = (point[0] - stroke.last[0]).hypot(point[1] - stroke.last[1]) > 1e-3;
    held_edit(store, pane, |t| {
        if moved {
            t.append_stroke(&stroke.id, &[point])?;
        }
        t.end_stroke(&stroke.id).map(|_| ())
    });
    flush(store, pane, true);
    super::points::hold_clock(pane, false);
    after_change(store, pane);
}

/// The stroke was taken back (a cancelled pointer, the first finger of a
/// pinch, Escape): its operation goes.
pub(super) fn cancelled(store: AppStore, pane: PaneId, stroke: Stroke) {
    held_edit(store, pane, |t| t.delete_paint_op(&stroke.id));
    flush(store, pane, true);
    super::points::hold_clock(pane, false);
}

/// An edit made here at once and sent later with the others ([`flush`]).
pub(super) fn held_edit<R>(store: AppStore, pane: PaneId, f: impl FnOnce(&mut BoardTxn<'_, '_>) -> pimble_board::doc::Result<R>) -> Option<R> {
    let held = with_session(pane, |s| (s.store_id, s.node_id))?;
    let author = crate::shares::author_in(store, held.0, held.1);
    let now = now_stamp();
    let (result, first) = with_session(pane, |s| {
        let edited = s.board.edit(Stamp::new(author.as_ref(), &now), f);
        s.sync_view();
        let first = s.outbox.is_empty();
        if let Some(bytes) = edited.update {
            s.outbox.push(bytes);
        }
        (edited.result, first)
    })?;
    // Nothing moves for a while: what is held still goes out.
    if first {
        let _ = set_timeout(APPEND_EVERY_MS as u32 + 10, move || flush(store, pane, false));
    }
    match result {
        Ok(r) => Some(r),
        Err(e) => {
            tracing::info!("a paint edit was refused: {e}");
            None
        }
    }
}

/// Send what [`held_edit`] kept, merged into one update: when `now`, or
/// once about 100 ms have passed since the last send.
pub(super) fn flush(store: AppStore, pane: PaneId, now: bool) {
    let out = with_session(pane, |s| {
        if s.outbox.is_empty() || (!now && now_ms() - s.outbox_at < APPEND_EVERY_MS) {
            return None;
        }
        s.outbox_at = now_ms();
        Some(((s.store_id, s.node_id), std::mem::take(&mut s.outbox)))
    })
    .flatten();
    let Some((held, updates)) = out else { return };
    let bytes = if updates.len() == 1 {
        updates.into_iter().next().unwrap_or_default()
    } else {
        match yrs::merge_updates_v1(&updates) {
            Ok(merged) => merged,
            Err(e) => {
                // Sent one by one rather than lost.
                tracing::warn!("paint updates could not be merged ({e}); sending them one by one");
                for bytes in updates {
                    send_update(store, pane, held.0, held.1, bytes);
                }
                return;
            }
        }
    };
    send_update(store, pane, held.0, held.1, bytes);
}

// ── The eyedropper ──────────────────────────────────────────────────────────

/// The color the person sees at physical pixel (`px`, `py`): the view's
/// own pixels when it has them, else the board composited there over its
/// background.
fn color_under(pane: PaneId, px: f32, py: f32, dark: bool) -> Option<Color> {
    with_session(pane, |s| {
        if let Some(c) = s.view.color_at(px, py) {
            return c;
        }
        let p = s.view.screen_to_board(px, py);
        let snapshot = s.snapshot();
        let (pictures, _) = pictures_of(snapshot);
        pimble_board::sample_color(snapshot, &pictures, p, Some(background(dark)))
    })
}

/// Pick the color under (`px`, `py`) into the color well's front square
/// and the recent colors (the brush paints with the front square). The
/// selection is not written: picking is looking.
pub(super) fn eyedrop(store: AppStore, pane: PaneId, px: f32, py: f32) {
    let dark = untracked(|| store.dark_mode.get());
    let Some(color) = color_under(pane, px, py, dark) else { return };
    let hex = Color::rgb(color.r, color.g, color.b).to_hex();
    let st = store.board_style;
    match untracked(|| st.front.get()) {
        Well::Stroke => {
            st.stroke.set_if_changed(hex.clone());
            st.stroke_gradient.set_if_changed(None);
        }
        Well::Fill => {
            st.fill.set_if_changed(Some(hex.clone()));
            st.fill_gradient.set_if_changed(None);
        }
    }
    st.recent.update(|r| {
        let next = super::rail::remember_recent(r, &hex);
        if *r != next {
            *r = next;
        }
    });
}

// ── The brush cursor ────────────────────────────────────────────────────────

/// A circle as four cubic segments (the edit overlay draws curves).
pub fn circle(center: Point, radius: f32) -> Vec<[Point; 4]> {
    const K: f32 = 0.552_284_8;
    let (cx, cy, r) = (center.x, center.y, radius);
    let k = r * K;
    let p = |x: f32, y: f32| Point::new(cx + x, cy + y);
    vec![
        [p(r, 0.0), p(r, k), p(k, r), p(0.0, r)],
        [p(0.0, r), p(-k, r), p(-r, k), p(-r, 0.0)],
        [p(-r, 0.0), p(-r, -k), p(-k, -r), p(0.0, -r)],
        [p(0.0, -r), p(k, -r), p(r, -k), p(r, 0.0)],
    ]
}

/// The brush cursor's diameter for the current paint tool, board units
/// (`None`: no circle), untracked.
pub(super) fn cursor_size(store: AppStore) -> Option<f32> {
    untracked(|| current_settings(store)).map(|s| s.size)
}

/// The brush cursor over pane `pane`'s board: a circle of the brush's size
/// where the pointer is (never smaller than a few pixels on screen).
pub(super) fn cursor_overlay(s: &Session, size: f32, scale: f32) -> Option<EditOverlay> {
    if !s.over || s.alt || matches!(s.gesture, Gesture::Pan { .. } | Gesture::Pinch | Gesture::Eyedrop) {
        return None;
    }
    let at = s.view.screen_to_board(s.pointer.0, s.pointer.1);
    let min = 3.0 * scale / s.view.zoom().max(1e-3);
    let mut o = EditOverlay::default();
    o.curves = circle(at, (size / 2.0).max(min));
    Some(o)
}

// ── Layer thumbnails ────────────────────────────────────────────────────────

/// A thumbnail's size in physical pixels (shown at half that).
pub const THUMB: (u32, u32) = (48, 36);
/// How long a board is left alone before its thumbnails are drawn again.
const THUMB_MS: u32 = 600;

/// The scale that fits `rect` into a thumbnail.
pub fn thumb_scale(rect: &Rect) -> f32 {
    let (w, h) = (THUMB.0 as f32, THUMB.1 as f32);
    (w / rect.w.max(1e-3)).min(h / rect.h.max(1e-3))
}

/// `image` centered in a thumbnail's box (cut to it when larger).
pub fn boxed(image: &RgbaImage) -> RgbaImage {
    let (w, h) = THUMB;
    let mut out = RgbaImage { width: w, height: h, data: vec![0; (w * h * 4) as usize] };
    let ox = (w as i64 - image.width as i64) / 2;
    let oy = (h as i64 - image.height as i64) / 2;
    for y in 0..image.height as i64 {
        let ty = y + oy;
        if ty < 0 || ty >= h as i64 {
            continue;
        }
        for x in 0..image.width as i64 {
            let tx = x + ox;
            if tx < 0 || tx >= w as i64 {
                continue;
            }
            let from = ((y * image.width as i64 + x) * 4) as usize;
            let to = ((ty * w as i64 + tx) * 4) as usize;
            out.data[to..to + 4].copy_from_slice(&image.data[from..from + 4]);
        }
    }
    out
}

/// What a layer's thumbnail is drawn from: a paint layer's operations, a
/// vector layer's elements (their fingerprints), so an unchanged layer is
/// not drawn again.
fn signature(snapshot: &BoardSnapshot, layer: &pimble_board::Layer) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    layer.kind.as_str().hash(&mut h);
    match layer.kind {
        LayerKind::Paint => {
            for op in snapshot.paint_ops(&layer.id) {
                op.raw.id.hash(&mut h);
                op.fingerprint.hash(&mut h);
            }
        }
        _ => {
            for e in snapshot.elements.iter().filter(|e| e.layer == layer.id) {
                e.raw.id.hash(&mut h);
                e.fingerprint.hash(&mut h);
            }
        }
    }
    h.finish()
}

/// Draw one layer's thumbnail: a paint layer through `render_paint_layer`
/// (its tiles, from the shared cache), a vector layer through the regular
/// renderer limited to its elements. Both unclipped: a thumbnail shows what
/// the layer holds; the row's clip icon says it is clipped. `None` for an
/// empty layer.
fn draw_thumb(snapshot: &BoardSnapshot, pictures: &HashMap<String, Arc<Picture>>, layer: &pimble_board::Layer) -> Option<RgbaImage> {
    let rendered = match layer.kind {
        LayerKind::Paint => {
            let rect = snapshot.paint_bounds(&layer.id)?;
            pimble_board::render_paint_layer(snapshot, &layer.id, pictures, thumb_scale(&rect), None)?
        }
        LayerKind::Group | LayerKind::Other(_) => return None,
        LayerKind::Vector => {
            let ids: HashSet<String> = snapshot.elements.iter().filter(|e| e.layer == layer.id).map(|e| e.raw.id.clone()).collect();
            let rect = snapshot.elements.iter().filter(|e| e.layer == layer.id).map(|e| e.bounds).reduce(|a, b| a.union(&b))?;
            pimble_board::render_area_unclipped(snapshot, pictures, rect, thumb_scale(&rect), Some(&ids))?
        }
    };
    Some(boxed(&rendered.image))
}

/// A transparent thumbnail, for a layer not drawn yet or with nothing in it.
pub(super) fn blank_thumb() -> String {
    static BLANK: OnceLock<String> = OnceLock::new();
    BLANK.get_or_init(|| data_url(&RgbaImage { width: 1, height: 1, data: vec![0; 4] }).unwrap_or_default()).clone()
}

pub(super) fn data_url(image: &RgbaImage) -> Option<String> {
    use base64::Engine;
    let png = image.encode_png().ok()?;
    Some(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png)))
}

#[derive(Default)]
struct Thumbs {
    /// The board's version a timer waits on (`None`: no timer).
    waiting: Option<u64>,
    /// What each layer's thumbnail was drawn from.
    drawn: HashMap<String, u64>,
    /// A drawing is under way (on the desktop, off the UI thread).
    busy: bool,
}

thread_local! {
    static THUMBS: RefCell<[Option<Thumbs>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
}

fn with_thumbs<R>(pane: PaneId, f: impl FnOnce(&mut Thumbs) -> R) -> R {
    THUMBS.with(|t| f(t.borrow_mut()[pane.index()].get_or_insert_with(Thumbs::default)))
}

/// Pane `pane`'s board changed (or opened): its layer thumbnails are drawn
/// again once it has been left alone for a moment, never per frame.
pub(super) fn thumbs_due(store: AppStore, pane: PaneId) {
    let Some(version) = with_session(pane, |s| s.version) else { return };
    let arm = with_thumbs(pane, |t| {
        if t.waiting.is_some() {
            return false;
        }
        t.waiting = Some(version);
        true
    });
    if arm {
        let _ = set_timeout(THUMB_MS, move || thumbs_timer(store, pane));
    }
}

fn thumbs_timer(store: AppStore, pane: PaneId) {
    let Some(version) = with_session(pane, |s| s.version) else {
        with_thumbs(pane, |t| *t = Thumbs::default());
        return;
    };
    let waited = with_thumbs(pane, |t| t.waiting);
    if waited != Some(version) || with_session(pane, |s| !matches!(s.gesture, Gesture::Idle)).unwrap_or(false) {
        // Still changing: wait again.
        with_thumbs(pane, |t| t.waiting = Some(version));
        let _ = set_timeout(THUMB_MS, move || thumbs_timer(store, pane));
        return;
    }
    with_thumbs(pane, |t| t.waiting = None);
    // Nobody sees them while the layers panel is folded or hidden: they are
    // drawn when it opens (`ui::render_panels`).
    if !thumbs_shown(store, pane) {
        return;
    }
    draw_thumbs(store, pane);
}

/// Whether pane `pane`'s layers panel shows (unfolded, not hidden by Tab).
pub(super) fn thumbs_shown(store: AppStore, pane: PaneId) -> bool {
    let ui = store.pane(pane).board_ui;
    untracked(|| !ui.panels_hidden.get() && !ui.folded.get()[0])
}

/// Draw the thumbnails of the layers that changed since they were drawn.
fn draw_thumbs(store: AppStore, pane: PaneId) {
    if with_thumbs(pane, |t| std::mem::replace(&mut t.busy, true)) {
        // One at a time; the next change asks again.
        return;
    }
    let drawn = with_thumbs(pane, |t| t.drawn.clone());
    let job = with_session(pane, |s| {
        let snapshot = s.snapshot();
        let layers: Vec<(pimble_board::Layer, u64)> = snapshot
            .layers
            .iter()
            .filter(|l| !l.kind.is_group())
            .map(|l| (l.clone(), signature(snapshot, l)))
            .collect();
        let all: Vec<String> = layers.iter().map(|(l, _)| l.id.clone()).collect();
        let changed: Vec<(pimble_board::Layer, u64)> = layers.into_iter().filter(|(l, sig)| drawn.get(&l.id) != Some(sig)).collect();
        let (pictures, every) = pictures_of(snapshot);
        // A snapshot of its own to draw from (off the UI thread on the
        // desktop), drawing paint from the shared tiles.
        (s.board.snapshot().with_paint_cache(shared_cache()), pictures, changed, all, every)
    });
    let Some((snapshot, pictures, changed, all, every)) = job else {
        with_thumbs(pane, |t| t.busy = false);
        return;
    };
    let work = move || -> Vec<(String, u64, Option<String>)> {
        changed
            .iter()
            .map(|(l, sig)| (l.id.clone(), *sig, draw_thumb(&snapshot, &pictures, l).and_then(|img| data_url(&img))))
            .collect()
    };
    let done = move |results: Vec<(String, u64, Option<String>)>| {
        with_thumbs(pane, |t| {
            t.busy = false;
            t.drawn.retain(|id, _| all.contains(id));
            // Drawn without a picture not decoded yet: still due, drawn
            // again when it arrives (`boards::installed`). A fast first
            // draw showed a layer's strokes without its picture for good.
            if every {
                for (id, sig, _) in &results {
                    t.drawn.insert(id.clone(), *sig);
                }
            }
        });
        let ui = store.pane(pane).board_ui;
        let mut thumbs = untracked(|| ui.thumbs.get());
        let before = thumbs.clone();
        thumbs.retain(|id, _| all.contains(id));
        for (id, _, url) in results {
            match url {
                Some(url) => {
                    thumbs.insert(id, url);
                }
                None => {
                    thumbs.remove(&id);
                }
            }
        }
        if thumbs != before {
            ui.thumbs.set(thumbs);
        }
    };
    #[cfg(all(feature = "native", not(test)))]
    std::thread::spawn(move || {
        let results = work();
        run_on_main_thread(move || done(results));
    });
    #[cfg(any(not(feature = "native"), test))]
    done(work());
}

/// Forget pane `pane`'s thumbnails (its board closed).
pub(super) fn forget_thumbs(store: AppStore, pane: PaneId) {
    THUMBS.with(|t| t.borrow_mut()[pane.index()] = None);
    store.pane(pane).board_ui.thumbs.set_if_changed(HashMap::new());
}

/// The paint layers' operations that draw picture `src` (`pixels`), for a
/// view to draw again when that picture arrives.
pub(super) fn pixels_bounds(snapshot: &BoardSnapshot, src: &str) -> Vec<Rect> {
    snapshot
        .paint
        .iter()
        .filter(|op| matches!(&op.raw.body, PaintBody::Pixels(p) if p.src == src))
        .filter_map(|op| op.bounds)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_picker_offers_every_preset_and_the_highlighter_is_its_own_tool() {
        let names: Vec<&str> = preset_choices().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["round", "pencil", "ink", "airbrush", "marker", "charcoal", "watercolor", "highlighter"]);
        assert_eq!(tool_for_preset("highlighter"), Tool::Highlighter);
        assert_eq!(tool_for_preset("watercolor"), Tool::Brush);
        assert_eq!(preset_for(Tool::Highlighter, "ink").name, "highlighter");
        assert_eq!(preset_for(Tool::Brush, "ink").name, "ink");
        // The brush never paints with the highlighter's preset by its name,
        // and an unknown name is the round brush.
        assert_eq!(preset_for(Tool::Brush, "highlighter").name, "round");
        assert_eq!(preset_for(Tool::Brush, "nonsense").name, "round");
        assert_eq!(default_brushes().len(), PRESETS.len());
    }

    #[test]
    fn a_mouse_paints_at_full_pressure_and_a_pen_at_its_own() {
        assert_eq!(brush_pressure(SurfacePointerKind::Mouse, 0.5), Some(1.0));
        assert_eq!(brush_pressure(SurfacePointerKind::Mouse, 0.0), Some(1.0));
        assert_eq!(brush_pressure(SurfacePointerKind::Touch, 0.5), Some(1.0));
        assert_eq!(brush_pressure(SurfacePointerKind::Pen, 0.3), Some(0.3));
        assert_eq!(brush_pressure(SurfacePointerKind::Pen, 1.7), Some(1.0));
        assert_eq!(brush_pressure(SurfacePointerKind::Eraser, 0.6), Some(0.6));
        // A pen reading nothing keeps the last pressure.
        assert_eq!(brush_pressure(SurfacePointerKind::Pen, 0.0), None);
        assert_eq!(brush_pressure(SurfacePointerKind::Pen, f32::NAN), None);
    }

    #[test]
    fn brackets_step_the_size_within_its_limits() {
        assert_eq!(next_size(10.0, true), 12.0);
        assert_eq!(next_size(12.0, false), 10.0);
        // At least a whole unit either way.
        assert_eq!(next_size(2.0, true), 3.0);
        assert_eq!(next_size(3.0, false), 2.0);
        assert!(next_size(0.5, false) >= 0.5);
        assert_eq!(next_size(brush::MAX_SIZE, true), brush::MAX_SIZE);
        // Up then down comes back near where it was.
        let mut s = 8.0;
        for _ in 0..5 {
            s = next_size(s, true);
        }
        for _ in 0..5 {
            s = next_size(s, false);
        }
        assert!((s - 8.0).abs() <= 1.0, "{s}");
    }

    #[test]
    fn the_cursor_is_a_circle_of_the_brush_size() {
        let c = Point::new(10.0, 20.0);
        let curves = circle(c, 5.0);
        assert_eq!(curves.len(), 4);
        for seg in &curves {
            for end in [seg[0], seg[3]] {
                assert!(((end.x - c.x).hypot(end.y - c.y) - 5.0).abs() < 1e-4);
            }
        }
        // The segments join up.
        for i in 0..4 {
            assert_eq!(curves[i][3], curves[(i + 1) % 4][0]);
        }
    }

    #[test]
    fn a_thumbnail_is_centered_in_its_box() {
        let rect = Rect::new(0.0, 0.0, 480.0, 120.0);
        assert!((thumb_scale(&rect) - 0.1).abs() < 1e-6);
        let image = RgbaImage { width: 2, height: 2, data: vec![255; 16] };
        let out = boxed(&image);
        assert_eq!((out.width, out.height), THUMB);
        let at = |x: u32, y: u32| out.data[((y * THUMB.0 + x) * 4 + 3) as usize];
        assert_eq!(at(THUMB.0 / 2 - 1, THUMB.1 / 2 - 1), 255);
        assert_eq!(at(THUMB.0 / 2, THUMB.1 / 2), 255);
        assert_eq!(at(0, 0), 0);
        // Larger than the box: cut to it.
        let big = RgbaImage { width: 100, height: 100, data: vec![9; 40000] };
        assert_eq!(boxed(&big).data.len(), (THUMB.0 * THUMB.1 * 4) as usize);
    }
}
