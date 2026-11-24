//! What an edit says it did (docs/MCP_CONTRACT.md "What an edit says it did"):
//! the text it took out or replaced, where that was, and what was next to it,
//! read from the content before the edit and handed back to the caller. Nothing
//! of it is kept anywhere: the document is the only record.

use rinch_editor_core::{Fragment, Node, Schema};

use super::headings::blocks_markdown;
use super::quotes::{textblock_text, Match};
use crate::error::Result;

/// How many characters of the old text an echo keeps.
pub const EXCERPT: usize = 120;

/// How many characters of each neighbor an echo keeps.
pub const NEIGHBOR: usize = 60;

/// One stretch an edit removed, replaced or inserted at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Echo {
    /// The top-level blocks it covered, `from..to`, numbered as in the content
    /// before the edit (as get_node numbered them). `from == to` is an
    /// insertion before block `from`; for a quote, the block holding it.
    pub from: usize,
    pub to: usize,
    /// The old text: the blocks' Markdown, or the quote a text edit replaced,
    /// whitespace run together, cut at [`EXCERPT`] characters with "…". Empty
    /// for an insertion.
    pub old: String,
    /// What came right before it: the end of the block before (for a quote,
    /// the text before it in its paragraph). None at the start.
    pub before: Option<String>,
    /// What came right after it: the start of the block after (for a quote,
    /// the text after it in its paragraph). None at the end.
    pub after: Option<String>,
    /// Whether it is a quote inside a block rather than whole blocks.
    pub in_block: bool,
}

/// `text` with its whitespace run together.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first `max` characters of `text`, whitespace run together, "…" when cut.
pub fn excerpt(text: &str, max: usize) -> String {
    let text = flat(text);
    if text.chars().count() <= max {
        return text;
    }
    let mut cut: String = text.chars().take(max).collect();
    cut.push('…');
    cut
}

/// The first `max` characters of `text` as they are, "…" after when cut.
pub(super) fn head(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max).collect();
    cut.push('…');
    cut
}

/// The last `max` characters of `text` as they are, "…" before when cut.
pub(super) fn tail(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let kept: String = text.chars().skip(count - max).collect();
    format!("…{kept}")
}

/// A block's text for saying which it is: a textblock's own text, any other
/// block's Markdown.
pub(super) fn block_text(block: &Node) -> String {
    if block.is_textblock() {
        return textblock_text(block);
    }
    let schema = Schema::starter_kit();
    schema.branch("doc", Fragment::from_node(block.clone())).map(|d| super::render(&d)).unwrap_or_default()
}

/// The neighbors of the blocks `from..to` of `doc`: the end of the one before
/// and the start of the one after.
fn neighbors(doc: &Node, from: usize, to: usize) -> (Option<String>, Option<String>) {
    let before = (from > 0).then(|| tail(&flat(&block_text(doc.child(from - 1))), NEIGHBOR));
    let after = (to < doc.child_count()).then(|| excerpt(&block_text(doc.child(to)), NEIGHBOR));
    (before, after)
}

/// The echo of the blocks `from..to` of `doc` taken out or replaced.
pub(super) fn of_blocks(schema: &Schema, doc: &Node, from: usize, to: usize) -> Result<Echo> {
    let old = excerpt(&blocks_markdown(schema, doc, from, to)?, EXCERPT);
    let (before, after) = neighbors(doc, from, to);
    Ok(Echo { from, to, old, before, after, in_block: false })
}

/// The echo of an insertion before block `at` of `doc`.
pub(super) fn of_insert(doc: &Node, at: usize) -> Echo {
    let (before, after) = neighbors(doc, at, at);
    Echo { from: at, to: at, old: String::new(), before, after, in_block: false }
}

/// The echo of the quote at `m` replaced.
pub(super) fn of_quote(m: &Match) -> Echo {
    let some = |s: String| (!s.trim().is_empty()).then_some(s);
    Echo {
        from: m.block(),
        to: m.block() + 1,
        old: excerpt(&m.text[m.start..m.end], EXCERPT),
        before: some(tail(&m.text[..m.start], NEIGHBOR)),
        after: some(head(&m.text[m.end..], NEIGHBOR)),
        in_block: true,
    }
}
