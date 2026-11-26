//! Naming a heading and its section: by some of its text, by a heading path, or
//! by its block number (docs/MCP_CONTRACT.md "Naming what an edit touches").

use rinch_editor_core::{Fragment, Node, Schema};

use super::quotes::{block_count, matches, textblock_text};
use super::{collab, refused, render};
use crate::error::Result;

/// How an edit names a heading: some of its text or a heading path
/// ([`find_heading`]), or its top-level block number, as get_node's outline
/// numbers it, with optionally some text that must be in that heading now
/// (`expect`, as [`super::BlockTarget::Index`] takes it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadingTarget<'a> {
    Name(&'a str),
    Index { index: usize, expect: Option<&'a str> },
}

impl<'a> HeadingTarget<'a> {
    /// The heading at `index`, with nothing expected of it.
    pub fn index(index: usize) -> Self {
        HeadingTarget::Index { index, expect: None }
    }
}

/// A heading's text made loose for comparing: lowercase, curly apostrophes and
/// quotes straightened and then dropped with every other punctuation mark,
/// whitespace collapsed. "The County’s Edges!" and "the countys edges" read the
/// same. The search index compares a query with titles and headings this way too.
pub fn loose(text: &str) -> String {
    let mut out = String::new();
    for word in text.split_whitespace() {
        let word: String = word.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect();
        if word.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&word);
    }
    out
}

/// One top-level heading of a document and the section under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    /// Its block's index among the document's top-level blocks.
    pub index: usize,
    /// 1 for `#`, 2 for `##`, ...
    pub level: i64,
    /// Its text (an atom in it as U+FFFC).
    pub text: String,
    /// The headings it sits under, then itself: what a heading path names.
    pub path: Vec<String>,
    /// Where its section ends: the next top-level heading of its level or
    /// higher, or the end of the document (exclusive).
    pub end: usize,
}

/// Every top-level heading of `doc`, in order, with its path and section.
pub fn headings(doc: &Node) -> Vec<Heading> {
    let mut out: Vec<Heading> = Vec::new();
    let mut stack: Vec<(i64, String)> = Vec::new();
    for (index, block) in doc.content().iter().enumerate() {
        if block.type_name() != "heading" {
            continue;
        }
        let level = block.attrs().get_int("level").unwrap_or(1);
        let text = textblock_text(block);
        while stack.last().is_some_and(|(l, _)| *l >= level) {
            stack.pop();
        }
        stack.push((level, text.clone()));
        out.push(Heading { index, level, text, path: stack.iter().map(|(_, t)| t.clone()).collect(), end: section_end(doc, index, level) });
    }
    out
}

/// How a heading path is written where one is shown (search hits, outlines):
/// the headings from the outermost in, joined by " > ". Naming a section takes
/// it as it is, or joined by "/".
pub const PATH_SEPARATOR: &str = " > ";

/// The headings among `among` one segment of a name names: those whose loose
/// text equals the segment's, when any does; else those containing it as
/// written; else those containing it in any case.
fn named_by<'a>(among: impl Iterator<Item = &'a Heading> + Clone, segment: &str) -> Vec<&'a Heading> {
    let segment = segment.trim();
    if segment.is_empty() {
        return Vec::new();
    }
    let wanted = loose(segment);
    let exact: Vec<&Heading> = among.clone().filter(|h| !wanted.is_empty() && loose(&h.text) == wanted).collect();
    if !exact.is_empty() {
        return exact;
    }
    let contains: Vec<&Heading> = among.clone().filter(|h| h.text.contains(segment)).collect();
    if !contains.is_empty() {
        return contains;
    }
    let lower = segment.to_lowercase();
    among.filter(|h| h.text.to_lowercase().contains(&lower)).collect()
}

/// The headings a path of segments names: the first among all, each next one
/// inside the section of the one before.
fn named_by_path<'a>(all: &'a [Heading], segments: &[&str]) -> Vec<&'a Heading> {
    let mut found: Vec<&Heading> = named_by(all.iter(), segments[0]);
    for segment in &segments[1..] {
        let mut next: Vec<&Heading> = Vec::new();
        for outer in &found {
            let inside = all.iter().filter(|h| h.index > outer.index && h.index < outer.end);
            for h in named_by(inside, segment) {
                if !next.iter().any(|n| n.index == h.index) {
                    next.push(h);
                }
            }
        }
        found = next;
    }
    found
}

