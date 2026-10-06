//! A plain description of rich content, for building a [`ContentDoc`](crate::ContentDoc)
//! from outside the editor: importers (Scrivener RTF), the CLI, tests.
//!
//! The vocabulary is exactly what rinch's collaboration projection accepts and the
//! app's editor renders: text blocks (paragraph, heading, code block), nested
//! bullet/ordered lists, block quotes, tables and horizontal rules, with the
//! starter-kit marks and, inside a paragraph or heading, the inline atoms (an image, a
//! hard break). Anything outside it (task lists) has no variant here on purpose: a
//! document built from these blocks always projects, so an import never produces a
//! node the editor refuses to open.

use std::rc::Rc;

use rinch_editor_core::{AttrValue, Attrs, Fragment, Mark as EditorMark, Node, Schema};

use crate::error::{CrdtError, Result};

/// One top-level block of a document, or one block inside a list item.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Paragraph { runs: Vec<Inline>, align: Align, indent: u32 },
    Heading { level: u8, runs: Vec<Inline> },
    CodeBlock { text: String },
    BulletList { items: Vec<ListItem> },
    OrderedList { start: i64, items: Vec<ListItem> },
    Blockquote { blocks: Vec<Block> },
    Table { rows: Vec<TableRow> },
    /// A rule across the page (a scene break): a block with nothing in it.
    HorizontalRule,
}

impl Block {
    /// A left-aligned, unindented paragraph of text runs, images and hard
    /// breaks (a `Vec<Run>` does as well as a `Vec<Inline>`).
    pub fn paragraph<I: Into<Inline>>(runs: Vec<I>) -> Self {
        Block::Paragraph { runs: runs.into_iter().map(Into::into).collect(), align: Align::Left, indent: 0 }
    }

    /// A paragraph of unmarked text.
    pub fn plain(text: impl Into<String>) -> Self {
        Self::paragraph(vec![Run::plain(text)])
    }

    /// The block's text with no formatting, list items joined by newlines. A
    /// hard break reads as a newline; an image and a rule read as nothing.
    pub fn plain_text(&self) -> String {
        match self {
            Block::HorizontalRule => String::new(),
            Block::Paragraph { runs, .. } | Block::Heading { runs, .. } => runs_text(runs),
            Block::CodeBlock { text } => text.clone(),
            Block::BulletList { items } | Block::OrderedList { items, .. } => items
                .iter()
                .map(|item| item.blocks.iter().map(Block::plain_text).collect::<Vec<_>>().join("\n"))
                .collect::<Vec<_>>()
                .join("\n"),
            Block::Blockquote { blocks } => blocks_text(blocks),
            Block::Table { rows } => rows
                .iter()
                .map(|row| row.cells.iter().map(|cell| blocks_text(&cell.blocks)).collect::<Vec<_>>().join("\t"))
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

fn blocks_text(blocks: &[Block]) -> String {
    blocks.iter().map(Block::plain_text).collect::<Vec<_>>().join("\n")
}

/// One row of a table.
#[derive(Debug, Clone, PartialEq)]
pub struct TableRow {
    pub cells: Vec<TableCell>,
}

/// One cell of a table: a paragraph, usually. A cell spanning several columns or rows
/// says so, and the cells it covers are left out of their rows, as in HTML.
#[derive(Debug, Clone, PartialEq)]
pub struct TableCell {
    pub header: bool,
    pub colspan: u32,
    pub rowspan: u32,
    pub blocks: Vec<Block>,
}

impl TableCell {
    /// A body cell holding `blocks`, spanning nothing.
    pub fn new(blocks: Vec<Block>) -> Self {
        TableCell { header: false, colspan: 1, rowspan: 1, blocks }
    }

    /// A header cell holding `blocks`, spanning nothing.
    pub fn header(blocks: Vec<Block>) -> Self {
        TableCell { header: true, ..Self::new(blocks) }
    }
}

/// One entry of a list: a paragraph, usually, then any nested list.
#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    pub blocks: Vec<Block>,
}

/// One piece of a paragraph's or a heading's content: a run of text, or one
/// of the inline atoms rinch's collaboration scope holds.
#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text(Run),
    Image(Image),
    /// A line break inside the block (Shift+Enter). The marks of the text
    /// around it are not kept on it: they change nothing a reader sees.
    HardBreak,
}

impl Inline {
    /// The run, when this is text.
    pub fn as_run(&self) -> Option<&Run> {
        match self {
            Inline::Text(run) => Some(run),
            _ => None,
        }
    }

    /// The image, when this is one.
    pub fn as_image(&self) -> Option<&Image> {
        match self {
            Inline::Image(image) => Some(image),
            _ => None,
        }
    }
}

impl From<Run> for Inline {
    fn from(run: Run) -> Self {
        Inline::Text(run)
    }
}

impl From<Image> for Inline {
    fn from(image: Image) -> Self {
        Inline::Image(image)
    }
}

/// A picture in the text: rinch's `image` atom. `src` is a URL, for a picture
/// kept in a store a `pimble-blob:` one (`pimble_core::BlobUrl`); `alt` and
/// `title` are empty when the image has none. An image can carry marks (a
/// link makes it clickable).
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub src: String,
    pub alt: String,
    pub title: String,
    pub marks: Vec<Mark>,
}

