//! Markdown in and out of a node's content (docs/MCP_CONTRACT.md "Markdown, in and
//! out"), and the edits that touch only the blocks they name ("Edits that touch only
//! what they name").
//!
//! The conversion is rinch's own (`rinch_editor_core::serialize::markdown`), between
//! Markdown and the editor model, so the model that comes out of a document is the
//! model an edit goes back in through: a block nobody names keeps everything the
//! collaboration scope knows about it, including what [`crate::Block`] cannot say.
//! This module adds:
//!
//! - [`check`] refuses, with a sentence naming the construct and its line, anything a
//!   document cannot hold: what rinch's strict parser names as something it would drop,
//!   and what it reads that Pimble has no place for yet. A refusal writes nothing.
//! - The block edits, as functions from the model before to the model after. Each one
//!   rebuilds only the branch it changes, so every other node is the same `Rc` and the
//!   projection's diff (`CollabSession::record_local`, block prefix and suffix by
//!   identity, then a text splice inside a changed block) records exactly the change
//!   (`edits`), and each says what it took out (`echo`).
//!
//! A block is named by a **quote** (`quotes`: text in a textblock of the document, at
//! any depth: a list item's paragraph names its list; `occurrence` picks one of several
//! places) or by its **number** among the top-level blocks; a heading by its text, a
//! heading path or its number (`headings`). No match and several matches are both
//! refusals that list what they found.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use rinch_editor_core::serialize::markdown::{doc_from_markdown_strict, doc_to_markdown, Construct, MarkdownError};
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
    strict(&Schema::starter_kit(), md).map(|_| ())
}

fn cannot_hold(line: usize, what: &str) -> CrdtError {
    refused(format!("Line {line} is {what}; Pimble documents cannot hold that yet, so nothing was written."))
}

/// What rinch's strict parser accepts and a Pimble document still cannot hold: images
/// (no storage yet, `docs/IMAGES_CONTRACT.md`), task lists (outside the collaboration
/// scope), and inline code inside a mark the code mark excludes (dropped without a word).
fn check_scope(md: &str) -> Result<()> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    // Turned on to be recognised and refused, never to be parsed into something.
    options.insert(Options::ENABLE_TASKLISTS);
    let line = |offset: usize| md[..offset.min(md.len())].matches('\n').count() + 1;
    // The marks open around the current text: inline code cannot carry any of them.
    let mut open: Vec<&'static str> = Vec::new();
    for (event, range) in Parser::new_ext(md, options).into_offset_iter() {
        match &event {
            Event::Start(Tag::Strong) => open.push("bold"),
            Event::Start(Tag::Emphasis) => open.push("italic"),
            Event::Start(Tag::Strikethrough) => open.push("struck through"),
            Event::Start(Tag::Link { dest_url, .. })
                if pimble_core::mention::is_mention_href(dest_url) && pimble_core::mention::parse_href(dest_url).is_none() =>
            {
                return Err(refused(format!(
                    "Line {} is a mention that names no account; a mention is [@Name](pimble-person:<account>), \
                     its account as list_people shows it, so nothing was written.",
                    line(range.start)
                )));
            }
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
            Event::Start(Tag::Image { dest_url, .. }) if !is_pimble_picture(dest_url) => {
                return Err(refused(format!(
                    "Line {} is a picture from outside Pimble; only pictures already in Pimble \
                     (the pimble-blob: and pimble-board: addresses get_node shows) can be written \
                     in Markdown, so nothing was written. A new picture goes in with insert_canvas.",
                    line(range.start)
                )));
            }
            Event::TaskListMarker(_) => return Err(cannot_hold(line(range.start), "a task list item")),
            _ => {}
        }
    }
    Ok(())
}

/// The same two, where Markdown's own syntax did not show them: inside an HTML table.
fn check_scope_of(node: &Node) -> Result<()> {
    let what = match node.type_name() {
        "image" if !node.attrs().get_str("src").is_some_and(is_pimble_picture) => Some("a picture from outside Pimble"),
        "task_list" => Some("a task list"),
        _ => None,
    };
    if let Some(what) = what {
        return Err(refused(format!(
            "This Markdown has {what} inside an HTML table; Pimble documents cannot hold that yet, so nothing was written."
        )));
    }
    node.content().iter().try_for_each(check_scope_of)
}

