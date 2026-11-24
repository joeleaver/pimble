//! The tools that write a note's text by naming what they touch
//! (docs/MCP_CONTRACT.md "Edits that touch only what they name"). Each is one
//! `NodeDoc::edit_markdown` sent through `Pimble::edit`, or with `dry_run` one
//! `NodeDoc::preview_markdown` that sends nothing. A block is named by a quote
//! of its text or by its number, never by a number read as text ("Naming what
//! an edit touches"); every edit that removes or replaces says what was there
//! ("What an edit says it did").

use pimble_api::Addr;
use pimble_crdt::markdown::{BlockTarget, Echo, HeadingTarget, InsertAt, MarkdownEdit, Occurrence};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use super::{crdt, Pimble};

/// Which of the places a quote appears: 1, 2, ... or "last".
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(untagged)]
pub enum OccurrenceParam {
    Number(usize),
    Word(String),
}

impl OccurrenceParam {
    fn get(&self) -> Result<Occurrence, String> {
        match self {
            OccurrenceParam::Number(n) => Ok(Occurrence::Nth(*n)),
            OccurrenceParam::Word(w) if w.trim().eq_ignore_ascii_case("last") => Ok(Occurrence::Last),
            OccurrenceParam::Word(w) => match w.trim().parse::<usize>() {
                Ok(n) => Ok(Occurrence::Nth(n)),
                Err(_) => Err(format!("`occurrence` is a number from 1, or \"last\"; \"{w}\" is neither.")),
            },
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppendParams {
    /// The node to append to.
    pub node: String,
    /// What to append, as Markdown.
    pub markdown: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InsertAfterParams {
    /// The node to insert into.
    pub node: String,
    /// Text that appears in the block to insert after (a list item's text names
    /// its whole list), matched as text: "13" is the characters 1 and 3, never
    /// block 13. With `after_section`, some of a heading's text or a heading
    /// path, as get_node's `section` takes. Give this or `block_index`.
    #[serde(default)]
    pub quote: Option<String>,
    /// The number of the block to insert after (with `after_section`, of the
    /// heading whose section to insert after), as get_node's `outline` and
    /// `blocks` number them, from 0. Give this or `quote`.
    #[serde(default)]
    pub block_index: Option<usize>,
    /// When `quote` appears in several blocks: which, 1 for the first, or "last".
    #[serde(default)]
    pub occurrence: Option<OccurrenceParam>,
    /// Go after the whole section under the heading named, not just the heading.
    #[serde(default)]
    pub after_section: bool,
    /// What to insert, as Markdown.
    pub markdown: String,
    /// Say where it would go, and write nothing.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplaceSectionParams {
    /// The node.
    pub node: String,
    /// The heading whose section is replaced (the heading itself stays): some of
    /// its text, or a heading path such as "Design/Roads" for one inside
    /// another's section, as get_node's `section` takes. Give this or `block_index`.
    #[serde(default)]
    pub heading: Option<String>,
    /// The heading's block number, as get_node's `outline` prints it. Give this
    /// or `heading`.
    #[serde(default)]
    pub block_index: Option<usize>,
    /// The section's new content, as Markdown.
    pub markdown: String,
    /// Say what would be replaced, and write nothing.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplaceTextParams {
    /// The node.
    pub node: String,
    /// The exact text to replace, inside one paragraph (plain text: no Markdown
    /// syntax).
    pub quote: String,
    /// When `quote` appears several times: which, 1 for the first, or "last".
    #[serde(default)]
    pub occurrence: Option<OccurrenceParam>,
    /// What replaces it (plain text; empty deletes the quote).
    pub text: String,
    /// Say what would be replaced, and write nothing.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplaceBlockParams {
    /// The node.
    pub node: String,
    /// Text that appears in the block (a heading, a paragraph; a list item's
    /// text names its whole list), matched as text: "13" is the characters 1
    /// and 3, never block 13. Give this or `block_index`.
    #[serde(default)]
    pub quote: Option<String>,
    /// The block's number, as get_node's `outline` and `blocks` number them,
    /// from 0. Give this or `quote`.
    #[serde(default)]
    pub block_index: Option<usize>,
    /// When `quote` appears in several blocks: which, 1 for the first, or "last".
    #[serde(default)]
    pub occurrence: Option<OccurrenceParam>,
    /// What replaces that block, as Markdown; empty removes it.
    #[serde(default)]
    pub markdown: String,
    /// Say what would be replaced or removed, and write nothing.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveSectionParams {
    /// The node.
    pub node: String,
    /// The heading to remove, together with everything under it: some of its
    /// text, or a heading path, as get_node's `section` takes. Give this or
    /// `block_index`.
    #[serde(default)]
    pub heading: Option<String>,
    /// The heading's block number, as get_node's `outline` prints it. Give this
    /// or `heading`.
    #[serde(default)]
    pub block_index: Option<usize>,
    /// Say what would be removed, and write nothing.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplaceContentParams {
    /// The node.
    pub node: String,
    /// The node's whole new content, as Markdown. Blocks that stay the same are left
    /// untouched; empty clears the content.
    pub markdown: String,
    /// Say which stretches would change, and write nothing.
    #[serde(default)]
    pub dry_run: bool,
}

/// The occurrence a tool's argument names.
fn occurrence(param: &Option<OccurrenceParam>) -> Result<Option<Occurrence>, String> {
    param.as_ref().map(OccurrenceParam::get).transpose()
}

/// The block a tool's `quote`, `block_index` and `occurrence` name: exactly
/// one of the first two.
fn block_target<'a>(quote: Option<&'a str>, index: Option<usize>, occurrence: Option<Occurrence>) -> Result<BlockTarget<'a>, String> {
    match (quote, index) {
        (Some(_), Some(_)) => Err("Give `quote` or `block_index`, not both.".into()),
        (None, None) => Err(
            "Name the block: `quote` (some of its text) or `block_index` (its number, as get_node's `outline` and `blocks` print it)."
                .into(),
        ),
        (None, Some(_)) if occurrence.is_some() => {
            Err("`occurrence` picks one of the places a quote appears; with `block_index` leave it out.".into())
        }
        (None, Some(index)) => Ok(BlockTarget::Index(index)),
        (Some(quote), None) => Ok(BlockTarget::Quote { quote, occurrence }),
    }
}

/// The heading a tool's `heading` and `block_index` name: exactly one of them.
fn heading_target(heading: Option<&str>, index: Option<usize>) -> Result<HeadingTarget<'_>, String> {
    match (heading, index) {
        (Some(_), Some(_)) => Err("Give `heading` or `block_index`, not both.".into()),
        (None, None) => Err(
            "Name the heading: `heading` (some of its text, or a heading path) or `block_index` (its number, as get_node's `outline` prints it)."
                .into(),
        ),
        (Some(name), None) => Ok(HeadingTarget::Name(name)),
        (None, Some(index)) => Ok(HeadingTarget::Index(index)),
    }
}

/// Blocks `from..to` in words: "block 4", "blocks 4-7", or for an empty
/// stretch where it is.
fn blocks(from: usize, to: usize) -> String {
    match to.saturating_sub(from) {
        0 => format!("nothing (an empty stretch at block {from})"),
        1 => format!("block {from}"),
        _ => format!("blocks {from}-{}", to - 1),
    }
}

/// What an echo says, one indented line each: what was there, and what was
/// either side of it.
fn echo_lines(echo: &Echo) -> String {
    let mut out = String::new();
    if !echo.old.is_empty() {
        out.push_str(&format!("  was: \"{}\"\n", echo.old));
    }
    let (start, end) = if echo.in_block { ("(the start of its paragraph)", "(the end of its paragraph)") } else { ("(the start of the note)", "(the end of the note)") };
    let side = |s: &Option<String>, edge: &str| s.as_ref().map_or_else(|| edge.to_string(), |s| format!("\"{s}\""));
    out.push_str(&format!("  before it: {}\n", side(&echo.before, start)));
    out.push_str(&format!("  after it: {}\n", side(&echo.after, end)));
    out
}

/// An edit's answer: what was done (or with `dry_run` what would be), then
/// what each echo says.
fn answer(dry_run: bool, done: String, would: String, echoes: &[Echo]) -> String {
    let mut out = if dry_run { format!("Dry run, nothing written: this would {would}.\n") } else { format!("{done}.\n") };
    for echo in echoes {
        out.push_str(&echo_lines(echo));
    }
    out
}

/// How many changed stretches a whole-content rewrite lists.
const STRETCHES_LISTED: usize = 20;

impl Pimble {
    /// Make `edit` in the node `node` names, or with `dry_run` judge it and
    /// write nothing: what it took out or would take out.
    async fn edit_text(&self, node: &str, edit: MarkdownEdit<'_>, dry_run: bool) -> Result<(Addr, Vec<Echo>), String> {
        let addr = self.resolve(node).await?;
        if dry_run {
            // The one write, with an edit that changes nothing: the document is
            // brought up to date and the access judged, and nothing is sent.
            let previewed = self.api.edit(addr, |doc, _| doc.preview_markdown(edit).map_err(crdt)).await?;
            self.present(addr).await;
            return Ok((addr, previewed.value));
        }
        let mut echoes = Vec::new();
        self.edit(addr, |doc| {
            echoes = doc.edit_markdown(edit).map_err(crdt)?.1;
            Ok(())
        })
        .await?;
        Ok((addr, echoes))
    }
}

#[tool_router(router = text_edit_router, vis = "pub(super)")]
impl Pimble {
    /// Append Markdown after the node's content.
    #[tool(name = "append")]
    pub(super) async fn append(&self, Parameters(p): Parameters<AppendParams>) -> Result<String, String> {
        let addr = self.resolve(&p.node).await?;
        self.edit(addr, |doc| doc.append_markdown(&p.markdown).map(drop).map_err(crdt)).await?;
        Ok(format!("Appended to {}.", addr.link()))
    }

