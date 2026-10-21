//! The board view's DOM (docs/IMAGE_MARKUP_CONTRACT.md sections 1, 4, 5, 6
//! and 8): the surface and what floats over it (selection outlines and
//! handles, the marquee, the text input, the attribution under the pointer,
//! the markup control), the side panel (the navigator and its frames, the
//! style controls, the layers), and the board tools in the one toolbar.
//! Rendered once per pane slot; everything that changes is a signal.

use super::export::Format;
use super::tools::{Align2, Order};
use super::*;
use crate::appearance::IconGlyph;

thread_local! {
    /// Each pane's text input: (document key, node id), for focusing it.
    static TEXT_INPUTS: RefCell<[Option<(u64, usize)>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
    /// Each pane's layer rename input.
    static RENAME_INPUTS: RefCell<[Option<(u64, usize)>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
    /// Nodes a card asked about once already (`GetNode`).
    static CARD_ASKED: RefCell<HashSet<(StoreId, NodeId)>> = RefCell::new(HashSet::new());
}

/// Start renaming layer `id` (named `name`) in pane `pane`'s layers panel:
/// Enter keeps the new name, Escape drops it.
fn begin_rename(pane: PaneId, ui: PaneUi, id: &str, name: &str) {
    ui.rename_text.set(name.to_string());
    ui.renaming.set(Some(id.to_string()));
    rinch::core::set_keyboard_interceptor(move |data| match data.key.as_str() {
        "Enter" => {
            run_on_main_thread(move || finish_rename(pane, ui, true));
            true
        }
        "Escape" => {
            run_on_main_thread(move || finish_rename(pane, ui, false));
            true
        }
        _ => false,
    });
    let _ = set_timeout(0, move || {
        if let Some((doc_key, node)) = RENAME_INPUTS.with(|t| t.borrow()[pane.index()]) {
            rinch::core::request_focus(doc_key, node);
        }
    });
    focus_in_browser("input.pimble-board__layer-name-input");
}

/// End a layer rename, keeping the typed name when `keep`.
fn finish_rename(pane: PaneId, ui: PaneUi, keep: bool) {
    rinch::core::clear_keyboard_interceptor();
    let Some(id) = untracked(|| ui.renaming.get()) else { return };
    ui.renaming.set(None);
    let name = untracked(|| ui.rename_text.get());
    if keep && !name.trim().is_empty() {
        let Some(store) = APP.with(Cell::get) else { return };
        new_undo_step(pane);
        local_edit(store, pane, |t| t.rename_layer(&id, name.trim()));
    }
}

/// Put the keyboard in pane `pane`'s text input.
pub(super) fn focus_text_input(pane: PaneId) {
    if let Some((doc_key, node)) = TEXT_INPUTS.with(|t| t.borrow()[pane.index()]) {
        rinch::core::request_focus(doc_key, node);
    }
    focus_in_browser("textarea.pimble-board__text-input");
}

/// Typing ended: the text input lets go of the keyboard. (On the desktop the
/// board hears keys again from its next press: rinch has no way to give a
/// surface the keyboard from code, only a press on it does.)
pub(super) fn release_text_input(pane: PaneId) {
    if let Some((doc_key, node)) = TEXT_INPUTS.with(|t| t.borrow()[pane.index()]) {
        rinch::core::post_release_request(doc_key, node);
    }
    #[cfg(not(feature = "native"))]
    {
        use wasm_bindgen::JsCast;
        if let Some(el) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.active_element())
            .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
        {
            if el.class_name().contains("pimble-board__text-input") {
                let _ = el.blur();
            }
        }
    }
}

/// The browser: focus the shown element matching `selector` once the press
/// that asked for it is over (the canvas keeps the focus of its own press,
/// and rinch-web's focus request does not reach a field it shows that late).
fn focus_in_browser(selector: &'static str) {
    #[cfg(not(feature = "native"))]
    {
        use wasm_bindgen::JsCast;
        let _ = set_timeout(40, move || {
            let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
            let Ok(list) = document.query_selector_all(selector) else { return };
            for i in 0..list.length() {
                let Some(el) = list.get(i).and_then(|n| n.dyn_into::<web_sys::HtmlElement>().ok()) else { continue };
                if el.offset_width() > 0 {
                    let _ = el.focus();
                    return;
                }
            }
        });
    }
    #[cfg(feature = "native")]
    let _ = selector;
}

/// Publish the node cards on pane `pane`'s board, whose live titles and
/// colors an effect in [`render_board`] reads from the node cache.
pub(super) fn refresh_card_labels(store: AppStore, pane: PaneId) {
    let cards: Vec<(StoreId, NodeId, String)> = with_session(pane, |s| {
        s.snapshot()
            .elements
            .iter()
            .filter_map(|e| match &e.raw.geom {
                Geom::Card { link, .. } => pimble_core::PimbleUrl::parse(link).map(|u| (u.store, u.node, link.clone())),
                _ => None,
            })
            .collect()
    })
    .unwrap_or_default();
    store.pane(pane).board_ui.cards.set_if_changed(cards);
}

fn css_align(align: Align) -> &'static str {
    match align {
        Align::Left => "left",
        Align::Center => "center",
        Align::Right => "right",
    }
}

