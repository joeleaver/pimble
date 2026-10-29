//! The tools (docs/MCP_CONTRACT.md "The tools"). Every write is an edit of the
//! node's existing document sent with `applyEdit`, or one of the RPCs that make
//! structural edits of node documents; nothing replaces a document wholesale.

use std::collections::HashSet;
use std::sync::Arc;

use base64::Engine;
use pimble_client::PimbleClient;
use pimble_core::{custom_keys, LinkResolution, PimbleUrl, Place, Store, StoreAccess, StoreKind, SyncState};
use pimble_crdt::{CrdtError, NodeDoc};
use pimble_rpc::EditOperation;
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

use crate::connection::Connection;
use crate::nodes::{self, label, Addr};
use crate::presence::Presence;
use crate::sheets::guide;

mod canvas_tools;
mod discussion_tools;
#[cfg(test)]
mod presence_tests;
mod sheet_tools;

const INSTRUCTIONS: &str = "\
Pimble is the person's personal information manager: stores hold trees of nodes, each node \
has a title, tags and rich-text content. These tools read, search and write the stores open \
on the person's computer, as a collaborator: edits merge with whatever the person is typing.

Naming a node: every `node`/`parent` argument takes a pimble: link (what Pimble's \"Copy \
Link\" gives the person, and what every tool answer shows), a bare node id, or a path of \
titles from the store's name such as \"Family Management/MEDICAL\". When a name is ambiguous \
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
    conn: Arc<Connection>,
    /// The client id every edit carries, so notifications say where it came from.
    client_id: String,
    /// Where this agent is working, said to everyone (docs/PRESENCE_CONTRACT.md
    /// section 8).
    presence: Presence,
    tool_router: ToolRouter<Self>,
}

fn rpc(e: impl std::fmt::Display) -> String {
    let message = e.to_string();
    match StoreAccess::refusal_in(&message) {
        Some(sentence) => sentence.to_string(),
        None => message,
    }
}

