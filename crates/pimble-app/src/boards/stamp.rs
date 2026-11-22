//! Symbols and scatter in the canvas (docs/DRAWING_WAVE6_CONTRACT.md
//! section 2, "Built: symbols and scatter"), the same code in both apps:
//!
//! - **Make Symbol** (the selection's menu): the selection becomes a
//!   symbol's definition and is replaced by one placement of it, where it was
//!   (`BoardTxn::make_symbol`); the Stamp then takes that symbol.
//! - **Edit Symbol** (one scatter selected): the pane shows the symbol's
//!   elements alone, in its own space (`ViewOptions::editing_symbol`), edited
//!   with every ordinary tool; what is drawn there goes into the symbol.
//!   Done (or Escape with nothing selected) goes back to the canvas where it
//!   was. Every placement of the symbol changes as it is edited.
//! - **The Stamp** (Shift+S, the Draw group): places the chosen symbol along
//!   the pointer, one scatter per stroke, or into the one selected scatter of
//!   that symbol; spacing and scatter are in symbol sizes, the jitter is
//!   seeded from the scatter's id, so a stroke replays the same.
//! - **Scatter in Shape** (one closed shape selected): fills it with the
//!   Stamp's symbol at its spacing, Poisson-disk, seeded from the shape.
//!
//! Appends go into the document at once and out about every 100 ms, as a
//! paint stroke's do; the undo clock is held from press to release, so a
//! stroke is one undo step.

use std::collections::HashMap;

use pimble_board::scatter::{self, FillMethod, Jitter, Placement as Placed, RegionFill, Stamp, StampWalker};

use super::*;
use crate::appearance::IconGlyph;

/// The Stamp's settings, one set for the window like every tool's.
#[derive(Clone, Copy)]
pub struct StampUi {
    /// The symbol it places (`None`: the canvas's newest).
    pub symbol: Signal<Option<String>>,
    /// Between placements, in symbol sizes.
    pub spacing: Signal<f64>,
    /// How far from the path a placement may stray, in symbol sizes.
    pub scatter: Signal<f64>,
    /// Every placement's scale.
    pub size: Signal<f64>,
    /// How much the scale varies, 0 to 0.9.
    pub size_jitter: Signal<f64>,
    /// How far the rotation varies either way, degrees.
    pub rotation_jitter: Signal<f64>,
    /// How far each placement moves toward the well's stroke color, 0 to 1.
    pub tint: Signal<f64>,
    /// How much each placement's color varies, 0 to 1.
    pub tint_jitter: Signal<f64>,
}

impl StampUi {
    pub fn new() -> Self {
        Self {
            symbol: Signal::new(None),
            spacing: Signal::new(1.0),
            scatter: Signal::new(0.3),
            size: Signal::new(1.0),
            size_jitter: Signal::new(0.2),
            rotation_jitter: Signal::new(8.0),
            tint: Signal::new(0.0),
            tint_jitter: Signal::new(0.3),
        }
    }
}

impl Default for StampUi {
    fn default() -> Self {
        Self::new()
    }
}

/// One symbol as the picker shows it: its id, name and a thumbnail (a
/// `data:` URL).
#[derive(Debug, Clone, PartialEq)]
pub struct SymbolRow {
    pub id: String,
    pub name: String,
    pub thumb: String,
}

/// A stamp stroke under way: the scatter it places into (made by this
/// stroke, or the selected one it appends to) and the walker that places.
pub struct Stamping {
    pub id: String,
    pub walker: StampWalker,
    /// The scatter was made by this stroke (a cancel deletes it).
    pub made: bool,
}

/// What a press says when the canvas has no symbol to stamp.
pub const NO_SYMBOL: &str = "There is no symbol to stamp yet: draw something, select it and choose Make Symbol in the selection's ··· menu.";
const EDITING: &str = "Finish editing the symbol first (Done, at the top).";
/// The thumbnails' size, pixels.
const THUMB_PX: u32 = 40;

