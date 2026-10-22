//! The sheet's own toolbar (Joe, 2026-10-22: a sheet node should not show
//! the text's toolbar): it takes the toolbar's place while the focused pane
//! shows a sheet node, as the board's does for a board. Its row, column and
//! table buttons act on the cell last pressed (the one being typed in, or
//! the computed cell the formula bar is about, or the last one), and on the
//! sheet's only plain table when no cell has been pressed yet.

use rinch::prelude::*;
use rinch_tabler_icons::TablerIcon;

use super::{
    add_summary, add_summary_row, add_table, cell_pressed, delete_column_of, delete_row, delete_table, group_by, open_column, rotate_table,
    toggle_all_groups, undo, CellKey, TableView,
};
use super::moving;
use crate::appearance::IconGlyph;
use crate::panes::PaneId;
use crate::state::AppStore;

/// What the focused pane's buttons act on: its table, and the cell in it
/// when a cell was pressed.
#[derive(Clone)]
struct Aim {
    table: TableView,
    cell: Option<CellKey>,
}

impl Aim {
    /// A plain table's row, when the cell is one of its rows.
    fn row(&self) -> Option<&str> {
        self.cell.as_ref().filter(|_| !self.table.summary).and_then(|c| c.row.as_deref())
    }

    /// A plain table's column, when the cell is in one.
    fn col(&self) -> Option<&str> {
        self.cell
            .as_ref()
            .filter(|_| !self.table.summary)
            .map(|c| c.col.as_str())
            .filter(|c| self.table.columns.iter().any(|col| col.key == *c))
    }
}

/// Pane `pane`'s aim, read reactively.
fn aim_of(store: AppStore, pane: PaneId) -> Option<Aim> {
    let ui = store.pane(pane).sheet_ui;
    let tables = ui.tables.get();
    let cell = ui.editing.get().or_else(|| ui.selected.get()).or_else(|| ui.last.get());
    if let Some(cell) = cell {
        if let Some(table) = tables.iter().find(|t| t.id == cell.table) {
            return Some(Aim { table: table.clone(), cell: Some(cell) });
        }
    }
    let mut plain = tables.iter().filter(|t| !t.summary);
    match (plain.next(), plain.next()) {
        (Some(only), None) => Some(Aim { table: only.clone(), cell: None }),
        _ => None,
    }
}

/// One button: dimmed and inert while `enabled` says no.
fn button(
    __scope: &mut RenderScope,
    icon: TablerIcon,
    tip: &'static str,
    enabled: impl Fn() -> bool + Copy + 'static,
    active: impl Fn() -> bool + Copy + 'static,
    run: impl Fn() + 'static,
) -> NodeHandle {
    rsx! {
        div {
            title: tip,
            class: {move || if active() { "pimble-board-tool pimble-board-tool--active" } else { "pimble-board-tool" }},
            style: {move || if enabled() { "" } else { "opacity: 0.35; pointer-events: none;" }},
            onclick: move || if enabled() { run() },
            IconGlyph { name: icon.name().to_string() }
        }
    }
}

fn group(__scope: &mut RenderScope, label: &'static str, writable: impl Fn() -> bool + Copy + 'static, buttons: Vec<NodeHandle>) -> NodeHandle {
    rsx! {
        div {
            class: "pimble-toolbar__group",
            style: {move || if writable() { "" } else { "opacity: 0.4; pointer-events: none;" }},
            div { class: "pimble-toolbar__buttons", {buttons} }
            div { class: "pimble-toolbar__label", {label} }
        }
    }
}

