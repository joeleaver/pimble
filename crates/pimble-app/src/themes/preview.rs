//! What the Theme picker's preview shows (docs/THEMES_CONTRACT.md section 9):
//! the note's own text as plain data, read once from the editor holding it
//! when the picker opens, or a few sample lines when no editor holds it.
//!
//! The data is the editor's blocks less what a preview does not need to
//! show (pictures, canvases and sheets in the text, links' targets), so
//! `app/theme_preview.rs` can draw it with the elements the editor draws
//! and `document_theme.css` dresses it exactly as it dresses the note.

use pimble_crdt::text_roles::is_subtitle;
use rinch_editor_core::tables::{self, Rect};
use rinch_editor_core::{Mark, Node as EditorNode};

use crate::paragraph_roles;

/// No more top-level blocks than this are shown: a preview is a page, and a
/// long note would cost a long redraw on every theme the pointer crosses.
pub(crate) const MAX_BLOCKS: usize = 40;

/// Nesting deeper than this (a list in a quote in a table cell ...) is cut:
/// nothing a theme draws differs below it.
const MAX_NESTING: usize = 8;

/// A table shows no more rows than this, for the reason [`MAX_BLOCKS`] gives.
pub(crate) const MAX_TABLE_ROWS: usize = 40;

/// Cells are placed on explicit grid lines only up to this many columns, as
/// the editor does (rinch-editor-view's `MAX_DEFINITE_GRID_TRACKS`): past it
/// they are left to auto-placement, which is what the editor shows then too.
pub(crate) const MAX_PLACED_COLUMNS: usize = 10_000;

/// One block of a preview, as the editor's schema names it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Block {
    /// `classes` are the ones the editor puts on the block
    /// ([`crate::paragraph_roles`]).
    Paragraph { align: Option<&'static str>, subtitle: bool, classes: String, inlines: Vec<Inline> },
    Heading { level: u8, align: Option<&'static str>, classes: String, inlines: Vec<Inline> },
    List { ordered: bool, start: i64, items: Vec<Vec<Block>> },
    Quote(Vec<Block>),
    Code(String),
    /// A table as the editor lays it out: a grid of `columns` tracks (the
    /// editor's `tables::column_count`), each cell on its rectangle in the
    /// table's map, so spans read as they do in the note.
    Table { columns: usize, rows: Vec<Vec<Cell>> },
    Rule,
}

/// One cell of a table row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Cell {
    pub header: bool,
    pub blocks: Vec<Block>,
    /// Its rectangle in the table's grid; none for a cell the map leaves out
    /// (a ragged row's overflow), which the editor does not place either.
    pub rect: Option<Rect>,
}

/// Inline content: a run of text with its marks, or a hard break.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Inline {
    Text(Run),
    Break,
}

/// Raised or lowered text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Script {
    #[default]
    Normal,
    Sub,
    Super,
}

/// A run of text and the marks the editor would draw it with.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub code: bool,
    pub link: bool,
    /// Highlighted, and in the highlight's own color when it names one.
    pub highlight: Option<Option<String>>,
    /// The text color mark's color.
    pub color: Option<String>,
    pub script: Script,
}

/// The blocks of an editor document, the first [`MAX_BLOCKS`] of them.
pub(crate) fn of_doc(doc: &EditorNode) -> Vec<Block> {
    blocks_of(doc.content().children(), MAX_BLOCKS, 0)
}

/// A heading, a subtitle and two paragraphs, for a node no editor holds (the
/// row menu on a note that is not open): enough to show a theme's headings,
/// subtitle, body and paragraph spacing.
pub(crate) fn sample() -> Vec<Block> {
    let run = |text: &str| vec![Inline::Text(Run { text: text.to_string(), ..Run::default() })];
    let para = |text: &str| Block::Paragraph { align: None, subtitle: false, classes: String::new(), inlines: run(text) };
    vec![
        Block::Heading { level: 1, align: None, classes: paragraph_roles::BEFORE_SUBTITLE.to_string(), inlines: run("The Harbor") },
        Block::Paragraph { align: None, subtitle: true, classes: paragraph_roles::UNDER[0].to_string(), inlines: run("A morning on the water") },
        para("The boats came in after noon, low with the morning's catch. Gulls followed them past the breakwater."),
        para("By evening the quay was quiet again, and the lamps came on one by one along the water."),
        Block::Quote(vec![para("The sea gives and the sea takes.")]),
    ]
}

