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
use super::tools::{Align2, Order};
use super::*;
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
pub const FLYOUTS: [&[Tool]; 2] = [
    // Shapes (Draw).
    &[Tool::Rect, Tool::Ellipse, Tool::Diamond, Tool::Line, Tool::Arrow],
    // More diagram tools.
    &[Tool::Connector, Tool::Step, Tool::Frame],
];

/// The rail, top to bottom: Select and Hand; Draw (the pen, which becomes
/// the Pencil with paths, the highlighter, which moves to Paint with paint
/// layers, the shapes, the eraser); Diagram (text, sticky notes, the other
/// diagram tools, discussion pins); pictures.
pub const RAIL: [Slot; 13] = [
    Slot::Tool(Tool::Select),
    Slot::Tool(Tool::Hand),
    Slot::Gap,
    Slot::Tool(Tool::Pen),
    Slot::Tool(Tool::Highlighter),
    Slot::Flyout(0),
    Slot::Tool(Tool::Eraser),
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
    format!("{} ({})", tool.name(), tool.key().to_ascii_uppercase())
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
}

impl Controls {
    pub fn any(&self) -> bool {
        self.stroke || self.fill || self.width || self.dash || self.heads || self.font || self.align || self.opacity
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
        Kind::Line => Controls { stroke: true, width: true, dash: true, opacity: true, ..none },
        Kind::Arrow | Kind::Connector => Controls { stroke: true, width: true, dash: true, heads: true, opacity: true, ..none },
        Kind::Rect | Kind::Ellipse | Kind::Diamond | Kind::Polygon | Kind::Star => {
            Controls { stroke: true, fill: true, width: true, dash: true, font: true, align: true, opacity: true, ..none }
        }
        Kind::Path => Controls { stroke: true, fill: true, width: true, dash: true, opacity: true, ..none },
        Kind::Text | Kind::Callout => Controls { stroke: true, fill: true, font: true, align: true, opacity: true, ..none },
        // A sticky note's color is its fill.
        Kind::Sticky => Controls { fill: true, font: true, align: true, opacity: true, ..none },
        Kind::Step => Controls { stroke: true, font: true, opacity: true, ..none },
        Kind::Spotlight => Controls { opacity: true, ..none },
        Kind::Frame | Kind::Picture | Kind::Group | Kind::Card | Kind::Other(_) => none,
    }
}

/// What a selection of these kinds can change: whatever any of them can.
pub fn controls_for(kinds: &[Kind]) -> Controls {
    kinds.iter().fold(Controls::default(), |c, k| c.or(controls_of(k)))
}

/// What the context bar says when the tool has nothing to set.
pub fn hint(tool: Tool) -> &'static str {
    match tool {
        Tool::Select => "Click to select, drag to select several",
        Tool::Hand => "Drag to move around (Space held does it with any tool)",
        Tool::Eraser => "Drag across marks to erase them",
        Tool::Pin => "Click to start a discussion there",
        Tool::Frame => "Drag out a frame; what it covers goes with it",
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
}

/// What a key does while `keys` has the board's keys (`None`: not ours).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAct {
    Tool(Tool),
    Swap,
    TogglePanels,
    GroupLayers,
}

