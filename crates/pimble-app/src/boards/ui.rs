//! The board view's DOM (docs/IMAGE_MARKUP_CONTRACT.md sections 1, 4, 5, 6
//! and 8; docs/DRAWING_CONTRACT.md section 4): the surface, filling the
//! pane, and what floats over it: selection outlines and handles, the
//! marquee, the text input, the attribution under the pointer, the markup
//! control, and the canvas chrome (the tool rail, color well
//! and minimap corner of [`super::rail`], the panels: layers, meaning, frames).
//! Rendered once per pane slot; everything that changes is a signal.

use super::*;
use crate::appearance::IconGlyph;
use rinch_tabler_icons::{render_tabler_icon, TablerIconStyle};

thread_local! {
    /// Each pane's text input: (document key, node id), for focusing it.
    static TEXT_INPUTS: RefCell<[Option<(u64, u64)>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
    /// Each pane's layer rename input.
    static RENAME_INPUTS: RefCell<[Option<(u64, u64)>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
    /// Each pane's board view, to ask the document what holds the focus.
    static VIEWS: RefCell<[Option<NodeHandle>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
    /// Nodes a card asked about once already (`GetNode`).
    static CARD_ASKED: RefCell<HashSet<(StoreId, NodeId)>> = RefCell::new(HashSet::new());
}

/// Start renaming layer `id` (named `name`) in pane `pane`'s layers panel:
/// Enter keeps the new name, Escape drops it.
pub(super) fn begin_rename(pane: PaneId, ui: PaneUi, id: &str, name: &str) {
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
/// Pane `pane`'s shape in the split view, tracked: narrow (less than about
/// half the width: the context bar takes the whole top and the panels go
/// below it) and small (a quarter or less: the panels start folded).
pub(super) fn narrow_or_small(store: AppStore, pane: PaneId) -> (bool, bool) {
    let tiling = store.tiling.get();
    if tiling.len() < 2 {
        return (false, false);
    }
    tiling.rect_of(pane).map(|r| (r.w < 0.6, r.w * r.h < 0.3)).unwrap_or((false, false))
}

/// Whether the keyboard belongs to something other than the board's
/// floating chrome: a field anywhere (the chrome's own included), the
/// editor, the tree, the canvas's surface (which hears its keys itself). A
/// press on the chrome leaves the focus on what it pressed (a button, its
/// icon), which counts as the chrome's.
pub(super) fn something_focused(pane: PaneId) -> bool {
    let Some(view) = VIEWS.with(|v| v.borrow()[pane.index()].clone()) else { return true };
    let Some(active) = view.active_element() else { return false };
    let tag = active.tag_name().unwrap_or_default().to_ascii_lowercase();
    if matches!(tag.as_str(), "input" | "textarea" | "select") || active.get_attribute("contenteditable").is_some() {
        return true;
    }
    let mut at = Some(active);
    let mut steps = 0;
    while let Some(node) = at {
        if node.node_id() == view.node_id() {
            return false;
        }
        if node.get_attribute("class").is_some_and(|c| c.split_whitespace().any(|c| c == "pimble-board__canvas")) {
            return true;
        }
        steps += 1;
        if steps > 64 {
            break;
        }
        at = node.parent_node();
    }
    true
}

/// The input renaming a layer in pane `pane`'s panel, given the keyboard
/// when a rename starts.
pub(super) fn rename_input_is(pane: PaneId, input: &NodeHandle) {
    RENAME_INPUTS.with(|t| t.borrow_mut()[pane.index()] = Some((input.doc_key(), input.node_id().0)));
}

pub(super) fn finish_rename(pane: PaneId, ui: PaneUi, keep: bool) {
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
/// it) and the floating chrome over it. Rendered once per pane slot.
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

    let rail = super::rail::render_rail(__scope, store, pane);
    let well = super::rail::render_color_well(__scope, store, pane);
    let corner = super::rail::render_map_corner(__scope, store, pane);
    let panels = render_panels(__scope, store, pane);
    let writable = move || !read_only();
    // A quarter of a split view (or less) is small: the panels start
    // folded there, and the context bar scrolls.
    let small = move || narrow_or_small(store, pane).1;
    let was_small = Signal::new(false);
    let _ = rinch::Effect::new(move || {
        let now = small();
        untracked(|| {
            if now && !was_small.get() {
                ui.folded.set([true; 3]);
            }
            was_small.set(now);
        });
    });

    // Another tool finishes the Pen's path and lets go of Direct select's
    // points.
    let _ = rinch::Effect::new(move || {
        let _ = store.board_tool.get();
        // Later, never inside whatever set the tool.
        run_on_main_thread(move || super::points::tool_changed(store, pane));
    });

    // The selected element's gradient handles show while the well's picker
    // edits its gradient.
    let _ = rinch::Effect::new(move || {
        let st = store.board_style;
        let _ = (ui.popup.get(), st.front.get(), st.fill_gradient.get(), st.stroke_gradient.get());
        run_on_main_thread(move || {
            super::points::refresh_overlay(store, pane);
            super::present(pane);
        });
    });

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

    // Measuring (docs/MEASURE_CONTRACT.md): the readout near the pointer
    // and the Scale panel.
    let readout = super::measure::render_readout(__scope, store, pane);
    let scale_panel = super::measure::render_panel(__scope, store, pane);
    let canvas = rsx! {
    div {
        class: "pimble-board__canvas",
        // A tree row dropped here becomes a node card (the browser's
        // drop; the desktop's arrives as the surface's own `Drop`).
        ondrop: move || tools::drop_tree_row(store, pane),
        RenderSurface { surface: Some(surface) }
        // The discussions pinned here (docs/DISCUSSIONS_CONTRACT.md section 6):
        // a numbered marker each; a press opens its discussion beside the board.
        for pin in ui.pins.get() {
            div {
                key: format!("{} {:.1} {:.1} {} {} {}", pin.node_id, pin.x, pin.y, pin.number, pin.count, pin.unread),
                class: if pin.unread { "pimble-board__pin pimble-board__pin--unread" } else { "pimble-board__pin" },
                title: format!("{} ({} message{})", pin.title, pin.count, if pin.count == 1 { "" } else { "s" }),
                style: format!("left: {:.1}px; top: {:.1}px;", pin.x, pin.y),
                onmousedown: move || crate::pane_view::press_in_view(),
                onclick: {
                    let (s, n) = (pin.store_id, pin.node_id);
                    move || crate::discussions::open_beside(store, pane, s, n)
                },
                {pin.number.to_string()}
            }
        }
        for (x, y, w, h) in ui.outlines.get() {
            div {
                key: format!("{x:.1} {y:.1} {w:.1} {h:.1}"),
                class: "pimble-board__outline",
                style: format!("left: {x:.1}px; top: {y:.1}px; width: {w:.1}px; height: {h:.1}px;"),
            }
        }
        // Everyone else's selections and pointers (docs/PRESENCE_CONTRACT.md
        // section 5), in their colors.
        for o in ui.others.get().outlines {
            div {
                key: format!("{:.1} {:.1} {:.1} {:.1} {}", o.x, o.y, o.w, o.h, o.color),
                class: "pimble-board__outline pimble-board__outline--other",
                style: format!("left: {:.1}px; top: {:.1}px; width: {:.1}px; height: {:.1}px; border-color: {};", o.x, o.y, o.w, o.h, o.color.clone()),
            }
        }
        for p in ui.others.get().pointers {
            div {
                // A keyed row is drawn once: everything it shows is in its key.
                key: format!("{} {} {} {}", p.session, p.style, p.name, p.title),
                class: "pimble-presence-pointer",
                style: p.style.clone(),
                title: p.title.clone(),
                span {
                    class: "pimble-presence-pointer__arrow",
                    {render_tabler_icon(__scope, TablerIcon::Pointer, TablerIconStyle::Filled)}
                }
                span {
                    class: "pimble-presence-pointer__name",
                    style: p.tag_style.clone(),
                    onclick: {
                        let key = p.key.clone();
                        move || crate::presence::follow::start(store, pane, &key)
                    },
                    {p.name.clone()}
                }
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
        {readout}
        {scale_panel}
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
            class: "pimble-board__bottom-center",
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
    }
    };
    // The desktop's pixel scale reads the box the surface fills.
    BOUNDS.with(|b| b.borrow_mut()[pane.index()] = Some(canvas.bounds_signal()));
    // Files dropped from the system are pictures for the board (contract
    // section 1), where they were let go. The browser finds the board by this
    // attribute (`install_browser_handlers`, which knows where the drop was);
    // on the desktop rinch puts the drop point and this element's box in the
    // click context before the handler runs (joeleaver/rinch#1654), and the
    // board takes a point in the surface's physical pixels.
    canvas.set_attribute("data-pimble-board", &pane.index().to_string());
    #[cfg(feature = "native")]
    {
        let handler = __scope.register_file_drop_handler(move |paths: Vec<std::path::PathBuf>| {
            let ctx = rinch::core::get_click_context();
            let at = (ctx.element_width > 0.0 && ctx.element_height > 0.0).then(|| {
                let scale = super::scale_of(pane);
                ((ctx.mouse_x - ctx.element_x) * scale, (ctx.mouse_y - ctx.element_y) * scale)
            });
            add_image_files(store, Dest::Board(pane, at), paths);
        });
        canvas.set_attribute("data-onfiledrop", &handler.0.to_string());
    }
    let dim = move || if writable() { "" } else { "opacity: 0.45; pointer-events: none;" };
    let view = rsx! {
        div {
            class: {move || match narrow_or_small(store, pane) {
                (_, true) => "pimble-board pimble-board--narrow pimble-board--small",
                (true, false) => "pimble-board pimble-board--narrow",
                _ => "pimble-board",
            }},
            style: {move || if state.show_board.get() { "" } else { "display: none;" }},
            {canvas}
            div {
                class: "pimble-board__left",
                div {
                    style: {move || dim()},
                    onmouseenter: move || tools::pointer_off(store, pane),
                    onmousedown: move || {
                        crate::pane_view::press_in_view();
                        super::rail::keys_to(store, pane, super::rail::Keys::Chrome);
                    },
                    {rail}
                }
                div { class: "pimble-board__grow" }
                div { style: {move || dim()}, onmouseenter: move || tools::pointer_off(store, pane), {well} }
            }
            div {
                class: "pimble-board__right",
                {panels}
                div { class: "pimble-board__grow" }
                {corner}
            }
        }
    };
    VIEWS.with(|v| v.borrow_mut()[pane.index()] = Some(view.clone()));
    // The theme's background, on every board.
    let _ = rinch::Effect::new(move || {
        let dark = store.dark_mode.get();
        untracked(|| set_dark_mode(dark));
    });
    // The map shown: the navigator draws once it has room.
    let _ = rinch::Effect::new(move || {
        if style.map.get() {
            untracked(|| {
                let _ = set_timeout(50, move || nav::changed(store, pane));
            });
        }
    });
    view
}

// ── The panels ──────────────────────────────────────────────────────────────

/// The panels on the right (contract section 4): Layers, Meaning, Frames,
/// each folding to its title row; Tab hides the stack.
fn render_panels(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let state = store.pane(pane);
    let ui = state.board_ui;
    let writable = move || {
        state
            .board
            .get()
            .is_some_and(|held| store.node_access_tracked(held.store_id, held.node_id).allows_write())
    };
    let dim = move || if writable() { "" } else { "opacity: 0.45; pointer-events: none;" };
    let layers = super::layers::render_layers(__scope, store, pane);
    let layers = rsx! { div { style: {move || dim()}, {layers} } };
    let meaning = render_meaning(__scope, store, pane);
    let meaning = rsx! { div { style: {move || dim()}, {meaning} } };
    let frames = rsx! {
        div {
            class: "pimble-board__frames",
            div {
                class: "pimble-board__label",
                style: {move || if ui.frames.get().is_empty() { "" } else { "display: none;" }},
                "No frames yet. The frame tool draws one."
            }
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
    };
    let boxes: Vec<NodeHandle> = [("Layers", TablerIcon::Stack2, layers), ("Meaning", TablerIcon::Tags, meaning), ("Frames", TablerIcon::Frame, frames)]
        .into_iter()
        .enumerate()
        .map(|(i, (title, icon, body))| panel_box(__scope, ui, i, title, icon, body))
        .collect();
    // The layers' thumbnails are not drawn while the panel is folded or
    // hidden: opening it draws what changed meanwhile.
    let _ = rinch::Effect::new(move || {
        let open = !ui.panels_hidden.get() && !ui.folded.get()[0];
        if open {
            untracked(|| super::paint::thumbs_due(store, pane));
        }
    });
    rsx! {
        div {
            class: "pimble-board__panels",
            style: {move || if ui.panels_hidden.get() { "display: none;" } else { "" }},
            onmouseenter: move || super::tools::pointer_off(store, pane),
            onmousedown: move || crate::pane_view::press_in_view(),
            {boxes}
        }
    }
}

/// One floating panel: a title row that folds it, and its body. Folded,
/// it is its icon alone (the contract's "each folds to its icon"), so the
/// folded panels make a small column of icons.
fn panel_box(__scope: &mut RenderScope, ui: PaneUi, index: usize, title: &'static str, icon: TablerIcon, body: NodeHandle) -> NodeHandle {
    let folded = move || ui.folded.get()[index];
    rsx! {
        div {
            class: {move || if folded() { "pimble-float pimble-board__panel-box pimble-board__panel-box--icon" } else { "pimble-float pimble-board__panel-box" }},
            div {
                class: "pimble-board__panel-icon",
                title: format!("{title}: click to unfold (Tab hides every panel)"),
                style: {move || if folded() { "" } else { "display: none;" }},
                onclick: move || ui.folded.update(|f| f[index] = false),
                IconGlyph { name: icon.name().to_string() }
            }
            div {
                class: "pimble-board__panel-title",
                title: "Fold to an icon (Tab hides every panel)",
                style: {move || if folded() { "display: none;" } else { "" }},
                onclick: move || ui.folded.update(|f| f[index] = true),
                span { class: "pimble-board__layer-icon", IconGlyph { name: icon.name().to_string() } }
                span { class: "pimble-board__grow", {title} }
                span { class: "pimble-board__layer-icon", IconGlyph { name: TablerIcon::ChevronUp.name().to_string() } }
            }
            div {
                class: "pimble-board__panel-body",
                style: {move || if folded() { "display: none;" } else { "" }},
                {body}
            }
        }
    }
}

/// The opacity as the slider's `f64` signal, kept in step with the style's.
pub(super) fn opacity_signal(store: AppStore) -> Signal<f64> {
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

/// The feather slider's place, mirrored from the selection's feather
/// (docs/DRAWING_WAVE6_CONTRACT.md section 3).
pub(super) fn soft_signal(store: AppStore) -> Signal<f64> {
    thread_local! {
        static SIGNAL: Cell<Option<Signal<f64>>> = const { Cell::new(None) };
    }
    if let Some(sig) = SIGNAL.with(Cell::get) {
        return sig;
    }
    let sig = Signal::new(0.0f64);
    let st = store.board_style;
    let _ = rinch::Effect::new(move || {
        let v = super::rail::soft_to_slider(st.feather.get());
        untracked(|| sig.set_if_changed(v));
    });
    SIGNAL.with(|s| s.set(Some(sig)));
    sig
}

/// "Meaning" (docs/IMAGE_MARKUP_CONTRACT.md, "Meaning"): what the selected
/// element, or else the picked layer, means: a free note and named fields,
/// for the people and the assistants who read the canvas. Nothing here is
/// drawn or put on a legend.
fn render_meaning(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let ui = store.pane(pane).board_ui;
    // Picking another layer changes what the section is about.
    let _ = rinch::Effect::new(move || {
        let _ = ui.layer.get();
        let _ = ui.selected.get();
        untracked(|| super::publish_meaning(pane, ui));
    });
    let add_field = move || {
        let (name, value) = untracked(|| ui.new_field.get());
        if name.trim().is_empty() {
            crate::events::show_notice(store, "A field needs a name.".to_string());
            return;
        }
        super::write_field(store, pane, name.trim(), Some(value.trim()));
        ui.new_field.set((String::new(), String::new()));
    };
    let add = rsx! {
        div { class: "pimble-board-chip", title: "Add this field", onclick: move || add_field(), "Add" }
    };
    rsx! {
        div {
            class: "pimble-board__section",
            style: {move || if ui.meaning.get().is_some() { "" } else { "display: none;" }},
            onmousedown: move || super::rail::keys_to(store, pane, super::rail::Keys::Typing),
            div { class: "pimble-board__meaning-heading", {move || ui.meaning.get().map(|m| m.heading).unwrap_or_default()} }
            textarea {
                class: "pimble-board__note",
                placeholder: "What this means, for people and assistants reading the canvas",
                value: move || ui.note_value.get().1,
                oninput: move |value: String| super::write_note(store, pane, value),
            }
            div {
                class: "pimble-board__fields",
                for (name, value, label, tip) in ui
                    .meaning
                    .get()
                    .map(|m| m.fields)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(n, v)| (n.clone(), v, n.clone(), n))
                {
                    div {
                        key: format!("{name}={value}"),
                        class: "pimble-board__field",
                        span { class: "pimble-board__field-name", title: tip, {label} }
                        input {
                            r#type: "text",
                            class: "pimble-board__field-value",
                            title: "Enter keeps a new value",
                            value: {value.clone()},
                            oninput: {
                                let name = name.clone();
                                move |v: String| ui.field_edit.set((name.clone(), v))
                            },
                            onsubmit: {
                                let name = name.clone();
                                move || {
                                    let (editing, typed) = untracked(|| ui.field_edit.get());
                                    if editing == name {
                                        super::write_field(store, pane, &name, Some(typed.trim()));
                                    }
                                }
                            },
                        }
                        div {
                            class: "pimble-board__layer-icon",
                            title: "Remove this field",
                            onclick: {
                                let name = name.clone();
                                move || super::write_field(store, pane, &name, None)
                            },
                            IconGlyph { name: TablerIcon::X.name().to_string() }
                        }
                    }
                }
            }
            div {
                class: "pimble-board__field pimble-board__field--new",
                input {
                    r#type: "text",
                    class: "pimble-board__field-name-input",
                    placeholder: "field",
                    value: move || ui.new_field.get().0,
                    oninput: move |v: String| ui.new_field.update(|f| f.0 = v),
                }
                input {
                    r#type: "text",
                    class: "pimble-board__field-value",
                    placeholder: "value",
                    value: move || ui.new_field.get().1,
                    oninput: move |v: String| ui.new_field.update(|f| f.1 = v),
                    onsubmit: move || {
                        let (name, value) = untracked(|| ui.new_field.get());
                        if !name.trim().is_empty() {
                            super::write_field(store, pane, name.trim(), Some(value.trim()));
                            ui.new_field.set((String::new(), String::new()));
                        }
                    },
                }
                {add}
            }
        }
    }
}

