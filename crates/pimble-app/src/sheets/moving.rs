//! Moving rows and columns (Joe, 2026-10-22: drag them, as Google Sheets
//! does, by a grip that shows while the pointer is over the row or the
//! column's header; docs/SHEETS_CONTRACT.md "Moving rows and columns").
//!
//! Whatever moves follows the pointer in the ghost's color and the rows or
//! columns it passes slide aside to make room (Joe: "columns dragged like
//! rows"). A plain table's row is dragged with `Drag::absolute`, every row
//! being `ROW_PITCH` tall. A column, and a rotated table's record or field,
//! is `draggable`: the cells it passes hear the drag over them with their
//! painted box, which is how their sizes are learned (they differ, and the
//! browser backend reports no element's bounds otherwise). A row moves only
//! among the rows of its own group: the group comes from a cell's value, and
//! a drag that changed a cell would surprise. The toolbar's Move buttons
//! step one place.

use std::collections::HashMap;

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

// ── Sliding: what moves follows the pointer, the rest make room ──────────

/// A row or column on the move, along one axis of the screen: `ids` the
/// order it moves in, `from` where it was, `to` where it would land, `offset`
/// how far the pointer has taken it, `size` how much room it takes. `seen`
/// holds where each one it has passed over lies (start and size, as drawn
/// before anything slid), learned as the pointer reached it.
#[derive(Debug, Clone, PartialEq)]
pub struct Slide {
    pub table: String,
    /// A column moves (its cells are named by column key), or a row (by row id).
    pub cols: bool,
    /// Down the screen (a plain table's row, a rotated table's field), or across.
    pub vertical: bool,
    pub ids: Vec<String>,
    pub from: usize,
    pub to: usize,
    pub offset: f32,
    pub size: f32,
    /// Where the pointer was pressed, along the axis.
    pub start: f32,
    pub seen: HashMap<String, (f32, f32)>,
}

impl Slide {
    /// The id a cell's key names along this slide's axis.
    fn id_of<'a>(&self, key: &'a CellKey) -> Option<&'a str> {
        if key.table != self.table {
            return None;
        }
        if self.cols { Some(key.col.as_str()) } else { key.row.as_deref() }
    }

    /// The pointer is at `p` along the axis: what moves is under it, and it
    /// lands past every neighbor whose middle the pointer has crossed.
    pub(crate) fn pointer_at(&mut self, p: f32) {
        self.offset = p - self.start;
        let crossed = |i: usize, forward: bool| {
            self.seen.get(&self.ids[i]).is_some_and(|&(s, w)| if forward { p > s + w / 2.0 } else { p < s + w / 2.0 })
        };
        let mut to = self.from;
        while to + 1 < self.ids.len() && crossed(to + 1, true) {
            to += 1;
        }
        if to == self.from {
            while to > 0 && crossed(to - 1, false) {
                to -= 1;
            }
        }
        self.to = to;
    }

    /// A cell of `id` was reached, drawn at `start` and `size` along the
    /// axis; kept the first time, while nothing has slid it.
    pub(crate) fn saw(&mut self, id: &str, start: f32, size: f32) {
        if id != self.ids[self.from] && size > 0.0 && self.shift(id).is_none() {
            self.seen.entry(id.to_string()).or_insert((start, size));
        }
    }

    /// How far `id` is drawn from its place, and whether it is what moves.
    pub(crate) fn shift(&self, id: &str) -> Option<(f32, bool)> {
        let i = self.ids.iter().position(|x| x == id)?;
        if i == self.from {
            return Some((self.offset, true));
        }
        let (from, to) = (self.from, self.to);
        let by = if from < to && i > from && i <= to {
            -self.size
        } else if to < from && i >= to && i < from {
            self.size
        } else {
            return None;
        };
        Some((by, false))
    }

    /// How cell `key` draws: lifted under the pointer, or slid aside.
    pub(crate) fn style_of(&self, key: &CellKey) -> String {
        let Some((by, lifted)) = self.id_of(key).and_then(|id| self.shift(id)) else { return String::new() };
        let axis = if self.vertical { "Y" } else { "X" };
        if lifted {
            // It lets the pointer through to the cells it passes, which say
            // where they are.
            format!("position: relative; z-index: 3; pointer-events: none; background: var(--pimble-sheet-ghost); transform: translate{axis}({by:.1}px);")
        } else {
            format!("position: relative; transform: translate{axis}({by:.1}px);")
        }
    }
}

