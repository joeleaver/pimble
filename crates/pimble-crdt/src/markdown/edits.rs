//! The block edits, as functions from the model before to the model after
//! (docs/MCP_CONTRACT.md "Edits that touch only what they name"). Each rebuilds
//! only the branch it changes, so every other node is the same `Rc` and the
//! projection's diff records exactly the change; each says what it took out
//! ([`Echo`]).

use std::rc::Rc;

use rinch_editor_core::{Fragment, Node, Schema};

use super::echo::{self, Echo};
use super::headings::{headings, target_heading, Heading, HeadingTarget};
use super::quotes::{is_blank, judge_delete_quote, matches, pick, target_block, BlockTarget, Occurrence};
use super::{collab, parse, refused, render, ATOM};
use crate::error::Result;

/// Where an insertion goes: after one block, or after a heading's section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertAt<'a> {
    Block(BlockTarget<'a>),
    Section(HeadingTarget<'a>),
}

/// One edit of a node's content in Markdown terms, to make
/// (`NodeDoc::edit_markdown`) or to preview (`NodeDoc::preview_markdown`):
/// both run the same function on the same model, so a preview says exactly
/// what the edit would do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkdownEdit<'a> {
    /// `markdown` after a block or a section.
    InsertAfter { at: InsertAt<'a>, markdown: &'a str },
    /// The blocks under a heading (the heading stays) replaced by `markdown`.
    ReplaceSection { heading: HeadingTarget<'a>, markdown: &'a str },
    /// A quote inside one paragraph replaced by plain `text`.
    ReplaceText { quote: &'a str, occurrence: Option<Occurrence>, text: &'a str },
    /// One top-level block replaced by `markdown`, or removed when it is empty.
    ReplaceBlock { block: BlockTarget<'a>, markdown: &'a str },
    /// A heading and its section removed.
    RemoveSection { heading: HeadingTarget<'a> },
    /// The whole content rewritten as the fewest block edits.
    ReplaceContent { markdown: &'a str },
}

/// The models `edit` steps through from `doc` (one for every edit but a
/// whole-content rewrite, which records each changed stretch as its own step),
/// and what it took out. Refusals change nothing.
pub(crate) fn steps(schema: &Rc<Schema>, doc: &Node, edit: MarkdownEdit<'_>) -> Result<(Vec<Node>, Vec<Echo>)> {
    let one = |(after, echo): (Node, Echo)| (vec![after], vec![echo]);
    Ok(match edit {
        MarkdownEdit::InsertAfter { at, markdown } => one(insert_after(schema, doc, at, markdown)?),
        MarkdownEdit::ReplaceSection { heading, markdown } => one(replace_section(schema, doc, heading, markdown)?),
        MarkdownEdit::ReplaceText { quote, occurrence, text } => one(replace_text(schema, doc, quote, occurrence, text)?),
        MarkdownEdit::ReplaceBlock { block, markdown } => one(replace_block(schema, doc, block, markdown)?),
        MarkdownEdit::RemoveSection { heading } => one(remove_section(schema, doc, heading)?),
        MarkdownEdit::ReplaceContent { markdown } => replace_content(schema, doc, markdown)?,
    })
}

/// `doc` with its top-level blocks `from..to` replaced by `blocks`. The blocks outside
/// the range are the same nodes, which is what keeps the recorded edit to the range.
fn splice(doc: &Node, from: usize, to: usize, blocks: &[Node]) -> Node {
    let children: Vec<Node> = doc.content().children()[..from]
        .iter()
        .chain(blocks.iter())
        .chain(doc.content().children()[to..].iter())
        .cloned()
        .collect();
    doc.copy_with_content(Fragment::from_children(children))
}

fn parsed_blocks(schema: &Rc<Schema>, md: &str) -> Result<Vec<Node>> {
    Ok(parse(schema, md)?.content().children().to_vec())
}

/// After `doc`'s content (instead of it, when it is blank).
pub(crate) fn append(schema: &Rc<Schema>, doc: &Node, md: &str) -> Result<Node> {
    let blocks = parsed_blocks(schema, md)?;
    if is_blank(doc) {
        return Ok(splice(doc, 0, 1, &blocks));
    }
    Ok(splice(doc, doc.child_count(), doc.child_count(), &blocks))
}

/// The whole content of a blank `doc`. Anything already written is a refusal: this is
/// the first write of a new node, never a rewrite.
pub(crate) fn write_new(schema: &Rc<Schema>, doc: &Node, md: &str) -> Result<Node> {
    if !is_blank(doc) {
        return Err(refused(
            "This node already has content; append to it or replace a section or some text instead.",
        ));
    }
    append(schema, doc, md)
}

/// `md` after the block `at` names, or after the section under the heading it
/// names.
pub(crate) fn insert_after(schema: &Rc<Schema>, doc: &Node, at: InsertAt<'_>, md: &str) -> Result<(Node, Echo)> {
    let at = match at {
        InsertAt::Section(heading) => target_heading(doc, heading)?.end,
        InsertAt::Block(block) => target_block(doc, block)? + 1,
    };
    let blocks = parsed_blocks(schema, md)?;
    Ok((splice(doc, at, at, &blocks), echo::of_insert(doc, at)))
}

/// The blocks under the heading `heading` names (the heading itself stays), up
/// to the next heading of its level or higher.
pub(crate) fn replace_section(schema: &Rc<Schema>, doc: &Node, heading: HeadingTarget<'_>, md: &str) -> Result<(Node, Echo)> {
    let h = target_heading(doc, heading)?;
    let blocks = parsed_blocks(schema, md)?;
    let echo = echo::of_blocks(schema, doc, h.index + 1, h.end)?;
    Ok((splice(doc, h.index + 1, h.end, &blocks), echo))
}

/// `doc` with `children` as its blocks, one empty paragraph when there are none (an
/// editor document always has a block).
fn with_blocks(schema: &Rc<Schema>, doc: &Node, mut children: Vec<Node>) -> Result<Node> {
    if children.is_empty() {
        children.push(schema.branch("paragraph", Fragment::empty()).map_err(collab)?);
    }
    Ok(doc.copy_with_content(Fragment::from_children(children)))
}

/// The top-level block `block` names, replaced by `md`; an empty `md` deletes it.
/// What removes or re-levels one heading, or turns a paragraph into a list.
/// Removing by a quote too short to be sure of, or one that reads like a block
/// number, is refused ([`judge_delete_quote`]): a number names a block only as
/// [`BlockTarget::Index`].
pub(crate) fn replace_block(schema: &Rc<Schema>, doc: &Node, block: BlockTarget<'_>, md: &str) -> Result<(Node, Echo)> {
    let removing = md.trim().is_empty();
    if let (true, BlockTarget::Quote { quote, occurrence }) = (removing, block) {
        judge_delete_quote(doc, quote, occurrence)?;
    }
    let index = target_block(doc, block)?;
    let blocks = if removing { Vec::new() } else { parsed_blocks(schema, md)? };
    let echo = echo::of_blocks(schema, doc, index, index + 1)?;
    let after = splice(doc, index, index + 1, &blocks);
    Ok((with_blocks(schema, &after, after.content().children().to_vec())?, echo))
}

/// The heading `heading` names and everything under it, up to the next heading
/// of its level or higher, removed.
pub(crate) fn remove_section(schema: &Rc<Schema>, doc: &Node, heading: HeadingTarget<'_>) -> Result<(Node, Echo)> {
    let h = target_heading(doc, heading)?;
    let echo = echo::of_blocks(schema, doc, h.index, h.end)?;
    let after = splice(doc, h.index, h.end, &[]);
    Ok((with_blocks(schema, &after, after.content().children().to_vec())?, echo))
}

/// Every top-level heading of `level` and the section under it, taken out;
/// `links` (Markdown) goes where the first one was. Refused when there is
/// none to take.
pub(crate) fn cut_sections(schema: &Rc<Schema>, doc: &Node, level: i64, links: Option<&str>) -> Result<Node> {
    let pieces: Vec<Heading> = headings(doc).into_iter().filter(|h| h.level == level).collect();
    let Some(first) = pieces.first().map(|h| h.index) else {
        return Err(refused(format!("This note has no level-{level} headings to split it at.")));
    };
    let mut after = doc.clone();
    for h in pieces.iter().rev() {
        after = splice(&after, h.index, h.end, &[]);
    }
    if let Some(md) = links.filter(|md| !md.trim().is_empty()) {
        let blocks = parsed_blocks(schema, md)?;
        after = splice(&after, first, first, &blocks);
    }
    with_blocks(schema, &after, after.content().children().to_vec())
}

/// The whole content replaced by `md`, as the smallest set of block edits: blocks
/// that read the same before and after (the same Markdown) stay the same nodes, and
/// each stretch between them is one step, last stretch first so earlier indices hold.
/// Recorded step by step, an unchanged block between two changed ones is never touched,
/// so anyone typing in it keeps their words and caret, and it keeps whatever Markdown
/// cannot say about it. An empty `md` clears the content. Each changed stretch
/// is echoed, in document order, numbered as in `doc`.
pub(crate) fn replace_content(schema: &Rc<Schema>, doc: &Node, md: &str) -> Result<(Vec<Node>, Vec<Echo>)> {
    let new: Vec<Node> = if md.trim().is_empty() { Vec::new() } else { parsed_blocks(schema, md)? };
    let old: Vec<Node> = doc.content().children().to_vec();
    let key = |block: &Node| -> Result<String> {
        Ok(render(&schema.branch("doc", Fragment::from_node(block.clone())).map_err(collab)?))
    };
    let old_keys = old.iter().map(key).collect::<Result<Vec<_>>>()?;
    let new_keys = new.iter().map(key).collect::<Result<Vec<_>>>()?;
    // A blank document is replaced outright.
    if is_blank(doc) {
        return Ok((vec![with_blocks(schema, doc, new)?], Vec::new()));
    }

    // Longest common subsequence of the blocks' Markdown.
    let (n, m) = (old.len(), new.len());
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old_keys[i] == new_keys[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    // The changed stretches: (old range, new blocks).
    let mut hunks: Vec<(usize, usize, Vec<Node>)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    let (mut start_i, mut start_j) = (0, 0);
    let flush = |i: usize, j: usize, start_i: usize, start_j: usize, hunks: &mut Vec<(usize, usize, Vec<Node>)>| {
        if i > start_i || j > start_j {
            hunks.push((start_i, i, new[start_j..j].to_vec()));
        }
    };
    while i < n || j < m {
        if i < n && j < m && old_keys[i] == new_keys[j] {
            flush(i, j, start_i, start_j, &mut hunks);
            i += 1;
            j += 1;
            start_i = i;
            start_j = j;
        } else if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
            j += 1;
        } else {
            i += 1;
        }
    }
    flush(i, j, start_i, start_j, &mut hunks);

    let echoes = hunks.iter().map(|(from, to, _)| echo::of_blocks(schema, doc, *from, *to)).collect::<Result<Vec<_>>>()?;
    let mut steps = Vec::with_capacity(hunks.len());
    let mut current = doc.clone();
    for (from, to, blocks) in hunks.into_iter().rev() {
        current = splice(&current, from, to, &blocks);
        if current.child_count() == 0 {
            current = with_blocks(schema, &current, Vec::new())?;
        }
        steps.push(current.clone());
    }
    Ok((steps, echoes))
}

/// The occurrence of `quote` (the only one, unless `occurrence` picks one)
/// replaced by `text` inside its textblock. Text the two
/// share at either end is left alone; the rest of the new text takes the marks of the
/// first character it replaces, as typing over a selection does, and everything else
/// keeps its own. An empty `text` deletes the quote.
pub(crate) fn replace_text(
    schema: &Rc<Schema>,
    doc: &Node,
    quote: &str,
    occurrence: Option<Occurrence>,
    text: &str,
) -> Result<(Node, Echo)> {
    if quote.contains('\n') || text.contains('\n') {
        return Err(refused("Replacing text works inside one paragraph; a quote or replacement with a line break spans more."));
    }
    if quote.is_empty() {
        return Err(refused("Name the text to replace with an exact quote of it."));
    }
    let found = pick(matches(doc, quote, |_| true), quote, occurrence, "paragraph", "{n} times", false)?;
    let echo = echo::of_quote(&found);
    // Only what differs is replaced: the text the quote and its replacement share at
    // either end stays where it is, marks and all (and so does anyone's caret in it).
    let prefix = common_prefix(quote, text);
    let suffix = common_prefix_rev(&quote[prefix..], &text[prefix..]);
    let (start, end) = (found.start + prefix, found.end - suffix);
    let text = &text[prefix..text.len() - suffix];
    if start == end && text.is_empty() {
        return Ok((doc.clone(), echo));
    }
    Ok((rebuild_at(doc, &found.path, &|textblock| splice_text(schema, textblock, start, end, text))?, echo))
}

/// The byte length of the longest common prefix of `a` and `b`, on char boundaries.
fn common_prefix(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).map(|(x, _)| x.len_utf8()).sum()
}

/// The byte length of the longest common suffix of `a` and `b`, on char boundaries.
fn common_prefix_rev(a: &str, b: &str) -> usize {
    a.chars().rev().zip(b.chars().rev()).take_while(|(x, y)| x == y).map(|(x, _)| x.len_utf8()).sum()
}

/// `node` with the descendant at `path` replaced by `f` of it, every other branch the
/// same node.
fn rebuild_at(node: &Node, path: &[usize], f: &dyn Fn(&Node) -> Result<Node>) -> Result<Node> {
    let Some((&first, rest)) = path.split_first() else {
        return f(node);
    };
    let child = rebuild_at(node.child(first), rest, f)?;
    Ok(node.copy_with_content(node.content().replace_child(first, child)))
}

/// A textblock with the byte range `start..end` of its [`textblock_text`] replaced by
/// `text`. Atoms inside the range go with it.
fn splice_text(schema: &Rc<Schema>, textblock: &Node, start: usize, end: usize, text: &str) -> Result<Node> {
    let mut out: Vec<Node> = Vec::new();
    let mut inserted = text.is_empty();
    let mut pos = 0;
    let push_text = |out: &mut Vec<Node>, s: &str, like: &Node| -> Result<()> {
        if s.is_empty() {
            return Ok(());
        }
        let marks = like.marks().to_vec();
        if let Some(last) = out.last_mut() {
            if let Some(prev) = last.text() {
                if last.marks() == marks.as_slice() {
                    *last = schema.text_with_marks(&format!("{prev}{s}"), marks).map_err(collab)?;
                    return Ok(());
                }
            }
        }
        out.push(schema.text_with_marks(s, marks).map_err(collab)?);
        Ok(())
    };
    let children = textblock.content().children();
    for (i, child) in children.iter().enumerate() {
        let len = child.text().map_or(ATOM.len_utf8(), str::len);
        let (from, to) = (pos, pos + len);
        pos = to;
        let Some(own) = child.text() else {
            // An atom: kept unless the quote covers it.
            if to <= start || from >= end {
                out.push(child.clone());
            }
            continue;
        };
        let keep_before = &own[..start.clamp(from, to) - from];
        let keep_after = &own[end.clamp(from, to) - from..];
        push_text(&mut out, keep_before, child)?;
        // The insertion point is in this run, or at its end with no run after it to
        // take it (the paragraph's end, or an atom next).
        let ends_here = start == to && children.get(i + 1).is_none_or(|next| next.text().is_none());
        if !inserted && start >= from && (start < to || ends_here) {
            push_text(&mut out, text, child)?;
            inserted = true;
        }
        push_text(&mut out, keep_after, child)?;
    }
    if !inserted {
        // An empty paragraph, or a point between two atoms: unmarked text.
        out.push(schema.text(text).map_err(collab)?);
    }
    Ok(textblock.copy_with_content(Fragment::from_children(out)))
}
