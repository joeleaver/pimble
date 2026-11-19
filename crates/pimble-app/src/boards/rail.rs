//! The canvas's floating chrome (docs/DRAWING_CONTRACT.md section 4): the
//! tool rail on the left with its flyouts, the context bar at the top (the
//! tool's or the selection's options), the color well in the bottom left,
//! and the zoom corner (zoom, view switches, export, the map) in the bottom
//! right. Everything floats over the canvas, which fills the pane.
//!
//! The pure parts are tested here: the rail's slots and flyouts (every tool
//! is on the rail exactly once), the faces flyouts remember, which controls
//! the context bar offers for a tool or a selection, swapping the well's
//! colors and the recent colors.

use super::export::Format;
use super::gradient;
use super::paint;
use super::tools::{Align2, Order};
use pimble_board::BrushSettings;
use super::*;
use pimble_board::{CombineOp, FlipAxis, GradientKind, Paint};
use crate::appearance::IconGlyph;

// ── The rail's model ────────────────────────────────────────────────────────

/// One place on the rail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Tool(Tool),
    /// A button showing one of `FLYOUTS[i]`, which opens the rest.
    Flyout(usize),
    /// Insert pictures.
    Picture,
    /// A gap between groups.
    Gap,
}

/// The flyouts' tools, the first of each its face until another is used.
pub const FLYOUTS: [&[Tool]; 5] = [
    // Shapes (Draw).
    &[Tool::Rect, Tool::Ellipse, Tool::Diamond, Tool::Polygon, Tool::Star, Tool::Line, Tool::Arrow],
    // More diagram tools.
    &[Tool::Connector, Tool::Step, Tool::Frame],
    // The brush and its highlighter preset (Paint).
    &[Tool::Brush, Tool::Highlighter],
    // The fill and the brushes that work pixels already there (Paint).
    &[Tool::Fill, Tool::Smudge, Tool::Blur],
    // The paint selections (Paint).
    &[Tool::SelectRect, Tool::SelectLasso],
];

/// The rail, top to bottom: Select, Direct select and Hand; Draw (the Pen,
/// the Pencil, the Stamp, the shapes); Paint (the brush and the highlighter, the
/// eraser, the fill with smudge and blur, the paint selections, the
/// eyedropper); Diagram (text, sticky notes, the other diagram tools,
/// discussion pins); pictures.
pub const RAIL: [Slot; 20] = [
    Slot::Tool(Tool::Select),
    Slot::Tool(Tool::DirectSelect),
    Slot::Tool(Tool::Hand),
    Slot::Gap,
    Slot::Tool(Tool::Pen),
    Slot::Tool(Tool::Pencil),
    Slot::Tool(Tool::Stamp),
    Slot::Flyout(0),
    Slot::Gap,
    Slot::Flyout(2),
    Slot::Tool(Tool::Eraser),
    Slot::Flyout(3),
    Slot::Flyout(4),
    Slot::Tool(Tool::Eyedropper),
    Slot::Gap,
    Slot::Tool(Tool::Text),
    Slot::Tool(Tool::Sticky),
    Slot::Flyout(1),
    Slot::Tool(Tool::Pin),
    Slot::Picture,
];

/// Each flyout's first face.
pub fn default_faces() -> Vec<Tool> {
    FLYOUTS.iter().map(|f| f[0]).collect()
}

/// The flyout `tool` is in.
pub fn flyout_of(tool: Tool) -> Option<usize> {
    FLYOUTS.iter().position(|f| f.contains(&tool))
}

/// The faces once `tool` is used (from the rail, a flyout or its letter):
/// its flyout shows it from now on.
pub fn remember_face(faces: &[Tool], tool: Tool) -> Vec<Tool> {
    let mut faces: Vec<Tool> = if faces.len() == FLYOUTS.len() { faces.to_vec() } else { default_faces() };
    if let Some(i) = flyout_of(tool) {
        faces[i] = tool;
    }
    faces
}

/// The rail button's tooltip: the tool's name and letter.
pub fn tip(tool: Tool) -> String {
    format!("{} ({})", tool.name(), tool.key_label())
}

// ── The context bar's model ─────────────────────────────────────────────────

/// Which style controls the context bar shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Controls {
    pub stroke: bool,
    pub fill: bool,
    pub width: bool,
    pub dash: bool,
    pub heads: bool,
    pub font: bool,
    pub align: bool,
    pub opacity: bool,
    /// Stroke ends and corners.
    pub cap: bool,
    pub join: bool,
    /// A rectangle's corner radius.
    pub radius: bool,
    /// A polygon's sides.
    pub sides: bool,
    /// A star's points and inner radius.
    pub star: bool,
    /// A path's fill rule.
    pub rule: bool,
    /// The Pencil's smoothing (the tool's, never a selection's).
    pub smoothing: bool,
    /// A paint tool's: the color it paints with (the well's front square),
    /// its preset, size, opacity, flow and hardness. The eraser has a size.
    pub color: bool,
    pub preset: bool,
    pub size: bool,
    pub brush_opacity: bool,
    pub flow: bool,
    pub hardness: bool,
    /// The Fill's tolerance, and contiguous or everywhere.
    pub tolerance: bool,
    pub contiguous: bool,
    /// Smudge's and Blur's strength.
    pub strength: bool,
    /// The Stamp's symbol and settings (a selected scatter's symbol).
    pub stamp: bool,
}

impl Controls {
    pub fn any(&self) -> bool {
        self.stroke
            || self.fill
            || self.width
            || self.dash
            || self.heads
            || self.font
            || self.align
            || self.opacity
            || self.cap
            || self.join
            || self.radius
            || self.sides
            || self.star
            || self.rule
            || self.smoothing
            || self.color
            || self.preset
            || self.size
            || self.brush_opacity
            || self.flow
            || self.hardness
            || self.tolerance
            || self.contiguous
            || self.strength
            || self.stamp
    }

    /// Any of the pickers (everything but the two swatches).
    pub fn pickers(&self) -> bool {
        Controls { stroke: false, fill: false, color: false, ..*self }.any()
    }

    fn or(self, o: Controls) -> Controls {
        Controls {
            stroke: self.stroke || o.stroke,
            fill: self.fill || o.fill,
            width: self.width || o.width,
            dash: self.dash || o.dash,
            heads: self.heads || o.heads,
            font: self.font || o.font,
            align: self.align || o.align,
            opacity: self.opacity || o.opacity,
            cap: self.cap || o.cap,
            join: self.join || o.join,
            radius: self.radius || o.radius,
            sides: self.sides || o.sides,
            star: self.star || o.star,
            rule: self.rule || o.rule,
            smoothing: self.smoothing || o.smoothing,
            color: self.color || o.color,
            preset: self.preset || o.preset,
            size: self.size || o.size,
            brush_opacity: self.brush_opacity || o.brush_opacity,
            flow: self.flow || o.flow,
            hardness: self.hardness || o.hardness,
            tolerance: self.tolerance || o.tolerance,
            contiguous: self.contiguous || o.contiguous,
            strength: self.strength || o.strength,
            stamp: self.stamp || o.stamp,
        }
    }
}

/// What an element of `kind` can change (what `tools::new_style` and
/// `tools::apply_style` write for it).
pub fn controls_of(kind: &Kind) -> Controls {
    let none = Controls::default();
    match kind {
        Kind::Pen => Controls { stroke: true, width: true, dash: true, opacity: true, ..none },
        Kind::Highlighter => Controls { stroke: true, width: true, opacity: true, ..none },
        Kind::Line => Controls { stroke: true, width: true, dash: true, opacity: true, cap: true, ..none },
        Kind::Arrow | Kind::Connector => Controls { stroke: true, width: true, dash: true, heads: true, opacity: true, cap: true, ..none },
        Kind::Rect | Kind::Ellipse | Kind::Diamond | Kind::Polygon | Kind::Star => Controls {
            stroke: true,
            fill: true,
            width: true,
            dash: true,
            font: true,
            align: true,
            opacity: true,
            cap: true,
            join: true,
            radius: *kind == Kind::Rect,
            sides: *kind == Kind::Polygon,
            star: *kind == Kind::Star,
            ..none
        },
        Kind::Path => Controls { stroke: true, fill: true, width: true, dash: true, opacity: true, cap: true, join: true, rule: true, ..none },
        Kind::Text | Kind::Callout => Controls { stroke: true, fill: true, font: true, align: true, opacity: true, ..none },
        // A sticky note's color is its fill.
        Kind::Sticky => Controls { fill: true, font: true, align: true, opacity: true, ..none },
        Kind::Step => Controls { stroke: true, font: true, opacity: true, ..none },
        Kind::Spotlight => Controls { opacity: true, ..none },
        // A scatter: its opacity, and its symbol and tint (the stamp's row).
        Kind::Scatter => Controls { opacity: true, stamp: true, ..none },
        Kind::Frame | Kind::Picture | Kind::Group | Kind::Card | Kind::Other(_) => none,
    }
}

/// What a selection of these kinds can change: whatever any of them can.
pub fn controls_for(kinds: &[Kind]) -> Controls {
    kinds.iter().fold(Controls::default(), |c, k| c.or(controls_of(k)))
}

/// What the context bar offers for `tool` with nothing selected: what it
/// makes takes, and the Pencil's smoothing. The Pen makes paths, whose fill
/// rule is only a selected path's.
pub fn controls_of_tool(tool: Tool) -> Controls {
    match tool {
        Tool::Brush | Tool::Highlighter => {
            return Controls { color: true, preset: true, size: true, brush_opacity: true, flow: true, hardness: true, ..Controls::default() };
        }
        Tool::Eraser => return Controls { size: true, ..Controls::default() },
        Tool::Fill => return Controls { color: true, tolerance: true, contiguous: true, ..Controls::default() },
        Tool::Smudge | Tool::Blur => return Controls { size: true, strength: true, ..Controls::default() },
        Tool::Stamp => return Controls { stamp: true, ..Controls::default() },
        _ => {}
    }
    let mut c = tool.kind().map(|k| controls_of(&k)).unwrap_or_default();
    c.rule = false;
    c.smoothing = tool == Tool::Pencil;
    c
}

/// The Pencil's smoothing steps: how far (logical pixels at the zoom it is
/// drawn at) the fitted path may stray from the freehand points.
pub const SMOOTHING: [(&str, f32); 3] = [("Low", 0.75), ("Medium", 2.0), ("High", 5.0)];
/// The Pencil's first smoothing.
pub const DEFAULT_SMOOTHING: f32 = 2.0;
/// Letter spacing steps (em) the text options offer.
pub const TRACKINGS: [(&str, f32); 5] = [("Tight", -0.05), ("Normal", 0.0), ("Wide", 0.1), ("Wider", 0.25), ("Widest", 0.5)];
/// Halo widths the text options offer, as fractions of the font size
/// (written as board units: a halo stays as wide when the text is resized
/// by its font size later only if picked again).
pub const HALOS: [(&str, f32); 4] = [("Off", 0.0), ("S", 0.06), ("M", 0.12), ("L", 0.2)];
/// Halo colors offered: white, black, and map paper.
pub const HALO_COLORS: [(&str, &str); 3] = [("#ffffff", "White"), ("#000000", "Black"), ("#f4ecd8", "Paper")];

/// The step of [`HALOS`] nearest a halo `fraction` of the font size wide.
pub fn halo_step(fraction: f32) -> f32 {
    HALOS.iter().skip(1).map(|(_, v)| *v).min_by(|a, b| (a - fraction).abs().total_cmp(&(b - fraction).abs())).unwrap_or(0.0)
}

/// The polygon sides and star points the context bar offers.
pub const SIDES: [u32; 7] = [3, 4, 5, 6, 8, 10, 12];
/// The miter limits offered while corners are mitered (4 is the default).
pub const MITERS: [f32; 5] = [1.0, 2.0, 4.0, 8.0, 10.0];
pub const STAR_POINTS: [u32; 6] = [4, 5, 6, 7, 8, 12];

/// What the context bar says when the tool has nothing to set.
pub fn hint(tool: Tool) -> &'static str {
    match tool {
        Tool::Select => "Click to select, drag to select several",
        Tool::DirectSelect => "Click a path or shape to edit its points; double-click adds or smooths one",
        Tool::Hand => "Drag to move around (Space held does it with any tool)",
        Tool::Eraser => "Drag across marks to erase them",
        Tool::Eyedropper => "Click to pick the color under the pointer (Alt with a brush does it too)",
        Tool::Pin => "Click to start a discussion there",
        Tool::Frame => "Drag out a frame; what it covers goes with it",
        Tool::Stamp => "Drag to place the symbol along the way",
        Tool::SelectRect => "Drag over paint to select it, then move, scale or turn it",
        Tool::SelectLasso => "Draw round paint to select it, then move, scale or turn it",
        _ => "",
    }
}