thread_local! {
    /// The box of the cell a grip sits in, and the press, from the last
    /// press on one: the moved one's size, and where its offset counts from.
    static PRESSED: std::cell::Cell<(f32, f32, f32, f32)> = const { std::cell::Cell::new((0.0, 0.0, 0.0, 0.0)) };
}

/// A press in the cell holding a grip that drags by drop (a column's header,
/// a rotated table's field name or record foot).
pub(crate) fn pressed() {
    let ctx = rinch::core::get_click_context();
    PRESSED.with(|p| p.set((ctx.mouse_x, ctx.mouse_y, ctx.element_width, ctx.element_height)));
}

/// A grip's drag starts: what moves follows the pointer, drawn by the app
/// (rinch's own copy of the grip is hidden).
pub(crate) fn start(store: AppStore, pane: PaneId, what: Moving) {
    let ui = store.pane(pane).sheet_ui;
    let (table, id, cols) = match &what {
        Moving::Row { table, row } => (table.clone(), row.clone(), false),
        Moving::Col { table, col } => (table.clone(), col.clone(), true),
    };
    let Some(t) = table_of(store, pane, &table) else { return };
    let ids: Vec<String> = if cols { columns(&t) } else { siblings(&t, &id) }.into_iter().map(str::to_string).collect();
    let Some(from) = ids.iter().position(|x| *x == id) else { return };
    rinch::core::suppress_drag_ghost();
    commit(store, pane, Move::Stay);
    // A rotated table's field runs across the screen and moves down it; its
    // records are columns on screen and move across.
    let vertical = cols && t.rotated;
    let (x, y, w, h) = PRESSED.with(|p| p.get());
    let (start, size) = if vertical { (y, h) } else { (x, w) };
    ui.slide.set(Some(Slide { table, cols, vertical, ids, from, to: from, offset: 0.0, size, start, seen: HashMap::new() }));
    ui.moving.set(Some(what));
}

/// The pointer moved during the drag.
pub(crate) fn dragged(store: AppStore, pane: PaneId) {
    let ui = store.pane(pane).sheet_ui;
    let ctx = rinch::core::get_click_context();
    let Some(mut s) = untracked(|| ui.slide.get()) else { return };
    s.pointer_at(if s.vertical { ctx.mouse_y } else { ctx.mouse_x });
    ui.slide.set(Some(s));
}

/// The drag passes over cell `key`, drawn in the box the click context holds.
pub(crate) fn over(store: AppStore, pane: PaneId, key: &CellKey) {
    let ui = store.pane(pane).sheet_ui;
    let ctx = rinch::core::get_click_context();
    let Some(mut s) = untracked(|| ui.slide.get()) else { return };
    let Some(id) = s.id_of(key).map(str::to_string) else { return };
    let (start, size) = if s.vertical { (ctx.element_y, ctx.element_height) } else { (ctx.element_x, ctx.element_width) };
    s.saw(&id, start, size);
    s.pointer_at(if s.vertical { ctx.mouse_y } else { ctx.mouse_x });
    ui.slide.set_if_changed(Some(s));
}

/// The drag ends, dropped or not.
pub(crate) fn end(store: AppStore, pane: PaneId) {
    let ui = store.pane(pane).sheet_ui;
    ui.moving.set(None);
    ui.slide.set(None);
}

/// The drag is let go over a cell: what moved lands where it is shown.
pub(crate) fn dropped(store: AppStore, pane: PaneId) {
    let ui = store.pane(pane).sheet_ui;
    let done = untracked(|| ui.slide.get());
    end(store, pane);
    let Some(s) = done.filter(|s| s.to != s.from) else { return };
    let (moved, target) = (s.ids[s.from].clone(), s.ids[s.to].clone());
    let side = if s.to > s.from { DropSide::After } else { DropSide::Before };
    aim(pane, Some(&s.table));
    if s.cols {
        local_edit(store, pane, |t| t.move_column(&s.table, &moved, placement(&target, side)));
    } else {
        local_edit(store, pane, |t| t.move_row(&s.table, &moved, placement(&target, side)));
    }
}

