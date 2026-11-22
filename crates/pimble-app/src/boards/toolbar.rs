//! The toolbar for canvases (docs/CANVAS_TOOLBAR_CONTRACT.md): it takes the
//! text toolbar's place while the focused pane shows a canvas (a board node,
//! or a picture in a note's text while its markup is being edited), and
//! while the focused note's selection is exactly a canvas in its text
//! ("Selected picture"), as the sheet's does for a sheet node. Still one
//! toolbar, above the panes, for the focused pane (Joe, 2026-10-08): every
//! button acts on the pane focused when it is pressed, and its state is read
//! from that pane.
//!
//! Groups, labeled like the text groups: Canvas (Edit Canvas; only for a
//! canvas in a note's text), View (the minimap, zoom out, the canvas zoom
//! percentage with Fit and 100%, zoom in, fit), Snap (grid, magnet), People
//! (color by author), Export (one dropdown), History (undo, redo). On a
//! canvas this device may only read, View, People and Export stay live and
//! Snap and History dim.
//!
//! On a selected picture (its canvas not open): Export acts on the picture
//! directly; Snap toggles the per-viewer settings; View, People and Edit
//! Canvas open the canvas over the note (as a double-click does) and then do
//! what they say; History is the note's own undo and redo.

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
pub(crate) const GROUPS: [(&str, bool); 6] =
    [("Canvas", false), ("View", true), ("Snap", false), ("People", true), ("Export", true), ("History", false)];

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
    crate::toolbar::picked().filter(|p| p.pane == pane).map(Aim::Picked)
}

/// Run `f` on the pane's canvas once it is open and fitted: at once for a
/// canvas that shows, after opening it over the note for a selected picture.
fn on_open_canvas(store: AppStore, f: impl FnOnce(AppStore, PaneId) + 'static) {
    match untracked(|| aim(store)) {
        Some(Aim::Board(pane)) => f(store, pane),
        Some(Aim::Picked(p)) => {
            crate::text_boards::mark_up_at(store, p.pane, p.pos, &p.src, p.board.clone());
            when_open(store, p.pane, Box::new(f), 0);
        }
        None => {}
    }
}

/// `f`, once pane `pane`'s canvas has a size and its first fit (a canvas
/// that did not open, a read-only note's, gives up after two seconds).
fn when_open(store: AppStore, pane: PaneId, f: Box<dyn FnOnce(AppStore, PaneId)>, tries: u32) {
    let ready = with_session(pane, |s| s.fitted && s.view.size().0 > 1) == Some(true);
    if ready {
        f(store, pane);
    } else if tries < 100 {
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

/// The canvas toolbar, rendered once; it follows the focused pane.
pub(crate) fn render_canvas_toolbar(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let st = store.board_style;
    let focused = move || store.focused_pane.get();
    let now = move || untracked(|| aim(store));
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
    let in_text = move || picked() || store.pane(focused()).markup.get();
    // A selected picture's View and People open its canvas, which a note
    // this device may only read cannot.
    let opens = move || !picked() || writable();
    let always = || true;
    let never = || false;

    // Canvas: open or close a canvas in text.
    let canvas = vec![button(
        __scope,
        TablerIcon::Edit,
        "Edit Canvas",
        move || store.pane(focused()).markup.get(),
        move || match now() {
            Some(Aim::Board(pane)) => end_markup(store, pane),
            Some(Aim::Picked(p)) => crate::text_boards::mark_up_at(store, p.pane, p.pos, &p.src, p.board.clone()),
            None => {}
        },
    )];

    // View.
    let zoom_open = Signal::new(false);
    let zoom_face = rsx! { span { class: "pimble-ctb__zoom", {move || shown_zoom(store)} } };
    let zoom_rows = vec![
        menu_row(__scope, zoom_open, TablerIcon::FocusCentered, "Fit", || None, move || on_open_canvas(store, fit)),
        menu_row(__scope, zoom_open, TablerIcon::ZoomReset, "100%", || None, move || on_open_canvas(store, actual_size)),
    ];
    let view = vec![
        button(__scope, TablerIcon::Map, "Show the Minimap", move || st.map.get(), move || {
            on_open_canvas(store, move |_, _| st.map.update(|v| *v = !*v))
        }),
        button(__scope, TablerIcon::ZoomOut, "Zoom Out", never, move || on_open_canvas(store, |s, p| zoom_by(s, p, 1.0 / 1.25))),
        dropdown(__scope, "Canvas zoom", "pimble-ctb__zoom-button", zoom_open, zoom_face, zoom_rows),
        button(__scope, TablerIcon::ZoomIn, "Zoom In", never, move || on_open_canvas(store, |s, p| zoom_by(s, p, 1.25))),
        button(__scope, TablerIcon::FocusCentered, "Fit", never, move || on_open_canvas(store, fit)),
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
    // picture, the picture itself, its canvas left closed.
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
        Some(Aim::Picked(p)) => export::no_original(&p),
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
        "Copy and save the canvas as a picture, or import SVG",
        "pimble-ctb__export-button",
        export_open,
        export_face,
        export_rows,
    )];

    // History: the canvas's, or the note's while its picture is merely
    // selected (deleting or moving the picture is an edit of the text).
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

    let groups: Vec<NodeHandle> = vec![
        group(__scope, GROUPS[0].0, in_text, writable, canvas),
        group(__scope, GROUPS[1].0, always, opens, view),
        group(__scope, GROUPS[2].0, always, writable, snap),
        group(__scope, GROUPS[3].0, always, opens, people),
        group(__scope, GROUPS[4].0, always, always, export),
        group(__scope, GROUPS[5].0, always, writable, history),
    ];
    rsx! {
        div {
            class: "pimble-toolbar pimble-ctb",
            // The canvas's letters keep working after a button here, as
            // they did after a press on the canvas's own chrome.
            onmousedown: move || {
                let pane = untracked(focused);
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
        assert_eq!(labels, ["Canvas", "View", "Snap", "People", "Export", "History"]);
        // Read-only: View and Export (and the way of looking, People) stay
        // live; Snap, History and Edit Canvas dim.
        let live: Vec<&str> = GROUPS.iter().filter(|(_, live)| *live).map(|(l, _)| *l).collect();
        assert_eq!(live, ["View", "People", "Export"]);
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
