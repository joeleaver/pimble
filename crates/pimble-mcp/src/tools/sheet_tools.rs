//! The sheet tools (docs/SHEETS_CONTRACT.md section 10, wave 1). Each fetches
//! the node, fills a book with what its formulas read, runs one operation of
//! `crate::sheets::ops` on the node's document, and sends the update that
//! made through `applyEdit` (`Pimble::edit`): a cell edit writes that cell,
//! never the table.

use std::collections::{BTreeMap, HashMap, HashSet};

use pimble_client::PimbleClient;
use pimble_core::{custom_keys, node_types, Node, Store};
use pimble_crdt::NodeDoc;
use pimble_sheet::doc::Sheets;
use pimble_sheet::{ColumnType, NodeRef};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use super::{rpc, Pimble};
use crate::nodes::{self, label, Addr};
use crate::sheets::book::{reads_table, McpBook};
use crate::sheets::load::{load_book, node_ref, sheets_of};
use crate::sheets::ops::{self, ColumnSpec, Filter, RowUpdate, RowValues, Work};
use crate::sheets::{guide, ops::find_table};

/// The icon a new sheet node gets, as the app's "New Sheet" gives it.
const SHEET_ICON: &str = "table";

/// How many rows get_node shows of each table.
const NODE_ROWS: usize = 50;

// ── Parameters ───────────────────────────────────────────────────────────