/// `md` as an editor `doc`, or the refusal: [`check_scope`], then rinch's strict parse,
/// which names whatever else it would drop (HTML other than the tags it reads marks,
/// line breaks and tables from, footnotes, unsafe addresses).
fn strict(schema: &Schema, md: &str) -> Result<Node> {
    check_scope(md)?;
    let doc = doc_from_markdown_strict(schema, md).map_err(|e| match e {
        MarkdownError::Unsupported { construct, line, .. } => cannot_hold(
            line,
            match construct {
                Construct::InlineHtml | Construct::HtmlBlock => {
                    "HTML other than the tags for underline, highlight, color, sub/superscript, line breaks and tables"
                }
                Construct::UnmatchedTag => "an HTML tag without its partner",
                Construct::Footnote => "a footnote",
                Construct::TaskList => "a task list item",
                Construct::UnsafeLink => "a link to an unsafe address",
                Construct::UnsafeImage => "an image",
                Construct::UnsupportedMark => "formatting with no place in a document",
                _ => "something with no place in a document",
            },
        ),
        MarkdownError::Invalid(e) => collab(e),
    })?;
    check_scope_of(&doc)?;
    // A mention written without its id is given one as it enters a document
    // (docs/NOTIFICATIONS_CONTRACT.md "Built in wave 0").
    Ok(crate::mentions::with_ids(&with_canvas_attrs(&doc)?, schema))
}

/// `md` as an editor `doc`, after [`check`]. Markdown with no blocks at all is
/// refused, since writing it would change nothing anyone asked for.
pub fn parse(schema: &Schema, md: &str) -> Result<Node> {
    let doc = strict(schema, md)?;
    if md.trim().is_empty() {
        return Err(refused("There is no Markdown to write."));
    }
    Ok(doc)
}

/// An editor `doc` as Markdown: CommonMark with `~~strike~~` and pipe tables, and the
/// HTML tags rinch writes for what Markdown has no syntax for (underline, highlight,
/// text colour, sub/superscript, a table with merged cells or blocks in a cell).
/// A canvas in text is its picture with the canvas's id after the URL,
/// `![alt](pimble-blob:<store>/<hash>#canvas=<id>)` (docs/CANVAS_CONTRACT.md,
/// decision 5).
pub fn render(doc: &Node) -> String {
    match with_canvas_urls(doc) {
        Some(doc) => doc_to_markdown(&doc),
        None => doc_to_markdown(doc),
    }
}

/// The attribute of a picture in text naming its canvas (a board, in storage).
const CANVAS_ATTR: &str = "data-board";

/// What follows a canvas's picture URL in Markdown.
pub const CANVAS_FRAGMENT: &str = "#canvas=";

/// `doc` with each canvas's id written into its picture's URL; `None` when it
/// has no canvas in its text.
fn with_canvas_urls(doc: &Node) -> Option<Node> {
    fn names_canvas(node: &Node) -> Option<(&str, &str)> {
        if node.type_name() != "image" {
            return None;
        }
        let id = node.attrs().get_str(CANVAS_ATTR).filter(|id| !id.is_empty())?;
        let src = node.attrs().get_str("src").filter(|src| !src.contains('#'))?;
        Some((src, id))
    }
    fn walk(node: &Node, schema: &Schema) -> Node {
        if let Some((src, id)) = names_canvas(node) {
            let attrs = node.attrs().with("src", rinch_editor_core::AttrValue::from(format!("{src}{CANVAS_FRAGMENT}{id}").as_str()));
            return schema.create_node("image", attrs, Fragment::empty()).map(|n| n.with_marks(node.marks().to_vec())).unwrap_or_else(|_| node.clone());
        }
        if node.is_text() || node.child_count() == 0 {
            return node.clone();
        }
        node.copy_with_content(Fragment::from_children(node.content().iter().map(|child| walk(child, schema)).collect()))
    }
    let mut any = false;
    doc.nodes_between(0, doc.content_size(), &mut |node, _, _| {
        any |= names_canvas(node).is_some();
        !any
    });
    any.then(|| walk(doc, &Schema::starter_kit()))
}

/// A picture Pimble holds: a blob of a store, or a canvas (docs/CANVAS_CONTRACT.md).
/// Nothing else can be written, since its bytes are not in any store.
fn is_pimble_picture(src: &str) -> bool {
    let src = src.trim();
    src.starts_with("pimble-blob:") || src.starts_with("pimble-board:")
}

/// The canvas a picture's Markdown URL names after [`CANVAS_FRAGMENT`] (or the
/// fragment before 2026-10-23, `#board=`), and the URL without it.
fn split_canvas_url(src: &str) -> (&str, Option<&str>) {
    for fragment in [CANVAS_FRAGMENT, "#board="] {
        if let Some(at) = src.find(fragment) {
            let id = &src[at + fragment.len()..];
            return (&src[..at], (!id.is_empty()).then_some(id));
        }
    }
    (src, None)
}