/// The first `limit` of `nodes`, siblings, as blocks. Their classes are read
/// from all of them, as the editor reads them.
fn blocks_of(nodes: &[EditorNode], limit: usize, depth: usize) -> Vec<Block> {
    if depth > MAX_NESTING {
        return Vec::new();
    }
    let classes = paragraph_roles::classes(nodes);
    nodes
        .iter()
        .zip(classes)
        .take(limit)
        .filter_map(|(node, classes)| block_of(node, classes, depth))
        .collect()
}

fn all_of(node: &EditorNode, depth: usize) -> Vec<Block> {
    blocks_of(node.content().children(), usize::MAX, depth)
}

fn children(node: &EditorNode) -> std::slice::Iter<'_, EditorNode> {
    node.content().children().iter()
}

fn block_of(node: &EditorNode, classes: String, depth: usize) -> Option<Block> {
    let block = match node.type_name() {
        "paragraph" => Block::Paragraph { align: align_of(node), subtitle: is_subtitle(node), classes, inlines: inlines_of(node) },
        "heading" => {
            let level = node.attrs().get_int("level").unwrap_or(1).clamp(1, 6) as u8;
            Block::Heading { level, align: align_of(node), classes, inlines: inlines_of(node) }
        }
        "bullet_list" | "task_list" => Block::List { ordered: false, start: 1, items: items_of(node, depth) },
        "ordered_list" => {
            let start = node.attrs().get_int("start").unwrap_or(1);
            Block::List { ordered: true, start, items: items_of(node, depth) }
        }
        "blockquote" => Block::Quote(all_of(node, depth + 1)),
        "code_block" => Block::Code(children(node).filter_map(|n| n.text()).collect()),
        "table" => table_of(node, depth),
        "horizontal_rule" => Block::Rule,
        // Pictures, canvases, sheets and spacers in the text: what they show
        // is theirs, not the theme's.
        _ => return None,
    };
    Some(block)
}

fn table_of(table: &EditorNode, depth: usize) -> Block {
    let rects = tables::cell_rects(table);
    let rows = children(table)
        .zip(rects)
        .take(MAX_TABLE_ROWS)
        .map(|(row, row_rects)| {
            children(row)
                .zip(row_rects)
                .map(|(cell, rect)| Cell { header: cell.type_name() == "table_header_cell", blocks: all_of(cell, depth + 1), rect })
                .collect()
        })
        .collect();
    Block::Table { columns: tables::column_count(table).max(1), rows }
}

fn items_of(list: &EditorNode, depth: usize) -> Vec<Vec<Block>> {
    children(list).map(|item| all_of(item, depth + 1)).collect()
}

/// A textblock's own alignment, when someone set one. An unset alignment is
/// `None`, so the theme decides it; a chosen one, `left` included, wins over
/// the theme, as rinch's editor writes it (an inline `text-align` only for a
/// block that holds one, joeleaver/rinch#2210).
fn align_of(node: &EditorNode) -> Option<&'static str> {
    match node.attrs().get_str("text_align") {
        Some("left") => Some("left"),
        Some("center") => Some("center"),
        Some("right") => Some("right"),
        Some("justify") => Some("justify"),
        _ => None,
    }
}

fn inlines_of(node: &EditorNode) -> Vec<Inline> {
    children(node)
        .filter_map(|child| match (child.text(), child.type_name()) {
            (Some(text), _) => Some(Inline::Text(run_of(text, child.marks()))),
            (None, "hard_break") => Some(Inline::Break),
            _ => None,
        })
        .collect()
}

