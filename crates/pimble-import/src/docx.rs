//! Word (.docx) to rich blocks.
//!
//! A .docx is a zip ([`crate::zip`]) of XML parts. The main part (found through the
//! package relationships, `word/document.xml` in every file Word writes) holds the
//! body; `styles.xml`, `numbering.xml` and the main part's relationships are read
//! when present, for headings, run formatting a style carries, list kinds and
//! hyperlink targets. Each XML part is read into a small element tree with its
//! namespaces resolved, so a file that binds the WordprocessingML namespace to an
//! unusual prefix (or uses the strict namespace) reads the same.
//!
//! The body walk emits one [`Para`] per `w:p` with its runs and properties, and a
//! [`Block::Table`] per `w:tbl`; [`items_to_blocks`] then groups list paragraphs into
//! nested [`Block::BulletList`]/[`Block::OrderedList`]s and turns heading-styled
//! paragraphs into [`Block::Heading`]s, as the RTF importer does.
//!
//! Everything the content model cannot hold is reduced rather than kept: pictures,
//! drawings and text boxes are skipped, a page or column break is nothing, a nested
//! table reads as its cells' paragraphs, footnote and comment references are dropped,
//! hidden text and deleted tracked changes are left out (insertions are kept), and a
//! field reads as its displayed result. Color comes only from the run itself: the
//! color a style gives (a theme's heading blue, the hyperlink style) is the editor's
//! business, not the document's.

use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};
use pimble_crdt::{Align, Block, Inline, ListItem, Mark, Run, TableCell, TableRow};
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;
use quick_xml::NsReader;

use crate::zip::Archive;

const NOT_A_DOCX: &str = "This is not a Word document (.docx).";

/// Convert the body of a .docx into the blocks a content document is built from.
pub fn docx_to_blocks(bytes: &[u8]) -> Result<Vec<Block>> {
    let archive = Archive::open(bytes).map_err(|_| anyhow!(NOT_A_DOCX))?;
    let main = main_part_name(&archive);
    let document = archive.read(&main).ok().flatten().ok_or_else(|| anyhow!(NOT_A_DOCX))?;
    let document = parse_xml(&document).map_err(|e| anyhow!("The document's text is damaged ({e})."))?;

    // The main part's own relationships name its styles, numbering and link targets.
    let (dir, file) = main.rsplit_once('/').unwrap_or(("", main.as_str()));
    let rels_name = if dir.is_empty() { format!("_rels/{file}.rels") } else { format!("{dir}/_rels/{file}.rels") };
    let rels = read_part(&archive, &rels_name).map(|el| Rels::read(&el, dir)).unwrap_or_default();
    // A part that is missing or does not parse is as good as absent: the text still imports.
    let styles = read_part(&archive, rels.part("styles").as_deref().unwrap_or(&join(dir, "styles.xml")))
        .map(|el| Styles::read(&el))
        .unwrap_or_default();
    let numbering = read_part(&archive, rels.part("numbering").as_deref().unwrap_or(&join(dir, "numbering.xml")))
        .map(|el| Numbering::read(&el))
        .unwrap_or_default();

    let Some(body) = document.child("w:body") else { return Ok(Vec::new()) };
    let walk = |code: bool| {
        let mut walker = Walker {
            styles: &styles,
            numbering: &numbering,
            links: &rels.links,
            fields: Vec::new(),
            counters: HashMap::new(),
            code,
            text_chars: 0,
            monospace_chars: 0,
        };
        let mut items = Vec::new();
        walker.blocks_in(body, &mut items, false);
        (items, walker.monospace_chars * 2 > walker.text_chars)
    };
    // A monospaced run is code, unless most of the document is monospaced: then it
    // is a manuscript typed in Courier, and the font is only how it looks.
    let (items, mostly_monospace) = walk(true);
    let items = if mostly_monospace { walk(false).0 } else { items };
    Ok(items_to_blocks(items))
}

/// The main document part's name: the package relationship of type `officeDocument`,
/// or `word/document.xml` when the package does not say.
fn main_part_name(archive: &Archive) -> String {
    read_part(archive, "_rels/.rels")
        .and_then(|el| Rels::read(&el, "").part("officeDocument"))
        .filter(|name| archive.read(name).ok().flatten().is_some())
        .unwrap_or_else(|| "word/document.xml".to_string())
}

fn read_part(archive: &Archive, name: &str) -> Option<El> {
    archive.read(name).ok().flatten().and_then(|bytes| parse_xml(&bytes).ok())
}

/// `target` relative to the directory `dir` of the part that names it, as a name
/// inside the archive. A leading `/` is the package root; `..` climbs.
fn join(dir: &str, target: &str) -> String {
    let mut parts: Vec<&str> =
        if target.starts_with('/') { Vec::new() } else { dir.split('/').filter(|s| !s.is_empty()).collect() };
    for segment in target.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

// ── XML ──────────────────────────────────────────────────────────────

/// Deeper nesting than this is an error: no document nests anywhere near it, and the
/// walk over the tree is recursive.
const MAX_DEPTH: usize = 256;

/// An element with its namespace resolved to a short fixed prefix (`w:p`, `r:id`,
/// `mc:Choice`); a namespace this reader does not know is `*`, and an unprefixed
/// name in no namespace is bare.
#[derive(Debug, Default)]
struct El {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
}

#[derive(Debug)]
enum Node {
    El(El),
    Text(String),
}

impl El {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    /// The `w:val` attribute, which most properties carry their value in.
    fn val(&self) -> Option<&str> {
        self.attr("w:val")
    }

    fn elements(&self) -> impl Iterator<Item = &El> {
        self.children.iter().filter_map(|n| match n {
            Node::El(el) => Some(el),
            Node::Text(_) => None,
        })
    }

    fn child(&self, name: &str) -> Option<&El> {
        self.elements().find(|el| el.name == name)
    }

    /// The text directly inside this element.
    fn text(&self) -> String {
        self.children
            .iter()
            .filter_map(|n| match n {
                Node::Text(t) => Some(t.as_str()),
                Node::El(_) => None,
            })
            .collect()
    }
}

fn prefix_for(namespace: &[u8]) -> &'static str {
    match namespace {
        b"http://schemas.openxmlformats.org/wordprocessingml/2006/main"
        | b"http://purl.oclc.org/ooxml/wordprocessingml/main" => "w",
        b"http://schemas.openxmlformats.org/officeDocument/2006/relationships"
        | b"http://purl.oclc.org/ooxml/officeDocument/relationships" => "r",
        b"http://schemas.openxmlformats.org/officeDocument/2006/math"
        | b"http://purl.oclc.org/ooxml/officeDocument/math" => "m",
        b"http://schemas.openxmlformats.org/markup-compatibility/2006" => "mc",
        b"http://schemas.openxmlformats.org/package/2006/relationships" => "pr",
        b"http://www.w3.org/XML/1998/namespace" => "xml",
        _ => "*",
    }
}

fn qualified(ns: ResolveResult, local: &[u8]) -> String {
    let local = String::from_utf8_lossy(local);
    match ns {
        ResolveResult::Bound(ns) => format!("{}:{local}", prefix_for(ns.0)),
        ResolveResult::Unbound => local.into_owned(),
        ResolveResult::Unknown(_) => format!("*:{local}"),
    }
}

