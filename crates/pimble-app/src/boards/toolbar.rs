//! The canvas groups of the main toolbar (docs/TOOLBAR_CONTRACT.md): shown
//! while `toolbar::which` says the focused pane's selected item is a canvas
//! (a canvas pane, a canvas being edited, or a picture or embedded canvas
//! node selected in a note's text). Still one toolbar, above the panes, for
//! the focused pane (Joe, 2026-10-08): every button acts on the pane focused
//! when it is pressed, and its state is read from that pane.
//!
//! Groups, labeled like the text groups: Canvas (Edit Canvas, which is Done
//! while the canvas is being edited), the selection's own group (named for
//! what is selected, or for the tool in hand: what the floating bar over
//! the canvas carried until 2026-11-21), View (the minimap, zoom out, the
//! canvas zoom percentage with Fit and 100%, zoom in, fit), Snap (grid,
//! magnet), People (color by author), Export (one dropdown), History (undo,
//! redo). On a canvas this device may only read, View, People and Export
//! stay live and the rest dim.
//!
//! On a selected picture or embedded canvas (not open): Export acts on it
//! directly; Snap toggles the per-viewer settings; View, People and Edit
//! Canvas open it (a picture's canvas over the note, an embedded canvas node
//! in the pane) and then do what they say; History is the note's own.

use rinch::prelude::*;
use rinch_tabler_icons::TablerIcon;

use super::export::{self, Format};
use super::rail::{keys_to, zoom_by, Keys};
use super::*;
use crate::appearance::IconGlyph;
use crate::toolbar::Picked;

/// A group of the canvas toolbar, in order: its label, and whether it stays
/// live on a canvas this device may only read (the rule `render_canvas_toolbar`
/// builds the groups by; on a selected picture of such a note, View and
/// People dim too, since they open the canvas).
pub(crate) const GROUPS: [(&str, bool); 7] = [
    ("Canvas", false),
    ("Selection", false),
    ("View", true),
    ("Snap", false),
    ("People", true),
    ("Export", true),
    ("History", false),
];

/// The canvas zoom as the toolbar shows it.
pub(crate) fn zoom_label(zoom: f32) -> String {
    format!("{}%", zoom.round())
}

/// The zoom, in percent, a canvas whose visible extent is `extent` opens at
/// in a view of `width` x `height` CSS pixels: fitted with a 24 pixel
/// margin, never above 100% (`boards::present`).
pub(crate) fn opening_zoom(extent: Rect, width: f32, height: f32) -> f32 {
    let fitted = pimble_board::render::View::fit(&extent, width.max(1.0) as u32, height.max(1.0) as u32, 24.0).scale;
    fitted.min(1.0) * 100.0
}

/// What the toolbar acts on: the canvas a pane shows, or a canvas selected
/// in a pane's text.
#[derive(Debug, Clone, PartialEq)]
enum Aim {
    Board(PaneId),
    Picked(Picked),
}

/// The focused pane's aim, read reactively.
fn aim(store: AppStore) -> Option<Aim> {
    let pane = store.focused_pane.get();
    if store.pane(pane).show_board.get() {
        return Some(Aim::Board(pane));
    }
    crate::toolbar::picked(store, pane).map(Aim::Picked)
}