// ── The color well's model ──────────────────────────────────────────────────

/// The well's two squares.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Well {
    #[default]
    Stroke,
    Fill,
}

/// Swap stroke and fill (X). With no fill, the stroke color becomes the fill
/// too and the stroke stays: a stroke cannot be nothing.
pub fn swapped(stroke: &str, fill: Option<&str>) -> (String, Option<String>) {
    match fill {
        Some(f) => (f.to_string(), Some(stroke.to_string())),
        None => (stroke.to_string(), Some(stroke.to_string())),
    }
}

/// How many recent colors the well keeps.
pub const RECENT: usize = 8;

/// The recent colors once `color` is picked: it first, no repeats.
pub fn remember_recent(recent: &[String], color: &str) -> Vec<String> {
    let mut out = vec![color.to_string()];
    out.extend(recent.iter().filter(|c| !c.eq_ignore_ascii_case(color)).take(RECENT - 1).cloned());
    out
}

// ── Keys ────────────────────────────────────────────────────────────────────

/// Who hears the board's keys, by where the last press on the board was.
/// A press on the floating chrome takes the keyboard from the canvas's
/// surface (rinch gives a surface the keyboard back only on a press on it),
/// so the chrome listens through the keyboard interceptor instead.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Keys {
    /// The surface hears them itself.
    #[default]
    Canvas,
    /// The rail, the context bar, the color well's squares, the zoom
    /// corner: tool letters, X and Tab act as on the canvas.
    Chrome,
    /// The layers panel: Ctrl+G groups the picked layers.
    Layers,
    /// Somewhere with inputs (the meaning panel, the picker's hex box):
    /// every key is theirs.
    Typing,
    /// The well's stop bar: Delete or Backspace removes the picked stop
    /// (a field that holds the focus keeps its keys, as everywhere).
    Stops,
}

thread_local! {
    /// When the stop bar was last pressed (ms): the well popup's own press
    /// handler, which the browser runs after the bar's, leaves the keys with
    /// the bar.
    static STOP_PRESS: std::cell::Cell<f64> = const { std::cell::Cell::new(f64::NEG_INFINITY) };
}

/// What a key does while `keys` has the board's keys (`None`: not ours).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAct {
    Tool(Tool),
    Swap,
    TogglePanels,
    GroupLayers,
    /// Shift+H or Shift+V: flip the selection (with nothing selected, the
    /// letter's tool).
    Flip(FlipAxis),
    /// `[` (false) or `]` (true): a paint tool's size.
    Bracket(bool),
    /// Delete on the stop bar: the picked stop goes.
    RemoveStop,
}

/// The chrome's key judgement, pure: plain letters, X and Tab from the
/// chrome; Ctrl+G from the layers panel.
pub fn key_act(keys: Keys, key: &str, ctrl: bool, shift: bool, alt: bool) -> Option<KeyAct> {
    match keys {
        Keys::Chrome if !ctrl && !alt => match key {
            "Tab" => Some(KeyAct::TogglePanels),
            "x" | "X" => Some(KeyAct::Swap),
            "[" => Some(KeyAct::Bracket(false)),
            "]" => Some(KeyAct::Bracket(true)),
            k if shift && tools::flip_key(k).is_some() => tools::flip_key(k).map(KeyAct::Flip),
            k => Tool::from_chord(k, shift).map(KeyAct::Tool),
        },
        Keys::Layers if ctrl && !shift && !alt && key.eq_ignore_ascii_case("g") => Some(KeyAct::GroupLayers),
        Keys::Stops if !ctrl && !alt && matches!(key, "Delete" | "Backspace") => Some(KeyAct::RemoveStop),
        _ => None,
    }
}

/// A press on the board at `keys`: who hears its keys from now on, and the
/// interceptor that hears them for the chrome (it answers only while the
/// press's place still has them, its pane is focused and shows the board,
/// and lets every other key by; renaming a layer puts its own in place).
pub(super) fn keys_to(store: AppStore, pane: PaneId, keys: Keys) {
    let ui = store.pane(pane).board_ui;
    ui.keys.set_if_changed(keys);
    if !matches!(keys, Keys::Chrome | Keys::Layers | Keys::Stops) || untracked(|| ui.renaming.get()).is_some() {
        return;
    }
    rinch::core::set_keyboard_interceptor(move |data| {
        let ui = store.pane(pane).board_ui;
        let live = untracked(|| store.focused_pane.get() == pane && store.pane(pane).show_board.get());
        if !live || with_session(pane, |s| s.text.is_some()).unwrap_or(true) {
            return false;
        }
        // Something holds the focus (a field anywhere, the editor, the
        // canvas's surface itself): the key is its own.
        if super::ui::something_focused(pane) {
            return false;
        }
        let Some(act) = key_act(untracked(|| ui.keys.get()), &data.key, data.ctrl, data.shift, data.alt) else { return false };
        run_on_main_thread(move || match act {
            KeyAct::Tool(tool) => pick_tool(store, pane, tool),
            KeyAct::Swap => swap_colors(store, pane),
            KeyAct::TogglePanels => ui.panels_hidden.update(|h| *h = !*h),
            KeyAct::GroupLayers => super::layers::group_picked(store, pane),
            KeyAct::Flip(axis) if untracked(|| ui.selected.get()) > 0 => tools::flip(store, pane, axis),
            KeyAct::Flip(FlipAxis::Horizontal) => pick_tool(store, pane, Tool::Hand),
            KeyAct::Flip(FlipAxis::Vertical) => pick_tool(store, pane, Tool::Select),
            KeyAct::RemoveStop => gradient::remove_picked(store, pane),
            KeyAct::Bracket(larger) => {
                if untracked(|| store.board_tool.get()).paints() {
                    super::paint::step_size(store, pane, larger);
                }
            }
        });
        true
    });
}

// ── Acting ──────────────────────────────────────────────────────────────────

/// Use `tool` from the rail.
fn pick_tool(store: AppStore, pane: PaneId, tool: Tool) {
    tools::end_text(store, pane);
    store.board_tool.set(tool);
    store.pane(pane).board_ui.popup.set(Popup::None);
}

/// Open `popup`, or close it when it is open.
fn toggle(ui: PaneUi, popup: Popup) {
    ui.popup.update(|p| *p = if *p == popup { Popup::None } else { popup });
}

/// A color picked for the well's `well` square: new elements take it, the
/// selection gets it, it joins the recent colors.
///
/// A solid color replaces a gradient: it is written as a paint
/// (`tools::apply_paint`), never as the solid key alone, which would leave
/// the gradient beside it showing.
pub(super) fn set_color(store: AppStore, pane: PaneId, well: Well, hex: Option<&str>) {
    let st = store.board_style;
    match (well, hex) {
        (Well::Stroke, Some(hex)) => {
            let Some(c) = Color::parse(hex) else { return };
            st.stroke.set(c.to_hex());
            st.stroke_gradient.set(None);
            tools::apply_paint(store, pane, Well::Stroke, &Paint::Solid(c));
            st.recent.update(|r| *r = remember_recent(r, &c.to_hex()));
        }
        (Well::Stroke, None) => {}
        (Well::Fill, Some(hex)) => {
            let Some(c) = Color::parse(hex) else { return };
            st.fill.set(Some(c.to_hex()));
            st.fill_gradient.set(None);
            tools::apply_paint(store, pane, Well::Fill, &Paint::Solid(c));
            st.recent.update(|r| *r = remember_recent(r, &c.to_hex()));
        }
        (Well::Fill, None) => {
            st.fill.set(None);
            st.fill_gradient.set(None);
            tools::apply_paint(store, pane, Well::Fill, &Paint::None);
        }
    }
}

/// A color picked in the well's picker or its recent colors: the picked
/// stop's when the front square is a gradient, else the square's.
fn pick_color(store: AppStore, pane: PaneId, hex: &str) {
    if !gradient::color_picked(store, pane, hex) {
        set_color(store, pane, untracked(|| store.board_style.front.get()), Some(hex));
    }
}

/// A square's paint: its gradient, or its color.
fn paint_of(color: Option<&str>, gradient: Option<pimble_board::Gradient>) -> Paint {
    match (gradient, color.and_then(Color::parse)) {
        (Some(g), _) => Paint::Gradient(g),
        (None, Some(c)) => Paint::Solid(c),
        (None, None) => Paint::None,
    }
}

/// What a square shows as a CSS background: its gradient, or its color.
fn square_css(color: Option<String>, gradient: Option<pimble_board::Gradient>) -> String {
    match (gradient, color) {
        (Some(g), _) => format!("background: {};", gradient::css(&g)),
        (None, Some(c)) => format!("background: {c};"),
        (None, None) => String::new(),
    }
}

/// X: swap the stroke and fill colors, of new elements and the selection.
pub(super) fn swap_colors(store: AppStore, pane: PaneId) {
    let st = store.board_style;
    let had_fill = untracked(|| st.fill.get()).is_some();
    let (stroke, fill) = untracked(|| swapped(&st.stroke.get(), st.fill.get().as_deref()));
    // Gradients go with their colors.
    let (sg, fg) = untracked(|| (st.stroke_gradient.get(), st.fill_gradient.get()));
    let stroke_gradient = if had_fill { fg } else { sg.clone() };
    let fill_gradient = sg;
    st.stroke.set(stroke.clone());
    st.fill.set(fill.clone());
    st.stroke_gradient.set(stroke_gradient.clone());
    st.fill_gradient.set(fill_gradient.clone());
    st.stop.set(0);
    tools::apply_paint(store, pane, Well::Stroke, &paint_of(Some(&stroke), stroke_gradient));
    tools::apply_paint_quietly(store, pane, Well::Fill, &paint_of(fill.as_deref(), fill_gradient));
}

/// Zoom pane `pane`'s board in or out about its middle.
fn zoom_by(store: AppStore, pane: PaneId, factor: f32) {
    let Some((w, h)) = with_session(pane, |s| s.view.size()) else { return };
    tools::zoom_about(store, pane, w as f32 / 2.0, h as f32 / 2.0, factor);
}

// ── Rendering ───────────────────────────────────────────────────────────────

/// A square icon button.
fn icon_button(__scope: &mut RenderScope, icon: TablerIcon, tip: String, active: impl Fn() -> bool + 'static, run: impl Fn() + 'static) -> NodeHandle {
    rsx! {
        div {
            title: tip,
            class: {|| if active() { "pimble-board-tool pimble-board-tool--active" } else { "pimble-board-tool" }},
            onclick: move || run(),
            IconGlyph { name: icon.name().to_string() }
        }
    }
}

/// A context-bar button that opens `popup`, a row of choices under the bar,
/// showing the current choice as its face.
///
/// `size` is a class giving a face of words its width ("" for an icon's):
/// the button does not grow with its face.
pub(super) fn picker_button(__scope: &mut RenderScope, ui: PaneUi, tip: &'static str, popup: Popup, size: &'static str, face: NodeHandle) -> NodeHandle {
    let class = format!("pimble-board-tool pimble-board__chooser {size}");
    let active = format!("{class} pimble-board-tool--active");
    rsx! {
        div {
            title: tip,
            class: {move || if ui.popup.get() == popup { active.clone() } else { class.clone() }},
            onclick: move || toggle(ui, popup),
            {face}
        }
    }
}

/// A text button.
fn text_button(__scope: &mut RenderScope, label: &'static str, tip: &'static str, active: impl Fn() -> bool + 'static, run: impl Fn() + 'static) -> NodeHandle {
    rsx! {
        div {
            title: tip,
            class: {|| if active() { "pimble-board-chip pimble-board-chip--active" } else { "pimble-board-chip" }},
            onclick: move || run(),
            {label}
        }
    }
}

/// A row of a popup menu.
fn menu_item(__scope: &mut RenderScope, store: AppStore, pane: PaneId, icon: TablerIcon, label: &'static str, run: Box<dyn Fn(AppStore, PaneId)>) -> NodeHandle {
    let ui = store.pane(pane).board_ui;
    rsx! {
        div {
            class: "pimble-board__menu-item",
            onclick: move || {
                ui.popup.set(Popup::None);
                run(store, pane);
            },
            span { class: "pimble-board__menu-icon", IconGlyph { name: icon.name().to_string() } }
            span { {label} }
        }
    }
}

