//! A plain description of rich content, for building a [`ContentDoc`](crate::ContentDoc)
//! from outside the editor: importers (Scrivener RTF), the CLI, tests.
//!
//! The vocabulary is exactly what rinch's collaboration projection accepts and the
//! app's editor renders: flat text blocks (paragraph, heading, code block) and nested
//! bullet/ordered lists, with the starter-kit marks. Anything outside it (block
//! quotes, tables, images, hard breaks) has no variant here on purpose: a document
//! built from these blocks always projects, so an import never produces a node the
//! editor refuses to open.

use std::rc::Rc;

use rinch_editor_core::{AttrValue, Attrs, Fragment, Mark as EditorMark, Node, Schema};

use crate::error::{CrdtError, Result};

/// One top-level block of a document, or one block inside a list item.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Paragraph { runs: Vec<Run>, align: Align, indent: u32 },
    Heading { level: u8, runs: Vec<Run> },
    CodeBlock { text: String },
    BulletList { items: Vec<ListItem> },
    OrderedList { start: i64, items: Vec<ListItem> },
}

impl Block {
    /// A left-aligned, unindented paragraph.
    pub fn paragraph(runs: Vec<Run>) -> Self {
        Block::Paragraph { runs, align: Align::Left, indent: 0 }
    }

    /// A paragraph of unmarked text.
    pub fn plain(text: impl Into<String>) -> Self {
        Self::paragraph(vec![Run::plain(text)])
    }

    /// The block's text with no formatting, list items joined by newlines.
    pub fn plain_text(&self) -> String {
        match self {
            Block::Paragraph { runs, .. } | Block::Heading { runs, .. } => runs_text(runs),
            Block::CodeBlock { text } => text.clone(),
            Block::BulletList { items } | Block::OrderedList { items, .. } => items
                .iter()
                .map(|item| item.blocks.iter().map(Block::plain_text).collect::<Vec<_>>().join("\n"))
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

/// One entry of a list: a paragraph, usually, then any nested list.
#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    pub blocks: Vec<Block>,
}

/// A stretch of text sharing one set of marks.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub text: String,
    pub marks: Vec<Mark>,
}

impl Run {
    pub fn plain(text: impl Into<String>) -> Self {
        Run { text: text.into(), marks: Vec::new() }
    }

    pub fn marked(text: impl Into<String>, marks: Vec<Mark>) -> Self {
        Run { text: text.into(), marks }
    }
}

/// An inline mark, named as the starter-kit schema names it.
#[derive(Debug, Clone, PartialEq)]
pub enum Mark {
    Bold,
    Italic,
    Underline,
    Strike,
    Code,
    Link { href: String },
    /// A background colour as CSS (`#rrggbb`); `None` is the default highlight.
    Highlight { color: Option<String> },
    /// A text colour as CSS (`#rrggbb`).
    TextColor { color: String },
    Subscript,
    Superscript,
}

impl Mark {
    fn name(&self) -> &'static str {
        match self {
            Mark::Bold => "bold",
            Mark::Italic => "italic",
            Mark::Underline => "underline",
            Mark::Strike => "strike",
            Mark::Code => "code",
            Mark::Link { .. } => "link",
            Mark::Highlight { .. } => "highlight",
            Mark::TextColor { .. } => "text_color",
            Mark::Subscript => "subscript",
            Mark::Superscript => "superscript",
        }
    }

