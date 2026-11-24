//! Refusing a removal named too loosely to be sure of (docs/MCP_CONTRACT.md
//! "Naming what an edit touches"): a quote or a heading name so short, or so
//! like a number, that it may name something other than what was meant. An
//! agent once removed the paragraph holding "13" when it meant block 13, and
//! `remove_section(heading: "3")` took out "Phase 3". Each refusal says what
//! the edit would have removed and how to name it surely.

use rinch_editor_core::Node;

use super::echo::{block_text, excerpt, NEIGHBOR};
use super::headings::{last_segment, loose, Heading, HeadingTarget};
use super::quotes::{block_count, listed, matches, pick, Narrowing, Quote};
use super::refused;
use crate::error::Result;

/// The fewest characters a quote must have to remove a block or text by it, or
/// a heading name to remove or empty a section by part of the heading's text.
pub const MIN_DELETE_QUOTE: usize = 12;

/// Whether `quote` reads like a number ("13", "#13", "4-6", "12.") rather
/// than text: digits and nothing but `#`, `.`, `-`, `,` and spaces.
fn looks_like_a_number(quote: &str) -> bool {
    quote.chars().any(|c| c.is_ascii_digit()) && quote.chars().all(|c| c.is_ascii_digit() || "#.-, ".contains(c))
}

/// Why `quote` is too loose to remove by, or None when it is not: it reads
/// like a number, or it is shorter than [`MIN_DELETE_QUOTE`]. `names` is what
/// the number sentence says is text instead ("a quote is", "a heading is
/// named by its").
fn too_loose(quote: &str, names: &str) -> Option<String> {
    let trimmed = quote.trim();
    if looks_like_a_number(trimmed) {
        return Some(format!("\"{quote}\" reads like a block number, but {names} text"));
    }
    (trimmed.chars().count() < MIN_DELETE_QUOTE).then(|| {
        format!("\"{quote}\" is shorter than {MIN_DELETE_QUOTE} characters, too little to be sure what it means")
    })
}

/// The block number a number-like `quote` reads as, when the note has it.
fn meant_block(doc: &Node, quote: &str) -> Option<usize> {
    let trimmed = quote.trim();
    trimmed.trim_start_matches('#').parse::<usize>().ok().filter(|&n| looks_like_a_number(trimmed) && n < block_count(doc))
}

/// Refuse removing a block by a quote too loose to be sure of. With
/// `block_index` on offer the sentence gives the number to use; without it,
/// how much more to quote.
pub(super) fn judge_delete_quote(doc: &Node, quote: Quote<'_>) -> Result<()> {
    let Some(why) = too_loose(quote.text, "a quote is") else {
        return Ok(());
    };
    let by_index = quote.narrowing == Narrowing::OccurrenceOrBlockIndex;
    let found = matches(doc, quote.text, |_| true);
    let would = match pick(found.clone(), quote, "block") {
        Ok(m) => {
            let sure = if by_index {
                format!("to remove that block give `block_index: {}`", m.block())
            } else {
                format!("to remove that block quote at least {MIN_DELETE_QUOTE} characters of it")
            };
            format!("It would have removed block {} (\"{}\"); {sure}.", m.block(), excerpt(&m.text, NEIGHBOR))
        }
        Err(_) if found.is_empty() => "It names no block in this node.".to_string(),
        Err(_) => {
            let sure = if by_index { "Give `block_index` for the one to remove." } else { "Quote more of the one to remove." };
            format!("It appears {} times: {}. {sure}", found.len(), listed(&found))
        }
    };
    let hint = match meant_block(doc, quote.text) {
        Some(n) if by_index => format!(" If you meant block {n}, give `block_index: {n}`."),
        _ => String::new(),
    };
    let longer = if by_index { " A longer quote works too." } else { "" };
    Err(refused(format!("Nothing was removed: {why}. {would}{hint}{longer}")))
}

/// Refuse deleting text (a replacement with nothing) by a quote too loose to be
/// sure of. Quoting the words either side and giving them back without it
/// deletes exactly the same, surely: only what differs is replaced.
pub(super) fn judge_delete_text(doc: &Node, quote: Quote<'_>) -> Result<()> {
    let Some(why) = too_loose(quote.text, "a quote is") else {
        return Ok(());
    };
    let found = matches(doc, quote.text, |_| true);
    let would = match pick(found.clone(), quote, "paragraph") {
        Ok(m) => format!("It would have deleted it from block {} (\"{}\").", m.block(), excerpt(&m.text, NEIGHBOR)),
        Err(_) if found.is_empty() => "It appears nowhere in this node.".to_string(),
        Err(_) => format!("It appears {} times: {}.", found.len(), listed(&found)),
    };
    Err(refused(format!(
        "Nothing was deleted: {why}. {would} To delete it surely, quote it with the words either side and give those words back without it (quote \"the 13 apples\", text \"the apples\"): only what differs changes."
    )))
}

/// Refuse removing or emptying (`verb`) the section of heading `h` when
/// `target` named it by a short or number-like part of its text. A name that
/// is the heading's whole text (as find_heading compares, ignoring case and
/// punctuation), or a heading path ending in it, is sure however short: a
/// heading titled "3" is removed by "3". A block number is sure.
pub(super) fn judge_delete_heading(doc: &Node, target: HeadingTarget<'_>, h: &Heading, verb: &str) -> Result<()> {
    let HeadingTarget::Name(name) = target else {
        return Ok(());
    };
    let last = last_segment(name);
    let whole = loose(&h.text);
    if loose(name) == whole || loose(last) == whole {
        return Ok(());
    }
    let Some(why) = too_loose(last, "a heading is named by its") else {
        return Ok(());
    };
    let hint = match meant_block(doc, last) {
        Some(n) if n != h.index => format!(" If you meant the block numbered {n}, name it by that number."),
        _ => String::new(),
    };
    let text = excerpt(&block_text(doc.child(h.index)), NEIGHBOR);
    Err(refused(format!(
        "Nothing was changed: {why}. It names the heading \"{text}\" (block {}) by only part of its text; to {verb} that section, give the heading's whole text or its block number, {}.{hint}",
        h.index, h.index
    )))
}