impl Image {
    /// An image with no title and no marks.
    pub fn new(src: impl Into<String>, alt: impl Into<String>) -> Self {
        Image { src: src.into(), alt: alt.into(), title: String::new(), marks: Vec::new() }
    }
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

    /// The reverse of [`Mark::name`] and [`Mark::attrs`]: an editor mark as one of
    /// ours. An error for a mark this vocabulary has no variant for.
    fn from_editor(mark: &EditorMark) -> Result<Self> {
        let color = || mark.attrs.get_str("color").filter(|c| !c.is_empty()).map(str::to_string);
        Ok(match mark.type_name() {
            "bold" => Mark::Bold,
            "italic" => Mark::Italic,
            "underline" => Mark::Underline,
            "strike" => Mark::Strike,
            "code" => Mark::Code,
            "link" => Mark::Link { href: mark.attrs.get_str("href").unwrap_or_default().to_string() },
            "highlight" => Mark::Highlight { color: color() },
            "text_color" => Mark::TextColor { color: color().unwrap_or_default() },
            "subscript" => Mark::Subscript,
            "superscript" => Mark::Superscript,
            other => return Err(CrdtError::Collab(format!("mark `{other}` has no `Mark` variant"))),
        })
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

    /// Anything the editor does not write reads as the default, as the editor reads it.
    fn from_attr(value: Option<&str>) -> Self {
        match value {
            Some("center") => Align::Center,
            Some("right") => Align::Right,
            Some("justify") => Align::Justify,
            _ => Align::Left,
        }
    }
}

fn runs_text(runs: &[Inline]) -> String {
    runs.iter()
        .map(|inline| match inline {
            Inline::Text(run) => run.text.as_str(),
            Inline::HardBreak => "\n",
            Inline::Image(_) => "",
        })
        .collect()
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
        Block::Blockquote { blocks } => {
            schema.branch("blockquote", build_blocks(schema, blocks)?).map_err(collab)
        }
        Block::Table { rows } => {
            let mut row_nodes = Vec::with_capacity(rows.len().max(1));
            for row in rows {
                let mut cells = Vec::with_capacity(row.cells.len());
                for cell in &row.cells {
                    let mut attrs = Attrs::new();
                    if cell.colspan > 1 {
                        attrs = attrs.with("colspan", AttrValue::Int(cell.colspan as i64));
                    }
                    if cell.rowspan > 1 {
                        attrs = attrs.with("rowspan", AttrValue::Int(cell.rowspan as i64));
                    }
                    let name = if cell.header { "table_header_cell" } else { "table_cell" };
                    cells.push(schema.create_node(name, attrs, build_blocks(schema, &cell.blocks)?).map_err(collab)?);
                }
                row_nodes.push(schema.branch("table_row", Fragment::from_children(cells)).map_err(collab)?);
            }
            if row_nodes.is_empty() {
                // A table needs a row; one with no rows is one empty cell.
                let cell = schema.branch("table_cell", build_blocks(schema, &[])?).map_err(collab)?;
                row_nodes.push(schema.branch("table_row", Fragment::from_node(cell)).map_err(collab)?);
            }
            schema.branch("table", Fragment::from_children(row_nodes)).map_err(collab)
        }
        Block::HorizontalRule => schema.branch("horizontal_rule", Fragment::empty()).map_err(collab),
    }
}