fn run_of(text: &str, marks: &[Mark]) -> Run {
    let mut run = Run { text: text.to_string(), ..Run::default() };
    for mark in marks {
        match mark.type_name() {
            "bold" => run.bold = true,
            "italic" => run.italic = true,
            "underline" => run.underline = true,
            "strike" => run.strike = true,
            "code" => run.code = true,
            "link" => run.link = true,
            "highlight" => run.highlight = Some(mark.attrs.get_str("color").and_then(hex_color)),
            "text_color" => run.color = mark.attrs.get_str("color").and_then(hex_color),
            "subscript" => run.script = Script::Sub,
            "superscript" => run.script = Script::Super,
            _ => {}
        }
    }
    run
}

/// A `#rgb` or `#rrggbb` color, the only form a preview writes into a style:
/// a mark's attribute can arrive from anyone who edits the note.
fn hex_color(value: &str) -> Option<String> {
    let value = value.trim();
    let hex = value.strip_prefix('#')?;
    ((hex.len() == 3 || hex.len() == 6) && hex.chars().all(|c| c.is_ascii_hexdigit())).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use rinch_editor_core::{Attrs, Fragment, Schema};

    use super::*;

    fn schema() -> Schema {
        Schema::starter_kit()
    }

    fn doc(schema: &Schema, blocks: Vec<EditorNode>) -> EditorNode {
        schema.branch(&schema.top_node, Fragment::from_children(blocks)).unwrap()
    }

    fn para(schema: &Schema, inline: Vec<EditorNode>) -> EditorNode {
        schema.branch("paragraph", Fragment::from_children(inline)).unwrap()
    }

    fn mark(schema: &Schema, name: &str, attrs: Attrs) -> Mark {
        Mark::new(schema.mark_type(name).unwrap().clone(), attrs)
    }

    #[test]
    fn blocks_marks_and_breaks_are_read_as_the_editor_holds_them() {
        let s = schema();
        let bold = mark(&s, "bold", Attrs::new());
        let color = mark(&s, "text_color", Attrs::new().with("color", "#ff0000".to_string()));
        let heading = s.create_node("heading", Attrs::new().with("level", 2i64), Fragment::from_node(s.text("Title").unwrap())).unwrap();
        let p = para(
            &s,
            vec![
                s.text_with_marks("loud", vec![bold]).unwrap(),
                s.branch("hard_break", Fragment::empty()).unwrap(),
                s.text_with_marks("red", vec![color]).unwrap(),
            ],
        );
        let rule = s.branch("horizontal_rule", Fragment::empty()).unwrap();
        let blocks = of_doc(&doc(&s, vec![heading, p, rule]));
        assert_eq!(blocks.len(), 3);
        assert!(matches!(&blocks[0], Block::Heading { level: 2, .. }));
        let Block::Paragraph { inlines, .. } = &blocks[1] else { panic!("a paragraph") };
        assert_eq!(inlines.len(), 3);
        assert!(matches!(&inlines[0], Inline::Text(run) if run.bold && run.text == "loud"));
        assert_eq!(inlines[1], Inline::Break);
        assert!(matches!(&inlines[2], Inline::Text(run) if run.color.as_deref() == Some("#ff0000")));
        assert_eq!(blocks[2], Block::Rule);
    }

    #[test]
    fn a_chosen_alignment_is_kept_and_an_unset_one_left_to_the_theme() {
        let s = schema();
        let aligned = |align: &str| {
            s.create_node("paragraph", Attrs::new().with("text_align", align.to_string()), Fragment::from_node(s.text("x").unwrap())).unwrap()
        };
        let plain = para(&s, vec![s.text("x").unwrap()]);
        let blocks = of_doc(&doc(&s, vec![plain, aligned("left"), aligned("center"), aligned("justify")]));
        let aligns: Vec<_> = blocks
            .iter()
            .map(|b| match b {
                Block::Paragraph { align, .. } => *align,
                _ => panic!("a paragraph"),
            })
            .collect();
        assert_eq!(aligns, [None, Some("left"), Some("center"), Some("justify")]);
    }

    #[test]
    fn lists_quotes_code_and_tables_keep_their_shape() {
        let s = schema();
        let item = |text: &str| s.branch("list_item", Fragment::from_node(para(&s, vec![s.text(text).unwrap()]))).unwrap();
        let list = s.create_node("ordered_list", Attrs::new().with("start", 3i64), Fragment::from_children(vec![item("a"), item("b")])).unwrap();
        let quote = s.branch("blockquote", Fragment::from_node(para(&s, vec![s.text("said").unwrap()]))).unwrap();
        let code = s.branch("code_block", Fragment::from_node(s.text("fn x() {}").unwrap())).unwrap();
        let cell = |kind: &str, text: &str| s.branch(kind, Fragment::from_node(para(&s, vec![s.text(text).unwrap()]))).unwrap();
        let row = |kind: &str| s.branch("table_row", Fragment::from_children(vec![cell(kind, "x"), cell(kind, "y")])).unwrap();
        let table = s.branch("table", Fragment::from_children(vec![row("table_header_cell"), row("table_cell")])).unwrap();
        let blocks = of_doc(&doc(&s, vec![list, quote, code, table]));
        assert!(matches!(&blocks[0], Block::List { ordered: true, start: 3, items } if items.len() == 2));
        assert!(matches!(&blocks[1], Block::Quote(inner) if inner.len() == 1));
        assert_eq!(blocks[2], Block::Code("fn x() {}".to_string()));
        let Block::Table { columns, rows } = &blocks[3] else { panic!("a table") };
        assert_eq!(*columns, 2);
        assert!(rows[0].iter().all(|c| c.header) && rows[1].iter().all(|c| !c.header));
        assert_eq!(rows[1].len(), 2);
        assert_eq!(rows[1][1].rect, Some(Rect { left: 1, top: 1, right: 2, bottom: 2 }));
    }

    #[test]
    fn a_color_that_is_not_a_hex_color_is_dropped() {
        assert_eq!(hex_color("#a0b1c2").as_deref(), Some("#a0b1c2"));
        assert_eq!(hex_color(" #abc ").as_deref(), Some("#abc"));
        assert_eq!(hex_color("red;background:url(x)"), None);
        assert_eq!(hex_color("#12345"), None);
        assert_eq!(hex_color("#ggg"), None);
    }

    #[test]
    fn only_the_first_blocks_of_a_long_note_are_read() {
        let s = schema();
        let many = (0..MAX_BLOCKS + 10).map(|i| para(&s, vec![s.text(&format!("line {i}")).unwrap()])).collect();
        assert_eq!(of_doc(&doc(&s, many)).len(), MAX_BLOCKS);
    }

    #[test]
    fn a_spanning_cell_keeps_its_rectangle_and_a_long_table_is_cut() {
        let s = schema();
        let cell = |attrs: Attrs| s.create_node("table_cell", attrs, Fragment::from_node(para(&s, vec![s.text("x").unwrap()]))).unwrap();
        let wide = s.branch("table_row", Fragment::from_node(cell(Attrs::new().with("colspan", 2i64)))).unwrap();
        let mut rows = vec![wide];
        rows.extend((0..MAX_TABLE_ROWS + 5).map(|_| s.branch("table_row", Fragment::from_children(vec![cell(Attrs::new()), cell(Attrs::new())])).unwrap()));
        let table = s.branch("table", Fragment::from_children(rows)).unwrap();
        let blocks = of_doc(&doc(&s, vec![table]));
        let Block::Table { columns, rows } = &blocks[0] else { panic!("a table") };
        assert_eq!(*columns, 2);
        assert_eq!(rows.len(), MAX_TABLE_ROWS);
        assert_eq!(rows[0][0].rect, Some(Rect { left: 0, top: 0, right: 2, bottom: 1 }));
    }

    #[test]
    fn the_sample_shows_a_heading_paragraphs_and_a_quote() {
        let sample = sample();
        assert!(matches!(sample[0], Block::Heading { level: 1, .. }));
        assert!(sample.iter().filter(|b| matches!(b, Block::Paragraph { .. })).count() >= 2);
        assert!(sample.iter().any(|b| matches!(b, Block::Quote(_))));
    }
}