/// Pane `pane`'s board view: the canvas (the surface and what floats over
/// it) and the side panel. Rendered once per pane slot.
pub(crate) fn render_board(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let state = store.pane(pane);
    let ui = state.board_ui;
    let style = store.board_style;
    let surface = surface(store, pane);
    let read_only = move || {
        state
            .board
            .get()
            .is_some_and(|held| !store.node_access_tracked(held.store_id, held.node_id).allows_write())
    };

    // The text being typed: a real input over the surface (contract section
    // 4), its box and size following the element on screen.
    let text_input = rsx! {
        textarea {
            class: "pimble-board__text-input",
            spellcheck: "false",
            style: {move || match ui.text_box.get() {
                Some(b) => format!(
                    "left: {:.1}px; top: {:.1}px; width: {:.1}px; height: {:.1}px; font-size: {:.1}px; color: {}; background: {}; text-align: {};",
                    b.x, b.y, b.w, b.h, b.font_px, b.color, b.background, css_align(b.align)
                ),
                None => "display: none;".to_string(),
            }},
            value: move || ui.text_value.get().1,
            oninput: move |value: String| tools::text_input(store, pane, value),
        }
    };
    TEXT_INPUTS.with(|t| t.borrow_mut()[pane.index()] = Some((text_input.doc_key(), text_input.node_id().0)));

    let handle = |__scope: &mut RenderScope, class: &'static str| rsx! { div { class: class } };
    let handles: Vec<NodeHandle> = [
        "pimble-board__handle pimble-board__handle--nw",
        "pimble-board__handle pimble-board__handle--n",
        "pimble-board__handle pimble-board__handle--ne",
        "pimble-board__handle pimble-board__handle--e",
        "pimble-board__handle pimble-board__handle--se",
        "pimble-board__handle pimble-board__handle--s",
        "pimble-board__handle pimble-board__handle--sw",
        "pimble-board__handle pimble-board__handle--w",
        "pimble-board__handle pimble-board__handle--rotate",
    ]
    .into_iter()
    .map(|class| handle(__scope, class))
    .collect();

    let panel = render_panel(__scope, store, pane);

    // Node cards show their node's title and color live (contract section
    // 4): read from the node cache, tracked, so a rename shows at once.
    let _ = rinch::Effect::new(move || {
        let cards = ui.cards.get();
        let mut labels = HashMap::new();
        for (store_id, node_id, link) in cards {
            let sig = store.node_data.with(|map| map.get(&(store_id, node_id)).copied());
            match sig {
                Some(sig) => {
                    let (title, color) = sig.with(|n| (n.metadata.title.clone(), n.metadata.color().map(str::to_string)));
                    let title = if title.trim().is_empty() { "Untitled".to_string() } else { title };
                    labels.insert(link, CardLabel { title, color: color.as_deref().and_then(Color::parse) });
                }
                None => {
                    if CARD_ASKED.with(|a| a.borrow_mut().insert((store_id, node_id))) {
                        untracked(|| store.send(BackendCommand::GetNode { store_id, node_id }));
                    }
                }
            }
        }
        untracked(|| set_card_labels(store, pane, labels));
    });

    let canvas = rsx! {
    div {
        class: "pimble-board__canvas",
        // A tree row dropped here becomes a node card (the browser's
        // drop; the desktop's arrives as the surface's own `Drop`).
        ondrop: move || tools::drop_tree_row(store, pane),
        RenderSurface { surface: Some(surface) }
        for (x, y, w, h) in ui.outlines.get() {
            div {
                key: format!("{x:.1} {y:.1} {w:.1} {h:.1}"),
                class: "pimble-board__outline",
                style: format!("left: {x:.1}px; top: {y:.1}px; width: {w:.1}px; height: {h:.1}px;"),
            }
        }
        div {
            class: "pimble-board__selection",
            style: {move || match ui.handles.get() {
                Some((x, y, w, h)) if !read_only() => format!("left: {x:.1}px; top: {y:.1}px; width: {w:.1}px; height: {h:.1}px;"),
                _ => "display: none;".to_string(),
            }},
            {handles}
        }
        div {
            class: "pimble-board__marquee",
            style: {move || match ui.marquee.get() {
                Some((x, y, w, h)) => format!("left: {x:.1}px; top: {y:.1}px; width: {w:.1}px; height: {h:.1}px;"),
                None => "display: none;".to_string(),
            }},
        }
        {text_input}
        div {
            class: "pimble-board__tip",
            style: {move || match ui.tip.get() {
                Some((x, y, _)) => format!("left: {x:.1}px; top: {y:.1}px;"),
                None => "display: none;".to_string(),
            }},
            {move || ui.tip.get().map(|(_, _, text)| text).unwrap_or_default()}
        }
        // The floating markup control (contract section 5): shown
        // while the pointer is over the board, and always (dimmed)
        // while the markup is hidden.
        div {
            class: {move || {
                let (_, visible) = state.board_marks.get();
                match (visible, ui.hover.get()) {
                    (false, _) => "pimble-board__markup pimble-board__markup--hidden",
                    (true, true) => "pimble-board__markup pimble-board__markup--shown",
                    (true, false) => "pimble-board__markup",
                }
            }},
            style: {move || if state.board_marks.get().0 > 0 { "" } else { "display: none;" }},
            title: {move || if read_only() { "You can read this, not change it." } else { "Show or hide the markup, for everyone" }},
            onclick: move || {
                if !read_only() {
                    toggle_markup(store, pane);
                }
            },
            span {
                class: "pimble-board__markup-icon",
                if state.board_marks.get().1 {
                    IconGlyph { name: TablerIcon::Eye.name().to_string() }
                } else {
                    IconGlyph { name: TablerIcon::EyeOff.name().to_string() }
                }
            }
            span {
                {move || {
                    let (count, visible) = state.board_marks.get();
                    if visible { format!("Markup · {count}") } else { "Markup hidden".to_string() }
                }}
            }
        }
    }
    };
    // Marking up a picture in text (contract section 2): "Done" goes back
    // to the text.
    let done = rsx! {
        div {
            class: "pimble-board__done",
            style: {move || if state.markup.get() { "" } else { "display: none;" }},
            Button {
                size: "xs",
                onclick: move || end_markup(store, pane),
                "Done"
            }
        }
    };
    // The desktop's pixel scale reads the box the surface fills.
    BOUNDS.with(|b| b.borrow_mut()[pane.index()] = Some(canvas.bounds_signal()));
    let view = rsx! {
        div {
            class: "pimble-board",
            style: {move || if state.show_board.get() { "" } else { "display: none;" }},
            {canvas}
            {done}
            {panel}
        }
    };
    // The theme's background, on every board.
    let _ = rinch::Effect::new(move || {
        let dark = store.dark_mode.get();
        untracked(|| set_dark_mode(dark));
    });
    // The panel shown or hidden: the navigator draws once it has room.
    let _ = rinch::Effect::new(move || {
        if style.panel.get() {
            untracked(|| {
                let _ = set_timeout(50, move || nav::changed(store, pane));
            });
        }
    });
    view
}