/// The tool rail.
pub(super) fn render_rail(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let st = store.board_style;
    // A tool used by its letter becomes its flyout's face too (one effect
    // for the window).
    thread_local! {
        static FACES_FOLLOW: Cell<bool> = const { Cell::new(false) };
    }
    if !FACES_FOLLOW.with(|f| f.replace(true)) {
        let _ = rinch::Effect::new(move || {
            let tool = store.board_tool.get();
            untracked(|| {
                let faces = remember_face(&st.faces.get(), tool);
                st.faces.set_if_changed(faces);
            });
        });
    }
    let slots: Vec<NodeHandle> = RAIL
        .into_iter()
        .map(|slot| match slot {
            Slot::Tool(tool) => icon_button(__scope, tool.icon(), tip(tool), move || store.board_tool.get() == tool, move || pick_tool(store, pane, tool)),
            Slot::Flyout(i) => render_flyout(__scope, store, pane, i),
            Slot::Picture => icon_button(__scope, TablerIcon::PhotoPlus, "Insert pictures (or paste or drop them on the canvas)".to_string(), || false, move || {
                tools::end_text(store, pane);
                pick_images(store, Dest::Board(pane, None));
            }),
            Slot::Gap => rsx! { div { class: "pimble-board__rail-gap" } },
        })
        .collect();
    rsx! {
        div {
            class: "pimble-float pimble-board__rail",
            {slots}
        }
    }
}

/// How long a press on a flyout button is held before its flyout opens.
const HOLD_MS: u32 = 350;

/// A flyout button: its face is the tool last used from it; a click uses
/// that tool, the corner mark or a held press opens the rest beside it.
fn render_flyout(__scope: &mut RenderScope, store: AppStore, pane: PaneId, index: usize) -> NodeHandle {
    let st = store.board_style;
    let ui = store.pane(pane).board_ui;
    let members: &'static [Tool] = FLYOUTS[index];
    let face = move || st.faces.get().get(index).copied().unwrap_or(members[0]);
    // A held press: which press it is (a release or a new press moves it
    // on), and whether its flyout opened, so the click that ends it uses
    // nothing.
    let press = Signal::new(0u64);
    let opened_by_hold = Signal::new(false);
    let items: Vec<NodeHandle> = members
        .iter()
        .map(|&tool| {
            rsx! {
                div {
                    title: tip(tool),
                    class: {move || if store.board_tool.get() == tool { "pimble-board__menu-item pimble-board__menu-item--active" } else { "pimble-board__menu-item" }},
                    onclick: move || pick_tool(store, pane, tool),
                    span { class: "pimble-board__menu-icon", IconGlyph { name: tool.icon().name().to_string() } }
                    span { class: "pimble-board__grow", {tool.name()} }
                    span { class: "pimble-board__menu-key", {tool.key_label()} }
                }
            }
        })
        .collect();
    rsx! {
        div {
            class: "pimble-board__slot",
            div {
                class: {move || if members.contains(&store.board_tool.get()) { "pimble-board-tool pimble-board-tool--active" } else { "pimble-board-tool" }},
                title: {move || format!("{} (hold for more)", tip(face()))},
                onmousedown: move || {
                    let n = untracked(|| press.get()) + 1;
                    press.set(n);
                    opened_by_hold.set(false);
                    let _ = set_timeout(HOLD_MS, move || {
                        if untracked(|| press.get()) == n {
                            opened_by_hold.set(true);
                            ui.popup.set(Popup::Flyout(index));
                        }
                    });
                },
                onmouseup: move || press.update(|n| *n += 1),
                onclick: move || {
                    if untracked(|| opened_by_hold.get()) {
                        opened_by_hold.set(false);
                        return;
                    }
                    pick_tool(store, pane, untracked(face));
                },
                for tool in vec![face()] {
                    span { key: tool.name(), class: "pimble-board__face", IconGlyph { name: tool.icon().name().to_string() } }
                }
                div {
                    class: "pimble-board__corner-mark",
                    title: "More tools",
                    onclick: move || toggle(ui, Popup::Flyout(index)),
                    div { class: "pimble-board__corner-fill" }
                }
            }
            div {
                class: "pimble-float pimble-board__flyout",
                style: {move || if ui.popup.get() == Popup::Flyout(index) { "" } else { "display: none;" }},
                {items}
            }
        }
    }
}