/// The segments of a heading path written with `" > "` or `"/"`, or none when
/// `name` has neither.
fn path_segments(name: &str) -> Option<Vec<&str>> {
    let segments: Vec<&str> = if name.contains('>') { name.split('>').collect() } else { name.split('/').collect() };
    (segments.len() > 1 && segments.iter().all(|s| !s.trim().is_empty())).then_some(segments)
}

/// The heading a name names last: the last segment of a heading path, or the
/// whole name when it is not one.
pub(super) fn last_segment(name: &str) -> &str {
    let name = name.trim();
    path_segments(name).and_then(|segments| segments.last().copied()).unwrap_or(name).trim()
}

/// The one top-level heading `name` names: some of its text (a heading whose
/// text is exactly it, ignoring case and punctuation, wins over headings that
/// merely contain it), or a heading path such as "Design/The county's edges"
/// or "Design > The county's edges" for a heading inside another's section.
/// No match and several are refusals; several are listed by their paths.
/// The one way a section is found: reading one, inserting after one,
/// replacing or removing one, and splitting a note by its headings.
pub fn find_heading(doc: &Node, name: &str) -> Result<Heading> {
    let name = name.trim();
    if name.is_empty() {
        return Err(refused("Name the heading with some of its text."));
    }
    let all = headings(doc);
    let mut found = named_by(all.iter(), name);
    if found.len() != 1 {
        if let Some(segments) = path_segments(name) {
            let by_path = named_by_path(&all, &segments);
            if by_path.len() == 1 || (found.is_empty() && !by_path.is_empty()) {
                found = by_path;
            }
        }
    }
    match found.as_slice() {
        [one] => Ok((*one).clone()),
        [] => {
            // A heading inside a quote or a list has no section of its own.
            if !matches(doc, name, |b| b.type_name() == "heading").is_empty() {
                return Err(refused(format!("The heading \"{name}\" is inside another block, so it has no section.")));
            }
            Err(refused(format!("No heading in this node contains \"{name}\".")))
        }
        several => {
            let listed: Vec<String> =
                several.iter().map(|h| format!("\"{}\" (block {})", h.path.join(PATH_SEPARATOR), h.index)).collect();
            Err(refused(format!(
                "\"{name}\" names {} headings in this node: {}. Give one of these heading paths, or its block number, so it names one.",
                several.len(),
                listed.join(", ")
            )))
        }
    }
}

/// The one top-level heading `target` names: by name ([`find_heading`]), or
/// the heading at that block number, refused when the block is not a heading
/// or does not hold what `expect` says it should.
pub fn target_heading(doc: &Node, target: HeadingTarget<'_>) -> Result<Heading> {
    let (index, expect) = match target {
        HeadingTarget::Name(name) => return find_heading(doc, name),
        HeadingTarget::Index { index, expect } => (index, expect),
    };
    let count = block_count(doc);
    if index >= count {
        return Err(super::quotes::no_such_block(count, index));
    }
    super::quotes::check_expected(doc, index, expect)?;
    if let Some(h) = headings(doc).into_iter().find(|h| h.index == index) {
        return Ok(h);
    }
    let block = doc.child(index);
    let text = super::echo::excerpt(&super::echo::block_text(block), super::echo::NEIGHBOR);
    Err(refused(format!(
        "Block {index} is a {}, not a heading (\"{text}\"); a section starts at a heading. replace_block replaces or removes one block.",
        super::quotes::kind_of(block)
    )))
}

/// Where the section under the heading at `index` ends: the next top-level heading of
/// its level or higher, or the end of the document.
pub(super) fn section_end(doc: &Node, index: usize, level: i64) -> usize {
    (index + 1..doc.child_count())
        .find(|&i| {
            let block = doc.child(i);
            block.type_name() == "heading" && block.attrs().get_int("level").unwrap_or(1) <= level
        })
        .unwrap_or(doc.child_count())
}

/// The top-level blocks `from..to` of `doc` as Markdown (empty for none).
pub fn blocks_markdown(schema: &Schema, doc: &Node, from: usize, to: usize) -> Result<String> {
    let to = to.min(doc.child_count());
    if from >= to {
        return Ok(String::new());
    }
    let blocks = doc.content().children()[from..to].to_vec();
    Ok(render(&schema.branch("doc", Fragment::from_children(blocks)).map_err(collab)?))
}