    /// Name where by `quote` (some of a block's text, never its number) or by
    /// `block_index` (its number, as get_node's `outline` and `blocks` print
    /// it); `occurrence` picks one of several blocks a quote appears in.
    /// Inserts Markdown after that block, or with `after_section` after the
    /// whole section under the heading named. Answers where it went, between
    /// which blocks; `dry_run: true` answers that and writes nothing.
    #[tool(name = "insert_after")]
    pub(super) async fn insert_after(&self, Parameters(p): Parameters<InsertAfterParams>) -> Result<String, String> {
        let occurrence = occurrence(&p.occurrence)?;
        let at = if p.after_section {
            if occurrence.is_some() {
                return Err("`occurrence` picks among a quote's places in the text; name a section by its heading's text, a heading path or `block_index`.".into());
            }
            InsertAt::Section(heading_target(p.quote.as_deref(), p.block_index)?)
        } else {
            InsertAt::Block(block_target(p.quote.as_deref(), p.block_index, occurrence)?)
        };
        let (addr, echoes) = self.edit_text(&p.node, MarkdownEdit::InsertAfter { at, markdown: &p.markdown }, p.dry_run).await?;
        let at = echoes.first().map_or(0, |e| e.from);
        let place = format!("after block {} of {}, the new blocks starting at block {at}", at.saturating_sub(1), addr.link());
        Ok(answer(p.dry_run, format!("Inserted {place}"), format!("insert {place}"), &echoes))
    }

