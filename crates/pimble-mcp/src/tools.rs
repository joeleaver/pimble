//! The tools (docs/MCP_CONTRACT.md "The tools"). Every write is an edit of the
//! node's existing document sent with `applyEdit`, or one of the RPCs that make
//! structural edits of node documents; nothing replaces a document wholesale.

use std::sync::Arc;

use pimble_api::names::label;
use pimble_api::rpc::RpcBackend;
use pimble_api::{Addr, ApiError, Edited, Pimble as Api};
use pimble_core::{Node, Store, StoreAccess, StoreId};
use pimble_crdt::{CrdtError, NodeDoc};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{
    Implementation, ListResourcesResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse,
    ReadResourceResult, Resource, ResourceContents, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{tool_handler, ErrorData, RoleServer, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::connection::{Connection, Shared};
use crate::sheets::guide;

mod canvas_tools;
mod discussion_tools;
mod metanode_tools;
mod node_tools;
mod read_tools;
mod sheet_tools;
mod text_edit_tools;
mod viewer_tools;
#[cfg(test)]
mod api_tests;
#[cfg(test)]
mod hashtags_tests;
#[cfg(test)]
mod internal_tests;
#[cfg(test)]
mod large_notes_tests;
#[cfg(test)]
mod presence_tests;
#[cfg(test)]
mod text_edit_tests;

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

Long notes: get_node with outline: true lists a note's headings (indented, each with its \
block number and its section's size) and section reads one heading's section, by some of its \
text or a heading path such as \"Design/The county's edges\" or \"Design > The county's \
edges\"; blocks reads a range of blocks (\"12-20\"). A whole note over 15,000 characters says \
so first. search finds notes by words or meaning; each hit's `in:` line is the heading path \
of where it matched, to pass to get_node as section, and a note whose title or a heading is \
exactly the query comes first. Without store, search looks in every open store and lists the \
hits of the store you last worked in first; pass store to search one. split_note splits a \
note by its headings into child notes, one per section, the note keeping its link and what \
comes before the first heading.

Content is Markdown: headings, paragraphs, bold, italic, strikethrough, inline code, links, \
fenced code, bullet and numbered lists (nested), block quotes, pipe tables, horizontal \
rules. Underline, highlight, text colour, subscript and superscript are the HTML tags <u>, \
<mark>, <span style=\"color:#rrggbb\">, <sub> and <sup>; a table with merged cells or blocks \
in a cell is an HTML <table>. Pictures are canvases: \
a picture already in Pimble is written as get_node shows it (![alt](pimble-blob:...#canvas=<id>)), so \
keep those lines when rewriting; insert_canvas puts a new picture or an empty canvas into a note. Other \
images, task lists, footnotes and any other HTML are refused with a sentence naming the line; nothing is \
written then. Link to another node with its pimble: link as the href. A mention of a \
person is a link to their account, [@Will](pimble-person:<account>): list_people says who can \
open a note, by the names they go by in its share, each with the Markdown that mentions them. \
Keep a mention's href as get_node shows it (its ?m= is the mention's own id, so rewriting a note \
does not mention anyone again); a new mention is written without one.

Edits touch only what they name: append, insert_after (after a block, or after a heading's \
section), replace_section (the blocks under a heading), replace_text (an exact quote inside \
one paragraph, plain text), replace_block (one block replaced, or removed with empty \
Markdown: a heading removed or re-levelled), remove_section (a heading and everything under \
it). Name a block by `quote` (some of its text, matched as text: \"13\" is the characters 1 and \
3, never block 13) or by `block_index` (its number, as get_node's outline and blocks print it); \
a heading by `heading` (as get_node's section names one) or by `block_index`. A quote that \
appears in several places is refused with every place listed; `occurrence` (1, 2, ... or \
\"last\") picks one. Removing a block by a quote under 12 characters, or one that reads like a \
number, is refused: use block_index. Every edit answers what it took out (the old text's first \
120 characters), its block numbers and its neighbors; `dry_run: true` answers the same and \
writes nothing. For restructuring, replace_content \
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
bold, italic, links and mentions (as in notes); your messages are signed with your agent's name.

A metanode is a node shown a particular way: it names which nodes it shows (its source, such as \
its children two levels deep) and the viewer that shows them (such as a kanban, whose columns are \
the first level and cards the second, in the tree's order). get_node says when a node is one, \
get_metanode lists what it shows, set_metanode sets or clears its viewer and source. A metanode can also have a query \
(set_metanode with query): it shows its own children and then the nodes its query finds, which live \
elsewhere. Moving, creating and editing the nodes it shows is done with the ordinary tools. \
list_viewers says, for every installed viewer, how it reads the tree, which node fields it reads (a \
timeline's date, a todo list's done) and what settings it takes, with their schema and an example; \
get_viewer_settings and set_viewer_settings read and write a metanode's settings (which calendar a \
timeline counts in, a calendar node's whole definition), checked against that schema. A plugin \
may also check what it alone understands: set_viewer_settings and set_fields ask it first, refuse a \
value it cannot use (a date its timeline cannot read) with its sentence, and say how a good one reads; \
call_plugin_tool runs its other tools (list_viewers lists them), such as trying a date in a calendar \
before writing many.

Canvases are infinite whiteboards (a canvas node) and the marks on pictures in a note's text (a \
picture's canvas, named by the note and the id after #canvas= in its URL). get_canvas lists a canvas's \
layers and elements with ids, positions and text; render_canvas shows it as an image; create_canvas makes \
a canvas node (empty or from a picture file); insert_canvas puts one into a note's text; add_elements \
draws sticky notes, text, shapes, frames, lines, arrows, connectors that follow the elements they join, \
pen strokes, numbered steps, callouts, node cards, pictures and groups in one edit; update_elements and \
delete_elements change them; combine_elements unites, subtracts, intersects or excludes shapes into one \
path; flip_elements mirrors; outline_stroke turns strokes into filled outlines; import_svg draws SVG markup or an .svg file; fills and strokes take linear \
and radial gradients (fill_gradient, stroke_gradient); paint paints brush strokes (round, pencil, ink, \
airbrush, marker, charcoal, watercolor, highlighter; with pressure; erasing) into paint layers, and \
highlights a picture; add_layer, update_layer and delete_layer manage layers; set_canvas sets a \
page and shows or hides the markup. Large canvases: get_canvas pages (offset, limit) and lists one layer \
(layer), and says each layer's size and the document's; get_elements gives elements whole (every point, \
path data, transform and style, as add_elements takes them; a paint layer's strokes and fills as paint \
takes them), export_canvas gives a canvas or layer as SVG, and copy_elements and copy_layer copy between \
canvases without passing the geometry through you. A paint layer of thousands of strokes makes the \
document large and slow everywhere: flatten_layer turns its history into pictures (in tiles, any size; \
scale sets the density), and painting answers say when a canvas passes 8 MB. Coordinates are canvas units, x right and y down; a picture at 100% \
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
    /// What a plugin's tools reach the stores through: this agent's own
    /// connection, so the server judges them by its grant. None for an API
    /// handed in (the presence tests), whose plugins then run no tools.
    tool_backend: Option<Arc<dyn pimble_api::Backend>>,
    /// The store this process last read or wrote in (any tool that named a
    /// node or a store in it): an unscoped search ranks its hits first
    /// (docs/MCP_CONTRACT.md, "search"). In memory only.
    last_store: std::sync::Mutex<Option<StoreId>>,
    tool_router: ToolRouter<Self>,
}

/// A library's or the API's refusal, as the sentence a tool answers with.
fn crdt(e: CrdtError) -> String {
    ApiError::from(e).sentence()
}

/// What get_node says of a note out of step on this device
/// (docs/CONVERGENCE_CONTRACT.md section 4): since when, in UTC, and why.
/// Every kind, the ones the app says only in Properties worded as there
/// (wave 3), with the day.
fn out_of_step_line(entry: &pimble_core::OutOfStep) -> String {
    let when = match entry.kind {
        pimble_core::OutOfStepKind::Waiting | pimble_core::OutOfStepKind::SetAside => entry.since.format("%Y-%m-%d").to_string(),
        _ => entry.since.format("%Y-%m-%d %H:%M UTC").to_string(),
    };
    entry.sentence(&when)
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
        let tool_backend: Arc<dyn pimble_api::Backend> = Arc::new(RpcBackend::new(Shared(conn.clone())));
        viewer_tools::warm();
        Self { tool_backend: Some(tool_backend), ..Self::with_api(Api::new(RpcBackend::new(Shared(conn)), client_id)) }
    }

    pub fn with_api(api: Api) -> Self {
        Self {
            api: Arc::new(api),
            tool_backend: None,
            last_store: std::sync::Mutex::new(None),
            tool_router: Self::read_router() + Self::node_router() + Self::metanode_router() + Self::sheet_router() + Self::discussion_router() + Self::canvas_router() + Self::viewer_router() + Self::text_edit_router(),
        }
    }

    /// The node a tool's argument names ([`Api::resolve`]); its store is then
    /// the one this session last worked in.
    async fn resolve(&self, name: &str) -> Result<Addr, ApiError> {
        let addr = self.api.resolve(name).await?;
        self.worked_in(addr.store);
        Ok(addr)
    }

    /// The store a tool's argument names ([`Api::find_store`]), which is then
    /// the one this session last worked in.
    async fn find_store(&self, name: &str) -> Result<Store, ApiError> {
        let store = self.api.find_store(name).await?;
        self.worked_in(store.id);
        Ok(store)
    }

    fn worked_in(&self, store: StoreId) {
        *self.last_store.lock().unwrap_or_else(|e| e.into_inner()) = Some(store);
    }

    fn last_store(&self) -> Option<StoreId> {
        *self.last_store.lock().unwrap_or_else(|e| e.into_inner())
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

    /// What get_node says of a node before its content: title, link, path,
    /// type, tags, fields, children, discussions, modified, access, and what
    /// kind of node it is when that matters.
    /// Why a node waits to sync, when its store's link holds it back: asked
    /// of the stores as they are now, not as last listed.
    /// And whether it is out of step on this device (docs/CONVERGENCE_CONTRACT.md
    /// section 4), since when and why.
    async fn sync_notes(&self, addr: Addr) -> (Option<String>, Option<pimble_core::OutOfStep>) {
        let Ok(stores) = self.api.stores().await else { return (None, None) };
        let Some(store) = stores.into_iter().find(|s| s.id == addr.store) else { return (None, None) };
        let waiting = store.waiting.into_iter().find(|w| w.node_id == addr.node).map(|w| w.reason);
        let out_of_step = store.out_of_step.into_iter().find(|o| o.node_id == addr.node);
        (waiting, out_of_step)
    }

    async fn node_header(&self, addr: Addr, node: &Node) -> String {
        let mut out = format!(
            "title: {}\nlink: {}\npath: {}\ntype: {}\n",
            label(node),
            addr.link(),
            self.api.path(addr).await,
            type_name(node)
        );
        if !node.metadata.tags.is_empty() {
            out.push_str(&format!("tags: {}\n", node.metadata.tags.join(", ")));
        }
        if !node.metadata.text_tags.is_empty() {
            let written: Vec<String> = node.metadata.text_tags.iter().map(|t| format!("#{t}")).collect();
            out.push_str(&format!("tags from the text: {}\n", written.join(", ")));
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
        // Held back by its store's link, and why (docs/SYNC_LIMITS_CONTRACT.md).
        let (waiting, out_of_step) = self.sync_notes(addr).await;
        if let Some(reason) = waiting {
            out.push_str(&format!("sync: waiting. {reason} (Edits here are kept on this computer and go out once it syncs.)\n"));
        }
        if let Some(entry) = out_of_step {
            out.push_str(&format!("sync: {}\n", out_of_step_line(&entry)));
        }
        match node.access {
            StoreAccess::Read => out.push_str("access: read-only\n"),
            StoreAccess::Comment => out.push_str("access: comment-only (start_discussion and reply work here)\n"),
            StoreAccess::Full => {}
        }
        if let Some(metanode) = &node.metadata.metanode {
            // docs/METANODES_CONTRACT.md section 3: what it shows, and how.
            out.push_str(&format!(
                "({}; get_metanode lists what it shows; list_viewers says what its viewer reads and takes; get_viewer_settings shows its settings)\n",
                metanode.describe(&label(node))
            ));
            if let Some(why) = metanode.source.not_understood() {
                out.push_str(&format!("({why})\n"));
            }
        }
        if node.metadata.custom.contains_key("mount") || node.node_type == "mount" {
            out.push_str("(a mount: its children are another store's; list_children shows them)\n");
        }
        out
    }
}

// ── Parameters shared by the tool modules ─────────────────────────────────

#[derive(Deserialize, JsonSchema)]
pub struct NodeParam {
    /// The node: a pimble: link, a node id, or a path like "Store/Folder/Note".
    pub node: String,
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

#[cfg(test)]
mod out_of_step_tests {
    /// get_node's line for a note out of step: since when, and the reason the
    /// app shows on the note.
    #[test]
    fn get_node_says_since_when_and_why() {
        let since = chrono::DateTime::parse_from_rfc3339("2026-11-19T14:05:00Z").unwrap().with_timezone(&chrono::Utc);
        let behind = "Will's device holds changes this one doesn't have yet. They'll arrive when Will opens Pimble.";
        let entry = pimble_core::OutOfStep { node_id: pimble_core::NodeId::new(), since, reason: behind.into(), kind: pimble_core::OutOfStepKind::Behind };
        assert_eq!(super::out_of_step_line(&entry), format!("Out of step since 2026-11-19 14:05 UTC: {behind}"));
        // What nobody can act on: worded as Properties words it.
        let waiting = pimble_core::OutOfStep { reason: pimble_rpc::vault::WAITING_REASON.into(), kind: pimble_core::OutOfStepKind::Waiting, ..entry.clone() };
        assert_eq!(super::out_of_step_line(&waiting), "Some changes from another device never arrived (waiting since 2026-11-19).");
        let aside = pimble_core::OutOfStep { reason: pimble_rpc::vault::SET_ASIDE_REASON.into(), kind: pimble_core::OutOfStepKind::SetAside, ..entry };
        assert_eq!(super::out_of_step_line(&aside), "Some changes from another device never arrived and were set aside on 2026-11-19.");
    }
}