/// Whether pane `pane` is editing a symbol.
pub(super) fn editing(pane: PaneId) -> bool {
    with_session(pane, |s| s.options.editing_symbol.is_some()).unwrap_or(false)
}

/// The symbol the Stamp places on pane `pane`'s canvas: the chosen one when
/// the canvas has it, else its newest (which becomes the choice).
pub(super) fn chosen_symbol(store: AppStore, pane: PaneId) -> Option<String> {
    let chosen = untracked(|| store.board_style.stamp.symbol.get());
    let found = with_session(pane, |s| {
        let snapshot = s.snapshot();
        match chosen.as_deref().and_then(|c| snapshot.symbol(c)) {
            Some(def) => Some(def.id.clone()),
            None => snapshot.symbols.last().map(|d| d.id.clone()),
        }
    })
    .flatten()?;
    if chosen.as_deref() != Some(found.as_str()) {
        store.board_style.stamp.symbol.set(Some(found.clone()));
    }
    Some(found)
}

/// The Stamp's settings in board units for `symbol` (spacing and scatter
/// are in its sizes).
fn stamp_of(store: AppStore, pane: PaneId, symbol: &str) -> Stamp {
    let st = store.board_style;
    let across = with_session(pane, |s| s.snapshot().symbol(symbol).and_then(|d| d.bounds).map(|b| b.w.max(b.h))).flatten().unwrap_or(40.0).max(1.0);
    untracked(|| {
        let size = st.stamp.size.get() as f32;
        Stamp {
            spacing: (st.stamp.spacing.get() as f32 * across * size).max(0.5),
            scatter: st.stamp.scatter.get() as f32 * across * size,
            jitter: jitter_of(store),
        }
    })
}

fn jitter_of(store: AppStore) -> Jitter {
    let st = store.board_style;
    untracked(|| {
        let amount = st.stamp.tint.get().clamp(0.0, 1.0);
        let tint = match Color::parse(&st.stroke.get()) {
            Some(c) if amount > 0.0 => Color { a: (amount * 255.0).round() as u8, ..c },
            _ => Color::TRANSPARENT,
        };
        Jitter {
            size: st.stamp.size.get() as f32,
            size_jitter: st.stamp.size_jitter.get() as f32,
            rotation_jitter: st.stamp.rotation_jitter.get() as f32,
            tint,
            tint_jitter: st.stamp.tint_jitter.get() as f32,
        }
    })
}

// ── The Stamp ───────────────────────────────────────────────────────────────

/// A press with the Stamp at board point `p`.
pub(super) fn press(store: AppStore, pane: PaneId, p: Point) {
    if editing(pane) {
        crate::events::show_notice(store, EDITING.to_string());
        return;
    }
    if !writable_or_say(store, pane) {
        return;
    }
    let Some(symbol) = chosen_symbol(store, pane) else {
        crate::events::show_notice(store, NO_SYMBOL.to_string());
        return;
    };
    // Into the one selected scatter of this symbol, else a new one.
    let into = with_session(pane, |s| {
        let [id] = s.selected.as_slice() else { return None };
        let e = s.snapshot().get(id)?;
        let sc = e.raw.scatter.as_ref().filter(|sc| sc.symbol == symbol)?;
        s.editable(id).then(|| (id.clone(), sc.placements.len()))
    })
    .flatten();
    let layer = match &into {
        Some(_) => None,
        None => match tools::target_layer(store, pane) {
            Some(layer) => Some(layer),
            None => return,
        },
    };
    let stamp = stamp_of(store, pane, &symbol);
    new_undo_step(pane);
    points::hold_clock(pane, true);
    let begun = local_edit_said(store, pane, |t| {
        let (id, count, made) = match &into {
            Some((id, n)) => (id.clone(), *n, false),
            None => (t.add_scatter(layer.as_deref(), &symbol, &[], None)?, 0, true),
        };
        let mut walker = StampWalker::new(stamp, scatter::seed_of(&id)).after(count as u64);
        let first = walker.walk(p);
        t.append_placements(&id, &first)?;
        Ok(Stamping { id, walker, made })
    });
    let Some(stamping) = begun else {
        points::hold_clock(pane, false);
        return;
    };
    with_session(pane, |s| {
        if stamping.made {
            s.selected.clear();
        }
        s.gesture = Gesture::Stamp(stamping);
    });
    after_change(store, pane);
}