// ── The side panel ──────────────────────────────────────────────────────────

fn render_panel(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let state = store.pane(pane);
    let ui = state.board_ui;
    let style = store.board_style;
    let nav_surface = nav::surface(store, pane);
    let writable = move || {
        state
            .board
            .get()
            .is_some_and(|held| store.node_access_tracked(held.store_id, held.node_id).allows_write())
    };
    let style_section = render_style(__scope, store, pane);
    let layers_section = render_layers(__scope, store, pane);
    rsx! {
        div {
            class: "pimble-board__panel",
            style: {move || if style.panel.get() { "" } else { "display: none;" }},
            div { class: "pimble-board__panel-title", "Navigator" }
            div {
                class: "pimble-board__nav",
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
                class: "pimble-board__frames",
                for (id, name) in ui.frames.get() {
                    div {
                        key: format!("{id} {name}"),
                        class: "pimble-board__frame-row",
                        title: "Show this frame",
                        onclick: {
                            let id = id.clone();
                            move || {
                                let rect = with_session(pane, |s| {
                                    s.snapshot().get(&id).map(|e| {
                                        let r = e.pivot_bounds;
                                        Rect::new(r.x + e.world.tx, r.y + e.world.ty, r.w * e.world.sx, r.h * e.world.sy)
                                    })
                                })
                                .flatten();
                                if let Some(rect) = rect {
                                    show_rect(store, pane, rect);
                                }
                            }
                        },
                        IconGlyph { name: TablerIcon::Frame.name().to_string() }
                        span { {name.clone()} }
                    }
                }
            }
            div {
                style: {move || if writable() { "" } else { "opacity: 0.45; pointer-events: none;" }},
                {style_section}
                {layers_section}
            }
        }
    }
}