thread_local! {
    /// Per pane: the canvas node it opened from an embed in its note's text
    /// (`None` inside while it has not shown yet), which Done goes back from.
    static FROM_EMBED: RefCell<[Option<Signal<Option<Option<(StoreId, NodeId)>>>>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
}

fn from_embed(pane: PaneId) -> Option<Signal<Option<Option<(StoreId, NodeId)>>>> {
    FROM_EMBED.with(|f| f.borrow()[pane.index()]).filter(|s| s.is_alive())
}

/// Open the selected canvas: a picture's over the note, an embedded canvas
/// node in the pane (remembered, so Done goes back to the note).
fn open_picked(store: AppStore, p: &Picked) {
    if p.embed.is_some() {
        if let Some(sig) = from_embed(p.pane) {
            sig.set(Some(None));
        }
    }
    crate::text_boards::mark_up_at(store, p.pane, p.pos, &p.src, p.board.clone());
}

/// What the Canvas group's one button is now.
#[derive(Debug, Clone, PartialEq)]
enum EditState {
    /// A canvas selected in the text, not open: "Edit Canvas".
    Edit,
    /// A picture's canvas open over the note: "Done" goes back to the text.
    DoneMarkup,
    /// A symbol being edited: "Done" goes back to the canvas.
    DoneSymbol(String),
    /// A canvas node opened from an embed: "Done" goes back to the note.
    DoneEmbed,
    /// A canvas node opened any other way: no Canvas group.
    None,
}

fn edit_state(store: AppStore) -> EditState {
    match aim(store) {
        Some(Aim::Picked(_)) => EditState::Edit,
        Some(Aim::Board(pane)) => {
            let state = store.pane(pane);
            if let Some(name) = state.board_ui.editing_symbol.get() {
                EditState::DoneSymbol(name)
            } else if state.markup.get() {
                EditState::DoneMarkup
            } else if from_embed(pane).is_some_and(|s| matches!(s.get(), Some(Some(b)) if state.board.get().is_some_and(|h| (h.store_id, h.node_id) == b))) {
                EditState::DoneEmbed
            } else {
                EditState::None
            }
        }
        None => EditState::None,
    }
}

/// Run `f` on the pane's canvas once it is open and fitted: at once for a
/// canvas that shows, after opening it over the note for a selected picture.
fn on_open_canvas(store: AppStore, f: impl FnOnce(AppStore, PaneId) + 'static) {
    match untracked(|| aim(store)) {
        Some(Aim::Board(pane)) => f(store, pane),
        Some(Aim::Picked(p)) => {
            open_picked(store, &p);
            when_open(store, p.pane, Box::new(f), 0);
        }
        None => {}
    }
}

/// `f`, once pane `pane`'s canvas has a size and its first fit (a canvas
/// that did not open, a read-only note's, gives up after two seconds).
fn when_open(store: AppStore, pane: PaneId, f: Box<dyn FnOnce(AppStore, PaneId)>, tries: u32) {
    let ready = untracked(|| store.pane(pane).show_board.get()) && with_session(pane, |s| s.fitted && s.view.size().0 > 1) == Some(true);
    if ready {
        f(store, pane);
    } else if tries < 150 {
        let _ = set_timeout(20, move || when_open(store, pane, f, tries + 1));
    }
}

/// One button: active while `active` says so.
fn button(
    __scope: &mut RenderScope,
    icon: TablerIcon,
    tip: &'static str,
    active: impl Fn() -> bool + Copy + 'static,
    run: impl Fn() + 'static,
) -> NodeHandle {
    rsx! {
        div {
            title: tip,
            class: {move || if active() { "pimble-board-tool pimble-board-tool--active" } else { "pimble-board-tool" }},
            onclick: move || run(),
            IconGlyph { name: icon.name().to_string() }
        }
    }
}

/// A labeled group, shown while `shown` says so, dimmed and inert while
/// `live` says no.
fn group(
    __scope: &mut RenderScope,
    label: &'static str,
    shown: impl Fn() -> bool + Copy + 'static,
    live: impl Fn() -> bool + Copy + 'static,
    buttons: Vec<NodeHandle>,
) -> NodeHandle {
    rsx! {
        div {
            class: "pimble-toolbar__group",
            style: {move || if !shown() { "display: none;" } else if live() { "" } else { "opacity: 0.4; pointer-events: none;" }},
            div { class: "pimble-toolbar__buttons", {buttons} }
            div { class: "pimble-toolbar__label", {label} }
        }
    }
}

/// A row of a toolbar dropdown: closes it, then runs; dimmed with its
/// reason as the tooltip while `off` gives one.
fn menu_row(
    __scope: &mut RenderScope,
    open: Signal<bool>,
    icon: TablerIcon,
    label: &'static str,
    off: impl Fn() -> Option<&'static str> + Copy + 'static,
    run: impl Fn() + 'static,
) -> NodeHandle {
    rsx! {
        div {
            class: "pimble-board__menu-item",
            title: {move || off().unwrap_or_default().to_string()},
            style: {move || if off().is_some() { "opacity: 0.4;" } else { "" }},
            onclick: move || {
                if off().is_none() {
                    open.set(false);
                    run();
                }
            },
            span { class: "pimble-board__menu-icon", IconGlyph { name: icon.name().to_string() } }
            span { {label} }
        }
    }
}

/// A button that opens a dropdown of `rows` under it; `face` is what the
/// button shows, `class` its classes beside the board tool's.
fn dropdown(
    __scope: &mut RenderScope,
    tip: &'static str,
    class: &'static str,
    open: Signal<bool>,
    face: NodeHandle,
    rows: Vec<NodeHandle>,
) -> NodeHandle {
    let shut = format!("pimble-board-tool {class}");
    let shown = format!("{shut} pimble-board-tool--active");
    rsx! {
        div {
            Popover {
                opened_fn: move || open.get(),
                onclose: move || open.set(false),
                position: "bottom-start",
                close_on_click_outside: true,
                close_on_escape: true,
                z_index: 300,
                PopoverTarget {
                    div {
                        title: tip,
                        class: {move || if open.get() { shown.clone() } else { shut.clone() }},
                        onclick: move || open.set(!untracked(|| open.get())),
                        {face}
                    }
                }
                PopoverDropdown {
                    div {
                        class: "pimble-ctb__menu",
                        {rows}
                    }
                }
            }
        }
    }
}

/// The percentage the toolbar shows: the canvas's zoom, or for a selected
/// picture the zoom its canvas would open at ("Fit" while that cannot be
/// told: the picture has not arrived, or the pane has no size yet).
fn shown_zoom(store: AppStore) -> String {
    match aim(store) {
        Some(Aim::Board(pane)) => zoom_label(store.pane(pane).board_ui.zoom.get()),
        Some(Aim::Picked(p)) => {
            let size = crate::text_boards::text_bounds(p.pane).map(|b| b.get()).filter(|b| b.width > 1.0 && b.height > 1.0);
            let extent = untracked(|| export::picked_raw(store, &p))
                .and_then(|raw| pimble_board::BoardSnapshot::build_with(Some(raw), ViewOptions::default()).visible_extent());
            match (size, extent) {
                (Some(b), Some(extent)) => zoom_label(opening_zoom(extent, b.width, b.height)),
                _ => "Fit".to_string(),
            }
        }
        None => String::new(),
    }
}

/// The Canvas group's one button: "Edit Canvas" (a pencil) while the canvas
/// is only selected, "Done" (a check, the primary color) while it is being
/// edited (docs/TOOLBAR_CONTRACT.md "Edit and Done").
fn edit_button(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let state = move || edit_state(store);
    let editing = move || !matches!(state(), EditState::Edit | EditState::None);
    rsx! {
        div {
            class: {move || if editing() { "pimble-ctb__edit pimble-ctb__edit--done" } else { "pimble-ctb__edit" }},
            title: {move || match state() {
                EditState::Edit => "Edit Canvas (double-click)".to_string(),
                EditState::DoneMarkup => "Done: back to the text (Escape)".to_string(),
                EditState::DoneSymbol(name) => format!("Done editing the symbol \"{name}\" (Escape)"),
                EditState::DoneEmbed => "Done: back to the note".to_string(),
                EditState::None => String::new(),
            }},
            onclick: move || {
                match untracked(state) {
                    EditState::Edit => {
                        if let Some(Aim::Picked(p)) = untracked(|| aim(store)) {
                            open_picked(store, &p);
                        }
                    }
                    EditState::DoneMarkup => {
                        if let Some(Aim::Board(pane)) = untracked(|| aim(store)) {
                            end_markup(store, pane);
                        }
                    }
                    EditState::DoneSymbol(_) => {
                        if let Some(Aim::Board(pane)) = untracked(|| aim(store)) {
                            super::stamp::done(store, pane);
                        }
                    }
                    EditState::DoneEmbed => {
                        if let Some(Aim::Board(pane)) = untracked(|| aim(store)) {
                            if let Some(sig) = from_embed(pane) {
                                sig.set(None);
                            }
                            store.nav_step.set(Some(true));
                        }
                    }
                    EditState::None => {}
                }
            },
            span {
                class: "pimble-ctb__face",
                style: {move || if editing() { "display: none;" } else { "" }},
                IconGlyph { name: TablerIcon::Pencil.name().to_string() }
            }
            span {
                class: "pimble-ctb__face",
                style: {move || if editing() { "" } else { "display: none;" }},
                IconGlyph { name: TablerIcon::Check.name().to_string() }
            }
            span { class: "pimble-ctb__edit-label", {move || if editing() { "Done" } else { "Edit Canvas" }} }
        }
    }
}

/// The group for what is selected in pane `pane`'s canvas (or the tool in
/// hand): the controls the floating bar carried, labeled with what they act
/// on; their pickers open under the group, over the canvas.
fn selection_group(__scope: &mut RenderScope, store: AppStore, pane: PaneId, live: impl Fn() -> bool + Copy + 'static) -> NodeHandle {
    let (row, popups) = super::rail::render_context_bar(__scope, store, pane);
    let shown = move || store.focused_pane.get() == pane && store.pane(pane).show_board.get() && super::rail::context_shown(store, pane);
    rsx! {
        div {
            class: "pimble-toolbar__group pimble-ctb__selection",
            style: {move || if !shown() { "display: none;" } else if live() { "" } else { "opacity: 0.4; pointer-events: none;" }},
            {row}
            div { class: "pimble-toolbar__label", {move || super::rail::context_label(store, pane)} }
            {popups}
        }
    }
}

/// The canvas toolbar, rendered once; it follows the focused pane.
pub(crate) fn render_canvas_toolbar(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let st = store.board_style;
    let focused = move || store.focused_pane.get();
    let now = move || untracked(|| aim(store));
    // Which canvas node each pane opened from an embed, followed as its
    // board changes: Done shows while it still shows that node.
    for pane in PaneId::ALL {
        let sig: Signal<Option<Option<(StoreId, NodeId)>>> = Signal::new(None);
        FROM_EMBED.with(|f| f.borrow_mut()[pane.index()] = Some(sig));
        let _ = rinch::Effect::new(move || {
            let board = store.pane(pane).board.get().map(|h| (h.store_id, h.node_id));
            untracked(|| match (sig.get(), board) {
                (Some(None), Some(b)) => sig.set(Some(Some(b))),
                (Some(Some(was)), now) if now != Some(was) => sig.set(None),
                _ => {}
            });
        });
    }
    let writable = move || match aim(store) {
        Some(Aim::Board(pane)) => store
            .pane(pane)
            .board
            .get()
            .is_some_and(|held| store.node_access_tracked(held.store_id, held.node_id).allows_write()),
        Some(Aim::Picked(p)) => store
            .pane(p.pane)
            .active_edit
            .get()
            .is_some_and(|held| store.node_access_tracked(held.store_id, held.node_id).allows_write()),
        None => false,
    };
    let picked = move || matches!(aim(store), Some(Aim::Picked(_)));
    // A selected picture's View, People and Edit Canvas open it: a picture's
    // canvas cannot be opened for markup on a note this device may only
    // read. An embedded canvas node opens to be looked at, whatever its note.
    let opens = move || match aim(store) {
        Some(Aim::Picked(p)) if p.embed.is_none() => writable(),
        _ => true,
    };
    let canvas_shown = move || edit_state(store) != EditState::None;
    let always = || true;
    let never = || false;

    let canvas = vec![edit_button(__scope, store)];
    let selections: Vec<NodeHandle> = PaneId::ALL.into_iter().map(|pane| selection_group(__scope, store, pane, writable)).collect();

    // View.
    let zoom_open = Signal::new(false);
    let zoom_face = rsx! { span { class: "pimble-ctb__zoom", {move || shown_zoom(store)} } };
    // While the selection's group shows, View is the percentage alone (its
    // menu holds every View command), so the toolbar stays one row.
    let compact = move || {
        let pane = focused();
        store.pane(pane).show_board.get() && super::rail::context_shown(store, pane)
    };
    let wide = move |__scope: &mut RenderScope, b: NodeHandle| rsx! { div { style: {move || if compact() { "display: none;" } else { "" }}, {b} } };
    let zoom_rows = vec![
        menu_row(__scope, zoom_open, TablerIcon::FocusCentered, "Fit", || None, move || on_open_canvas(store, fit)),
        menu_row(__scope, zoom_open, TablerIcon::ZoomReset, "100%", || None, move || on_open_canvas(store, actual_size)),
        menu_row(__scope, zoom_open, TablerIcon::ZoomIn, "Zoom In", || None, move || on_open_canvas(store, |s, p| zoom_by(s, p, 1.25))),
        menu_row(__scope, zoom_open, TablerIcon::ZoomOut, "Zoom Out", || None, move || on_open_canvas(store, |s, p| zoom_by(s, p, 1.0 / 1.25))),
        menu_row(__scope, zoom_open, TablerIcon::Map, "Show or Hide the Minimap", || None, move || on_open_canvas(store, move |_, _| st.map.update(|v| *v = !*v))),
    ];
    let map_btn = button(__scope, TablerIcon::Map, "Show the Minimap", move || st.map.get(), move || {
        on_open_canvas(store, move |_, _| st.map.update(|v| *v = !*v))
    });
    let out_btn = button(__scope, TablerIcon::ZoomOut, "Zoom Out", never, move || on_open_canvas(store, |s, p| zoom_by(s, p, 1.0 / 1.25)));
    let in_btn = button(__scope, TablerIcon::ZoomIn, "Zoom In", never, move || on_open_canvas(store, |s, p| zoom_by(s, p, 1.25)));
    let fit_btn = button(__scope, TablerIcon::FocusCentered, "Fit", never, move || on_open_canvas(store, fit));
    let view = vec![
        wide(__scope, map_btn),
        wide(__scope, out_btn),
        dropdown(__scope, "Canvas Zoom", "pimble-ctb__zoom-button", zoom_open, zoom_face, zoom_rows),
        wide(__scope, in_btn),
        wide(__scope, fit_btn),
    ];

    // Snap: per viewer, every canvas; no canvas needs to be open.
    let snap = vec![
        button(__scope, TablerIcon::LayoutGrid, "Show the Grid (only here)", move || st.grid.get(), move || {
            let on = !untracked(|| st.grid.get());
            st.grid.set(on);
            set_grid(store, on);
        }),
        button(__scope, TablerIcon::Magnet, "Snap to the Grid (only here)", move || st.snap.get(), move || st.snap.update(|v| *v = !*v)),
    ];

    // People: this pane only.
    let people = vec![button(
        __scope,
        TablerIcon::Users,
        "Color by Author (this pane only; hides nothing)",
        move || matches!(aim(store), Some(Aim::Board(pane)) if store.pane(pane).board_ui.by_author.get()),
        move || {
            on_open_canvas(store, |store, pane| {
                let on = !untracked(|| store.pane(pane).board_ui.by_author.get());
                set_color_by_author(store, pane, on);
            })
        },
    )];

    // Export: everything the canvas's export menu had; on a selected
    // picture or embedded canvas, that canvas itself, left closed.
    let export_open = Signal::new(false);
    let export_face = rsx! { span { class: "pimble-ctb__face", IconGlyph { name: TablerIcon::Download.name().to_string() } } };
    // An export: `board` on a canvas that shows, `picked` on a selected one.
    let either = move |board: fn(AppStore, PaneId), picked: fn(AppStore, &Picked)| {
        move || match now() {
            Some(Aim::Board(pane)) => board(store, pane),
            Some(Aim::Picked(p)) => picked(store, &p),
            None => {}
        }
    };
    let on = || None;
    let original_off = move || match aim(store) {
        Some(Aim::Picked(p)) => export::no_original(store, &p),
        _ => None,
    };
    let import_off = move || picked().then_some("Edit the canvas to import SVG into it.");
    let export_rows = vec![
        menu_row(__scope, export_open, TablerIcon::ClipboardCopy, "Copy Picture (Ctrl+C)", on, either(export::copy_picture, export::copy_picked)),
        menu_row(__scope, export_open, TablerIcon::Photo, "Save as PNG...", on, either(
            |s, p| export::save_picture(s, p, Format::Png, false),
            |s, p| export::save_picked(s, p, Format::Png, false),
        )),
        menu_row(__scope, export_open, TablerIcon::Photo, "Save as JPEG...", on, either(
            |s, p| export::save_picture(s, p, Format::Jpeg, false),
            |s, p| export::save_picked(s, p, Format::Jpeg, false),
        )),
        menu_row(__scope, export_open, TablerIcon::FileExport, "Save as SVG...", on, either(
            |s, p| export::save_picture(s, p, Format::Svg, false),
            |s, p| export::save_picked(s, p, Format::Svg, false),
        )),
        menu_row(__scope, export_open, TablerIcon::Eye, "Save with All Markup (PNG)...", on, either(
            |s, p| export::save_picture(s, p, Format::Png, true),
            |s, p| export::save_picked(s, p, Format::Png, true),
        )),
        menu_row(__scope, export_open, TablerIcon::PhotoDown, "Save Original As...", original_off, either(export::save_original, export::save_picked_original)),
        menu_row(__scope, export_open, TablerIcon::Stack2, "Flatten into New Canvas", on, either(export::flatten, export::flatten_picked)),
        menu_row(__scope, export_open, TablerIcon::FileVector, "Import SVG...", import_off, either(super::svg_import::pick, |_, _| {})),
    ];
    let export = vec![dropdown(
        __scope,
        "Copy and Save the Canvas as a Picture, or Import SVG",
        "pimble-ctb__export-button",
        export_open,
        export_face,
        export_rows,
    )];

    // History: the canvas's, or the note's while its canvas is merely
    // selected (deleting or moving it is an edit of the text).
    let undo = move |redo: bool| {
        move || match now() {
            Some(Aim::Board(pane)) => tools::undo(store, pane, redo),
            Some(Aim::Picked(p)) => {
                let h = crate::editor::editor(p.pane);
                h.command(if redo { "redo" } else { "undo" });
                h.focus();
                crate::toolbar::bump_toolbar(p.pane);
            }
            None => {}
        }
    };
    let history = vec![
        button(__scope, TablerIcon::ArrowBackUp, "Undo (Ctrl+Z)", never, undo(false)),
        button(__scope, TablerIcon::ArrowForwardUp, "Redo (Ctrl+Shift+Z)", never, undo(true)),
    ];

    let mut groups: Vec<NodeHandle> = vec![group(__scope, GROUPS[0].0, canvas_shown, opens, canvas)];
    groups.extend(selections);
    groups.extend([
        group(__scope, GROUPS[2].0, always, opens, view),
        group(__scope, GROUPS[3].0, always, writable, snap),
        group(__scope, GROUPS[4].0, always, opens, people),
        group(__scope, GROUPS[5].0, always, always, export),
        group(__scope, GROUPS[6].0, always, writable, history),
    ]);
    rsx! {
        div {
            class: "pimble-toolbar pimble-ctb",
            // The canvas's letters keep working after a button here, as
            // they did after a press on the canvas's own chrome.
            onmousedown: move || {
                let pane = untracked(|| store.focused_pane.get());
                if untracked(|| store.pane(pane).show_board.get()) {
                    keys_to(store, pane, Keys::Chrome);
                }
            },
            {groups}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_groups_are_the_contracts() {
        let labels: Vec<&str> = GROUPS.iter().map(|(l, _)| *l).collect();
        assert_eq!(labels, ["Canvas", "Selection", "View", "Snap", "People", "Export", "History"]);
        // Read-only: View and Export (and the way of looking, People) stay
        // live; Edit Canvas, the selection's controls, Snap and History dim.
        let live: Vec<&str> = GROUPS.iter().filter(|(_, live)| *live).map(|(l, _)| *l).collect();
        assert_eq!(live, ["View", "People", "Export"]);
    }

    /// No two exports of one canvas default to the same file name.
    #[test]
    fn every_export_has_its_own_file_name() {
        let names = [
            export::rendered_name("Trip", Format::Png, false),
            export::rendered_name("Trip", Format::Jpeg, false),
            export::rendered_name("Trip", Format::Svg, false),
            export::rendered_name("Trip", Format::Png, true),
            export::original_name("Trip", "png"),
            export::original_name("Trip", "jpg"),
        ];
        let distinct: std::collections::HashSet<&String> = names.iter().collect();
        assert_eq!(distinct.len(), names.len(), "{names:?}");
        assert_eq!(names[0], "Trip.png");
        assert_eq!(names[4], "Trip (original).png");
    }

    #[test]
    fn the_zoom_reads_as_a_whole_percentage() {
        assert_eq!(zoom_label(100.0), "100%");
        assert_eq!(zoom_label(37.4), "37%");
        assert_eq!(zoom_label(1600.0), "1600%");
    }

    /// A selected picture's percentage is what its canvas opens at: fitted
    /// with a margin, and a small picture at its own size.
    #[test]
    fn a_canvas_opens_fitted_and_never_above_its_own_size() {
        // 1000 x 500 into 548 x 400: (548 - 48) / 1000 = 50%.
        assert_eq!(zoom_label(opening_zoom(Rect::new(0.0, 0.0, 1000.0, 500.0), 548.0, 400.0)), "50%");
        // Height-bound: (348 - 48) / 600 = 50%.
        assert_eq!(zoom_label(opening_zoom(Rect::new(0.0, 0.0, 300.0, 600.0), 1000.0, 348.0)), "50%");
        // Small: 100%.
        assert_eq!(zoom_label(opening_zoom(Rect::new(10.0, 10.0, 200.0, 100.0), 1000.0, 800.0)), "100%");
    }
}