/// The pointer moved while stamping: what the path to `p` places.
pub(super) fn walk(stamping: &mut Stamping, p: Point) -> Option<(String, Vec<Placed>)> {
    let placed = stamping.walker.walk(p);
    (!placed.is_empty()).then(|| (stamping.id.clone(), placed))
}

/// Placements made by a move: into the document at once, out with the
/// others about every 100 ms.
pub(super) fn moved(store: AppStore, pane: PaneId, id: String, placed: Vec<Placed>) {
    paint::held_edit(store, pane, |t| t.append_placements(&id, &placed));
    paint::flush(store, pane, false);
    after_change(store, pane);
}

/// The stroke ended at `p`: the last placements go in, everything held goes
/// out, the undo step closes; the scatter is selected.
pub(super) fn released(store: AppStore, pane: PaneId, mut stamping: Stamping, p: Point) {
    if let Some((id, placed)) = walk(&mut stamping, p) {
        paint::held_edit(store, pane, |t| t.append_placements(&id, &placed));
    }
    paint::flush(store, pane, true);
    points::hold_clock(pane, false);
    tools::select(store, pane, vec![stamping.id]);
}

/// The stroke was taken back: a scatter it made goes (one it appended to
/// keeps what was placed; undo takes that back).
pub(super) fn cancelled(store: AppStore, pane: PaneId, stamping: Stamping) {
    if stamping.made {
        paint::held_edit(store, pane, |t| t.delete_element(&stamping.id));
    }
    paint::flush(store, pane, true);
    points::hold_clock(pane, false);
}

// ── Making and editing symbols ──────────────────────────────────────────────

/// Make Symbol: the selection becomes a symbol, replaced by one placement of
/// it where it was; the Stamp takes it.
pub(super) fn make_symbol(store: AppStore, pane: PaneId) {
    if editing(pane) {
        crate::events::show_notice(store, EDITING.to_string());
        return;
    }
    let ids = with_session(pane, |s| s.selected.clone()).unwrap_or_default();
    if ids.is_empty() {
        crate::events::show_notice(store, scatter::NO_SYMBOL_ELEMENTS.to_string());
        return;
    }
    new_undo_step(pane);
    let Some(made) = local_edit_said(store, pane, |t| t.make_symbol(&ids, "", false)) else { return };
    store.board_style.stamp.symbol.set(Some(made.symbol.clone()));
    let name = with_session(pane, |s| s.snapshot().symbol(&made.symbol).map(|d| d.name.clone())).flatten().unwrap_or_default();
    if let Some(instance) = made.instance {
        tools::select(store, pane, vec![instance]);
    }
    crate::events::show_notice(store, format!("Made \"{name}\": the Stamp (Shift+S) places it, Edit Symbol changes every placement."));
}

/// Edit Symbol: the selected scatter's symbol, alone, in its own space.
pub(super) fn edit_symbol(store: AppStore, pane: PaneId) {
    let symbol = with_session(pane, |s| {
        let [id] = s.selected.as_slice() else { return None };
        s.snapshot().get(id)?.raw.scatter.as_ref().map(|sc| sc.symbol.clone())
    })
    .flatten();
    match symbol {
        Some(symbol) => enter(store, pane, &symbol),
        None => crate::events::show_notice(store, "Select one scatter (a placed symbol) to edit its symbol.".to_string()),
    }
}