/// A small square button in the panel or the toolbar.
fn icon_button(
    __scope: &mut RenderScope,
    icon: TablerIcon,
    tip: String,
    active: impl Fn() -> bool + 'static,
    run: impl Fn() + 'static,
) -> NodeHandle {
    rsx! {
        div {
            title: tip,
            class: {|| if active() { "pimble-board-tool pimble-board-tool--active" } else { "pimble-board-tool" }},
            onclick: move || run(),
            IconGlyph { name: icon.name().to_string() }
        }
    }
}

/// A text button in the panel.
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

/// The style controls (contract section 4, "Style"): what new elements
/// take, and what a change writes to every selected element.
fn render_style(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let st = store.board_style;
    let picker_open = Signal::new(false);
    let apply = move |s: Style| tools::apply_style(store, pane, s);

    let strokes: Vec<NodeHandle> = palette()
        .into_iter()
        .map(|color| {
            let c2 = color.clone();
            let c3 = color.clone();
            let tip = color.clone();
            let background = format!("background: {color};");
            rsx! {
                div {
                    title: tip,
                    class: {move || if st.stroke.get().eq_ignore_ascii_case(&c2) { "pimble-board-swatch pimble-board-swatch--active" } else { "pimble-board-swatch" }},
                    style: background,
                    onclick: move || {
                        st.stroke.set(c3.clone());
                        apply(Style { stroke: Color::parse(&c3), ..Style::default() });
                    },
                }
            }
        })
        .collect();
    let fills: Vec<NodeHandle> = palette()
        .into_iter()
        .map(|color| {
            let c2 = color.clone();
            let c3 = color.clone();
            let tip = color.clone();
            let background = format!("background: {color};");
            rsx! {
                div {
                    title: tip,
                    class: {move || if st.fill.get().is_some_and(|f| f.eq_ignore_ascii_case(&c2)) { "pimble-board-swatch pimble-board-swatch--active" } else { "pimble-board-swatch" }},
                    style: background,
                    onclick: move || {
                        st.fill.set(Some(c3.clone()));
                        apply(Style { fill: Color::parse(&c3), ..Style::default() });
                    },
                }
            }
        })
        .collect();
    let no_fill = rsx! {
        div {
            title: "No fill",
            class: {move || if st.fill.get().is_none() { "pimble-board-swatch pimble-board-swatch--none pimble-board-swatch--active" } else { "pimble-board-swatch pimble-board-swatch--none" }},
            onclick: move || {
                st.fill.set(None);
                // An empty style is "clear the fill" (`tools::apply_style`).
                apply(Style::default());
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
                apply(Style { dash: Some(dash), ..Style::default() });
            })
        })
        .collect();
    let sizes: Vec<NodeHandle> = FONT_SIZES
        .iter()
        .map(|(label, size)| {
            let size = *size;
            text_button(__scope, label, "Font size", move || st.font_size.get() == size, move || {
                st.font_size.set(size);
                apply(Style { font_size: Some(size), ..Style::default() });
            })
        })
        .collect();
    let aligns: Vec<NodeHandle> = [(Align::Left, TablerIcon::AlignLeft, "Align left"), (Align::Center, TablerIcon::AlignCenter, "Center"), (Align::Right, TablerIcon::AlignRight, "Align right")]
        .into_iter()
        .map(|(align, icon, tip)| {
            icon_button(__scope, icon, tip.to_string(), move || st.align.get() == align, move || {
                st.align.set(align);
                apply(Style { align: Some(align), ..Style::default() });
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
            apply(Style { arrow_heads: Some(h), ..Style::default() });
        })
    })
    .collect();

    rsx! {
        div {
            class: "pimble-board__section",
            div { class: "pimble-board__panel-title", "Style" }
            div { class: "pimble-board__label", "Color" }
            div {
                class: "pimble-board__swatches",
                {strokes}
                div {
                    title: "Pick any color",
                    class: "pimble-board-swatch pimble-board-swatch--picker",
                    onclick: move || picker_open.update(|v| *v = !*v),
                    IconGlyph { name: TablerIcon::ColorPicker.name().to_string() }
                }
            }
            div {
                class: "pimble-board__picker",
                style: {move || if picker_open.get() { "" } else { "display: none;" }},
                ColorPicker {
                    value_fn: move || st.stroke.get(),
                    swatches: palette(),
                    swatches_per_row: 7,
                    size: "sm",
                    onchange: move |hex: String| {
                        if let Some(c) = Color::parse(&hex) {
                            st.stroke.set(c.to_hex());
                            apply(Style { stroke: Some(c), ..Style::default() });
                        }
                    },
                }
            }
            div { class: "pimble-board__label", "Fill" }
            div { class: "pimble-board__swatches", {no_fill} {fills} }
            div { class: "pimble-board__label", "Width" }
            div { class: "pimble-board__row", {widths} }
            div { class: "pimble-board__label", "Line" }
            div { class: "pimble-board__row", {dashes} }
            div { class: "pimble-board__label", "Arrow heads" }
            div { class: "pimble-board__row", {heads} }
            div { class: "pimble-board__label", {move || format!("Opacity {}%", (st.opacity.get() * 100.0).round())} }
            Slider {
                min: 0.1,
                max: 1.0,
                step: 0.05,
                size: "sm",
                value_signal: Some(opacity_signal(store)),
                onchange: move |v: f64| {
                    st.opacity.set(v as f32);
                    apply(Style { opacity: Some(v as f32), ..Style::default() });
                },
            }
            div { class: "pimble-board__label", "Text" }
            div { class: "pimble-board__row", {sizes} }
            div { class: "pimble-board__row", {aligns} }
        }
    }
}

