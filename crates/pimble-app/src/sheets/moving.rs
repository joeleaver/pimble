//! Moving rows and columns (Joe, 2026-10-22: drag them, as Google Sheets
//! does, by a grip that shows while the pointer is over the row or the
//! column's header; docs/SHEETS_CONTRACT.md "Moving rows and columns").
//!
//! A grip is `draggable`; every cell hears the drag pass over it and the drop,
//! so a move works the same on a table drawn either way round (a rotated
//! table's records are its columns on screen). A row moves only among the rows
//! of its own group: the group comes from a cell's value, and a drag that
//! changed a cell would surprise. The toolbar's Move buttons step one place.

use rinch::prelude::*;

use super::{aim, commit, local_edit, CellKey, Move, TableView};
use crate::panes::PaneId;
use crate::state::AppStore;
use pimble_sheet::doc::Placement;

/// What a grip is dragging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Moving {
    Row { table: String, row: String },
    Col { table: String, col: String },
}

/// Where the dragged row or column would land: beside `target` (a row id or
/// a column key), `after` it or before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropAt {
    pub table: String,
    pub target: String,
    pub after: bool,
}

/// The rows a row of `t` moves among: its group's, or every row.
fn siblings<'a>(t: &'a TableView, row: &str) -> Vec<&'a str> {
    let all: Vec<&str> = t.rows.iter().map(|r| r.id.as_str()).collect();
    if t.groups.is_empty() {
        return all;
    }
    t.groups
        .iter()
        .map(|g| g.rows.iter().filter_map(|&i| all.get(i).copied()).collect::<Vec<_>>())
        .find(|ids| ids.contains(&row))
        .unwrap_or_default()
}

fn columns(t: &TableView) -> Vec<&str> {
    t.columns.iter().filter(|c| !c.result).map(|c| c.key.as_str()).collect()
}

/// Dropped on `target`, `moved` takes its place, `target` giving way toward
/// where `moved` was: before it when `moved` came from after it.
pub(crate) fn landing(order: &[&str], moved: &str, target: &str) -> Option<DropSide> {
    let from = order.iter().position(|id| *id == moved)?;
    let to = order.iter().position(|id| *id == target)?;
    match from.cmp(&to) {
        std::cmp::Ordering::Equal => None,
        std::cmp::Ordering::Greater => Some(DropSide::Before),
        std::cmp::Ordering::Less => Some(DropSide::After),
    }
}