/// Show symbol `symbol` alone in pane `pane`, fitted.
pub(super) fn enter(store: AppStore, pane: PaneId, symbol: &str) {
    super::tools::cancel_gesture(store, pane);
    let name = with_session(pane, |s| {
        let name = s.snapshot().symbol(symbol)?.name.clone();
        if s.symbol_camera.is_none() {
            s.symbol_camera = Some((s.view.center(), s.view.zoom()));
        }
        s.options.editing_symbol = Some(symbol.to_string());
        let options = s.options.clone();
        s.view.set_options(options, &s.board);
        s.selected.clear();
        if let Some(r) = s.snapshot().visible_extent() {
            let margin = 0.3 * s.view.size().0.min(s.view.size().1) as f32;
            s.view.fit(&r, margin);
        }
        s.view.settle(&picture_source());
        Some(name)
    })
    .flatten();
    let Some(name) = name else { return };
    let ui = store.pane(pane).board_ui;
    ui.editing_symbol.set(Some(name));
    ui.layer.set(None);
    if untracked(|| store.board_tool.get()) == Tool::Stamp {
        store.board_tool.set(Tool::Select);
    }
    after_change(store, pane);
}

/// Done: back to the canvas, where it was.
pub(super) fn done(store: AppStore, pane: PaneId) {
    super::tools::cancel_gesture(store, pane);
    let left = with_session(pane, |s| {
        s.options.editing_symbol.take()?;
        let options = s.options.clone();
        s.view.set_options(options, &s.board);
        s.selected.clear();
        if let Some((center, zoom)) = s.symbol_camera.take() {
            s.view.set_camera(center, zoom);
        }
        s.view.settle(&picture_source());
        Some(())
    })
    .flatten();
    store.pane(pane).board_ui.editing_symbol.set(None);
    if left.is_some() {
        after_change(store, pane);
    }
}

/// Scatter in Shape: the one selected closed shape filled with the Stamp's
/// symbol at its spacing and jitter, seeded from the shape.
pub(super) fn scatter_in_shape(store: AppStore, pane: PaneId) {
    if editing(pane) {
        crate::events::show_notice(store, EDITING.to_string());
        return;
    }
    let shape = with_session(pane, |s| {
        let [id] = s.selected.as_slice() else { return None };
        let e = s.snapshot().get(id)?;
        pimble_board::clip::is_closed(&e.raw.kind, &e.raw.geom).then(|| id.clone())
    })
    .flatten();
    let Some(shape) = shape else {
        crate::events::show_notice(store, scatter::NEEDS_CLOSED_SHAPE.to_string());
        return;
    };
    let Some(symbol) = chosen_symbol(store, pane) else {
        crate::events::show_notice(store, NO_SYMBOL.to_string());
        return;
    };
    let stamp = stamp_of(store, pane, &symbol);
    let fill = RegionFill { spacing: stamp.spacing, position_jitter: 0.8, method: FillMethod::Poisson, seed: scatter::seed_of(&shape), jitter: stamp.jitter };
    new_undo_step(pane);
    if let Some(id) = local_edit_said(store, pane, |t| t.scatter_in_shape(&shape, &symbol, &fill, None)) {
        tools::select(store, pane, vec![id]);
    }
}

/// A symbol picked in the picker: the Stamp's from now on, and the selected
/// scatters' too.
fn pick(store: AppStore, pane: PaneId, symbol: String) {
    store.board_style.stamp.symbol.set(Some(symbol.clone()));
    let scatters: Vec<String> = with_session(pane, |s| {
        let snapshot = s.snapshot();
        s.selected.iter().filter(|id| snapshot.get(id).is_some_and(|e| e.raw.kind == Kind::Scatter && e.raw.scatter.as_ref().is_some_and(|sc| sc.symbol != symbol))).cloned().collect()
    })
    .unwrap_or_default();
    if !scatters.is_empty() {
        new_undo_step(pane);
        local_edit_said(store, pane, |t| {
            for id in &scatters {
                t.set_scatter_symbol(id, &symbol)?;
            }
            Ok(())
        });
    }
    store.pane(pane).board_ui.popup.set(Popup::None);
}