/// The opacity as the slider's `f64` signal, kept in step with the style's.
fn opacity_signal(store: AppStore) -> Signal<f64> {
    thread_local! {
        static SIGNAL: Cell<Option<Signal<f64>>> = const { Cell::new(None) };
    }
    if let Some(sig) = SIGNAL.with(Cell::get) {
        return sig;
    }
    let sig = Signal::new(1.0f64);
    let st = store.board_style;
    let _ = rinch::Effect::new(move || {
        let v = st.opacity.get() as f64;
        untracked(|| sig.set_if_changed(v));
    });
    SIGNAL.with(|s| s.set(Some(sig)));
    sig
}

/// The layers panel (contract section 5): rows top first, each with its
/// visibility, lock, name (renamed in place), element count; the picked
/// row is where new elements go, and its opacity is below the list. Drag a
/// row onto another to put it there.
fn render_layers(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let ui = store.pane(pane).board_ui;
    let drag: DragContext<String> = DragContext::new();
    let layer_opacity = Signal::new(1.0f64);
    // The picked layer's opacity, shown on the slider.
    let _ = rinch::Effect::new(move || {
        let layers = ui.layers.get();
        let picked = ui.layer.get();
        let opacity = picked.and_then(|id| layers.iter().find(|l| l.id == id).map(|l| l.opacity)).unwrap_or(1.0);
        untracked(|| layer_opacity.set_if_changed(opacity as f64));
    });
    let picked = move || ui.layer.get().or_else(|| ui.layers.get().iter().find(|l| !l.locked).map(|l| l.id.clone()));

    // Renaming a layer: one input over the list, given the keyboard when a
    // rename starts; Enter keeps the name, Escape (the board's) drops it.
    let rename_input = rsx! {
        input {
            r#type: "text",
            class: "pimble-board__layer-name-input",
            value: move || ui.rename_text.get(),
            oninput: move |v: String| ui.rename_text.set(v),
            onsubmit: move || finish_rename(pane, ui, true),
        }
    };
    RENAME_INPUTS.with(|t| t.borrow_mut()[pane.index()] = Some((rename_input.doc_key(), rename_input.node_id().0)));

    let new_layer = text_button(__scope, "New", "New Layer (above the picked one)", || false, move || {
        let count = untracked(|| ui.layers.get().len());
        let above = untracked(|| ui.layer.get());
        new_undo_step(pane);
        let made = local_edit(store, pane, |t| {
            let placement = match &above {
                Some(id) => Placement::Above(id.clone()),
                None => Placement::Top,
            };
            t.add_layer(&format!("Layer {}", count + 1), placement)
        });
        if let Some(id) = made {
            ui.layer.set(Some(id));
        }
    });
    let delete_layer = text_button(__scope, "Delete", "Delete Layer (with its elements)", || false, move || {
        let Some(id) = untracked(|| picked()) else { return };
        if untracked(|| ui.layers.get().len()) <= 1 {
            crate::events::show_notice(store, "A board keeps at least one layer.".to_string());
            return;
        }
        new_undo_step(pane);
        local_edit(store, pane, |t| t.delete_layer(&id));
        ui.layer.set(None);
    });
    let merge_down = text_button(__scope, "Merge Down", "Merge Down (into the layer below)", || false, move || {
        let Some(id) = untracked(|| picked()) else { return };
        new_undo_step(pane);
        local_edit(store, pane, |t| t.merge_layer_down(&id));
        ui.layer.set(None);
    });
    let move_here = text_button(__scope, "Move Selection Here", "Move Selection to Layer", || false, move || {
        let Some(id) = untracked(|| picked()) else { return };
        let ids = with_session(pane, |s| s.selected.clone()).unwrap_or_default();
        if ids.is_empty() {
            crate::events::show_notice(store, "Select something to move to this layer.".to_string());
            return;
        }
        new_undo_step(pane);
        local_edit(store, pane, |t| t.move_to_layer(&ids, &id));
    });

    rsx! {
        div {
            class: "pimble-board__section",
            div { class: "pimble-board__panel-title", "Layers" }
            div {
                class: "pimble-board__row",
                style: {move || if ui.renaming.get().is_some() { "" } else { "display: none;" }},
                {rename_input}
            }
            div {
                class: "pimble-board__layers",
                for (row, class) in layer_rows(ui.layers.get(), ui.layer.get()) {
                    div {
                        key: format!("{} {} {} {} {} {} {}", row.id, row.name, row.visible, row.locked, row.count, row.opacity, ui.layer.get().as_deref() == Some(row.id.as_str())),
                        // The key holds whether it is picked: the row is drawn again when that changes.
                        class: class,
                        draggable: "true",
                        ondragstart: {
                            let id = row.id.clone();
                            move || drag.set(id.clone())
                        },
                        ondragend: move || drag.clear(),
                        ondrop: {
                            let target = row.id.clone();
                            move || {
                                let Some(dragged) = drag.take() else { return };
                                if dragged == target {
                                    return;
                                }
                                // Rows are top first: a row dropped on one
                                // takes its place, the target moving toward
                                // where the dragged one was.
                                let layers = untracked(|| ui.layers.get());
                                let from = layers.iter().position(|l| l.id == dragged);
                                let to = layers.iter().position(|l| l.id == target);
                                let (Some(from), Some(to)) = (from, to) else { return };
                                let placement = if from > to { Placement::Above(target.clone()) } else { Placement::Below(target.clone()) };
                                new_undo_step(pane);
                                local_edit(store, pane, |t| t.move_layer(&dragged, placement));
                            }
                        },
                        onclick: {
                            let id = row.id.clone();
                            move || ui.layer.set(Some(id.clone()))
                        },
                        div {
                            title: if row.visible { "Hide this layer, for everyone" } else { "Show this layer" },
                            class: "pimble-board__layer-icon",
                            onclick: {
                                let id = row.id.clone();
                                let visible = row.visible;
                                move || {
                                    new_undo_step(pane);
                                    local_edit(store, pane, |t| t.set_layer_visible(&id, !visible));
                                }
                            },
                            IconGlyph { name: if row.visible { TablerIcon::Eye.name().to_string() } else { TablerIcon::EyeOff.name().to_string() } }
                        }
                        div {
                            title: if row.locked { "Unlock this layer" } else { "Lock this layer against accidents" },
                            class: "pimble-board__layer-icon",
                            onclick: {
                                let id = row.id.clone();
                                let locked = row.locked;
                                move || {
                                    new_undo_step(pane);
                                    local_edit(store, pane, |t| t.set_layer_locked(&id, !locked));
                                }
                            },
                            IconGlyph { name: if row.locked { TablerIcon::Lock.name().to_string() } else { TablerIcon::LockOpen.name().to_string() } }
                        }
                        span {
                            class: "pimble-board__layer-name",
                            title: "Click to pick this layer; click its name again to rename it",
                            onclick: {
                                let id = row.id.clone();
                                let name = row.name.clone();
                                move || {
                                    // The row's own click does not reach past this one.
                                    if untracked(|| ui.layer.get()).as_deref() == Some(id.as_str()) {
                                        begin_rename(pane, ui, &id, &name);
                                    } else {
                                        ui.layer.set(Some(id.clone()));
                                    }
                                }
                            },
                            {row.name.clone()}
                        }

                        span { class: "pimble-board__layer-count", title: "Elements in this layer", {row.count.to_string()} }
                    }
                }
            }
            div { class: "pimble-board__label", {move || format!("Layer opacity {}%", (layer_opacity.get() * 100.0).round())} }
            Slider {
                min: 0.0,
                max: 1.0,
                step: 0.05,
                size: "sm",
                value_signal: Some(layer_opacity),
                onchange: move |v: f64| {
                    let Some(id) = untracked(|| picked()) else { return };
                    new_undo_step(pane);
                    local_edit(store, pane, |t| t.set_layer_opacity(&id, v as f32));
                },
            }
            div { class: "pimble-board__row pimble-board__row--wrap", {new_layer} {delete_layer} {merge_down} {move_here} }
        }
    }
}