    fn attrs(&self) -> Attrs {
        match self {
            Mark::Link { href } => Attrs::new().with("href", href.clone()),
            Mark::Highlight { color: Some(color) } => Attrs::new().with("color", color.clone()),
            Mark::TextColor { color } => Attrs::new().with("color", color.clone()),
            _ => Attrs::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

impl Align {
    fn as_str(self) -> &'static str {
        match self {
            Align::Left => "left",
            Align::Center => "center",
            Align::Right => "right",
            Align::Justify => "justify",
        }
    }
}

fn runs_text(runs: &[Run]) -> String {
    runs.iter().map(|r| r.text.as_str()).collect()
}

/// One paragraph per line of `text` (a blank line is an empty paragraph; empty `text`
/// is one empty paragraph), unmarked.
pub fn blocks_from_plain_text(text: &str) -> Vec<Block> {
    text.split('\n').map(Block::plain).collect()
}

/// Build the editor's `doc` node for `blocks`. An empty slice becomes one empty
/// paragraph, since an editor document is never empty.
pub(crate) fn build_doc(schema: &Rc<Schema>, blocks: &[Block]) -> Result<Node> {
    let mut children = Vec::with_capacity(blocks.len().max(1));
    for block in blocks {
        children.push(build_block(schema, block)?);
    }
    if children.is_empty() {
        children.push(build_block(schema, &Block::plain(""))?);
    }
    schema
        .branch("doc", Fragment::from_children(children))
        .map_err(|e| CrdtError::Collab(e.to_string()))
}

fn build_block(schema: &Rc<Schema>, block: &Block) -> Result<Node> {
    let collab = |e: rinch_editor_core::EditorError| CrdtError::Collab(e.to_string());
    match block {
        Block::Paragraph { runs, align, indent } => {
            let mut attrs = Attrs::new();
            if *align != Align::Left {
                attrs = attrs.with("text_align", align.as_str().to_string());
            }
            if *indent > 0 {
                attrs = attrs.with("indent", AttrValue::Int(*indent as i64));
            }
            schema.create_node("paragraph", attrs, build_runs(schema, runs)?).map_err(collab)
        }
        Block::Heading { level, runs } => {
            let level = (*level).clamp(1, 6) as i64;
            let attrs = Attrs::new().with("level", AttrValue::Int(level));
            schema.create_node("heading", attrs, build_runs(schema, runs)?).map_err(collab)
        }
        Block::CodeBlock { text } => {
            let content = if text.is_empty() {
                Fragment::empty()
            } else {
                Fragment::from_node(schema.text(text).map_err(collab)?)
            };
            schema.branch("code_block", content).map_err(collab)
        }
        Block::BulletList { items } => {
            schema.branch("bullet_list", build_items(schema, items)?).map_err(collab)
        }
        Block::OrderedList { start, items } => {
            let mut attrs = Attrs::new();
            if *start != 1 {
                attrs = attrs.with("start", AttrValue::Int(*start));
            }
            schema.create_node("ordered_list", attrs, build_items(schema, items)?).map_err(collab)
        }
    }
}

fn build_items(schema: &Rc<Schema>, items: &[ListItem]) -> Result<Fragment> {
    let collab = |e: rinch_editor_core::EditorError| CrdtError::Collab(e.to_string());
    let mut nodes = Vec::with_capacity(items.len());
    for item in items {
        let mut blocks = Vec::with_capacity(item.blocks.len().max(1));
        for block in &item.blocks {
            blocks.push(build_block(schema, block)?);
        }
        if blocks.is_empty() {
            blocks.push(build_block(schema, &Block::plain(""))?);
        }
        nodes.push(schema.branch("list_item", Fragment::from_children(blocks)).map_err(collab)?);
    }
    if nodes.is_empty() {
        // A list needs at least one item; an empty one is a paragraph's worth of nothing.
        let empty = build_block(schema, &Block::plain(""))?;
        nodes.push(schema.branch("list_item", Fragment::from_node(empty)).map_err(collab)?);
    }
    Ok(Fragment::from_children(nodes))
}

fn build_runs(schema: &Rc<Schema>, runs: &[Run]) -> Result<Fragment> {
    let collab = |e: rinch_editor_core::EditorError| CrdtError::Collab(e.to_string());
    let mut nodes = Vec::with_capacity(runs.len());
    for run in runs {
        if run.text.is_empty() {
            continue;
        }
        let mut marks = Vec::with_capacity(run.marks.len());
        for mark in &run.marks {
            let typ = schema
                .mark_type(mark.name())
                .ok_or_else(|| CrdtError::Collab(format!("schema has no mark `{}`", mark.name())))?
                .clone();
            marks.push(EditorMark::new(typ, mark.attrs()));
        }
        nodes.push(schema.text_with_marks(&run.text, marks).map_err(collab)?);
    }
    Ok(Fragment::from_children(nodes))
}
