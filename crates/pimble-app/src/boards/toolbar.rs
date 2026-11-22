//! The toolbar for canvases (docs/CANVAS_TOOLBAR_CONTRACT.md): it takes the
//! text toolbar's place while the focused pane shows a canvas (a board node,
//! or a picture in a note's text while its markup is being edited), as the
//! sheet's does for a sheet node. Still one toolbar, above the panes, for the
//! focused pane (Joe, 2026-10-08): every button acts on the pane focused when
//! it is pressed, and its state is read from that pane.
//!
//! Groups, labeled like the text groups: View (the minimap, zoom out, the
//! canvas zoom percentage with Fit and 100%, zoom in, fit), Snap (grid,
//! magnet), People (color by author), Export (one dropdown), History (undo,
//! redo). On a canvas this device may only read, View, People and Export stay
//! live and Snap and History dim.

use rinch::prelude::*;
use rinch_tabler_icons::TablerIcon;

use super::export::{self, Format};
use super::rail::{keys_to, zoom_by, Keys};
use super::*;
use crate::appearance::IconGlyph;

/// A group of the canvas toolbar, in order: its label, and whether it stays
/// live on a canvas this device may only read.
pub(crate) const GROUPS: [(&str, bool); 5] = [("View", true), ("Snap", false), ("People", true), ("Export", true), ("History", false)];

/// The canvas zoom as the toolbar shows it.
pub(crate) fn zoom_label(zoom: f32) -> String {
    format!("{}%", zoom.round())
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

/// A labeled group, dimmed and inert while `live` says no.
fn group(__scope: &mut RenderScope, label: &'static str, live: impl Fn() -> bool + Copy + 'static, buttons: Vec<NodeHandle>) -> NodeHandle {
    rsx! {
        div {
            class: "pimble-toolbar__group",
            style: {move || if live() { "" } else { "opacity: 0.4; pointer-events: none;" }},
            div { class: "pimble-toolbar__buttons", {buttons} }
            div { class: "pimble-toolbar__label", {label} }
        }
    }
}

/// A row of a toolbar dropdown: closes it, then runs on the focused pane.
fn menu_row(
    __scope: &mut RenderScope,
    store: AppStore,
    open: Signal<bool>,
    icon: TablerIcon,
    label: &'static str,
    run: fn(AppStore, PaneId),
) -> NodeHandle {
    rsx! {
        div {
            class: "pimble-board__menu-item",
            onclick: move || {
                open.set(false);
                run(store, untracked(|| store.focused_pane.get()));
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

/// The canvas toolbar, rendered once; it follows the focused pane.
pub(crate) fn render_canvas_toolbar(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let st = store.board_style;
    let focused = move || store.focused_pane.get();
    let now = move || untracked(focused);
    let writable = move || {
        let state = store.pane(focused());
        state
            .board
            .get()
            .is_some_and(|held| store.node_access_tracked(held.store_id, held.node_id).allows_write())
    };
    let always = || true;
    let never = || false;

    // View.
    let zoom_open = Signal::new(false);
    let zoom_face = rsx! {
        span { class: "pimble-ctb__zoom", {move || zoom_label(store.pane(focused()).board_ui.zoom.get())} }
    };
    let zoom_rows = vec![
        menu_row(__scope, store, zoom_open, TablerIcon::FocusCentered, "Fit", fit),
        menu_row(__scope, store, zoom_open, TablerIcon::ZoomReset, "100%", actual_size),
    ];
    let view = vec![
        button(__scope, TablerIcon::Map, "Show the Minimap", move || st.map.get(), move || st.map.update(|v| *v = !*v)),
        button(__scope, TablerIcon::ZoomOut, "Zoom Out", never, move || zoom_by(store, now(), 1.0 / 1.25)),
        dropdown(__scope, "Canvas zoom", "pimble-ctb__zoom-button", zoom_open, zoom_face, zoom_rows),
        button(__scope, TablerIcon::ZoomIn, "Zoom In", never, move || zoom_by(store, now(), 1.25)),
        button(__scope, TablerIcon::FocusCentered, "Fit", never, move || fit(store, now())),
    ];

    // Snap: per viewer, every canvas.
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
        move || store.pane(focused()).board_ui.by_author.get(),
        move || {
            let pane = now();
            let on = !untracked(|| store.pane(pane).board_ui.by_author.get());
            set_color_by_author(store, pane, on);
        },
    )];

    // Export: everything the canvas's export menu had.
    let export_open = Signal::new(false);
    let export_face = rsx! { span { class: "pimble-ctb__face", IconGlyph { name: TablerIcon::Download.name().to_string() } } };
    let export_rows = vec![
        menu_row(__scope, store, export_open, TablerIcon::ClipboardCopy, "Copy Picture (Ctrl+C)", export::copy_picture),
        menu_row(__scope, store, export_open, TablerIcon::Photo, "Save as PNG...", |s, p| export::save_picture(s, p, Format::Png, false)),
        menu_row(__scope, store, export_open, TablerIcon::Photo, "Save as JPEG...", |s, p| export::save_picture(s, p, Format::Jpeg, false)),
        menu_row(__scope, store, export_open, TablerIcon::FileExport, "Save as SVG...", |s, p| export::save_picture(s, p, Format::Svg, false)),
        menu_row(__scope, store, export_open, TablerIcon::Eye, "Save with All Markup (PNG)...", |s, p| export::save_picture(s, p, Format::Png, true)),
        menu_row(__scope, store, export_open, TablerIcon::PhotoDown, "Save Original As...", export::save_original),
        menu_row(__scope, store, export_open, TablerIcon::Stack2, "Flatten into New Canvas", export::flatten),
        menu_row(__scope, store, export_open, TablerIcon::FileVector, "Import SVG...", super::svg_import::pick),
    ];
    let export = vec![dropdown(
        __scope,
        "Copy and save the canvas as a picture, or import SVG",
        "pimble-ctb__export-button",
        export_open,
        export_face,
        export_rows,
    )];

    let history = vec![
        button(__scope, TablerIcon::ArrowBackUp, "Undo (Ctrl+Z)", never, move || tools::undo(store, now(), false)),
        button(__scope, TablerIcon::ArrowForwardUp, "Redo (Ctrl+Shift+Z)", never, move || tools::undo(store, now(), true)),
    ];

    let groups: Vec<NodeHandle> = GROUPS
        .iter()
        .zip([view, snap, people, export, history])
        .map(|(&(label, live_read_only), buttons)| {
            if live_read_only {
                group(__scope, label, always, buttons)
            } else {
                group(__scope, label, writable, buttons)
            }
        })
        .collect();
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
        assert_eq!(labels, ["View", "Snap", "People", "Export", "History"]);
        // Read-only: View and Export (and the way of looking, People) stay
        // live; Snap and History dim.
        let live: Vec<&str> = GROUPS.iter().filter(|(_, live)| *live).map(|(l, _)| *l).collect();
        assert_eq!(live, ["View", "People", "Export"]);
    }

    #[test]
    fn the_zoom_reads_as_a_whole_percentage() {
        assert_eq!(zoom_label(100.0), "100%");
        assert_eq!(zoom_label(37.4), "37%");
        assert_eq!(zoom_label(1600.0), "1600%");
    }
}