    /// Name the heading by `heading` (some of its text or a heading path,
    /// never its number) or by `block_index` (its number, as get_node's
    /// `outline` prints it). Replaces the blocks under it, up to the next
    /// heading of its level or higher; the heading stays. Answers what was
    /// there (its first 120 characters), its block numbers and its neighbors;
    /// `dry_run: true` answers that and writes nothing.
    #[tool(name = "replace_section")]
    pub(super) async fn replace_section(&self, Parameters(p): Parameters<ReplaceSectionParams>) -> Result<String, String> {
        let heading = heading_target(p.heading.as_deref(), p.block_index)?;
        let (addr, echoes) = self.edit_text(&p.node, MarkdownEdit::ReplaceSection { heading, markdown: &p.markdown }, p.dry_run).await?;
        let e = &echoes[0];
        let what = format!("{} of {}, the section under the heading at block {}", blocks(e.from, e.to), addr.link(), e.from.saturating_sub(1));
        Ok(answer(p.dry_run, format!("Replaced {what}"), format!("replace {what}"), &echoes))
    }

    /// `quote` is the exact text to replace (plain text inside one paragraph,
    /// never a block number); `occurrence` picks one when it appears several
    /// times, and several without it are refused with every place listed.
    /// Replaces it with new text. Answers the old text, its block number and
    /// the text either side of it; `dry_run: true` answers that and writes
    /// nothing.
    #[tool(name = "replace_text")]
    pub(super) async fn replace_text(&self, Parameters(p): Parameters<ReplaceTextParams>) -> Result<String, String> {
        let edit = MarkdownEdit::ReplaceText { quote: &p.quote, occurrence: occurrence(&p.occurrence)?, text: &p.text };
        let (addr, echoes) = self.edit_text(&p.node, edit, p.dry_run).await?;
        let what = format!("text in block {} of {}", echoes[0].from, addr.link());
        Ok(answer(p.dry_run, format!("Replaced {what}"), format!("replace {what}"), &echoes))
    }

