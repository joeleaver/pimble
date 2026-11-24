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

/// How an edit names one top-level block: a quote of some of its text (a list
/// item's text names its whole list), or its number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockTarget<'a> {
    Quote { quote: &'a str, occurrence: Option<Occurrence> },
    Index(usize),
}

impl<'a> BlockTarget<'a> {
    /// The block a quote names, which must appear in one block only.
    pub fn quote(quote: &'a str) -> Self {
        BlockTarget::Quote { quote, occurrence: None }
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
/// document order. An empty quote appears nowhere.
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
        for (start, _) in text.match_indices(quote) {
            out.push(Match { path: path.clone(), start, end: start + quote.len(), text: text.clone() });
        }
    }
    out
}

/// The places `quote` appears, one per top-level block (its first in each):
/// what a quote naming a block chooses among.
pub(super) fn block_matches(doc: &Node, quote: &str) -> Vec<Match> {
    let mut out: Vec<Match> = Vec::new();
    for m in matches(doc, quote, |_| true) {
        if out.last().map(Match::block) != Some(m.block()) {
            out.push(m);
        }
    }
    out
}

/// Every match, numbered as `occurrence` counts them.
pub(super) fn listed(found: &[Match]) -> String {
    found.iter().enumerate().map(|(i, m)| format!("({}) {}", i + 1, m.describe())).collect::<Vec<_>>().join("; ")
}

/// The one match `occurrence` picks among `found`, the places `quote` appears.
/// `what` is what a quote names ("block", "paragraph"); `places` how the count
/// reads, `{n}` standing for it ("{n} times", "in {n} blocks"); `by_index`
/// whether the edit also takes a
/// block number. Nothing found, several found with no occurrence, and an
/// occurrence past the last are refusals that list every match.
pub(super) fn pick(found: Vec<Match>, quote: &str, occurrence: Option<Occurrence>, what: &str, places: &str, by_index: bool) -> Result<Match> {
    let n = found.len();
    if n == 0 {
        return Err(refused(format!("No {what} in this node contains \"{quote}\".")));
    }
    let or_index = if by_index { ", or `block_index`" } else { "" };
    let places = places.replace("{n}", &n.to_string());
    match occurrence {
        None if n == 1 => Ok(found.into_iter().next().expect("one match")),
        None => Err(refused(format!(
            "\"{quote}\" appears {places} in this node: {}. Give `occurrence` (1 to {n}, or \"last\"){or_index}, or quote more of the {what} so it names one.",
            listed(&found)
        ))),
        Some(Occurrence::Nth(0)) => Err(refused("`occurrence` counts from 1 (or is \"last\").")),
        Some(Occurrence::Nth(k)) if k > n => Err(refused(format!(
            "\"{quote}\" appears {places} in this node, so there is no occurrence {k}: {}.",
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

/// The top-level block `target` names.
pub(super) fn target_block(doc: &Node, target: BlockTarget<'_>) -> Result<usize> {
    match target {
        BlockTarget::Index(index) => {
            let count = block_count(doc);
            if index >= count {
                return Err(no_such_block(count, index));
            }
            Ok(index)
        }
        BlockTarget::Quote { quote, occurrence } => {
            if quote.is_empty() {
                return Err(refused("Name the block with some of its text, or give `block_index`."));
            }
            Ok(pick(block_matches(doc, quote), quote, occurrence, "block", "in {n} blocks", true)?.block())
        }
    }
}

/// The fewest characters a quote must have to remove a block by it.
pub const MIN_DELETE_QUOTE: usize = 12;

/// Whether `quote` reads like a number ("13", "#13", "4-6", "12.") rather
/// than text: digits and nothing but `#`, `.`, `-`, `,` and spaces.
fn looks_like_a_number(quote: &str) -> bool {
    quote.chars().any(|c| c.is_ascii_digit()) && quote.chars().all(|c| c.is_ascii_digit() || "#.-, ".contains(c))
}

/// Refuse removing a block by a quote too short to be sure of, or one that
/// reads like a block number (an agent once removed the paragraph holding
/// "13" when it meant block 13). The sentence says what it would have removed
/// and how to name it surely.
pub(super) fn judge_delete_quote(doc: &Node, quote: &str, occurrence: Option<Occurrence>) -> Result<()> {
    let trimmed = quote.trim();
    let number = looks_like_a_number(trimmed);
    if !number && trimmed.chars().count() >= MIN_DELETE_QUOTE {
        return Ok(());
    }
    let why = if number {
        format!("\"{quote}\" reads like a block number, but a quote is text")
    } else {
        format!("\"{quote}\" is shorter than {MIN_DELETE_QUOTE} characters, too little to be sure which block it means")
    };
    let found = block_matches(doc, quote);
    let would = match pick(found.clone(), quote, occurrence, "block", "in {n} blocks", true) {
        Ok(m) => format!(
            "It would have removed block {} (\"{}\"); to remove that block give `block_index: {}`.",
            m.block(),
            excerpt(&m.text, super::echo::NEIGHBOR),
            m.block()
        ),
        Err(_) if found.is_empty() => "It names no block in this node.".to_string(),
        Err(_) => format!("It appears in {} blocks: {}. Give `block_index` for the one to remove.", found.len(), listed(&found)),
    };
    let meant = trimmed.trim_start_matches('#').parse::<usize>().ok().filter(|_| number);
    let hint = match meant {
        Some(n) if n < block_count(doc) => format!(" If you meant block {n}, give `block_index: {n}`."),
        _ => String::new(),
    };
    Err(refused(format!("Nothing was removed: {why}. {would}{hint} A longer quote works too.")))
}