fn element(reader: &NsReader<&[u8]>, name: String, start: &BytesStart) -> Result<El> {
    let mut attrs = Vec::new();
    for attr in start.attributes() {
        let attr = attr?;
        let (ns, local) = reader.resolve_attribute(attr.key);
        let key = qualified(ns, local.as_ref());
        attrs.push((key, attr.unescape_value()?.into_owned()));
    }
    Ok(El { name, attrs, children: Vec::new() })
}

/// Read an XML part into its root element. Malformed XML is an error.
fn parse_xml(bytes: &[u8]) -> Result<El> {
    let mut reader = NsReader::from_reader(bytes);
    let mut stack: Vec<El> = Vec::new();
    let mut root: Option<El> = None;
    let attach = |stack: &mut Vec<El>, root: &mut Option<El>, el: El| match stack.last_mut() {
        Some(parent) => parent.children.push(Node::El(el)),
        None => {
            if root.is_none() {
                *root = Some(el);
            }
        }
    };
    loop {
        let (ns, event) = reader.read_resolved_event()?;
        match event {
            Event::Start(start) => {
                if stack.len() >= MAX_DEPTH {
                    bail!("nested too deeply");
                }
                let name = qualified(ns, start.local_name().as_ref());
                let el = element(&reader, name, &start)?;
                stack.push(el);
            }
            Event::Empty(start) => {
                let name = qualified(ns, start.local_name().as_ref());
                let el = element(&reader, name, &start)?;
                attach(&mut stack, &mut root, el);
            }
            Event::End(_) => {
                let el = stack.pop().ok_or_else(|| anyhow!("unbalanced end tag"))?;
                attach(&mut stack, &mut root, el);
            }
            Event::Text(text) => {
                if let Some(top) = stack.last_mut() {
                    top.children.push(Node::Text(text.unescape()?.into_owned()));
                }
            }
            Event::CData(data) => {
                if let Some(top) = stack.last_mut() {
                    top.children.push(Node::Text(String::from_utf8_lossy(&data).into_owned()));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        bail!("unexpected end of XML");
    }
    root.ok_or_else(|| anyhow!("no root element"))
}

/// An `mc:AlternateContent`'s content: its first choice, or the fallback when it has
/// none. Both hold the same thing for a consumer that understands them, and what a
/// choice usually needs (drawings) is skipped either way.
fn alternate(el: &El) -> Option<&El> {
    el.child("mc:Choice").or_else(|| el.child("mc:Fallback"))
}

/// An on/off property (`w:b`, `w:i`, ...): on when present, unless its value says off.
fn toggle(el: &El) -> bool {
    !matches!(el.val(), Some("0" | "false" | "off"))
}

fn int_attr(el: &El, name: &str) -> Option<i64> {
    el.attr(name)?.trim().parse().ok()
}

// ── Relationships, styles, numbering ─────────────────────────────────

#[derive(Debug, Default)]
struct Rels {
    /// Relationship id to hyperlink target.
    links: HashMap<String, String>,
    /// Relationship type (its last path segment: `styles`, `numbering`, ...) to the
    /// part's name inside the archive.
    parts: HashMap<String, String>,
}

impl Rels {
    fn read(root: &El, dir: &str) -> Rels {
        let mut rels = Rels::default();
        for rel in root.elements().filter(|el| el.name == "pr:Relationship") {
            let (Some(id), Some(target), Some(kind)) = (rel.attr("Id"), rel.attr("Target"), rel.attr("Type")) else {
                continue;
            };
            let kind = kind.rsplit('/').next().unwrap_or(kind);
            if kind == "hyperlink" {
                rels.links.insert(id.to_string(), target.to_string());
            } else if rel.attr("TargetMode") != Some("External") {
                rels.parts.entry(kind.to_string()).or_insert_with(|| join(dir, target));
            }
        }
        rels
    }

    fn part(&self, kind: &str) -> Option<String> {
        self.parts.get(kind).cloned()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum VertAlign {
    Baseline,
    Superscript,
    Subscript,
}

/// Run properties as one level states them: `None` is "not said here", so levels
/// layer (document defaults, paragraph style, run style, the run itself).
#[derive(Debug, Clone, Default)]
struct RunProps {
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strike: Option<bool>,
    vert: Option<VertAlign>,
    hidden: Option<bool>,
    monospace: Option<bool>,
    /// `Some(None)` is an explicit "no highlight".
    highlight: Option<Option<String>>,
    color: Option<Option<String>>,
    shading: Option<Option<String>>,
}

impl RunProps {
    /// Layer `other` over these: what `other` says wins.
    fn over(&mut self, other: &RunProps) {
        macro_rules! take {
            ($($f:ident),*) => { $( if other.$f.is_some() { self.$f = other.$f.clone(); } )* };
        }
        take!(bold, italic, underline, strike, vert, hidden, monospace, highlight, color, shading);
    }

    /// The same, less color: a style's color is not imported (see the module docs).
    fn without_color(mut self) -> Self {
        self.highlight = None;
        self.color = None;
        self.shading = None;
        self
    }

    /// Read a `w:rPr`, and the character style it names (`w:rStyle`).
    fn read(rpr: &El) -> (RunProps, Option<String>) {
        let mut props = RunProps::default();
        let mut style = None;
        for el in rpr.elements() {
            match el.name.as_str() {
                "w:rStyle" => style = el.val().map(str::to_string),
                "w:b" => props.bold = Some(toggle(el)),
                "w:i" => props.italic = Some(toggle(el)),
                "w:u" => props.underline = Some(!matches!(el.val(), Some("none" | "0" | "false"))),
                "w:strike" | "w:dstrike" => props.strike = Some(toggle(el) || props.strike == Some(true)),
                "w:vanish" => props.hidden = Some(toggle(el)),
                "w:vertAlign" => {
                    props.vert = Some(match el.val() {
                        Some("superscript") => VertAlign::Superscript,
                        Some("subscript") => VertAlign::Subscript,
                        _ => VertAlign::Baseline,
                    })
                }
                "w:highlight" => props.highlight = Some(el.val().and_then(highlight_color)),
                "w:color" => props.color = Some(el.val().and_then(hex_color)),
                "w:shd" => props.shading = Some(el.attr("w:fill").and_then(hex_color)),
                "w:rFonts" => {
                    if let Some(font) = el.attr("w:ascii").or_else(|| el.attr("w:hAnsi")) {
                        let f = font.to_ascii_lowercase();
                        props.monospace = Some(
                            f.contains("courier") || f.contains("mono") || f.contains("menlo") || f.contains("consolas"),
                        );
                    }
                }
                _ => {}
            }
        }
        (props, style)
    }
}

/// `RRGGBB` as CSS; `auto` and anything malformed are no color.
fn hex_color(value: &str) -> Option<String> {
    (value.len() == 6 && value.bytes().all(|b| b.is_ascii_hexdigit())).then(|| format!("#{}", value.to_ascii_lowercase()))
}

/// Word's sixteen highlighter colors; `none` is no highlight.
fn highlight_color(name: &str) -> Option<String> {
    let hex = match name {
        "yellow" => "#ffff00",
        "green" => "#00ff00",
        "cyan" => "#00ffff",
        "magenta" => "#ff00ff",
        "blue" => "#0000ff",
        "red" => "#ff0000",
        "darkBlue" => "#000080",
        "darkCyan" => "#008080",
        "darkGreen" => "#008000",
        "darkMagenta" => "#800080",
        "darkRed" => "#800000",
        "darkYellow" => "#808000",
        "darkGray" => "#808080",
        "lightGray" => "#c0c0c0",
        "black" => "#000000",
        "white" => "#ffffff",
        _ => return None,
    };
    Some(hex.to_string())
}

/// Paragraph properties as one level states them, like [`RunProps`].
#[derive(Debug, Clone, Default)]
struct ParaProps {
    style: Option<String>,
    num_id: Option<i64>,
    ilvl: Option<i64>,
    align: Option<Align>,
    /// Left indent in twips.
    left: Option<i64>,
    outline: Option<i64>,
}

impl ParaProps {
    fn over(&mut self, other: &ParaProps) {
        macro_rules! take {
            ($($f:ident),*) => { $( if other.$f.is_some() { self.$f = other.$f.clone(); } )* };
        }
        take!(style, num_id, ilvl, align, left, outline);
    }

    fn read(ppr: &El) -> ParaProps {
        let mut props = ParaProps::default();
        for el in ppr.elements() {
            match el.name.as_str() {
                "w:pStyle" => props.style = el.val().map(str::to_string),
                "w:numPr" => {
                    props.num_id = el.child("w:numId").and_then(|n| int_attr(n, "w:val"));
                    props.ilvl = el.child("w:ilvl").and_then(|n| int_attr(n, "w:val"));
                }
                "w:jc" => {
                    props.align = Some(match el.val() {
                        Some("center") => Align::Center,
                        Some("right" | "end") => Align::Right,
                        Some(v) if v == "both" || v == "distribute" || v.ends_with("Kashida") || v == "thaiDistribute" => {
                            Align::Justify
                        }
                        _ => Align::Left,
                    })
                }
                "w:ind" => props.left = int_attr(el, "w:left").or_else(|| int_attr(el, "w:start")),
                "w:outlineLvl" => props.outline = int_attr(el, "w:val"),
                _ => {}
            }
        }
        props
    }
}

#[derive(Debug, Default)]
struct Style {
    name: String,
    based_on: Option<String>,
    run: RunProps,
    para: ParaProps,
}

#[derive(Debug, Default)]
struct Styles {
    by_id: HashMap<String, Style>,
    /// The document defaults' run properties (`w:docDefaults`).
    defaults: RunProps,
    /// The paragraph style a paragraph naming none has (`Normal`, usually).
    default_para: Option<String>,
}

impl Styles {
    fn read(root: &El) -> Styles {
        let mut styles = Styles::default();
        if let Some(rpr) = root.child("w:docDefaults").and_then(|d| d.child("w:rPrDefault")).and_then(|d| d.child("w:rPr")) {
            styles.defaults = RunProps::read(rpr).0.without_color();
        }
        for el in root.elements().filter(|el| el.name == "w:style") {
            let Some(id) = el.attr("w:styleId") else { continue };
            if el.attr("w:type") == Some("paragraph") && el.attr("w:default").map_or(false, |d| d == "1" || d == "true") {
                styles.default_para = Some(id.to_string());
            }
            let style = Style {
                name: el.child("w:name").and_then(El::val).unwrap_or(id).to_string(),
                based_on: el.child("w:basedOn").and_then(El::val).map(str::to_string),
                run: el.child("w:rPr").map(|r| RunProps::read(r).0.without_color()).unwrap_or_default(),
                para: el.child("w:pPr").map(ParaProps::read).unwrap_or_default(),
            };
            styles.by_id.insert(id.to_string(), style);
        }
        styles
    }

    /// The style `id` and the styles it is based on, the most basic first. A chain
    /// that loops or runs absurdly long is cut short.
    fn chain<'s>(&'s self, id: Option<&'s str>) -> Vec<&'s Style> {
        let mut chain: Vec<&Style> = Vec::new();
        let mut next = id;
        while let Some(id) = next {
            let Some(style) = self.by_id.get(id) else { break };
            if chain.len() >= 16 || chain.iter().any(|s| std::ptr::eq(*s, style)) {
                break;
            }
            chain.push(style);
            next = style.based_on.as_deref();
        }
        chain.reverse();
        chain
    }

    fn run_props(&self, id: Option<&str>) -> RunProps {
        let mut props = RunProps::default();
        for style in self.chain(id) {
            props.over(&style.run);
        }
        props
    }
}

/// The heading level a style's name says, as the RTF importer reads one.
fn heading_level_from_style(name: &str) -> Option<u8> {
    let lower = name.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("heading") {
        return rest.trim().parse::<u8>().ok().filter(|l| (1..=6).contains(l)).or(Some(1));
    }
    match lower.as_str() {
        "title" => Some(1),
        "subtitle" => Some(2),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
struct Level {
    ordered: bool,
    start: i64,
}

#[derive(Debug, Default)]
struct Numbering {
    /// `w:num` id to its abstract definition's id and its per-level start overrides.
    nums: HashMap<i64, (i64, HashMap<i64, i64>)>,
    /// Abstract definition id to its levels, by `w:ilvl`.
    abstracts: HashMap<i64, HashMap<i64, Level>>,
}

impl Numbering {
    fn read(root: &El) -> Numbering {
        let mut numbering = Numbering::default();
        for el in root.elements() {
            match el.name.as_str() {
                "w:abstractNum" => {
                    let Some(id) = int_attr(el, "w:abstractNumId") else { continue };
                    let levels = el
                        .elements()
                        .filter(|l| l.name == "w:lvl")
                        .filter_map(|l| {
                            let ilvl = int_attr(l, "w:ilvl")?;
                            let format = l.child("w:numFmt").and_then(El::val).unwrap_or("decimal");
                            let start = l.child("w:start").and_then(|s| int_attr(s, "w:val")).unwrap_or(1);
                            Some((ilvl, Level { ordered: !matches!(format, "bullet" | "none"), start }))
                        })
                        .collect();
                    numbering.abstracts.insert(id, levels);
                }
                "w:num" => {
                    let Some(id) = int_attr(el, "w:numId") else { continue };
                    let Some(abstract_id) = el.child("w:abstractNumId").and_then(|a| int_attr(a, "w:val")) else { continue };
                    let overrides = el
                        .elements()
                        .filter(|o| o.name == "w:lvlOverride")
                        .filter_map(|o| Some((int_attr(o, "w:ilvl")?, int_attr(o.child("w:startOverride")?, "w:val")?)))
                        .collect();
                    numbering.nums.insert(id, (abstract_id, overrides));
                }
                _ => {}
            }
        }
        numbering
    }

    /// What level `ilvl` of list `num_id` is; a bulleted list when the numbering
    /// part does not say.
    fn level(&self, num_id: i64, ilvl: i64) -> Level {
        let Some((abstract_id, overrides)) = self.nums.get(&num_id) else {
            return Level { ordered: false, start: 1 };
        };
        let mut level = self
            .abstracts
            .get(abstract_id)
            .and_then(|levels| levels.get(&ilvl))
            .copied()
            .unwrap_or(Level { ordered: false, start: 1 });
        if let Some(start) = overrides.get(&ilvl) {
            level.start = *start;
        }
        level
    }
}

// ── The body ─────────────────────────────────────────────────────────

/// One paragraph as the walk emitted it, before list and heading grouping.
#[derive(Debug)]
struct Para {
    runs: Vec<Inline>,
    align: Align,
    /// Left indent in twips.
    indent: i64,
    heading: Option<u8>,
    list: Option<ListEntry>,
}

#[derive(Debug, Clone, Copy)]
struct ListEntry {
    num_id: i64,
    level: i64,
    ordered: bool,
    /// The number this entry shows, when the list is ordered.
    number: i64,
}

impl Para {
    fn is_empty(&self) -> bool {
        self.runs.iter().all(|inline| matches!(inline, Inline::Text(run) if run.text.trim().is_empty()))
    }
}

#[derive(Debug)]
enum Item {
    Para(Para),
    Table(Block),
}

/// A complex field (`w:fldChar begin` ... `separate` ... `end`) the walk is inside.
#[derive(Debug, Default)]
struct Field {
    /// Past `separate`: the displayed result, which is the text kept.
    in_result: bool,
    instruction: String,
    link: Option<String>,
}

struct Walker<'a> {
    styles: &'a Styles,
    numbering: &'a Numbering,
    links: &'a HashMap<String, String>,
    /// Open complex fields, the innermost last. They span runs and even paragraphs.
    fields: Vec<Field>,
    /// The next number of each `(numId, ilvl)`, so a list interrupted by a paragraph
    /// continues where it left off, as Word numbers it.
    counters: HashMap<(i64, i64), i64>,
    /// Whether a monospaced run is marked as code.
    code: bool,
    /// Characters of text emitted, and how many of them in a monospaced font.
    text_chars: usize,
    monospace_chars: usize,
}

impl Walker<'_> {
    /// The block-level content of `el` (the body, a cell, a content control).
    /// Inside a table, a nested table reads as its cells' paragraphs.
    fn blocks_in(&mut self, el: &El, out: &mut Vec<Item>, in_table: bool) {
        for child in el.elements() {
            match child.name.as_str() {
                "w:p" => out.push(Item::Para(self.paragraph(child))),
                "w:tbl" if in_table => {
                    for row in table_rows(child) {
                        for cell in row_cells(row) {
                            self.blocks_in(cell, out, true);
                        }
                    }
                }
                "w:tbl" => {
                    if let Some(table) = self.table(child) {
                        out.push(Item::Table(table));
                    }
                }
                "w:sdt" => {
                    if let Some(content) = child.child("w:sdtContent") {
                        self.blocks_in(content, out, in_table);
                    }
                }
                "w:customXml" | "w:ins" | "w:moveTo" => self.blocks_in(child, out, in_table),
                "mc:AlternateContent" => {
                    if let Some(content) = alternate(child) {
                        self.blocks_in(content, out, in_table);
                    }
                }
                _ => {}
            }
        }
    }

    fn paragraph(&mut self, p: &El) -> Para {
        let direct = p.child("w:pPr").map(ParaProps::read).unwrap_or_default();
        let style_id = direct.style.clone().or_else(|| self.styles.default_para.clone());
        let chain = self.styles.chain(style_id.as_deref());
        let mut props = ParaProps::default();
        for style in &chain {
            props.over(&style.para);
        }
        props.over(&direct);

        let named_level = chain.iter().rev().find_map(|s| heading_level_from_style(&s.name));
        let outline_level = props.outline.filter(|l| (0..=8).contains(l)).map(|l| (l + 1).min(6) as u8);
        let heading = named_level.or(outline_level);

        // A heading's look (bold, larger, a theme color) is what makes it a heading;
        // its runs keep only what is set on them.
        let mut base = self.styles.defaults.clone();
        if heading.is_none() {
            for style in &chain {
                base.over(&style.run);
            }
        }
        let mut runs = Vec::new();
        self.inlines(p, &base, None, &mut runs);

        let list = match props.num_id {
            Some(num_id) if num_id != 0 && heading.is_none() => {
                let level = props.ilvl.unwrap_or(0).clamp(0, 8);
                let def = self.numbering.level(num_id, level);
                let number = *self.counters.get(&(num_id, level)).unwrap_or(&def.start);
                self.counters.insert((num_id, level), number.saturating_add(1));
                // A deeper level starts again under each new entry above it.
                self.counters.retain(|&(n, l), _| n != num_id || l <= level);
                Some(ListEntry { num_id, level, ordered: def.ordered, number })
            }
            _ => None,
        };
        Para { runs, align: props.align.unwrap_or_default(), indent: props.left.unwrap_or(0), heading, list }
    }

    /// The inline content of a paragraph or of something inside one (a hyperlink,
    /// an insertion, a content control), appended to `out`.
    fn inlines(&mut self, el: &El, base: &RunProps, link: Option<&str>, out: &mut Vec<Inline>) {
        for child in el.elements() {
            match child.name.as_str() {
                "w:r" => self.run(child, base, link, out),
                "w:hyperlink" => {
                    // An internal link (`w:anchor`) has nowhere to go here: its text stays, unlinked.
                    let target = child.attr("r:id").and_then(|id| self.links.get(id)).map(String::as_str);
                    self.inlines(child, base, target.or(link), out);
                }
                "w:fldSimple" => {
                    let url = child.attr("w:instr").and_then(parse_hyperlink_url);
                    self.inlines(child, base, url.as_deref().or(link), out);
                }
                "w:ins" | "w:moveTo" | "w:smartTag" | "w:customXml" | "w:dir" | "w:bdo" => {
                    self.inlines(child, base, link, out)
                }
                "w:sdt" => {
                    if let Some(content) = child.child("w:sdtContent") {
                        self.inlines(content, base, link, out);
                    }
                }
                "mc:AlternateContent" => {
                    if let Some(content) = alternate(child) {
                        self.inlines(content, base, link, out);
                    }
                }
                // An equation reads as its text.
                "m:oMathPara" | "m:oMath" => {
                    let mut text = String::new();
                    math_text(child, &mut text, 0);
                    self.emit(out, &text, Vec::new());
                }
                // `w:del`, `w:moveFrom` (deleted text), `w:pPr`, bookmarks, comment
                // ranges, proofing marks: nothing to read.
                _ => {}
            }
        }
    }

    fn run(&mut self, r: &El, base: &RunProps, link: Option<&str>, out: &mut Vec<Inline>) {
        let (direct, style) = r.child("w:rPr").map(RunProps::read).unwrap_or_default();
        let mut props = base.clone();
        props.over(&self.styles.run_props(style.as_deref()));
        props.over(&direct);
        self.run_content(r, &props, &direct, link, out);
    }

    fn run_content(&mut self, r: &El, props: &RunProps, direct: &RunProps, link: Option<&str>, out: &mut Vec<Inline>) {
        for child in r.elements() {
            match child.name.as_str() {
                "w:fldChar" => match child.attr("w:fldCharType") {
                    Some("begin") => self.fields.push(Field::default()),
                    Some("separate") => {
                        if let Some(field) = self.fields.last_mut() {
                            field.in_result = true;
                            field.link = parse_hyperlink_url(&field.instruction);
                        }
                    }
                    Some("end") => {
                        self.fields.pop();
                    }
                    _ => {}
                },
                "w:instrText" => {
                    if let Some(field) = self.fields.last_mut().filter(|f| !f.in_result) {
                        field.instruction.push_str(&child.text());
                    }
                }
                "mc:AlternateContent" => {
                    if let Some(content) = alternate(child) {
                        self.run_content(content, props, direct, link, out);
                    }
                }
                _ if props.hidden == Some(true) || !self.visible() => {}
                "w:t" => {
                    let text = child.text();
                    let text = if child.attr("xml:space") == Some("preserve") {
                        text
                    } else {
                        text.trim_matches(|c| matches!(c, ' ' | '\t' | '\r' | '\n')).to_string()
                    };
                    let chars = text.chars().filter(|c| !c.is_whitespace()).count();
                    self.text_chars += chars;
                    if props.monospace == Some(true) {
                        self.monospace_chars += chars;
                    }
                    let marks = self.marks(props, direct, link);
                    self.emit(out, &text, marks);
                }
                "w:tab" | "w:ptab" => {
                    let marks = self.marks(props, direct, link);
                    self.emit(out, "\t", marks);
                }
                "w:noBreakHyphen" => {
                    let marks = self.marks(props, direct, link);
                    self.emit(out, "-", marks);
                }
                // A line break; a page or column break has nothing to be here.
                "w:br" if matches!(child.attr("w:type"), None | Some("textWrapping")) => out.push(Inline::HardBreak),
                "w:cr" => out.push(Inline::HardBreak),
                // `w:softHyphen` (invisible unless the line breaks there), symbols in
                // symbol fonts, drawings, pictures, embedded objects, footnote,
                // endnote and comment references: nothing to read.
                _ => {}
            }
        }
    }

    /// Whether text here is shown: not inside a field's instruction.
    fn visible(&self) -> bool {
        self.fields.iter().all(|f| f.in_result)
    }

    fn marks(&self, props: &RunProps, direct: &RunProps, link: Option<&str>) -> Vec<Mark> {
        let link = link.map(str::to_string).or_else(|| self.fields.iter().rev().find_map(|f| f.link.clone()));
        let on = |v: Option<bool>| v == Some(true);
        let mut marks = Vec::new();
        if self.code && on(props.monospace) {
            // `code` excludes the other formatting marks in the schema.
            marks.push(Mark::Code);
        } else {
            if on(props.bold) {
                marks.push(Mark::Bold);
            }
            if on(props.italic) {
                marks.push(Mark::Italic);
            }
            // A link's underline comes from the hyperlink style; the link mark shows it.
            if on(props.underline) && (link.is_none() || on(direct.underline)) {
                marks.push(Mark::Underline);
            }
            if on(props.strike) {
                marks.push(Mark::Strike);
            }
            if let Some(href) = link {
                marks.push(Mark::Link { href });
            }
        }
        match props.vert {
            Some(VertAlign::Superscript) => marks.push(Mark::Superscript),
            Some(VertAlign::Subscript) => marks.push(Mark::Subscript),
            _ => {}
        }
        if let Some(Some(color)) = &props.color {
            if color != "#000000" {
                marks.push(Mark::TextColor { color: color.clone() });
            }
        }
        let highlight = match &props.highlight {
            Some(highlight) => highlight.clone(),
            None => props.shading.clone().flatten(),
        };
        if let Some(color) = highlight {
            marks.push(Mark::Highlight { color: Some(color) });
        }
        marks
    }

    /// Append text to `out`, joining the previous run when the marks agree.
    fn emit(&self, out: &mut Vec<Inline>, text: &str, marks: Vec<Mark>) {
        if text.is_empty() {
            return;
        }
        if let Some(Inline::Text(last)) = out.last_mut() {
            if last.marks == marks {
                last.text.push_str(text);
                return;
            }
        }
        out.push(Inline::Text(Run { text: text.to_string(), marks }));
    }

    /// A `w:tbl` as a table: a row per `w:tr`, a cell per `w:tc` holding its blocks.
    /// A horizontal merge (`w:gridSpan`) is a colspan; a vertical merge (`w:vMerge`)
    /// is a rowspan on the cell that starts it, the cells it covers left out of their
    /// rows. `None` for a table with no cells.
    fn table(&mut self, tbl: &El) -> Option<Block> {
        let mut rows: Vec<TableRow> = Vec::new();
        // Grid column to the (row, cell) a vertical merge there started at.
        let mut merges: HashMap<usize, (usize, usize)> = HashMap::new();
        for tr in table_rows(tbl) {
            let trpr = tr.child("w:trPr");
            let header = trpr.and_then(|p| p.child("w:tblHeader")).map_or(false, toggle);
            let mut column =
                trpr.and_then(|p| p.child("w:gridBefore")).and_then(|g| int_attr(g, "w:val")).unwrap_or(0).clamp(0, 63) as usize;
            let mut cells: Vec<TableCell> = Vec::new();
            let mut extended: Vec<(usize, usize)> = Vec::new();
            for tc in row_cells(tr) {
                let tcpr = tc.child("w:tcPr");
                let span = tcpr.and_then(|p| p.child("w:gridSpan")).and_then(|g| int_attr(g, "w:val")).unwrap_or(1).clamp(1, 63)
                    as usize;
                let vmerge = tcpr.and_then(|p| p.child("w:vMerge")).map(|m| m.val().unwrap_or("continue"));
                if vmerge == Some("continue") {
                    if let Some(&(r, c)) = merges.get(&column) {
                        rows[r].cells[c].rowspan += 1;
                        extended.push((r, c));
                        column += span;
                        continue;
                    }
                }
                let mut items = Vec::new();
                self.blocks_in(tc, &mut items, true);
                let blocks = items_to_blocks(items);
                for c in column..column + span {
                    if vmerge == Some("restart") {
                        merges.insert(c, (rows.len(), cells.len()));
                    } else {
                        merges.remove(&c);
                    }
                }
                cells.push(TableCell { header, colspan: span as u32, rowspan: 1, blocks });
                column += span;
            }
            if cells.is_empty() {
                // A row that is all continuations is no row: the spans that reached it stop short of it.
                for (r, c) in extended {
                    rows[r].cells[c].rowspan -= 1;
                }
                continue;
            }
            rows.push(TableRow { cells });
        }
        (!rows.is_empty()).then_some(Block::Table { rows })
    }
}

/// A table's rows, including those wrapped in content controls or custom XML.
fn table_rows(tbl: &El) -> Vec<&El> {
    let mut rows = Vec::new();
    collect(tbl, "w:tr", &mut rows, 0);
    rows
}

/// A row's cells, including those wrapped in content controls or custom XML.
fn row_cells(tr: &El) -> Vec<&El> {
    let mut cells = Vec::new();
    collect(tr, "w:tc", &mut cells, 0);
    cells
}

fn collect<'e>(el: &'e El, name: &str, out: &mut Vec<&'e El>, depth: usize) {
    for child in el.elements() {
        if child.name == name {
            out.push(child);
        } else if depth < 8 && matches!(child.name.as_str(), "w:sdt" | "w:sdtContent" | "w:customXml") {
            collect(child, name, out, depth + 1);
        }
    }
}

fn math_text(el: &El, out: &mut String, depth: usize) {
    for child in el.elements() {
        if child.name == "m:t" {
            out.push_str(&child.text());
        } else if depth < MAX_DEPTH {
            math_text(child, out, depth + 1);
        }
    }
}

/// The URL of a `HYPERLINK` field instruction (`HYPERLINK "https://example.com"`);
/// `None` for another field, or a link to a bookmark (`\l`).
fn parse_hyperlink_url(instruction: &str) -> Option<String> {
    let rest = instruction.trim_start().strip_prefix("HYPERLINK")?.trim();
    let url = if let Some(quoted) = rest.strip_prefix('"') {
        quoted.split('"').next()?.to_string()
    } else {
        let first = rest.split_whitespace().next()?;
        if first.starts_with('\\') {
            return None;
        }
        first.to_string()
    };
    (!url.is_empty()).then_some(url)
}

// ── Paragraphs to blocks ─────────────────────────────────────────────

fn items_to_blocks(mut items: Vec<Item>) -> Vec<Block> {
    while matches!(items.last(), Some(Item::Para(p)) if p.is_empty() && p.list.is_none()) {
        items.pop();
    }
    let mut out = Vec::new();
    let mut lists = ListStack::default();
    for item in items {
        let para = match item {
            Item::Table(table) => {
                lists.flush(&mut out);
                out.push(table);
                continue;
            }
            Item::Para(para) => para,
        };
        if let Some(level) = para.heading {
            lists.flush(&mut out);
            out.push(Block::Heading { level: level.clamp(1, 6), runs: para.runs });
            continue;
        }
        if let Some(entry) = para.list {
            let block = Block::Paragraph { runs: para.runs, align: para.align, indent: 0 };
            lists.push_item(&mut out, entry, block);
            continue;
        }
        lists.flush(&mut out);
        // Indent in steps of half an inch, as the RTF importer counts it.
        let indent = if para.indent >= 360 { (para.indent as f64 / 720.0).round().min(16.0) as u32 } else { 0 };
        out.push(Block::Paragraph { runs: para.runs, align: para.align, indent });
    }
    lists.flush(&mut out);
    out
}

/// Builds nested lists from a run of list paragraphs: one open list per level, the
/// innermost last. A deeper level nests under the last item of the level above; a
/// shallower level closes the deeper lists into their parents first.
#[derive(Default)]
struct ListStack {
    open: Vec<OpenList>,
}

struct OpenList {
    num_id: i64,
    level: i64,
    ordered: bool,
    start: i64,
    items: Vec<ListItem>,
}

impl OpenList {
    fn into_block(self) -> Block {
        if self.ordered {
            Block::OrderedList { start: self.start, items: self.items }
        } else {
            Block::BulletList { items: self.items }
        }
    }
}

impl ListStack {
    fn push_item(&mut self, out: &mut Vec<Block>, entry: ListEntry, block: Block) {
        // Close lists deeper than this level, and a same-level list of another kind
        // or another list (a new list started right after the previous one).
        while let Some(top) = self.open.last() {
            let same = top.level == entry.level && top.num_id == entry.num_id && top.ordered == entry.ordered;
            if top.level > entry.level || (top.level == entry.level && !same) {
                self.close_top(out);
            } else {
                break;
            }
        }
        if self.open.last().map_or(true, |top| top.level < entry.level) {
            self.open.push(OpenList {
                num_id: entry.num_id,
                level: entry.level,
                ordered: entry.ordered,
                start: entry.number,
                items: Vec::new(),
            });
        }
        if let Some(top) = self.open.last_mut() {
            top.items.push(ListItem { blocks: vec![block] });
        }
    }

    fn close_top(&mut self, out: &mut Vec<Block>) {
        let Some(list) = self.open.pop() else { return };
        let block = list.into_block();
        match self.open.last_mut() {
            Some(parent) => {
                if parent.items.is_empty() {
                    parent.items.push(ListItem { blocks: Vec::new() });
                }
                if let Some(item) = parent.items.last_mut() {
                    item.blocks.push(block);
                }
            }
            None => out.push(block),
        }
    }

    fn flush(&mut self, out: &mut Vec<Block>) {
        while !self.open.is_empty() {
            self.close_top(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zip::write_zip;

    const NS: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006""#;

    /// A .docx whose body is `body`, with the optional parts given.
    fn docx_with(body: &str, styles: Option<&str>, numbering: Option<&str>, links: &[(&str, &str)]) -> Vec<u8> {
        let document = format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {NS}><w:body>{body}<w:sectPr><w:pgSz w:w="12240"/></w:sectPr></w:body></w:document>"#);
        let package_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let mut rels = String::from(r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdS" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rIdN" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/>"#);
        for (id, url) in links {
            rels.push_str(&format!(r#"<Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="{url}" TargetMode="External"/>"#));
        }
        rels.push_str("</Relationships>");
        let styles = styles.map(|s| format!(r#"<?xml version="1.0"?><w:styles {NS}>{s}</w:styles>"#));
        let numbering = numbering.map(|n| format!(r#"<?xml version="1.0"?><w:numbering {NS}>{n}</w:numbering>"#));
        let mut entries: Vec<(&str, &[u8], bool)> = vec![
            ("[Content_Types].xml", b"<Types/>", false),
            ("_rels/.rels", package_rels.as_bytes(), false),
            ("word/_rels/document.xml.rels", rels.as_bytes(), false),
            // Deflated, so method 8 is what every test reads the body through.
            ("word/document.xml", document.as_bytes(), true),
        ];
        if let Some(styles) = &styles {
            entries.push(("word/styles.xml", styles.as_bytes(), false));
        }
        if let Some(numbering) = &numbering {
            entries.push(("word/numbering.xml", numbering.as_bytes(), true));
        }
        write_zip(&entries, b"")
    }

    fn docx(body: &str) -> Vec<u8> {
        docx_with(body, None, None, &[])
    }

    fn runs_of(block: &Block) -> Vec<Run> {
        match block {
            Block::Paragraph { runs, .. } | Block::Heading { runs, .. } => runs.iter().filter_map(Inline::as_run).cloned().collect(),
            other => panic!("expected a text block, got {other:?}"),
        }
    }

    fn find(runs: &[Run], text: &str) -> Run {
        runs.iter().find(|r| r.text.trim() == text).cloned().unwrap_or_else(|| panic!("no run {text:?} in {runs:?}"))
    }

    /// Every result must also build a node's document: the importer never produces
    /// content the editor refuses.
    fn blocks(bytes: &[u8]) -> Vec<Block> {
        let blocks = docx_to_blocks(bytes).unwrap();
        pimble_crdt::NodeDoc::from_blocks(&blocks).unwrap();
        blocks
    }

    #[test]
    fn plain_paragraphs() {
        let blocks = blocks(&docx(
            r#"<w:p><w:r><w:t>Hello World</w:t></w:r></w:p><w:p/><w:p><w:r><w:t xml:space="preserve">Second </w:t></w:r><w:r><w:t>line</w:t></w:r></w:p><w:p/><w:p/>"#,
        ));
        assert_eq!(blocks.len(), 3, "{blocks:?}");
        assert_eq!(blocks[0].plain_text(), "Hello World");
        assert_eq!(blocks[1].plain_text(), "", "an empty paragraph inside the text stays");
        assert_eq!(blocks[2].plain_text(), "Second line");
    }

    #[test]
    fn run_formatting() {
        let body = r#"<w:p>
            <w:r><w:t xml:space="preserve">Normal </w:t></w:r>
            <w:r><w:rPr><w:b/></w:rPr><w:t>bold</w:t></w:r>
            <w:r><w:rPr><w:i w:val="1"/><w:b w:val="0"/></w:rPr><w:t>italic</w:t></w:r>
            <w:r><w:rPr><w:u w:val="single"/></w:rPr><w:t>under</w:t></w:r>
            <w:r><w:rPr><w:u w:val="none"/></w:rPr><w:t>notunder</w:t></w:r>
            <w:r><w:rPr><w:dstrike/></w:rPr><w:t>gone</w:t></w:r>
            <w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:t>sup</w:t></w:r>
            <w:r><w:rPr><w:color w:val="FF0000"/></w:rPr><w:t>red</w:t></w:r>
            <w:r><w:rPr><w:color w:val="auto"/></w:rPr><w:t>auto</w:t></w:r>
            <w:r><w:rPr><w:highlight w:val="yellow"/></w:rPr><w:t>marked</w:t></w:r>
            <w:r><w:rPr><w:shd w:val="clear" w:fill="00FF00"/></w:rPr><w:t>shaded</w:t></w:r>
            <w:r><w:rPr><w:rStyle w:val="Strong"/></w:rPr><w:t>strong</w:t></w:r>
            <w:r><w:rPr><w:rFonts w:ascii="Courier New"/><w:b/></w:rPr><w:t>code</w:t></w:r>
            <w:r><w:rPr><w:vanish/></w:rPr><w:t>hidden</w:t></w:r>
            <w:r><w:tab/><w:t>a</w:t><w:br/><w:t>b</w:t><w:br w:type="page"/><w:noBreakHyphen/><w:softHyphen/></w:r>
        </w:p>"#;
        let styles = r#"<w:style w:type="character" w:styleId="Strong"><w:name w:val="Strong"/><w:rPr><w:b/><w:color w:val="00FF00"/></w:rPr></w:style>"#;
        let blocks = blocks(&docx_with(body, Some(styles), None, &[]));
        let runs = runs_of(&blocks[0]);
        assert_eq!(find(&runs, "bold").marks, vec![Mark::Bold]);
        assert_eq!(find(&runs, "italic").marks, vec![Mark::Italic]);
        assert_eq!(find(&runs, "under").marks, vec![Mark::Underline]);
        assert_eq!(find(&runs, "notunder").marks, vec![]);
        assert_eq!(find(&runs, "gone").marks, vec![Mark::Strike]);
        assert_eq!(find(&runs, "sup").marks, vec![Mark::Superscript]);
        assert_eq!(find(&runs, "red").marks, vec![Mark::TextColor { color: "#ff0000".into() }]);
        assert_eq!(find(&runs, "auto").marks, vec![]);
        assert_eq!(find(&runs, "marked").marks, vec![Mark::Highlight { color: Some("#ffff00".into()) }]);
        assert_eq!(find(&runs, "shaded").marks, vec![Mark::Highlight { color: Some("#00ff00".into()) }]);
        assert_eq!(find(&runs, "strong").marks, vec![Mark::Bold], "a style's color is not imported");
        assert_eq!(find(&runs, "code").marks, vec![Mark::Code]);
        let text = blocks[0].plain_text();
        assert!(!text.contains("hidden"), "{text:?}");
        assert!(text.ends_with("\ta\nb-"), "{text:?}");
        let Block::Paragraph { runs, .. } = &blocks[0] else { panic!() };
        assert!(runs.contains(&Inline::HardBreak));
    }

    #[test]
    fn a_manuscript_in_courier_is_not_code() {
        let courier = r#"<w:rPr><w:rFonts w:ascii="Courier New"/><w:i/></w:rPr>"#;
        let body = format!(r#"<w:p><w:r>{courier}<w:t>All of the words of the story</w:t></w:r><w:r><w:t xml:space="preserve"> and a few</w:t></w:r></w:p>"#);
        let blocks = blocks(&docx(&body));
        assert_eq!(runs_of(&blocks[0])[0].marks, vec![Mark::Italic]);
    }

    #[test]
    fn headings_from_styles_and_outline_levels() {
        let styles = r#"
            <w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
            <w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:basedOn w:val="Normal"/><w:rPr><w:b/><w:color w:val="2F5496"/></w:rPr></w:style>
            <w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/></w:style>
            <w:style w:type="paragraph" w:styleId="MyChapter"><w:name w:val="My Chapter"/><w:basedOn w:val="Heading2"/></w:style>
            <w:style w:type="paragraph" w:styleId="Quote"><w:name w:val="Quote"/><w:rPr><w:i/></w:rPr></w:style>"#;
        let body = r#"
            <w:p><w:pPr><w:pStyle w:val="Title"/></w:pPr><w:r><w:t>The Title</w:t></w:r></w:p>
            <w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>Section</w:t></w:r></w:p>
            <w:p><w:pPr><w:pStyle w:val="MyChapter"/></w:pPr><w:r><w:t>Chapter</w:t></w:r></w:p>
            <w:p><w:pPr><w:outlineLvl w:val="2"/></w:pPr><w:r><w:t>Outlined</w:t></w:r></w:p>
            <w:p><w:pPr><w:pStyle w:val="Quote"/><w:jc w:val="center"/></w:pPr><w:r><w:t>Said</w:t></w:r></w:p>
            <w:p><w:pPr><w:jc w:val="both"/><w:ind w:left="1440"/></w:pPr><w:r><w:t>Body</w:t></w:r></w:p>"#;
        let blocks = blocks(&docx_with(body, Some(styles), None, &[]));
        assert!(matches!(&blocks[0], Block::Heading { level: 1, .. }), "{:?}", blocks[0]);
        assert!(matches!(&blocks[1], Block::Heading { level: 2, .. }), "{:?}", blocks[1]);
        assert_eq!(runs_of(&blocks[1])[0].marks, vec![], "a heading's style look is not marked on its runs");
        assert!(matches!(&blocks[2], Block::Heading { level: 2, .. }), "{:?}", blocks[2]);
        assert!(matches!(&blocks[3], Block::Heading { level: 3, .. }), "{:?}", blocks[3]);
        assert!(matches!(&blocks[4], Block::Paragraph { align: Align::Center, indent: 0, .. }), "{:?}", blocks[4]);
        assert_eq!(runs_of(&blocks[4])[0].marks, vec![Mark::Italic], "a paragraph style's italic reaches its runs");
        assert!(matches!(&blocks[5], Block::Paragraph { align: Align::Justify, indent: 2, .. }), "{:?}", blocks[5]);
    }

    #[test]
    fn lists_nest_and_number() {
        let numbering = r#"
            <w:abstractNum w:abstractNumId="0">
                <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/></w:lvl>
                <w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="bullet"/></w:lvl>
            </w:abstractNum>
            <w:abstractNum w:abstractNumId="1">
                <w:lvl w:ilvl="0"><w:start w:val="3"/><w:numFmt w:val="decimal"/></w:lvl>
            </w:abstractNum>
            <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
            <w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num>"#;
        let item = |num: u8, level: u8, text: &str| {
            format!(r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="{level}"/><w:numId w:val="{num}"/></w:numPr></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#)
        };
        let body = [
            item(1, 0, "one"),
            item(1, 1, "one.a"),
            item(1, 1, "one.b"),
            item(1, 0, "two"),
            r#"<w:p><w:r><w:t>between</w:t></w:r></w:p>"#.to_string(),
            item(2, 0, "third"),
            item(2, 0, "fourth"),
            r#"<w:p><w:r><w:t>interrupted</w:t></w:r></w:p>"#.to_string(),
            item(2, 0, "fifth"),
        ]
        .concat();
        let blocks = blocks(&docx_with(&body, None, Some(numbering), &[]));
        assert_eq!(blocks.len(), 5, "{blocks:#?}");
        let Block::BulletList { items } = &blocks[0] else { panic!("{:?}", blocks[0]) };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].blocks[0].plain_text(), "one");
        let Block::BulletList { items: nested } = &items[0].blocks[1] else { panic!("{:?}", items[0]) };
        assert_eq!(nested.len(), 2);
        assert_eq!(nested[1].blocks[0].plain_text(), "one.b");
        assert_eq!(items[1].blocks[0].plain_text(), "two");
        let Block::OrderedList { start: 3, items } = &blocks[2] else { panic!("{:?}", blocks[2]) };
        assert_eq!(items.len(), 2);
        let Block::OrderedList { start: 5, items } = &blocks[4] else { panic!("an interrupted list continues: {:?}", blocks[4]) };
        assert_eq!(items[0].blocks[0].plain_text(), "fifth");
    }

    #[test]
    fn a_list_paragraph_style_carries_its_numbering() {
        let styles = r#"<w:style w:type="paragraph" w:styleId="ListBullet"><w:name w:val="List Bullet"/><w:pPr><w:numPr><w:numId w:val="1"/></w:numPr></w:pPr></w:style>"#;
        let numbering = r#"<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#;
        let body = r#"<w:p><w:pPr><w:pStyle w:val="ListBullet"/></w:pPr><w:r><w:t>a</w:t></w:r></w:p><w:p><w:pPr><w:pStyle w:val="ListBullet"/></w:pPr><w:r><w:t>b</w:t></w:r></w:p>"#;
        let blocks = blocks(&docx_with(body, Some(styles), Some(numbering), &[]));
        assert!(matches!(&blocks[..], [Block::BulletList { items }] if items.len() == 2), "{blocks:?}");
    }

    #[test]
    fn tables_with_merged_cells() {
        let cell = |props: &str, text: &str| format!(r#"<w:tc><w:tcPr>{props}</w:tcPr><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:tc>"#);
        let body = format!(
            r#"<w:p><w:r><w:t>Before</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol/><w:gridCol/><w:gridCol/></w:tblGrid>
                <w:tr><w:trPr><w:tblHeader/></w:trPr>{}{}</w:tr>
                <w:tr>{}{}{}</w:tr>
                <w:tr>{}{}{}</w:tr>
            </w:tbl><w:p><w:r><w:t>After</w:t></w:r></w:p>"#,
            cell(r#"<w:gridSpan w:val="2"/>"#, "Wide"),
            cell("", "H3"),
            cell(r#"<w:vMerge w:val="restart"/>"#, "Tall"),
            cell("", "b"),
            cell("", "c"),
            cell("<w:vMerge/>", ""),
            cell("", "e"),
            format!(
                r#"<w:tc><w:p><w:r><w:t>f</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>nested</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:tc>"#
            ),
        );
        let blocks = blocks(&docx(&body));
        assert_eq!(blocks.len(), 3, "{blocks:#?}");
        let Block::Table { rows } = &blocks[1] else { panic!("{:?}", blocks[1]) };
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].cells.len(), 2);
        assert!(rows[0].cells[0].header);
        assert_eq!(rows[0].cells[0].colspan, 2);
        assert_eq!(rows[1].cells[0].rowspan, 2);
        assert_eq!(rows[1].cells[0].blocks[0].plain_text(), "Tall");
        assert_eq!(rows[2].cells.len(), 2, "the covered cell is left out");
        assert_eq!(rows[2].cells[0].blocks[0].plain_text(), "e");
        let last: Vec<String> = rows[2].cells[1].blocks.iter().map(Block::plain_text).collect();
        assert_eq!(last, vec!["f", "nested"], "a nested table reads as its paragraphs");
    }

    #[test]
    fn hyperlinks_take_their_target() {
        let styles = r#"<w:style w:type="character" w:styleId="Hyperlink"><w:name w:val="Hyperlink"/><w:rPr><w:u w:val="single"/><w:color w:val="0563C1"/></w:rPr></w:style>"#;
        let body = r#"<w:p><w:r><w:t xml:space="preserve">See </w:t></w:r><w:hyperlink r:id="rId9"><w:r><w:rPr><w:rStyle w:val="Hyperlink"/></w:rPr><w:t>the site</w:t></w:r></w:hyperlink><w:hyperlink w:anchor="_Toc1"><w:r><w:t xml:space="preserve"> and here</w:t></w:r></w:hyperlink></w:p>"#;
        let blocks = blocks(&docx_with(body, Some(styles), None, &[("rId9", "https://example.com/a?b=1&amp;c=2")]));
        let runs = runs_of(&blocks[0]);
        assert_eq!(find(&runs, "the site").marks, vec![Mark::Link { href: "https://example.com/a?b=1&c=2".into() }]);
        assert_eq!(find(&runs, "and here").marks, vec![]);
    }

    #[test]
    fn tracked_changes_and_fields() {
        let body = r#"<w:p>
            <w:r><w:t xml:space="preserve">Kept </w:t></w:r>
            <w:del w:id="1" w:author="x"><w:r><w:delText>deleted </w:delText></w:r></w:del>
            <w:ins w:id="2" w:author="x"><w:r><w:t xml:space="preserve">inserted </w:t></w:r></w:ins>
            <w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>7</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>
            <w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> HYPERLINK "https://pimble.app" </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> link</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>
            <w:fldSimple w:instr=" DATE "><w:r><w:t xml:space="preserve"> today</w:t></w:r></w:fldSimple>
            <w:r><w:footnoteReference w:id="1"/></w:r>
            <w:r><w:drawing><w:inline/></w:drawing></w:r>
            <mc:AlternateContent><mc:Choice Requires="wps"><w:r><w:t xml:space="preserve"> chosen</w:t></w:r></mc:Choice><mc:Fallback><w:r><w:t xml:space="preserve"> fallback</w:t></w:r></mc:Fallback></mc:AlternateContent>
            <w:sdt><w:sdtContent><w:r><w:t xml:space="preserve"> control</w:t></w:r></w:sdtContent></w:sdt>
        </w:p>"#;
        let blocks = blocks(&docx(body));
        assert_eq!(blocks[0].plain_text(), "Kept inserted 7 link today chosen control");
        let runs = runs_of(&blocks[0]);
        assert_eq!(find(&runs, "link").marks, vec![Mark::Link { href: "https://pimble.app".into() }]);
    }

    #[test]
    fn a_prefix_other_than_w_reads_the_same() {
        let document = r#"<?xml version="1.0"?><x:document xmlns:x="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><x:body><x:p><x:r><x:rPr><x:b/></x:rPr><x:t>Odd</x:t></x:r></x:p></x:body></x:document>"#;
        // No package relationships at all: the main part is found by its usual name.
        let zip = write_zip(&[("word/document.xml", document.as_bytes(), false)], b"");
        let blocks = blocks(&zip);
        assert_eq!(runs_of(&blocks[0]), vec![Run::marked("Odd", vec![Mark::Bold])]);
    }

    #[test]
    fn malformed_input_errors_without_panicking() {
        let not_docx = |bytes: &[u8]| {
            let e = docx_to_blocks(bytes).unwrap_err().to_string();
            assert_eq!(e, NOT_A_DOCX);
        };
        not_docx(b"");
        not_docx(b"{\\rtf1 not a zip}");
        not_docx(&write_zip(&[("hello.txt", b"hi", false)], b""));

        let good = docx(r#"<w:p><w:r><w:t>Hello</w:t></w:r></w:p>"#);
        for len in 0..good.len() {
            let _ = docx_to_blocks(&good[..len]);
        }
        let mut state = 0x9E37_79B9u32;
        for _ in 0..200 {
            let mut bad = good.clone();
            for _ in 0..4 {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let at = state as usize % bad.len();
                bad[at] = (state >> 8) as u8;
            }
            let _ = docx_to_blocks(&bad);
        }

        let broken = write_zip(&[("word/document.xml", b"<w:document><w:body><w:p>", false)], b"");
        assert!(docx_to_blocks(&broken).is_err());
        let deep = format!("<a>{}</a>", "<b>".repeat(MAX_DEPTH + 10));
        let deep = write_zip(&[("word/document.xml", deep.as_bytes(), false)], b"");
        assert!(docx_to_blocks(&deep).is_err());
    }
}