/// The chrome's key judgement, pure: plain letters, X and Tab from the
/// chrome; Ctrl+G from the layers panel.
pub fn key_act(keys: Keys, key: &str, ctrl: bool, shift: bool, alt: bool) -> Option<KeyAct> {
    match keys {
        Keys::Chrome if !ctrl && !alt => match key {
            "Tab" => Some(KeyAct::TogglePanels),
            "x" | "X" => Some(KeyAct::Swap),
            k => Tool::from_key(k).map(KeyAct::Tool),
        },
        Keys::Layers if ctrl && !shift && !alt && key.eq_ignore_ascii_case("g") => Some(KeyAct::GroupLayers),
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
    if !matches!(keys, Keys::Chrome | Keys::Layers) || untracked(|| ui.renaming.get()).is_some() {
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
pub(super) fn set_color(store: AppStore, pane: PaneId, well: Well, hex: Option<&str>) {
    let st = store.board_style;
    match (well, hex) {
        (Well::Stroke, Some(hex)) => {
            let Some(c) = Color::parse(hex) else { return };
            st.stroke.set(c.to_hex());
            tools::apply_style(store, pane, Style { stroke: Some(c), ..Style::default() });
            st.recent.update(|r| *r = remember_recent(r, &c.to_hex()));
        }
        (Well::Stroke, None) => {}
        (Well::Fill, Some(hex)) => {
            let Some(c) = Color::parse(hex) else { return };
            st.fill.set(Some(c.to_hex()));
            tools::apply_style(store, pane, Style { fill: Some(c), ..Style::default() });
            st.recent.update(|r| *r = remember_recent(r, &c.to_hex()));
        }
        (Well::Fill, None) => {
            st.fill.set(None);
            // An empty style is "clear the fill" (`tools::apply_style`).
            tools::apply_style(store, pane, Style::default());
        }
    }
}

/// X: swap the stroke and fill colors, of new elements and the selection.
pub(super) fn swap_colors(store: AppStore, pane: PaneId) {
    let st = store.board_style;
    let (stroke, fill) = untracked(|| swapped(&st.stroke.get(), st.fill.get().as_deref()));
    st.stroke.set(stroke.clone());
    st.fill.set(fill.clone());
    tools::apply_style(
        store,
        pane,
        Style { stroke: Color::parse(&stroke), fill: fill.as_deref().and_then(Color::parse), ..Style::default() },
    );
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
fn picker_button(__scope: &mut RenderScope, ui: PaneUi, tip: &'static str, popup: Popup, face: NodeHandle) -> NodeHandle {
    rsx! {
        div {
            title: tip,
            class: {move || if ui.popup.get() == popup { "pimble-board-tool pimble-board__chooser pimble-board-tool--active" } else { "pimble-board-tool pimble-board__chooser" }},
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
                    span { class: "pimble-board__menu-key", {tool.key().to_ascii_uppercase().to_string()} }
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
            store.board_tool.get().kind().map(|k| controls_of(&k)).unwrap_or_default()
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
            style: {move || format!("border-color: {};", st.stroke.get())},
            onclick: move || {
                st.front.set(Well::Stroke);
                ui.popup.set(Popup::Well);
            },
        }
    };
    let fill_swatch = rsx! {
        div {
            title: "Fill color",
            class: {move || if st.fill.get().is_some() { "pimble-board__ctx-color" } else { "pimble-board__ctx-color pimble-board-swatch--none" }},
            style: {move || st.fill.get().map(|f| format!("background: {f};")).unwrap_or_default()},
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

    // The selection's arrangement.
    let act = |__scope: &mut RenderScope, icon: TablerIcon, tip: &'static str, run: fn(AppStore, PaneId)| icon_button(__scope, icon, tip.to_string(), || false, move || run(store, pane));
    let duplicate = act(__scope, TablerIcon::Copy, "Duplicate (Ctrl+D)", tools::duplicate);
    let group = act(__scope, TablerIcon::BoxMultiple, "Group (Ctrl+G)", tools::group);
    let ungroup = act(__scope, TablerIcon::Package, "Ungroup (Ctrl+Shift+G)", tools::ungroup);
    let lock = act(__scope, TablerIcon::Lock, "Lock / Unlock (Ctrl+L)", tools::toggle_lock);
    let delete = act(__scope, TablerIcon::Trash, "Delete (Delete)", delete_selected);
    let arrange_btn = icon_button(__scope, TablerIcon::Dots, "More: forward, backward, front, back".to_string(), move || ui.popup.get() == Popup::Arrange, move || toggle(ui, Popup::Arrange));
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
        span { class: "pimble-board__face-text", {move || { let size = st.font_size.get(); FONT_SIZES.iter().find(|(_, s)| *s == size).map(|(l, _)| l.to_string()).unwrap_or_else(|| format!("{size}")) }} }
    };
    let opacity_face = rsx! {
        span { class: "pimble-board__face-text", {move || format!("{}%", (st.opacity.get() * 100.0).round())} }
    };
    let width_btn = picker_button(__scope, ui, "Width", Popup::Width, width_face);
    let dash_btn = picker_button(__scope, ui, "Line style", Popup::Dash, dash_face);
    let heads_btn = picker_button(__scope, ui, "Arrow heads", Popup::Heads, heads_face);
    let font_btn = picker_button(__scope, ui, "Font size", Popup::Font, font_face);
    let align_text_btn = picker_button(__scope, ui, "Text alignment", Popup::TextAlign, align_face);
    let opacity_btn = picker_button(__scope, ui, "Opacity", Popup::Opacity, opacity_face);
    let more_items: Vec<NodeHandle> = vec![
        menu_item(__scope, store, pane, TablerIcon::ChevronUp, "Bring Forward (Ctrl+])", Box::new(|s, p| tools::reorder(s, p, Order::Forward))),
        menu_item(__scope, store, pane, TablerIcon::ChevronDown, "Send Backward (Ctrl+[)", Box::new(|s, p| tools::reorder(s, p, Order::Backward))),
        menu_item(__scope, store, pane, TablerIcon::StackPush, "Bring to Front (Ctrl+Shift+])", Box::new(|s, p| tools::reorder(s, p, Order::Front))),
        menu_item(__scope, store, pane, TablerIcon::StackPop, "Send to Back (Ctrl+Shift+[)", Box::new(|s, p| tools::reorder(s, p, Order::Back))),
    ];
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
                    div { class: "pimble-board__ctx-group pimble-board__ctx-sep", style: {move || shown(controls().width || controls().dash || controls().heads || controls().font || controls().align || controls().opacity)},
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().width)}, {width_btn} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().dash)}, {dash_btn} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().heads)}, {heads_btn} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().font)}, {font_btn} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().align)}, {align_text_btn} }
                        div { class: "pimble-board__ctx-group", style: {move || shown(controls().opacity)}, {opacity_btn} }
                    }
                    div {
                        class: "pimble-board__ctx-group pimble-board__ctx-sep",
                        style: {move || shown(ui.selected.get() > 0)},
                        {duplicate}
                        div { class: "pimble-board__ctx-group", style: {move || shown(ui.selected.get() > 1)}, {align_btn} {group} }
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
            div { class: "pimble-float pimble-board__options", style: {move || shown(ui.popup.get() == Popup::Font)}, {sizes} }
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
        }
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
    rsx! {
        div {
            class: "pimble-board__well-wrap",
            onmousedown: move || crate::pane_view::press_in_view(),
            div {
                class: "pimble-float pimble-board__well-pop",
                style: {move || if ui.popup.get() == Popup::Well { "" } else { "display: none;" }},
                onmousedown: move || keys_to(store, pane, Keys::Typing),
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
                ColorPicker {
                    value_fn: move || if st.front.get() == Well::Fill { st.fill.get().unwrap_or_else(|| st.stroke.get()) } else { st.stroke.get() },
                    swatches: palette(),
                    swatches_per_row: 7,
                    size: "sm",
                    with_input: true,
                    onchange: move |hex: String| set_color(store, pane, untracked(|| st.front.get()), Some(&hex)),
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
                                move || set_color(store, pane, untracked(|| st.front.get()), Some(&color))
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
                            st.fill.get().map(|f| format!("background: {f};")).unwrap_or_default(),
                            if st.front.get() == Well::Fill { 2 } else { 1 }
                        )},
                        onclick: move || press(Well::Fill),
                    }
                    div {
                        title: "Stroke (click again for the picker)",
                        class: "pimble-board__well-stroke",
                        style: {move || format!("border-color: {}; z-index: {};", st.stroke.get(), if st.front.get() == Well::Stroke { 2 } else { 1 })},
                        onclick: move || press(Well::Stroke),
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
    let export_btn = icon_button(__scope, TablerIcon::Download, "Copy and save the canvas as a picture".to_string(), move || ui.popup.get() == Popup::Export, move || toggle(ui, Popup::Export));
    let export_items: Vec<NodeHandle> = vec![
        menu_item(__scope, store, pane, TablerIcon::ClipboardCopy, "Copy Picture (Ctrl+C)", Box::new(export::copy_picture)),
        menu_item(__scope, store, pane, TablerIcon::Photo, "Save as PNG...", Box::new(|s, p| export::save_picture(s, p, Format::Png, false))),
        menu_item(__scope, store, pane, TablerIcon::Photo, "Save as JPEG...", Box::new(|s, p| export::save_picture(s, p, Format::Jpeg, false))),
        menu_item(__scope, store, pane, TablerIcon::FileExport, "Save as SVG...", Box::new(|s, p| export::save_picture(s, p, Format::Svg, false))),
        menu_item(__scope, store, pane, TablerIcon::Eye, "Save with All Markup (PNG)...", Box::new(|s, p| export::save_picture(s, p, Format::Png, true))),
        menu_item(__scope, store, pane, TablerIcon::PhotoDown, "Save Original As...", Box::new(export::save_original)),
        menu_item(__scope, store, pane, TablerIcon::Stack2, "Flatten into New Canvas", Box::new(export::flatten)),
    ];
    rsx! {
        div {
            class: "pimble-board__corner",
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
        assert_eq!(faces, vec![Tool::Rect, Tool::Connector]);
        let faces = remember_face(&faces, Tool::Ellipse);
        assert_eq!(faces, vec![Tool::Ellipse, Tool::Connector]);
        // A tool outside every flyout changes no face.
        assert_eq!(remember_face(&faces, Tool::Pen), faces);
        let faces = remember_face(&faces, Tool::Frame);
        assert_eq!(faces, vec![Tool::Ellipse, Tool::Frame]);
        // Faces of the wrong shape start again from the defaults.
        assert_eq!(remember_face(&[], Tool::Step), vec![Tool::Rect, Tool::Step]);
        assert_eq!(flyout_of(Tool::Arrow), Some(0));
        assert_eq!(flyout_of(Tool::Text), None);
    }

    #[test]
    fn tips_name_the_letter() {
        assert_eq!(tip(Tool::Rect), "Rectangle (R)");
        assert_eq!(tip(Tool::Pin), "Discussion pin (Q)");
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
        // Every tool that makes something has controls, but the frame.
        for tool in Tool::ALL {
            if let Some(kind) = tool.kind() {
                assert_eq!(controls_of(&kind).any(), kind != Kind::Frame, "{tool:?}");
            }
        }
    }

    #[test]
    fn the_chrome_hears_the_canvas_keys() {
        assert_eq!(key_act(Keys::Chrome, "r", false, false, false), Some(KeyAct::Tool(Tool::Rect)));
        assert_eq!(key_act(Keys::Chrome, "X", false, true, false), Some(KeyAct::Swap));
        assert_eq!(key_act(Keys::Chrome, "Tab", false, false, false), Some(KeyAct::TogglePanels));
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