/// The toolbar, rendered once; it follows the focused pane.
pub(crate) fn render_sheet_toolbar(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let focused = move || store.focused_pane.get();
    let writable = move || {
        let pane = focused();
        store
            .pane(pane)
            .sheet
            .get()
            .is_some_and(|held| store.node_access_tracked(held.store_id, held.node_id).allows_write())
    };
    let aim = move || aim_of(store, focused());
    // Run `f` with the focused pane and its aim, when it has one.
    let with_aim = move |f: fn(AppStore, PaneId, Aim)| {
        move || {
            let pane = untracked(focused);
            if let Some(aim) = untracked(|| aim_of(store, pane)) {
                f(store, pane, aim);
            }
        }
    };
    let always = || true;
    let never = || false;
    let has_table = move || aim().is_some_and(|a| !a.table.summary);
    let has_row = move || aim().is_some_and(|a| a.row().is_some());
    let has_col = move || aim().is_some_and(|a| a.col().is_some());
    let has_groups = move || aim().is_some_and(|a| !a.table.groups.is_empty());
    let has_any = move || aim().is_some();
    let can_row = move |by: i32| move || aim().is_some_and(|a| a.row().is_some_and(|r| moving::row_can_step(&a.table, r, by)));
    let can_col = move |by: i32| move || aim().is_some_and(|a| a.col().is_some_and(|c| moving::col_can_step(&a.table, c, by)));

    let sheet = vec![
        button(__scope, TablerIcon::TablePlus, "Add Table", always, never, move || add_table(store, untracked(focused))),
        button(__scope, TablerIcon::MathFunction, "Add Summary Table", always, never, move || add_summary(store, untracked(focused))),
    ];
    let row = vec![
        button(__scope, TablerIcon::RowInsertBottom, "Add Row", has_table, never, with_aim(|store, pane, aim| {
            // The new-row line's first column a person types in.
            if let Some(col) = aim.table.columns.iter().find(|c| !c.computed) {
                cell_pressed(store, pane, CellKey { table: aim.table.id.clone(), row: None, col: col.key.clone() });
            }
        })),
        button(__scope, TablerIcon::ArrowUp, "Move Row Up", can_row(-1), never, with_aim(|store, pane, aim| {
            if let Some(row) = aim.row() {
                moving::step_row(store, pane, &aim.table.id, row, -1);
            }
        })),
        button(__scope, TablerIcon::ArrowDown, "Move Row Down", can_row(1), never, with_aim(|store, pane, aim| {
            if let Some(row) = aim.row() {
                moving::step_row(store, pane, &aim.table.id, row, 1);
            }
        })),
        button(__scope, TablerIcon::RowRemove, "Delete Row", has_row, never, with_aim(|store, pane, aim| {
            if let Some(row) = aim.row() {
                delete_row(store, pane, &aim.table.id, row);
                store.pane(pane).sheet_ui.last.set(None);
            }
        })),
    ];
    let column = vec![
        button(__scope, TablerIcon::ColumnInsertRight, "Add Column", has_table, never, with_aim(|store, pane, aim| open_column(store, pane, &aim.table.id, None))),
        button(__scope, TablerIcon::Adjustments, "Column Settings", has_col, never, with_aim(|store, pane, aim| {
            if let Some(col) = aim.col() {
                open_column(store, pane, &aim.table.id, Some(col));
            }
        })),
        button(
            __scope,
            TablerIcon::ListTree,
            "Group by This Column",
            has_col,
            move || aim().is_some_and(|a| a.col().is_some() && a.table.group_by.as_deref() == a.col()),
            with_aim(|store, pane, aim| {
                if let Some(col) = aim.col() {
                    let again = aim.table.group_by.as_deref() == Some(col);
                    group_by(store, pane, &aim.table.id, if again { None } else { Some(col) });
                }
            }),
        ),
        button(__scope, TablerIcon::ArrowLeft, "Move Column Left", can_col(-1), never, with_aim(|store, pane, aim| {
            if let Some(col) = aim.col() {
                moving::step_col(store, pane, &aim.table.id, col, -1);
            }
        })),
        button(__scope, TablerIcon::ArrowRight, "Move Column Right", can_col(1), never, with_aim(|store, pane, aim| {
            if let Some(col) = aim.col() {
                moving::step_col(store, pane, &aim.table.id, col, 1);
            }
        })),
        button(__scope, TablerIcon::ColumnRemove, "Delete Column", has_col, never, with_aim(|store, pane, aim| {
            if let Some(col) = aim.col() {
                delete_column_of(store, pane, &aim.table.id, col);
                store.pane(pane).sheet_ui.last.set(None);
            }
        })),
    ];
    let table = vec![
        button(__scope, TablerIcon::Sum, "Add Summary Row", has_table, never, with_aim(|store, pane, aim| add_summary_row(store, pane, &aim.table.id))),
        button(
            __scope,
            TablerIcon::RotateClockwise2,
            "Rotate Table (fields down the left)",
            has_table,
            move || aim().is_some_and(|a| a.table.rotated),
            with_aim(|store, pane, aim| rotate_table(store, pane, &aim.table.id, !aim.table.rotated)),
        ),
        button(__scope, TablerIcon::Fold, "Collapse or Expand All Groups", has_groups, never, with_aim(|store, pane, aim| {
            toggle_all_groups(store, pane, &aim.table.id)
        })),
        button(__scope, TablerIcon::TableMinus, "Delete Table", has_any, never, with_aim(|store, pane, aim| {
            delete_table(store, pane, &aim.table.id);
            store.pane(pane).sheet_ui.last.set(None);
        })),
    ];
    let history = vec![
        button(__scope, TablerIcon::ArrowBackUp, "Undo (Ctrl+Z)", always, never, move || {
            undo(store, untracked(focused), false);
        }),
        button(__scope, TablerIcon::ArrowForwardUp, "Redo (Ctrl+Shift+Z)", always, never, move || {
            undo(store, untracked(focused), true);
        }),
    ];

    let groups = vec![
        group(__scope, "Sheet", writable, sheet),
        group(__scope, "Row", writable, row),
        group(__scope, "Column", writable, column),
        group(__scope, "Table", writable, table),
        group(__scope, "History", writable, history),
    ];
    rsx! {
        div {
            class: "pimble-toolbar",
            {groups}
        }
    }
}