// ── What the chrome shows ───────────────────────────────────────────────────

thread_local! {
    /// Thumbnails by (symbol id, its hash).
    static THUMBS: RefCell<HashMap<(String, u64), String>> = RefCell::new(HashMap::new());
}

/// The canvas's symbols to the picker (a thumbnail drawn again only when a
/// symbol changed), and the name of the one being edited.
pub(super) fn publish(store: AppStore, pane: PaneId) {
    let rows = with_session(pane, |s| {
        let snapshot = s.snapshot();
        snapshot
            .symbols
            .iter()
            .map(|d| {
                let key = (d.id.clone(), d.hash);
                let thumb = THUMBS.with(|t| t.borrow().get(&key).cloned()).unwrap_or_else(|| {
                    let url = scatter::thumbnail(d, THUMB_PX, &pimble_board::NoPictures).and_then(|img| paint::data_url(&img)).unwrap_or_else(paint::blank_thumb);
                    THUMBS.with(|t| {
                        let mut t = t.borrow_mut();
                        if t.len() > 256 {
                            t.clear();
                        }
                        t.insert(key, url.clone());
                    });
                    url
                });
                SymbolRow { id: d.id.clone(), name: d.name.clone(), thumb }
            })
            .collect::<Vec<_>>()
    });
    let ui = store.pane(pane).board_ui;
    if let Some(rows) = rows {
        ui.symbols.set_if_changed(rows);
    }
}