/// The blocks of a container that needs at least one (a quote, a cell): an empty
/// slice is one empty paragraph.
fn build_blocks(schema: &Rc<Schema>, blocks: &[Block]) -> Result<Fragment> {
    let mut nodes = Vec::with_capacity(blocks.len().max(1));
    for block in blocks {
        nodes.push(build_block(schema, block)?);
    }
    if nodes.is_empty() {
        nodes.push(build_block(schema, &Block::plain(""))?);
    }
    Ok(Fragment::from_children(nodes))
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

fn build_runs(schema: &Rc<Schema>, runs: &[Inline]) -> Result<Fragment> {
    let collab = |e: rinch_editor_core::EditorError| CrdtError::Collab(e.to_string());
    let mut nodes = Vec::with_capacity(runs.len());
    for inline in runs {
        match inline {
            Inline::Text(run) if run.text.is_empty() => {}
            Inline::Text(run) => {
                nodes.push(schema.text_with_marks(&run.text, build_marks(schema, &run.marks)?).map_err(collab)?);
            }
            Inline::Image(image) => {
                let mut attrs = Attrs::new().with("src", image.src.clone());
                if !image.alt.is_empty() {
                    attrs = attrs.with("alt", image.alt.clone());
                }
                if !image.title.is_empty() {
                    attrs = attrs.with("title", image.title.clone());
                }
                let node = schema.create_node("image", attrs, Fragment::empty()).map_err(collab)?;
                nodes.push(node.with_marks(build_marks(schema, &image.marks)?));
            }
            Inline::HardBreak => nodes.push(schema.branch("hard_break", Fragment::empty()).map_err(collab)?),
        }
    }
    Ok(Fragment::from_children(nodes))
}

fn build_marks(schema: &Rc<Schema>, marks: &[Mark]) -> Result<Vec<EditorMark>> {
    marks
        .iter()
        .map(|mark| {
            let typ = schema
                .mark_type(mark.name())
                .ok_or_else(|| CrdtError::Collab(format!("schema has no mark `{}`", mark.name())))?
                .clone();
            Ok(EditorMark::new(typ, mark.attrs()))
        })
        .collect()
}

// ── Reading: an editor document back to blocks ───────────────────────────

/// The blocks of an editor `doc` node: the reverse of [`build_doc`], for reading a
/// document's content from outside the editor (an export, a test's assertion).
///
/// [`Block`] is the importer's vocabulary and is narrower than the schema in four
/// places, all read here without complaint and without the value: a heading's
/// `text_align` and `indent`, a code block's `language`, a link's `title` and
/// `target`, and the marks on a hard break. So this is not how content is carried from one document to another (a
/// transplant goes through the editor's own model and loses none of them, see
/// `content_doc::fresh_snapshot`); it is a faithful reading of everything `Block` can
/// say. Runs come back canonical: adjacent text with the same marks is one run, marks
/// in the order the projection keeps them (by name), and empty text is no run. A node
/// or mark with no variant here is an error, never a silent drop.
pub(crate) fn read_doc(doc: &Node) -> Result<Vec<Block>> {
    doc.content().iter().map(read_block).collect()
}

fn read_block(node: &Node) -> Result<Block> {
    Ok(match node.type_name() {
        "paragraph" => Block::Paragraph {
            runs: read_runs(node)?,
            align: Align::from_attr(node.attrs().get_str("text_align")),
            indent: node.attrs().get_int("indent").unwrap_or(0).clamp(0, u32::MAX as i64) as u32,
        },
        "heading" => Block::Heading {
            level: node.attrs().get_int("level").unwrap_or(1).clamp(1, 6) as u8,
            runs: read_runs(node)?,
        },
        // A code block holds text and nothing else (the schema allows no atom in one).
        "code_block" => Block::CodeBlock { text: runs_text(&read_runs(node)?) },
        "horizontal_rule" => Block::HorizontalRule,
        "bullet_list" => Block::BulletList { items: read_items(node)? },
        "ordered_list" => {
            Block::OrderedList { start: node.attrs().get_int("start").unwrap_or(1), items: read_items(node)? }
        }
        "blockquote" => Block::Blockquote { blocks: node.content().iter().map(read_block).collect::<Result<_>>()? },
        "table" => Block::Table { rows: node.content().iter().map(read_row).collect::<Result<_>>()? },
        other => return Err(CrdtError::Collab(format!("block `{other}` has no `Block` variant"))),
    })
}

fn read_row(row: &Node) -> Result<TableRow> {
    let span = |cell: &Node, name: &str| cell.attrs().get_int(name).unwrap_or(1).clamp(1, u32::MAX as i64) as u32;
    let cells = row
        .content()
        .iter()
        .map(|cell| {
            let header = match cell.type_name() {
                "table_cell" => false,
                "table_header_cell" => true,
                other => return Err(CrdtError::Collab(format!("`{other}` in a table row has no `Block` variant"))),
            };
            Ok(TableCell {
                header,
                colspan: span(cell, "colspan"),
                rowspan: span(cell, "rowspan"),
                blocks: cell.content().iter().map(read_block).collect::<Result<_>>()?,
            })
        })
        .collect::<Result<_>>()?;
    Ok(TableRow { cells })
}

fn read_items(list: &Node) -> Result<Vec<ListItem>> {
    list.content()
        .iter()
        .map(|item| match item.type_name() {
            "list_item" => Ok(ListItem { blocks: item.content().iter().map(read_block).collect::<Result<_>>()? }),
            other => Err(CrdtError::Collab(format!("`{other}` in a list has no `Block` variant"))),
        })
        .collect()
}

fn read_runs(textblock: &Node) -> Result<Vec<Inline>> {
    let mut runs: Vec<Inline> = Vec::new();
    for child in textblock.content().iter() {
        let read_marks = || child.marks().iter().map(Mark::from_editor).collect::<Result<Vec<_>>>();
        let Some(text) = child.text() else {
            runs.push(match child.type_name() {
                "image" => {
                    let attr = |name| child.attrs().get_str(name).unwrap_or_default().to_string();
                    Inline::Image(Image { src: attr("src"), alt: attr("alt"), title: attr("title"), marks: read_marks()? })
                }
                "hard_break" => Inline::HardBreak,
                other => return Err(CrdtError::Collab(format!("inline `{other}` has no `Block` variant"))),
            });
            continue;
        };
        if text.is_empty() {
            continue;
        }
        let marks = read_marks()?;
        match runs.last_mut() {
            Some(Inline::Text(last)) if last.marks == marks => last.text.push_str(text),
            _ => runs.push(Inline::Text(Run { text: text.to_string(), marks })),
        }
    }
    Ok(runs)
}
