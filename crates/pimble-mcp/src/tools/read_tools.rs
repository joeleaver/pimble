//! The tools that read: the stores, a node's children and outline, a node
//! found by name, a node whole or in part, search, links, people and what
//! was deleted.

use std::collections::HashSet;

use pimble_api::names::{label, stop_sentence, store_tops};
use pimble_api::{Addr, Pimble as Api};
use pimble_core::{LinkResolution, PimbleUrl, Store, StoreAccess, StoreKind, SyncState};
use pimble_crdt::{CrdtError, NodeDoc};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;


use super::{crdt, type_name, LinkParam, NodeParam, Pimble, StoreParam};

#[derive(Deserialize, JsonSchema)]
pub struct GetNodeParams {
    /// The node: a pimble: link, a node id, or a path like "Store/Folder/Note".
    pub node: String,
    /// Read only one heading and its section: some of the heading's text, or a
    /// heading path such as "Design/The county's edges" (or "Design > The
    /// county's edges", as search hits and the outline write it) for a heading
    /// inside another's section. Several headings that match are listed.
    #[serde(default)]
    pub section: Option<String>,
    /// Read only the outline: every heading, indented by level, with its block
    /// number and its section's size in characters, and the node's metadata.
    /// For a long note, read this first, then a `section`.
    #[serde(default)]
    pub outline: bool,
    /// Read only these top-level blocks, numbered from 0 as the outline numbers
    /// them: "12" or "12-20" (both ends included).
    #[serde(default)]
    pub blocks: Option<String>,
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

#[tool_router(router = read_router, vis = "pub(super)")]
impl Pimble {
    /// The stores open on this computer: name, id, kind, sync state, and whether
    /// you may change them.
    #[tool(name = "list_stores")]
    pub(super) async fn list_stores(&self) -> Result<String, String> {
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
            // What the link is holding back, and why (docs/SYNC_LIMITS_CONTRACT.md):
            // every other note of the store keeps syncing.
            if !store.waiting.is_empty() {
                out.push_str(&format!("  {} (the rest keeps syncing):\n", pimble_core::waiting_phrase(store.waiting.len())));
                for waiting in &store.waiting {
                    let link = PimbleUrl { store: store.id, node: waiting.node_id, anchor: None };
                    out.push_str(&format!("  - {link}: {}\n", waiting.reason));
                }
            }
        }
        Ok(out)
    }