/// The inverse of [`with_canvas_urls`]: each picture whose URL names a canvas takes
/// the id back into `data-board`, its URL as stored. A canvas that began empty
/// (`pimble-board:in-text`) is only a canvas, so one naming none is refused.
pub(crate) fn with_canvas_attrs(doc: &Node) -> Result<Node> {
    fn walk(node: &Node, schema: &Schema) -> Result<Node> {
        if node.type_name() == "image" {
            let src = node.attrs().get_str("src").unwrap_or_default();
            let (bare, canvas) = split_canvas_url(src);
            if bare == IN_TEXT_SRC && canvas.is_none() {
                return Err(refused(format!(
                    "A {IN_TEXT_SRC} picture is an empty canvas and names it after {CANVAS_FRAGMENT}; \
                     this one names none, so nothing was written."
                )));
            }
            let Some(canvas) = canvas else { return Ok(node.clone()) };
            let attrs = node
                .attrs()
                .with("src", rinch_editor_core::AttrValue::from(bare))
                .with(CANVAS_ATTR, rinch_editor_core::AttrValue::from(canvas));
            return schema.create_node("image", attrs, Fragment::empty()).map(|n| n.with_marks(node.marks().to_vec())).map_err(collab);
        }
        if node.is_text() || node.child_count() == 0 {
            return Ok(node.clone());
        }
        let children = node.content().iter().map(|child| walk(child, schema)).collect::<Result<Vec<_>>>()?;
        Ok(node.copy_with_content(Fragment::from_children(children)))
    }
    walk(doc, &Schema::starter_kit())
}

/// The `src` of a canvas that began empty, with no picture under it.
pub const IN_TEXT_SRC: &str = "pimble-board:in-text";

/// The canvases the pictures in `doc` name, once per picture.
pub fn canvases_named(doc: &Node) -> Vec<String> {
    let mut out = Vec::new();
    doc.nodes_between(0, doc.content_size(), &mut |node, _, _| {
        if node.type_name() == "image" {
            if let Some(id) = node.attrs().get_str(CANVAS_ATTR).filter(|id| !id.is_empty()) {
                out.push(id.to_string());
            }
        }
        true
    });
    out
}

/// Refuse an edit whose pictures name a canvas that is not in the node (`held`: the
/// ids of the canvases in its `data`), or name one more often than before: a
/// picture's canvas lives in the note that holds the picture, one picture each, and
/// a picture naming a canvas the note has not got would show nothing on every device.
/// Pictures that were there before are judged as they were (a canvas that has not
/// arrived yet is not the edit's doing).
pub(crate) fn check_canvases(before: &Node, after: &Node, held: &std::collections::HashSet<String>) -> Result<()> {
    let tally = |doc: &Node| {
        let mut count: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        for id in canvases_named(doc) {
            *count.entry(id).or_default() += 1;
        }
        count
    };
    let (was, now) = (tally(before), tally(after));
    for (id, n) in &now {
        let had = was.get(id).copied().unwrap_or(0);
        if *n <= had {
            continue;
        }
        if *n > 1 {
            return Err(refused(format!(
                "Canvas {id} would be shown twice; a canvas belongs to one picture, so nothing was \
                 written. insert_canvas puts in another one."
            )));
        }
        if !held.contains(id) {
            return Err(refused(format!(
                "There is no canvas {id} in this note (a picture's canvas lives in the note that holds it), \
                 so nothing was written. insert_canvas puts a new one in."
            )));
        }
    }
    Ok(())
}

// ── Embeds ───────────────────────────────────────────────────────────────

/// The `data-*` attributes Pimble's `embed` atoms carry. rinch keeps and merges them
/// per attribute and knows nothing of what they mean; these are what they mean.
pub mod embed_attrs {
    /// A table of the note's own sheets shown where the atom is: its table id.
    pub const SHEET: &str = "data-sheet";
    /// Another node shown where the atom is, as a `pimble:` link, which any reader of
    /// the content counts as a link to it.
    pub const NODE: &str = "data-node";
    /// What the shown node is ([`KIND_SHEET`], [`KIND_DISCUSSION`]), which says which
    /// view draws it.
    pub const KIND: &str = "data-kind";
    /// The shown node's title when the atom was written, for a reader that cannot
    /// fetch it.
    pub const TITLE: &str = "data-title";
    /// The one table of a shown sheet node, when only one is shown.
    pub const TABLE: &str = "data-table";
    /// [`KIND`] of a sheet node shown in the text.
    pub const KIND_SHEET: &str = "sheet";
    /// [`KIND`] of a discussion shown in the text.
    pub const KIND_DISCUSSION: &str = "discussion";
}

/// What an `embed` atom in a node's text shows (docs/SHEETS_CONTRACT.md section 1,
/// docs/DISCUSSIONS_CONTRACT.md). The app builds these as editor nodes from
/// [`Embed::attrs`] and reads them with [`Embed::from_attrs`]; Markdown carries them as
/// the HTML element rinch writes for an embed, which [`Embed::to_markdown`] and
/// [`Embed::from_markdown_line`] write and read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Embed {
    /// A table of the note's own sheets, by id.
    Table { id: String },
    /// Another node: a sheet node (all its tables, or `table` alone) or a discussion.
    Node { kind: String, node: pimble_core::PimbleUrl, title: Option<String>, table: Option<String> },
}

