//! The sheet view's DOM (docs/SHEETS_CONTRACT.md section 3): each table with
//! its title row, one-line column headers, the cells, the new-row line, the
//! column settings and the formula bar. Rendered once per pane slot; a table
//! is keyed by its id and version, so only a table that changed is built
//! again.

use super::*;

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
    let error = t.error.clone().map(|e| rsx! { div { class: "pimble-sheet__error", {e} } });

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
            rsx! {
                div { class: class, title: tip, onclick: open, {c.name.clone()} {mark} }
            }
        })
        .collect();

    let body: Vec<NodeHandle> = t
        .rows
        .iter()
        .map(|r| {
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
            rsx! { div { class: "pimble-sheet__row", {cells} div { class: "pimble-sheet__tail", {tail} } } }
        })
        .collect();

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
        rsx! { div { class: "pimble-sheet__row pimble-sheet__new-row", {cells} div { class: "pimble-sheet__tail" } } }
    });
    let empty = (summary && t.rows.is_empty() && t.error.is_none())
        .then(|| rsx! { div { class: "pimble-sheet__empty", "Nothing matches yet." } });

    let grid_style = format!("grid-template-columns: repeat({}, minmax(90px, max-content)) 30px;", columns.len().max(1));
    let form_tid = tid.clone();
    let bar_tid = tid.clone();
    let note_tid = tid.clone();
    rsx! {
        section {
            class: "pimble-sheet__table",
            div {
                class: "pimble-sheet__title",
                input {
                    class: "pimble-sheet__name",
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
                    span { class: "pimble-sheet__note", {note} }
                }
                span { class: "pimble-sheet__count", {rows_label} }
                if holds_text(pane) {
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
            div {
                class: "pimble-sheet__scroll",
                div {
                    class: "pimble-sheet__grid",
                    style: grid_style,
                    div {
                        class: "pimble-sheet__row",
                        {heads}
                        div {
                            class: "pimble-sheet__add-col",
                            title: "Add a column",
                            onclick: move || open_column(store, pane, &id_add, None),
                            "+"
                        }
                    }
                    {body}
                    {new_row}
                }
            }
            {empty}
        }
    }
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
    let shown = match cell.chip {
        Some(_) if !cell.text.is_empty() => rsx! { span { class: "pimble-sheet__chip", {cell.text.clone()} } },
        _ => rsx! { span { {cell.text.clone()} } },
    };
    let tip = cell.error.clone().unwrap_or_default();
    let press_key = key.clone();
    let editing_key = key.clone();
    let editing_key2 = key.clone();
    rsx! {
        div {
            class: class,
            title: tip,
            onclick: move || cell_pressed(store, pane, press_key.clone()),
            if ui.editing.get().as_ref() == Some(&editing_key) {
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
    let field = move |f: fn(&ColumnForm) -> String| move || ui.column_form.get().map(|form| f(&form)).unwrap_or_default();
    let shown = move |kind: &'static str| move || ui.column_form.get().is_some_and(|f| f.kind == kind && f.result.is_none());
    let is_link = shown("row_link");
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
                Button { variant: "default", size: "xs", onclick: move || close_column(store, pane), "Cancel" }
                Button { variant: "filled", size: "xs", onclick: move || apply_column(store, pane), {if existing || result { "Done" } else { "Add column" }} }
            }
        }
    }
}
