//! Naming a block or a place in the text: by a quote of its text (with which
//! occurrence, when it appears several times), or a block by its number
//! (docs/MCP_CONTRACT.md "Naming what an edit touches").
//!
//! A quote is text, never a number: "13" names whatever block contains the
//! characters "13". A block's number is [`BlockTarget::Index`], the numbering
//! get_node's outline and blocks print (top-level blocks from 0).

use rinch_editor_core::Node;

use super::echo::{excerpt, head, tail};
use super::{refused, ATOM};
use crate::error::{CrdtError, Result};

/// Which of the places a quote appears an edit means: the `Nth` (from 1) or the
/// last. Left out, the quote must appear once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Occurrence {
    Nth(usize),
    Last,
}

/// What the caller lets a quote be narrowed by besides quoting more of the
/// text: what the refusal of a quote that appears several times advises. A
/// tool's own parameters decide it (replace_block takes both `occurrence` and
/// `block_index`, replace_text only `occurrence`, a sheet's `after_text`
/// neither).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Narrowing {
    /// Only a longer quote.
    Nothing,
    /// `occurrence`, or a longer quote.
    Occurrence,
    /// `occurrence`, `block_index`, or a longer quote.
    OccurrenceOrBlockIndex,
}

/// A quote of the text, with which of its places is meant when it appears
/// several times, and what else the caller lets it be narrowed by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quote<'a> {
    pub text: &'a str,
    pub occurrence: Option<Occurrence>,
    pub narrowing: Narrowing,
}

impl<'a> Quote<'a> {
    /// A quote that must appear exactly once, from a caller that offers nothing
    /// else to choose by.
    pub fn alone(text: &'a str) -> Self {
        Quote { text, occurrence: None, narrowing: Narrowing::Nothing }
    }
}

