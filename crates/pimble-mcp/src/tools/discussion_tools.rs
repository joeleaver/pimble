//! The discussion tools (docs/DISCUSSIONS_CONTRACT.md section 9). Every write
//! is one operation of `pimble_discussion` on the discussion node's document,
//! sent through `applyEdit` (`Pimble::edit`), or the create that makes a
//! discussion with its first message. An agent's messages are stamped with
//! the agent's id and name (contract section 3), and an agent edits and
//! deletes only its own, as the app offers a person only theirs.


use pimble_api::recipes::{self, FirstMessage};
use pimble_api::{label, Addr};
use pimble_core::{node_types, Node};
use pimble_crdt::markdown::{embed_attrs, Embed};
use pimble_crdt::NodeDoc;
use pimble_discussion::{Author, Body, Discussion, DiscussionSnapshot, Stamp};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use super::Pimble;

/// A message's time: RFC 3339 UTC with milliseconds, the one shape every
/// writer uses (messages sort by it as a string, as the app writes it).
fn now() -> String {
    pimble_api::now()
}

#[derive(Deserialize, JsonSchema)]
pub struct ListDiscussionsParams {
    /// The node whose discussions to list: a pimble: link, a node id, or a path.
    pub node: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct DiscussionParam {
    /// The discussion: its pimble: link or node id (list_discussions shows them).
    pub discussion: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct StartDiscussionParams {
    /// Start it on this node (it then belongs to the node and goes where the
    /// node goes). Give this or `parent`.
    #[serde(default)]
    pub on: Option<String>,
    /// Or make it a discussion node of its own, a row under this parent.
    #[serde(default)]
    pub parent: Option<String>,
    /// Its topic (the discussion node's title); may be empty.
    #[serde(default)]
    pub topic: String,
    /// The first message, in Markdown: **bold**, *italic*, [text](url), and
    /// pimble: links to nodes.
    pub message: String,
    /// With `on` (a text node): a few words of its text (read once, as
    /// replace_text quotes) where the discussion is anchored. Left out, it is
    /// on the node as a whole.
    #[serde(default)]
    pub after_text: Option<String>,
    /// With `on`: also show the discussion in the node's text, after the
    /// block `after_text` names (at the end when left out). This edits the
    /// node's text.
    #[serde(default)]
    pub inline: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReplyParams {
    /// The discussion: its pimble: link or node id.
    pub discussion: String,
    /// The id of the message replied to (read_discussion shows ids); at the
    /// top level when left out.
    #[serde(default)]
    pub reply_to: Option<String>,
    /// The message, in Markdown (as start_discussion).
    pub message: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct EditMessageParams {
    /// The discussion: its pimble: link or node id.
    pub discussion: String,
    /// The id of one of your own messages.
    pub message_id: String,
    /// Its new text, in Markdown.
    pub message: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct DeleteMessageParams {
    /// The discussion: its pimble: link or node id.
    pub discussion: String,
    /// The id of one of your own messages.
    pub message_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ResolveParams {
    /// The discussion: its pimble: link or node id.
    pub discussion: String,
    /// Open it again instead.
    #[serde(default)]
    pub reopen: bool,
}

fn snapshot_of(node: &Node) -> Result<DiscussionSnapshot, String> {
    let doc = NodeDoc::load(&node.content).map_err(|e| format!("The discussion's document could not be read: {e}"))?;
    Ok(Discussion::of_node(&doc).read())
}

fn topic_of(node: &Node) -> String {
    if node.metadata.title.trim().is_empty() {
        "(no topic)".to_string()
    } else {
        node.metadata.title.clone()
    }
}

/// One line about a discussion for a listing.
fn summary(addr: Addr, node: &Node) -> String {
    let snap = snapshot_of(node).unwrap_or_default();
    let mut line = format!("- {} ({}): {} message(s)", topic_of(node), addr.link(), snap.live_count());
    if snap.resolved.is_some() {
        line.push_str(", resolved");
    }
    if let Some(latest) = snap.latest() {
        let who = latest.author.as_ref().map(|a| a.name.as_str()).filter(|n| !n.is_empty()).unwrap_or("someone");
        let mut text = latest.body.text();
        if text.chars().count() > 80 {
            text = text.chars().take(80).collect::<String>() + "…";
        }
        line.push_str(&format!("; latest from {who} at {}: {text}", latest.created));
    }
    line
}

impl Pimble {
    /// `markdown` with each discussion shown inline replaced by a line
    /// naming it (get_node; read_discussion reads it).
    pub(super) async fn inline_discussions(&self, markdown: &str) -> String {
        let mut out = String::new();
        for line in markdown.split_inclusive('\n') {
            let Some(url) = Embed::from_markdown_line(line).and_then(|e| e.node_of_kind(embed_attrs::KIND_DISCUSSION).cloned()) else {
                out.push_str(line);
                continue;
            };
            let at = Addr { store: url.store, node: url.node };
            match self.api.node(at).await {
                Ok(n) => out.push_str(&format!("(the discussion {} is shown here: {})\n", topic_of(&n), summary(at, &n).trim_start_matches("- "))),
                Err(_) => out.push_str(&format!("(a discussion shown here could not be read: {})\n", at.link())),
            }
        }
        out
    }

    /// A discussion node's thread, for get_node.
    pub(super) fn discussion_body(node: &Node) -> String {
        match snapshot_of(node) {
            Ok(snap) if !snap.thread().is_empty() => snap.to_markdown(),
            Ok(_) => "No messages yet.\n".to_string(),
            Err(e) => e,
        }
    }

    /// What this agent's messages are stamped with: its id and name, as the
    /// server knows them (asked once).
    async fn author(&self) -> Option<Author> {
        self.api.me().await.map(|me| me.discussion_author("AI agent"))
    }

    /// The discussion named `name`: its address and node, refused when it is
    /// not a discussion.
    async fn discussion(&self, name: &str) -> Result<(Addr, Node), String> {
        let addr = self.api.resolve(name).await?;
        let node = self.api.node(addr).await?;
        if node.node_type != node_types::DISCUSSION {
            return Err(format!("{} is not a discussion; list_discussions names a node's discussions.", label(&node)));
        }
        Ok((addr, node))
    }

    /// One discussion write through `applyEdit`.
    async fn discussion_edit(
        &self,
        addr: Addr,
        op: impl FnOnce(&mut pimble_discussion::DiscussionTxn<'_, '_>) -> pimble_discussion::doc::Result<String> + Send,
    ) -> Result<String, String> {
        let mut answer = String::new();
        let answer_ref = &mut answer;
        self.edit(addr, move |doc| {
            let edited = Discussion::of_node(doc).edit(op);
            *answer_ref = edited.result.map_err(|e| e.to_string())?;
            Ok(())
        })
        .await?;
        Ok(answer)
    }

    /// Refused unless message `id` of the discussion is this agent's own.
    fn require_own(snap: &DiscussionSnapshot, id: &str, me: Option<&Author>) -> Result<(), String> {
        let message = snap.message(id).ok_or("There is no such message here; read_discussion lists the ids.")?;
        // A message with no author is the person's (written signed out),
        // never an agent's: an agent always signs.
        match (&message.author, me) {
            (Some(author), Some(me)) if author.id == me.id => Ok(()),
            (None, None) => Ok(()),
            _ => Err("That message is someone else's; you can change only your own.".into()),
        }
    }
}

#[tool_router(router = discussion_router, vis = "pub(super)")]
impl Pimble {
    /// List the discussions on a node (each with its link, how many messages,
    /// and the latest), or say what a discussion node is.
    #[tool(name = "list_discussions")]
    async fn list_discussions(&self, Parameters(p): Parameters<ListDiscussionsParams>) -> Result<String, String> {
        let addr = self.api.resolve(&p.node).await?;
        let node = self.api.node(addr).await?;
        self.present(addr).await;
        if node.node_type == node_types::DISCUSSION {
            return Ok(format!("{} is itself a discussion:\n{}", label(&node), summary(addr, &node)));
        }
        let attached = self.api.attachments(addr).await?;
        let discussions: Vec<&Node> = attached.iter().filter(|n| n.node_type == node_types::DISCUSSION).collect();
        if discussions.is_empty() {
            return Ok(format!("{} has no discussions. start_discussion starts one.", label(&node)));
        }
        let mut out = format!("Discussions on {}:\n", label(&node));
        for d in discussions {
            out.push_str(&summary(Addr { store: addr.store, node: d.id }, d));
            out.push('\n');
        }
        Ok(out)
    }

    /// Read a discussion: its topic, what it is on, and every message as a
    /// nested list (replies under what they reply to), each with its id,
    /// author and time.
    #[tool(name = "read_discussion")]
    async fn read_discussion(&self, Parameters(p): Parameters<DiscussionParam>) -> Result<String, String> {
        let (addr, node) = self.discussion(&p.discussion).await?;
        self.present(addr).await;
        let snap = snapshot_of(&node)?;
        let mut out = format!("Discussion: {} ({})\n", topic_of(&node), addr.link());
        if let Some(host) = node.attached_to {
            let host_addr = Addr { store: addr.store, node: host };
            self.present(host_addr).await;
            let host_title = self.api.node(host_addr).await.map(|n| label(&n)).unwrap_or_else(|_| "a node".into());
            out.push_str(&format!("On: {host_title} ({})\n", host_addr.link()));
        }
        if let Some(resolved) = &snap.resolved {
            let by = resolved.by.as_ref().map(|a| format!(" by {}", a.name)).unwrap_or_default();
            out.push_str(&format!("Resolved{by} at {}.\n", resolved.at));
        }
        if snap.thread().is_empty() {
            out.push_str("No messages yet.\n");
        } else {
            out.push('\n');
            out.push_str(&snap.to_markdown());
        }
        Ok(out)
    }

    /// Start a discussion with its first message: on a node (`on`), where it
    /// belongs to the node, or as a discussion node of its own under `parent`.
    #[tool(name = "start_discussion")]
    pub(super) async fn start_discussion(&self, Parameters(p): Parameters<StartDiscussionParams>) -> Result<String, String> {
        let body = Body::from_markdown(&p.message);
        if body.is_blank() {
            return Err("A message needs some text.".into());
        }
        let author = self.author().await;
        // Where on its host it is: a place in the host's text, named by a quote.
        let host = match &p.on {
            Some(on) => Some(self.api.resolve(on).await?),
            None => None,
        };
        let anchor = match (host, &p.after_text) {
            (Some(host), Some(quote)) => {
                let doc = self.api.doc(host).await?;
                let sticky = doc.sticky_at_quote(quote).map_err(super::crdt)?;
                Some(pimble_discussion::Anchor::Text { sticky })
            }
            (None, Some(_)) => return Err("after_text names a place in the text of the node given as `on`.".into()),
            _ => None,
        };
        if p.inline && p.on.is_none() {
            return Err("inline shows the discussion in the text of the node given as `on`.".into());
        }
        // The discussion with its first message, made in the one create
        // (attached or a node of its own is only where it is made).
        let topic = p.topic.trim().to_string();
        let now = now();
        let first = FirstMessage { author: author.as_ref(), at: &now, body: &body };
        let init = recipes::discussion(&topic, anchor.as_ref(), Some(first))?;
        match (host, &p.parent) {
            (Some(host), None) => {
                let addr = self.api.create_attached(host, init).await?;
                let mut answer = format!("Started the discussion {} on {}", addr.link(), self.describe(host).await);
                if p.inline {
                    let url = pimble_core::PimbleUrl::node(addr.store, addr.node);
                    let atom = Embed::discussion(url, Some(topic.clone())).to_markdown();
                    let quote = p.after_text.clone();
                    let shown = self
                        .edit(host, move |doc| {
                            match &quote {
                                Some(quote) => doc.insert_markdown_after(quote, false, &atom),
                                None => doc.append_markdown(&atom),
                            }
                            .map(|_| ())
                            .map_err(super::crdt)
                        })
                        .await;
                    match shown {
                        Ok(()) => answer.push_str(", shown in its text"),
                        Err(e) => answer.push_str(&format!("; it could not be shown in the text ({e}), and is listed with the node's discussions")),
                    }
                }
                answer.push('.');
                Ok(answer)
            }
            (None, Some(parent)) => {
                let parent = self.api.resolve(parent).await?;
                let addr = self.api.create(parent, pimble_core::Place::Last, init).await?;
                Ok(format!("Created the discussion {}.", self.describe(addr).await))
            }
            _ => Err("Give `on` (the node it is about) or `parent` (where a discussion node of its own goes), not both.".into()),
        }
    }

    /// Reply in a discussion: to a message (`reply_to`, its id), or at the
    /// top level.
    #[tool(name = "reply")]
    async fn reply(&self, Parameters(p): Parameters<ReplyParams>) -> Result<String, String> {
        let (addr, _) = self.discussion(&p.discussion).await?;
        let author = self.author().await;
        let body = Body::from_markdown(&p.message);
        let now = now();
        let reply_to = p.reply_to.clone();
        let id = self
            .discussion_edit(addr, move |t| t.post(Stamp::new(author.as_ref(), &now), reply_to.as_deref(), &body))
            .await?;
        Ok(format!("Posted message {id}."))
    }

    /// Rewrite one of your own messages.
    #[tool(name = "edit_message")]
    async fn edit_message(&self, Parameters(p): Parameters<EditMessageParams>) -> Result<String, String> {
        let (addr, node) = self.discussion(&p.discussion).await?;
        let author = self.author().await;
        Self::require_own(&snapshot_of(&node)?, &p.message_id, author.as_ref())?;
        let body = Body::from_markdown(&p.message);
        let now = now();
        let id = p.message_id.clone();
        self.discussion_edit(addr, move |t| t.edit_message(Stamp::new(author.as_ref(), &now), &id, &body).map(|_| String::new()))
            .await?;
        Ok(format!("Edited message {}.", p.message_id))
    }

    /// Delete one of your own messages (its replies stay where they are).
    #[tool(name = "delete_message")]
    async fn delete_message(&self, Parameters(p): Parameters<DeleteMessageParams>) -> Result<String, String> {
        let (addr, node) = self.discussion(&p.discussion).await?;
        let author = self.author().await;
        Self::require_own(&snapshot_of(&node)?, &p.message_id, author.as_ref())?;
        let now = now();
        let id = p.message_id.clone();
        self.discussion_edit(addr, move |t| t.delete_message(Stamp::new(author.as_ref(), &now), &id).map(|_| String::new()))
            .await?;
        Ok(format!("Deleted message {}.", p.message_id))
    }

    /// Mark a discussion resolved, or open it again (`reopen`).
    #[tool(name = "resolve_discussion")]
    async fn resolve_discussion(&self, Parameters(p): Parameters<ResolveParams>) -> Result<String, String> {
        let (addr, node) = self.discussion(&p.discussion).await?;
        let author = self.author().await;
        let now = now();
        let reopen = p.reopen;
        self.discussion_edit(addr, move |t| {
            if reopen {
                t.reopen()?;
            } else {
                t.resolve(Stamp::new(author.as_ref(), &now))?;
            }
            Ok(String::new())
        })
        .await?;
        Ok(format!("{} {}.", if reopen { "Reopened" } else { "Resolved" }, topic_of(&node)))
    }
}
