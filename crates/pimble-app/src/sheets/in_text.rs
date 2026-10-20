//! Sheets in text (docs/SHEETS_CONTRACT.md sections 1 and 9, wave 2): a
//! note's tables, each drawn where its `embed` atom (`data-sheet=<table id>`)
//! sits in the text, by a rinch node view registered on every pane's editor.
//!
//! The tables are the note's own `data["sheet:<id>"]` keys, held by the
//! pane's sheet session over the note's document (`text_opened`); the atom
//! only says where one is shown. The view draws the same table component a
//! sheet node does, from the same per-pane view state. A table no atom names
//! any more (its atom deleted, or never put back after an undo) is listed
//! under the text, "Tables no longer in the text", with "Put Back": nothing
//! is deleted behind the person's back.

use rinch_editor_core::Attrs;

use super::*;
use crate::rinch_editor::{EditorKey, NodeViewCtx, NodeViewRegistration, NodeViewSpec};

/// The embed attribute naming its table (a `data-*` attribute, which rinch
/// keeps and merges per attribute; rinch knows nothing of what it means).
pub(crate) const SHEET_ATTR: &str = "data-sheet";

/// Insert > "Sheet" with no note to put it in.
const NO_NOTE: &str = "Open a note to put a table in it.";

thread_local! {
    /// Each pane's editor's node view, registered once for the editor's life.
    static REGISTERED: RefCell<[Option<NodeViewRegistration>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
}

/// Draw sheets in pane `pane`'s editor (once per editor; the editors live as
/// long as the window).
pub(crate) fn register(store: AppStore, pane: PaneId) {
    if REGISTERED.with(|r| r.borrow()[pane.index()].is_some()) {
        return;
    }
    let spec = NodeViewSpec::new("embed", move |__scope: &mut RenderScope, ctx: NodeViewCtx| render_embed(__scope, store, pane, ctx))
        .matches(|attrs: &Attrs| attrs.get_str(SHEET_ATTR).is_some_and(|id| !id.is_empty()))
        .on_key(move |key: &EditorKey<'_>| key_in_view(store, pane, key));
    let registration = crate::editor::editor(pane).register_node_view(spec);
    REGISTERED.with(|r| r.borrow_mut()[pane.index()] = Some(registration));
}

/// The tables of pane `pane` named `id` (one, or none before it arrives).
fn tables_named(ui: SheetUi, id: &str) -> Vec<TableView> {
    ui.tables.get().into_iter().filter(|t| t.id == id).collect()
}

fn render_embed(__scope: &mut RenderScope, store: AppStore, pane: PaneId, ctx: NodeViewCtx) -> NodeHandle {
    let ui = store.pane(pane).sheet_ui;
    let attrs = ctx.attrs;
    let id = move || attrs.get().get_str(SHEET_ATTR).unwrap_or_default().to_string();
    rsx! {
        div {
            class: "pimble-sheet pimble-sheet--in-text",
            onmousedown: move || crate::pane_view::press_in_view(),
            for table in tables_named(ui, &id()) {
                div {
                    key: format!("{}:{}", table.id, table.version),
                    {super::ui::table_node(__scope, store, pane, table.clone())}
                }
            }
            if tables_named(ui, &id()).is_empty() {
                div { class: "pimble-sheet__empty", "This table has not arrived yet." }
            }
        }
    }
}

/// A key while the keyboard is in a table (an input of it): typing's own
/// keys, then the sheet's own undo unless a cell is being typed into (the
/// input undoes its text). An Escape nobody used gives the keyboard back to
/// the text.
fn key_in_view(store: AppStore, pane: PaneId, key: &EditorKey<'_>) -> bool {
    if !key.primary && !key.alt && key_in_table(store, pane, key.key) {
        return true;
    }
    if !key.primary || key.alt || TYPING.with(Cell::get).is_some() {
        return false;
    }
    let redo = match key.key {
        "z" | "Z" => key.shift,
        "y" | "Y" => true,
        _ => return false,
    };
    undo(store, pane, redo);
    true
}

/// The ids of the tables pane `pane`'s text shows, in document order.
fn embedded_ids(pane: PaneId) -> Vec<String> {
    let doc = crate::editor::editor(pane).doc();
    let mut ids = Vec::new();
    doc.nodes_between(0, doc.content_size(), &mut |node, _, _| {
        if node.type_name() == "embed" {
            if let Some(id) = node.attrs().get_str(SHEET_ATTR).filter(|id| !id.is_empty()) {
                ids.push(id.to_string());
            }
        }
        !node.is_textblock()
    });
    ids
}

/// Pane `pane`'s text changed (typed, a peer's edit, an undo): which tables
/// it shows follows, for the "no longer in the text" list.
pub(crate) fn text_changed(pane: PaneId) {
    let Some(store) = APP.with(Cell::get) else { return };
    if !holds_text(pane) {
        return;
    }
    let ids = embedded_ids(pane);
    let ui = store.pane(pane).sheet_ui;
    if untracked(|| ui.embedded.get()) != ids {
        ui.embedded.set(ids);
    }
}

/// Insert > "Sheet": a new table in the focused pane's note, at the caret.
pub fn insert_sheet(store: AppStore) {
    let pane = store.focused();
    let Some(active) = untracked(|| store.pane(pane).active_edit.get()) else {
        crate::events::show_notice(store, NO_NOTE.to_string());
        return;
    };
    if !store.node_access(active.store_id, active.node_id).allows_write() {
        crate::events::show_notice(store, pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string());
        return;
    }
    if !holds_text(pane) {
        return;
    }
    let Some(snap) = snapshot(pane) else { return };
    let name = next_table_name(&snap, "Table");
    let Some(id) = local_edit(store, pane, |t| new_table(t, &name)) else { return };
    if !put_in(pane, &id) {
        tracing::warn!("table {id} was made but the editor refused its embed; it is listed under the text");
    }
    text_changed(pane);
}

/// Table `table`'s atom at pane `pane`'s caret, with a paragraph after it
/// when it ends the text: Escape out of a table puts the caret there, and
/// with nothing after it the table itself would be selected and the next
/// key typed over it.
fn put_in(pane: PaneId, table: &str) -> bool {
    let handle = crate::editor::editor(pane);
    handle.focus();
    if !handle.insert_embed(Attrs::new().with(SHEET_ATTR, table)) {
        return false;
    }
    handle.update(|state| {
        let doc = &state.doc;
        let last = doc.child_count().checked_sub(1).map(|i| doc.child(i))?;
        if last.type_name() != "embed" {
            return None;
        }
        let paragraph = state.schema().create_node("paragraph", Attrs::new(), rinch_editor_core::Fragment::empty()).ok()?;
        let end = doc.content_size();
        let mut tr = state.tr();
        tr.replace_with(end, end, rinch_editor_core::Fragment::from_node(paragraph)).ok()?;
        tr.set_selection(rinch_editor_core::Selection::cursor(rinch_editor_core::Pos(end + 1)));
        Some(tr)
    });
    true
}

/// The table's "×" in text: its atom leaves the text (one undo step of the
/// text), and the table is listed under it until put back or deleted.
pub(crate) fn remove_embeds(pane: PaneId, table: &str) {
    let handle = crate::editor::editor(pane);
    handle.update(|state| {
        let mut at = Vec::new();
        state.doc.nodes_between(0, state.doc.content_size(), &mut |node, pos, _| {
            if node.type_name() == "embed" && node.attrs().get_str(SHEET_ATTR) == Some(table) {
                at.push((pos, node.node_size()));
            }
            !node.is_textblock()
        });
        if at.is_empty() {
            return None;
        }
        let mut tr = state.tr();
        for (pos, size) in at.into_iter().rev() {
            tr.delete(pos, pos + size).ok()?;
        }
        Some(tr)
    });
    text_changed(pane);
}

/// "Put Back": the table's atom at the caret.
fn put_back(pane: PaneId, table: &str) {
    put_in(pane, table);
    text_changed(pane);
}

/// The tables of pane `pane`'s note that its text does not show.
fn lost_tables(ui: SheetUi) -> Vec<(String, String)> {
    let shown = ui.embedded.get();
    ui.tables.get().into_iter().filter(|t| !shown.contains(&t.id)).map(|t| (t.id, t.name)).collect()
}

/// "Tables no longer in the text", under pane `pane`'s text.
pub(crate) fn render_lost(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let state = store.pane(pane);
    let ui = state.sheet_ui;
    rsx! {
        div {
            class: "pimble-sheet-lost",
            style: {move || if state.show_editor.get() && !lost_tables(ui).is_empty() { "" } else { "display: none;" }},
            span { class: "pimble-sheet-lost__title", "Tables no longer in the text:" }
            for (id, name) in lost_tables(ui) {
                span {
                    key: id.clone(),
                    class: "pimble-sheet-lost__item",
                    span { {name.clone()} }
                    button {
                        class: "pimble-sheet__adder",
                        title: "Put this table back at the caret",
                        onclick: {
                            let id = id.clone();
                            move || put_back(pane, &id)
                        },
                        "Put Back"
                    }
                    button {
                        class: "pimble-sheet__adder",
                        title: "Delete this table and everything in it",
                        onclick: {
                            let id = id.clone();
                            move || discard_table(store, pane, &id)
                        },
                        "Delete"
                    }
                }
            }
        }
    }
}

// ── Convert to Sheet Node ────────────────────────────────────────────────

/// "Convert to Sheet Node" refused: a row link ties the table to another of
/// the note's tables, and a row link points within its own node only.
fn tied_sentence(name: &str) -> String {
    format!("A link column ties {name} to another table of this note, and a link reaches rows of its own note only, so it cannot move to a node of its own.")
}

/// A table on its way to a sheet node of its own.
struct ConvertJob {
    pane: PaneId,
    host: (StoreId, NodeId),
    table: String,
    title: String,
}

thread_local! {
    static CONVERTS: RefCell<HashMap<u64, ConvertJob>> = RefCell::new(HashMap::new());
    /// Request ids of this module (the boards in text count from 1 << 48).
    static NEXT_REQUEST: Cell<u64> = const { Cell::new(2 << 48) };
}

/// "Convert to Sheet Node" (contract section 1): a sheet node under the note
/// holding the table, ids kept; when it is made, the table's place in the
/// text becomes a link to it and the table leaves the note.
pub(crate) fn convert_to_sheet_node(store: AppStore, pane: PaneId, table: &str) {
    let Some((store_id, node_id)) = with_session(pane, |s| (s.store_id, s.node_id)) else { return };
    if !store.node_access(store_id, node_id).allows_write() {
        say(store, pane, pimble_core::StoreAccess::READ_ONLY_REFUSAL);
        return;
    }
    let Some(snap) = snapshot(pane) else { return };
    let Some(t) = snap.table(table).cloned() else { return };
    let tied = !t.row_links().is_empty() || snap.tables.iter().any(|o| o.id != t.id && o.row_links().contains(&t.id));
    if tied {
        say(store, pane, tied_sentence(&t.name));
        return;
    }
    // Its formulas name this note's tables with the note (references across
    // notes); the ones left here name it in its new node once that is made.
    let from = node_ref(store_id, node_id);
    let moved = t.map_formulas(|f| formula::rehome::moved_out(f, &t.id, &from, None));
    let copied = Sheets::standalone().edit(|x| x.copy_table(&moved));
    let content = match (copied.result, copied.update) {
        (Ok(()), Some(content)) => content,
        (Err(e), _) => {
            say(store, pane, e.to_string());
            return;
        }
        (Ok(()), None) => return,
    };
    let request = NEXT_REQUEST.with(|n| n.replace(n.get() + 1));
    CONVERTS.with(|c| c.borrow_mut().insert(request, ConvertJob { pane, host: (store_id, node_id), table: t.id.clone(), title: t.name.clone() }));
    store.send(BackendCommand::CreateNode {
        store_id,
        parent_id: Some(node_id),
        title: t.name.clone(),
        init: Some(NodeInit {
            node_type: node_types::SHEET.to_string(),
            content,
            custom: vec![
                (pimble_core::custom_keys::ICON.to_string(), serde_json::json!(SHEET_ICON)),
                (pimble_core::custom_keys::EXPLICIT_TITLE.to_string(), serde_json::json!(true)),
            ],
            request: Some(request),
        }),
    });
}

/// A node made for a request of this module: the converted table's place
/// in the text becomes a link to it, and the table leaves the note.
pub(crate) fn node_created(store: AppStore, request: u64, store_id: StoreId, node_id: NodeId) {
    let Some(job) = CONVERTS.with(|c| c.borrow_mut().remove(&request)) else { return };
    let holds = holds_text(job.pane) && with_session(job.pane, |s| (s.store_id, s.node_id)) == Some(job.host);
    if !holds {
        crate::events::show_notice(store, "The note was closed before the sheet node was made; the table is in both.".to_string());
        return;
    }
    let href = pimble_core::PimbleUrl::node(store_id, node_id).to_string();
    let (table, title) = (job.table.clone(), job.title.clone());
    crate::editor::editor(job.pane).update(move |state| {
        let mut at = Vec::new();
        state.doc.nodes_between(0, state.doc.content_size(), &mut |node, pos, _| {
            if node.type_name() == "embed" && node.attrs().get_str(SHEET_ATTR) == Some(table.as_str()) {
                at.push((pos, node.node_size()));
            }
            !node.is_textblock()
        });
        if at.is_empty() {
            return None;
        }
        let schema = state.schema();
        let link = rinch_editor_core::Mark::new(schema.mark_type("link")?.clone(), Attrs::new().with("href", href.as_str()));
        let text = schema.text_with_marks(&title, vec![link]).ok()?;
        let paragraph = schema.create_node("paragraph", Attrs::new(), rinch_editor_core::Fragment::from_node(text)).ok()?;
        let mut tr = state.tr();
        for (pos, size) in at.into_iter().rev() {
            tr.replace_with(pos, pos + size, rinch_editor_core::Fragment::from_node(paragraph.clone())).ok()?;
        }
        Some(tr)
    });
    let to = node_ref(store_id, node_id);
    let rest: Vec<pimble_sheet::TableSnap> = snapshot(job.pane)
        .map(|s| s.tables.into_iter().filter(|t| t.id != job.table).collect())
        .unwrap_or_default();
    local_edit(store, job.pane, |t| {
        t.delete_table(&job.table)?;
        for table in &rest {
            let after = table.map_formulas(|f| formula::rehome::left_behind(f, &job.table, &to));
            write_formulas(t, table, &after)?;
        }
        Ok(())
    });
    text_changed(job.pane);
}

/// Write the formulas and source of `after` where they differ from `before`
/// (the same table, rewritten).
fn write_formulas(t: &mut doc::SheetsTxn<'_, '_>, before: &pimble_sheet::TableSnap, after: &pimble_sheet::TableSnap) -> doc::Result<()> {
    if after.source != before.source {
        t.set_source(&after.id, after.source.as_deref())?;
    }
    for (old, new) in before.columns.iter().zip(&after.columns) {
        if old.ty != new.ty {
            t.set_column_type(&after.id, &new.id, new.ty.clone())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{BackendEvent, BackendHandle};
    use crossbeam_channel::bounded;

    const P: [PaneId; MAX_PANES] = PaneId::ALL;

    fn store_with_commands() -> (AppStore, crossbeam_channel::Receiver<BackendCommand>) {
        let store = AppStore::new();
        let (cmd_tx, cmd_rx) = bounded::<BackendCommand>(512);
        let (_event_tx, event_rx) = bounded::<BackendEvent>(1);
        store.backend.set(Some(BackendHandle { cmd_tx, event_rx }));
        crate::links::set_app_store(store);
        (store, cmd_rx)
    }

    fn broadcasts(sent: &[BackendCommand], node: NodeId) -> usize {
        sent.iter().filter(|c| matches!(c, BackendCommand::BroadcastChanges { node_id, .. } if *node_id == node)).count()
    }

    fn table_ids(pane: PaneId) -> Vec<String> {
        snapshot(pane).map(|s| s.tables.iter().map(|t| t.id.clone()).collect()).unwrap_or_default()
    }

    /// Insert > "Sheet" (contract section 1): a table in the note's `data`
    /// and its atom at the caret, both sent; the text shows it.
    #[test]
    fn insert_sheet_makes_a_table_and_its_atom() {
        let (store, commands) = store_with_commands();
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        crate::editor::start_editing(store, P[0], store_id, node_id, &[]);
        assert!(holds_text(P[0]));
        let _ = commands.try_iter().count();

        insert_sheet(store);
        let ids = table_ids(P[0]);
        assert_eq!(ids.len(), 1);
        assert_eq!(embedded_ids(P[0]), ids, "the atom names the table");
        assert_eq!(untracked(|| store.pane(P[0]).sheet_ui.embedded.get()), ids);
        assert!(untracked(|| lost_tables(store.pane(P[0]).sheet_ui)).is_empty());
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert!(broadcasts(&sent, node_id) >= 2, "the table and the atom both travel");
        crate::editor::stop_editing(store, P[0]);
        assert!(!holds_text(P[0]));
    }

    /// Two panes on one note: a table edit in one reaches the other's tables
    /// and its editor's document; typing in the other's editor reaches the
    /// first's copy of the document.
    #[test]
    fn two_panes_on_one_note_share_its_tables() {
        let (store, _commands) = store_with_commands();
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        crate::editor::start_editing(store, P[0], store_id, node_id, &[]);
        crate::editor::start_editing(store, P[1], store_id, node_id, &[]);
        insert_sheet(store);
        let ids = table_ids(P[0]);
        assert_eq!(table_ids(P[1]), ids, "the other pane's tables");
        assert_eq!(embedded_ids(P[1]), ids, "the other pane's text shows the atom");

        // The other editor's document has the table too: its snapshot carries it.
        let snap = crate::editor::editor(P[1]).collab_snapshot().expect("a session");
        let doc = NodeDoc::load(&snap).unwrap();
        assert_eq!(Sheets::of_node(&doc).read().tables.len(), 1);

        // Typing in P[1]: P[0]'s copy takes it.
        let before = with_session(P[0], |s| s.doc.state_vector()).unwrap();
        crate::editor::editor(P[1]).insert_text("hello");
        let after = with_session(P[0], |s| s.doc.state_vector()).unwrap();
        assert_ne!(before, after, "the typing reached the first pane's document");
    }

    /// The table's "×" in text takes its atom out; the table is listed under
    /// the text, "Put Back" brings it back, "Delete" deletes it.
    #[test]
    fn a_table_out_of_the_text_is_listed_and_put_back() {
        let (store, _commands) = store_with_commands();
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        crate::editor::start_editing(store, P[0], store_id, node_id, &[]);
        insert_sheet(store);
        let id = table_ids(P[0]).remove(0);
        let ui = store.pane(P[0]).sheet_ui;

        delete_table(store, P[0], &id);
        assert!(embedded_ids(P[0]).is_empty());
        assert_eq!(table_ids(P[0]), vec![id.clone()], "the table stays");
        assert_eq!(untracked(|| lost_tables(ui)).into_iter().map(|(i, _)| i).collect::<Vec<_>>(), vec![id.clone()]);

        put_back(P[0], &id);
        assert_eq!(embedded_ids(P[0]), vec![id.clone()]);
        assert!(untracked(|| lost_tables(ui)).is_empty());

        delete_table(store, P[0], &id);
        discard_table(store, P[0], &id);
        assert!(table_ids(P[0]).is_empty());
        assert!(untracked(|| lost_tables(ui)).is_empty());
    }

    /// "Convert to Sheet Node": a sheet node under the note with the table
    /// (ids kept); once made, the table's place in the text is a link to it
    /// and the note no longer holds the table. A table another reads stays.
    #[test]
    fn convert_moves_a_table_to_a_sheet_node_of_its_own() {
        let (store, commands) = store_with_commands();
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        crate::editor::start_editing(store, P[0], store_id, node_id, &[]);
        insert_sheet(store);
        let id = table_ids(P[0]).remove(0);
        let _ = commands.try_iter().count();

        convert_to_sheet_node(store, P[0], &id);
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        let (request, content) = sent
            .iter()
            .find_map(|c| match c {
                BackendCommand::CreateNode { parent_id: Some(p), init: Some(init), .. } if *p == node_id => {
                    Some((init.request.expect("a request"), init.content.clone()))
                }
                _ => None,
            })
            .expect("a sheet node under the note");
        let made = NodeDoc::load(&content).unwrap();
        assert!(Sheets::of_node(&made).table(&id).is_some(), "the table, under its own id");

        let new_node = NodeId::new();
        node_created(store, request, store_id, new_node);
        assert!(embedded_ids(P[0]).is_empty());
        assert!(table_ids(P[0]).is_empty(), "the note no longer holds it");
        let href = pimble_core::PimbleUrl::node(store_id, new_node).to_string();
        let mut linked = false;
        let doc = crate::editor::editor(P[0]).doc();
        doc.nodes_between(0, doc.content_size(), &mut |node, _, _| {
            linked |= node.marks().iter().any(|m| m.attrs.get_str("href") == Some(href.as_str()));
            true
        });
        assert!(linked, "its place in the text links to the sheet node");

        // Two tables, one linking to the other: neither moves.
        insert_sheet(store);
        insert_sheet(store);
        let ids = table_ids(P[0]);
        local_edit(store, P[0], |t| t.add_column(&ids[0], "other", ColumnType::RowLink { table: ids[1].clone() }, Placement::Last).map(|_| ()));
        let _ = commands.try_iter().count();
        convert_to_sheet_node(store, P[0], &ids[1]);
        assert!(commands.try_iter().all(|c| !matches!(c, BackendCommand::CreateNode { .. })));
        assert!(untracked(|| store.pane(P[0]).sheet_ui.message.get()).is_some_and(|m| m.contains("cannot move")));

        // A formula reading the moved table: it names the new node after.
        let third = local_edit(store, P[0], |t| new_table(t, "Totals")).unwrap();
        local_edit(store, P[0], |t| {
            t.add_column(&third, "sum", ColumnType::Formula { formula: format!("=SUM({{t:{}}}[{{c:x}}])", ids[0]), format: pimble_sheet::Format::Auto }, Placement::Last)
                .map(|_| ())
        });
        let link = snapshot(P[0]).unwrap().table(&ids[0]).unwrap().columns.last().unwrap().id.clone();
        local_edit(store, P[0], |t| t.delete_column(&ids[0], &link));
        let _ = commands.try_iter().count();
        convert_to_sheet_node(store, P[0], &ids[0]);
        let request = commands
            .try_iter()
            .find_map(|c| match c {
                BackendCommand::CreateNode { init: Some(init), .. } => init.request,
                _ => None,
            })
            .expect("it moves: only a row link ties a table");
        let new_node = NodeId::new();
        node_created(store, request, store_id, new_node);
        let totals = snapshot(P[0]).unwrap().table(&third).cloned().unwrap();
        let formula = totals.columns.iter().find_map(|c| match &c.ty {
            ColumnType::Formula { formula, .. } => Some(formula.clone()),
            _ => None,
        });
        let token = formula::note_token(&node_ref(store_id, new_node));
        assert_eq!(formula, Some(format!("=SUM({token}!{{t:{}}}[{{c:x}}])", ids[0])));
    }

    /// A peer's table and its atom, arriving together (the MCP's
    /// create_table): the text shows it, and it is not listed as lost.
    #[test]
    fn a_peers_table_in_text_is_shown_not_lost() {
        let (store, _commands) = store_with_commands();
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        crate::editor::start_editing(store, P[0], store_id, node_id, &[]);
        crate::editor::editor(P[0]).insert_text("Trip");
        let mut peer = NodeDoc::load(&crate::editor::editor(P[0]).collab_snapshot().unwrap()).unwrap();
        let before = peer.state_vector();
        let made = Sheets::of_node(&peer).edit(|t| new_table(t, "Budget"));
        let id = made.result.unwrap();
        peer.append_markdown(&format!("<div data-pm-type=\"embed\" data-sheet=\"{id}\"></div>\n")).unwrap();
        let bytes = peer.diff_since(&before).unwrap();

        crate::editor::apply_remote(store, store_id, node_id, &bytes);
        crate::sheets::apply_remote(store, store_id, node_id, &bytes);
        assert_eq!(embedded_ids(P[0]), vec![id.clone()]);
        assert_eq!(untracked(|| store.pane(P[0]).sheet_ui.embedded.get()), vec![id]);
        assert!(untracked(|| lost_tables(store.pane(P[0]).sheet_ui)).is_empty());
    }

    /// A link-to-a-node column under a parent: typing a name that is not
    /// there offers "Create ... under ...", which makes the node there and
    /// links the cell to it when it arrives.
    #[test]
    fn a_node_link_cell_creates_the_node_it_names() {
        let (store, commands) = store_with_commands();
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        let projects = NodeId::new();
        let mut parent = pimble_core::Node::new("document");
        parent.id = projects;
        parent.metadata.title = "Projects".into();
        store.upsert_node(store_id, parent);
        crate::editor::start_editing(store, P[0], store_id, node_id, &[]);
        insert_sheet(store);
        let table = table_ids(P[0]).remove(0);
        let col = local_edit(store, P[0], |t| {
            t.add_column(&table, "project", ColumnType::NodeLink { under: Some(node_ref(store_id, projects)) }, Placement::Last)
        })
        .unwrap();
        let _ = commands.try_iter().count();

        let key = CellKey { table: table.clone(), row: None, col: col.clone() };
        begin_typing(store, P[0], key);
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert!(sent.iter().any(|c| matches!(c, BackendCommand::GetChildren { node_id, .. } if *node_id == projects)), "its choices are fetched");
        typed(store, P[0], "Deck repair".to_string());
        let ui = store.pane(P[0]).sheet_ui;
        assert_eq!(untracked(|| ui.choices.get()), vec!["Create \"Deck repair\" under Projects".to_string()]);
        typing_key(store, P[0], "Enter");
        let request = commands
            .try_iter()
            .find_map(|c| match c {
                BackendCommand::CreateNode { parent_id: Some(p), title, init: Some(init), .. } if p == projects && title == "Deck repair" => init.request,
                _ => None,
            })
            .expect("the node is made under Projects");
        let made = NodeId::new();
        crate::sheets::node_created(store, request, store_id, made);
        let snap = snapshot(P[0]).unwrap();
        let row = &snap.table(&table).unwrap().rows[0];
        assert_eq!(row.cells.get(&col), Some(&Stored::Node(node_ref(store_id, made))));
        end_typing();
    }

    /// A note with a table, in the node cache as the server sent it.
    fn cached_note(store: AppStore, store_id: StoreId, title: &str, amounts: &[f64]) -> (NodeId, String, String, NodeDoc) {
        let doc = NodeDoc::new();
        let made = Sheets::of_node(&doc).edit(|t| {
            let table = t.create_table("Costs", Placement::Last)?;
            let col = t.add_column(&table, "amount", ColumnType::Number, Placement::Last)?;
            for a in amounts {
                t.add_row(&table, Placement::Last, &[(col.clone(), Stored::Number(*a))])?;
            }
            Ok((table, col))
        });
        let (table, col) = made.result.unwrap();
        let mut node = pimble_core::Node::new("document");
        node.metadata.title = title.into();
        node.content = doc.save();
        let id = node.id;
        store.upsert_node(store_id, node);
        (id, table, col, doc)
    }

    fn cell_text(store: AppStore, pane: PaneId, table: &str, col_name: &str) -> String {
        let tables = untracked(|| store.pane(pane).sheet_ui.tables.get());
        let t = tables.iter().find(|t| t.id == table).unwrap();
        let i = t.columns.iter().position(|c| c.name == col_name).unwrap();
        t.rows[0].cells[i].text.clone()
    }

    /// Another note a formula reads (contract sections 6 and 7): resolved,
    /// subscribed while read and let go after, a peer's edit of it shown,
    /// and a deleted one `#REF!` with its sentence.
    #[test]
    fn a_note_a_formula_reads_is_resolved_held_and_followed() {
        let (store, commands) = store_with_commands();
        let store_id = StoreId::new();
        let (b, b_table, b_col, b_doc) = cached_note(store, store_id, "Budget", &[40.0]);
        let node_id = NodeId::new();
        crate::editor::start_editing(store, P[0], store_id, node_id, &[]);
        insert_sheet(store);
        let table = table_ids(P[0]).remove(0);
        let token = formula::note_token(&node_ref(store_id, b));
        let total = format!("=SUM({token}!{{t:{b_table}}}[{{c:{b_col}}}])");
        let col = local_edit(store, P[0], |t| t.add_column(&table, "total", ColumnType::Formula { formula: total.clone(), format: pimble_sheet::Format::Auto }, Placement::Last)).unwrap();
        local_edit(store, P[0], |t| t.add_row(&table, Placement::Last, &[]).map(|_| ()));
        assert_eq!(cell_text(store, P[0], &table, "total"), "40", "the cache answers at once");
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        let url = pimble_core::PimbleUrl::node(store_id, b);
        assert!(sent.iter().any(|c| matches!(c, BackendCommand::ResolveLink { url: u, .. } if *u == url)), "its resolution is asked");

        assert!(reads::link_resolved(store, &url, &pimble_core::LinkResolution::Live { store_id, node_id: b }));
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert!(sent.iter().any(|c| matches!(c, BackendCommand::SubscribeNodeChanges { node_id: n, .. } if *n == b)), "held while read");
        assert!(store.is_editing(store_id, b));

        // A peer adds a row to Budget: the total follows.
        let before = b_doc.state_vector();
        Sheets::of_node(&b_doc).edit(|t| t.add_row(&b_table, Placement::Last, &[(b_col.clone(), Stored::Number(2.0))]).map(|_| ())).result.unwrap();
        let update = b_doc.diff_since(&before).unwrap();
        crate::sheets::apply_remote(store, store_id, b, &update);
        assert_eq!(cell_text(store, P[0], &table, "total"), "42");

        // No longer read: let go.
        local_edit(store, P[0], |t| t.delete_column(&table, &col));
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert!(sent.iter().any(|c| matches!(c, BackendCommand::UnsubscribeNodeChanges { node_id: n, .. } if *n == b)));
        assert!(!store.is_editing(store_id, b));

        // A deleted note: #REF!, and why.
        let (gone, g_table, g_col, _) = cached_note(store, store_id, "Old plan", &[1.0]);
        let token = formula::note_token(&node_ref(store_id, gone));
        let f = format!("=SUM({token}!{{t:{g_table}}}[{{c:{g_col}}}])");
        local_edit(store, P[0], |t| t.add_column(&table, "old", ColumnType::Formula { formula: f.clone(), format: pimble_sheet::Format::Auto }, Placement::Last).map(|_| ()));
        let url = pimble_core::PimbleUrl::node(store_id, gone);
        assert!(reads::link_resolved(store, &url, &pimble_core::LinkResolution::Deleted { store_id, node_id: gone }));
        let tables = untracked(|| store.pane(P[0]).sheet_ui.tables.get());
        let t = tables.iter().find(|t| t.id == table).unwrap();
        let i = t.columns.iter().position(|c| c.name == "old").unwrap();
        let cell = &t.rows[0].cells[i];
        assert_eq!(cell.text, "#REF!");
        assert!(cell.error.as_deref().is_some_and(|e| e.contains("deleted")), "{:?}", cell.error);
    }
}