fn crdt(e: CrdtError) -> String {
    match e {
        CrdtError::Refused(sentence) => sentence,
        other => format!("The edit could not be made: {other}"),
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

impl Pimble {
    pub fn new(conn: Arc<Connection>) -> Self {
        // An agent's edits are attributed to it (docs/AGENTS_CONTRACT.md).
        let client_id = match crate::connection::agent_id() {
            Some(id) => format!("agent:{id}"),
            None => format!("mcp:{}", uuid::Uuid::new_v4()),
        };
        Self::with_presence(conn, client_id.clone(), Presence::new(client_id))
    }

    pub fn with_presence(conn: Arc<Connection>, client_id: String, presence: Presence) -> Self {
        Self {
            conn,
            client_id,
            presence,
            tool_router: Self::tool_router() + Self::sheet_router() + Self::discussion_router() + Self::canvas_router(),
        }
    }

    async fn client(&self) -> Result<Arc<PimbleClient>, String> {
        self.conn.client().await
    }

    /// Say this agent is in `addr` (a tool read it), keeping its caret there.
    async fn present(&self, client: &PimbleClient, addr: Addr) {
        self.presence.say(client, addr, None).await;
    }

    /// Fetch the node's document, let `edit` change it, and send what changed as
    /// one `applyEdit`: the one way content and fields are written here. Then
    /// the agent is present there, its caret at the end of what it wrote, and
    /// in the node a discussion is on.
    async fn edit(
        &self,
        client: &PimbleClient,
        addr: Addr,
        edit: impl FnOnce(&mut NodeDoc) -> Result<(), String>,
    ) -> Result<(), String> {
        let (caret, host) = self.edit_doc(client, addr, edit).await?;
        self.presence.say(client, addr, caret).await;
        if let Some(host) = host {
            self.present(client, Addr { store: addr.store, node: host }).await;
        }
        Ok(())
    }

    /// The write itself, saying nothing: the caret after it (a sticky index
    /// into the content) and the node a discussion is on.
    async fn edit_doc(
        &self,
        client: &PimbleClient,
        addr: Addr,
        edit: impl FnOnce(&mut NodeDoc) -> Result<(), String>,
    ) -> Result<(Option<Vec<u8>>, Option<pimble_core::NodeId>), String> {
        let node = client.get_node(addr.store, addr.node).await.map_err(rpc)?;
        if node.access == StoreAccess::Read {
            return Err(StoreAccess::READ_ONLY_REFUSAL.to_string());
        }
        let unreadable = |e: CrdtError| format!("The node's document could not be read: {e}");
        let mut doc = NodeDoc::load(&node.content).map_err(unreadable)?;
        let before = doc.state_vector();
        let text_before = doc.textblock_texts();
        edit(&mut doc)?;
        let caret = doc.sticky_after_change(&text_before);
        let delta = doc.diff_since(&before).map_err(|e| e.to_string())?;
        // Whether the edit changed anything is what merging it into the document as it
        // was says, never the state vector: a deletion alone does not move it.
        let mut untouched = NodeDoc::load(&node.content).map_err(unreadable)?;
        if !untouched.apply_update(&delta).map_err(|e| e.to_string())?.changed {
            return Err("That would change nothing, so nothing was written.".into());
        }
        let changes = base64::engine::general_purpose::STANDARD.encode(delta);
        client
            .apply_edit(addr.store, addr.node, &self.client_id, EditOperation::IncrementalChanges { changes })
            .await
            .map_err(rpc)?;
        Ok((caret, node.attached_to))
    }

    async fn describe(&self, client: &PimbleClient, stores: &[Store], addr: Addr) -> String {
        format!("{} ({})", nodes::path_of(client, stores, addr).await, addr.link())
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
pub struct MoveNodeParams {
    /// The node to move.
    pub node: String,
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
        let client = self.client().await?;
        let stores = client.list_stores().await.map_err(rpc)?;
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
        let client = self.client().await?;
        let depth = p.depth.unwrap_or(1).clamp(1, 6);
        let stores = client.list_stores().await.map_err(rpc)?;
        // A store with several shared roots lists them as its top level.
        let tops = match nodes::find_store(&stores, &p.node) {
            Ok(store) if !store.roots.is_empty() => nodes::store_tops(store),
            _ => vec![nodes::resolve(&client, &p.node).await?],
        };
        let mut out = String::new();
        let mut seen = HashSet::new();
        for top in tops {
            if !store_listing_is_top(&stores, top) {
                out.push_str(&format!("{}:\n", self.describe(&client, &stores, top).await));
            }
            outline(&client, top, depth, 0, &mut seen, &mut out).await?;
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
        let client = self.client().await?;
        let stores = client.list_stores().await.map_err(rpc)?;
        let wanted = p.title.trim().to_lowercase();
        if wanted.is_empty() {
            return Err("Give some of the title to look for.".into());
        }
        let searched: Vec<&Store> = match &p.store {
            Some(name) => vec![nodes::find_store(&stores, name)?],
            None => stores.iter().collect(),
        };
        // (rank, path, link): 0 exact, 1 prefix, 2 contains.
        let mut hits: Vec<(u8, String, String)> = Vec::new();
        let mut seen = HashSet::new();
        for store in searched {
            let mut queue: Vec<(Addr, String)> =
                nodes::store_tops(store).into_iter().map(|a| (a, store.name.clone())).collect();
            while let Some((at, path)) = queue.pop() {
                if !seen.insert(at) {
                    continue;
                }
                let Ok(children) = nodes::children(&client, at).await else { continue };
                for (addr, node) in children {
                    let title = label(&node);
                    let child_path = format!("{path}/{title}");
                    let lower = title.to_lowercase();
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
                        hits.push((rank, child_path.clone(), addr.link()));
                    }
                    queue.push((addr, child_path));
                }
            }
        }
        if hits.is_empty() {
            return Ok(format!("No node's title contains \"{}\".", p.title.trim()));
        }
        hits.sort();
        let total = hits.len();
        let mut out: String =
            hits.iter().take(30).map(|(_, path, link)| format!("- {path} ({link})\n")).collect();
        if total > 30 {
            out.push_str(&format!("... and {} more; be more specific.\n", total - 30));
        }
        Ok(out)
    }

    /// A node: its link, path, title, tags, number of children, and content as
    /// Markdown.
    #[tool(name = "get_node")]
    async fn get_node(&self, Parameters(p): Parameters<NodeParam>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        let stores = client.list_stores().await.map_err(rpc)?;
        let node = client.get_node(addr.store, addr.node).await.map_err(rpc)?;
        self.present(&client, addr).await;
        let doc = NodeDoc::load(&node.content).map_err(|e| e.to_string())?;
        let content = match doc.markdown() {
            Ok(md) => md,
            Err(_) => format!("(formatting left out: this content cannot be shown as Markdown)\n\n{}", doc.text()),
        };
        let mut out = format!(
            "title: {}\nlink: {}\npath: {}\ntype: {}\n",
            label(&node),
            addr.link(),
            nodes::path_of(&client, &stores, addr).await,
            nodes::type_name(&node)
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
        if node.access == StoreAccess::Read {
            out.push_str("access: read-only\n");
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
            out.push_str(&self.canvas_body(&client, &node, None).await);
            None
        } else if node.node_type == pimble_core::node_types::SHEET {
            out.push_str("(a sheet node: its content is its tables)\n");
            self.tables_of_node(&client, addr, &node).await.map(|t| ("Tables", t))
        } else {
            // A text node's tables are shown where they are in its text.
            let content = self.inline_discussions(&client, &content).await;
            let (content, lost) = self.tables_in_text(&client, addr, &node, &content).await;
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
        let client = self.client().await?;
        let stores = client.list_stores().await.map_err(rpc)?;
        let only = match &p.store {
            Some(name) => vec![nodes::find_store(&stores, name)?.id],
            None => Vec::new(),
        };
        let limit = p.limit.unwrap_or(20).clamp(1, 50);
        let results = client.search(p.query.clone(), only, p.semantic, limit).await.map_err(rpc)?;
        if results.is_empty() {
            return Ok(format!("Nothing matches \"{}\".", p.query));
        }
        let mut out = String::new();
        for r in results {
            let addr = Addr { store: r.store_id, node: r.node_id };
            let path = nodes::path_of(&client, &stores, addr).await;
            let snippet: String = r.snippet.split_whitespace().collect::<Vec<_>>().join(" ");
            out.push_str(&format!("- {path} ({})\n  {snippet}\n", addr.link()));
        }
        Ok(out)
    }

    /// Where a pimble: link points now (a node that moved out of a share or store
    /// is followed to where it went).
    #[tool(name = "resolve_link")]
    async fn resolve_link(&self, Parameters(p): Parameters<LinkParam>) -> Result<String, String> {
        let client = self.client().await?;
        let url = PimbleUrl::parse(p.link.trim()).ok_or_else(|| format!("\"{}\" is not a Pimble link.", p.link))?;
        let stores = client.list_stores().await.map_err(rpc)?;
        let resolution = client.resolve_link(url.store, url.node).await.map_err(rpc)?;
        Ok(match resolution {
            LinkResolution::Live { store_id, node_id } => {
                let addr = Addr { store: store_id, node: node_id };
                format!("Live: {}", self.describe(&client, &stores, addr).await)
            }
            LinkResolution::Deleted { store_id, node_id } => {
                format!("Deleted (undelete_node can put it back): {}", Addr { store: store_id, node: node_id }.link())
            }
            other => other.sentence().unwrap_or("The link leads nowhere.").to_string(),
        })
    }

    /// A store's recently deleted nodes, which undelete_node can put back.
    #[tool(name = "list_deleted")]
    async fn list_deleted(&self, Parameters(p): Parameters<StoreParam>) -> Result<String, String> {
        let client = self.client().await?;
        let stores = client.list_stores().await.map_err(rpc)?;
        let store = nodes::find_store(&stores, &p.store)?;
        let deleted = client.list_deleted(store.id).await.map_err(rpc)?;
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
        let client = self.client().await?;
        let parent = nodes::resolve(&client, &p.parent).await?;
        // A node created under a mount lives in the mount's source store.
        let parent_node = client.get_node(parent.store, parent.node).await.map_err(rpc)?;
        let (store, parent_id) = match parent_node.mount_ref() {
            Some(mount) => (mount.source_store, mount.source_node),
            None => (parent.store, parent.node),
        };
        if let Some(md) = &p.markdown {
            pimble_crdt::markdown::check(md).map_err(crdt)?;
        }
        let node_id = client.create_node(store, Some(parent_id), "document", p.title.clone()).await.map_err(rpc)?;
        let addr = Addr { store, node: node_id };
        let markdown = p.markdown.clone();
        self.edit(&client, addr, move |doc| {
            doc.set_custom(custom_keys::EXPLICIT_TITLE, &serde_json::json!(true), &now()).map_err(crdt)?;
            if let Some(md) = markdown.as_deref().filter(|md| !md.trim().is_empty()) {
                doc.write_markdown(md).map_err(crdt)?;
            }
            Ok(())
        })
        .await?;
        let stores = client.list_stores().await.map_err(rpc)?;
        Ok(format!("Created {}", self.describe(&client, &stores, addr).await))
    }

    /// Append Markdown after the node's content.
    #[tool(name = "append")]
    async fn append(&self, Parameters(p): Parameters<AppendParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        self.edit(&client, addr, |doc| doc.append_markdown(&p.markdown).map(drop).map_err(crdt)).await?;
        Ok(format!("Appended to {}.", addr.link()))
    }

    /// Insert Markdown after the block a quote names, or after a heading's whole
    /// section.
    #[tool(name = "insert_after")]
    async fn insert_after(&self, Parameters(p): Parameters<InsertAfterParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        self.edit(&client, addr, |doc| {
            doc.insert_markdown_after(&p.after, p.after_section, &p.markdown).map(drop).map_err(crdt)
        })
        .await?;
        Ok(format!("Inserted into {}.", addr.link()))
    }

    /// Replace the blocks under a heading (up to the next heading of its level or
    /// higher); the heading stays.
    #[tool(name = "replace_section")]
    async fn replace_section(&self, Parameters(p): Parameters<ReplaceSectionParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        self.edit(&client, addr, |doc| doc.replace_section_markdown(&p.heading, &p.markdown).map(drop).map_err(crdt))
            .await?;
        Ok(format!("Replaced the section under \"{}\" in {}.", p.heading, addr.link()))
    }

    /// Replace an exact quote inside one paragraph with new text.
    #[tool(name = "replace_text")]
    async fn replace_text(&self, Parameters(p): Parameters<ReplaceTextParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        self.edit(&client, addr, |doc| doc.replace_text(&p.quote, &p.text).map(drop).map_err(crdt)).await?;
        Ok(format!("Replaced the text in {}.", addr.link()))
    }

    /// Replace one block (a heading, a paragraph, a list, a code block) with
    /// Markdown, or remove it with empty Markdown: removing or re-levelling a heading,
    /// turning a paragraph into a list.
    #[tool(name = "replace_block")]
    async fn replace_block(&self, Parameters(p): Parameters<ReplaceBlockParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        self.edit(&client, addr, |doc| doc.replace_block_markdown(&p.block, &p.markdown).map(drop).map_err(crdt))
            .await?;
        let did = if p.markdown.trim().is_empty() { "Removed" } else { "Replaced" };
        Ok(format!("{did} the block in {}.", addr.link()))
    }

    /// Remove a heading and everything under it, up to the next heading of its level
    /// or higher.
    #[tool(name = "remove_section")]
    async fn remove_section(&self, Parameters(p): Parameters<RemoveSectionParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        self.edit(&client, addr, |doc| doc.remove_section(&p.heading).map(drop).map_err(crdt)).await?;
        Ok(format!("Removed the section \"{}\" from {}.", p.heading, addr.link()))
    }

    /// Replace the node's whole content, for restructuring. Only what differs is
    /// written: blocks that read the same stay untouched (and so does anyone typing in
    /// them), and the node keeps its link. Prefer the smaller edits for small changes.
    #[tool(name = "replace_content")]
    async fn replace_content(&self, Parameters(p): Parameters<ReplaceContentParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        self.edit(&client, addr, |doc| doc.replace_content_markdown(&p.markdown).map(drop).map_err(crdt)).await?;
        Ok(format!("Replaced the content of {}.", addr.link()))
    }

    /// Rename a node.
    #[tool(name = "rename")]
    async fn rename(&self, Parameters(p): Parameters<RenameParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        let title = p.title.trim().to_string();
        if title.is_empty() {
            return Err("A title cannot be empty.".into());
        }
        self.edit(&client, addr, |doc| {
            let at = now();
            doc.set_title(&title, &at).map_err(crdt)?;
            doc.set_custom(custom_keys::EXPLICIT_TITLE, &serde_json::json!(true), &at).map_err(crdt)
        })
        .await?;
        Ok(format!("Renamed {} to \"{title}\".", addr.link()))
    }

    /// Set a node's tags (replacing the ones it has).
    #[tool(name = "set_tags")]
    async fn set_tags(&self, Parameters(p): Parameters<SetTagsParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        let tags: Vec<String> = p.tags.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
        self.edit(&client, addr, |doc| doc.set_tags(&tags, &now()).map_err(crdt)).await?;
        Ok(format!("Tagged {}: {}.", addr.link(), if tags.is_empty() { "(none)".into() } else { tags.join(", ") }))
    }

    /// Move a node under a new parent, in its store or into another one. A move
    /// out of a share or between stores gives the node a new link, which the answer
    /// names.
    #[tool(name = "move_node")]
    async fn move_node(&self, Parameters(p): Parameters<MoveNodeParams>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        let parent = nodes::resolve(&client, &p.new_parent).await?;
        let place = match (&p.after, p.first) {
            (Some(_), true) => return Err("Give `after` or `first`, not both.".into()),
            (None, true) => Place::First,
            (None, false) => Place::Last,
            (Some(after), false) => {
                let after = nodes::resolve(&client, after).await?;
                if after.store != parent.store {
                    return Err("`after` must be a child of `new_parent`.".into());
                }
                Place::After(after.node)
            }
        };
        let (node_id, left) = if parent.store == addr.store {
            let r = client.move_node(addr.store, addr.node, parent.node, place).await.map_err(rpc)?;
            (r.node_id, r.left_shares.len())
        } else {
            let r = client
                .transplant_node(addr.store, addr.node, parent.store, parent.node, place)
                .await
                .map_err(rpc)?;
            (r.node_id, r.left_shares.len())
        };
        let now_at = Addr { store: parent.store, node: node_id };
        let stores = client.list_stores().await.map_err(rpc)?;
        let mut out = format!("Moved to {}", self.describe(&client, &stores, now_at).await);
        if now_at != addr {
            out.push_str(&format!(". It has a new link; the old one ({}) now leads here.", addr.link()));
        }
        if left > 0 {
            out.push_str(&format!(" It left {left} share(s): members there see it as deleted."));
        }
        Ok(out)
    }

    /// Delete a node and everything under it. Undoable: undelete_node, or the app's
    /// Recently Deleted.
    #[tool(name = "delete_node")]
    async fn delete_node(&self, Parameters(p): Parameters<NodeParam>) -> Result<String, String> {
        let client = self.client().await?;
        let addr = nodes::resolve(&client, &p.node).await?;
        client.delete_node(addr.store, addr.node).await.map_err(rpc)?;
        Ok(format!("Deleted {} (undelete_node puts it back).", addr.link()))
    }

    /// Put a deleted node back where it was.
    #[tool(name = "undelete_node")]
    async fn undelete_node(&self, Parameters(p): Parameters<NodeParam>) -> Result<String, String> {
        let client = self.client().await?;
        let url = PimbleUrl::parse(p.node.trim());
        let addr = match url {
            Some(url) => Addr { store: url.store, node: url.node },
            None => nodes::resolve(&client, &p.node).await?,
        };
        client.undelete_node(addr.store, addr.node).await.map_err(rpc)?;
        let stores = client.list_stores().await.map_err(rpc)?;
        Ok(format!("Put back {}", self.describe(&client, &stores, addr).await))
    }
}

/// Whether listing `top` is listing a store's single root (so no header line).
fn store_listing_is_top(stores: &[Store], top: Addr) -> bool {
    stores.iter().any(|s| s.id == top.store && s.root_node_id == top.node)
}

/// An indented outline of `at`'s subtree, `depth` levels deep.
fn outline<'a>(
    client: &'a PimbleClient,
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
        for (addr, node) in nodes::children(client, at).await? {
            let count = node.children.len();
            let mount = if node.node_type == "mount" { ", a mount" } else { "" };
            let children = match count {
                0 => String::new(),
                1 => ", 1 child".into(),
                n => format!(", {n} children"),
            };
            out.push_str(&format!("{}- {} ({}{children}{mount})\n", "  ".repeat(indent), label(&node), addr.link()));
            if depth > 1 && (count > 0 || node.node_type == "mount") {
                outline(client, addr, depth - 1, indent + 1, seen, out).await?;
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
            self.presence.set_device(app.title.as_deref().unwrap_or(&app.name));
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