/// How cell `key` draws while something slides.
pub(crate) fn style_of(slide: &Option<Slide>, key: &CellKey) -> String {
    slide.as_ref().map(|s| s.style_of(key)).unwrap_or_default()
}

// ── A plain table's row ──────────────────────────────────────────────────

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

// ── The toolbar's steps ──────────────────────────────────────────────────

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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Columns a (0-100), b (100-160), c (160-300), d (300-340); b is dragged
    /// from its middle, x = 130.
    fn sliding_b() -> Slide {
        Slide {
            table: "t".into(),
            cols: true,
            vertical: false,
            ids: ["a", "b", "c", "d"].map(String::from).to_vec(),
            from: 1,
            to: 1,
            offset: 0.0,
            size: 60.0,
            start: 130.0,
            seen: HashMap::new(),
        }
    }

    #[test]
    fn a_dragged_column_passes_a_neighbor_once_the_pointer_crosses_its_middle() {
        let mut s = sliding_b();
        // Into c: learned there, but its middle (230) is not crossed yet.
        s.saw("c", 160.0, 140.0);
        s.pointer_at(200.0);
        assert_eq!((s.to, s.offset), (1, 70.0), "it follows the pointer; c stays");
        s.pointer_at(240.0);
        assert_eq!(s.to, 2, "past c's middle: it would land after c");
        assert_eq!(s.shift("c"), Some((-60.0, false)), "c slides left by b's width");
        assert_eq!(s.shift("b"), Some((110.0, true)));
        // c, slid, is reached again: what was learned before it slid stands.
        s.saw("c", 100.0, 140.0);
        assert_eq!(s.seen["c"], (160.0, 140.0));
        // d is not learned yet, so even far right it lands after c.
        s.pointer_at(330.0);
        assert_eq!(s.to, 2);
        s.saw("d", 300.0, 40.0);
        s.pointer_at(330.0);
        assert_eq!(s.to, 3, "past d's middle too");
        // Back to where it started: everything back in place.
        s.pointer_at(130.0);
        assert_eq!((s.to, s.shift("c"), s.shift("d")), (1, None, None));
        // Left past a's middle.
        s.saw("a", 0.0, 100.0);
        s.pointer_at(40.0);
        assert_eq!(s.to, 0);
        assert_eq!(s.shift("a"), Some((60.0, false)), "a slides right to make room");
    }

    #[test]
    fn a_cell_draws_by_the_slide() {
        let mut s = sliding_b();
        s.saw("c", 160.0, 140.0);
        s.pointer_at(240.0);
        let cell = |col: &str| CellKey { table: "t".into(), row: Some("r".into()), col: col.into() };
        assert!(s.style_of(&cell("b")).contains("translateX(110.0px)"), "{}", s.style_of(&cell("b")));
        assert!(s.style_of(&cell("b")).contains("pointer-events: none"));
        assert!(s.style_of(&cell("c")).contains("translateX(-60.0px)"));
        assert_eq!(s.style_of(&cell("d")), "");
        let elsewhere = CellKey { table: "u".into(), row: None, col: "b".into() };
        assert_eq!(s.style_of(&elsewhere), "", "another table's cells stay put");
        // A rotated table's field moves down the screen.
        let field = Slide { vertical: true, ..s.clone() };
        assert!(field.style_of(&cell("b")).contains("translateY("));
        // A record (a row) is named by its row id.
        let record = Slide { cols: false, ids: ["r1", "r2"].map(String::from).to_vec(), from: 0, to: 0, offset: 12.0, ..sliding_b() };
        let r1 = CellKey { table: "t".into(), row: Some("r1".into()), col: "whatever".into() };
        assert!(record.style_of(&r1).contains("translateX(12.0px)"));
    }
}