/// The context bar: the tool's options, or the selection's, then the
/// selection's arrangement, then undo and redo.
pub(super) fn render_context_bar(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let st = store.board_style;
    let state = store.pane(pane);
    let ui = state.board_ui;
    let apply = move |s: Style| tools::apply_style(store, pane, s);
    let controls = move || {
        if ui.selected.get() > 0 {
            controls_for(&ui.selected_kinds.get())
        } else {
            controls_of_tool(store.board_tool.get())
        }
    };
    let shown = move |on: bool| if on { "" } else { "display: none;" };
    let label = move || {
        let n = ui.selected.get();
        if n == 1 {
            ui.selected_kinds.get().first().map(|k| kind_word(k).to_string()).unwrap_or_default()
        } else if n > 1 {
            format!("{n} selected")
        } else {
            store.board_tool.get().name().to_string()
        }
    };

    // Stroke and fill: their swatches bring the color well's picker up.
    let stroke_swatch = rsx! {
        div {
            title: "Stroke color",
            class: "pimble-board__ctx-color pimble-board__ctx-color--stroke",
            style: {move || match st.stroke_gradient.get() {
                Some(g) => format!("border-color: transparent; background: {};", gradient::css(&g)),
                None => format!("border-color: {};", st.stroke.get()),
            }},
            onclick: move || {
                st.front.set(Well::Stroke);
                ui.popup.set(Popup::Well);
            },
            div { class: "pimble-board__ctx-hole", style: {move || if st.stroke_gradient.get().is_some() { "" } else { "display: none;" }} }
        }
    };
    let fill_swatch = rsx! {
        div {
            title: "Fill color",
            class: {move || if st.fill.get().is_some() { "pimble-board__ctx-color" } else { "pimble-board__ctx-color pimble-board-swatch--none" }},
            style: {move || square_css(st.fill.get(), st.fill_gradient.get())},
            onclick: move || {
                st.front.set(Well::Fill);
                ui.popup.set(Popup::Well);
            },
        }
    };
    let widths: Vec<NodeHandle> = WIDTHS
        .iter()
        .map(|w| {
            let w = *w;
            let dot = (w * 1.2 + 2.0).min(18.0).round();
            rsx! {
                div {
                    title: format!("Width {w}"),
                    class: {move || if st.width.get() == w { "pimble-board-tool pimble-board-tool--active" } else { "pimble-board-tool" }},
                    onclick: move || {
                        st.width.set(w);
                        ui.popup.set(Popup::None);
                        apply(Style { width: Some(w), ..Style::default() });
                    },
                    div { class: "pimble-board-width", style: format!("width: {dot}px; height: {dot}px;") }
                }
            }
        })
        .collect();
    let dashes: Vec<NodeHandle> = [(Dash::Solid, TablerIcon::Line, "Solid"), (Dash::Dashed, TablerIcon::LineDashed, "Dashed"), (Dash::Dotted, TablerIcon::LineDotted, "Dotted")]
        .into_iter()
        .map(|(dash, icon, tip)| {
            icon_button(__scope, icon, tip.to_string(), move || st.dash.get() == dash, move || {
                st.dash.set(dash);
                // A preset picked: the custom pattern goes (it would win).
                st.dash_text.set(String::new());
                ui.popup.set(Popup::None);
                apply(Style { dash: Some(dash), ..Style::default() });
            })
        })
        .collect();
    let heads: Vec<NodeHandle> = [
        (ArrowHeads::End, TablerIcon::ArrowRight, "Head at the end"),
        (ArrowHeads::Start, TablerIcon::ArrowLeft, "Head at the start"),
        (ArrowHeads::Both, TablerIcon::ArrowsLeftRight, "Heads at both ends"),
        (ArrowHeads::None, TablerIcon::Minus, "No heads"),
    ]
    .into_iter()
    .map(|(h, icon, tip)| {
        icon_button(__scope, icon, tip.to_string(), move || st.heads.get() == h, move || {
            st.heads.set(h);
            ui.popup.set(Popup::None);
            apply(Style { arrow_heads: Some(h), ..Style::default() });
        })
    })
    .collect();
    let sizes: Vec<NodeHandle> = FONT_SIZES
        .iter()
        .map(|(label, size)| {
            let size = *size;
            text_button(__scope, label, "Font size", move || st.font_size.get() == size, move || {
                st.font_size.set(size);
                ui.popup.set(Popup::None);
                apply(Style { font_size: Some(size), ..Style::default() });
            })
        })
        .collect();
    // Lettering (docs/DRAWING_WAVE6_CONTRACT.md section 4): family, bold,
    // italic, letter spacing, halo, and a text's place along its path.
    let families: Vec<NodeHandle> = pimble_board::FontFamily::ALL
        .into_iter()
        .map(|family| {
            text_button(__scope, family.label(), "Font", move || st.font.get() == family, move || {
                st.font.set(family);
                apply(Style { font: Some(family), ..Style::default() });
            })
        })
        .collect();
    let bold_btn = icon_button(__scope, TablerIcon::Bold, "Bold".to_string(), move || st.bold.get(), move || {
        let on = !untracked(|| st.bold.get());
        st.bold.set(on);
        apply(Style { bold: Some(on), ..Style::default() });
    });
    let italic_btn = icon_button(__scope, TablerIcon::Italic, "Italic".to_string(), move || st.italic.get(), move || {
        let on = !untracked(|| st.italic.get());
        st.italic.set(on);
        apply(Style { italic: Some(on), ..Style::default() });
    });
    let trackings: Vec<NodeHandle> = TRACKINGS
        .iter()
        .map(|&(label, v)| {
            text_button(__scope, label, "Letter spacing", move || (st.tracking.get() - v).abs() < 1e-3, move || {
                st.tracking.set(v);
                apply(Style { tracking: Some(v), ..Style::default() });
            })
        })
        .collect();
    let halos: Vec<NodeHandle> = HALOS
        .iter()
        .map(|&(label, v)| {
            text_button(__scope, label, "Halo: the letters outlined behind, to read over a busy background", move || (st.halo.get() - v).abs() < 1e-3, move || {
                st.halo.set(v);
                tools::apply_halo(store, pane, v, &untracked(|| st.halo_color.get()));
            })
        })
        .collect();
    let halo_colors: Vec<NodeHandle> = HALO_COLORS
        .iter()
        .map(|&(hex, name)| {
            rsx! {
                div {
                    title: format!("{name} halo"),
                    class: {move || if st.halo_color.get() == hex { "pimble-board__ctx-color pimble-board__halo-swatch pimble-board-tool--active" } else { "pimble-board__ctx-color pimble-board__halo-swatch" }},
                    style: format!("background: {hex};"),
                    onclick: move || {
                        st.halo_color.set(hex.to_string());
                        // Picking a color shows the halo.
                        let width = untracked(|| st.halo.get());
                        let width = if width > 0.0 { width } else { HALOS[2].1 };
                        st.halo.set(width);
                        tools::apply_halo(store, pane, width, hex);
                    },
                }
            }
        })
        .collect();
    let sides_along: Vec<NodeHandle> = pimble_board::AlongSide::ALL
        .into_iter()
        .map(|side| {
            let label = match side {
                pimble_board::AlongSide::Above => "Above",
                pimble_board::AlongSide::On => "On",
                pimble_board::AlongSide::Below => "Below",
            };
            text_button(__scope, label, "Which side of the path", move || st.along_side.get() == side, move || {
                st.along_side.set(side);
                apply(Style { along_side: Some(side), ..Style::default() });
            })
        })
        .collect();
    let along_slider = rsx! {
        div { class: "pimble-board__ctx-opacity",
            Slider {
                min: 0.0,
                max: 1.0,
                step: 0.01,
                size: "xs",
                value_signal: Some(mirror(store, 2, |st| st.along_offset.get() as f64)),
                onchange: move |v: f64| {
                    st.along_offset.set(v as f32);
                    apply(Style { along_offset: Some(v as f32), ..Style::default() });
                },
            }
        }
    };
    let aligns: Vec<NodeHandle> = [(Align::Left, TablerIcon::AlignLeft, "Align left"), (Align::Center, TablerIcon::AlignCenter, "Center"), (Align::Right, TablerIcon::AlignRight, "Align right")]
        .into_iter()
        .map(|(align, icon, tip)| {
            icon_button(__scope, icon, tip.to_string(), move || st.align.get() == align, move || {
                st.align.set(align);
                ui.popup.set(Popup::None);
                apply(Style { align: Some(align), ..Style::default() });
            })
        })
        .collect();

    // A live shape's parameters (they stay live until a point is edited).
    let radius_slider = rsx! {
        div { class: "pimble-board__ctx-opacity",
            Slider {
                min: 0.0,
                max: 100.0,
                step: 1.0,
                size: "xs",
                value_signal: Some(mirror(store, 0, |st| st.radius.get() as f64)),
                onchange: move |v: f64| {
                    st.radius.set(v as f32);
                    apply(Style { radius: Some(v as f32), ..Style::default() });
                },
            }
        }
    };
    let inner_slider = rsx! {
        div { class: "pimble-board__ctx-opacity",
            Slider {
                min: 0.1,
                max: 0.9,
                step: 0.05,
                size: "xs",
                value_signal: Some(mirror(store, 1, |st| st.inner.get() as f64)),
                onchange: move |v: f64| {
                    st.inner.set(v as f32);
                    apply(Style { inner: Some(v as f32), ..Style::default() });
                },
            }
        }
    };
    let sides: Vec<NodeHandle> = SIDES
        .iter()
        .map(|&n| {
            text_button(__scope, number_label(n), "Sides", move || st.sides.get() == n, move || {
                st.sides.set(n);
                apply(Style { sides: Some(n), ..Style::default() });
            })
        })
        .collect();
    let star_points: Vec<NodeHandle> = STAR_POINTS
        .iter()
        .map(|&n| {
            text_button(__scope, number_label(n), "Points", move || st.points.get() == n, move || {
                st.points.set(n);
                apply(Style { points: Some(n), ..Style::default() });
            })
        })
        .collect();
    // Stroke ends and corners, and a path's fill rule.
    let dashed = move || st.dash.get() != Dash::Solid;
    let caps: Vec<NodeHandle> = pimble_board::Cap::ALL
        .into_iter()
        .map(|cap| {
            text_button(__scope, cap_word(cap), "Stroke ends", move || st.cap.get().unwrap_or(if dashed() { pimble_board::Cap::Butt } else { pimble_board::Cap::Round }) == cap, move || {
                st.cap.set(Some(cap));
                apply(Style { cap: Some(cap), ..Style::default() });
            })
        })
        .collect();
    let joins: Vec<NodeHandle> = pimble_board::Join::ALL
        .into_iter()
        .map(|join| {
            text_button(__scope, join_word(join), "Stroke corners", move || st.join.get().unwrap_or(pimble_board::Join::Round) == join, move || {
                st.join.set(Some(join));
                apply(Style { join: Some(join), ..Style::default() });
            })
        })
        .collect();
    // The miter limit, shown while corners are mitered.
    let miters: Vec<NodeHandle> = MITERS
        .iter()
        .map(|&m| {
            text_button(__scope, number_label(m as u32), "Miter limit: how far a sharp corner may reach before it is cut", move || (st.miter.get() - m).abs() < 1e-3, move || {
                st.miter.set(m);
                apply(Style { miter: Some(m), ..Style::default() });
            })
        })
        .collect();
    let mitered = move || st.join.get() == Some(pimble_board::Join::Miter);
    // A custom dash pattern, typed: applied as soon as it reads as one,
    // cleared (the preset draws) when emptied.
    let pattern_input = rsx! {
        input {
            r#type: "text",
            class: {move || if !st.dash_text.get().trim().is_empty() && pimble_board::parse_dash_pattern(&st.dash_text.get()).is_none() { "pimble-board__pattern pimble-board__pattern--bad" } else { "pimble-board__pattern" }},
            placeholder: "e.g. 6 3 1 3",
            title: "A dash pattern: lengths on and off in turn, board units",
            value: move || st.dash_text.get(),
            oninput: move |v: String| {
                st.dash_text.set(v.clone());
                if v.trim().is_empty() {
                    tools::clear_dash_pattern(store, pane);
                } else if let Some(p) = pimble_board::parse_dash_pattern(&v) {
                    apply(Style { dash_pattern: Some(p), ..Style::default() });
                }
            },
        }
    };
    let rules: Vec<NodeHandle> = [(pimble_board::PathRule::NonZero, "Nonzero", "Overlaps fill"), (pimble_board::PathRule::EvenOdd, "Even-odd", "Overlaps make holes")]
        .into_iter()
        .map(|(rule, label, tip)| {
            text_button(__scope, label, tip, move || st.rule.get() == rule, move || {
                st.rule.set(rule);
                tools::set_rule(store, pane, rule);
            })
        })
        .collect();
    let smoothings: Vec<NodeHandle> = SMOOTHING
        .iter()
        .map(|&(label, v)| {
            text_button(__scope, label, "How far the fitted path may stray from the stroke", move || st.smoothing.get() == v, move || {
                st.smoothing.set(v);
                ui.popup.set(Popup::None);
            })
        })
        .collect();
    let shape_face = rsx! {
        div { class: "pimble-board__face-wrap",
            span { class: "pimble-board__face-text", style: {move || shown(controls().radius)}, {move || format!("r {}", st.radius.get().round())} }
            span { class: "pimble-board__face-text", style: {move || shown(controls().sides)}, {move || format!("{} sides", st.sides.get())} }
            span { class: "pimble-board__face-text", style: {move || shown(controls().star)}, {move || format!("{} pts", st.points.get())} }
        }
    };
    let detail_face = rsx! { span { class: "pimble-board__face", IconGlyph { name: TablerIcon::BorderCornerRounded.name().to_string() } } };
    let smoothing_face = rsx! {
        span { class: "pimble-board__face-text", {move || { let v = st.smoothing.get(); SMOOTHING.iter().find(|(_, s)| *s == v).map(|(l, _)| l.to_string()).unwrap_or_default() }} }
    };
    let shape_btn = picker_button(__scope, ui, "Shape: corner radius, sides, points", Popup::Shape, "pimble-board__chooser--words", shape_face);
    let detail_btn = picker_button(__scope, ui, "Stroke ends, corners and dashes, fill rule", Popup::Detail, "", detail_face);
    let smoothing_btn = picker_button(__scope, ui, "Smoothing", Popup::Smoothing, "pimble-board__chooser--words", smoothing_face);

    // A paint tool's: the color it paints with (the color well's front
    // square), its preset, size, opacity, flow and hardness.
    let brush_swatch = rsx! {
        div {
            title: "Brush color: the color well's front square (the eyedropper, or Alt held while painting, picks one)",
            class: "pimble-board__ctx-color pimble-board__ctx-brush",
            style: {move || format!("background: {};", paint::brush_color(store).to_hex())},
            onclick: move || ui.popup.set(Popup::Well),
        }
    };
    let presets: Vec<NodeHandle> = paint::preset_choices()
        .into_iter()
        .map(|(name, label)| {
            text_button(__scope, label, "Brush preset", move || paint::current_preset(store).map(|p| p.name) == Some(name), move || {
                paint::pick_preset(store, name);
                ui.popup.set(Popup::None);
            })
        })
        .collect();
    let setting = move |read: fn(&BrushSettings) -> f32| paint::current_settings(store).map(|s| read(&s)).unwrap_or(0.0);
    let preset_face = rsx! {
        span { class: "pimble-board__face-text", {move || paint::current_preset(store).map(|p| p.label).unwrap_or("")} }
    };
    let size_face = rsx! { span { class: "pimble-board__face-text", {move || size_label(setting(|s| s.size))} } };
    let brush_opacity_face = rsx! { span { class: "pimble-board__face-text", {move || format!("{}%", (setting(|s| s.opacity) * 100.0).round())} } };
    let flow_face = rsx! { span { class: "pimble-board__face-text", {move || format!("Flow {}%", (setting(|s| s.flow) * 100.0).round())} } };
    let hardness_face = rsx! { span { class: "pimble-board__face-text", {move || format!("Hard {}%", (setting(|s| s.hardness) * 100.0).round())} } };
    let preset_btn = picker_button(__scope, ui, "Brush preset", Popup::Preset, "pimble-board__chooser--words", preset_face);
    let size_btn = picker_button(__scope, ui, "Size ([ and ] change it)", Popup::BrushSize, "pimble-board__chooser--number", size_face);
    let brush_opacity_btn = picker_button(__scope, ui, "Opacity of the stroke", Popup::BrushOpacity, "pimble-board__chooser--number", brush_opacity_face);
    let flow_btn = picker_button(__scope, ui, "Flow: how much paint each dab lays down", Popup::Flow, "pimble-board__chooser--words", flow_face);
    let hardness_btn = picker_button(__scope, ui, "Hardness of the brush's edge", Popup::Hardness, "pimble-board__chooser--words", hardness_face);
    // The size slider is logarithmic: fine steps where brushes are small.
    let size_row = paint_slider(__scope, store, 0, "Size", (0.0, 1.0, 0.005), |s| size_to_slider(s.size), |s, v| s.size = slider_to_size(v), |s| size_label(s.size));
    let brush_opacity_row = paint_slider(__scope, store, 1, "Opacity", (0.05, 1.0, 0.05), |s| s.opacity as f64, |s, v| s.opacity = v as f32, |s| format!("{}%", (s.opacity * 100.0).round()));
    let flow_row = paint_slider(__scope, store, 2, "Flow", (0.02, 1.0, 0.02), |s| s.flow as f64, |s, v| s.flow = v as f32, |s| format!("{}%", (s.flow * 100.0).round()));
    let hardness_row = paint_slider(__scope, store, 3, "Hardness", (0.0, 1.0, 0.05), |s| s.hardness as f64, |s, v| s.hardness = v as f32, |s| format!("{}%", (s.hardness * 100.0).round()));

    // The Fill's tolerance and reach, Smudge's and Blur's strength.
    let tolerances: Vec<NodeHandle> = paint::TOLERANCES
        .iter()
        .map(|&(label, v)| {
            text_button(__scope, label, "Tolerance: how far a color may be from the one clicked and still fill", move || (st.tolerance.get() - v).abs() < 0.005, move || {
                st.tolerance.set(v);
                ui.popup.set(Popup::None);
            })
        })
        .collect();
    let fill_modes: Vec<NodeHandle> = [(true, "Contiguous", "Fill what touches the pixel clicked"), (false, "Everywhere", "Fill every matching pixel in view")]
        .into_iter()
        .map(|(on, label, tip)| {
            text_button(__scope, label, tip, move || st.contiguous.get() == on, move || {
                st.contiguous.set(on);
                ui.popup.set(Popup::None);
            })
        })
        .collect();
    let strengths: Vec<NodeHandle> = paint::STRENGTHS
        .iter()
        .map(|&(label, v)| {
            text_button(__scope, label, "Strength", move || (st.strength.get() - v).abs() < 0.005, move || {
                st.strength.set(v);
                ui.popup.set(Popup::None);
            })
        })
        .collect();
    let tolerance_face = rsx! { span { class: "pimble-board__face-text", {move || format!("Tol {}", paint::choice_label(&paint::TOLERANCES, st.tolerance.get()))} } };
    let fill_mode_face = rsx! { span { class: "pimble-board__face-text", {move || if st.contiguous.get() { "Contiguous" } else { "Everywhere" }} } };
    let strength_face = rsx! { span { class: "pimble-board__face-text", {move || format!("Strength {}", paint::choice_label(&paint::STRENGTHS, st.strength.get()))} } };
    let tolerance_btn = picker_button(__scope, ui, "Tolerance", Popup::Tolerance, "pimble-board__chooser--long", tolerance_face);
    let fill_mode_btn = picker_button(__scope, ui, "Contiguous or everywhere", Popup::FillMode, "pimble-board__chooser--long", fill_mode_face);
    let strength_btn = picker_button(__scope, ui, "Strength", Popup::Strength, "pimble-board__chooser--long", strength_face);

    // The selection's arrangement.
    let act = |__scope: &mut RenderScope, icon: TablerIcon, tip: &'static str, run: fn(AppStore, PaneId)| icon_button(__scope, icon, tip.to_string(), || false, move || run(store, pane));
    let duplicate = act(__scope, TablerIcon::Copy, "Duplicate (Ctrl+D)", tools::duplicate);
    let group = act(__scope, TablerIcon::BoxMultiple, "Group (Ctrl+G)", tools::group);
    let ungroup = act(__scope, TablerIcon::Package, "Ungroup (Ctrl+Shift+G)", tools::ungroup);
    let lock = act(__scope, TablerIcon::Lock, "Lock / Unlock (Ctrl+L)", tools::toggle_lock);
    let delete = act(__scope, TablerIcon::Trash, "Delete (Delete)", delete_selected);
    let combine_items: Vec<NodeHandle> = [
        (CombineOp::Union, TablerIcon::LayersUnion, "Union"),
        (CombineOp::Subtract, TablerIcon::LayersSubtract, "Subtract (the back one minus the rest)"),
        (CombineOp::Intersect, TablerIcon::LayersIntersect, "Intersect"),
        (CombineOp::Exclude, TablerIcon::LayersDifference, "Exclude"),
    ]
    .into_iter()
    .map(|(op, icon, label)| menu_item(__scope, store, pane, icon, label, Box::new(move |s, p| tools::combine(s, p, op))))
    .collect();
    let combine_face = rsx! { span { class: "pimble-board__face", IconGlyph { name: TablerIcon::LayersUnion.name().to_string() } } };
    let combine_btn = picker_button(__scope, ui, "Combine: union, subtract, intersect, exclude", Popup::Combine, "", combine_face);
    let arrange_btn = icon_button(__scope, TablerIcon::Dots, "More: forward, backward, front, back, flip, outline stroke".to_string(), move || ui.popup.get() == Popup::Arrange, move || toggle(ui, Popup::Arrange));
    let align_btn = icon_button(__scope, TablerIcon::LayoutAlignCenter, "Align and distribute".to_string(), move || ui.popup.get() == Popup::Align, move || toggle(ui, Popup::Align));
    let undo_btn = act(__scope, TablerIcon::ArrowBackUp, "Undo (Ctrl+Z)", |s, p| tools::undo(s, p, false));
    let redo_btn = act(__scope, TablerIcon::ArrowForwardUp, "Redo (Ctrl+Shift+Z)", |s, p| tools::undo(s, p, true));

    let align_items: Vec<NodeHandle> = vec![
        menu_item(__scope, store, pane, TablerIcon::LayoutAlignLeft, "Align Left", Box::new(|s, p| tools::align(s, p, Align2::Left))),
        menu_item(__scope, store, pane, TablerIcon::LayoutAlignCenter, "Align Centers", Box::new(|s, p| tools::align(s, p, Align2::Center))),
        menu_item(__scope, store, pane, TablerIcon::LayoutAlignRight, "Align Right", Box::new(|s, p| tools::align(s, p, Align2::Right))),
        menu_item(__scope, store, pane, TablerIcon::LayoutAlignTop, "Align Top", Box::new(|s, p| tools::align(s, p, Align2::Top))),
        menu_item(__scope, store, pane, TablerIcon::LayoutAlignMiddle, "Align Middles", Box::new(|s, p| tools::align(s, p, Align2::Middle))),
        menu_item(__scope, store, pane, TablerIcon::LayoutAlignBottom, "Align Bottom", Box::new(|s, p| tools::align(s, p, Align2::Bottom))),
        menu_item(__scope, store, pane, TablerIcon::LayoutDistributeVertical, "Distribute Across", Box::new(|s, p| tools::align(s, p, Align2::SpreadAcross))),
        menu_item(__scope, store, pane, TablerIcon::LayoutDistributeHorizontal, "Distribute Down", Box::new(|s, p| tools::align(s, p, Align2::SpreadDown))),
    ];
    // The pickers' faces: the current choice.
    let width_face = rsx! {
        div { class: "pimble-board-width", style: {move || { let d = (st.width.get() * 1.2 + 2.0).min(18.0).round(); format!("width: {d}px; height: {d}px;") }} }
    };
    let icon_face = |__scope: &mut RenderScope, options: Vec<(TablerIcon, Box<dyn Fn() -> bool>)>| -> NodeHandle {
        let spans: Vec<NodeHandle> = options
            .into_iter()
            .map(|(icon, on)| rsx! { span { class: "pimble-board__face", style: {|| if on() { "" } else { "display: none;" }}, IconGlyph { name: icon.name().to_string() } } })
            .collect();
        rsx! { div { class: "pimble-board__face-wrap", {spans} } }
    };
    let dash_face = icon_face(__scope, vec![
        (TablerIcon::Line, Box::new(move || st.dash.get() == Dash::Solid)),
        (TablerIcon::LineDashed, Box::new(move || st.dash.get() == Dash::Dashed)),
        (TablerIcon::LineDotted, Box::new(move || st.dash.get() == Dash::Dotted)),
    ]);
    let heads_face = icon_face(__scope, vec![
        (TablerIcon::ArrowRight, Box::new(move || st.heads.get() == ArrowHeads::End)),
        (TablerIcon::ArrowLeft, Box::new(move || st.heads.get() == ArrowHeads::Start)),
        (TablerIcon::ArrowsLeftRight, Box::new(move || st.heads.get() == ArrowHeads::Both)),
        (TablerIcon::Minus, Box::new(move || st.heads.get() == ArrowHeads::None)),
    ]);
    let align_face = icon_face(__scope, vec![
        (TablerIcon::AlignLeft, Box::new(move || st.align.get() == Align::Left)),
        (TablerIcon::AlignCenter, Box::new(move || st.align.get() == Align::Center)),
        (TablerIcon::AlignRight, Box::new(move || st.align.get() == Align::Right)),
    ]);
    let font_face = rsx! {
        span {
            class: "pimble-board__face-text",
            style: {move || format!(
                "font-weight: {}; font-style: {}; font-family: {};",
                if st.bold.get() { 700 } else { 400 },
                if st.italic.get() { "italic" } else { "normal" },
                if st.font.get() == pimble_board::FontFamily::Serif { "serif" } else { "inherit" },
            )},
            {move || {
                let size = st.font_size.get();
                let size = FONT_SIZES.iter().find(|(_, s)| *s == size).map(|(l, _)| l.to_string()).unwrap_or_else(|| format!("{size}"));
                match st.font.get() {
                    pimble_board::FontFamily::Sans => format!("{size} Aa"),
                    pimble_board::FontFamily::Serif => format!("{size} Serif"),
                    pimble_board::FontFamily::Condensed => format!("{size} Cond"),
                }
            }}
        }
    };
    let opacity_face = rsx! {
        span { class: "pimble-board__face-text", {move || format!("{}%", (st.opacity.get() * 100.0).round())} }
    };
    let width_btn = picker_button(__scope, ui, "Width", Popup::Width, "", width_face);
    let dash_btn = picker_button(__scope, ui, "Line style", Popup::Dash, "", dash_face);
    let heads_btn = picker_button(__scope, ui, "Arrow heads", Popup::Heads, "", heads_face);
    let font_btn = picker_button(__scope, ui, "Text: size, font, spacing, halo", Popup::Font, "pimble-board__chooser--words", font_face);
    let align_text_btn = picker_button(__scope, ui, "Text alignment", Popup::TextAlign, "", align_face);
    let opacity_btn = picker_button(__scope, ui, "Opacity", Popup::Opacity, "pimble-board__chooser--number", opacity_face);
    let more_items: Vec<NodeHandle> = vec![
        menu_item(__scope, store, pane, TablerIcon::ChevronUp, "Bring Forward (Ctrl+])", Box::new(|s, p| tools::reorder(s, p, Order::Forward))),
        menu_item(__scope, store, pane, TablerIcon::ChevronDown, "Send Backward (Ctrl+[)", Box::new(|s, p| tools::reorder(s, p, Order::Backward))),
        menu_item(__scope, store, pane, TablerIcon::StackPush, "Bring to Front (Ctrl+Shift+])", Box::new(|s, p| tools::reorder(s, p, Order::Front))),
        menu_item(__scope, store, pane, TablerIcon::StackPop, "Send to Back (Ctrl+Shift+[)", Box::new(|s, p| tools::reorder(s, p, Order::Back))),
        menu_item(__scope, store, pane, TablerIcon::FlipVertical, "Flip Horizontal (Shift+H)", Box::new(|s, p| tools::flip(s, p, FlipAxis::Horizontal))),
        menu_item(__scope, store, pane, TablerIcon::FlipHorizontal, "Flip Vertical (Shift+V)", Box::new(|s, p| tools::flip(s, p, FlipAxis::Vertical))),
        menu_item(__scope, store, pane, TablerIcon::BorderOuter, "Outline Stroke", Box::new(tools::outline_stroke)),
        menu_item(__scope, store, pane, TablerIcon::Route, "Put Text on Path", Box::new(tools::put_text_on_path)),
        menu_item(__scope, store, pane, TablerIcon::RouteOff, "Release from Path", Box::new(tools::release_from_path)),
        menu_item(__scope, store, pane, TablerIcon::Components, "Make Symbol", Box::new(super::stamp::make_symbol)),
        menu_item(__scope, store, pane, TablerIcon::Edit, "Edit Symbol", Box::new(super::stamp::edit_symbol)),
        menu_item(__scope, store, pane, TablerIcon::Trees, "Scatter in Shape", Box::new(super::stamp::scatter_in_shape)),
    ];
    let (stamp_bar, stamp_popups) = super::stamp::render_controls(__scope, store, pane);
    let writable = move || {
        state
            .board
            .get()
            .is_some_and(|held| store.node_access_tracked(held.store_id, held.node_id).allows_write())
    };

    rsx! {
        div {
            class: "pimble-board__top",
            style: {move || match (super::ui::narrow_or_small(store, pane).0, ui.panels_hidden.get()) {
                (true, _) => "right: 8px;",
                (false, true) => "right: 56px;",
                _ => "",
            }},
            div {
                class: "pimble-float pimble-board__context",
                onmouseenter: move || tools::pointer_off(store, pane),
                onmousedown: move || {
                    crate::pane_view::press_in_view();
                    keys_to(store, pane, Keys::Chrome);
                },
                // Marking up a picture in text (docs/IMAGE_MARKUP_CONTRACT.md
                // section 2): "Done" goes back to the text.
                div {
                    class: "pimble-board__ctx-group",
                    style: {move || shown(state.markup.get())},
                    Button {
                        size: "xs",
                        onclick: move || end_markup(store, pane),
                        "Done"
                    }
                }
                // Editing a symbol (docs/DRAWING_WAVE6_CONTRACT.md section
                // 2): "Done" goes back to the canvas.
                div {
                    class: "pimble-board__ctx-group",
                    style: {move || shown(ui.editing_symbol.get().is_some())},
                    span { class: "pimble-board__ctx-label", {move || ui.editing_symbol.get().map(|n| format!("Symbol \"{n}\"")).unwrap_or_default()} }
                    Button {
                        size: "xs",
                        onclick: move || super::stamp::done(store, pane),
                        "Done"
                    }
                }
                div {
                    class: "pimble-board__ctx-group",
                    style: {move || if writable() { "" } else { "opacity: 0.45; pointer-events: none;" }},
                    div { class: "pimble-board__ctx-label", {move || label()} }
                    div {
                        class: "pimble-board__ctx-hint",
                        style: {move || shown(ui.selected.get() == 0 && !controls().any() && !hint(store.board_tool.get()).is_empty())},
                        {move || hint(store.board_tool.get())}
                    }
                    div { class: "pimble-board__ctx-group", style: {move || shown(controls().stroke)}, {stroke_swatch} }
                    div { class: "pimble-board__ctx-group", style: {move || shown(controls().fill)}, {fill_swatch} }
                    div { class: "pimble-board__ctx-group", style: {move || shown(controls().color)}, {brush_swatch} }
                    div { class: "pimble-board__ctx-group pimble-board__ctx-sep", style: {move || shown(controls().pickers())},
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().width)}, {width_btn} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().dash)}, {dash_btn} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().heads)}, {heads_btn} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().font)}, {font_btn} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().align)}, {align_text_btn} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().opacity)}, {opacity_btn} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().radius || controls().sides || controls().star)}, {shape_btn} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().cap || controls().join || controls().rule)}, {detail_btn} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().smoothing)}, {smoothing_btn} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().preset)}, {preset_btn} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().size)}, {size_btn} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().brush_opacity)}, {brush_opacity_btn} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().flow)}, {flow_btn} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().hardness)}, {hardness_btn} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().tolerance)}, {tolerance_btn} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().contiguous)}, {fill_mode_btn} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().strength)}, {strength_btn} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().stamp)}, {stamp_bar} }
                    }
                    div {
                        class: "pimble-board__ctx-group pimble-board__ctx-sep",
                        style: {move || shown(ui.selected.get() > 0)},
                        {duplicate}
                        div { class: "pimble-board__ctx-group", style: {move || shown(ui.selected.get() > 1)}, {combine_btn} {align_btn} {group} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(ui.selected_kinds.get().contains(&Kind::Group))}, {ungroup} }
                        {lock}
                        {delete}
                        {arrange_btn}
                    }
                    div { class: "pimble-board__ctx-group pimble-board__ctx-sep", {undo_btn} {redo_btn} }
                }
            }
            div {
                class: "pimble-float pimble-board__menu",
                style: {move || shown(ui.popup.get() == Popup::Arrange)},
                {more_items}
            }
            div { class: "pimble-float pimble-board__options", style: {move || shown(ui.popup.get() == Popup::Width)}, {widths} }
            div { class: "pimble-float pimble-board__options", style: {move || shown(ui.popup.get() == Popup::Dash)}, {dashes} }
            div { class: "pimble-float pimble-board__options", style: {move || shown(ui.popup.get() == Popup::Heads)}, {heads} }
            div {
                class: "pimble-float pimble-board__options pimble-board__options--rows",
                style: {move || shown(ui.popup.get() == Popup::Font)},
                div { class: "pimble-board__row",
                    span { class: "pimble-board__ctx-label", "Size" }
                    {sizes}
                }
                div { class: "pimble-board__row",
                    span { class: "pimble-board__ctx-label", "Font" }
                    {families}
                    {bold_btn}
                    {italic_btn}
                }
                div { class: "pimble-board__row",
                    span { class: "pimble-board__ctx-label", "Spacing" }
                    {trackings}
                }
                div { class: "pimble-board__row",
                    span { class: "pimble-board__ctx-label", "Halo" }
                    {halos}
                    {halo_colors}
                }
                div { class: "pimble-board__row", style: {move || shown(st.on_path.get() && ui.selected.get() > 0)},
                    span { class: "pimble-board__ctx-label", "Path" }
                    {sides_along}
                    {along_slider}
                }
            }
            div { class: "pimble-float pimble-board__options", style: {move || shown(ui.popup.get() == Popup::TextAlign)}, {aligns} }
            div {
                class: "pimble-float pimble-board__options",
                style: {move || shown(ui.popup.get() == Popup::Opacity)},
                div { class: "pimble-board__ctx-opacity",
                    Slider {
                        min: 0.1,
                        max: 1.0,
                        step: 0.05,
                        size: "xs",
                        value_signal: Some(super::ui::opacity_signal(store)),
                        onchange: move |v: f64| {
                            st.opacity.set(v as f32);
                            apply(Style { opacity: Some(v as f32), ..Style::default() });
                        },
                    }
                }
                span { class: "pimble-board__ctx-number", {move || format!("{}%", (st.opacity.get() * 100.0).round())} }
            }
            div {
                class: "pimble-float pimble-board__menu",
                style: {move || shown(ui.popup.get() == Popup::Align)},
                {align_items}
            }
            div {
                class: "pimble-float pimble-board__menu",
                style: {move || shown(ui.popup.get() == Popup::Combine)},
                {combine_items}
            }
            div {
                class: "pimble-float pimble-board__options pimble-board__options--rows",
                style: {move || shown(ui.popup.get() == Popup::Shape)},
                div { class: "pimble-board__row", style: {move || shown(controls().radius)},
                    span { class: "pimble-board__ctx-label", "Corners" }
                    {radius_slider}
                    span { class: "pimble-board__ctx-number", {move || format!("{}", st.radius.get().round())} }
                }
                div { class: "pimble-board__row", style: {move || shown(controls().sides)},
                    span { class: "pimble-board__ctx-label", "Sides" }
                    {sides}
                }
                div { class: "pimble-board__row", style: {move || shown(controls().star)},
                    span { class: "pimble-board__ctx-label", "Points" }
                    {star_points}
                }
                div { class: "pimble-board__row", style: {move || shown(controls().star)},
                    span { class: "pimble-board__ctx-label", "Inner" }
                    {inner_slider}
                    span { class: "pimble-board__ctx-number", {move || format!("{}%", (st.inner.get() * 100.0).round())} }
                }
            }
            div {
                class: "pimble-float pimble-board__options pimble-board__options--rows",
                style: {move || shown(ui.popup.get() == Popup::Detail)},
                div { class: "pimble-board__row", style: {move || shown(controls().cap)},
                    span { class: "pimble-board__ctx-label", "Ends" }
                    {caps}
                }
                div { class: "pimble-board__row", style: {move || shown(controls().join)},
                    span { class: "pimble-board__ctx-label", "Corners" }
                    {joins}
                }
                div { class: "pimble-board__row", style: {move || shown(controls().join && mitered())},
                    span { class: "pimble-board__ctx-label", "Miter" }
                    {miters}
                }
                div { class: "pimble-board__row", style: {move || shown(controls().dash)},
                    span { class: "pimble-board__ctx-label", "Dashes" }
                    {pattern_input}
                }
                div { class: "pimble-board__row", style: {move || shown(controls().rule && ui.selected.get() > 0)},
                    span { class: "pimble-board__ctx-label", "Fill" }
                    {rules}
                }
            }
            div { class: "pimble-float pimble-board__options", style: {move || shown(ui.popup.get() == Popup::Smoothing)}, {smoothings} }
            div { class: "pimble-float pimble-board__options", style: {move || shown(ui.popup.get() == Popup::Preset)}, {presets} }
            div { class: "pimble-float pimble-board__options", style: {move || shown(ui.popup.get() == Popup::BrushSize)}, {size_row} }
            div { class: "pimble-float pimble-board__options", style: {move || shown(ui.popup.get() == Popup::BrushOpacity)}, {brush_opacity_row} }
            div { class: "pimble-float pimble-board__options", style: {move || shown(ui.popup.get() == Popup::Flow)}, {flow_row} }
            div { class: "pimble-float pimble-board__options", style: {move || shown(ui.popup.get() == Popup::Hardness)}, {hardness_row} }
            div { class: "pimble-float pimble-board__options", style: {move || shown(ui.popup.get() == Popup::Tolerance)}, {tolerances} }
            div { class: "pimble-float pimble-board__options", style: {move || shown(ui.popup.get() == Popup::FillMode)}, {fill_modes} }
            div { class: "pimble-float pimble-board__options", style: {move || shown(ui.popup.get() == Popup::Strength)}, {strengths} }
            {stamp_popups}
        }
    }
}

