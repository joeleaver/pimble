//! The tools (docs/MCP_CONTRACT.md "The tools"). Every write is an edit of the
//! node's existing document sent with `applyEdit`, or one of the RPCs that make
//! structural edits of node documents; nothing replaces a document wholesale.

use std::collections::HashSet;
use std::sync::Arc;

use base64::Engine;
use pimble_client::PimbleClient;
use pimble_core::{custom_keys, LinkResolution, PimbleUrl, Store, StoreAccess, StoreKind, SyncState};
use pimble_crdt::{CrdtError, NodeDoc};
use pimble_rpc::EditOperation;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::connection::Connection;
use crate::nodes::{self, label, Addr};

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
fenced code, bullet and numbered lists (nested), horizontal rules. Tables, block quotes, \
images, task lists, footnotes and HTML are refused with a sentence naming the line; nothing \
is written then. Link to another node with its pimble: link as the href.

Edits touch only what they name: append, insert_after (after a block, or after a heading's \
section), replace_section (the blocks under a heading), replace_text (an exact quote inside \
one paragraph). A quote must appear exactly once in the node. There is no whole-document \
rewrite. create_node takes the new node's first content. Deleting is undoable \
(undelete_node, and the app's Recently Deleted).";

pub struct Pimble {
    conn: Arc<Connection>,
    /// The client id every edit carries, so notifications say where it came from.
    client_id: String,
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
        Self {
            conn,
            client_id: format!("mcp:{}", uuid::Uuid::new_v4()),
            tool_router: Self::tool_router(),
        }
    }

    async fn client(&self) -> Result<Arc<PimbleClient>, String> {
        self.conn.client().await
    }

    /// Fetch the node's document, let `edit` change it, and send what changed as
    /// one `applyEdit`: the one way content and fields are written here.
    async fn edit(
        &self,
        client: &PimbleClient,
        addr: Addr,
        edit: impl FnOnce(&mut NodeDoc) -> Result<(), String>,
    ) -> Result<(), String> {
        let node = client.get_node(addr.store, addr.node).await.map_err(rpc)?;
        if node.access == StoreAccess::Read {
            return Err(StoreAccess::READ_ONLY_REFUSAL.to_string());
        }
        let mut doc = NodeDoc::load(&node.content).map_err(|e| format!("The node's document could not be read: {e}"))?;
        let before = doc.state_vector();
        edit(&mut doc)?;
        if doc.state_vector() == before {
            return Err("That would change nothing, so nothing was written.".into());
        }
        let delta = doc.diff_since(&before).map_err(|e| e.to_string())?;
        let changes = base64::engine::general_purpose::STANDARD.encode(delta);
        client
            .apply_edit(addr.store, addr.node, &self.client_id, EditOperation::IncrementalChanges { changes })
            .await
            .map_err(rpc)
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
pub struct HostedStoreParam {
    /// The hosted store's id, from list_hosted_stores.
    pub store_id: String,
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
    /// Where among the new parent's children (0 first); the end when left out.
    #[serde(default)]
    pub position: Option<usize>,
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
            node.node_type
        );
        if !node.metadata.tags.is_empty() {
            out.push_str(&format!("tags: {}\n", node.metadata.tags.join(", ")));
        }
        out.push_str(&format!("children: {}\n", node.children.len()));
        out.push_str(&format!("modified: {}\n", node.metadata.modified_at.to_rfc3339()));
        if node.access == StoreAccess::Read {
            out.push_str("access: read-only\n");
        }
        if node.metadata.custom.contains_key("mount") || node.node_type == "mount" {
            out.push_str("(a mount: its children are another store's; list_children shows them)\n");
        }
        out.push_str("---\n");
        out.push_str(if content.is_empty() { "(no content)" } else { &content });
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

    /// The signed-in Pimble Cloud account's hosted stores, and which are open here.
    #[tool(name = "list_hosted_stores")]
    async fn list_hosted_stores(&self) -> Result<String, String> {
        let client = self.client().await?;
        let status = client.cloud_status().await.map_err(rpc)?;
        if !status.signed_in {
            return Err("Sign in from Pimble's Account menu first.".into());
        }
        let open: HashSet<String> =
            client.list_stores().await.map_err(rpc)?.iter().map(|s| s.id.to_string()).collect();
        let hosted = client.cloud_list_hosted_stores().await.map_err(rpc)?;
        if hosted.is_empty() {
            return Ok("The account has no hosted stores.".into());
        }
        Ok(hosted
            .iter()
            .map(|h| {
                let here = if open.contains(&h.store_id) { "open here" } else { "not open here" };
                format!("- {} (id {}, {}, {here})\n", h.name, h.store_id, h.role)
            })
            .collect())
    }

    /// Open one of the account's hosted stores on this computer (it stays in step
    /// with Pimble Cloud, encrypted), and add it to the person's open stores.
    #[tool(name = "add_hosted_store")]
    async fn add_hosted_store(&self, Parameters(p): Parameters<HostedStoreParam>) -> Result<String, String> {
        let client = self.client().await?;
        let status = client.cloud_status().await.map_err(rpc)?;
        if !status.signed_in {
            return Err("Sign in from Pimble's Account menu first.".into());
        }
        let id = pimble_core::StoreId::parse(p.store_id.trim()).map_err(|_| format!("\"{}\" is not a store id.", p.store_id))?;
        let store = client.cloud_add_hosted_store(id).await.map_err(rpc)?;
        if let Some(path) = store.local_path() {
            if let Err(e) = pimble_server::local::add_open_store(&path.to_string_lossy()) {
                tracing::warn!("could not add {} to the saved store list: {e}", path.display());
            }
        }
        Ok(format!("Opened {} (id {}); it is syncing from Pimble Cloud.", store.name, store.id))
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
        let (node_id, left) = if parent.store == addr.store {
            let r = client.move_node(addr.store, addr.node, parent.node, p.position).await.map_err(rpc)?;
            (r.node_id, r.left_shares.len())
        } else {
            let r = client
                .transplant_node(addr.store, addr.node, parent.store, parent.node, p.position)
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
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("pimble", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}
