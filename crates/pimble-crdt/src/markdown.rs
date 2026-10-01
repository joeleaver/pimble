//! Markdown in and out of a node's content (docs/MCP_CONTRACT.md "Markdown, in and
//! out"), and the edits that touch only the blocks they name ("Edits that touch only
//! what they name").
//!
//! The conversion is rinch's own (`rinch_editor_core::serialize::markdown`), between
//! Markdown and the editor model, so the model that comes out of a document is the
//! model an edit goes back in through: a block nobody names keeps everything the
//! collaboration scope knows about it, including what [`crate::Block`] cannot say.
//! This module adds what rinch's lenient parser does not do:
//!
//! - [`check`] refuses, with a sentence naming the construct and its line, anything the
//!   collaboration scope cannot hold or the parser would drop. A refusal writes nothing.
//! - The block edits, as functions from the model before to the model after. Each one
//!   rebuilds only the branch it changes, so every other node is the same `Rc` and the
//!   projection's diff (`CollabSession::record_local`, block prefix and suffix by
//!   identity, then a text splice inside a changed block) records exactly the change.
//!
//! A block is named by a **quote**: text that appears in exactly one textblock of the
//! document (at any depth: a list item's paragraph names its list). No match and several
//! matches are both refusals that say so.

use std::rc::Rc;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use rinch_editor_core::serialize::markdown::{doc_from_markdown, doc_to_markdown};
use rinch_editor_core::{Fragment, Node, Schema};

use crate::error::{CrdtError, Result};

/// What stands in for an inline atom (an image, a hard break) in the text a quote is
/// matched against, so a quote never spans one.
const ATOM: char = '\u{FFFC}';

fn refused(sentence: impl Into<String>) -> CrdtError {
    CrdtError::Refused(sentence.into())
}

fn collab(e: impl std::fmt::Display) -> CrdtError {
    CrdtError::Collab(e.to_string())
}

// ── Markdown in and out ──────────────────────────────────────────────────

/// Refuse what a node's content cannot hold: the first such construct, with its
/// 1-based line. `Ok` means [`parse`] keeps everything `md` says.
pub fn check(md: &str) -> Result<()> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    // Turned on to be recognised and refused, never to be parsed into something.
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);
    let line = |offset: usize| md[..offset.min(md.len())].matches('\n').count() + 1;
    // The marks open around the current text: inline code cannot carry any of them.
    let mut open: Vec<&'static str> = Vec::new();
    for (event, range) in Parser::new_ext(md, options).into_offset_iter() {
        match &event {
            Event::Start(Tag::Strong) => open.push("bold"),
            Event::Start(Tag::Emphasis) => open.push("italic"),
            Event::Start(Tag::Strikethrough) => open.push("struck through"),
            Event::Start(Tag::Link { .. }) => open.push("a link"),
            Event::End(TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough | TagEnd::Link) => {
                open.pop();
            }
            Event::Code(_) if !open.is_empty() => {
                return Err(refused(format!(
                    "Line {} puts inline code inside {}; inline code in Pimble cannot also be bold, \
                     italic, struck through or a link, so nothing was written. Put the code \
                     outside it (\"**Run** `make`\", \"see [the docs](url): `make`\").",
                    line(range.start),
                    open.last().unwrap()
                )));
            }
            _ => {}
        }
        let what = match &event {
            Event::Start(Tag::BlockQuote(_)) => Some("a block quote"),
            Event::Start(Tag::Table(_)) => Some("a table"),
            Event::Start(Tag::Image { .. }) => Some("an image"),
            Event::Start(Tag::FootnoteDefinition(_)) | Event::FootnoteReference(_) => Some("a footnote"),
            Event::Start(Tag::HtmlBlock) | Event::Html(_) | Event::InlineHtml(_) => Some("HTML"),
            Event::TaskListMarker(_) => Some("a task list item"),
            Event::Start(Tag::MetadataBlock(_)) => Some("a metadata block"),
            Event::Start(Tag::Link { dest_url, .. }) if !safe_link(dest_url) => Some("a link to an unsafe address"),
            _ => None,
        };
        if let Some(what) = what {
            return Err(refused(format!(
                "Line {} is {what}; Pimble documents cannot hold that yet, so nothing was written.",
                line(range.start)
            )));
        }
    }
    Ok(())
}