/// A slider's value mirrored from the style (slot `slot`: 0 the corner
/// radius, 1 a star's inner radius), so the slider follows a selection.
fn mirror(store: AppStore, slot: usize, read: fn(&StyleUi) -> f64) -> Signal<f64> {
    thread_local! {
        static SIGNALS: RefCell<[Option<Signal<f64>>; 3]> = const { RefCell::new([None, None, None]) };
    }
    if let Some(sig) = SIGNALS.with(|s| s.borrow()[slot]) {
        return sig;
    }
    let st = store.board_style;
    let sig = Signal::new(read(&st));
    let _ = rinch::Effect::new(move || {
        let v = read(&st);
        untracked(|| sig.set_if_changed(v));
    });
    SIGNALS.with(|s| s.borrow_mut()[slot] = Some(sig));
    sig
}

/// A brush size as the context bar shows it: whole units, halves below ten.
pub fn size_label(size: f32) -> String {
    if size < 10.0 && (size.fract() - 0.5).abs() < 0.01 {
        format!("{size:.1}")
    } else {
        format!("{}", size.round())
    }
}

/// The size slider's place for a brush size: logarithmic over the sizes it
/// offers, so small brushes get fine steps.
pub fn size_to_slider(size: f32) -> f64 {
    let (lo, hi) = paint::SIZE_RANGE;
    ((size.clamp(lo, hi) / lo).ln() / (hi / lo).ln()) as f64
}