    /// Name the block by `quote` (some of its text, never its number) or by
    /// `block_index` (its number, as get_node's `outline` and `blocks` print
    /// it); `occurrence` picks one of several blocks a quote appears in, and
    /// several without it are refused with every place listed. Replaces that
    /// one block (a heading, a paragraph, a list, a code block) with Markdown,
    /// or removes it with empty Markdown: removing or re-levelling a heading,
    /// turning a paragraph into a list. Removing by a quote under 12
    /// characters, or one that reads like a number, is refused: use
    /// `block_index`. Answers what was there (its first 120 characters), its
    /// block number and its neighbors; `dry_run: true` answers that and writes
    /// nothing.
    #[tool(name = "replace_block")]
    pub(super) async fn replace_block(&self, Parameters(p): Parameters<ReplaceBlockParams>) -> Result<String, String> {
        let block = block_target(p.quote.as_deref(), p.block_index, occurrence(&p.occurrence)?)?;
        let (addr, echoes) = self.edit_text(&p.node, MarkdownEdit::ReplaceBlock { block, markdown: &p.markdown }, p.dry_run).await?;
        let what = format!("{} of {}", blocks(echoes[0].from, echoes[0].to), addr.link());
        let (done, would) = if p.markdown.trim().is_empty() { ("Removed", "remove") } else { ("Replaced", "replace") };
        Ok(answer(p.dry_run, format!("{done} {what}"), format!("{would} {what}"), &echoes))
    }

    /// Name the heading by `heading` (some of its text or a heading path,
    /// never its number) or by `block_index` (its number, as get_node's
    /// `outline` prints it). Removes it and everything under it, up to the next
    /// heading of its level or higher. Answers what was there (its first 120
    /// characters), its block numbers and its neighbors; `dry_run: true`
    /// answers that and writes nothing.
    #[tool(name = "remove_section")]
    pub(super) async fn remove_section(&self, Parameters(p): Parameters<RemoveSectionParams>) -> Result<String, String> {
        let heading = heading_target(p.heading.as_deref(), p.block_index)?;
        let (addr, echoes) = self.edit_text(&p.node, MarkdownEdit::RemoveSection { heading }, p.dry_run).await?;
        let what = format!("{} of {}, a heading and its section", blocks(echoes[0].from, echoes[0].to), addr.link());
        Ok(answer(p.dry_run, format!("Removed {what}"), format!("remove {what}"), &echoes))
    }

    /// Replace the node's whole content, for restructuring. Only what differs is
    /// written: blocks that read the same stay untouched (and so does anyone typing in
    /// them), and the node keeps its link. Prefer the smaller edits for small changes.
    /// Answers each changed stretch, by its block numbers before the edit and its
    /// old text; `dry_run: true` answers that and writes nothing.
    #[tool(name = "replace_content")]
    pub(super) async fn replace_content(&self, Parameters(p): Parameters<ReplaceContentParams>) -> Result<String, String> {
        let (addr, echoes) = self.edit_text(&p.node, MarkdownEdit::ReplaceContent { markdown: &p.markdown }, p.dry_run).await?;
        let stretches = match echoes.len() {
            0 => "the whole of its empty content".to_string(),
            1 => "1 changed stretch, numbered as the blocks were".to_string(),
            n => format!("{n} changed stretches, numbered as the blocks were"),
        };
        let what = format!("the content of {}: {stretches}", addr.link());
        let mut out = if p.dry_run { format!("Dry run, nothing written: this would replace {what}.\n") } else { format!("Replaced {what}.\n") };
        for e in echoes.iter().take(STRETCHES_LISTED) {
            let was = if e.old.is_empty() { "new blocks".to_string() } else { format!("was \"{}\"", e.old) };
            out.push_str(&format!("- {}: {was}\n", blocks(e.from, e.to)));
        }
        if echoes.len() > STRETCHES_LISTED {
            out.push_str(&format!("- and {} more stretches\n", echoes.len() - STRETCHES_LISTED));
        }
        Ok(out)
    }
}
