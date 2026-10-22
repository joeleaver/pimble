//! The sheet view's DOM (docs/SHEETS_CONTRACT.md section 3): each table with
//! its title row, one-line column headers, the cells, the new-row line, the
//! column settings and the formula bar. Rendered once per pane slot; a table
//! is keyed by its id and version, so only a table that changed is built
//! again.

use super::*;
use crate::appearance::IconGlyph;
use rinch_tabler_icons::TablerIcon;

thread_local! {
    /// Each pane's cell input: (document key, node id), for focusing it.
    static EDIT_INPUTS: RefCell<[Option<(u64, usize)>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
}

/// Put the keyboard in pane `pane`'s cell input once it shows.
pub(super) fn focus_edit_input(pane: PaneId) {
    let _ = set_timeout(0, move || {
        if let Some((doc_key, node)) = EDIT_INPUTS.with(|t| t.borrow()[pane.index()]) {
            rinch::core::request_focus(doc_key, node);
        }
    });
    #[cfg(not(feature = "native"))]
    {
        use wasm_bindgen::JsCast;
        let _ = set_timeout(40, move || {
            let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
            let Ok(list) = document.query_selector_all("input.pimble-sheet__edit") else { return };
            for i in 0..list.length() {
                let Some(el) = list.get(i).and_then(|n| n.dyn_into::<web_sys::HtmlElement>().ok()) else { continue };
                if el.offset_width() > 0 {
                    let _ = el.focus();
                    return;
                }
            }
        });
    }
}

/// A press in one of the view's one-line fields (a table's name, a
/// summary's formula, the formula bar): Enter commits it with `commit`,
/// Escape drops the change. Plain inputs hear Enter only through the
/// keyboard interceptor, like the app's other inline fields.
fn enter_commits(store: AppStore, pane: PaneId, commit: impl Fn() + 'static) {
    super::commit(store, pane, Move::Stay);
    let commit: std::rc::Rc<dyn Fn()> = std::rc::Rc::new(commit);
    // A table in text hears its keys through its node view instead
    // (`field_key`), which the editor offers them to first.
    FIELD.with(|f| *f.borrow_mut() = Some((pane, commit.clone())));
    rinch::core::set_keyboard_interceptor(move |data| {
        let key = data.key.clone();
        let handled = matches!(key.as_str(), "Enter" | "Escape");
        if !handled || data.kind != rinch::core::KeyEventKind::Down {
            return handled;
        }
        field_key(store, pane, &key)
    });
}

thread_local! {
    /// The one-line field being edited: its pane, and what Enter commits.
    static FIELD: RefCell<Option<(PaneId, std::rc::Rc<dyn Fn()>)>> = const { RefCell::new(None) };
}

/// Enter or Escape in pane `pane`'s one-line field: commit it, or drop the
/// change, a turn later. `false` when no field of the pane is being edited.
pub(super) fn field_key(store: AppStore, pane: PaneId, key: &str) -> bool {
    if !matches!(key, "Enter" | "Escape") {
        return false;
    }
    let Some(commit) = FIELD.with(|f| {
        let mut f = f.borrow_mut();
        if f.as_ref().is_some_and(|(p, _)| *p == pane) {
            f.take().map(|(_, c)| c)
        } else {
            None
        }
    }) else {
        return false;
    };
    let enter = key == "Enter";
    let _ = set_timeout(0, move || {
        rinch::core::clear_keyboard_interceptor();
        if enter {
            commit();
        } else {
            recompute(store, pane);
        }
    });
    true
}

/// Pane `pane`'s sheet view: shown while it holds a sheet node.
pub(crate) fn render_sheet(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let state = store.pane(pane);
    let ui = state.sheet_ui;
    rsx! {
        div {
            class: "pimble-sheet",
            style: {move || if state.show_sheet.get() { "" } else { "display: none;" }},
            if let Some(message) = ui.message.get() {
                div { class: "pimble-sheet__message", {message} }
            }
            // A pane holding a note draws its tables in the text instead.
            for table in node_tables(store, pane) {
                div {
                    key: format!("{}:{}", table.id, table.version),
                    {table_node(__scope, store, pane, table.clone())}
                }
            }
            div {
                class: "pimble-sheet__adders",
                button { class: "pimble-sheet__adder", onclick: move || add_table(store, pane), "+ Table" }
                button { class: "pimble-sheet__adder", onclick: move || add_summary(store, pane), "+ Summary" }
                button { class: "pimble-sheet__adder", title: "Undo your last change to this sheet", onclick: move || { undo(store, pane, false); }, "Undo" }
                button { class: "pimble-sheet__adder", title: "Redo", onclick: move || { undo(store, pane, true); }, "Redo" }
            }
        }
    }
}

/// The tables pane `pane`'s sheet view draws: none while the pane holds a
/// note, whose tables are drawn in its text.
fn node_tables(store: AppStore, pane: PaneId) -> Vec<TableView> {
    let state = store.pane(pane);
    if state.show_sheet.get() {
        state.sheet_ui.tables.get()
    } else {
        Vec::new()
    }
}

pub(super) fn table_node(__scope: &mut RenderScope, store: AppStore, pane: PaneId, t: TableView) -> NodeHandle {
    let ui = store.pane(pane).sheet_ui;
    let tid = t.id.clone();
    let name = Signal::new(t.name.clone());
    let source = Signal::new(t.source.clone());
    let rows_label = format!("{} {}", t.rows.len(), if t.rows.len() == 1 { "row" } else { "rows" });
    let columns = t.columns.clone();

    let id_name = tid.clone();
    let id_source = tid.clone();
    let id_delete = tid.clone();
    let id_convert = tid.clone();
    let id_add = tid.clone();
    let id_sum = tid.clone();
    let id_collapse = tid.clone();
    let id_rotate = tid.clone();
    let rotated = t.rotated;
    let grouped = !t.groups.is_empty();
    let summary_tid = tid.clone();
    let summary = t.summary;
    let source_row = summary.then(|| {
        rsx! {
            input {
                class: "pimble-sheet__source",
                value: move || source.get(),
                oninput: move |v: String| source.set(v),
                onclick: {
                    let id = id_source.clone();
                    move || {
                        let id = id.clone();
                        enter_commits(store, pane, move || set_source(store, pane, &id, &source.get()))
                    }
                },
            }
        }
    });
    let error = t.error.clone().or_else(|| t.note.clone()).map(|e| rsx! { div { class: "pimble-sheet__error", {e} } });

    let heads: Vec<NodeHandle> = columns
        .iter()
        .map(|c| {
            let open = {
                let (tid, key) = (tid.clone(), c.key.clone());
                move || open_column(store, pane, &tid, Some(&key))
            };
            let mark = (!c.mark.is_empty()).then(|| rsx! { span { class: "pimble-sheet__mark", {format!("\u{00a0}{}", c.mark)} } });
            let class = format!(
                "pimble-sheet__head{}{}",
                if c.numeric { " pimble-sheet__num" } else { "" },
                if c.computed { " pimble-sheet__head--computed" } else { "" }
            );
            let tip = if c.result { format!("{} · click to rename", c.tip) } else { format!("{} · click to change", c.tip) };
            let press = {
                let (tid, col) = (tid.clone(), c.clone());
                move || start_resize(store, pane, &tid, &col)
            };
            let grip = (!summary && !c.result).then(|| col_grip(__scope, store, pane, &tid, &c.key, false));
            let hover = (tid.clone(), c.key.clone());
            let key = CellKey { table: tid.clone(), row: None, col: c.key.clone() };
            let (over_key, mark_key) = (key.clone(), key);
            // The header is the cell under the column's settings; its right
            // edge is a grip beside it, so a drag there opens nothing.
            rsx! {
                div {
                    class: "pimble-sheet__head-wrap",
                    style: move || moving::style_of(&ui.slide.get(), &mark_key),
                    onmousedown: press,
                    onmouseenter: move || ui.hover_col.set_if_changed(Some(hover.clone())),
                    onmouseleave: move || ui.hover_col.set_if_changed(None),
                    ondragover: move || moving::over(store, pane, &over_key),
                    ondrop: move || moving::dropped(store, pane),
                    div { class: class, title: tip, onclick: open, {c.name.clone()} {mark} }
                    {grip}
                    div { class: "pimble-sheet__grip", title: "Drag to change this column's width" }
                }
            }
        })
        .collect();

    // A row, hidden while the group it is in is collapsed (`group`: that
    // group's key).
    let row_node = |__scope: &mut RenderScope, r: &RowView, group: Option<String>| {
        let cells: Vec<NodeHandle> = columns
            .iter()
            .zip(&r.cells)
            .map(|(c, cell)| cell_node(__scope, store, pane, CellKey { table: tid.clone(), row: Some(r.id.clone()), col: c.key.clone() }, c, Some(cell.clone())))
            .collect();
        let tail = (!summary).then(|| {
            let (tid, rid) = (tid.clone(), r.id.clone());
            rsx! {
                button { class: "pimble-sheet__delete-row", title: "Delete this row", onclick: move || delete_row(store, pane, &tid, &rid), "×" }
            }
        });
        let hidden = collapsed_fn(store, pane, &tid, group);
        let grip = (!summary).then(|| row_grip(__scope, store, pane, &tid, &r.id, ""));
        let hover = (tid.clone(), r.id.clone());
        let (gutter_at, tail_at) = ((tid.clone(), r.id.clone()), (tid.clone(), r.id.clone()));
        rsx! {
            div {
                class: "pimble-sheet__row",
                style: move || if hidden.get() { "display: none;" } else { "" },
                div {
                    class: "pimble-sheet__gutter",
                    style: move || ghost_style(&ui.row_drag.get(), &gutter_at.0, Some(&gutter_at.1)),
                    onmouseenter: move || ui.hover_row.set_if_changed(Some(hover.clone())),
                    {grip}
                }
                {cells}
                div {
                    class: "pimble-sheet__tail",
                    style: move || ghost_style(&ui.row_drag.get(), &tail_at.0, Some(&tail_at.1)),
                    {tail}
                }
            }
        }
    };
    let mut body: Vec<NodeHandle> = Vec::new();
    if t.groups.is_empty() {
        for r in &t.rows {
            body.push(row_node(__scope, r, None));
        }
    } else {
        for g in &t.groups {
            let (gt, gk) = (tid.clone(), g.key.clone());
            let open = collapsed_fn(store, pane, &tid, Some(g.key.clone()));
            let count = format!("{} {}", g.rows.len(), if g.rows.len() == 1 { "row" } else { "rows" });
            let (label, label_tip) = (g.label.clone(), g.label.clone());
            body.push(rsx! {
                div {
                    class: "pimble-sheet__group",
                    onclick: move || toggle_group(store, pane, &gt, &gk),
                    span { class: "pimble-sheet__group-chevron", {move || if open.get() { "▸" } else { "▾" }} }
                    span { class: "pimble-sheet__group-label", title: label_tip, {label} }
                    span { class: "pimble-sheet__group-count", {count} }
                }
            });
            for &i in &g.rows {
                if let Some(r) = t.rows.get(i) {
                    body.push(row_node(__scope, r, Some(g.key.clone())));
                }
            }
            for s in &g.subtotals {
                body.push(summary_row_node(__scope, store, pane, &tid, &columns, s, true));
            }
        }
    }
    let bottom: Vec<NodeHandle> = t.summary_rows.iter().map(|s| summary_row_node(__scope, store, pane, &tid, &columns, s, false)).collect();

    let new_row = (!summary).then(|| {
        let mut first = true;
        let cells: Vec<NodeHandle> = columns
            .iter()
            .map(|c| {
                let key = CellKey { table: tid.clone(), row: None, col: c.key.clone() };
                let placeholder = (!c.computed && std::mem::replace(&mut first, false)).then(|| "Add a row…".to_string());
                let cell = placeholder.map(|p| CellView { text: p, error: None, chip: None });
                cell_node(__scope, store, pane, key, c, cell)
            })
            .collect();
        let gap = gutter(__scope);
        rsx! { div { class: "pimble-sheet__row pimble-sheet__new-row", {gap} {cells} div { class: "pimble-sheet__tail" } } }
    });
    let empty = (summary && t.rows.is_empty() && t.error.is_none())
        .then(|| rsx! { div { class: "pimble-sheet__empty", "Nothing matches yet." } });

    let (grid_tid, grid_cols) = (tid.clone(), columns.clone());
    let rotated_layout = t.rotated && !summary;
    let (rot_style, grid_children): (String, Vec<NodeHandle>) = if rotated_layout {
        rotated_grid(__scope, store, pane, &t)
    } else {
        let id_add = id_add.clone();
        let head_gap = gutter(__scope);
        let head_row = rsx! {
            div {
                class: "pimble-sheet__row",
                {head_gap}
                {heads}
                div {
                    class: "pimble-sheet__add-col",
                    title: "Add a column",
                    onclick: move || open_column(store, pane, &id_add, None),
                    "+"
                }
            }
        };
        let mut children = vec![head_row];
        children.extend(body);
        children.extend(bottom);
        children.extend(new_row);
        (String::new(), children)
    };
    let form_tid = tid.clone();
    let bar_tid = tid.clone();
    let note_tid = tid.clone();
    let scroll_tid = tid.clone();
    rsx! {
        section {
            class: "pimble-sheet__table",
            div {
                class: "pimble-sheet__title",
                input {
                    class: "pimble-sheet__name",
                    // As wide as the name, so the chip after it sits beside it.
                    style: move || format!("width: {}ch;", name.get().chars().count().max(4) + 2),
                    value: move || name.get(),
                    oninput: move |v: String| name.set(v),
                    onclick: {
                        let id = id_name.clone();
                        move || {
                            let id = id.clone();
                            enter_commits(store, pane, move || rename_table(store, pane, &id, &name.get()))
                        }
                    },
                }
                span { class: "pimble-sheet__kind", {if summary { "summary" } else { "table" }} }
                {source_row}
                if ui.selected.get().is_some_and(|k| k.table == bar_tid) && !ui.formula_text.get().is_empty() {
                    input {
                        class: "pimble-sheet__formula",
                        value: move || ui.formula_text.get(),
                        oninput: move |v: String| ui.formula_text.set(v),
                        onclick: move || enter_commits(store, pane, move || set_formula(store, pane, ui.formula_text.get())),
                    }
                }
                if let Some(note) = ui.selected.get().filter(|k| k.table == note_tid).and_then(|_| ui.formula_note.get()) {
                    span { class: "pimble-sheet__note", title: move || ui.formula_note.get().unwrap_or_default(), {note} }
                }
                span { class: "pimble-sheet__count", {rows_label} }
                if !summary {
                    button {
                        class: "pimble-sheet__delete-table",
                        title: "Add a row that sums up this table's columns",
                        onclick: {
                            let id = id_sum.clone();
                            move || add_summary_row(store, pane, &id)
                        },
                        "+ Summary Row"
                    }
                }
                if !summary {
                    button {
                        class: "pimble-sheet__delete-table",
                        title: if rotated { "Draw a record per row again" } else { "Draw the fields down the left and each record as a column" },
                        onclick: {
                            let id = id_rotate.clone();
                            move || rotate_table(store, pane, &id, !rotated)
                        },
                        {if rotated { "Unrotate" } else { "Rotate Table" }}
                    }
                }
                if grouped {
                    button {
                        class: "pimble-sheet__delete-table",
                        title: "Collapse every group to its subtotals, or open them all",
                        onclick: {
                            let id = id_collapse.clone();
                            move || toggle_all_groups(store, pane, &id)
                        },
                        "Collapse All"
                    }
                }
                if holds_text(pane) && is_own_table(pane, &id_convert) {
                    button {
                        class: "pimble-sheet__delete-table",
                        title: "Move this table to a sheet node of its own, under this note",
                        onclick: {
                            let id = id_convert.clone();
                            move || in_text::convert_to_sheet_node(store, pane, &id)
                        },
                        "Move"
                    }
                }
                button {
                    class: "pimble-sheet__delete-table",
                    title: "Delete this table",
                    onclick: move || delete_table(store, pane, &id_delete),
                    "×"
                }
            }
            {error}
            if ui.column_form.get().is_some_and(|f| f.table == form_tid) {
                {column_form(__scope, store, pane)}
            }
            if ui.summary_form.get().is_some_and(|f| f.table == summary_tid) {
                {summary_form(__scope, store, pane)}
            }
            div {
                // A link cell's choices hang below the grid: the scroller,
                // which would clip them, lets them show while they are open.
                class: {
                    let tid = scroll_tid.clone();
                    move || {
                        let open = ui.editing.get().is_some_and(|k| k.table == tid) && !ui.choices.get().is_empty();
                        if open { "pimble-sheet__scroll pimble-sheet__scroll--open" } else { "pimble-sheet__scroll" }
                    }
                },
                div {
                    class: "pimble-sheet__grid",
                    onmouseleave: move || ui.hover_row.set_if_changed(None),
                    // The slot a column will land in is empty while it slides
                    // there: the grid takes a drop let go over it.
                    ondragover: move || moving::dragged(store, pane),
                    ondrop: move || moving::dropped(store, pane),
                    style: {move || if rotated_layout {
                        rot_style.clone()
                    } else {
                        grid_columns(&grid_cols, ui.resizing.get().filter(|(t, ..)| *t == grid_tid).map(|(_, k, w)| (k, w)))
                    }},
                    {grid_children}
                }
            }
            {empty}
        }
    }
}

/// The grid's columns: each column's stored width, or as wide as its
/// contents (at least 90px) when it has none; `resizing` is a column being
/// dragged and its width so far. Then the rows' tail.
fn grid_columns(columns: &[ColView], resizing: Option<(String, f32)>) -> String {
    let tracks: Vec<String> = columns
        .iter()
        .map(|c| match (&resizing, c.width) {
            (Some((key, w)), _) if *key == c.key => format!("{w:.0}px"),
            (_, Some(w)) => format!("{w:.0}px"),
            _ => "minmax(90px, max-content)".to_string(),
        })
        .collect();
    let tracks = if tracks.is_empty() { "minmax(90px, max-content)".to_string() } else { tracks.join(" ") };
    // First the gutter that holds each row's grip.
    format!("grid-template-columns: 18px {tracks} 30px;")
}

/// How near a header's right edge (in pixels) a press grabs it.
const GRIP: f32 = 7.0;

/// A press on a column's header: on its right edge, a drag that sets the
/// column's width (shown as it moves, written to the document when it ends;
/// docs/SHEETS_CONTRACT.md "Layout"). The press's element is the header's
/// cell on both backends, so its bounds are the column's width now.
fn start_resize(store: AppStore, pane: PaneId, table: &str, col: &ColView) {
    // A press on the column's move grip lands here too: its drag measures
    // from this header (`moving::pressed`).
    moving::pressed();
    let ctx = rinch::core::get_click_context();
    if ctx.element_width <= 0.0 || ctx.mouse_x < ctx.element_x + ctx.element_width - GRIP {
        return;
    }
    let ui = store.pane(pane).sheet_ui;
    let (from, start) = (ctx.mouse_x, ctx.element_width);
    let width = move |px: f32| (start + px - from).max(doc::MIN_COLUMN_WIDTH as f32).min(doc::MAX_COLUMN_WIDTH as f32);
    let (t_move, k_move) = (table.to_string(), col.key.clone());
    let (t_end, c_end) = (table.to_string(), col.clone());
    ui.resizing.set(Some((table.to_string(), col.key.clone(), start)));
    rinch::core::Drag::absolute()
        .on_move(move |px, _| ui.resizing.set(Some((t_move.clone(), k_move.clone(), width(px)))))
        .on_end(move |px, _| {
            // The last move is where it was let go; a press without a move
            // leaves the width as it was.
            let w = untracked(|| ui.resizing.get()).map(|(.., w)| w).unwrap_or_else(|| width(px));
            ui.resizing.set(None);
            if (w - start).abs() >= 1.0 {
                set_column_width(store, pane, &t_end, &c_end, Some(w as f64));
            }
        })
        .on_cancel(move |_, _| ui.resizing.set(None))
        .start();
}

/// How part of row `row` of `table` shows while a row follows the pointer
/// (`moving::RowDrag`): the dragged one lifted in the ghost's color, the rows
/// it passed a row over.
fn ghost_style(drag: &Option<moving::RowDrag>, table: &str, row: Option<&str>) -> String {
    let Some((dy, lifted)) = drag.as_ref().zip(row).and_then(|(d, row)| d.offset(table, row)) else { return String::new() };
    if lifted {
        format!("position: relative; z-index: 3; transform: translateY({dy:.1}px); background: var(--pimble-sheet-ghost);")
    } else {
        format!("position: relative; transform: translateY({dy:.1}px);")
    }
}

/// A row's grip: shown while the pointer is over the row and nothing is
/// being dragged, dragged to move the row (`moving.rs`).
fn row_grip(__scope: &mut RenderScope, store: AppStore, pane: PaneId, table: &str, row: &str, extra_class: &'static str) -> NodeHandle {
    let ui = store.pane(pane).sheet_ui;
    let what = moving::Moving::Row { table: table.to_string(), row: row.to_string() };
    let start_what = what;
    let mine = (table.to_string(), row.to_string());
    // No grip shows during a drag: what moves is the row itself.
    let class = move |mine: &(String, String)| {
        let on = ui.moving.get().is_none() && !ui.row_dragging.get() && ui.hover_row.get().as_ref() == Some(mine);
        match (on, extra_class.is_empty()) {
            (true, true) => "pimble-sheet__move pimble-sheet__move--on",
            (true, false) => "pimble-sheet__move pimble-sheet__move--on pimble-sheet__move--inline",
            (false, true) => "pimble-sheet__move",
            (false, false) => "pimble-sheet__move pimble-sheet__move--inline",
        }
    };
    if extra_class.is_empty() {
        // A plain table's row follows the pointer (`moving::press_row`).
        let (table, row) = (table.to_string(), row.to_string());
        rsx! {
            div {
                class: {move || class(&mine)},
                title: "Drag to move this row",
                onmousedown: move || moving::press_row(store, pane, &table, &row),
                IconGlyph { name: TablerIcon::GripVertical.name().to_string() }
            }
        }
    } else {
        // A rotated table's record is a column on screen: it is dropped on
        // the record it should take the place of.
        rsx! {
            div {
                class: {move || class(&mine)},
                title: "Drag to move this record",
                draggable: "true",
                ondragstart: move || moving::start(store, pane, start_what.clone()),
                ondragmove: move || moving::dragged(store, pane),
                ondragend: move || moving::end(store, pane),
                IconGlyph { name: TablerIcon::GripVertical.name().to_string() }
            }
        }
    }
}

/// A column's grip, in its header: shown while the pointer is over the
/// header and nothing is being dragged.
fn col_grip(__scope: &mut RenderScope, store: AppStore, pane: PaneId, table: &str, col: &str, rotated: bool) -> NodeHandle {
    let ui = store.pane(pane).sheet_ui;
    let what = moving::Moving::Col { table: table.to_string(), col: col.to_string() };
    let start_what = what;
    let mine = (table.to_string(), col.to_string());
    let (on, off) = if rotated {
        ("pimble-sheet__move pimble-sheet__move--field pimble-sheet__move--on", "pimble-sheet__move pimble-sheet__move--field")
    } else {
        ("pimble-sheet__move pimble-sheet__move--col pimble-sheet__move--on", "pimble-sheet__move pimble-sheet__move--col")
    };
    rsx! {
        div {
            class: {move || if ui.moving.get().is_none() && !ui.row_dragging.get() && ui.hover_col.get().as_ref() == Some(&mine) { on } else { off }},
            title: if rotated { "Drag to move this field" } else { "Drag to move this column" },
            draggable: "true",
            ondragstart: move || moving::start(store, pane, start_what.clone()),
            ondragmove: move || moving::dragged(store, pane),
            ondragend: move || moving::end(store, pane),
            IconGlyph { name: if rotated { TablerIcon::GripVertical.name().to_string() } else { TablerIcon::GripHorizontal.name().to_string() } }
        }
    }
}

/// The empty first cell of a row with no grip (the header, summary rows, the
/// new-row line).
fn gutter(__scope: &mut RenderScope) -> NodeHandle {
    rsx! { div { class: "pimble-sheet__gutter" } }
}

fn cell_node(__scope: &mut RenderScope, store: AppStore, pane: PaneId, key: CellKey, col: &ColView, cell: Option<CellView>) -> NodeHandle {
    let ui = store.pane(pane).sheet_ui;
    let cell = cell.unwrap_or(CellView { text: String::new(), error: None, chip: None });
    let placeholder = key.row.is_none();
    let mut class = String::from("pimble-sheet__cell");
    if col.numeric {
        class.push_str(" pimble-sheet__num");
    }
    if col.computed {
        class.push_str(" pimble-sheet__cell--computed");
    }
    if cell.error.is_some() {
        class.push_str(" pimble-sheet__cell--error");
    }
    if placeholder {
        class.push_str(" pimble-sheet__cell--placeholder");
    }
    // The text stays inside its cell, cut short with an ellipsis; the
    // tooltip has all of it.
    let shown = match cell.chip {
        Some(_) if !cell.text.is_empty() => rsx! { div { class: "pimble-sheet__text", span { class: "pimble-sheet__chip", {cell.text.clone()} } } },
        _ => rsx! { div { class: "pimble-sheet__text", {cell.text.clone()} } },
    };
    let tip = cell.error.clone().unwrap_or_else(|| if placeholder { String::new() } else { cell.text.clone() });
    let press_key = key.clone();
    let editing_key = key.clone();
    let editing_key2 = key.clone();
    let (over_key, mark_key) = (key.clone(), key.clone());
    let hover = key.row.clone().map(|row| (key.table.clone(), row));
    rsx! {
        div {
            class: class,
            title: tip,
            style: move || {
                let mark = moving::style_of(&ui.slide.get(), &mark_key);
                format!("{mark}{}", ghost_style(&ui.row_drag.get(), &mark_key.table, mark_key.row.as_deref()))
            },
            onmouseenter: move || ui.hover_row.set_if_changed(hover.clone()),
            ondragover: move || moving::over(store, pane, &over_key),
            ondrop: move || moving::dropped(store, pane),
            onclick: move || cell_pressed(store, pane, press_key.clone()),
            // The edit box lies over the cell and the text stays under it,
            // hidden, so opening a cell never resizes its row or columns.
            if ui.editing.get().as_ref() == Some(&editing_key) {
                div { class: "pimble-sheet__under", {shown.clone()} }
                {edit_input(__scope, store, pane)}
            } else {
                {shown.clone()}
            }
            if ui.editing.get().as_ref() == Some(&editing_key2) && !ui.choices.get().is_empty() {
                div {
                    class: "pimble-sheet__choices",
                    for (i, label) in ui.choices.get().into_iter().enumerate() {
                        div {
                            key: format!("{i}:{label}"),
                            class: {move || if ui.choice.get() == Some(i) { "pimble-sheet__choice pimble-sheet__choice--on" } else { "pimble-sheet__choice" }},
                            onclick: {
                                let label = label.clone();
                                move || choose(store, pane, label.clone())
                            },
                            {label.clone()}
                        }
                    }
                }
            }
        }
    }
}

fn edit_input(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let ui = store.pane(pane).sheet_ui;
    let node = rsx! {
        input {
            r#type: "text",
            class: "pimble-sheet__edit",
            value: move || ui.edit_text.get(),
            oninput: move |v: String| typed(store, pane, v),
        }
    };
    EDIT_INPUTS.with(|t| t.borrow_mut()[pane.index()] = Some((node.doc_key(), node.node_id().0)));
    node
}

fn column_form(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let ui = store.pane(pane).sheet_ui;
    let Some(form) = untracked(|| ui.column_form.get()) else { return rsx! {} };
    let set = move |f: fn(&mut ColumnForm, String)| {
        move |v: String| {
            ui.column_form.update(|form| {
                if let Some(form) = form {
                    f(form, v);
                }
            })
        }
    };
    let kinds: Vec<SelectOption> = [
        ("text", "Text"),
        ("number", "Number"),
        ("money", "Money"),
        ("date", "Date"),
        ("hours", "Hours"),
        ("row_link", "Link to a row of a table"),
        ("node_link", "Link to a node"),
        ("formula", "Formula"),
    ]
    .iter()
    .map(|(v, l)| SelectOption::new(*v, *l))
    .collect();
    let formats: Vec<SelectOption> = [("auto", "Automatic"), ("number", "Number"), ("money", "Money"), ("date", "Date"), ("hours", "Hours"), ("percent", "Percent")]
        .iter()
        .map(|(v, l)| SelectOption::new(*v, *l))
        .collect();
    let tables: Vec<SelectOption> = plain_tables(pane).into_iter().map(|(id, name)| SelectOption::new(id, name)).collect();
    let result = form.result.is_some();
    let existing = form.col.is_some();
    // An existing column of a plain table can group its rows.
    let group_choice = form.col.clone().and_then(|col| {
        let view = untracked(|| ui.tables.get()).into_iter().find(|t| t.id == form.table)?;
        (!view.summary).then(|| (form.table.clone(), col.clone(), view.group_by.as_deref() == Some(col.as_str())))
    });
    let field = move |f: fn(&ColumnForm) -> String| move || ui.column_form.get().map(|form| f(&form)).unwrap_or_default();
    let shown = move |kind: &'static str| move || ui.column_form.get().is_some_and(|f| f.kind == kind && f.result.is_none());
    let is_link = shown("row_link");
    let is_node_link = shown("node_link");
    let is_formula = shown("formula");
    let is_formula2 = shown("formula");
    rsx! {
        div {
            class: "pimble-sheet__form",
            TextInput {
                label: "Name",
                size: "xs",
                value_fn: field(|f| f.name.clone()),
                oninput: set(|f, v| f.name = v),
                onsubmit: move || apply_column(store, pane),
            }
            if !result {
                Select {
                    label: "Type",
                    size: "xs",
                    value_fn: field(|f| f.kind.clone()),
                    onchange: set(|f, v| f.kind = v),
                    data: kinds.clone(),
                }
            }
            if is_link() {
                Select {
                    label: "Table",
                    size: "xs",
                    value_fn: field(|f| f.target.clone()),
                    onchange: set(|f, v| f.target = v),
                    data: tables.clone(),
                }
            }
            if is_node_link() {
                TextInput {
                    label: "Nodes under",
                    size: "xs",
                    placeholder: "anywhere",
                    value_fn: field(|f| f.under.clone()),
                    oninput: set(|f, v| f.under = v),
                    onsubmit: move || apply_column(store, pane),
                }
            }
            if is_formula() {
                div {
                    class: "pimble-sheet__form-formula",
                    TextInput {
                        label: "Formula",
                        size: "xs",
                        value_fn: field(|f| f.formula.clone()),
                        oninput: set(|f, v| f.formula = v),
                        onsubmit: move || apply_column(store, pane),
                    }
                }
            }
            if is_formula2() {
                Select {
                    label: "Show as",
                    size: "xs",
                    value_fn: field(|f| f.format.clone()),
                    onchange: set(|f, v| f.format = v),
                    data: formats.clone(),
                }
            }
            if let Some(error) = ui.column_form.get().and_then(|f| f.error) {
                div { class: "pimble-sheet__form-error", {error} }
            }
            if result {
                div { class: "pimble-sheet__form-hint", "This column comes from the summary's formula, so only its name changes here." }
            }
            div {
                class: "pimble-sheet__form-buttons",
                if existing {
                    Button { variant: "subtle", color: "red", size: "xs", onclick: move || delete_column(store, pane), "Delete column" }
                }
                if let Some((table, col, grouped)) = group_choice.clone() {
                    Button {
                        variant: "default",
                        size: "xs",
                        onclick: move || group_by(store, pane, &table, if grouped { None } else { Some(&col) }),
                        {if grouped { "Ungroup" } else { "Group by This Column" }}
                    }
                }
                Button { variant: "default", size: "xs", onclick: move || close_column(store, pane), "Cancel" }
                Button { variant: "filled", size: "xs", onclick: move || apply_column(store, pane), {if existing || result { "Done" } else { "Add column" }} }
            }
        }
    }
}

/// Whether rows of `group` in `table` are collapsed in this pane, tracked.
fn collapsed_fn(store: AppStore, pane: PaneId, table: &str, group: Option<String>) -> rinch::core::reactive::Memo<bool> {
    let ui = store.pane(pane).sheet_ui;
    let table = table.to_string();
    rinch::core::reactive::Memo::new(move || group.as_ref().is_some_and(|g| ui.collapsed.with(|c| c.contains(&(table.clone(), g.clone())))))
}

/// A summary row (contract section 4b): its name in the first cell, a
/// press on a cell opening its settings about that column.
fn summary_row_node(__scope: &mut RenderScope, store: AppStore, pane: PaneId, table: &str, columns: &[ColView], s: &SummaryRowView, subtotal: bool) -> NodeHandle {
    let cells: Vec<NodeHandle> = columns
        .iter()
        .zip(&s.cells)
        .enumerate()
        .map(|(i, (c, cell))| summary_cell(__scope, store, pane, table, s, c, cell, i == 0, false))
        .collect();
    let class = if subtotal { "pimble-sheet__row pimble-sheet__summary-row pimble-sheet__summary-row--sub" } else { "pimble-sheet__row pimble-sheet__summary-row" };
    let gap = gutter(__scope);
    rsx! { div { class: class, {gap} {cells} div { class: "pimble-sheet__tail" } } }
}

/// A summary row's settings: its name, and what the clicked column shows.
fn summary_form(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let ui = store.pane(pane).sheet_ui;
    let Some(form) = untracked(|| ui.summary_form.get()) else { return rsx! {} };
    let set = move |f: fn(&mut SummaryForm, String)| {
        move |v: String| {
            ui.summary_form.update(|form| {
                if let Some(form) = form {
                    f(form, v);
                }
            })
        }
    };
    let field = move |f: fn(&SummaryForm) -> String| move || ui.summary_form.get().map(|form| f(&form)).unwrap_or_default();
    let choices: Vec<SelectOption> = [("none", "Nothing"), ("sum", "Sum"), ("average", "Average"), ("count", "Count"), ("min", "Min"), ("max", "Max"), ("formula", "Formula")]
        .iter()
        .map(|(v, l)| SelectOption::new(*v, *l))
        .collect();
    let column = form.col.clone().and_then(|c| untracked(|| ui.tables.get()).into_iter().find(|t| t.id == form.table)?.columns.into_iter().find(|x| x.key == c).map(|x| x.name));
    let is_formula = move || ui.summary_form.get().is_some_and(|f| f.choice == "formula");
    rsx! {
        div {
            class: "pimble-sheet__form",
            TextInput {
                label: "Summary row",
                size: "xs",
                value_fn: field(|f| f.name.clone()),
                oninput: set(|f, v| f.name = v),
                onsubmit: move || apply_summary(store, pane),
            }
            if let Some(name) = column.clone() {
                Select {
                    label: {format!("{name} shows")},
                    size: "xs",
                    value_fn: field(|f| f.choice.clone()),
                    onchange: set(|f, v| f.choice = v),
                    data: choices.clone(),
                }
            }
            if is_formula() {
                div {
                    class: "pimble-sheet__form-formula",
                    TextInput {
                        label: "Formula",
                        size: "xs",
                        placeholder: "=SUM([amount])/COUNT([nights])",
                        value_fn: field(|f| f.formula.clone()),
                        oninput: set(|f, v| f.formula = v),
                        onsubmit: move || apply_summary(store, pane),
                    }
                }
            }
            if let Some(error) = ui.summary_form.get().and_then(|f| f.error) {
                div { class: "pimble-sheet__form-error", {error} }
            }
            if is_formula() {
                div { class: "pimble-sheet__form-hint", "[column] is the rows this sums up: the whole table at the bottom, the group in a subtotal." }
            }
            div {
                class: "pimble-sheet__form-buttons",
                Button { variant: "subtle", color: "red", size: "xs", onclick: move || delete_summary_row(store, pane), "Remove summary row" }
                Button { variant: "default", size: "xs", onclick: move || move_summary_row(store, pane, true), "Move up" }
                Button { variant: "default", size: "xs", onclick: move || move_summary_row(store, pane, false), "Move down" }
                Button { variant: "default", size: "xs", onclick: move || close_summary(store, pane), "Cancel" }
                Button { variant: "filled", size: "xs", onclick: move || apply_summary(store, pane), "Done" }
            }
        }
    }
}

/// One summary cell: a press opens the summary row's settings about its
/// column; `named` puts the row's name beside its value (its first cell, or
/// in a rotated table its header row).
#[allow(clippy::too_many_arguments)]
fn summary_cell(__scope: &mut RenderScope, store: AppStore, pane: PaneId, table: &str, s: &SummaryRowView, c: &ColView, cell: &CellView, named: bool, sub: bool) -> NodeHandle {
    let mut class = String::from("pimble-sheet__cell pimble-sheet__summary-cell");
    if c.numeric {
        class.push_str(" pimble-sheet__num");
    }
    if cell.error.is_some() {
        class.push_str(" pimble-sheet__cell--error");
    }
    if named {
        class.push_str(" pimble-sheet__summary-cell--named");
    }
    if sub {
        class.push_str(" pimble-sheet__summary-cell--sub");
    }
    let (t, r, k) = (table.to_string(), s.id.clone(), c.key.clone());
    let name = named.then(|| rsx! { span { class: "pimble-sheet__summary-name", {s.name.clone()} } });
    let tip = cell.error.clone().unwrap_or_else(|| {
        if cell.text.is_empty() { format!("{} · click to change what it shows", s.name) } else { format!("{}: {} · click to change what it shows", s.name, cell.text) }
    });
    // A column being dragged takes its summary cells along.
    let ui = store.pane(pane).sheet_ui;
    let mark_key = CellKey { table: table.to_string(), row: None, col: c.key.clone() };
    let over_key = mark_key.clone();
    rsx! {
        div {
            class: class,
            title: tip,
            style: move || {
                moving::style_of(&ui.slide.get(), &mark_key)
            },
            ondragover: move || moving::over(store, pane, &over_key),
            ondrop: move || moving::dropped(store, pane),
            onclick: move || open_summary(store, pane, &t, &r, Some(&k)),
            {name}
            div { class: "pimble-sheet__text", {cell.text.clone()} }
        }
    }
}

/// What one column of a rotated table is (contract section 4c).
#[derive(Clone)]
enum RotCol {
    /// A record, by index into the table's rows, and its group when grouped.
    Record(usize, Option<String>),
    /// A group's subtotal: the group's index and the summary row's.
    Subtotal(usize, usize),
    /// Holds a collapsed group's place when it has no subtotals to show.
    Stub(String),
    Summary(usize),
    New,
}

/// A rotated table's grid: its fields down the left, each record a column,
/// then the subtotals of its group, the summary columns and the new record.
fn rotated_grid(__scope: &mut RenderScope, store: AppStore, pane: PaneId, t: &TableView) -> (String, Vec<NodeHandle>) {
    let tid = t.id.clone();
    let mut cols: Vec<RotCol> = Vec::new();
    if t.groups.is_empty() {
        cols.extend((0..t.rows.len()).map(|i| RotCol::Record(i, None)));
    } else {
        for (k, g) in t.groups.iter().enumerate() {
            cols.extend(g.rows.iter().map(|&i| RotCol::Record(i, Some(g.key.clone()))));
            cols.extend((0..g.subtotals.len()).map(|j| RotCol::Subtotal(k, j)));
            if g.subtotals.is_empty() {
                cols.push(RotCol::Stub(g.key.clone()));
            }
        }
    }
    cols.extend((0..t.summary_rows.len()).map(RotCol::Summary));
    cols.push(RotCol::New);

    // A record's cells, and a stub's, are hidden while their group is
    // collapsed (a stub only shows then).
    let hidden = |c: &RotCol| -> Option<(rinch::core::reactive::Memo<bool>, bool)> {
        match c {
            RotCol::Record(_, Some(g)) => Some((collapsed_fn(store, pane, &tid, Some(g.clone())), false)),
            RotCol::Stub(g) => Some((collapsed_fn(store, pane, &tid, Some(g.clone())), true)),
            _ => None,
        }
    };
    let hides: Vec<_> = cols.iter().map(|c| hidden(c)).collect();
    let mut out: Vec<NodeHandle> = Vec::new();
    let mut row = 1;

    // The groups' band: a header over each group's columns.
    if !t.groups.is_empty() {
        out.push(pinned(rsx! { div { class: "pimble-sheet__rot-corner" } }, row));
        for g in &t.groups {
            let open = collapsed_fn(store, pane, &tid, Some(g.key.clone()));
            let (records, subs) = (g.rows.len(), g.subtotals.len());
            let (gt, gk) = (tid.clone(), g.key.clone());
            let count = format!("{} {}", records, if records == 1 { "record" } else { "records" });
            out.push(rsx! {
                div {
                    class: "pimble-sheet__group pimble-sheet__group--band",
                    style: move || {
                        // Collapsed: its subtotals, or the stub that holds its place.
                        let span = if open.get() { subs.max(1) } else { records + subs };
                        format!("grid-column: span {span}; grid-row: 1;")
                    },
                    onclick: move || toggle_group(store, pane, &gt, &gk),
                    span { class: "pimble-sheet__group-chevron", {move || if open.get() { "▸" } else { "▾" }} }
                    span { class: "pimble-sheet__group-label", {g.label.clone()} }
                    span { class: "pimble-sheet__group-count", {count} }
                }
            });
        }
        for c in &cols {
            if matches!(c, RotCol::Summary(_) | RotCol::New) {
                out.push(pinned(rsx! { div { class: "pimble-sheet__rot-corner" } }, row));
            }
        }
        row += 1;
    }

    // A row per field; the first is the header row (each record's label).
    let first_typed = t.columns.iter().position(|c| !c.computed);
    for (fi, c) in t.columns.iter().enumerate() {
        let head = fi == 0;
        let open = {
            let (tid, key) = (tid.clone(), c.key.clone());
            move || open_column(store, pane, &tid, Some(&key))
        };
        let mark = (!c.mark.is_empty()).then(|| rsx! { span { class: "pimble-sheet__mark", {format!("\u{00a0}{}", c.mark)} } });
        let class = format!("pimble-sheet__head pimble-sheet__rot-name{}", if c.computed { " pimble-sheet__head--computed" } else { "" });
        let tip = format!("{} · click to change", c.tip);
        let grip = col_grip(__scope, store, pane, &tid, &c.key, true);
        let ui = store.pane(pane).sheet_ui;
        let hover = (tid.clone(), c.key.clone());
        let key = CellKey { table: tid.clone(), row: None, col: c.key.clone() };
        let (over_key, mark_key) = (key.clone(), key);
        out.push(pinned(
            rsx! {
                div {
                    class: "pimble-sheet__head-wrap pimble-sheet__rot-name",
                    onmousedown: moving::pressed,
                    style: move || moving::style_of(&ui.slide.get(), &mark_key),
                    onmouseenter: move || ui.hover_col.set_if_changed(Some(hover.clone())),
                    onmouseleave: move || ui.hover_col.set_if_changed(None),
                    ondragover: move || moving::over(store, pane, &over_key),
                    ondrop: move || moving::dropped(store, pane),
                    div { class: class, title: tip, onclick: open, {c.name.clone()} {mark} }
                    {grip}
                }
            },
            row,
        ));
        for (col, h) in cols.iter().zip(&hides) {
            let node = match col {
                RotCol::Record(i, _) => {
                    let r = &t.rows[*i];
                    let key = CellKey { table: tid.clone(), row: Some(r.id.clone()), col: c.key.clone() };
                    rot_head(cell_node(__scope, store, pane, key, c, r.cells.get(fi).cloned()), head)
                }
                RotCol::Subtotal(k, j) => {
                    let s = &t.groups[*k].subtotals[*j];
                    summary_cell(__scope, store, pane, &tid, s, c, &s.cells[fi], head, true)
                }
                RotCol::Summary(j) => {
                    let s = &t.summary_rows[*j];
                    summary_cell(__scope, store, pane, &tid, s, c, &s.cells[fi], head, false)
                }
                RotCol::Stub(_) => rsx! { div { class: "pimble-sheet__cell pimble-sheet__rot-stub" } },
                RotCol::New => {
                    let key = CellKey { table: tid.clone(), row: None, col: c.key.clone() };
                    let cell = (Some(fi) == first_typed).then(|| CellView { text: "Add a record…".to_string(), error: None, chip: None });
                    rot_head(cell_node(__scope, store, pane, key, c, cell), head)
                }
            };
            out.push(rot_slot(__scope, pinned(node, row), h));
        }
        row += 1;
    }

    // Under the fields: add a field, and delete each record.
    let add = tid.clone();
    out.push(pinned(
        rsx! {
            div {
                class: "pimble-sheet__add-col pimble-sheet__rot-add",
                title: "Add a field",
                onclick: move || open_column(store, pane, &add, None),
                "+"
            }
        },
        row,
    ));
    for (col, h) in cols.iter().zip(&hides) {
        let node = match col {
            RotCol::Record(i, _) => {
                let (tid, rid) = (tid.clone(), t.rows[*i].id.clone());
                let grip = row_grip(__scope, store, pane, &tid, &rid, "inline");
                let ui = store.pane(pane).sheet_ui;
                let hover = (tid.clone(), rid.clone());
                rsx! {
                    div {
                        class: "pimble-sheet__tail pimble-sheet__rot-tail",
                        onmousedown: moving::pressed,
                        onmouseenter: move || ui.hover_row.set_if_changed(Some(hover.clone())),
                        {grip}
                        button { class: "pimble-sheet__delete-row", title: "Delete this record", onclick: move || delete_row(store, pane, &tid, &rid), "×" }
                    }
                }
            }
            _ => rsx! { div { class: "pimble-sheet__tail" } },
        };
        out.push(rot_slot(__scope, pinned(node, row), h));
    }
    let style = "grid-template-columns: max-content; grid-auto-columns: minmax(90px, max-content);".to_string();
    (style, out)
}

/// Put a rotated table's cell in grid row `row` (1-based): its columns are
/// implicit, so a collapsed group's hidden cells leave no gap in any row.
fn pinned(node: NodeHandle, row: usize) -> NodeHandle {
    node.set_style("grid-row", &row.to_string());
    node
}

/// A rotated table's cell that a collapsed group hides (or, a stub, shows).
fn rot_slot(__scope: &mut RenderScope, node: NodeHandle, h: &Option<(rinch::core::reactive::Memo<bool>, bool)>) -> NodeHandle {
    match *h {
        Some((m, shows_when_collapsed)) => rsx! {
            div {
                class: "pimble-sheet__rot-slot",
                style: move || if m.get() != shows_when_collapsed { "display: none;" } else { "display: contents;" },
                {node}
            }
        },
        None => node,
    }
}

/// A cell of a rotated table's header row (the first field) shows as a header.
fn rot_head(node: NodeHandle, head: bool) -> NodeHandle {
    if head {
        node.add_class("pimble-sheet__rot-head");
    }
    node
}