    /// A node's children, or with `depth` an indented outline of its subtree. Each
    /// line carries the node's link, to name it in the next call.
    #[tool(name = "list_children")]
    pub(super) async fn list_children(&self, Parameters(p): Parameters<ListChildrenParams>) -> Result<String, String> {
        let depth = p.depth.unwrap_or(1).clamp(1, 6);
        // A store with several shared roots lists them as its top level.
        let tops = match self.find_store(&p.node).await {
            Ok(store) if !store.roots.is_empty() => store_tops(&store),
            _ => vec![self.resolve(&p.node).await?],
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
    pub(super) async fn find_node(&self, Parameters(p): Parameters<FindNodeParams>) -> Result<String, String> {
        let wanted = p.title.trim().to_lowercase();
        if wanted.is_empty() {
            return Err("Give some of the title to look for.".into());
        }
        let searched: Vec<Store> = match &p.store {
            Some(name) => vec![self.find_store(name).await?],
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
    /// Markdown. For a long note, `outline: true` lists its headings with their
    /// sizes, and `section` reads one heading's section (a search hit's `in:`
    /// line is a heading path `section` takes as it is); `blocks` reads a range
    /// of blocks.
    #[tool(name = "get_node")]
    pub(super) async fn get_node(&self, Parameters(p): Parameters<GetNodeParams>) -> Result<String, String> {
        let addr = self.resolve(&p.node).await?;
        let node = self.api.node(addr).await?;
        self.present(addr).await;
        let doc = NodeDoc::load(&node.content).map_err(|e| e.to_string())?;
        let mut out = self.node_header(addr, &node).await;
        let part = p.section.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let blocks = p.blocks.as_deref().map(str::trim).filter(|s| !s.is_empty());
        if p.outline || part.is_some() || blocks.is_some() {
            if [p.outline, part.is_some(), blocks.is_some()].iter().filter(|x| **x).count() > 1 {
                return Err("Give one of `outline`, `section` and `blocks`.".into());
            }
            let special = [pimble_core::node_types::DISCUSSION, pimble_core::node_types::BOARD, pimble_core::node_types::SHEET];
            if special.contains(&node.node_type.as_str()) {
                return Err(format!("A {} has no headings to read by; read it whole.", type_name(&node)));
            }
            if p.outline {
                out.push_str("---\n");
                out.push_str(&outline_of(&doc).map_err(crdt)?);
                return Ok(out);
            }
            let (said, md) = match (part, blocks) {
                (Some(name), _) => {
                    let (heading, md) = doc.section_markdown(name).map_err(crdt)?;
                    let total = doc.outline().map_err(crdt)?.blocks;
                    (
                        format!(
                            "section: {} (blocks {}-{} of 0-{}; {} characters)\n",
                            heading.path.join(pimble_crdt::markdown::PATH_SEPARATOR),
                            heading.index,
                            heading.end.saturating_sub(1),
                            total.saturating_sub(1),
                            md.chars().count()
                        ),
                        md,
                    )
                }
                (None, Some(range)) => {
                    let (from, to) = block_range(range)?;
                    let (md, total) = doc.blocks_markdown(from, to).map_err(crdt)?;
                    let to = to.min(total.saturating_sub(1));
                    (format!("blocks: {from}-{to} of 0-{} ({} characters)\n", total.saturating_sub(1), md.chars().count()), md)
                }
                (None, None) => unreachable!(),
            };
            out.push_str(&said);
            out.push_str("---\n");
            let md = self.inline_discussions(&md).await;
            // A part of the text: the tables it holds are shown where they are,
            // and the note's other tables are not this part's to list.
            let (md, _) = self.tables_in_text(addr, &node, &md).await;
            out.push_str(md.trim_end());
            out.push('\n');
            return Ok(out);
        }
        let content = match doc.markdown() {
            Ok(md) => md,
            Err(_) => format!("(formatting left out: this content cannot be shown as Markdown)\n\n{}", doc.text()),
        };
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
            // A long note says so first, and how to read less of it; all of it
            // still follows.
            let size = content.chars().count();
            if size > LONG_NOTE {
                let headings = doc.outline().map(|o| o.sections.len()).unwrap_or(0);
                out.insert_str(
                    0,
                    &format!(
                        "(This note is {size} characters long{}; get_node with `outline: true` lists its headings and their sizes, and `section` reads one. The whole note follows.)\n",
                        match headings {
                            0 => String::new(),
                            1 => " with 1 heading".to_string(),
                            n => format!(" with {n} headings"),
                        }
                    ),
                );
            }
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

    /// Search the open stores, by words or (with `semantic`) by meaning. Each
    /// hit says where in the note it is (`in: Design > The county's edges`): a
    /// heading path to pass to get_node as `section`, to read just that part.
    /// A note with a heading (or title) that is exactly the query comes first.
    /// Without `store`, every open store is searched, and the hits from the
    /// store this session last read or wrote in come first.
    #[tool(name = "search")]
    pub(super) async fn search(&self, Parameters(p): Parameters<SearchParams>) -> Result<String, String> {
        let only = match &p.store {
            Some(name) => vec![self.find_store(name).await?.id],
            None => Vec::new(),
        };
        let limit = p.limit.unwrap_or(20).clamp(1, 50);
        let mut results = self.api.search(&p.query, only.clone(), p.semantic, limit).await?;
        // Unscoped: the store this session last worked in first, its hits in
        // their own order, then everyone else's in theirs.
        let mut favored = None;
        if let (None, Some(store)) = (&p.store, self.last_store()) {
            let mine = self.api.search(&p.query, vec![store], p.semantic, limit).await.unwrap_or_default();
            if !mine.is_empty() {
                let others: Vec<_> = results.into_iter().filter(|r| r.store_id != store).collect();
                results = mine.into_iter().chain(others).take(limit).collect();
                favored = Some(store);
            }
        }
        // A store searched whose index is still on its way may be missing
        // notes (docs/STARTUP_CONTRACT.md): said first.
        let indexing: Vec<String> = self
            .api
            .stores()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s.index.is_indexing() && (only.is_empty() || only.contains(&s.id)))
            .map(|s| s.name)
            .collect();
        let note = pimble_core::IndexState::search_note(&indexing);
        if results.is_empty() {
            return Ok(if note.is_empty() { format!("Nothing matches \"{}\".", p.query) } else { format!("Nothing matches \"{}\". {note}", p.query) });
        }
        let mut out = String::new();
        if !note.is_empty() {
            out.push_str(&note);
            out.push('\n');
        }
        if let Some(store) = favored {
            let name = self
                .api
                .known_stores()
                .await
                .ok()
                .and_then(|stores| stores.into_iter().find(|s| s.id == store).map(|s| s.name))
                .unwrap_or_else(|| store.to_string());
            out.push_str(&format!("Hits from {name} first (the store this session last worked in); pass `store` to search one store.\n"));
        }
        for r in results {
            let addr = Addr { store: r.store_id, node: r.node_id };
            let path = self.api.path(addr).await;
            let snippet: String = r.snippet.split_whitespace().collect::<Vec<_>>().join(" ");
            out.push_str(&format!("- {path} ({})\n", addr.link()));
            if !r.section.is_empty() {
                out.push_str(&format!("  in: {}\n", r.section.join(pimble_crdt::markdown::PATH_SEPARATOR)));
            }
            out.push_str(&format!("  {snippet}\n"));
        }
        Ok(out)
    }

    /// Where a pimble: link points now (a node that moved out of a share or store
    /// is followed to where it went).
    #[tool(name = "resolve_link")]
    pub(super) async fn resolve_link(&self, Parameters(p): Parameters<LinkParam>) -> Result<String, String> {
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

    /// Who can open a node (the members of the shares it is in), by the name each
    /// goes by in its share, with the Markdown that mentions each one.
    #[tool(name = "list_people")]
    pub(super) async fn list_people(&self, Parameters(p): Parameters<NodeParam>) -> Result<String, String> {
        let addr = self.resolve(&p.node).await?;
        let people = match self.api.backend().people(addr.store).await {
            Ok(people) => people,
            Err(_) => return Ok("Nobody else can open this note: its store is not shared.".into()),
        };
        let mut chain = vec![addr.node];
        chain.extend(self.api.ancestors(addr).await?.into_iter().map(|a| a.node));
        let members = people.members_of(&chain);
        if members.is_empty() {
            return Ok("Nobody else can open this note: it is in no share.".into());
        }
        let mut out = String::from("People who can open it (mention one with its Markdown):\n");
        for person in members {
            let mention = pimble_core::mention::Mention { account: person.user_id.clone(), id: None, name: person.name.clone() };
            out.push_str(&format!("- {}: [{}]({})\n", person.name, mention.words(), mention.href()));
        }
        Ok(out)
    }

    /// A store's recently deleted nodes, which undelete_node can put back.
    #[tool(name = "list_deleted")]
    pub(super) async fn list_deleted(&self, Parameters(p): Parameters<StoreParam>) -> Result<String, String> {
        let store = self.find_store(&p.store).await?;
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
}

/// Whether listing `top` is listing a store's single root (so no header line).
fn store_listing_is_top(stores: &[Store], top: Addr) -> bool {
    stores.iter().any(|s| s.id == top.store && s.root_node_id == top.node)
}

/// Past this many characters, a whole note read with get_node starts with a
/// line saying its size and how to read less of it.
const LONG_NOTE: usize = 15_000;

/// A `blocks` argument, "12" or "12-20", as an inclusive range.
fn block_range(range: &str) -> Result<(usize, usize), String> {
    let bad = || format!("`blocks` is a block number or a range such as \"12-20\" (not \"{range}\").");
    let number = |s: &str| s.trim().parse::<usize>().map_err(|_| bad());
    match range.split_once('-') {
        Some((a, b)) => {
            let (a, b) = (number(a)?, number(b)?);
            if b < a {
                return Err(bad());
            }
            Ok((a, b))
        }
        None => {
            let n = number(range)?;
            Ok((n, n))
        }
    }
}

/// A note's headings as get_node's `outline` answer: each indented by its
/// level, with its block and its section's size.
fn outline_of(doc: &NodeDoc) -> Result<String, CrdtError> {
    let outline = doc.outline()?;
    if outline.blocks == 0 {
        return Ok("(no content)\n".into());
    }
    let mut out = format!(
        "outline: {} characters in {} blocks (0-{}); `section` reads a heading's section (a heading path joins headings with \" > \" or \"/\"), `blocks` a range.\n",
        outline.total_chars,
        outline.blocks,
        outline.blocks - 1
    );
    if outline.sections.is_empty() {
        out.push_str("(no headings)\n");
        return Ok(out);
    }
    let first = outline.sections[0].0.index;
    if first > 0 {
        out.push_str(&format!("(before the first heading: blocks 0-{}, {} characters)\n", first - 1, outline.intro_chars));
    }
    let top = outline.sections.iter().map(|(h, _)| h.level).min().unwrap_or(1);
    for (h, chars) in &outline.sections {
        out.push_str(&format!(
            "{}- {} {} (block {}, {chars} characters)\n",
            "  ".repeat((h.level - top).max(0) as usize),
            "#".repeat(h.level.clamp(1, 6) as usize),
            h.text,
            h.index
        ));
    }
    Ok(out)
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