/// One step of `moved` along `order` (`by` is -1 or 1): beside the neighbor
/// it passes, or `None` at the end.
pub(crate) fn step(order: &[&str], moved: &str, by: i32) -> Option<(String, DropSide)> {
    let from = order.iter().position(|id| *id == moved)? as i64;
    let to = from + by as i64;
    if to < 0 || to >= order.len() as i64 {
        return None;
    }
    let side = if by < 0 { DropSide::Before } else { DropSide::After };
    Some((order[to as usize].to_string(), side))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DropSide {
    Before,
    After,
}

fn placement(target: &str, side: DropSide) -> Placement {
    match side {
        DropSide::Before => Placement::Before(target.to_string()),
        DropSide::After => Placement::After(target.to_string()),
    }
}

fn table_of(store: AppStore, pane: PaneId, table: &str) -> Option<TableView> {
    untracked(|| store.pane(pane).sheet_ui.tables.get()).into_iter().find(|t| t.id == table)
}

/// Where the drag now under way would land on the cell `key`, if it may.
fn landing_on(store: AppStore, pane: PaneId, moving: &Moving, key: &CellKey) -> Option<DropAt> {
    match moving {
        Moving::Row { table, row } if *table == key.table => {
            let target = key.row.as_deref()?;
            let t = table_of(store, pane, table)?;
            let side = landing(&siblings(&t, row), row, target)?;
            Some(DropAt { table: table.clone(), target: target.to_string(), after: side == DropSide::After })
        }
        Moving::Col { table, col } if *table == key.table => {
            let t = table_of(store, pane, table)?;
            let side = landing(&columns(&t), col, &key.col)?;
            Some(DropAt { table: table.clone(), target: key.col.clone(), after: side == DropSide::After })
        }
        _ => None,
    }
}

/// A grip's drag starts.
pub(crate) fn start(store: AppStore, pane: PaneId, what: Moving) {
    let ui = store.pane(pane).sheet_ui;
    commit(store, pane, Move::Stay);
    ui.drop_at.set(None);
    ui.moving.set(Some(what));
}

/// The drag ends, dropped or not.
pub(crate) fn end(store: AppStore, pane: PaneId) {
    let ui = store.pane(pane).sheet_ui;
    ui.moving.set(None);
    ui.drop_at.set(None);
}

/// The drag passes over cell `key`.
pub(crate) fn over(store: AppStore, pane: PaneId, key: &CellKey) {
    let ui = store.pane(pane).sheet_ui;
    let Some(moving) = untracked(|| ui.moving.get()) else { return };
    ui.drop_at.set_if_changed(landing_on(store, pane, &moving, key));
}

/// The drag is let go over cell `key`.
pub(crate) fn dropped(store: AppStore, pane: PaneId, key: &CellKey) {
    let ui = store.pane(pane).sheet_ui;
    let Some(moving) = untracked(|| ui.moving.get()) else { return };
    let at = landing_on(store, pane, &moving, key);
    end(store, pane);
    let Some(at) = at else { return };
    let side = if at.after { DropSide::After } else { DropSide::Before };
    aim(pane, Some(&at.table));
    match moving {
        Moving::Row { table, row } => {
            local_edit(store, pane, |t| t.move_row(&table, &row, placement(&at.target, side)));
        }
        Moving::Col { table, col } => {
            local_edit(store, pane, |t| t.move_column(&table, &col, placement(&at.target, side)));
        }
    }
}

/// A row's height, which every row has (`.pimble-sheet__cell`'s line
/// height, its padding and its bottom line): a live row drag reckons from it.
pub(crate) const ROW_PITCH: f32 = 31.0;

/// A plain table's row being dragged by its grip: it follows the pointer in
/// the ghost's color (`dy` from where it started), and the rows it passes
/// make room. `ids` are the rows of its group in order, `from` where it was
/// and `to` where it would land.
#[derive(Debug, Clone, PartialEq)]
pub struct RowDrag {
    pub table: String,
    pub ids: Vec<String>,
    pub from: usize,
    pub to: usize,
    pub dy: f32,
}

impl RowDrag {
    /// Where it is after the pointer moved `dy` from where it was pressed.
    pub(crate) fn moved(&self, dy: f32) -> RowDrag {
        let last = self.ids.len().saturating_sub(1) as f32;
        let dy = dy.clamp(-(self.from as f32) * ROW_PITCH, (last - self.from as f32) * ROW_PITCH);
        let to = (self.from as f32 + (dy / ROW_PITCH).round()).clamp(0.0, last) as usize;
        RowDrag { dy, to, ..self.clone() }
    }

    /// How far row `row` of `table` is drawn from its place: the dragged
    /// one under the pointer, each one it passed a row toward where it was.
    pub(crate) fn offset(&self, table: &str, row: &str) -> Option<(f32, bool)> {
        if table != self.table {
            return None;
        }
        let i = self.ids.iter().position(|id| id == row)?;
        if i == self.from {
            return Some((self.dy, true));
        }
        let (from, to) = (self.from, self.to);
        let shift = if from < to && i > from && i <= to {
            -ROW_PITCH
        } else if to < from && i >= to && i < from {
            ROW_PITCH
        } else {
            return None;
        };
        Some((shift, false))
    }
}

/// A press on a plain table's row grip: the row follows the pointer until it
/// is let go, and lands where it is then.
pub(crate) fn press_row(store: AppStore, pane: PaneId, table: &str, row: &str) {
    let Some(t) = table_of(store, pane, table) else { return };
    let ids: Vec<String> = siblings(&t, row).into_iter().map(str::to_string).collect();
    let Some(from) = ids.iter().position(|id| id == row) else { return };
    commit(store, pane, Move::Stay);
    let ui = store.pane(pane).sheet_ui;
    let start_y = rinch::core::get_click_context().mouse_y;
    let drag = RowDrag { table: table.to_string(), ids, from, to: from, dy: 0.0 };
    ui.row_drag.set(Some(drag.clone()));
    ui.row_dragging.set(true);
    let at_start = drag.clone();
    rinch::core::Drag::absolute()
        .on_move(move |_, y| ui.row_drag.set(Some(at_start.moved(y - start_y))))
        .on_end(move |_, _| {
            // The last move is where it was let go.
            let done = untracked(|| ui.row_drag.get());
            ui.row_drag.set(None);
            ui.row_dragging.set(false);
            let Some(d) = done.filter(|d| d.to != d.from) else { return };
            let target = d.ids[d.to].clone();
            let side = if d.to > d.from { DropSide::After } else { DropSide::Before };
            let row = d.ids[d.from].clone();
            aim(pane, Some(&d.table));
            local_edit(store, pane, |t| t.move_row(&d.table, &row, placement(&target, side)));
        })
        .on_cancel(move |_, _| {
            ui.row_drag.set(None);
            ui.row_dragging.set(false);
        })
        .start();
}

/// The toolbar's Move Up and Move Down: row `row` of `table` one place
/// along its group (`by` -1 or 1).
pub(crate) fn step_row(store: AppStore, pane: PaneId, table: &str, row: &str, by: i32) {
    let Some(t) = table_of(store, pane, table) else { return };
    let Some((target, side)) = step(&siblings(&t, row), row, by) else { return };
    commit(store, pane, Move::Stay);
    aim(pane, Some(table));
    local_edit(store, pane, |t| t.move_row(table, row, placement(&target, side)));
}

/// The toolbar's Move Left and Move Right, for column `col`.
pub(crate) fn step_col(store: AppStore, pane: PaneId, table: &str, col: &str, by: i32) {
    let Some(t) = table_of(store, pane, table) else { return };
    let Some((target, side)) = step(&columns(&t), col, by) else { return };
    commit(store, pane, Move::Stay);
    aim(pane, Some(table));
    local_edit(store, pane, |t| t.move_column(table, col, placement(&target, side)));
}

/// Whether row `row` of `table` can step `by` (for dimming the buttons).
pub(crate) fn row_can_step(t: &TableView, row: &str, by: i32) -> bool {
    step(&siblings(t, row), row, by).is_some()
}

pub(crate) fn col_can_step(t: &TableView, col: &str, by: i32) -> bool {
    step(&columns(t), col, by).is_some()
}

/// The marks cell `key` shows while a drag is under way: faint when it is
/// what moves, a line on the side the moved one would land. `rotated`: the
/// table is drawn with its records as columns.
pub(crate) fn marks(moving: &Option<Moving>, drop_at: &Option<DropAt>, key: &CellKey, rotated: bool) -> &'static str {
    let Some(moving) = moving else { return "" };
    let (is_moved, along_rows) = match moving {
        Moving::Row { table, row } => (*table == key.table && key.row.as_deref() == Some(row.as_str()), true),
        Moving::Col { table, col } => (*table == key.table && key.col == *col, false),
    };
    if is_moved {
        return " pimble-sheet__cell--moving";
    }
    let Some(at) = drop_at.as_ref().filter(|a| a.table == key.table) else { return "" };
    let hit = if along_rows { key.row.as_deref() == Some(at.target.as_str()) } else { key.col == at.target };
    if !hit {
        return "";
    }
    // A row is a line across the screen in a plain table and down it in a
    // rotated one; a column the other way round.
    match (along_rows != rotated, at.after) {
        (true, false) => " pimble-sheet__cell--drop-above",
        (true, true) => " pimble-sheet__cell--drop-below",
        (false, false) => " pimble-sheet__cell--drop-left",
        (false, true) => " pimble-sheet__cell--drop-right",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_row_takes_the_place_of_the_one_it_lands_on() {
        let order = ["a", "b", "c", "d"];
        // Down the list: after the target, which moves up toward where it was.
        assert_eq!(landing(&order, "a", "c"), Some(DropSide::After));
        // Up the list: before it.
        assert_eq!(landing(&order, "d", "b"), Some(DropSide::Before));
        assert_eq!(landing(&order, "b", "b"), None, "on itself: nothing moves");
        assert_eq!(landing(&order, "a", "z"), None, "a target that is not a sibling (another group) takes nothing");
    }

    #[test]
    fn a_step_passes_one_neighbor_and_stops_at_the_ends() {
        let order = ["a", "b", "c"];
        assert_eq!(step(&order, "b", -1), Some(("a".to_string(), DropSide::Before)));
        assert_eq!(step(&order, "b", 1), Some(("c".to_string(), DropSide::After)));
        assert_eq!(step(&order, "a", -1), None);
        assert_eq!(step(&order, "c", 1), None);
    }

    #[test]
    fn a_dragged_row_lands_where_the_pointer_has_taken_it_and_the_rows_it_passed_make_room() {
        let start = RowDrag { table: "t".into(), ids: ["a", "b", "c", "d"].map(String::from).to_vec(), from: 1, to: 1, dy: 0.0 };
        // Down a row and a half: past c, so it lands after c, and c moves up.
        let d = start.moved(ROW_PITCH * 1.6);
        assert_eq!(d.to, 3, "1.6 rows rounds to two");
        let d = start.moved(ROW_PITCH * 1.2);
        assert_eq!(d.to, 2);
        assert_eq!(d.offset("t", "b"), Some((ROW_PITCH * 1.2, true)), "the dragged row follows the pointer");
        assert_eq!(d.offset("t", "c"), Some((-ROW_PITCH, false)), "the row it passed moves up into its place");
        assert_eq!(d.offset("t", "d"), None);
        assert_eq!(d.offset("t", "a"), None);
        // Up past a, and beyond: held at the top of its group.
        let d = start.moved(-ROW_PITCH * 5.0);
        assert_eq!((d.to, d.dy), (0, -ROW_PITCH));
        assert_eq!(d.offset("t", "a"), Some((ROW_PITCH, false)));
        assert_eq!(d.offset("u", "a"), None, "another table's rows stay put");
    }

    #[test]
    fn the_landing_line_follows_the_table_round() {
        let row = |r: &str| CellKey { table: "t".into(), row: Some(r.into()), col: "c1".into() };
        let moving = Some(Moving::Row { table: "t".into(), row: "a".into() });
        let at = Some(DropAt { table: "t".into(), target: "b".into(), after: true });
        assert_eq!(marks(&moving, &at, &row("a"), false), " pimble-sheet__cell--moving");
        assert_eq!(marks(&moving, &at, &row("b"), false), " pimble-sheet__cell--drop-below");
        assert_eq!(marks(&moving, &at, &row("b"), true), " pimble-sheet__cell--drop-right", "a record is a column when rotated");
        assert_eq!(marks(&moving, &at, &row("c"), false), "");
        let col = Some(Moving::Col { table: "t".into(), col: "c1".into() });
        let at = Some(DropAt { table: "t".into(), target: "c2".into(), after: false });
        let cell = CellKey { table: "t".into(), row: Some("a".into()), col: "c2".into() };
        assert_eq!(marks(&col, &at, &cell, false), " pimble-sheet__cell--drop-left");
        assert_eq!(marks(&col, &at, &cell, true), " pimble-sheet__cell--drop-above", "a field is a row when rotated");
        assert_eq!(marks(&None, &at, &cell, false), "", "nothing while no drag is under way");
    }
}