/// What rinch's parser keeps a link for (`is_safe_url`, which it does not export):
/// anything but script and data URLs.
fn safe_link(url: &str) -> bool {
    let url: String = url.trim().chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
    !(url.starts_with("javascript:") || url.starts_with("vbscript:") || url.starts_with("data:"))
}

/// `md` as an editor `doc`, after [`check`]. Markdown with no blocks at all is
/// refused, since writing it would change nothing anyone asked for.
pub fn parse(schema: &Schema, md: &str) -> Result<Node> {
    check(md)?;
    if md.trim().is_empty() {
        return Err(refused("There is no Markdown to write."));
    }
    doc_from_markdown(schema, md).map_err(collab)
}

/// An editor `doc` as Markdown. Marks Markdown has no syntax for (underline,
/// highlight, text colour, sub/superscript) come out as plain text until rinch writes
/// them as HTML tags.
pub fn render(doc: &Node) -> String {
    doc_to_markdown(doc)
}

// ── Naming a block ───────────────────────────────────────────────────────

/// The text a quote is matched against: a textblock's text, atoms as [`ATOM`].
fn textblock_text(textblock: &Node) -> String {
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

/// A quote's one occurrence: the textblock's path from the `doc`, and the byte range
/// of the quote in [`textblock_text`].
struct Found {
    path: Vec<usize>,
    start: usize,
    end: usize,
}

/// The one place `quote` appears in `doc`, among textblocks `keep` accepts.
fn find_quote(doc: &Node, quote: &str, keep: impl Fn(&Node) -> bool, what: &str) -> Result<Found> {
    if quote.is_empty() {
        return Err(refused(format!("Name the {what} with some of its text.")));
    }
    let mut found: Option<Found> = None;
    let mut count = 0;
    for (path, block) in textblocks(doc) {
        if !keep(&block) {
            continue;
        }
        let text = textblock_text(&block);
        for (start, _) in text.match_indices(quote) {
            count += 1;
            if found.is_none() {
                found = Some(Found { path: path.clone(), start, end: start + quote.len() });
            }
        }
    }
    match (count, found) {
        (1, Some(found)) => Ok(found),
        (0, _) => Err(refused(format!("No {what} in this node contains \"{quote}\"."))),
        (n, _) => Err(refused(format!(
            "\"{quote}\" appears {n} times in this node; quote more of the {what} so it names one."
        ))),
    }
}

/// The top-level block a quote names.
fn block_index(doc: &Node, quote: &str) -> Result<usize> {
    Ok(find_quote(doc, quote, |_| true, "block")?.path[0])
}

/// The top-level heading a quote names, and its level.
fn heading_index(doc: &Node, quote: &str) -> Result<(usize, i64)> {
    let found = find_quote(doc, quote, |b| b.type_name() == "heading", "heading")?;
    if found.path.len() != 1 {
        return Err(refused(format!("The heading \"{quote}\" is inside another block, so it has no section.")));
    }
    let index = found.path[0];
    Ok((index, doc.child(index).attrs().get_int("level").unwrap_or(1)))
}

/// Where the section under the heading at `index` ends: the next top-level heading of
/// its level or higher, or the end of the document.
fn section_end(doc: &Node, index: usize, level: i64) -> usize {
    (index + 1..doc.child_count())
        .find(|&i| {
            let block = doc.child(i);
            block.type_name() == "heading" && block.attrs().get_int("level").unwrap_or(1) <= level
        })
        .unwrap_or(doc.child_count())
}

// ── The edits, model before to model after ───────────────────────────────

/// Whether `doc` holds nothing anyone wrote: one empty paragraph (what a node's
/// content is before anything is typed in it).
pub(crate) fn is_blank(doc: &Node) -> bool {
    doc.child_count() == 1 && doc.child(0).type_name() == "paragraph" && doc.child(0).content().is_empty()
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

/// After the top-level block a quote names; with `after_section` and a heading named,
/// after the section under that heading.
pub(crate) fn insert_after(schema: &Rc<Schema>, doc: &Node, quote: &str, after_section: bool, md: &str) -> Result<Node> {
    let at = if after_section {
        let (index, level) = heading_index(doc, quote)?;
        section_end(doc, index, level)
    } else {
        block_index(doc, quote)? + 1
    };
    let blocks = parsed_blocks(schema, md)?;
    Ok(splice(doc, at, at, &blocks))
}

/// The blocks under the heading a quote names (the heading itself stays), up to the
/// next heading of its level or higher.
pub(crate) fn replace_section(schema: &Rc<Schema>, doc: &Node, heading: &str, md: &str) -> Result<Node> {
    let (index, level) = heading_index(doc, heading)?;
    let end = section_end(doc, index, level);
    let blocks = parsed_blocks(schema, md)?;
    Ok(splice(doc, index + 1, end, &blocks))
}

/// `doc` with `children` as its blocks, one empty paragraph when there are none (an
/// editor document always has a block).
fn with_blocks(schema: &Rc<Schema>, doc: &Node, mut children: Vec<Node>) -> Result<Node> {
    if children.is_empty() {
        children.push(schema.branch("paragraph", Fragment::empty()).map_err(collab)?);
    }
    Ok(doc.copy_with_content(Fragment::from_children(children)))
}

/// The top-level block a quote names, replaced by `md`; an empty `md` deletes it.
/// What removes or re-levels one heading, or turns a paragraph into a list.
pub(crate) fn replace_block(schema: &Rc<Schema>, doc: &Node, quote: &str, md: &str) -> Result<Node> {
    let index = block_index(doc, quote)?;
    let blocks = if md.trim().is_empty() { Vec::new() } else { parsed_blocks(schema, md)? };
    let after = splice(doc, index, index + 1, &blocks);
    with_blocks(schema, &after, after.content().children().to_vec())
}

/// The heading a quote names and everything under it, up to the next heading of its
/// level or higher, removed.
pub(crate) fn remove_section(schema: &Rc<Schema>, doc: &Node, heading: &str) -> Result<Node> {
    let (index, level) = heading_index(doc, heading)?;
    let end = section_end(doc, index, level);
    let after = splice(doc, index, end, &[]);
    with_blocks(schema, &after, after.content().children().to_vec())
}

/// The whole content replaced by `md`, as the smallest set of block edits: blocks
/// that read the same before and after (the same Markdown) stay the same nodes, and
/// each stretch between them is one step, last stretch first so earlier indices hold.
/// Recorded step by step, an unchanged block between two changed ones is never touched,
/// so anyone typing in it keeps their words and caret, and it keeps whatever Markdown
/// cannot say about it. An empty `md` clears the content.
pub(crate) fn replace_content(schema: &Rc<Schema>, doc: &Node, md: &str) -> Result<Vec<Node>> {
    let new: Vec<Node> = if md.trim().is_empty() { Vec::new() } else { parsed_blocks(schema, md)? };
    let old: Vec<Node> = doc.content().children().to_vec();
    let key = |block: &Node| -> Result<String> {
        Ok(render(&schema.branch("doc", Fragment::from_node(block.clone())).map_err(collab)?))
    };
    let old_keys = old.iter().map(key).collect::<Result<Vec<_>>>()?;
    let new_keys = new.iter().map(key).collect::<Result<Vec<_>>>()?;
    // A blank document is replaced outright.
    if is_blank(doc) {
        return Ok(vec![with_blocks(schema, doc, new)?]);
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

    let mut steps = Vec::with_capacity(hunks.len());
    let mut current = doc.clone();
    for (from, to, blocks) in hunks.into_iter().rev() {
        current = splice(&current, from, to, &blocks);
        if current.child_count() == 0 {
            current = with_blocks(schema, &current, Vec::new())?;
        }
        steps.push(current.clone());
    }
    Ok(steps)
}

/// The one occurrence of `quote` replaced by `text` inside its textblock. Text the two
/// share at either end is left alone; the rest of the new text takes the marks of the
/// first character it replaces, as typing over a selection does, and everything else
/// keeps its own. An empty `text` deletes the quote.
pub(crate) fn replace_text(schema: &Rc<Schema>, doc: &Node, quote: &str, text: &str) -> Result<Node> {
    if quote.contains('\n') || text.contains('\n') {
        return Err(refused("Replacing text works inside one paragraph; a quote or replacement with a line break spans more."));
    }
    let found = find_quote(doc, quote, |_| true, "paragraph")?;
    // Only what differs is replaced: the text the quote and its replacement share at
    // either end stays where it is, marks and all (and so does anyone's caret in it).
    let prefix = common_prefix(quote, text);
    let suffix = common_prefix_rev(&quote[prefix..], &text[prefix..]);
    let (start, end) = (found.start + prefix, found.end - suffix);
    let text = &text[prefix..text.len() - suffix];
    if start == end && text.is_empty() {
        return Ok(doc.clone());
    }
    rebuild_at(doc, &found.path, &|textblock| splice_text(schema, textblock, start, end, text))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Rc<Schema> {
        Rc::new(Schema::starter_kit())
    }

    #[test]
    fn check_names_the_construct_and_its_line() {
        let err = check("# Title\n\nsome text\n\n| a | b |\n|---|---|\n| 1 | 2 |\n").unwrap_err();
        assert_eq!(
            err.to_string(),
            "Line 5 is a table; Pimble documents cannot hold that yet, so nothing was written."
        );
        assert!(check("> quoted").unwrap_err().to_string().starts_with("Line 1 is a block quote"));
        assert!(check("a\n\n- [ ] task").unwrap_err().to_string().starts_with("Line 3 is a task list item"));
        assert!(check("see <u>this</u>").unwrap_err().to_string().contains("HTML"));
        assert!(check("![cat](cat.png)").unwrap_err().to_string().contains("an image"));
        assert!(check("[x](javascript:alert(1))").unwrap_err().to_string().contains("unsafe"));
        check("# H\n\n**b** *i* ~~s~~ `c` [l](pimble:a/b)\n\n- one\n  - two\n\n1. x\n\n```rust\nfn x() {}\n```\n\n---\n")
            .unwrap();
    }

    #[test]
    fn dollar_signs_are_text_not_math() {
        check("it cost $5 and then $10").unwrap();
    }

    #[test]
    fn parse_and_render_round_trip_what_is_in_scope() {
        let md = "# Plan\n\nSome **bold** and *italic* text with a [link](pimble:s/n).\n\n- one\n- two\n\n```\ncode\n```";
        let doc = parse(&schema(), md).unwrap();
        assert_eq!(render(&doc), md);
    }

    #[test]
    fn empty_markdown_is_refused() {
        assert!(parse(&schema(), "  \n").unwrap_err().to_string().contains("no Markdown"));
    }

    fn doc(md: &str) -> Node {
        parse(&schema(), md).unwrap()
    }

    #[test]
    fn append_after_content_and_instead_of_a_blank_document() {
        let s = schema();
        let after = append(&s, &doc("first"), "second").unwrap();
        assert_eq!(render(&after), "first\n\nsecond");
        let blank = s.branch("doc", Fragment::from_node(s.branch("paragraph", Fragment::empty()).unwrap())).unwrap();
        assert!(is_blank(&blank));
        assert_eq!(render(&append(&s, &blank, "only").unwrap()), "only");
        assert!(write_new(&s, &doc("taken"), "x").unwrap_err().to_string().contains("already has content"));
    }

    #[test]
    fn insert_after_a_block_or_a_section() {
        let s = schema();
        let d = doc("# First\n\nf1\n\n## Sub\n\ns1\n\n# Second\n\nb1");
        assert_eq!(
            render(&insert_after(&s, &d, "f1", false, "new").unwrap()),
            "# First\n\nf1\n\nnew\n\n## Sub\n\ns1\n\n# Second\n\nb1"
        );
        assert_eq!(
            render(&insert_after(&s, &d, "Sub", true, "new").unwrap()),
            "# First\n\nf1\n\n## Sub\n\ns1\n\nnew\n\n# Second\n\nb1"
        );
        // First's section runs past its subheading to the next level-1 heading.
        assert_eq!(
            render(&insert_after(&s, &d, "First", true, "new").unwrap()),
            "# First\n\nf1\n\n## Sub\n\ns1\n\nnew\n\n# Second\n\nb1"
        );
        // And the last section runs to the end.
        assert_eq!(
            render(&insert_after(&s, &d, "Second", true, "new").unwrap()),
            "# First\n\nf1\n\n## Sub\n\ns1\n\n# Second\n\nb1\n\nnew"
        );
        assert!(insert_after(&s, &d, "f1", true, "x").unwrap_err().to_string().contains("No heading"));
    }

    #[test]
    fn replace_section_keeps_the_heading_and_stops_at_its_level() {
        let s = schema();
        let d = doc("# A\n\na1\n\n## A.1\n\na11\n\n# B\n\nb1");
        assert_eq!(render(&replace_section(&s, &d, "A.1", "fresh").unwrap()), "# A\n\na1\n\n## A.1\n\nfresh\n\n# B\n\nb1");
        assert_eq!(render(&replace_section(&s, &d, "B", "- x\n- y").unwrap()), "# A\n\na1\n\n## A.1\n\na11\n\n# B\n\n- x\n- y");
    }

    #[test]
    fn a_quote_must_name_exactly_one_place() {
        let s = schema();
        let d = doc("apple pie\n\napple tart");
        assert!(replace_text(&s, &d, "apple", "pear").unwrap_err().to_string().contains("appears 2 times"));
        assert!(replace_text(&s, &d, "plum", "pear").unwrap_err().to_string().contains("No paragraph"));
        assert!(replace_section(&s, &d, "apple pie", "x").unwrap_err().to_string().contains("No heading"));
    }

    #[test]
    fn replace_text_keeps_marks_outside_the_quote() {
        let s = schema();
        let d = doc("plain **bold words here** end");
        let after = replace_text(&s, &d, "words", "terms").unwrap();
        assert_eq!(render(&after), "plain **bold terms here** end");
        // The quote spans the bold; only "plain" differs, so the bold stays bold.
        let after = replace_text(&s, &d, "plain bo", "Plainly bo").unwrap();
        assert_eq!(render(&after), "Plainly **bold words here** end");
        // Text that differs across a mark boundary takes the first replaced char's marks.
        let after = replace_text(&s, &d, "plain bold", "a new").unwrap();
        assert_eq!(render(&after), "a new** words here** end");
        let after = replace_text(&s, &d, "here** end", "x").map(|d| render(&d));
        assert!(after.is_err(), "markdown syntax is not in the text");
        // The space before "here" is bold, so it stays bold. (rinch renders a bold run
        // ending in a space as `**... **`, which does not parse back as bold; reported
        // upstream with the Markdown marks PR.)
        let after = replace_text(&s, &d, "here end", "").unwrap();
        assert_eq!(render(&after), "plain **bold words **");
    }

    #[test]
    fn replace_text_reaches_inside_lists() {
        let s = schema();
        let d = doc("- milk\n- eggs\n  - brown");
        assert_eq!(render(&replace_text(&s, &d, "brown", "white").unwrap()), render(&doc("- milk\n- eggs\n  - white")));
        assert!(same_text(&replace_text(&s, &d, "brown", "brown").unwrap(), &d));
    }

    #[test]
    fn inline_code_inside_a_mark_is_refused() {
        for md in ["**`make`**", "**Run `make` now**", "[`foo`](https://x.y)", "~~`x`~~", "*a `b`*"] {
            let err = check(md).unwrap_err().to_string();
            assert!(err.contains("inline code inside"), "{md}: {err}");
        }
        check("**Run** `make`, see [the docs](https://x.y): `foo`").unwrap();
    }

    #[test]
    fn replace_block_re_levels_or_removes_one_block() {
        let s = schema();
        let d = doc("# Title\n\nintro\n\n## Old heading\n\nbody");
        assert_eq!(render(&replace_block(&s, &d, "Old heading", "### New heading").unwrap()), "# Title\n\nintro\n\n### New heading\n\nbody");
        assert_eq!(render(&replace_block(&s, &d, "Old heading", "").unwrap()), "# Title\n\nintro\n\nbody");
        assert_eq!(render(&replace_block(&s, &d, "intro", "- a\n- b").unwrap()), "# Title\n\n- a\n- b\n\n## Old heading\n\nbody");
    }

    #[test]
    fn remove_section_takes_the_heading_and_its_body() {
        let s = schema();
        let d = doc("# A\n\na1\n\n## A.x\n\nax\n\n# B\n\nb1");
        assert_eq!(render(&remove_section(&s, &d, "A.x").unwrap()), "# A\n\na1\n\n# B\n\nb1");
        assert_eq!(render(&remove_section(&s, &d, "B").unwrap()), "# A\n\na1\n\n## A.x\n\nax");
        let only = doc("# Only\n\ntext");
        assert!(is_blank(&remove_section(&s, &only, "Only").unwrap()));
    }

    #[test]
    fn replace_content_keeps_unchanged_blocks_as_the_same_nodes() {
        let s = schema();
        let d = doc("one\n\ntwo\n\nthree\n\nfour");
        let steps = replace_content(&s, &d, "one\n\n2\n\nthree\n\nfour\n\nfive").unwrap();
        let after = steps.last().unwrap();
        assert_eq!(render(after), "one\n\n2\n\nthree\n\nfour\n\nfive");
        assert!(after.child(0).same_ref(d.child(0)));
        assert!(after.child(2).same_ref(d.child(2)));
        assert!(after.child(3).same_ref(d.child(3)));
        assert_eq!(steps.len(), 2, "two separate stretches changed");
        // Reordering, deleting everything, and the same content.
        let swapped = replace_content(&s, &d, "four\n\none\n\ntwo\n\nthree").unwrap();
        assert_eq!(render(swapped.last().unwrap()), "four\n\none\n\ntwo\n\nthree");
        assert!(is_blank(replace_content(&s, &d, "").unwrap().last().unwrap()));
        assert!(replace_content(&s, &d, "one\n\ntwo\n\nthree\n\nfour").unwrap().is_empty());
    }

    fn same_text(a: &Node, b: &Node) -> bool {
        render(a) == render(b)
    }

    #[test]
    fn edits_leave_unnamed_blocks_the_same_nodes() {
        let s = schema();
        let d = doc("one\n\ntwo\n\nthree");
        let after = replace_text(&s, &d, "two", "2").unwrap();
        assert!(after.child(0).same_ref(d.child(0)));
        assert!(!after.child(1).same_ref(d.child(1)));
        assert!(after.child(2).same_ref(d.child(2)));
    }
}