impl Embed {
    /// A sheet node shown in the text.
    pub fn sheet_node(node: pimble_core::PimbleUrl, title: Option<String>) -> Embed {
        Embed::Node { kind: embed_attrs::KIND_SHEET.to_string(), node, title, table: None }
    }

    /// A discussion shown in the text.
    pub fn discussion(node: pimble_core::PimbleUrl, title: Option<String>) -> Embed {
        Embed::Node { kind: embed_attrs::KIND_DISCUSSION.to_string(), node, title, table: None }
    }

    /// The shown node of a [`Embed::Node`] of kind `kind`.
    pub fn node_of_kind(&self, kind: &str) -> Option<&pimble_core::PimbleUrl> {
        match self {
            Embed::Node { kind: k, node, .. } if k == kind => Some(node),
            _ => None,
        }
    }

    /// The atom's attributes, as an editor node carries them.
    pub fn attrs(&self) -> Vec<(&'static str, String)> {
        match self {
            Embed::Table { id } => vec![(embed_attrs::SHEET, id.clone())],
            Embed::Node { kind, node, title, table } => {
                let mut out = vec![(embed_attrs::NODE, node.to_string()), (embed_attrs::KIND, kind.clone())];
                if let Some(table) = table {
                    out.push((embed_attrs::TABLE, table.clone()));
                }
                if let Some(title) = title {
                    out.push((embed_attrs::TITLE, title.clone()));
                }
                out
            }
        }
    }

    /// What an atom with these attributes shows (`get` reads one by name), if it is
    /// one of Pimble's. An empty table id or an unreadable link is none.
    pub fn from_attrs<'a>(get: impl Fn(&str) -> Option<&'a str>) -> Option<Embed> {
        if let Some(id) = get(embed_attrs::SHEET).filter(|id| !id.is_empty()) {
            return Some(Embed::Table { id: id.to_string() });
        }
        let kind = get(embed_attrs::KIND).filter(|k| !k.is_empty())?;
        let node = pimble_core::PimbleUrl::parse(get(embed_attrs::NODE)?)?;
        Some(Embed::Node {
            kind: kind.to_string(),
            node,
            title: get(embed_attrs::TITLE).map(str::to_string),
            table: get(embed_attrs::TABLE).filter(|t| !t.is_empty()).map(str::to_string),
        })
    }

    /// The atom as one line of Markdown, written by rinch's own writer, so its
    /// attributes are in the order and escaping the strict reader takes as one atom
    /// (anything else is read as HTML and refused).
    pub fn to_markdown(&self) -> String {
        let schema = Schema::starter_kit();
        let attrs = self.attrs().into_iter().fold(rinch_editor_core::Attrs::new(), |a, (k, v)| a.with(k, v.as_str()));
        let written = schema
            .create_node("embed", attrs, Fragment::empty())
            .and_then(|atom| schema.create_node("doc", rinch_editor_core::Attrs::new(), Fragment::from_node(atom)))
            .map(|doc| doc_to_markdown(&doc))
            .unwrap_or_default();
        format!("{}\n", written.trim_end())
    }

    /// The atom a line of Markdown (as [`render`] writes a node's text) is, if it is
    /// one of Pimble's and nothing else.
    pub fn from_markdown_line(line: &str) -> Option<Embed> {
        let line = line.trim();
        if !line.starts_with("<div") || !line.contains("data-pm-type=\"embed\"") {
            return None;
        }
        let doc = doc_from_markdown_strict(&Schema::starter_kit(), line).ok()?;
        if doc.child_count() != 1 || doc.child(0).type_name() != "embed" {
            return None;
        }
        let atom = doc.child(0);
        Embed::from_attrs(|name| atom.attrs().get_str(name))
    }
}

// ── Naming what an edit touches, the edits, and what they say they did ─────

mod echo;
mod edits;
mod headings;
mod judge;
mod quotes;
#[cfg(test)]
mod edits_tests;
#[cfg(test)]
mod tests;

pub use echo::{excerpt, Echo, EXCERPT, NEIGHBOR};
pub use edits::{InsertAt, MarkdownEdit};
pub use headings::{blocks_markdown, find_heading, headings, loose, target_heading, Heading, HeadingTarget, PATH_SEPARATOR};
pub use judge::MIN_DELETE_QUOTE;
pub use quotes::{BlockTarget, Narrowing, Occurrence, Quote};
pub(crate) use edits::{append, cut_sections, steps, write_new};
pub(crate) use quotes::is_blank;