#[derive(Deserialize, JsonSchema)]
pub struct ListTablesParams {
    /// A node: its tables. With `reads`, the node whose table is read.
    #[serde(default)]
    pub node: Option<String>,
    /// A store (name or id): every table in it.
    #[serde(default)]
    pub store: Option<String>,
    /// The name of a table of `node`: list instead the tables whose formulas
    /// read it, in its store.
    #[serde(default)]
    pub reads: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct GetTableParams {
    /// The node holding the table: a pimble: link, a node id, or a path.
    pub node: String,
    /// The table's name (or id).
    pub table: String,
    /// Only rows whose cells equal these values, by column name (the value as
    /// shown, case-insensitive, or the same number).
    #[serde(default)]
    pub filter: Option<BTreeMap<String, String>>,
    /// At most this many rows (default 100, at most 1000).
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct EvaluateParams {
    /// The node whose tables the formula reads ("this note").
    pub node: String,
    /// The formula, as typed in Excel, starting with =.
    pub formula: String,
    /// Run it as a formula of this table, so [@column] and bare [column] work.
    #[serde(default)]
    pub table: Option<String>,
    /// With `table`: the row [@column] reads, by its id or first-column value.
    #[serde(default)]
    pub row: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateTableParams {
    /// The node the table goes in: a sheet node, or a text node (the table
    /// goes into its text, see after_text).
    pub node: String,
    /// The table's name: letters, digits and underscores (spaces become
    /// underscores), unique in the note.
    pub name: String,
    /// Its columns, in order.
    #[serde(default)]
    pub columns: Vec<ColumnSpec>,
    /// Its first rows: values by column name, as a person types them.
    #[serde(default)]
    pub rows: Vec<RowValues>,
    /// Keep values that do not parse (or links that match no row) as typed,
    /// instead of refusing them.
    #[serde(default)]
    pub keep_as_typed: bool,
    /// Place it after this table; at the end when left out.
    #[serde(default)]
    pub after: Option<String>,
    /// In a text node: put the table after the top-level block this quote
    /// names (a few words of it, as replace_text quotes); at the end of the
    /// text when left out.
    #[serde(default)]
    pub after_text: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateSheetParams {
    /// Where the new sheet node goes: the parent node (or a store's name).
    pub parent: String,
    /// The sheet node's title.
    pub title: String,
    /// Its first table's name (default Table1).
    #[serde(default)]
    pub table: Option<String>,
    /// The first table's columns, in order (default: name as text, amount as
    /// number). Formulas may read only this table.
    #[serde(default)]
    pub columns: Vec<ColumnSpec>,
    /// Its first rows: values by column name, as a person types them.
    #[serde(default)]
    pub rows: Vec<RowValues>,
    /// Keep values that do not parse as typed, instead of refusing them.
    #[serde(default)]
    pub keep_as_typed: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct AddRowsParams {
    /// The node holding the table.
    pub node: String,
    /// The table's name.
    pub table: String,
    /// The rows: values by column name, as a person types them ("$12.50",
    /// "yesterday", "1h 30m", a linked row's first-column value like "Civic").
    pub rows: Vec<RowValues>,
    /// Keep values that do not parse (or links that match no row) as typed,
    /// instead of refusing them.
    #[serde(default)]
    pub keep_as_typed: bool,
    /// Insert after this row (its id or first-column value); at the end when
    /// left out.
    #[serde(default)]
    pub after: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct UpdateRowsParams {
    /// The node holding the table.
    pub node: String,
    /// The table's name.
    pub table: String,
    /// The rows to change and their new values; only the cells named change.
    pub updates: Vec<RowUpdate>,
    /// Keep values that do not parse (or links that match no row) as typed,
    /// instead of refusing them.
    #[serde(default)]
    pub keep_as_typed: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct DeleteRowsParams {
    /// The node holding the table.
    pub node: String,
    /// The table's name.
    pub table: String,
    /// The rows, by id or first-column value.
    pub rows: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct AddColumnParams {
    /// The node holding the table.
    pub node: String,
    /// The table's name.
    pub table: String,
    #[serde(flatten)]
    pub column: ColumnSpec,
}

#[derive(Deserialize, JsonSchema)]
pub struct UpdateColumnParams {
    /// The node holding the table.
    pub node: String,
    /// The table's name.
    pub table: String,
    /// The column's name (or id).
    pub column: String,
    /// A new name.
    #[serde(default)]
    pub new_name: Option<String>,
    /// A new type: text, number, money, date, hours, link or formula. Values
    /// are carried over as typed, as the app does.
    #[serde(rename = "type", default)]
    pub ty: Option<String>,
    /// A link column's table.
    #[serde(default)]
    pub target: Option<String>,
    /// A node_link column's parent (title, path or pimble: link); "" for any
    /// node.
    #[serde(default)]
    pub under: Option<String>,
    /// A new formula, starting with = (makes it a formula column). Read
    /// sheet_formula_guide first.
    #[serde(default)]
    pub formula: Option<String>,
    /// A formula column's format: auto, number, money, date, hours or percent.
    #[serde(default)]
    pub format: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ColumnParams {
    /// The node holding the table.
    pub node: String,
    /// The table's name.
    pub table: String,
    /// The column's name (or id).
    pub column: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetSummaryParams {
    /// The sheet node.
    pub node: String,
    /// The summary's name: an existing summary, or a new table's name.
    pub table: String,
    /// A dynamic-array formula (GROUPBY, FILTER, SORT, UNIQUE, HSTACK, LET),
    /// starting with =. Read sheet_formula_guide first.
    pub formula: String,
    /// Names for the result's columns, by position; "" keeps the formula's.
    #[serde(default)]
    pub column_names: Option<Vec<String>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct TableParams {
    /// The node holding the table.
    pub node: String,
    /// The table's name (or id).
    pub table: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct RenameTableParams {
    /// The node holding the table.
    pub node: String,
    /// The table's name (or id).
    pub table: String,
    /// Its new name: letters, digits and underscores, unique in the note.
    pub new_name: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct MoveTableParams {
    /// The node holding the table.
    pub node: String,
    /// The table's name (or id).
    pub table: String,
    /// The node it moves to: a sheet node, or a text node (into its text).
    /// Left out, the table moves within its own node.
    #[serde(default)]
    pub to: Option<String>,
    /// Place it after this table of the node it ends up in; last when left out.
    #[serde(default)]
    pub after: Option<String>,
    /// When it ends up in a text node: after the top-level block this quote
    /// names; at the end of the text when left out (and, within a text node,
    /// where it was).
    #[serde(default)]
    pub after_text: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetFieldsParams {
    /// The node whose fields change.
    pub node: String,
    /// Field names to values as typed ("120", "Acme", "2026-10-20"); null
    /// removes a field. A name matching an existing field in another case is
    /// that field. Fields not named are left as they are.
    pub fields: BTreeMap<String, Option<String>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct RowsPointingHereParams {
    /// The node the rows link to.
    pub node: String,
}

// ── Plumbing ─────────────────────────────────────────────────────────────

/// Every formula of a column spec, as typed.
/// What the book is loaded for: the specs' formulas, and their `under`s
/// (marked, see `UNDER_MARK`).
fn shown_of(columns: &[ColumnSpec]) -> Vec<String> {
    let mut out: Vec<String> = columns.iter().filter_map(|c| c.formula.clone()).collect();
    out.extend(columns.iter().filter_map(|c| c.under.as_ref()).map(|u| format!("{}{u}", crate::sheets::load::UNDER_MARK)));
    out
}

fn as_strs(shown: &[String]) -> Vec<&str> {
    shown.iter().map(String::as_str).collect()
}

impl Pimble {
    /// Read a node's tables: `op` over its document and a book of what its
    /// formulas (and `shown`, formulas about to be checked) read.
    async fn sheet_read(&self, client: &PimbleClient, addr: Addr, shown: &[&str], op: impl FnOnce(&Work) -> Result<String, String>) -> Result<String, String> {
        let node = client.get_node(addr.store, addr.node).await.map_err(rpc)?;
        let mut book = load_book(client, addr, &node, shown).await;
        let doc = NodeDoc::load(&node.content).map_err(|e| format!("The node's document could not be read: {e}"))?;
        let work = Work::new(Sheets::of_node(&doc), &mut book);
        op(&work)
    }

    /// Edit a node's tables: `op` changes its document, and what it changed
    /// goes through `applyEdit`. A refused `op` sends nothing.
    async fn sheet_edit(
        &self,
        client: &PimbleClient,
        addr: Addr,
        shown: &[&str],
        op: impl FnOnce(&mut Work) -> Result<String, String> + Send,
    ) -> Result<String, String> {
        let node = client.get_node(addr.store, addr.node).await.map_err(rpc)?;
        let mut book = load_book(client, addr, &node, shown).await;
        let mut answer = String::new();
        let answer_ref = &mut answer;
        self.edit(client, addr, move |doc| {
            let mut work = Work::new(Sheets::of_node(doc), &mut book);
            *answer_ref = op(&mut work)?;
            Ok(())
        })
        .await?;
        Ok(answer)
    }

    /// A text node's Markdown with each table its text shows rendered where its
    /// atom is (contract section 10), and the tables no atom shows, rendered
    /// after it under "Tables no longer in the text" (`None` when there are
    /// none).
    pub(super) async fn tables_in_text(&self, client: &PimbleClient, addr: Addr, node: &Node, markdown: &str) -> (String, Option<String>) {
        // Sheet nodes the text embeds, rendered where they are.
        let markdown = self.embedded_sheet_nodes(client, markdown).await;
        let markdown = markdown.as_str();
        if sheets_of(node).tables.is_empty() {
            return (markdown.to_string(), None);
        }
        let mut book = load_book(client, addr, node, &[]).await;
        let Ok(doc) = NodeDoc::load(&node.content) else { return (markdown.to_string(), None) };
        let work = Work::new(Sheets::of_node(&doc), &mut book);
        let render = |id: &str| {
            ops::render_table(&work, id, &Filter { equals: Vec::new(), limit: NODE_ROWS, ..Default::default() })
                .unwrap_or_else(|e| format!("(a table could not be shown: {e})\n"))
        };
        let mut shown = HashSet::new();
        let mut out = String::new();
        for line in markdown.split_inclusive('\n') {
            match embedded_table(line) {
                Some(id) if work.snap().table(&id).is_some() => {
                    out.push_str(&render(&id));
                    shown.insert(id);
                }
                Some(_) => out.push_str("(a table that has not arrived, or was deleted)\n"),
                None => out.push_str(line),
            }
        }
        let lost: Vec<String> = work.snap().tables.iter().filter(|t| !shown.contains(&t.id)).map(|t| t.id.clone()).collect();
        let rest = (!lost.is_empty()).then(|| lost.iter().map(|id| render(id) + "\n").collect::<String>());
        (out, rest)
    }

    /// `markdown` with each embedded sheet node (an embed with `data-node`
    /// and `data-kind="sheet"`) replaced by its tables, under its title.
    async fn embedded_sheet_nodes(&self, client: &PimbleClient, markdown: &str) -> String {
        let mut out = String::new();
        for line in markdown.split_inclusive('\n') {
            let Some(url) = embedded_sheet_node(line) else {
                out.push_str(line);
                continue;
            };
            let at = Addr { store: url.store, node: url.node };
            match client.get_node(at.store, at.node).await {
                Ok(n) => {
                    let mut book = load_book(client, at, &n, &[]).await;
                    let rendered = NodeDoc::load(&n.content)
                        .map(|doc| ops::render_all(&Work::new(Sheets::of_node(&doc), &mut book), NODE_ROWS))
                        .unwrap_or_default();
                    out.push_str(&format!("(the sheet node {} is shown here, {})\n{rendered}", label(&n), at.link()));
                }
                Err(e) => out.push_str(&format!("(a sheet node shown here could not be read: {e})\n")),
            }
        }
        out
    }

    /// The tables of a node as get_node shows them, after its content.
    pub(super) async fn tables_of_node(&self, client: &PimbleClient, addr: Addr, node: &Node) -> Option<String> {
        if sheets_of(node).tables.is_empty() {
            return None;
        }
        let out = self.sheet_read(client, addr, &[], |w| Ok(ops::render_all(w, NODE_ROWS))).await;
        Some(out.unwrap_or_else(|e| format!("(its tables could not be shown: {e})")))
    }
}

/// A store's nodes with their paths, walked through the tree as the person
/// sees it.
async fn walk_store(client: &PimbleClient, store: &Store) -> Vec<(Addr, Node, String)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut queue: Vec<(Addr, String)> = nodes::store_tops(store).into_iter().map(|a| (a, store.name.clone())).collect();
    if let Some((top, _)) = queue.first() {
        if let Ok(root) = client.get_node(top.store, top.node).await {
            out.push((*top, root, store.name.clone()));
        }
    }
    while let Some((at, path)) = queue.pop() {
        if !seen.insert(at) {
            continue;
        }
        let Ok(children) = nodes::children(client, at).await else { continue };
        for (addr, node) in children {
            let child_path = format!("{path}/{}", label(&node));
            queue.push((addr, child_path.clone()));
            out.push((addr, node, child_path));
        }
    }
    out
}

/// The embed atom of table `id` as Markdown (rinch writes and reads it as
/// its element).
fn embed_markdown(id: &str) -> String {
    format!("<div data-pm-type=\"embed\" {SHEET_ATTR}=\"{id}\"></div>\n")
}

/// The sheet node a Markdown line's embed atom shows, if the line is one.
fn embedded_sheet_node(line: &str) -> Option<pimble_core::PimbleUrl> {
    let line = line.trim();
    if !line.starts_with("<div") || !line.contains("data-pm-type=\"embed\"") || !line.contains("data-kind=\"sheet\"") {
        return None;
    }
    let (_, rest) = line.split_once("data-node=\"")?;
    let (url, _) = rest.split_once('"')?;
    pimble_core::PimbleUrl::parse(url)
}

/// The table a Markdown line's embed atom names, if the line is one.
fn embedded_table(line: &str) -> Option<String> {
    let line = line.trim();
    if !line.starts_with("<div") || !line.contains("data-pm-type=\"embed\"") {
        return None;
    }
    let (_, rest) = line.split_once("data-sheet=\"")?;
    let (id, _) = rest.split_once('"')?;
    Some(id.to_string())
}

fn place_of(node: &Node) -> &'static str {
    if node.node_type == node_types::SHEET {
        "in the sheet node"
    } else {
        "in the text node"
    }
}

// ── Tools ────────────────────────────────────────────────────────────────

#[tool_router(router = sheet_router, vis = "pub(super)")]
impl Pimble {
    /// Sheet tables: a node's (name, kind, columns with types, rows), every
    /// table in a store, or (with `reads`) the tables whose formulas read a
    /// given one.
    #[tool(name = "list_tables")]
    async fn list_tables(&self, Parameters(p): Parameters<ListTablesParams>) -> Result<String, String> {
        let client = self.client().await?;
        let stores = client.list_stores().await.map_err(rpc)?;
        if let Some(reads) = &p.reads {
            let name = p.node.as_deref().ok_or("Name the node whose table is read, with `node`.")?;
            let addr = nodes::resolve(&client, name).await?;
            let node = client.get_node(addr.store, addr.node).await.map_err(rpc)?;
            let snap = sheets_of(&node);
            let target = find_table(&snap, reads)?.clone();
            let store = stores.iter().find(|s| s.id == addr.store).ok_or("That node's store is not open.")?;
            let mut out = String::new();
            for (at, n, path) in walk_store(&client, store).await {
                let book = McpBook::new(node_ref(at), sheets_of(&n), McpBook::local_today());
                for t in &book.home_snap().tables {
                    let mut formulas: Vec<&str> = t.source.as_deref().into_iter().collect();
                    formulas.extend(t.columns.iter().filter_map(|c| match &c.ty {
                        ColumnType::Formula { formula, .. } => Some(formula.as_str()),
                        _ => None,
                    }));
                    if formulas.iter().any(|f| reads_table(f, &node_ref(at), &node_ref(addr), &target.id)) {
                        out.push_str(&format!("- {} in {path} ({})\n", t.name, at.link()));
                    }
                }
            }
            if out.is_empty() {
                return Ok(format!("No table's formulas read {}.", target.name));
            }
            return Ok(format!("Tables that read {}:\n{out}", target.name));
        }
        let mut found: Vec<(Addr, Node, String)> = Vec::new();
        match (&p.node, &p.store) {
            (Some(name), _) => {
                let addr = nodes::resolve(&client, name).await?;
                let node = client.get_node(addr.store, addr.node).await.map_err(rpc)?;
                let path = nodes::path_of(&client, &stores, addr).await;
                found.push((addr, node, path));
            }
            (None, Some(store)) => {
                let store = nodes::find_store(&stores, store)?;
                found = walk_store(&client, store).await;
            }
            (None, None) => return Err("Name a node (`node`) or a store (`store`).".into()),
        }
        let mut out = String::new();
        for (addr, node, path) in &found {
            let snap = sheets_of(node);
            if snap.tables.is_empty() {
                continue;
            }
            // The same book get_table computes with: the notes and fields its
            // formulas read, not this node's tables alone.
            let book = load_book(&client, *addr, node, &[]).await;
            out.push_str(&format!("{} {path} ({}):\n", place_of(node), addr.link()));
            for t in &snap.tables {
                out.push_str(&format!("- {}\n", ops::table_summary(&book, &snap, t)));
            }
        }
        if out.is_empty() {
            return Ok(match &p.node {
                Some(_) => "That node has no tables.".into(),
                None => "That store has no tables.".into(),
            });
        }
        Ok(out)
    }

    /// One table: its columns (name, type, link target, formula, format) and
    /// its rows, typed values as typed and computed values as they show, with
    /// each error's code and sentence. Filters by column value; a row limit
    /// for big tables.
    #[tool(name = "get_table")]
    async fn get_table(&self, Parameters(p): Parameters<GetTableParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        let limit = p.limit.unwrap_or(100).clamp(1, 1000);
        let equals: Vec<(String, String)> = p.filter.clone().unwrap_or_default().into_iter().collect();
        self.sheet_read(&client, addr, &[], |w| {
            let id = find_table(w.snap(), &p.table)?.id.clone();
            ops::render_table(w, &id, &Filter { equals, limit, ..Default::default() })
        })
        .await
    }

    /// Run a formula without storing it, in a note's context (and a table's
    /// and one row's): to check a formula before setting it, or to answer a
    /// question from the data ("what did we spend on gas in September?").
    /// Formulas are Excel's; read sheet_formula_guide first.
    #[tool(name = "evaluate")]
    async fn evaluate(&self, Parameters(p): Parameters<EvaluateParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        let formula = p.formula.trim().to_string();
        self.sheet_read(&client, addr, &[&formula], |w| ops::evaluate(w, p.table.as_deref(), p.row.as_deref(), &formula))
            .await
    }

    /// The guide to sheet formulas: Excel Table syntax, the dot that follows
    /// a link, 'Note'! references, the functions Pimble has, what differs
    /// from Excel, and worked examples. Read it before writing a formula.
    #[tool(name = "sheet_formula_guide")]
    async fn sheet_formula_guide(&self) -> Result<String, String> {
        Ok(guide::text())
    }

    /// Create a sheet node (a node made of tables) under `parent`, with its
    /// first table, columns and rows written in the edit that creates it.
    #[tool(name = "create_sheet")]
    async fn create_sheet(&self, Parameters(p): Parameters<CreateSheetParams>) -> Result<String, String> {
        let client = self.client().await?;
        let parent = nodes::resolve(&client, &p.parent).await?;
        let parent_node = client.get_node(parent.store, parent.node).await.map_err(rpc)?;
        let (store, parent_id) = match parent_node.mount_ref() {
            Some(mount) => (mount.source_store, mount.source_node),
            None => (parent.store, parent.node),
        };
        let title = p.title.trim().to_string();
        if title.is_empty() {
            return Err("A title cannot be empty.".into());
        }
        let name = p.table.clone().unwrap_or_else(|| "Table1".into());
        // A stand-in for the node that does not exist yet: a sheet's own
        // tables carry no note token, so its formulas do not depend on it.
        let home = NodeRef { store, node: pimble_core::NodeId::new() };
        let content = ops::new_sheet_content(&name, &p.columns, &p.rows, p.keep_as_typed, home, McpBook::local_today())?;
        let custom = HashMap::from([
            (custom_keys::ICON.to_string(), serde_json::json!(SHEET_ICON)),
            (custom_keys::EXPLICIT_TITLE.to_string(), serde_json::json!(true)),
        ]);
        let node_id = client
            .create_node_with(store, Some(parent_id), node_types::SHEET, title, Some(content), custom)
            .await
            .map_err(rpc)?;
        let addr = Addr { store, node: node_id };
        let stores = client.list_stores().await.map_err(rpc)?;
        Ok(format!("Created the sheet {}; get_table shows its table.", self.describe(&client, &stores, addr).await))
    }

    /// Create a table in a sheet node, with its columns and first rows, in
    /// one edit. Formula columns are checked first (read sheet_formula_guide).
    #[tool(name = "create_table")]
    async fn create_table(&self, Parameters(p): Parameters<CreateTableParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        let node = client.get_node(addr.store, addr.node).await.map_err(rpc)?;
        let shown = shown_of(&p.columns);
        let shown = as_strs(&shown);
        if node.node_type == node_types::SHEET {
            if p.after_text.is_some() {
                return Err("after_text places a table in a text node's text; a sheet node has no text (use after).".into());
            }
            return self
                .sheet_edit(&client, addr, &shown, |w| {
                    let id = ops::create_table(w, &p.name, &p.columns, &p.rows, p.keep_as_typed, p.after.as_deref())?;
                    let out = ops::render_table(w, &id, &Filter { equals: Vec::new(), limit: 5, ..Default::default() })?;
                    Ok(format!("Created the table.\n{out}"))
                })
                .await;
        }
        if node.node_type == node_types::MOUNT || node.node_type == node_types::BOARD {
            return Err(format!("A table goes in a text node or a sheet node; this is a {} node.", node.node_type));
        }
        // A text node: the table, and its embed atom in the text, in one edit
        // (contract section 10).
        let mut book = load_book(&client, addr, &node, &shown).await;
        let mut answer = String::new();
        let answer_ref = &mut answer;
        self.edit(&client, addr, move |doc| {
            let id = {
                let mut work = Work::new(Sheets::of_node(doc), &mut book);
                let id = ops::create_table(&mut work, &p.name, &p.columns, &p.rows, p.keep_as_typed, p.after.as_deref())?;
                *answer_ref = ops::render_table(&work, &id, &Filter { equals: Vec::new(), limit: 5, ..Default::default() })?;
                id
            };
            let atom = embed_markdown(&id);
            match p.after_text.as_deref() {
                Some(quote) => doc.insert_markdown_after(quote, false, &atom),
                None => doc.append_markdown(&atom),
            }
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await?;
        Ok(format!("Created the table in the text.\n{answer}"))
    }

    /// Add rows to a table. Values by column name, as a person types them:
    /// "$12.50", "yesterday", "1h 30m", and a link as the linked row's
    /// first-column value ("Civic"). A value that does not parse, or a link
    /// that matches no row, is refused unless keep_as_typed.
    #[tool(name = "add_rows")]
    async fn add_rows(&self, Parameters(p): Parameters<AddRowsParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        self.sheet_edit(&client, addr, &[], |w| {
            let ids = ops::add_rows(w, &p.table, &p.rows, p.keep_as_typed, p.after.as_deref())?;
            Ok(format!("Added {} row(s) to {} (ids {}).", ids.len(), p.table, ids.join(", ")))
        })
        .await
    }

    /// Change rows' cells, rows named by id or by their first column's value.
    /// Only the cells named are written; values as a person types them.
    #[tool(name = "update_rows")]
    async fn update_rows(&self, Parameters(p): Parameters<UpdateRowsParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        self.sheet_edit(&client, addr, &[], |w| {
            let n = ops::update_rows(w, &p.table, &p.updates, p.keep_as_typed)?;
            Ok(format!("Changed {n} cell(s) in {}.", p.table))
        })
        .await
    }

    /// Delete rows, by id or by their first column's value.
    #[tool(name = "delete_rows")]
    async fn delete_rows(&self, Parameters(p): Parameters<DeleteRowsParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        self.sheet_edit(&client, addr, &[], |w| {
            let n = ops::delete_rows(w, &p.table, &p.rows)?;
            Ok(format!("Deleted {n} row(s) from {}.", p.table))
        })
        .await
    }

    /// Add a column: name, type (text, number, money, date, hours, link with
    /// `target`, or formula), and for a formula column its formula and
    /// format. A formula that does not parse, names something unknown, gives
    /// a list (#SPILL!) or reads itself (#CIRC!) is refused; an accepted one
    /// answers its first values. Read sheet_formula_guide first.
    #[tool(name = "add_column")]
    async fn add_column(&self, Parameters(p): Parameters<AddColumnParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        let shown = shown_of(std::slice::from_ref(&p.column));
        let shown = as_strs(&shown);
        self.sheet_edit(&client, addr, &shown, |w| ops::add_column(w, &p.table, &p.column)).await
    }

    /// Change a column: rename it, change its type (values carry over as
    /// typed, as in the app), its link target, formula or format. Formulas
    /// are checked as add_column checks them. Read sheet_formula_guide first.
    #[tool(name = "update_column")]
    async fn update_column(&self, Parameters(p): Parameters<UpdateColumnParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        let spec = ColumnSpec {
            name: String::new(),
            ty: p.ty.clone(),
            target: p.target.clone(),
            under: p.under.clone(),
            formula: p.formula.clone(),
            format: p.format.clone(),
        };
        let shown = shown_of(std::slice::from_ref(&spec));
        let shown = as_strs(&shown);
        self.sheet_edit(&client, addr, &shown, |w| ops::update_column(w, &p.table, &p.column, p.new_name.as_deref(), &spec))
            .await
    }

    /// Delete a column and its values; formulas that read it show #REF!.
    #[tool(name = "delete_column")]
    async fn delete_column(&self, Parameters(p): Parameters<ColumnParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        self.sheet_edit(&client, addr, &[], |w| ops::delete_column(w, &p.table, &p.column)).await
    }

    /// Set a summary's formula (a table whose rows come from GROUPBY, FILTER,
    /// SORT, UNIQUE, HSTACK or LET), making the summary when no table has
    /// that name, and name its result columns. Checked like add_column; the
    /// answer shows its first rows. Read sheet_formula_guide first.
    #[tool(name = "set_summary")]
    async fn set_summary(&self, Parameters(p): Parameters<SetSummaryParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        let formula = p.formula.trim().to_string();
        self.sheet_edit(&client, addr, &[&formula], |w| ops::set_summary(w, &p.table, &formula, p.column_names.as_deref()))
            .await
    }

    /// Rename a table; formulas that read it show the new name.
    #[tool(name = "rename_table")]
    async fn rename_table(&self, Parameters(p): Parameters<RenameTableParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        self.sheet_edit(&client, addr, &[], |w| ops::rename_table(w, &p.table, &p.new_name)).await
    }

    /// A node's fields: its own named values (a project's rate, a person's
    /// phone), which formulas read as `'Note'!rate` and, through a
    /// link-to-a-node column, `[@project].rate`.
    #[tool(name = "get_fields")]
    async fn get_fields(&self, Parameters(p): Parameters<super::NodeParam>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        let node = client.get_node(addr.store, addr.node).await.map_err(rpc)?;
        if node.metadata.fields.is_empty() {
            return Ok(format!("{} has no fields.", label(&node)));
        }
        let lines: Vec<String> = node.metadata.fields.iter().map(|(n, v)| format!("- {n}: {v}")).collect();
        Ok(format!("Fields of {}:\n{}", label(&node), lines.join("\n")))
    }

    /// Set (or with null, remove) a node's fields; each field is its own
    /// co-edited value, so only the fields named are written.
    #[tool(name = "set_fields")]
    async fn set_fields(&self, Parameters(p): Parameters<SetFieldsParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        if p.fields.keys().any(|n| n.trim().is_empty()) {
            return Err("A field needs a name.".into());
        }
        let now = chrono::Utc::now().to_rfc3339();
        let fields = p.fields.clone();
        self.edit(&client, addr, move |doc| {
            for (name, value) in &fields {
                doc.set_field(name, value.as_deref(), &now).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .await?;
        let node = client.get_node(addr.store, addr.node).await.map_err(rpc)?;
        let lines: Vec<String> = node.metadata.fields.iter().map(|(n, v)| format!("- {n}: {v}")).collect();
        Ok(format!("Fields of {} now:\n{}", label(&node), if lines.is_empty() { "(none)".to_string() } else { lines.join("\n") }))
    }

    /// Every row of every table, in every open store, whose link-to-a-node
    /// column links to this node, grouped by table with totals for its number
    /// columns (contract section 8, "rows that point here"). Computed values
    /// are computed as the app does.
    #[tool(name = "rows_pointing_here")]
    async fn rows_pointing_here(&self, Parameters(p): Parameters<RowsPointingHereParams>) -> Result<String, String> {
        let client = self.client().await?;
        let target = nodes::resolve(&client, &p.node).await?;
        let target_ref = node_ref(target);
        let stores = client.list_stores().await.map_err(rpc)?;
        let mut out = String::new();
        for store in &stores {
            for (at, node, path) in walk_store(&client, store).await {
                let snap = sheets_of(&node);
                let hits: Vec<(String, HashSet<String>)> = snap
                    .tables
                    .iter()
                    .filter_map(|t| {
                        let cols: Vec<&str> = t.columns.iter().filter(|c| matches!(c.ty, ColumnType::NodeLink { .. })).map(|c| c.id.as_str()).collect();
                        let rows: HashSet<String> = t
                            .rows
                            .iter()
                            .filter(|r| cols.iter().any(|c| r.cells.get(*c) == Some(&pimble_sheet::Stored::Node(target_ref))))
                            .map(|r| r.id.clone())
                            .collect();
                        (!rows.is_empty()).then(|| (t.id.clone(), rows))
                    })
                    .collect();
                if hits.is_empty() {
                    continue;
                }
                let mut book = load_book(&client, at, &node, &[]).await;
                let Ok(doc) = NodeDoc::load(&node.content) else { continue };
                let work = Work::new(Sheets::of_node(&doc), &mut book);
                for (table, rows) in hits {
                    let n = rows.len();
                    let filter = Filter { limit: NODE_ROWS, rows: Some(rows), totals: n > 1, ..Default::default() };
                    match ops::render_table(&work, &table, &filter) {
                        Ok(text) => out.push_str(&format!("{} {path} ({}):\n{text}\n", place_of(&node), at.link())),
                        Err(e) => out.push_str(&format!("(a table in {path} could not be shown: {e})\n")),
                    }
                }
            }
        }
        if out.is_empty() {
            return Ok("No table links to that node.".into());
        }
        Ok(out)
    }

    /// Delete a table with its columns and rows (in a text node, its place
    /// in the text goes too).
    #[tool(name = "delete_table")]
    async fn delete_table(&self, Parameters(p): Parameters<TableParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        let node = client.get_node(addr.store, addr.node).await.map_err(rpc)?;
        let mut book = load_book(&client, addr, &node, &[]).await;
        let mut answer = String::new();
        let answer_ref = &mut answer;
        self.edit(&client, addr, move |doc| {
            let id = {
                let mut work = Work::new(Sheets::of_node(doc), &mut book);
                let id = find_table(work.snap(), &p.table)?.id.clone();
                *answer_ref = ops::delete_table(&mut work, &p.table)?;
                id
            };
            // Nothing in the text names it: fine.
            let _ = doc.remove_embeds(SHEET_ATTR, &id);
            Ok(())
        })
        .await?;
        Ok(answer)
    }

    /// Move a table: within its node (after another table, or in a text node
    /// to another place in the text), or into another node, a sheet node or
    /// a text node's text (Convert to Sheet Node, and back). Its rows, columns
    /// and ids go with it; formulas on both sides that read it, or that it
    /// reads, are rewritten to name the other note, so they compute as before.
    /// A table a link column ties to another table of its note cannot leave
    /// it (a link reaches its own note's rows only).
    #[tool(name = "move_table")]
    async fn move_table(&self, Parameters(p): Parameters<MoveTableParams>) -> Result<String, String> {
        let client = self.client().await?;
        let from = nodes::resolve(&client, &p.node).await?;
        let from_node = client.get_node(from.store, from.node).await.map_err(rpc)?;
        let snap = sheets_of(&from_node);
        let table = find_table(&snap, &p.table)?.clone();
        let to = match &p.to {
            Some(to) => nodes::resolve(&client, to).await?,
            None => from,
        };
        let to_node = if to == from { from_node.clone() } else { client.get_node(to.store, to.node).await.map_err(rpc)? };
        let to_text = to_node.node_type != node_types::SHEET;
        if to_node.node_type == node_types::MOUNT || to_node.node_type == node_types::BOARD {
            return Err(format!("A table goes in a text node or a sheet node; that is a {} node.", to_node.node_type));
        }
        if p.after_text.is_some() && !to_text {
            return Err("after_text places a table in a text node's text; a sheet node has no text (use after).".into());
        }
        let after = p.after.clone();
        if to == from {
            let (id, quote) = (table.id.clone(), p.after_text.clone());
            self.edit(&client, from, move |doc| {
                if let Some(a) = &after {
                    let target = find_table(&Sheets::of_node(doc).read(), a)?.id.clone();
                    let edited = Sheets::of_node(doc).edit(|t| t.move_table(&id, pimble_sheet::doc::Placement::After(target)));
                    edited.result.map_err(|e| e.to_string())?;
                }
                if let Some(quote) = quote.as_deref() {
                    let _ = doc.remove_embeds(SHEET_ATTR, &id);
                    doc.insert_markdown_after(quote, false, &embed_markdown(&id)).map_err(|e| e.to_string())?;
                }
                Ok(())
            })
            .await?;
            return Ok(format!("Moved {} within its node.", table.name));
        }
        let tied = !table.row_links().is_empty() || snap.tables.iter().any(|o| o.id != table.id && o.row_links().contains(&table.id));
        if tied {
            return Err(format!(
                "A link column ties {} to another table of its note, and a link reaches rows of its own note only, so it cannot move to another node.",
                table.name
            ));
        }
        let (from_ref, to_ref) = (node_ref(from), node_ref(to));
        let moved = table.map_formulas(|f| pimble_sheet::formula::rehome::moved_out(f, &table.id, &from_ref, Some(&to_ref)));
        // Its new node first: if the second edit fails, the table is in both,
        // never in neither.
        let (id, quote) = (table.id.clone(), p.after_text.clone());
        self.edit(&client, to, move |doc| {
            let sheets = Sheets::of_node(doc);
            let target = match &after {
                Some(a) => Some(find_table(&sheets.read(), a)?.id.clone()),
                None => None,
            };
            // This note's tables that read it in its old note now read it here.
            let here: Vec<pimble_sheet::TableSnap> = sheets.read().tables;
            let edited = sheets.edit(|t| {
                for before in &here {
                    let after = before.map_formulas(|f| pimble_sheet::formula::rehome::arrived(f, &moved.id, &from_ref));
                    write_formulas(t, before, &after)?;
                }
                t.copy_table(&moved)?;
                if let Some(target) = target {
                    t.move_table(&moved.id, pimble_sheet::doc::Placement::After(target))?;
                }
                Ok(())
            });
            edited.result.map_err(|e| e.to_string())?;
            if to_text {
                let atom = embed_markdown(&id);
                match quote.as_deref() {
                    Some(q) => doc.insert_markdown_after(q, false, &atom),
                    None => doc.append_markdown(&atom),
                }
                .map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .await?;
        let id = table.id.clone();
        let rest: Vec<pimble_sheet::TableSnap> = snap.tables.iter().filter(|t| t.id != id).cloned().collect();
        let left = self
            .edit(&client, from, move |doc| {
                let edited = Sheets::of_node(doc).edit(|t| {
                    t.delete_table(&id)?;
                    for before in &rest {
                        let after = before.map_formulas(|f| pimble_sheet::formula::rehome::left_behind(f, &id, &to_ref));
                        write_formulas(t, before, &after)?;
                    }
                    Ok(())
                });
                edited.result.map_err(|e| e.to_string())?;
                let _ = doc.remove_embeds(SHEET_ATTR, &id);
                Ok(())
            })
            .await;
        match left {
            Ok(()) => Ok(format!("Moved {} to {}.", table.name, to.link())),
            Err(e) => Err(format!("{} was copied to {} but could not be taken out of its old node ({e}); it is in both now.", table.name, to.link())),
        }
    }
}

/// Write the formulas and source of `after` where they differ from `before`
/// (the same table, rewritten).
fn write_formulas(t: &mut pimble_sheet::doc::SheetsTxn<'_, '_>, before: &pimble_sheet::TableSnap, after: &pimble_sheet::TableSnap) -> pimble_sheet::doc::Result<()> {
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

/// The embed attribute naming a table in a text node's text.
const SHEET_ATTR: &str = "data-sheet";

/// The tools against a real server on a scratch store, called as an LLM
/// calls them.
#[cfg(test)]
mod server_tests {
    use std::sync::Arc;

    use pimble_client::PimbleClient;
    use pimble_server::{PimbleServer, ServerConfig};
    use rmcp::handler::server::wrapper::Parameters;
    use serde::de::DeserializeOwned;
    use serde_json::json;

    use super::super::Pimble;
    use crate::connection::Connection;

    fn p<T: DeserializeOwned>(v: serde_json::Value) -> Parameters<T> {
        Parameters(serde_json::from_value(v).expect("params"))
    }

    async fn start() -> (PimbleServer, Pimble, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut server = PimbleServer::with_config(ServerConfig {
            addr: "127.0.0.1:0".parse().unwrap(),
            replicas_dir: Some(dir.path().join("replicas")),
            ..Default::default()
        });
        server.start().await.expect("server starts");
        let client = PimbleClient::connect(format!("http://{}", server.addr())).await.expect("client connects");
        client.create_store(dir.path().join("trips.pimble"), "Trips").await.expect("store");
        let pimble = Pimble::new(Arc::new(Connection::with_client(client)));
        (server, pimble, dir)
    }

    /// The Spreadsheet session's repro: a summary whose grouping reads a
    /// formula column that reads, through a link, a date computed from the
    /// note's own field. get_table computed it; list_tables said "its formula
    /// fails" because it computed with this node's tables alone.
    #[tokio::test]
    async fn list_tables_computes_as_get_table_does() {
        let (mut server, pimble, _dir) = start().await;
        pimble.create_node(p(json!({ "parent": "Trips", "title": "Trip" }))).await.unwrap();
        pimble.set_fields(p(json!({ "node": "Trips/Trip", "fields": { "trip_start": "2026-10-01" } }))).await.unwrap();
        pimble
            .create_table(p(json!({
                "node": "Trips/Trip", "name": "Itinerary",
                "columns": [
                    { "name": "stop", "type": "text" },
                    { "name": "day", "type": "number" },
                    { "name": "date", "formula": "='Trip'!trip_start+[@day]-1" },
                ],
                "rows": [ { "stop": "Moab", "day": 1 }, { "stop": "Zion", "day": 2 } ],
            })))
            .await
            .unwrap();
        pimble
            .create_table(p(json!({
                "node": "Trips/Trip", "name": "Expenses",
                "columns": [
                    { "name": "at", "type": "link", "target": "Itinerary" },
                    { "name": "amount", "type": "money" },
                    { "name": "date", "formula": "=[@at].date" },
                ],
                "rows": [ { "at": "Moab", "amount": 10 }, { "at": "Zion", "amount": 20 }, { "at": "Moab", "amount": 5 } ],
            })))
            .await
            .unwrap();
        pimble
            .set_summary(p(json!({
                "node": "Trips/Trip", "table": "Spend_by_Stop",
                "formula": "=GROUPBY(HSTACK(Expenses[date],Expenses[at]),Expenses[amount],SUM)",
            })))
            .await
            .unwrap();

        let table = pimble.get_table(p(json!({ "node": "Trips/Trip", "table": "Spend_by_Stop" }))).await.unwrap();
        assert!(!table.contains("fails"), "{table}");
        assert!(table.contains("Moab") && table.contains("15"), "{table}");
        let listed = pimble.list_tables(p(json!({ "node": "Trips/Trip" }))).await.unwrap();
        assert!(!listed.contains("fails"), "{listed}");
        assert!(listed.contains("Spend_by_Stop"), "{listed}");
        let by_store = pimble.list_tables(p(json!({ "store": "Trips" }))).await.unwrap();
        assert!(!by_store.contains("fails"), "{by_store}");
        let _ = server.stop().await;
    }

    /// The other repro: reshaping a summary keeps its own columns on the
    /// result columns they read (contract section 4, result column keys).
    #[tokio::test]
    async fn set_summary_reshape_keeps_own_columns() {
        let (mut server, pimble, _dir) = start().await;
        pimble.create_node(p(json!({ "parent": "Trips", "title": "Trip" }))).await.unwrap();
        pimble
            .create_table(p(json!({
                "node": "Trips/Trip", "name": "Itinerary",
                "columns": [ { "name": "stop", "type": "text" }, { "name": "day total", "type": "number" } ],
                "rows": [ { "stop": "Moab", "day total": 100 }, { "stop": "Zion", "day total": 200 } ],
            })))
            .await
            .unwrap();
        pimble
            .create_table(p(json!({
                "node": "Trips/Trip", "name": "Expenses",
                "columns": [
                    { "name": "at", "type": "link", "target": "Itinerary" },
                    { "name": "date", "type": "date" },
                    { "name": "amount", "type": "money" },
                ],
                "rows": [
                    { "at": "Moab", "date": "2026-10-01", "amount": 10 },
                    { "at": "Zion", "date": "2026-10-02", "amount": 20 },
                ],
            })))
            .await
            .unwrap();
        pimble
            .set_summary(p(json!({
                "node": "Trips/Trip", "table": "Spend",
                "formula": "=GROUPBY(Expenses[at],Expenses[amount],SUM)",
                "column_names": ["stop", "extras"],
            })))
            .await
            .unwrap();
        pimble
            .add_column(p(json!({ "node": "Trips/Trip", "table": "Spend", "name": "budget", "formula": "=[@stop].[day total]" })))
            .await
            .unwrap();
        let reshaped = pimble
            .set_summary(p(json!({
                "node": "Trips/Trip", "table": "Spend",
                "formula": "=GROUPBY(HSTACK(Expenses[date],Expenses[at]),Expenses[amount],SUM)",
                "column_names": ["date", "stop", "extras"],
            })))
            .await
            .unwrap();
        assert!(!reshaped.contains("#VALUE!"), "{reshaped}");
        let table = pimble.get_table(p(json!({ "node": "Trips/Trip", "table": "Spend" }))).await.unwrap();
        assert!(table.contains("=[@stop].[day total]"), "{table}");
        assert!(!table.contains("#VALUE!") && !table.contains("#REF!"), "{table}");
        assert!(table.contains("200"), "{table}");
        let _ = server.stop().await;
    }
}