/// The brush size at a place of the size slider (whole units above ten,
/// halves below).
pub fn slider_to_size(v: f64) -> f32 {
    let (lo, hi) = paint::SIZE_RANGE;
    let size = lo * (hi / lo).powf(v.clamp(0.0, 1.0) as f32);
    if size < 10.0 {
        (size * 2.0).round() / 2.0
    } else {
        size.round()
    }
}

/// A slider row of a paint setting's popup (`slot`: its mirror), with its
/// label and value.
#[allow(clippy::too_many_arguments)]
fn paint_slider(
    __scope: &mut RenderScope,
    store: AppStore,
    slot: usize,
    label: &'static str,
    (min, max, step): (f64, f64, f64),
    read: fn(&BrushSettings) -> f64,
    write: fn(&mut BrushSettings, f64),
    shown: fn(&BrushSettings) -> String,
) -> NodeHandle {
    let value = paint_mirror(store, slot, read);
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
                    onchange: move |v: f64| paint::change_settings(store, |s| write(s, v)),
                }
            }
            span { class: "pimble-board__ctx-number", {move || paint::current_settings(store).map(|s| shown(&s)).unwrap_or_default()} }
        }
    }
}

/// A paint setting mirrored onto its slider (slot `slot`), so the slider
/// follows the tool, the preset and `[`/`]`.
fn paint_mirror(store: AppStore, slot: usize, read: fn(&BrushSettings) -> f64) -> Signal<f64> {
    thread_local! {
        static SIGNALS: RefCell<[Option<Signal<f64>>; 4]> = const { RefCell::new([None, None, None, None]) };
    }
    if let Some(sig) = SIGNALS.with(|s| s.borrow()[slot]) {
        return sig;
    }
    let sig = Signal::new(untracked(|| paint::current_settings(store)).map(|s| read(&s)).unwrap_or(0.0));
    let _ = rinch::Effect::new(move || {
        if let Some(s) = paint::current_settings(store) {
            let v = read(&s);
            untracked(|| sig.set_if_changed(v));
        }
    });
    SIGNALS.with(|s| s.borrow_mut()[slot] = Some(sig));
    sig
}

/// A small number as a chip's label.
fn number_label(n: u32) -> &'static str {
    const LABELS: [&str; 13] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12"];
    LABELS.get(n as usize).copied().unwrap_or("?")
}

fn cap_word(cap: pimble_board::Cap) -> &'static str {
    match cap {
        pimble_board::Cap::Butt => "Butt",
        pimble_board::Cap::Round => "Round",
        pimble_board::Cap::Square => "Square",
    }
}

fn join_word(join: pimble_board::Join) -> &'static str {
    match join {
        pimble_board::Join::Miter => "Miter",
        pimble_board::Join::Round => "Round",
        pimble_board::Join::Bevel => "Bevel",
    }
}

/// What the context bar calls one selected element.
fn kind_word(kind: &Kind) -> &'static str {
    match kind {
        Kind::Picture => "Picture",
        Kind::Pen => "Pen stroke",
        Kind::Highlighter => "Highlight",
        Kind::Line => "Line",
        Kind::Arrow => "Arrow",
        Kind::Rect => "Rectangle",
        Kind::Ellipse => "Ellipse",
        Kind::Diamond => "Diamond",
        Kind::Text => "Text",
        Kind::Sticky => "Sticky note",
        Kind::Step => "Numbered step",
        Kind::Callout => "Callout",
        Kind::Connector => "Connector",
        Kind::Frame => "Frame",
        Kind::Group => "Group",
        Kind::Card => "Node card",
        Kind::Spotlight => "Spotlight",
        Kind::Path => "Path",
        Kind::Polygon => "Polygon",
        Kind::Star => "Star",
        Kind::Scatter => "Scatter",
        Kind::Other(_) => "Element",
    }
}

