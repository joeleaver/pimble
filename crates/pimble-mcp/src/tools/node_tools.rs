//! The tools that make, name and place nodes: create, split, rename, tag,
//! move, delete and put back.


use pimble_api::{recipes, Addr};
use pimble_core::{PimbleUrl, Place};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;


use super::{crdt, NodeParam, Pimble};

#[derive(Deserialize, JsonSchema)]
pub struct SplitNoteParams {
    /// The text note to split.
    pub node: String,
    /// The heading level to split at (1 for "#", 2 for "##", ...). Leave it out
    /// for the highest level the note has.
    #[serde(default)]
    pub level: Option<u8>,
    /// Leave a list of links to the new notes in the note, where the first
    /// section was.
    #[serde(default)]
    pub links: bool,
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
    /// The node's tags, replacing the ones it has. A #hashtag in its text left
    /// out of this list is removed from the text.
    pub tags: Vec<String>,
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

#[tool_router(router = node_router, vis = "pub(super)")]
impl Pimble {
    /// Create a node under `parent`, with a title and optionally its first content.
    #[tool(name = "create_node")]
    pub(super) async fn create_node(&self, Parameters(p): Parameters<CreateNodeParams>) -> Result<String, String> {
        let parent = self.resolve(&p.parent).await?;
        // Made with its content in the one createNode (a node under a mount
        // lives in the mount's source store).
        let init = recipes::document(&p.title, p.markdown.as_deref())?;
        let addr = self.api.create(parent, Place::Last, init).await?;
        self.present(addr).await;
        Ok(format!("Created {}", self.describe(addr).await))
    }

    /// Split a long text note by its headings of one level (the highest level
    /// it has, unless `level` says): each section becomes a child note named by
    /// its heading and holding what was under it, in order after the note's
    /// other children; the note keeps what comes before the first heading (and
    /// anything between sections of a higher level), and with `links` a list
    /// of links to the new notes where the first section was. The note keeps
    /// its link; each new note has its own.
    #[tool(name = "split_note")]
    pub(super) async fn split_note(&self, Parameters(p): Parameters<SplitNoteParams>) -> Result<String, String> {
        let addr = self.resolve(&p.node).await?;
        if p.level == Some(0) {
            return Err("`level` is 1 for \"#\", 2 for \"##\", and so on.".into());
        }
        let split = self.api.split_by_headings(addr, p.level.map(i64::from), p.links).await?;
        self.present(addr).await;
        let mut out = format!(
            "Split {} at its level-{} headings into {} notes under it{}:\n",
            self.describe(addr).await,
            split.level,
            split.children.len(),
            if p.links { ", with links to them where the first section was" } else { "" }
        );
        for (child, title) in &split.children {
            out.push_str(&format!("- {title} ({})\n", child.link()));
        }
        Ok(out)
    }

    /// Rename a node.
    #[tool(name = "rename")]
    pub(super) async fn rename(&self, Parameters(p): Parameters<RenameParams>) -> Result<String, String> {
        let addr = self.resolve(&p.node).await?;
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

    /// Set a node's tags (replacing the ones it has). A node's tags are the
    /// ones entered for it and the #hashtags in its text: get_node lists both.
    /// A #hashtag kept in the list stays in the text and is not entered again;
    /// one left out of the list is removed from the text, every occurrence.
    #[tool(name = "set_tags")]
    pub(super) async fn set_tags(&self, Parameters(p): Parameters<SetTagsParams>) -> Result<String, String> {
        let addr = self.resolve(&p.node).await?;
        let tags: Vec<String> = p.tags.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
        // The entered tags; a #hashtag of the text left out of the list is
        // removed from the text (docs/HASHTAGS_CONTRACT.md).
        let mut removed: Vec<String> = Vec::new();
        self.edit(addr, |doc| {
            let before = doc.text_tags();
            doc.set_all_tags(&tags, &pimble_api::now()).map_err(crdt)?;
            let after = doc.text_tags();
            removed = before.into_iter().filter(|t| !after.iter().any(|a| pimble_crdt::hashtags::same_tag(a, t))).collect();
            Ok(())
        })
        .await?;
        let mut out = format!("Tagged {}: {}.", addr.link(), if tags.is_empty() { "(none)".into() } else { tags.join(", ") });
        if !removed.is_empty() {
            let written: Vec<String> = removed.iter().map(|t| format!("#{t}")).collect();
            out.push_str(&format!(" Removed from the text: {}.", written.join(", ")));
        }
        Ok(out)
    }

    /// Move a node (or several, with `nodes`) under a new parent, in its store or
    /// into another one. Judged first, as Pimble's explorer judges a drop: a node
    /// cannot go inside itself, nor where you may only read. A move out of a share
    /// or between stores gives the node a new link, which the answer names.
    #[tool(name = "move_node")]
    pub(super) async fn move_node(&self, Parameters(p): Parameters<MoveNodeParams>) -> Result<String, String> {
        let mut nodes = Vec::new();
        for name in p.node.iter().chain(&p.nodes) {
            nodes.push(self.resolve(name).await?);
        }
        if nodes.is_empty() {
            return Err("Name the node to move (`node`, or `nodes` for several).".into());
        }
        let target = self.resolve(&p.new_parent).await?;
        let place = match (&p.after, p.first) {
            (Some(_), true) => return Err("Give `after` or `first`, not both.".into()),
            (None, true) => Place::First,
            (None, false) => Place::Last,
            (Some(after), false) => Place::After(self.resolve(after).await?.node),
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
    pub(super) async fn delete_node(&self, Parameters(p): Parameters<NodeParam>) -> Result<String, String> {
        let addr = self.resolve(&p.node).await?;
        self.api.can_delete(&[addr]).await?;
        self.api.delete(addr).await?;
        Ok(format!("Deleted {} (undelete_node puts it back).", addr.link()))
    }

    /// Put a deleted node back where it was.
    #[tool(name = "undelete_node")]
    pub(super) async fn undelete_node(&self, Parameters(p): Parameters<NodeParam>) -> Result<String, String> {
        // A link to a deleted node names it as it is, not where it would lead.
        let addr = match PimbleUrl::parse(p.node.trim()) {
            Some(url) => Addr { store: url.store, node: url.node },
            None => self.resolve(&p.node).await?,
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