/// The layer rows with their class (picked or not).
fn layer_rows(layers: Vec<LayerRow>, picked: Option<String>) -> Vec<(LayerRow, &'static str)> {
    layers
        .into_iter()
        .map(|row| {
            let class = if picked.as_deref() == Some(row.id.as_str()) { "pimble-board__layer pimble-board__layer--picked" } else { "pimble-board__layer" };
            (row, class)
        })
        .collect()
}

// ── The toolbar ─────────────────────────────────────────────────────────────

/// The board tools in the one toolbar (docs/SPLIT_VIEW_CONTRACT.md "One
/// toolbar"), for the focused pane's board: every tool (its letter in its
/// tooltip), undo and redo, delete, Arrange and Export menus, the grid and
/// snapping, Color by Author, fit and 1:1, and the side panel. The editing
/// tools are dimmed on a board this device may only read.
pub(crate) fn render_board_toolbar(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let focused = move || store.focused_pane.get();
    let st = store.board_style;
    let writable = move || {
        let pane = focused();
        store
            .pane(pane)
            .board
            .get()
            .is_some_and(|held| store.node_access_tracked(held.store_id, held.node_id).allows_write())
    };
    let tools: Vec<NodeHandle> = Tool::ALL
        .into_iter()
        .map(|tool| {
            let tip = format!("{} ({})", tool.name(), tool.key().to_ascii_uppercase());
            icon_button(__scope, tool.icon(), tip, move || store.board_tool.get() == tool, move || {
                let pane = untracked(|| store.focused_pane.get());
                tools::end_text(store, pane);
                store.board_tool.set(tool);
            })
        })
        .collect();
    let on_focused = move |f: fn(AppStore, PaneId)| move || f(store, untracked(|| store.focused_pane.get()));
    let action = |__scope: &mut RenderScope, icon: TablerIcon, tip: &'static str, run: fn(AppStore, PaneId)| {
        icon_button(__scope, icon, tip.to_string(), || false, on_focused(run))
    };
    let undo_btn = action(__scope, TablerIcon::ArrowBackUp, "Undo (Ctrl+Z)", |store, pane| tools::undo(store, pane, false));
    let redo_btn = action(__scope, TablerIcon::ArrowForwardUp, "Redo (Ctrl+Shift+Z)", |store, pane| tools::undo(store, pane, true));
    let delete_btn = action(__scope, TablerIcon::Trash, "Delete the selection (Delete)", delete_selected);
    let fit_btn = action(__scope, TablerIcon::FocusCentered, "Fit", fit);
    let one_btn = action(__scope, TablerIcon::ZoomReset, "Actual size", actual_size);
    let grid_btn = icon_button(__scope, TablerIcon::LayoutGrid, "Show the grid (only here)".to_string(), move || st.grid.get(), move || {
        let on = !untracked(|| st.grid.get());
        st.grid.set(on);
        set_grid(store, on);
    });
    let snap_btn = icon_button(__scope, TablerIcon::Magnet, "Snap to the grid (only here)".to_string(), move || st.snap.get(), move || {
        st.snap.update(|v| *v = !*v);
    });
    let author_btn = icon_button(
        __scope,
        TablerIcon::Users,
        "Color by author (this pane only; hides nothing)".to_string(),
        move || store.pane(store.focused_pane.get()).board_ui.by_author.get(),
        move || {
            let pane = untracked(|| store.focused_pane.get());
            let on = !untracked(|| store.pane(pane).board_ui.by_author.get());
            set_color_by_author(store, pane, on);
        },
    );
    let panel_btn = icon_button(__scope, TablerIcon::LayoutSidebarRight, "Navigator, style and layers".to_string(), move || st.panel.get(), move || {
        st.panel.update(|v| *v = !*v);
    });

    // The Arrange menu.
    let arrange_open = Signal::new(false);
    let item = |__scope: &mut RenderScope, icon: TablerIcon, label: &'static str, run: Box<dyn Fn(AppStore, PaneId)>| {
        rsx! {
            DropdownMenuItem {
                left_section: icon,
                onclick: move || run(store, untracked(|| store.focused_pane.get())),
                {label}
            }
        }
    };
    let arrange_items: Vec<NodeHandle> = vec![
        item(__scope, TablerIcon::Copy, "Duplicate (Ctrl+D)", Box::new(tools::duplicate)),
        item(__scope, TablerIcon::ChevronUp, "Bring Forward (Ctrl+])", Box::new(|s, p| tools::reorder(s, p, Order::Forward))),
        item(__scope, TablerIcon::StackPush, "Bring to Front (Ctrl+Shift+])", Box::new(|s, p| tools::reorder(s, p, Order::Front))),
        item(__scope, TablerIcon::ChevronDown, "Send Backward (Ctrl+[)", Box::new(|s, p| tools::reorder(s, p, Order::Backward))),
        item(__scope, TablerIcon::StackPop, "Send to Back (Ctrl+Shift+[)", Box::new(|s, p| tools::reorder(s, p, Order::Back))),
        item(__scope, TablerIcon::BoxMultiple, "Group (Ctrl+G)", Box::new(tools::group)),
        item(__scope, TablerIcon::Package, "Ungroup (Ctrl+Shift+G)", Box::new(tools::ungroup)),
        item(__scope, TablerIcon::Lock, "Lock / Unlock (Ctrl+L)", Box::new(tools::toggle_lock)),
        item(__scope, TablerIcon::LayoutAlignLeft, "Align Left", Box::new(|s, p| tools::align(s, p, Align2::Left))),
        item(__scope, TablerIcon::LayoutAlignCenter, "Align Centers", Box::new(|s, p| tools::align(s, p, Align2::Center))),
        item(__scope, TablerIcon::LayoutAlignRight, "Align Right", Box::new(|s, p| tools::align(s, p, Align2::Right))),
        item(__scope, TablerIcon::LayoutAlignTop, "Align Top", Box::new(|s, p| tools::align(s, p, Align2::Top))),
        item(__scope, TablerIcon::LayoutAlignMiddle, "Align Middles", Box::new(|s, p| tools::align(s, p, Align2::Middle))),
        item(__scope, TablerIcon::LayoutAlignBottom, "Align Bottom", Box::new(|s, p| tools::align(s, p, Align2::Bottom))),
        item(__scope, TablerIcon::LayoutDistributeVertical, "Distribute Across", Box::new(|s, p| tools::align(s, p, Align2::SpreadAcross))),
        item(__scope, TablerIcon::LayoutDistributeHorizontal, "Distribute Down", Box::new(|s, p| tools::align(s, p, Align2::SpreadDown))),
    ];
    let export_open = Signal::new(false);
    let export_items: Vec<NodeHandle> = vec![
        item(__scope, TablerIcon::ClipboardCopy, "Copy Picture (Ctrl+C)", Box::new(export::copy_picture)),
        item(__scope, TablerIcon::Photo, "Save as PNG...", Box::new(|s, p| export::save_picture(s, p, Format::Png, false))),
        item(__scope, TablerIcon::Photo, "Save as JPEG...", Box::new(|s, p| export::save_picture(s, p, Format::Jpeg, false))),
        item(__scope, TablerIcon::FileExport, "Save as SVG...", Box::new(|s, p| export::save_picture(s, p, Format::Svg, false))),
        item(__scope, TablerIcon::Eye, "Save with All Markup (PNG)...", Box::new(|s, p| export::save_picture(s, p, Format::Png, true))),
        item(__scope, TablerIcon::PhotoDown, "Save Original As...", Box::new(export::save_original)),
        item(__scope, TablerIcon::Stack2, "Flatten into New Image", Box::new(export::flatten)),
    ];

    // Labeled groups, as the text's toolbar has (Joe, 2026-10-20: group them
    // in a way that makes sense, with category labels, on every toolbar).
    rsx! {
        div {
            class: "pimble-toolbar",
            div {
                class: "pimble-toolbar__group",
                style: {move || if writable() { "" } else { "opacity: 0.4; pointer-events: none;" }},
                div { class: "pimble-toolbar__buttons", {tools} }
                div { class: "pimble-toolbar__label", "Tools" }
            }
            div {
                class: "pimble-toolbar__group",
                style: {move || if writable() { "" } else { "opacity: 0.4; pointer-events: none;" }},
                div {
                    class: "pimble-toolbar__buttons",
                    {undo_btn}
                    {redo_btn}
                    {delete_btn}
                    DropdownMenu {
                        opened_fn: move || arrange_open.get(),
                        on_close: move || arrange_open.set(false),
                        DropdownMenuTarget {
                            div {
                                title: "Arrange",
                                class: "pimble-board-tool",
                                onclick: move || arrange_open.update(|v| *v = !*v),
                                IconGlyph { name: TablerIcon::ArrowsMove.name().to_string() }
                            }
                        }
                        DropdownMenuDropdown { {arrange_items} }
                    }
                }
                div { class: "pimble-toolbar__label", "Edit" }
            }
            div {
                class: "pimble-toolbar__group",
                div {
                    class: "pimble-toolbar__buttons",
                    {grid_btn}
                    {snap_btn}
                    {author_btn}
                    {panel_btn}
                }
                div { class: "pimble-toolbar__label", "Show" }
            }
            div {
                class: "pimble-toolbar__group",
                div {
                    class: "pimble-toolbar__buttons",
                    {fit_btn}
                    {one_btn}
                }
                div { class: "pimble-toolbar__label", "Zoom" }
            }
            div {
                class: "pimble-toolbar__group",
                div {
                    class: "pimble-toolbar__buttons",
                    DropdownMenu {
                        opened_fn: move || export_open.get(),
                        on_close: move || export_open.set(false),
                        DropdownMenuTarget {
                            div {
                                title: "Copy and save the board as a picture",
                                class: "pimble-board-tool",
                                onclick: move || export_open.update(|v| *v = !*v),
                                IconGlyph { name: TablerIcon::Download.name().to_string() }
                            }
                        }
                        DropdownMenuDropdown { {export_items} }
                    }
                }
                div { class: "pimble-toolbar__label", "Export" }
            }
        }
    }
}