/// The context bar's Stamp controls (the tool's, and a selected scatter's):
/// the symbol picker and the settings button; and their popups.
pub(super) fn render_controls(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> (NodeHandle, NodeHandle) {
    let ui = store.pane(pane).board_ui;
    let st = store.board_style.stamp;
    // The symbol the face shows: the selected scatter's, else the Stamp's.
    let shown_symbol = move || -> Option<SymbolRow> {
        let rows = ui.symbols.get();
        let chosen = st.symbol.get();
        let _ = ui.selected.get();
        let selected = untracked(|| with_session(pane, |s| {
            let [id] = s.selected.as_slice() else { return None };
            s.snapshot().get(id)?.raw.scatter.as_ref().map(|sc| sc.symbol.clone())
        }))
        .flatten();
        let want = selected.or(chosen);
        rows.iter().find(|r| Some(&r.id) == want.as_ref()).or(rows.last()).cloned()
    };
    let face = rsx! {
        span { class: "pimble-board__face-wrap pimble-board__symbol-face",
            img {
                class: "pimble-board__symbol-thumb",
                src: move || shown_symbol().map(|r| r.thumb).unwrap_or_else(paint::blank_thumb),
            }
            span { class: "pimble-board__face-text", {move || shown_symbol().map(|r| r.name).unwrap_or_else(|| "No symbol".to_string())} }
        }
    };
    let symbol_btn = rail::picker_button(__scope, ui, || "Symbol: the one the Stamp places (a selected scatter's changes to it)".to_string(), Popup::Symbol, "pimble-board__chooser--symbol", face);
    let settings_face = rsx! { span { class: "pimble-board__face", IconGlyph { name: rinch_tabler_icons::TablerIcon::Adjustments.name().to_string() } } };
    let settings_btn = rail::picker_button(
        __scope,
        ui,
        move || format!("Stamp: one every {:.1}× its size, size varies ±{:.0}% (spacing, scatter, size, jitter, tint)", st.spacing.get(), st.size_jitter.get() * 100.0),
        Popup::Stamp,
        "",
        settings_face,
    );
    let bar = rsx! {
        div { class: "pimble-board__ctx-group",
            {symbol_btn}
            div { class: "pimble-board__ctx-group", style: {move || if ui.selected.get() == 0 { "" } else { "display: none;" }}, {settings_btn} }
        }
    };
    let rows: Vec<NodeHandle> = vec![
        slider_row(__scope, "Spacing", st.spacing, (0.2, 5.0, 0.1), |v| format!("{v:.1}×")),
        slider_row(__scope, "Scatter", st.scatter, (0.0, 5.0, 0.1), |v| format!("{v:.1}×")),
        slider_row(__scope, "Size", st.size, (0.1, 5.0, 0.05), |v| format!("{:.0}%", v * 100.0)),
        slider_row(__scope, "Size jitter", st.size_jitter, (0.0, 0.9, 0.05), |v| format!("±{:.0}%", v * 100.0)),
        slider_row(__scope, "Rotation", st.rotation_jitter, (0.0, 180.0, 1.0), |v| format!("±{v:.0}°")),
        slider_row(__scope, "Tint", st.tint, (0.0, 1.0, 0.05), |v| if v <= 0.0 { "Off".to_string() } else { format!("{:.0}%", v * 100.0) }),
        slider_row(__scope, "Tint jitter", st.tint_jitter, (0.0, 1.0, 0.05), |v| format!("{:.0}%", v * 100.0)),
    ];
    let shown = move |on: bool| if on { "" } else { "display: none;" };
    let popups = rsx! {
        div {
            div { class: "pimble-float pimble-board__options pimble-board__symbols", style: {move || shown(ui.popup.get() == Popup::Symbol)},
                for (key, class_id, click_id, title, name, thumb) in ui.symbols.get().into_iter().map(|r| (format!("{} {} {}", r.id, r.name, r.thumb.len()), r.id.clone(), r.id.clone(), r.name.clone(), r.name.clone(), r.thumb.clone())) {
                    div {
                        key: key,
                        class: {move || if shown_symbol().is_some_and(|r| r.id == class_id) { "pimble-board-chip pimble-board-chip--active pimble-board__symbol" } else { "pimble-board-chip pimble-board__symbol" }},
                        title: title,
                        onclick: move || pick(store, pane, click_id.clone()),
                        img { class: "pimble-board__symbol-thumb", src: thumb }
                        span { {name} }
                    }
                }
                div {
                    class: "pimble-board__ctx-hint",
                    style: {move || shown(ui.symbols.get().is_empty())},
                    "No symbols yet: select a drawing and choose Make Symbol in the ··· menu."
                }
            }
            div { class: "pimble-float pimble-board__options pimble-board__stack", style: {move || shown(ui.popup.get() == Popup::Stamp)},
                div { class: "pimble-board__ctx-hint", "Spacing and scatter are in symbol sizes; the tint is the stroke color's." }
                {rows}
            }
        }
    };
    (bar, popups)
}

/// A slider row of the Stamp's settings popup.
fn slider_row(__scope: &mut RenderScope, label: &'static str, value: Signal<f64>, (min, max, step): (f64, f64, f64), shown: fn(f64) -> String) -> NodeHandle {
    rsx! {
        div { class: "pimble-board__row",
            span { class: "pimble-board__ctx-label", {label} }
            div { class: "pimble-board__ctx-slider",
                Slider {
                    min: min,
                    max: max,
                    step: step,
                    size: "xs",
                    value_signal: Some(value),
                    onchange: move |v: f64| value.set(v),
                }
            }
            span { class: "pimble-board__ctx-number", {move || shown(value.get())} }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stamp_is_shift_s_in_the_draw_group() {
        assert_eq!(Tool::from_chord("S", true), Some(Tool::Stamp));
        assert_eq!(Tool::from_key("s"), Some(Tool::Sticky), "plain S is still the sticky note");
        assert_eq!(Tool::Stamp.key_label(), "Shift+S");
        let at = rail::RAIL.iter().position(|s| *s == rail::Slot::Tool(Tool::Stamp)).unwrap();
        assert_eq!(rail::RAIL[at - 1], rail::Slot::Tool(Tool::Pencil), "beside the Pencil");
    }
}