/// The color well: the fill and stroke squares (the front one is the one
/// picked colors go to), the swap, and the picker with the recent colors.
pub(super) fn render_color_well(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let st = store.board_style;
    let ui = store.pane(pane).board_ui;
    let press = move |well: Well| {
        if untracked(|| st.front.get()) == well {
            toggle(ui, Popup::Well);
        } else {
            st.front.set(well);
        }
    };
    // The front square's gradient, tracked.
    let front_gradient = move || gradient::of_well(st, st.front.get());
    let kinds: Vec<NodeHandle> = [(None, "Solid", "One color"), (Some(GradientKind::Linear), "Linear", "A linear gradient"), (Some(GradientKind::Radial), "Radial", "A radial gradient")]
        .into_iter()
        .map(|(kind, label, tip)| {
            text_button(__scope, label, tip, move || front_gradient().map(|g| g.kind) == kind, move || gradient::switch_to(store, pane, kind))
        })
        .collect();
    let stop_marks = move || {
        let picked = st.stop.get();
        front_gradient()
            .map(|g| {
                g.stops
                    .iter()
                    .enumerate()
                    .map(|(i, s)| (i, format!("{:.2}", s.offset * 100.0), gradient::css_color(s.color), i == picked))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    rsx! {
        div {
            class: "pimble-board__well-wrap",
            onmousedown: move || crate::pane_view::press_in_view(),
            div {
                class: "pimble-float pimble-board__well-pop",
                style: {move || if ui.popup.get() == Popup::Well { "" } else { "display: none;" }},
                // The popup's inputs hear their own keys; a press on the stop
                // bar (which the browser bubbles here after it) leaves the
                // keys with the bar.
                onmousedown: move || {
                    if now_ms() - STOP_PRESS.with(|t| t.get()) > 100.0 {
                        keys_to(store, pane, Keys::Typing);
                    }
                },
                div {
                    class: "pimble-board__row",
                    span { class: "pimble-board__ctx-label pimble-board__grow", {move || if st.front.get() == Well::Fill { "Fill" } else { "Stroke" }} }
                    div {
                        class: "pimble-board-chip",
                        title: "No fill",
                        style: {move || if st.front.get() == Well::Fill { "" } else { "display: none;" }},
                        onclick: move || set_color(store, pane, Well::Fill, None),
                        "No fill"
                    }
                }
                // Solid, linear or radial (docs/DRAWING_CONTRACT.md, "Built
                // in wave 3 (UI)").
                div { class: "pimble-board__row pimble-board__well-kinds", {kinds} }
                // A gradient's stops: a press picks the stop under it (or adds
                // one there) and drags it along.
                div {
                    class: "pimble-board__row",
                    style: {move || if front_gradient().is_some() { "" } else { "display: none;" }},
                    div {
                        class: "pimble-board__stop-bar",
                        title: "Click to add a color, drag a color to move it",
                        style: {move || front_gradient().map(|g| format!("background: {};", gradient::bar_css(&g))).unwrap_or_default()},
                        onmousedown: move || {
                            let ctx = rinch::core::get_click_context();
                            STOP_PRESS.with(|t| t.set(now_ms()));
                            keys_to(store, pane, Keys::Stops);
                            gradient::press_bar(store, pane, gradient::offset_at(ctx.mouse_x - ctx.element_x, ctx.element_width));
                        },
                        for (i, left, color, picked) in stop_marks() {
                            div {
                                key: format!("{i}-{left}-{color}-{picked}"),
                                class: if picked { "pimble-board__stop pimble-board__stop--picked" } else { "pimble-board__stop" },
                                style: format!("left: {left}%; background: {color};"),
                            }
                        }
                    }
                    div {
                        class: "pimble-board-tool pimble-board-tool--small",
                        title: "Remove the picked color",
                        onclick: move || gradient::remove_picked(store, pane),
                        IconGlyph { name: TablerIcon::X.name().to_string() }
                    }
                }
                div {
                    style: {move || if front_gradient().is_some() { "display: none;" } else { "" }},
                    ColorPicker {
                        value_fn: move || if st.front.get() == Well::Fill { st.fill.get().unwrap_or_else(|| st.stroke.get()) } else { st.stroke.get() },
                        swatches: palette(),
                        swatches_per_row: 7,
                        size: "sm",
                        with_input: true,
                        onchange: move |hex: String| set_color(store, pane, untracked(|| st.front.get()), Some(&hex)),
                    }
                }
                // The picked stop's color, with its transparency.
                div {
                    style: {move || if front_gradient().is_some() { "" } else { "display: none;" }},
                    ColorPicker {
                        value_fn: move || {
                            let i = st.stop.get();
                            front_gradient().and_then(|g| g.stops.get(i).or(g.stops.first()).map(|s| s.color.to_hex())).unwrap_or_else(|| st.stroke.get())
                        },
                        swatches: palette(),
                        swatches_per_row: 7,
                        size: "sm",
                        with_input: true,
                        alpha: true,
                        format: "hexa",
                        onchange: move |hex: String| {
                            gradient::color_picked(store, pane, &hex);
                        },
                    }
                }
                div {
                    class: "pimble-board__label",
                    style: {move || if st.recent.get().is_empty() { "display: none;" } else { "" }},
                    "Recent"
                }
                div {
                    class: "pimble-board__swatches",
                    for (color, tip, background) in st.recent.get().into_iter().map(|c| (c.clone(), c.clone(), format!("background: {c};"))) {
                        div {
                            key: tip.clone(),
                            title: tip,
                            class: "pimble-board-swatch",
                            style: background,
                            onclick: {
                                let color = color.clone();
                                move || pick_color(store, pane, &color)
                            },
                        }
                    }
                }
            }
            div {
                class: "pimble-float pimble-board__well",
                onmousedown: move || keys_to(store, pane, Keys::Chrome),
                div {
                    class: "pimble-board__well-squares",
                    div {
                        title: "Fill (click again for the picker)",
                        class: {move || if st.fill.get().is_some() { "pimble-board__well-fill" } else { "pimble-board__well-fill pimble-board-swatch--none" }},
                        style: {move || format!(
                            "{} z-index: {};",
                            square_css(st.fill.get(), st.fill_gradient.get()),
                            if st.front.get() == Well::Fill { 2 } else { 1 }
                        )},
                        onclick: move || press(Well::Fill),
                    }
                    div {
                        title: "Stroke (click again for the picker)",
                        class: "pimble-board__well-stroke",
                        style: {move || {
                            let z = if st.front.get() == Well::Stroke { 2 } else { 1 };
                            match st.stroke_gradient.get() {
                                Some(g) => format!("border-color: transparent; background: {}; z-index: {z};", gradient::css(&g)),
                                None => format!("border-color: {}; z-index: {z};", st.stroke.get()),
                            }
                        }},
                        onclick: move || press(Well::Stroke),
                        div { class: "pimble-board__well-hole", style: {move || if st.stroke_gradient.get().is_some() { "" } else { "display: none;" }} }
                    }
                }
                div {
                    class: "pimble-board-tool pimble-board-tool--small",
                    title: "Swap fill and stroke (X)",
                    onclick: move || swap_colors(store, pane),
                    IconGlyph { name: TablerIcon::ArrowsExchange.name().to_string() }
                }
            }
        }
    }
}

/// The zoom corner: the map, zoom, the view's switches (only here) and
/// export.
pub(super) fn render_zoom_corner(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let st = store.board_style;
    let ui = store.pane(pane).board_ui;
    let nav_surface = nav::surface(store, pane);
    let act = |__scope: &mut RenderScope, icon: TablerIcon, tip: &'static str, run: fn(AppStore, PaneId)| icon_button(__scope, icon, tip.to_string(), || false, move || run(store, pane));
    let zoom_out = icon_button(__scope, TablerIcon::ZoomOut, "Zoom out".to_string(), || false, move || zoom_by(store, pane, 1.0 / 1.25));
    let zoom_in = icon_button(__scope, TablerIcon::ZoomIn, "Zoom in".to_string(), || false, move || zoom_by(store, pane, 1.25));
    let fit_btn = act(__scope, TablerIcon::FocusCentered, "Fit", fit);
    let map_btn = icon_button(__scope, TablerIcon::Map, "Show the map".to_string(), move || st.map.get(), move || st.map.update(|v| *v = !*v));
    let grid_btn = icon_button(__scope, TablerIcon::LayoutGrid, "Show the grid (only here)".to_string(), move || st.grid.get(), move || {
        let on = !untracked(|| st.grid.get());
        st.grid.set(on);
        set_grid(store, on);
    });
    let snap_btn = icon_button(__scope, TablerIcon::Magnet, "Snap to the grid (only here)".to_string(), move || st.snap.get(), move || st.snap.update(|v| *v = !*v));
    let author_btn = icon_button(__scope, TablerIcon::Users, "Color by author (this pane only; hides nothing)".to_string(), move || ui.by_author.get(), move || {
        let on = !untracked(|| ui.by_author.get());
        set_color_by_author(store, pane, on);
    });
    let export_btn = icon_button(__scope, TablerIcon::Download, "Copy and save the canvas as a picture, or import SVG".to_string(), move || ui.popup.get() == Popup::Export, move || toggle(ui, Popup::Export));
    let export_items: Vec<NodeHandle> = vec![
        menu_item(__scope, store, pane, TablerIcon::ClipboardCopy, "Copy Picture (Ctrl+C)", Box::new(export::copy_picture)),
        menu_item(__scope, store, pane, TablerIcon::Photo, "Save as PNG...", Box::new(|s, p| export::save_picture(s, p, Format::Png, false))),
        menu_item(__scope, store, pane, TablerIcon::Photo, "Save as JPEG...", Box::new(|s, p| export::save_picture(s, p, Format::Jpeg, false))),
        menu_item(__scope, store, pane, TablerIcon::FileExport, "Save as SVG...", Box::new(|s, p| export::save_picture(s, p, Format::Svg, false))),
        menu_item(__scope, store, pane, TablerIcon::Eye, "Save with All Markup (PNG)...", Box::new(|s, p| export::save_picture(s, p, Format::Png, true))),
        menu_item(__scope, store, pane, TablerIcon::PhotoDown, "Save Original As...", Box::new(export::save_original)),
        menu_item(__scope, store, pane, TablerIcon::Stack2, "Flatten into New Canvas", Box::new(export::flatten)),
        menu_item(__scope, store, pane, TablerIcon::FileVector, "Import SVG...", Box::new(super::svg_import::pick)),
    ];
    rsx! {
        div {
            class: "pimble-board__corner",
            onmouseenter: move || tools::pointer_off(store, pane),
            onmousedown: move || {
                crate::pane_view::press_in_view();
                keys_to(store, pane, Keys::Chrome);
            },
            div {
                class: "pimble-float pimble-board__menu pimble-board__menu--up",
                style: {move || if ui.popup.get() == Popup::Export { "" } else { "display: none;" }},
                {export_items}
            }
            div {
                class: "pimble-float pimble-board__map",
                style: {move || if st.map.get() { "" } else { "display: none;" }},
                RenderSurface { surface: Some(nav_surface) }
                div {
                    class: "pimble-board__nav-view",
                    style: {move || match ui.nav_view.get() {
                        Some((x, y, w, h)) => format!("left: {x:.1}px; top: {y:.1}px; width: {w:.1}px; height: {h:.1}px;"),
                        None => "display: none;".to_string(),
                    }},
                }
            }
            div {
                class: "pimble-float pimble-board__zoombar",
                {map_btn}
                {zoom_out}
                div {
                    class: "pimble-board__zoom",
                    title: "Actual size",
                    onclick: move || actual_size(store, pane),
                    {move || format!("{}%", ui.zoom.get())}
                }
                {zoom_in}
                {fit_btn}
                div { class: "pimble-board__ctx-sep" }
                {grid_btn}
                {snap_btn}
                {author_btn}
                {export_btn}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_is_on_the_rail_once() {
        let mut on_rail: Vec<Tool> = Vec::new();
        for slot in RAIL {
            match slot {
                Slot::Tool(t) => on_rail.push(t),
                Slot::Flyout(i) => on_rail.extend(FLYOUTS[i].iter().copied()),
                Slot::Picture | Slot::Gap => {}
            }
        }
        for tool in Tool::ALL {
            assert_eq!(on_rail.iter().filter(|t| **t == tool).count(), 1, "{tool:?}");
        }
        assert_eq!(on_rail.len(), Tool::ALL.len());
        // Every flyout is on the rail.
        for i in 0..FLYOUTS.len() {
            assert!(RAIL.contains(&Slot::Flyout(i)));
        }
    }

    #[test]
    fn flyouts_remember_the_tool_last_used() {
        let faces = default_faces();
        assert_eq!(faces, vec![Tool::Rect, Tool::Connector, Tool::Brush, Tool::Fill, Tool::SelectRect]);
        assert_eq!(flyout_of(Tool::Star), Some(0));
        assert_eq!(flyout_of(Tool::Polygon), Some(0));
        let faces = remember_face(&faces, Tool::Ellipse);
        assert_eq!(faces, vec![Tool::Ellipse, Tool::Connector, Tool::Brush, Tool::Fill, Tool::SelectRect]);
        // A tool outside every flyout changes no face.
        assert_eq!(remember_face(&faces, Tool::Pen), faces);
        let faces = remember_face(&faces, Tool::Frame);
        assert_eq!(faces, vec![Tool::Ellipse, Tool::Frame, Tool::Brush, Tool::Fill, Tool::SelectRect]);
        // The highlighter is the Paint flyout's other face; Smudge and Blur
        // share the Fill's, the lasso the rectangle select's.
        let faces = remember_face(&faces, Tool::Highlighter);
        let faces = remember_face(&faces, Tool::Blur);
        let faces = remember_face(&faces, Tool::SelectLasso);
        assert_eq!(faces, vec![Tool::Ellipse, Tool::Frame, Tool::Highlighter, Tool::Blur, Tool::SelectLasso]);
        // Faces of the wrong shape start again from the defaults.
        assert_eq!(remember_face(&[], Tool::Step), vec![Tool::Rect, Tool::Step, Tool::Brush, Tool::Fill, Tool::SelectRect]);
        assert_eq!(flyout_of(Tool::Arrow), Some(0));
        assert_eq!(flyout_of(Tool::Text), None);
        assert_eq!(flyout_of(Tool::Eraser), None);
    }

    #[test]
    fn tips_name_the_letter() {
        assert_eq!(tip(Tool::Rect), "Rectangle (U)");
        assert_eq!(tip(Tool::Brush), "Brush (B)");
        assert_eq!(tip(Tool::Eyedropper), "Eyedropper (I)");
        assert_eq!(tip(Tool::Pin), "Discussion pin (Q)");
        assert_eq!(tip(Tool::Pen), "Pen (P)");
        assert_eq!(tip(Tool::DirectSelect), "Direct select (A)");
        assert_eq!(tip(Tool::Pencil), "Pencil (N)");
        assert_eq!(tip(Tool::Fill), "Fill (G)");
        assert_eq!(tip(Tool::Smudge), "Smudge (R)");
        assert_eq!(tip(Tool::Blur), "Blur (Shift+R)");
        assert_eq!(tip(Tool::SelectRect), "Rectangle select (M)");
        assert_eq!(tip(Tool::SelectLasso), "Lasso select (L)");
        assert_eq!(tip(Tool::Highlighter), "Highlighter (Shift+B)");
        assert_eq!(tip(Tool::Line), "Line (Shift+W)");
    }

    /// Every tool has its own key (a letter, or a letter with Shift for a
    /// variant of the plain letter's tool), and the contract's P, A and N,
    /// B, E and I, G, R and Shift+R, M and L.
    #[test]
    fn every_tool_has_its_own_letter() {
        let mut chords = Vec::new();
        for tool in Tool::ALL {
            let chord = (tool.key(), tool.shifted());
            assert!(!chords.contains(&chord), "{tool:?} shares {chord:?}");
            chords.push(chord);
            assert_eq!(Tool::from_chord(&tool.key().to_string(), tool.shifted()), Some(tool), "{tool:?}");
            assert_ne!(tool.key(), 'x', "X swaps colors");
            // A shifted tool's plain letter is a tool of its own.
            if tool.shifted() {
                assert!(Tool::from_key(&tool.key().to_string()).is_some_and(|t| t != tool), "{tool:?}");
            }
        }
        assert_eq!(Tool::from_key("P"), Some(Tool::Pen));
        assert_eq!(Tool::from_key("a"), Some(Tool::DirectSelect));
        assert_eq!(Tool::from_key("n"), Some(Tool::Pencil));
        assert_eq!(Tool::from_key("b"), Some(Tool::Brush));
        assert_eq!(Tool::from_key("e"), Some(Tool::Eraser));
        assert_eq!(Tool::from_key("I"), Some(Tool::Eyedropper));
        assert_eq!(Tool::from_key("g"), Some(Tool::Fill));
        assert_eq!(Tool::from_key("r"), Some(Tool::Smudge));
        assert_eq!(Tool::from_chord("R", true), Some(Tool::Blur));
        assert_eq!(Tool::from_key("m"), Some(Tool::SelectRect));
        assert_eq!(Tool::from_key("l"), Some(Tool::SelectLasso));
        assert_eq!(Tool::from_chord("B", true), Some(Tool::Highlighter));
        assert_eq!(Tool::from_chord("W", true), Some(Tool::Line));
        assert_eq!(Tool::from_key("w"), Some(Tool::Arrow));
        // Shift with a letter that has no shifted tool is the plain one.
        assert_eq!(Tool::from_chord("N", true), Some(Tool::Pencil));
        assert_eq!(Tool::from_key("u"), Some(Tool::Rect));
        assert_eq!(Tool::from_key("y"), Some(Tool::Polygon));
        assert_eq!(Tool::from_key("j"), Some(Tool::Step));
        assert_eq!(Tool::from_key("z"), None);
    }

    #[test]
    fn paint_tools_offer_their_settings() {
        let brush = controls_of_tool(Tool::Brush);
        assert!(brush.color && brush.preset && brush.size && brush.brush_opacity && brush.flow && brush.hardness);
        assert!(!brush.stroke && !brush.fill && !brush.width && !brush.opacity, "the brush's own, not a shape's");
        assert_eq!(controls_of_tool(Tool::Highlighter), brush);
        let eraser = controls_of_tool(Tool::Eraser);
        assert!(eraser.size && !eraser.color && !eraser.preset);
        assert!(!controls_of_tool(Tool::Eyedropper).any());
        assert!(!hint(Tool::Eyedropper).is_empty());
        assert!(Tool::Brush.paints() && Tool::Highlighter.paints() && Tool::Eraser.paints());
        assert!(!Tool::Eyedropper.paints() && !Tool::Pencil.paints());
        // Wave 5: the Fill pours the brush color at a tolerance; Smudge and
        // Blur take a size and a strength; the selections have a hint.
        let fill = controls_of_tool(Tool::Fill);
        assert!(fill.color && fill.tolerance && fill.contiguous && !fill.size && !fill.preset);
        for tool in [Tool::Smudge, Tool::Blur] {
            let c = controls_of_tool(tool);
            assert!(c.size && c.strength && !c.color && !c.preset && !c.tolerance, "{tool:?}");
            assert!(tool.paints());
        }
        for tool in [Tool::SelectRect, Tool::SelectLasso] {
            assert!(!controls_of_tool(tool).any() && !hint(tool).is_empty() && tool.selects_paint() && !tool.paints());
        }
        assert!(!Tool::Fill.paints());
    }

    #[test]
    fn the_fill_and_strength_pickers_offer_their_steps() {
        let tolerances: Vec<f32> = paint::TOLERANCES.iter().map(|(_, v)| *v).collect();
        assert!(tolerances.windows(2).all(|w| w[0] < w[1]), "in order");
        assert!(tolerances.contains(&pimble_board::paint_ops::DEFAULT_TOLERANCE), "the default is a chip");
        assert!(tolerances.iter().all(|v| (0.0..=1.0).contains(v)));
        assert!(paint::STRENGTHS.iter().any(|(_, v)| *v == pimble_board::paint_ops::DEFAULT_STRENGTH));
        assert_eq!(paint::choice_label(&paint::TOLERANCES, 0.15), "15%");
        assert_eq!(paint::choice_label(&paint::TOLERANCES, 0.22), "22%", "a value set elsewhere still shows");
        assert_eq!(paint::choice_label(&paint::STRENGTHS, 1.0), "100%");
    }

    #[test]
    fn the_size_slider_is_logarithmic() {
        assert_eq!(slider_to_size(0.0), paint::SIZE_RANGE.0);
        assert_eq!(slider_to_size(1.0), paint::SIZE_RANGE.1);
        for size in [1.0, 2.5, 8.0, 24.0, 60.0, 250.0] {
            assert!((slider_to_size(size_to_slider(size)) - size).abs() <= 0.5, "{size}");
        }
        // The middle of the slider is a small brush, not a huge one.
        assert!(slider_to_size(0.5) < 20.0);
        assert_eq!(size_label(2.5), "2.5");
        assert_eq!(size_label(12.0), "12");
    }

    #[test]
    fn the_context_bar_offers_what_the_kinds_take() {
        let pen = controls_of(&Kind::Pen);
        assert!(pen.stroke && pen.width && pen.dash && !pen.fill && !pen.heads && !pen.font);
        let arrow = controls_of(&Kind::Arrow);
        assert!(arrow.heads);
        let sticky = controls_of(&Kind::Sticky);
        assert!(sticky.fill && !sticky.stroke && sticky.font);
        assert!(!controls_of(&Kind::Picture).any());
        assert!(!controls_of(&Kind::Frame).any());
        // A selection offers what any of its kinds takes.
        let mixed = controls_for(&[Kind::Pen, Kind::Sticky]);
        assert!(mixed.stroke && mixed.fill && mixed.width && mixed.font && !mixed.heads);
        assert!(!controls_for(&[]).any());
        assert!(!controls_for(&[Kind::Picture, Kind::Group]).any());
        // Stroke detail and the live shapes' own parameters.
        let rect = controls_of(&Kind::Rect);
        assert!(rect.radius && rect.cap && rect.join && !rect.sides && !rect.star && !rect.rule);
        assert!(controls_of(&Kind::Polygon).sides && controls_of(&Kind::Star).star);
        let path = controls_of(&Kind::Path);
        assert!(path.cap && path.join && path.rule && path.fill && !path.radius);
        assert!(!pen.cap && !pen.join, "pen strokes keep their round ends");
        assert!(controls_of(&Kind::Line).cap && !controls_of(&Kind::Line).join);
        // The Pencil offers smoothing; the Pen no fill rule until a path is selected.
        assert!(controls_of_tool(Tool::Pencil).smoothing && !controls_of(&Kind::Pen).smoothing);
        assert!(!controls_of_tool(Tool::Pen).rule && controls_of_tool(Tool::Pen).join);
        // Every tool that makes something has controls, but the frame.
        for tool in Tool::ALL {
            if let Some(kind) = tool.kind() {
                assert_eq!(controls_of(&kind).any(), kind != Kind::Frame, "{tool:?}");
            }
        }
    }

    #[test]
    fn the_chrome_hears_the_canvas_keys() {
        assert_eq!(key_act(Keys::Chrome, "u", false, false, false), Some(KeyAct::Tool(Tool::Rect)));
        assert_eq!(key_act(Keys::Chrome, "p", false, false, false), Some(KeyAct::Tool(Tool::Pen)));
        assert_eq!(key_act(Keys::Chrome, "a", false, false, false), Some(KeyAct::Tool(Tool::DirectSelect)));
        assert_eq!(key_act(Keys::Chrome, "N", false, true, false), Some(KeyAct::Tool(Tool::Pencil)));
        assert_eq!(key_act(Keys::Chrome, "X", false, true, false), Some(KeyAct::Swap));
        assert_eq!(key_act(Keys::Chrome, "Tab", false, false, false), Some(KeyAct::TogglePanels));
        assert_eq!(key_act(Keys::Chrome, "[", false, false, false), Some(KeyAct::Bracket(false)));
        assert_eq!(key_act(Keys::Chrome, "]", false, false, false), Some(KeyAct::Bracket(true)));
        assert_eq!(key_act(Keys::Stops, "Delete", false, false, false), Some(KeyAct::RemoveStop));
        assert_eq!(key_act(Keys::Stops, "Backspace", false, false, false), Some(KeyAct::RemoveStop));
        assert_eq!(key_act(Keys::Stops, "b", false, false, false), None, "letters on the stop bar pick no tool");
        assert_eq!(key_act(Keys::Chrome, "Delete", false, false, false), None);
        assert_eq!(key_act(Keys::Chrome, "b", false, false, false), Some(KeyAct::Tool(Tool::Brush)));
        assert_eq!(key_act(Keys::Chrome, "B", false, true, false), Some(KeyAct::Tool(Tool::Highlighter)));
        assert_eq!(key_act(Keys::Chrome, "r", false, false, false), Some(KeyAct::Tool(Tool::Smudge)));
        assert_eq!(key_act(Keys::Chrome, "R", false, true, false), Some(KeyAct::Tool(Tool::Blur)));
        assert_eq!(key_act(Keys::Chrome, "g", false, false, false), Some(KeyAct::Tool(Tool::Fill)));
        assert_eq!(key_act(Keys::Chrome, "m", false, false, false), Some(KeyAct::Tool(Tool::SelectRect)));
        // Shift+H and Shift+V flip; plain H and V are their tools.
        assert_eq!(key_act(Keys::Chrome, "H", false, true, false), Some(KeyAct::Flip(FlipAxis::Horizontal)));
        assert_eq!(key_act(Keys::Chrome, "V", false, true, false), Some(KeyAct::Flip(FlipAxis::Vertical)));
        assert_eq!(key_act(Keys::Chrome, "h", false, false, false), Some(KeyAct::Tool(Tool::Hand)));
        assert_eq!(key_act(Keys::Chrome, "v", false, false, false), Some(KeyAct::Tool(Tool::Select)));
        // The app's own shortcuts win: nothing with Ctrl or Alt.
        assert_eq!(key_act(Keys::Chrome, "r", true, false, false), None);
        assert_eq!(key_act(Keys::Chrome, "r", false, false, true), None);
        assert_eq!(key_act(Keys::Chrome, "g", true, false, false), None);
        assert_eq!(key_act(Keys::Layers, "g", true, false, false), Some(KeyAct::GroupLayers));
        assert_eq!(key_act(Keys::Layers, "r", false, false, false), None);
        // Where people type, and on the canvas (its surface hears them),
        // nothing is taken.
        for keys in [Keys::Typing, Keys::Canvas] {
            assert_eq!(key_act(keys, "x", false, false, false), None);
            assert_eq!(key_act(keys, "g", true, false, false), None);
        }
    }

    #[test]
    fn swapping_colors() {
        assert_eq!(swapped("#ff0000", Some("#00ff00")), ("#00ff00".to_string(), Some("#ff0000".to_string())));
        // No fill: the stroke becomes the fill too.
        assert_eq!(swapped("#ff0000", None), ("#ff0000".to_string(), Some("#ff0000".to_string())));
    }

    #[test]
    fn recent_colors_are_newest_first_without_repeats() {
        let r = remember_recent(&[], "#111111");
        let r = remember_recent(&r, "#222222");
        let r = remember_recent(&r, "#111111");
        assert_eq!(r, vec!["#111111".to_string(), "#222222".to_string()]);
        let mut r = Vec::new();
        for i in 0..20 {
            r = remember_recent(&r, &format!("#{i:06}"));
        }
        assert_eq!(r.len(), RECENT);
        assert_eq!(r[0], "#000019");
        // Case does not make a color new.
        assert_eq!(remember_recent(&["#ABCDEF".to_string()], "#abcdef").len(), 1);
    }
}
