//! The tools (docs/MCP_CONTRACT.md "The tools"). Every write is an edit of the
//! node's existing document sent with `applyEdit`, or one of the RPCs that make
//! structural edits of node documents; nothing replaces a document wholesale.

use std::collections::HashSet;
use std::sync::Arc;

use pimble_api::names::{label, stop_sentence, store_tops};
use pimble_api::rpc::RpcBackend;
use pimble_api::{recipes, Addr, ApiError, Edited, Pimble as Api};
use pimble_core::{LinkResolution, Node, PimbleUrl, Place, Store, StoreAccess, StoreKind, SyncState};
use pimble_crdt::{CrdtError, NodeDoc};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    Implementation, ListResourcesResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse,
    ReadResourceResult, Resource, ResourceContents, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{tool, tool_handler, tool_router, ErrorData, RoleServer, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::connection::{Connection, Shared};
use crate::sheets::guide;

mod canvas_tools;
mod discussion_tools;
#[cfg(test)]
mod api_tests;
#[cfg(test)]
mod presence_tests;
mod sheet_tools;

const INSTRUCTIONS: &str = "\
Pimble is the person's personal information manager: stores hold trees of nodes, each node \
has a title, tags and rich-text content. These tools read, search and write the stores open \
on the person's computer, as a collaborator: edits merge with whatever the person is typing.

Naming a node: every `node`/`parent` argument takes a pimble: link (what Pimble's \"Copy \
Link\" gives the person, and what every tool answer shows), a bare node id, or a path of \
titles from the store's name such as \"Family Management/MEDICAL\" (or from anywhere, such as \
\"MEDICAL/Dr Smith\"); a link to a node that moved leads to where it went. When a name is ambiguous \
the tool says so and lists the links to choose from. Use list_stores and list_children (with \
depth for an outline) or find_node to discover where things are.

Content is Markdown: headings, paragraphs, bold, italic, strikethrough, inline code, links, \
fenced code, bullet and numbered lists (nested), block quotes, pipe tables, horizontal \
rules. Underline, highlight, text colour, subscript and superscript are the HTML tags <u>, \
<mark>, <span style=\"color:#rrggbb\">, <sub> and <sup>; a table with merged cells or blocks \
in a cell is an HTML <table>. Pictures are canvases: \
a picture already in Pimble is written as get_node shows it (![alt](pimble-blob:...#canvas=<id>)), so \
keep those lines when rewriting; insert_canvas puts a new picture or an empty canvas into a note. Other \
images, task lists, footnotes and any other HTML are refused with a sentence naming the line; nothing is \
written then. Link to another node with its pimble: link as the href.

Edits touch only what they name: append, insert_after (after a block, or after a heading's \
section), replace_section (the blocks under a heading), replace_text (an exact quote inside \
one paragraph, plain text), replace_block (one block replaced, or removed with empty \
Markdown: a heading removed or re-levelled), remove_section (a heading and everything under \
it). A quote must appear exactly once in the node. For restructuring, replace_content \
rewrites the whole content but writes only the blocks that differ, and the node keeps its \
link: never delete and recreate a note to restructure it. create_node takes the new node's \
first content. Inline code cannot also be bold, italic, struck through or a link. Deleting is undoable \
(undelete_node, and the app's Recently Deleted).

Sheets are small named tables of typed columns and rows, with Excel formulas per column, kept \
in a sheet node or in a text node's text. list_tables and get_table read them (get_node shows them \
too, a text node's where they are in its text); create_sheet, create_table, add_rows, update_rows, \
delete_rows, add_column, update_column, delete_column, set_summary, rename_table, move_table and \
delete_table write them, touching only what they name; rows_pointing_here lists the rows that \
link to a node; get_fields and set_fields read and write a node's own fields (read in formulas as \
'Note'!field and [@link].field); evaluate \
runs a formula without saving it. Values go in as a person types them (\"$12.50\", \"yesterday\", \
a linked row's first-column value); rows are named by id or by their first column's value. \
Before writing a formula, read sheet_formula_guide (also the resource \"Sheet formulas\").

Discussions are threaded conversations, on a node (they belong to it and go where it goes) \
or as discussion nodes of their own. list_discussions lists a node's, read_discussion reads \
one as a nested list with message ids, start_discussion starts one with its first message, \
reply answers a message (or posts at the top level), edit_message and delete_message change \
only your own messages, resolve_discussion closes or reopens one. Messages are Markdown with \
bold, italic and links; your messages are signed with your agent's name.

A metanode is a node shown a particular way: it names which nodes it shows (its source, such as \
its children two levels deep) and the viewer that shows them (such as a kanban, whose columns are \
the first level and cards the second, in the tree's order). get_node says when a node is one, \
get_metanode lists what it shows, set_metanode sets or clears its viewer and source. A metanode can also have a query \
(set_metanode with query): it shows its own children and then the nodes its query finds, which live \
elsewhere. Moving, creating and editing the nodes it shows is done with the ordinary tools.

Canvases are infinite whiteboards (a canvas node) and the marks on pictures in a note's text (a \
picture's canvas, named by the note and the id after #canvas= in its URL). get_canvas lists a canvas's \
layers and elements with ids, positions and text; render_canvas shows it as an image; create_canvas makes \
a canvas node (empty or from a picture file); insert_canvas puts one into a note's text; add_elements \
draws sticky notes, text, shapes, frames, lines, arrows, connectors that follow the elements they join, \
pen strokes, numbered steps, callouts, node cards, pictures and groups in one edit; update_elements and \
delete_elements change them; add_layer, update_layer and delete_layer manage layers; set_canvas sets a \
page and shows or hides the markup. Coordinates are canvas units, x right and y down; a picture at 100% \
is one unit per pixel; strokes take smooth and closed, and a pen with a fill is a filled shape. Say what \
things mean: every element and layer takes a note and named fields (role=door, elevation=60 m), which \
get_canvas shows (with_field lists only the elements with one) and search finds; read them before \
guessing what a shape is, and write them when you draw something whose meaning a picture alone does not \
say. Locked layers and elements take no edits until unlocked, except to their notes and fields. Your elements are \
signed with your agent's name.";

pub struct Pimble {
    /// The one API (docs/API_CONTRACT.md): what the tools read and write
    /// through, as this agent, holding the documents it has read.
    api: Arc<Api>,
    tool_router: ToolRouter<Self>,
}

/// A library's or the API's refusal, as the sentence a tool answers with.
fn crdt(e: CrdtError) -> String {
    ApiError::from(e).sentence()
}

/// A node's type as the person knows it: a `board` node is a canvas in the
/// interface (docs/CANVAS_CONTRACT.md, "Glossary"); every other type is its own name.
pub fn type_name(node: &Node) -> &str {
    if node.node_type == pimble_core::node_types::BOARD {
        "canvas"
    } else {
        &node.node_type
    }
}

impl Pimble {
    pub fn new(conn: Arc<Connection>) -> Self {
        // An agent's edits are attributed to it (docs/AGENTS_CONTRACT.md).
        let client_id = match crate::connection::agent_id() {
            Some(id) => format!("agent:{id}"),
            None => format!("mcp:{}", uuid::Uuid::new_v4()),
        };
        Self::with_api(Api::new(RpcBackend::new(Shared(conn)), client_id))
    }

    pub fn with_api(api: Api) -> Self {
        Self {
            api: Arc::new(api),
            tool_router: Self::tool_router() + Self::sheet_router() + Self::discussion_router() + Self::canvas_router(),
        }
    }

    /// Say this agent is in `addr` (a tool read it), keeping its caret there.
    async fn present(&self, addr: Addr) {
        self.api.say(addr, None).await;
    }

    /// Let `edit` change the node's document and send what changed
    /// (`pimble_api::Pimble::edit`, the one write). Then the agent is present
    /// there, its caret at the end of what it wrote, and in the node a
    /// discussion is on.
    async fn edit(&self, addr: Addr, edit: impl FnOnce(&mut NodeDoc) -> Result<(), String> + Send) -> Result<(), String> {
        let edited = self.edit_doc(addr, edit).await?;
        self.api.say(addr, edited.caret).await;
        if let Some(host) = edited.host {
            self.present(Addr { store: addr.store, node: host }).await;
        }
        Ok(())
    }

    /// The write itself, saying nothing; refused when it would change
    /// nothing, so an agent hears that its edit missed.
    async fn edit_doc(&self, addr: Addr, edit: impl FnOnce(&mut NodeDoc) -> Result<(), String> + Send) -> Result<Edited<()>, String> {
        let edited = self.api.edit(addr, |doc, _| edit(doc)).await?;
        if !edited.changed {
            return Err("That would change nothing, so nothing was written.".into());
        }
        Ok(edited)
    }

    async fn describe(&self, addr: Addr) -> String {
        format!("{} ({})", self.api.path(addr).await, addr.link())
    }

    /// A query metanode's source from what an agent gave (docs/QUERY_METANODES_CONTRACT.md
    /// section 3), with its nodes and stores named as the source names them.
    async fn match_source(&self, q: &MatchParams) -> Result<pimble_core::metanode::Source, String> {
        use pimble_core::metanode::{node_ref, Condition, FieldOp, Order, Query, Source, Within, DEFAULT_LIMIT, MAX_LIMIT};
        let within = match q.within.as_deref().map(str::trim).filter(|w| !w.is_empty()) {
            None => None,
            Some(w) if w.eq_ignore_ascii_case("everywhere") => Some(Within::Everywhere),
            Some(w) => match self.api.find_store(w).await {
                Ok(store) => Some(Within::Store(store.id)),
                Err(_) => {
                    let under = self.api.resolve(w).await?;
                    Some(Within::Under(node_ref(under.store, under.node)))
                }
            },
        };
        let mut all = Vec::new();
        for c in &q.all {
            let given = [c.tag.is_some(), c.no_tag.is_some(), c.text.is_some(), c.field.is_some(), c.links_to.is_some(), c.node_type.is_some()];
            if given.iter().filter(|g| **g).count() != 1 {
                return Err("Each condition gives exactly one of `tag`, `no_tag`, `text`, `field`, `links_to`, `type`.".into());
            }
            all.push(if let Some(tag) = &c.tag {
                Condition::Tag { tag: tag.clone() }
            } else if let Some(no_tag) = &c.no_tag {
                Condition::NoTag { no_tag: no_tag.clone() }
            } else if let Some(text) = &c.text {
                Condition::Text { text: text.clone() }
            } else if let Some(field) = &c.field {
                let is = match c.is.as_deref().map(str::trim).unwrap_or("set") {
                    "<" => FieldOp::Less,
                    "<=" => FieldOp::AtMost,
                    "=" | "==" => FieldOp::Equal,
                    ">=" => FieldOp::AtLeast,
                    ">" => FieldOp::More,
                    "set" => FieldOp::Set,
                    "empty" => FieldOp::Empty,
                    other => return Err(format!("`is` is one of <, <=, =, >=, >, set, empty (not \"{other}\").")),
                };
                if !matches!(is, FieldOp::Set | FieldOp::Empty) && c.value.as_deref().is_none_or(|v| v.trim().is_empty()) {
                    return Err(format!("Comparing the field \"{field}\" needs a `value`."));
                }
                Condition::Field { field: field.clone(), is, value: c.value.clone() }
            } else if let Some(node) = &c.links_to {
                let target = self.api.resolve(node).await?;
                Condition::LinksTo { links_to: node_ref(target.store, target.node) }
            } else {
                Condition::Type { node_type: c.node_type.clone().unwrap_or_default() }
            });
        }
        let order = match q.order.as_deref().map(str::trim).unwrap_or("title") {
            "title" | "" => Order::Title,
            "modified" => Order::Modified,
            "created" => Order::Created,
            other => match other.strip_prefix("field:") {
                Some(name) if !name.trim().is_empty() => Order::Field(name.trim().to_string()),
                _ => return Err("`order` is \"title\", \"modified\", \"created\" or \"field:<name>\".".into()),
            },
        };
        let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
        if limit == 0 || limit > MAX_LIMIT {
            return Err(format!("`limit` is from 1 to {MAX_LIMIT}."));
        }
        Ok(Source { query: Query::Match { within, all, order, descending: q.descending, limit }, later: Default::default() })
    }
}

// ── Parameters ───────────────────────────────────────────────────────────

#[derive(Deserialize, JsonSchema)]
pub struct NodeParam {
    /// The node: a pimble: link, a node id, or a path like "Store/Folder/Note".
    pub node: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ListChildrenParams {
    /// The node (or a store's name, for its top level).
    pub node: String,
    /// How many levels to list; 1 lists the children only. At most 6.
    #[serde(default)]
    pub depth: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct FindNodeParams {
    /// The title to look for, case-insensitive: exact matches first, then titles
    /// starting with it, then titles containing it.
    pub title: String,
    /// Only this store (a name or id).
    #[serde(default)]
    pub store: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SearchParams {
    /// What to look for.
    pub query: String,
    /// Search by meaning instead of by words.
    #[serde(default)]
    pub semantic: bool,
    /// Only this store (a name or id).
    #[serde(default)]
    pub store: Option<String>,
    /// How many results, at most 50 (default 20).
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct LinkParam {
    /// A pimble: link.
    pub link: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct StoreParam {
    /// The store: its name or id.
    pub store: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateNodeParams {
    /// Where the new node goes: the parent node (or a store's name, for its top level).
    pub parent: String,
    /// The new node's title.
    pub title: String,
    /// The new node's content, as Markdown.
    #[serde(default)]
    pub markdown: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct AppendParams {
    /// The node to append to.
    pub node: String,
    /// What to append, as Markdown.
    pub markdown: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct InsertAfterParams {
    /// The node to insert into.
    pub node: String,
    /// Text that appears in exactly one block of the node: the new content goes after
    /// that block (a list item's text names its whole list).
    pub after: String,
    /// When `after` names a heading: go after the whole section under it instead.
    #[serde(default)]
    pub after_section: bool,
    /// What to insert, as Markdown.
    pub markdown: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReplaceSectionParams {
    /// The node.
    pub node: String,
    /// Text of the heading whose section is replaced; the heading itself stays.
    pub heading: String,
    /// The section's new content, as Markdown.
    pub markdown: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReplaceTextParams {
    /// The node.
    pub node: String,
    /// The exact text to replace, appearing exactly once in the node, inside one
    /// paragraph (plain text: no Markdown syntax).
    pub quote: String,
    /// What replaces it (plain text; empty deletes the quote).
    pub text: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReplaceBlockParams {
    /// The node.
    pub node: String,
    /// Text that appears in exactly one block (a heading, a paragraph, a list item's
    /// text names its whole list).
    pub block: String,
    /// What replaces that block, as Markdown; empty removes it.
    #[serde(default)]
    pub markdown: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct RemoveSectionParams {
    /// The node.
    pub node: String,
    /// Text of the heading to remove, together with everything under it.
    pub heading: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReplaceContentParams {
    /// The node.
    pub node: String,
    /// The node's whole new content, as Markdown. Blocks that stay the same are left
    /// untouched; empty clears the content.
    pub markdown: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct RenameParams {
    /// The node.
    pub node: String,
    /// The new title.
    pub title: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetTagsParams {
    /// The node.
    pub node: String,
    /// The node's tags, replacing the ones it has.
    pub tags: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetMetanodeParams {
    /// The node.
    pub node: String,
    /// The viewer to show it with (a plugin's id, such as "kanban"). Leave it
    /// out, or give "", to show it as an ordinary node again (its outline):
    /// its source is kept for next time.
    #[serde(default)]
    pub viewer: Option<String>,
    /// How many levels below the node it shows: 1 its children (the
    /// default), 2 its children and theirs (a kanban's columns and cards).
    /// Leave it out to keep the source it has.
    #[serde(default)]
    pub depth: Option<u32>,
    /// Make it a metanode with a query instead: it shows its own children, then the
    /// nodes this finds (live, never copied). Its viewer is "list" unless
    /// `viewer` says otherwise. Not together with `depth`.
    #[serde(default)]
    pub query: Option<MatchParams>,
}

/// What a metanode's query finds: the nodes every condition holds for.
#[derive(Deserialize, JsonSchema)]
pub struct MatchParams {
    /// Where it looks: leave it out for the node's own store; "everywhere"
    /// for every open store; a store's name; or a node (link, id or path) for
    /// everything under it.
    #[serde(default)]
    pub within: Option<String>,
    /// The conditions, all of which hold for a node it finds. Empty finds
    /// every node where it looks.
    #[serde(default)]
    pub all: Vec<ConditionParams>,
    /// "title" (the default), "modified", "created", or "field:<name>".
    #[serde(default)]
    pub order: Option<String>,
    /// Largest, newest or last first.
    #[serde(default)]
    pub descending: bool,
    /// At most this many (default 50, at most 500).
    #[serde(default)]
    pub limit: Option<u32>,
}

/// One condition: give exactly one of `tag`, `no_tag`, `text`, `field`,
/// `links_to`, `type`.
#[derive(Deserialize, JsonSchema, Default)]
pub struct ConditionParams {
    /// Tagged this.
    #[serde(default)]
    pub tag: Option<String>,
    /// Not tagged this.
    #[serde(default)]
    pub no_tag: Option<String>,
    /// Every word of this in the title or text, any case.
    #[serde(default)]
    pub text: Option<String>,
    /// A field's name, compared with `is` and `value`.
    #[serde(default)]
    pub field: Option<String>,
    /// For `field`: "<", "<=", "=", ">=", ">", "set" or "empty".
    #[serde(default)]
    pub is: Option<String>,
    /// For `field`: the value, as a person types it ("Friday", "$12", "Open").
    #[serde(default)]
    pub value: Option<String>,
    /// Content that links to this node (link, id or path).
    #[serde(default)]
    pub links_to: Option<String>,
    /// "document", "canvas", "sheet", "discussion", "kanban", or a node type.
    #[serde(default, rename = "type")]
    pub node_type: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct MoveNodeParams {
    /// The node to move.
    #[serde(default)]
    pub node: Option<String>,
    /// Several nodes to move together, in the order they land (instead of, or
    /// after, `node`). One that may not move refuses them all.
    #[serde(default)]
    pub nodes: Vec<String>,
    /// Its new parent (or a store's name, for its top level).
    pub new_parent: String,
    /// A child of the new parent to put it right after (a link or a name, as
    /// `node` takes). Leave out `after` and `first` for the end.
    #[serde(default)]
    pub after: Option<String>,
    /// Put it first among the new parent's children.
    #[serde(default)]
    pub first: bool,
}

// ── Tools ────────────────────────────────────────────────────────────────

#[tool_router(router = tool_router)]
impl Pimble {
    /// The stores open on this computer: name, id, kind, sync state, and whether
    /// you may change them.
    #[tool(name = "list_stores")]
    async fn list_stores(&self) -> Result<String, String> {
        let stores = self.api.stores().await?;
        if stores.is_empty() {
            return Ok("No stores are open in Pimble on this computer.".into());
        }
        let mut out = String::new();
        for store in &stores {
            let kind = match store.sync_mode {
                StoreKind::Vault => "encrypted",
                _ => "plain",
            };
            let sync = match &store.sync_state {
                SyncState::Synced { .. } => "synced",
                SyncState::Syncing => "syncing",
                _ => "local",
            };
            let access = if store.access == StoreAccess::Read { ", read-only" } else { "" };
            let shared = store.shared_by.as_deref().map(|by| format!(", shared by {by}")).unwrap_or_default();
            out.push_str(&format!(
                "- {} (id {}, root {}; {kind}, {sync}{access}{shared})\n",
                store.name,
                store.id,
                PimbleUrl { store: store.id, node: store.root_node_id, anchor: None }
            ));
        }
        Ok(out)
    }

    /// A node's children, or with `depth` an indented outline of its subtree. Each
    /// line carries the node's link, to name it in the next call.
    #[tool(name = "list_children")]
    async fn list_children(&self, Parameters(p): Parameters<ListChildrenParams>) -> Result<String, String> {
        let depth = p.depth.unwrap_or(1).clamp(1, 6);
        // A store with several shared roots lists them as its top level.
        let tops = match self.api.find_store(&p.node).await {
            Ok(store) if !store.roots.is_empty() => store_tops(&store),
            _ => vec![self.api.resolve(&p.node).await?],
        };
        let stores = self.api.known_stores().await?;
        let mut out = String::new();
        let mut seen = HashSet::new();
        for top in tops {
            if !store_listing_is_top(&stores, top) {
                out.push_str(&format!("{}:\n", self.describe(top).await));
            }
            outline(&self.api, top, depth, 0, &mut seen, &mut out).await?;
        }
        if out.is_empty() {
            out = "(no children)".into();
        }
        Ok(out)
    }

    /// Nodes whose title matches, each with its path and link: for "put this under
    /// the MEDICAL node".
    #[tool(name = "find_node")]
    async fn find_node(&self, Parameters(p): Parameters<FindNodeParams>) -> Result<String, String> {
        let wanted = p.title.trim().to_lowercase();
        if wanted.is_empty() {
            return Err("Give some of the title to look for.".into());
        }
        let searched: Vec<Store> = match &p.store {
            Some(name) => vec![self.api.find_store(name).await?],
            None => self.api.known_stores().await?,
        };
        // (rank, path, link): 0 exact, 1 prefix, 2 contains.
        let mut hits: Vec<(u8, String, String)> = Vec::new();
        for store in &searched {
            // The tops themselves are the store's rows, not names to find.
            let Ok(walked) = self.api.walk(store).await else { continue };
            for w in walked.into_iter().filter(|w| w.parent.is_some()) {
                let lower = label(&w.node).to_lowercase();
                let rank = if lower == wanted {
                    Some(0)
                } else if lower.starts_with(&wanted) {
                    Some(1)
                } else if lower.contains(&wanted) {
                    Some(2)
                } else {
                    None
                };
                if let Some(rank) = rank {
                    hits.push((rank, w.path, w.addr.link()));
                }
            }
        }
        if hits.is_empty() {
            return Ok(format!("No node's title contains \"{}\".", p.title.trim()));
        }
        hits.sort();
        hits.dedup();
        let total = hits.len();
        let mut out: String = hits.iter().take(30).map(|(_, path, link)| format!("- {path} ({link})\n")).collect();
        if total > 30 {
            out.push_str(&format!("... and {} more; be more specific.\n", total - 30));
        }
        Ok(out)
    }

    /// A node: its link, path, title, tags, number of children, and content as
    /// Markdown.
    #[tool(name = "get_node")]
    async fn get_node(&self, Parameters(p): Parameters<NodeParam>) -> Result<String, String> {
        let addr = self.api.resolve(&p.node).await?;
        let node = self.api.node(addr).await?;
        self.present(addr).await;
        let doc = NodeDoc::load(&node.content).map_err(|e| e.to_string())?;
        let content = match doc.markdown() {
            Ok(md) => md,
            Err(_) => format!("(formatting left out: this content cannot be shown as Markdown)\n\n{}", doc.text()),
        };
        let mut out = format!(
            "title: {}\nlink: {}\npath: {}\ntype: {}\n",
            label(&node),
            addr.link(),
            self.api.path(addr).await,
            type_name(&node)
        );
        if !node.metadata.tags.is_empty() {
            out.push_str(&format!("tags: {}\n", node.metadata.tags.join(", ")));
        }
        if !node.metadata.fields.is_empty() {
            let fields: Vec<String> = node.metadata.fields.iter().map(|(n, v)| format!("{n} = {v}")).collect();
            out.push_str(&format!("fields: {}\n", fields.join("; ")));
        }
        out.push_str(&format!("children: {}\n", node.children.len()));
        if node.attachments > 0 {
            out.push_str(&format!("discussions: {} (list_discussions lists them)\n", node.attachments));
        }
        out.push_str(&format!("modified: {}\n", node.metadata.modified_at.to_rfc3339()));
        match node.access {
            StoreAccess::Read => out.push_str("access: read-only\n"),
            StoreAccess::Comment => out.push_str("access: comment-only (start_discussion and reply work here)\n"),
            StoreAccess::Full => {}
        }
        if let Some(metanode) = &node.metadata.metanode {
            // docs/METANODES_CONTRACT.md section 3: what it shows, and how.
            out.push_str(&format!("({}; get_metanode lists what it shows)\n", metanode.describe(&label(&node))));
            if let Some(why) = metanode.source.not_understood() {
                out.push_str(&format!("({why})\n"));
            }
        }
        if node.metadata.custom.contains_key("mount") || node.node_type == "mount" {
            out.push_str("(a mount: its children are another store's; list_children shows them)\n");
        }
        out.push_str("---\n");
        let tables = if node.node_type == pimble_core::node_types::DISCUSSION {
            out.push_str("(a discussion: its content is its messages)\n");
            out.push_str(&Self::discussion_body(&node));
            None
        } else if node.node_type == pimble_core::node_types::BOARD {
            out.push_str("(a canvas: its content is what is drawn on it; render_canvas shows it as a picture)\n");
            out.push_str(&self.canvas_body(&node, None).await);
            None
        } else if node.node_type == pimble_core::node_types::SHEET {
            out.push_str("(a sheet node: its content is its tables)\n");
            self.tables_of_node(addr, &node).await.map(|t| ("Tables", t))
        } else {
            // A text node's tables are shown where they are in its text.
            let content = self.inline_discussions(&content).await;
            let (content, lost) = self.tables_in_text(addr, &node, &content).await;
            out.push_str(if content.is_empty() { "(no content)" } else { &content });
            if content.contains(pimble_crdt::markdown::CANVAS_FRAGMENT) {
                out.push_str(
                    "\n\n(Each picture whose URL ends in #canvas=<id> is a canvas: get_canvas and render_canvas \
                     read it, add_elements marks it up, with this node and canvas <id>. Keep the picture's line \
                     as it is when rewriting the text.)",
                );
            }
            lost.map(|t| ("Tables no longer in the text", t))
        };
        if let Some((heading, tables)) = tables {
            out.push_str(&format!("\n\n## {heading}\n\n"));
            out.push_str(&tables);
        }
        Ok(out)
    }

    /// Search the open stores, by words or (with `semantic`) by meaning.
    #[tool(name = "search")]
    async fn search(&self, Parameters(p): Parameters<SearchParams>) -> Result<String, String> {
        let only = match &p.store {
            Some(name) => vec![self.api.find_store(name).await?.id],
            None => Vec::new(),
        };
        let limit = p.limit.unwrap_or(20).clamp(1, 50);
        let results = self.api.search(&p.query, only, p.semantic, limit).await?;
        if results.is_empty() {
            return Ok(format!("Nothing matches \"{}\".", p.query));
        }
        let mut out = String::new();
        for r in results {
            let addr = Addr { store: r.store_id, node: r.node_id };
            let path = self.api.path(addr).await;
            let snippet: String = r.snippet.split_whitespace().collect::<Vec<_>>().join(" ");
            out.push_str(&format!("- {path} ({})\n  {snippet}\n", addr.link()));
        }
        Ok(out)
    }

    /// Where a pimble: link points now (a node that moved out of a share or store
    /// is followed to where it went).
    #[tool(name = "resolve_link")]
    async fn resolve_link(&self, Parameters(p): Parameters<LinkParam>) -> Result<String, String> {
        let url = PimbleUrl::parse(p.link.trim()).ok_or_else(|| format!("\"{}\" is not a Pimble link.", p.link))?;
        let (resolution, hops) = self.api.follow(&url).await?;
        Ok(match resolution {
            LinkResolution::Live { store_id, node_id } => {
                format!("Live: {}", self.describe(Addr { store: store_id, node: node_id }).await)
            }
            LinkResolution::Deleted { store_id, node_id } => {
                format!("Deleted (undelete_node can put it back): {}", Addr { store: store_id, node: node_id }.link())
            }
            other => {
                let said = stop_sentence(&url, hops, &other);
                if said.is_empty() { "The link leads nowhere." } else { said }.to_string()
            }
        })
    }

    /// A store's recently deleted nodes, which undelete_node can put back.
    #[tool(name = "list_deleted")]
    async fn list_deleted(&self, Parameters(p): Parameters<StoreParam>) -> Result<String, String> {
        let store = self.api.find_store(&p.store).await?;
        let deleted = self.api.list_deleted(store.id).await?;
        if deleted.is_empty() {
            return Ok(format!("Nothing was deleted in {}.", store.name));
        }
        Ok(deleted
            .iter()
            .map(|d| {
                let addr = Addr { store: store.id, node: d.node.id };
                format!(
                    "- {} ({}), was under {}, deleted {}\n",
                    label(&d.node),
                    addr.link(),
                    d.parent_title.as_deref().unwrap_or("?"),
                    d.deleted_at.as_deref().unwrap_or("?")
                )
            })
            .collect())
    }

    /// Create a node under `parent`, with a title and optionally its first content.
    #[tool(name = "create_node")]
    async fn create_node(&self, Parameters(p): Parameters<CreateNodeParams>) -> Result<String, String> {
        let parent = self.api.resolve(&p.parent).await?;
        // Made with its content in the one createNode (a node under a mount
        // lives in the mount's source store).
        let init = recipes::document(&p.title, p.markdown.as_deref())?;
        let addr = self.api.create(parent, Place::Last, init).await?;
        self.present(addr).await;
        Ok(format!("Created {}", self.describe(addr).await))
    }

    /// Append Markdown after the node's content.
    #[tool(name = "append")]
    async fn append(&self, Parameters(p): Parameters<AppendParams>) -> Result<String, String> {
        let addr = self.api.resolve(&p.node).await?;
        self.edit(addr, |doc| doc.append_markdown(&p.markdown).map(drop).map_err(crdt)).await?;
        Ok(format!("Appended to {}.", addr.link()))
    }

    /// Insert Markdown after the block a quote names, or after a heading's whole
    /// section.
    #[tool(name = "insert_after")]
    async fn insert_after(&self, Parameters(p): Parameters<InsertAfterParams>) -> Result<String, String> {
        let addr = self.api.resolve(&p.node).await?;
        self.edit(addr, |doc| doc.insert_markdown_after(&p.after, p.after_section, &p.markdown).map(drop).map_err(crdt)).await?;
        Ok(format!("Inserted into {}.", addr.link()))
    }

    /// Replace the blocks under a heading (up to the next heading of its level or
    /// higher); the heading stays.
    #[tool(name = "replace_section")]
    async fn replace_section(&self, Parameters(p): Parameters<ReplaceSectionParams>) -> Result<String, String> {
        let addr = self.api.resolve(&p.node).await?;
        self.edit(addr, |doc| doc.replace_section_markdown(&p.heading, &p.markdown).map(drop).map_err(crdt)).await?;
        Ok(format!("Replaced the section under \"{}\" in {}.", p.heading, addr.link()))
    }

    /// Replace an exact quote inside one paragraph with new text.
    #[tool(name = "replace_text")]
    async fn replace_text(&self, Parameters(p): Parameters<ReplaceTextParams>) -> Result<String, String> {
        let addr = self.api.resolve(&p.node).await?;
        self.edit(addr, |doc| doc.replace_text(&p.quote, &p.text).map(drop).map_err(crdt)).await?;
        Ok(format!("Replaced the text in {}.", addr.link()))
    }

    /// Replace one block (a heading, a paragraph, a list, a code block) with
    /// Markdown, or remove it with empty Markdown: removing or re-levelling a heading,
    /// turning a paragraph into a list.
    #[tool(name = "replace_block")]
    async fn replace_block(&self, Parameters(p): Parameters<ReplaceBlockParams>) -> Result<String, String> {
        let addr = self.api.resolve(&p.node).await?;
        self.edit(addr, |doc| doc.replace_block_markdown(&p.block, &p.markdown).map(drop).map_err(crdt)).await?;
        let did = if p.markdown.trim().is_empty() { "Removed" } else { "Replaced" };
        Ok(format!("{did} the block in {}.", addr.link()))
    }

    /// Remove a heading and everything under it, up to the next heading of its level
    /// or higher.
    #[tool(name = "remove_section")]
    async fn remove_section(&self, Parameters(p): Parameters<RemoveSectionParams>) -> Result<String, String> {
        let addr = self.api.resolve(&p.node).await?;
        self.edit(addr, |doc| doc.remove_section(&p.heading).map(drop).map_err(crdt)).await?;
        Ok(format!("Removed the section \"{}\" from {}.", p.heading, addr.link()))
    }

    /// Replace the node's whole content, for restructuring. Only what differs is
    /// written: blocks that read the same stay untouched (and so does anyone typing in
    /// them), and the node keeps its link. Prefer the smaller edits for small changes.
    #[tool(name = "replace_content")]
    async fn replace_content(&self, Parameters(p): Parameters<ReplaceContentParams>) -> Result<String, String> {
        let addr = self.api.resolve(&p.node).await?;
        self.edit(addr, |doc| doc.replace_content_markdown(&p.markdown).map(drop).map_err(crdt)).await?;
        Ok(format!("Replaced the content of {}.", addr.link()))
    }

    /// What a metanode shows (docs/METANODES_CONTRACT.md): its source's nodes as an
    /// indented outline in the tree's own order, each line with its link. A node
    /// with no viewer is read as its children. A metanode with a query lists its
    /// own children, then the nodes it finds, each marked found with where it lives.
    #[tool(name = "get_metanode")]
    async fn get_metanode(&self, Parameters(p): Parameters<NodeParam>) -> Result<String, String> {
        let addr = self.api.resolve(&p.node).await?;
        let forest = self.api.metanode_source(addr).await?;
        self.present(addr).await;
        let name = label(&forest.node);
        let mut out = match &forest.spec {
            Some(metanode) => format!(
                "{} ({}) is a metanode: {}, shown with the {} viewer.\n",
                self.api.path(addr).await,
                addr.link(),
                self.api.describe_source(addr, &metanode.source).await,
                metanode.viewer
            ),
            None => format!(
                "{} ({}) is not a metanode (set_metanode makes it one); {}:\n",
                self.api.path(addr).await,
                addr.link(),
                forest.source.describe(&name)
            ),
        };
        if let Some(why) = forest.not_understood() {
            out.push_str(&format!("{why}\n"));
        }
        if forest.entries.is_empty() {
            out.push_str("(it shows nothing: no children)\n");
        }
        for entry in forest.entries.iter().filter(|e| !e.found) {
            let indent = "  ".repeat(entry.depth.saturating_sub(1) as usize);
            out.push_str(&format!("{indent}- {} ({})\n", label(&entry.node), entry.addr.link()));
        }
        if forest.source.is_match() {
            // docs/QUERY_METANODES_CONTRACT.md section 7: the finds after its
            // own children, each marked; they live elsewhere.
            let found: Vec<_> = forest.found().collect();
            match found.len() {
                0 => out.push_str("Found: nothing matches now.\n"),
                n => out.push_str(&format!(
                    "Found ({n}; each lives elsewhere: move_node and delete_node work on it where it lives, not here):\n"
                )),
            }
            for entry in found {
                out.push_str(&format!("- {} ({}) [found, in {}]\n", label(&entry.node), entry.addr.link(), self.api.lives_in(entry.addr).await));
            }
        }
        Ok(out)
    }

    /// Make a node a metanode shown with a viewer and a source, or an ordinary node
    /// again. Every gesture in a viewer is an ordinary edit of real nodes, which the
    /// other tools already make (move_node, create_node, set_fields, set_tags).
    /// With `query` it becomes a metanode with a query: its own children, then the nodes the
    /// query finds (tags, absent tags, words, field values, links, types), live.
    #[tool(name = "set_metanode")]
    async fn set_metanode(&self, Parameters(p): Parameters<SetMetanodeParams>) -> Result<String, String> {
        use pimble_core::metanode::{Source, MAX_SUBTREE_DEPTH};
        let addr = self.api.resolve(&p.node).await?;
        let mut viewer = p.viewer.as_deref().map(str::trim).filter(|v| !v.is_empty());
        let source = match (p.depth, &p.query) {
            (Some(_), Some(_)) => return Err("Give `depth` or `query`, not both.".into()),
            (_, Some(query)) => {
                // A query metanode is shown as a list (docs/QUERY_METANODES_CONTRACT.md section 2).
                viewer = viewer.or(Some("list"));
                Some(self.match_source(query).await?)
            }
            (Some(0), _) => return Err("`depth` is at least 1 (the node's children).".into()),
            (Some(d), _) if d > MAX_SUBTREE_DEPTH => return Err(format!("`depth` is at most {MAX_SUBTREE_DEPTH}.")),
            (Some(1), _) => Some(Source::children()),
            (Some(d), _) => Some(Source::subtree(d)),
            (None, None) => None,
        };
        let edited = self.api.set_metanode(addr, viewer, source.as_ref()).await?;
        if !edited.changed {
            return Err("That would change nothing, so nothing was written.".into());
        }
        let node = self.api.node(addr).await?;
        Ok(match &node.metadata.metanode {
            Some(metanode) => format!(
                "{} is now a metanode: {}, shown with the {} viewer.",
                addr.link(),
                self.api.describe_source(addr, &metanode.source).await,
                metanode.viewer
            ),
            None => format!("{} is an ordinary node again, shown as its outline.", addr.link()),
        })
    }

    /// Rename a node.
    #[tool(name = "rename")]
    async fn rename(&self, Parameters(p): Parameters<RenameParams>) -> Result<String, String> {
        let addr = self.api.resolve(&p.node).await?;
        let title = p.title.trim().to_string();
        if title.is_empty() {
            return Err("A title cannot be empty.".into());
        }
        self.edit(addr, |doc| {
            let at = pimble_api::now();
            doc.set_title(&title, &at).map_err(crdt)?;
            doc.set_custom(pimble_core::custom_keys::EXPLICIT_TITLE, &serde_json::json!(true), &at).map_err(crdt)
        })
        .await?;
        Ok(format!("Renamed {} to \"{title}\".", addr.link()))
    }

    /// Set a node's tags (replacing the ones it has).
    #[tool(name = "set_tags")]
    async fn set_tags(&self, Parameters(p): Parameters<SetTagsParams>) -> Result<String, String> {
        let addr = self.api.resolve(&p.node).await?;
        let tags: Vec<String> = p.tags.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
        self.edit(addr, |doc| doc.set_tags(&tags, &pimble_api::now()).map_err(crdt)).await?;
        Ok(format!("Tagged {}: {}.", addr.link(), if tags.is_empty() { "(none)".into() } else { tags.join(", ") }))
    }

    /// Move a node (or several, with `nodes`) under a new parent, in its store or
    /// into another one. Judged first, as Pimble's explorer judges a drop: a node
    /// cannot go inside itself, nor where you may only read. A move out of a share
    /// or between stores gives the node a new link, which the answer names.
    #[tool(name = "move_node")]
    async fn move_node(&self, Parameters(p): Parameters<MoveNodeParams>) -> Result<String, String> {
        let mut nodes = Vec::new();
        for name in p.node.iter().chain(&p.nodes) {
            nodes.push(self.api.resolve(name).await?);
        }
        if nodes.is_empty() {
            return Err("Name the node to move (`node`, or `nodes` for several).".into());
        }
        let target = self.api.resolve(&p.new_parent).await?;
        let place = match (&p.after, p.first) {
            (Some(_), true) => return Err("Give `after` or `first`, not both.".into()),
            (None, true) => Place::First,
            (None, false) => Place::Last,
            (Some(after), false) => Place::After(self.api.resolve(after).await?.node),
        };
        // Under a mount is under its source, as everything made there is.
        let plan = self.api.plan_move(&nodes, target, place).await?;
        if let Place::After(after) = place {
            let parent = Addr { store: plan.parent.0, node: plan.parent.1 };
            if !self.api.children(parent).await?.iter().any(|(a, _)| a.node == after) {
                return Err("`after` must be a child of `new_parent`.".into());
            }
        }
        if plan.stays() {
            return Ok(format!("{}: nothing moved.", pimble_api::plan::WHERE_IT_IS));
        }
        let moved = self.api.move_nodes(&plan).await?;
        let mut out = format!("{}.", plan.line);
        if let Some(warning) = &plan.warning {
            out.push_str(&format!(" {warning}"));
        }
        for (i, to) in moved.iter().enumerate() {
            // The answers come in the order the requests went.
            let was = moved_from(&plan, i).unwrap_or(to.addr);
            out.push_str(&format!("\nMoved to {}", self.describe(to.addr).await));
            if to.new_link {
                out.push_str(&format!(". It has a new link; the old one ({}) now leads here.", was.link()));
            }
            if to.left_shares > 0 {
                out.push_str(&format!(" It left {} share(s): members there see it as deleted.", to.left_shares));
            }
        }
        Ok(out)
    }

    /// Delete a node and everything under it. Undoable: undelete_node, or the app's
    /// Recently Deleted.
    #[tool(name = "delete_node")]
    async fn delete_node(&self, Parameters(p): Parameters<NodeParam>) -> Result<String, String> {
        let addr = self.api.resolve(&p.node).await?;
        self.api.can_delete(&[addr]).await?;
        self.api.delete(addr).await?;
        Ok(format!("Deleted {} (undelete_node puts it back).", addr.link()))
    }

    /// Put a deleted node back where it was.
    #[tool(name = "undelete_node")]
    async fn undelete_node(&self, Parameters(p): Parameters<NodeParam>) -> Result<String, String> {
        // A link to a deleted node names it as it is, not where it would lead.
        let addr = match PimbleUrl::parse(p.node.trim()) {
            Some(url) => Addr { store: url.store, node: url.node },
            None => self.api.resolve(&p.node).await?,
        };
        self.api.undelete(addr).await?;
        Ok(format!("Put back {}", self.describe(addr).await))
    }
}

/// The node the `i`th answer of a plan's requests is about (each step
/// answers its nodes in order).
fn moved_from(plan: &pimble_api::plan::MovePlan, i: usize) -> Option<Addr> {
    use pimble_api::plan::MoveStep;
    let mut from = Vec::new();
    for step in &plan.steps {
        match step {
            MoveStep::Move { store, node, .. } => from.push(Addr { store: *store, node: *node }),
            MoveStep::MoveMany { store, nodes, .. } => from.extend(nodes.iter().map(|n| Addr { store: *store, node: *n })),
            MoveStep::Transplant { from: store, node, .. } => from.push(Addr { store: *store, node: *node }),
        }
    }
    from.get(i).copied()
}

/// Whether listing `top` is listing a store's single root (so no header line).
fn store_listing_is_top(stores: &[Store], top: Addr) -> bool {
    stores.iter().any(|s| s.id == top.store && s.root_node_id == top.node)
}

/// An indented outline of `at`'s subtree, `depth` levels deep.
fn outline<'a>(
    api: &'a Api,
    at: Addr,
    depth: u32,
    indent: usize,
    seen: &'a mut HashSet<Addr>,
    out: &'a mut String,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
    Box::pin(async move {
        if !seen.insert(at) {
            return Ok(());
        }
        for (addr, node) in api.children(at).await? {
            let count = node.children.len();
            let mount = if node.node_type == "mount" { ", a mount" } else { "" };
            let children = match count {
                0 => String::new(),
                1 => ", 1 child".into(),
                n => format!(", {n} children"),
            };
            out.push_str(&format!("{}- {} ({}{children}{mount})\n", "  ".repeat(indent), label(&node), addr.link()));
            if depth > 1 && (count > 0 || node.node_type == "mount") {
                outline(api, addr, depth - 1, indent + 1, seen, out).await?;
            }
        }
        Ok(())
    })
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Pimble {
    /// The client app said who it is: its name is the device the agent's
    /// presence names.
    async fn on_initialized(&self, context: rmcp::service::NotificationContext<RoleServer>) {
        if let Some(info) = context.peer.peer_info() {
            let app = &info.client_info;
            self.api.set_device(app.title.as_deref().unwrap_or(&app.name));
        }
    }

    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_resources().build())
            .with_server_info(Implementation::new("pimble", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    /// The one resource: the sheet formula guide (also the
    /// `sheet_formula_guide` tool, for clients that ignore resources).
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let guide = Resource::new(guide::URI, guide::NAME)
            .with_title(guide::NAME)
            .with_description("How to write formulas for Pimble's sheets: Excel Table syntax, links, other notes, functions, examples.")
            .with_mime_type("text/markdown");
        Ok(ListResourcesResult::with_all_items(vec![guide]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        if request.uri != guide::URI {
            return Err(ErrorData::resource_not_found(format!("There is no resource {}.", request.uri), None));
        }
        let contents = ResourceContents::text(guide::text(), guide::URI).with_mime_type("text/markdown");
        Ok(ReadResourceResponse::Complete(ReadResourceResult::new(vec![contents])))
    }
}