/// How an edit names one top-level block: a quote of some of its text (a list
/// item's text names its whole list), or its number, with optionally some text
/// that must be in that block now (`expect`: a number read before the note
/// changed names another block, and `expect` notices).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockTarget<'a> {
    Quote(Quote<'a>),
    Index { index: usize, expect: Option<&'a str> },
}

impl<'a> BlockTarget<'a> {
    /// The block a quote names, which must appear exactly once.
    pub fn quote(quote: &'a str) -> Self {
        BlockTarget::Quote(Quote::alone(quote))
    }

    /// The block at `index`, with nothing expected of it.
    pub fn index(index: usize) -> Self {
        BlockTarget::Index { index, expect: None }
    }
}

/// The text a quote is matched against: a textblock's text, atoms as [`ATOM`].
pub(super) fn textblock_text(textblock: &Node) -> String {
    textblock.content().iter().map(|child| child.text().map(str::to_string).unwrap_or_else(|| ATOM.to_string())).collect()
}

/// Every textblock under `node` (itself included) in document order, each with its
/// path of child indices from `node`.
fn textblocks(node: &Node) -> Vec<(Vec<usize>, Node)> {
    fn walk(node: &Node, path: &mut Vec<usize>, out: &mut Vec<(Vec<usize>, Node)>) {
        if node.is_textblock() {
            out.push((path.clone(), node.clone()));
            return;
        }
        for (i, child) in node.content().iter().enumerate() {
            if child.is_block() {
                path.push(i);
                walk(child, path, out);
                path.pop();
            }
        }
    }
    let mut out = Vec::new();
    walk(node, &mut Vec::new(), &mut out);
    out
}

/// One place a quote appears: the textblock's path from the document, the byte
/// range of the quote in its [`textblock_text`], and that text.
#[derive(Debug, Clone)]
pub(super) struct Match {
    pub path: Vec<usize>,
    pub start: usize,
    pub end: usize,
    pub text: String,
}

impl Match {
    /// The top-level block it is in.
    pub fn block(&self) -> usize {
        self.path[0]
    }

    /// How it reads in a refusal: its block, and the quote with the text
    /// either side of it.
    fn describe(&self) -> String {
        let before = tail(&self.text[..self.start], 30);
        let after = head(&self.text[self.end..], 30);
        format!("block {}: \"{before}{}{after}\"", self.block(), &self.text[self.start..self.end])
    }
}

/// Every place `quote` appears among the textblocks `keep` accepts, in
/// document order, overlapping places included. An empty quote appears
/// nowhere. A quote naming a block chooses among all of them too, so a quote
/// twice in one block (two items of one list) is as ambiguous as one in two
/// blocks.
pub(super) fn matches(doc: &Node, quote: &str, keep: impl Fn(&Node) -> bool) -> Vec<Match> {
    let mut out = Vec::new();
    if quote.is_empty() {
        return out;
    }
    for (path, block) in textblocks(doc) {
        if !keep(&block) {
            continue;
        }
        let text = textblock_text(&block);
        // Overlapping places count, each its own occurrence: "aa" appears
        // twice in "aaa", and either can be chosen.
        let mut from = 0;
        while let Some(at) = text[from..].find(quote) {
            let start = from + at;
            out.push(Match { path: path.clone(), start, end: start + quote.len(), text: text.clone() });
            from = start + text[start..].chars().next().map_or(1, char::len_utf8);
        }
    }
    out
}

/// Every match, numbered as `occurrence` counts them.
pub(super) fn listed(found: &[Match]) -> String {
    found.iter().enumerate().map(|(i, m)| format!("({}) {}", i + 1, m.describe())).collect::<Vec<_>>().join("; ")
}

/// The one match `quote.occurrence` picks among `found`, the places the quote
/// appears. `what` is what a quote names ("block", "paragraph"). Nothing
/// found, several found with no occurrence (advising what `quote.narrowing`
/// offers), and an occurrence past the last are refusals that list every
/// match.
pub(super) fn pick(found: Vec<Match>, quote: Quote<'_>, what: &str) -> Result<Match> {
    let n = found.len();
    let text = quote.text;
    if n == 0 {
        return Err(refused(format!("No {what} in this node contains \"{text}\".")));
    }
    match quote.occurrence {
        None if n == 1 => Ok(found.into_iter().next().expect("one match")),
        None => {
            let advice = match quote.narrowing {
                Narrowing::Nothing => format!("Quote more of the {what} so the quote appears once."),
                Narrowing::Occurrence => {
                    format!("Give `occurrence` (1 to {n}, or \"last\"), or quote more of the {what} so it names one.")
                }
                Narrowing::OccurrenceOrBlockIndex => format!(
                    "Give `occurrence` (1 to {n}, or \"last\"), or `block_index`, or quote more of the {what} so it names one."
                ),
            };
            Err(refused(format!("\"{text}\" appears {n} times in this node: {}. {advice}", listed(&found))))
        }
        Some(Occurrence::Nth(0)) => Err(refused("`occurrence` counts from 1 (or is \"last\").")),
        Some(Occurrence::Nth(k)) if k > n => Err(refused(format!(
            "\"{text}\" appears {n} times in this node, so there is no occurrence {k}: {}.",
            listed(&found)
        ))),
        Some(Occurrence::Nth(k)) => Ok(found.into_iter().nth(k - 1).expect("k <= n")),
        Some(Occurrence::Last) => Ok(found.into_iter().last().expect("n > 0")),
    }
}

/// Whether `doc` holds nothing anyone wrote: one empty paragraph (what a node's
/// content is before anything is typed in it).
pub(crate) fn is_blank(doc: &Node) -> bool {
    doc.child_count() == 1 && doc.child(0).type_name() == "paragraph" && doc.child(0).content().is_empty()
}

/// How many blocks get_node numbers: none in a blank document.
pub(super) fn block_count(doc: &Node) -> usize {
    if is_blank(doc) {
        0
    } else {
        doc.child_count()
    }
}

/// The refusal of a block number past the content's blocks.
pub(super) fn no_such_block(count: usize, index: usize) -> CrdtError {
    match count {
        0 => refused("This node has no content, so it has no blocks."),
        n => refused(format!("This node has blocks 0 to {}, so there is no block {index}.", n - 1)),
    }
}

/// A block's kind in words ("paragraph", "bullet list", ...).
pub(super) fn kind_of(block: &Node) -> String {
    block.type_name().replace('_', " ")
}

/// Refuse naming block `index` by number when `expect` is given and that block
/// does not contain it now: the number was read before the note changed, and
/// names another block. The sentence says what is there now.
pub(super) fn check_expected(doc: &Node, index: usize, expect: Option<&str>) -> Result<()> {
    let Some(expect) = expect else {
        return Ok(());
    };
    if expect.is_empty() {
        return Err(refused("`expect` is some text the block holds; leave it out to name the block by its number alone."));
    }
    let block = doc.child(index);
    if textblocks(block).iter().any(|(_, textblock)| textblock_text(textblock).contains(expect)) {
        return Ok(());
    }
    Err(refused(format!(
        "Nothing was changed: block {index} does not contain \"{expect}\"; it is a {} that reads \"{}\". The note may have changed since its blocks were numbered: read its outline or blocks again (get_node) for the number now.",
        kind_of(block),
        excerpt(&super::echo::block_text(block), super::echo::NEIGHBOR)
    )))
}

/// The top-level block `target` names.
pub(super) fn target_block(doc: &Node, target: BlockTarget<'_>) -> Result<usize> {
    match target {
        BlockTarget::Index { index, expect } => {
            let count = block_count(doc);
            if index >= count {
                return Err(no_such_block(count, index));
            }
            check_expected(doc, index, expect)?;
            Ok(index)
        }
        BlockTarget::Quote(quote) => {
            if quote.text.is_empty() {
                return Err(refused(match quote.narrowing {
                    Narrowing::OccurrenceOrBlockIndex => "Name the block with some of its text, or give `block_index`.",
                    _ => "Name the block with some of its text.",
                }));
            }
            Ok(pick(matches(doc, quote.text, |_| true), quote, "block")?.block())
        }
    }
}
