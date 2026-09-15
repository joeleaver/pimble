//! RTF to rich blocks, for the subset of RTF Scrivener writes.
//!
//! Two passes. The tokenizer turns the bytes into groups, control words and characters
//! (decoding `\'xx` as Windows-1252 and `\uN` as Unicode with its fallback skipped).
//! The interpreter walks the tokens with a stack of group-scoped formatting states,
//! reads the font, colour, stylesheet and list tables, and emits one [`Para`] per
//! `\par` with its runs and paragraph properties. [`paras_to_blocks`] then groups list
//! paragraphs into nested [`Block::BulletList`]/[`Block::OrderedList`]s, turns styled
//! or bold-and-larger paragraphs into headings, and produces the [`Block`]s a
//! [`pimble_crdt::ContentDoc`] is built from.
//!
//! Everything the content model cannot hold is reduced rather than dropped silently:
//! a table becomes one paragraph per row with cells separated by tabs, a line break
//! becomes a paragraph break, a picture is skipped, and Scrivener's inline placeholder
//! tags (`<$Scr_Ps::0>`, `<!$Scr_H::4>`) are stripped from the text.

use std::collections::HashMap;

use pimble_crdt::{Align, Block, ListItem, Mark, Run};

// ── Tokenizer ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Open,
    Close,
    Control { word: String, param: Option<i32> },
    /// A literal character of document text (already decoded).
    Char(char),
}

fn tokenize(rtf: &[u8]) -> Vec<Token> {
    let input = String::from_utf8_lossy(rtf);
    let mut tokens = Vec::with_capacity(input.len() / 2);
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '{' => tokens.push(Token::Open),
            '}' => tokens.push(Token::Close),
            '\r' | '\n' => {}
            '\\' => {
                let Some(&next) = chars.peek() else { break };
                if next.is_ascii_alphabetic() {
                    let (word, param) = read_control_word(&mut chars);
                    tokens.push(Token::Control { word, param });
                } else {
                    chars.next();
                    match next {
                        '\'' => {
                            let hex: String = chars.by_ref().take(2).collect();
                            if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                                tokens.push(Token::Char(decode_windows_1252(byte)));
                            }
                        }
                        '\\' | '{' | '}' => tokens.push(Token::Char(next)),
                        '~' => tokens.push(Token::Char('\u{00A0}')),
                        '_' => tokens.push(Token::Char('\u{2011}')),
                        '-' => {} // optional hyphen: invisible unless the line breaks there
                        '*' => tokens.push(Token::Control { word: "*".into(), param: None }),
                        '\r' | '\n' => tokens.push(Token::Control { word: "par".into(), param: None }),
                        _ => {} // an unknown control symbol
                    }
                }
            }
            _ => tokens.push(Token::Char(ch)),
        }
    }
    tokens
}

/// Read a control word and its optional numeric parameter; consumes the single
/// space that may delimit it.
fn read_control_word(chars: &mut std::iter::Peekable<std::str::Chars>) -> (String, Option<i32>) {
    let mut word = String::new();
    while let Some(&ch) = chars.peek() {
        if ch.is_ascii_alphabetic() {
            word.push(ch);
            chars.next();
        } else {
            break;
        }
    }
    let mut param_str = String::new();
    if let Some(&ch) = chars.peek() {
        if ch == '-' || ch.is_ascii_digit() {
            param_str.push(ch);
            chars.next();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() {
                    param_str.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
        }
    }
    if chars.peek() == Some(&' ') {
        chars.next();
    }
    let param = if param_str.is_empty() { None } else { param_str.parse().ok() };
    (word, param)
}

// ── Interpreter state ────────────────────────────────────────────────

/// Character formatting in force; copied on `{` and restored on `}`.
#[derive(Debug, Clone, Default, PartialEq)]
struct CharFmt {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    superscript: bool,
    subscript: bool,
    /// Index into the colour table; `None` or `Some(0)` is the automatic colour.
    color: Option<usize>,
    /// Index into the colour table, for `\highlightN` / `\cbN`.
    highlight: Option<usize>,
    /// Index into the font table.
    font: Option<usize>,
    /// Font size in half-points (`\fsN`).
    size: Option<i32>,
    /// The URL of the hyperlink field whose result this text is.
    link: Option<String>,
}

/// Paragraph formatting in force; reset by `\pard`, scoped like everything else.
#[derive(Debug, Clone, Default, PartialEq)]
struct ParaFmt {
    align: Align,
    /// Left indent in twips (`\liN`).
    left_indent: i32,
    /// `(\lsN, \ilvlN)` when the paragraph is a list entry.
    list: Option<(i32, i32)>,
    /// Stylesheet index (`\sN`).
    style: Option<i32>,
    in_table: bool,
}

/// What a group is for: ordinary text, a table this parser reads, or something skipped.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Dest {
    /// Not decided yet: the first control word of the group says.
    Pending,
    /// `\*` seen; the next control word decides between `fldinst` and skipping.
    PendingStar,
    Text,
    Skip,
    FontTable,
    ColorTable,
    StyleSheet,
    ListTable,
    ListOverrideTable,
    FieldInst,
}

#[derive(Debug, Clone)]
struct Group {
    dest: Dest,
    chr: CharFmt,
    para: ParaFmt,
    /// Unicode fallback length in force (`\ucN`), default 1.
    uc: usize,
    /// For a `\field` group: the URL its `\fldinst` child found, for its `\fldrslt`.
    field_url: Option<String>,
    is_field: bool,
}

/// One paragraph as the interpreter emitted it, before list/heading grouping.
#[derive(Debug, Clone)]
struct Para {
    runs: Vec<Run>,
    fmt: ParaFmt,
    /// Whether every run with text is bold (a heading candidate).
    all_bold: bool,
    /// The font size of the paragraph's first text, in half-points.
    size: Option<i32>,
}

#[derive(Debug, Default)]
struct ListDef {
    id: Option<i32>,
    /// Per level: whether the level is numbered (as opposed to bulleted).
    ordered_levels: Vec<bool>,
}

#[derive(Debug, Default)]
struct Tables {
    fonts: HashMap<usize, String>,
    /// `#rrggbb` per colour-table entry; entry 0 is the automatic colour.
    colors: Vec<Option<String>>,
    /// Stylesheet index to style name.
    styles: HashMap<i32, String>,
    lists: Vec<ListDef>,
    /// `\lsN` to `\listidN`.
    overrides: HashMap<i32, i32>,
}

struct Interp {
    tables: Tables,
    stack: Vec<Group>,
    paras: Vec<Para>,
    runs: Vec<Run>,
    text: String,
    /// The formatting the text in `text` was written with.
    text_fmt: CharFmt,
    /// A `\fldinst` group's instruction text, kept apart from document text.
    inst: String,
    /// Unicode fallback characters still to skip after a `\uN`.
    skip_chars: usize,
    // Table-reading scratch state.
    font_index: Option<usize>,
    font_name: String,
    color_rgb: [u8; 3],
    color_seen: bool,
    style_index: Option<i32>,
    style_name: String,
    all_bold: bool,
    para_size: Option<i32>,
}

impl Interp {
    fn new() -> Self {
        let root = Group {
            dest: Dest::Text,
            chr: CharFmt::default(),
            para: ParaFmt::default(),
            uc: 1,
            field_url: None,
            is_field: false,
        };
        Interp {
            tables: Tables::default(),
            stack: vec![root],
            paras: Vec::new(),
            runs: Vec::new(),
            text: String::new(),
            text_fmt: CharFmt::default(),
            inst: String::new(),
            skip_chars: 0,
            font_index: None,
            font_name: String::new(),
            color_rgb: [0; 3],
            color_seen: false,
            style_index: None,
            style_name: String::new(),
            all_bold: true,
            para_size: None,
        }
    }

    fn group(&mut self) -> &mut Group {
        self.stack.last_mut().expect("the root group is never popped")
    }

    fn dest(&self) -> Dest {
        self.stack.last().map(|g| g.dest).unwrap_or(Dest::Text)
    }

    /// The innermost decided destination: a `Pending` group inherits its parent's.
    fn effective_dest(&self) -> Dest {
        for g in self.stack.iter().rev() {
            match g.dest {
                Dest::Pending | Dest::PendingStar => continue,
                d => return d,
            }
        }
        Dest::Text
    }

    fn run(&mut self, tokens: &[Token]) {
        for token in tokens {
            match token {
                Token::Open => self.open(),
                Token::Close => self.close(),
                Token::Control { word, param } => self.control(word, *param),
                Token::Char(c) => self.character(*c),
            }
        }
        self.flush_run();
        self.finish_para();
    }

    fn open(&mut self) {
        let parent = self.stack.last().expect("root").clone();
        // A group inside a table or skipped destination belongs to it (a font
        // entry inside `\fonttbl`, a level inside `\listtable`); only a group in
        // document text has its own destination to decide.
        let dest = match self.effective_dest() {
            Dest::Skip | Dest::PendingStar => Dest::Skip,
            Dest::Text | Dest::Pending => Dest::Pending,
            table => table,
        };
        self.stack.push(Group { dest, chr: parent.chr, para: parent.para, uc: parent.uc, field_url: None, is_field: false });
        self.skip_chars = 0;
    }

    fn close(&mut self) {
        if self.stack.len() <= 1 {
            return;
        }
        let closing = self.stack.pop().expect("checked");
        self.skip_chars = 0;
        match closing.dest {
            Dest::FieldInst => {
                // Hand the instruction's URL to the enclosing field group.
                let inst = std::mem::take(&mut self.inst);
                if let Some(url) = parse_hyperlink_url(&inst) {
                    if let Some(field) = self.stack.iter_mut().rev().find(|g| g.is_field) {
                        field.field_url = Some(url);
                    }
                }
            }
            Dest::FontTable => self.finish_font(),
            Dest::StyleSheet => self.finish_style(),
            Dest::Text | Dest::Pending => {
                // Character formatting changes on the way out: flush what was
                // written under the closing group's formatting first.
                let outer = self.stack.last().expect("root").chr.clone();
                if outer != closing.chr {
                    self.flush_run();
                }
            }
            _ => {}
        }
    }

    fn control(&mut self, word: &str, param: Option<i32>) {
        self.skip_chars = 0;
        // Decide a pending group's destination from its first control word.
        match self.dest() {
            Dest::FontTable | Dest::ColorTable | Dest::StyleSheet | Dest::ListTable | Dest::ListOverrideTable
                if word == "*" =>
            {
                // An ignorable group inside a table (`{\*\panose ...}` in a font
                // entry) is not part of the entry.
                self.group().dest = Dest::Skip;
                return;
            }
            Dest::Pending => {
                let dest = match word {
                    "*" => Dest::PendingStar,
                    "fonttbl" => Dest::FontTable,
                    "colortbl" => Dest::ColorTable,
                    "stylesheet" => Dest::StyleSheet,
                    "field" => {
                        self.group().is_field = true;
                        Dest::Text
                    }
                    "fldrslt" => {
                        let url = self.stack.iter().rev().find(|g| g.is_field).and_then(|g| g.field_url.clone());
                        if url.is_some() {
                            self.flush_run();
                            self.group().chr.link = url;
                        }
                        Dest::Text
                    }
                    "info" | "header" | "footer" | "headerl" | "headerr" | "footerl" | "footerr" | "pict"
                    | "shppict" | "nonshppict" | "listtext" | "pntext" | "footnote" | "annotation" | "object"
                    | "xe" | "tc" | "themedata" | "colorschememapping" | "datastore" | "latentstyles" | "rsidtbl"
                    | "generator" | "userprops" | "docvar" => Dest::Skip,
                    _ => Dest::Text,
                };
                self.group().dest = dest;
                if dest != Dest::Text {
                    return;
                }
            }
            Dest::PendingStar => {
                let dest = match word {
                    "fldinst" => {
                        // The instruction collects into its own buffer; the
                        // document text written so far stays a run.
                        self.flush_run();
                        self.inst.clear();
                        Dest::FieldInst
                    }
                    "listtable" => Dest::ListTable,
                    "listoverridetable" => Dest::ListOverrideTable,
                    _ => Dest::Skip,
                };
                self.group().dest = dest;
                return;
            }
            _ => {}
        }

        match self.effective_dest() {
            Dest::Skip | Dest::FieldInst => {}
            Dest::FontTable => self.font_table_word(word, param),
            Dest::ColorTable => self.color_table_word(word, param),
            Dest::StyleSheet => {
                if word == "s" || word == "cs" || word == "ds" {
                    self.style_index = param;
                }
            }
            Dest::ListTable => self.list_table_word(word, param),
            Dest::ListOverrideTable => match word {
                "listoverride" => self.tables.overrides.insert(-1, -1).map(|_| ()).unwrap_or(()),
                "listid" => {
                    self.tables.overrides.insert(-1, param.unwrap_or(0));
                }
                "ls" => {
                    if let Some(id) = self.tables.overrides.remove(&-1) {
                        self.tables.overrides.insert(param.unwrap_or(0), id);
                    }
                }
                _ => {}
            },
            Dest::Text | Dest::Pending | Dest::PendingStar => self.text_word(word, param),
        }
    }

    fn text_word(&mut self, word: &str, param: Option<i32>) {
        let on = param != Some(0);
        match word {
            "par" | "line" | "row" => {
                self.flush_run();
                self.finish_para();
            }
            "cell" => {
                self.flush_run();
                self.push_text('\t');
                self.flush_run();
            }
            "pard" => {
                self.group().para = ParaFmt::default();
            }
            "plain" => {
                self.flush_run();
                let link = self.group().chr.link.clone();
                let g = self.group();
                g.chr = CharFmt { link, ..CharFmt::default() };
            }
            "b" => self.set_chr(|c| c.bold = on),
            "i" => self.set_chr(|c| c.italic = on),
            "ul" | "uld" | "uldb" | "ulw" | "ulth" | "ulwave" => self.set_chr(|c| c.underline = on),
            "ulnone" => self.set_chr(|c| c.underline = false),
            "strike" | "striked" => self.set_chr(|c| c.strike = on),
            "super" => self.set_chr(|c| {
                c.superscript = true;
                c.subscript = false;
            }),
            "sub" => self.set_chr(|c| {
                c.subscript = true;
                c.superscript = false;
            }),
            "nosupersub" => self.set_chr(|c| {
                c.superscript = false;
                c.subscript = false;
            }),
            "cf" => self.set_chr(|c| c.color = param.map(|p| p.max(0) as usize)),
            "highlight" | "cb" => self.set_chr(|c| c.highlight = param.map(|p| p.max(0) as usize)),
            "f" => self.set_chr(|c| c.font = param.map(|p| p.max(0) as usize)),
            "fs" => self.set_chr(|c| c.size = param),
            "qc" => self.group().para.align = Align::Center,
            "qr" => self.group().para.align = Align::Right,
            "qj" => self.group().para.align = Align::Justify,
            "ql" => self.group().para.align = Align::Left,
            "li" => self.group().para.left_indent = param.unwrap_or(0),
            "ls" => {
                let level = self.group().para.list.map(|(_, l)| l).unwrap_or(0);
                self.group().para.list = Some((param.unwrap_or(0), level));
            }
            "ilvl" => {
                if let Some((ls, _)) = self.group().para.list {
                    self.group().para.list = Some((ls, param.unwrap_or(0)));
                }
            }
            "s" => self.group().para.style = param,
            "intbl" => self.group().para.in_table = true,
            "uc" => self.group().uc = param.unwrap_or(1).max(0) as usize,
            "u" => {
                if let Some(code) = param {
                    let code = if code < 0 { code + 65536 } else { code };
                    if let Some(c) = char::from_u32(code as u32) {
                        self.push_text(c);
                    }
                    self.skip_chars = self.group().uc;
                }
            }
            "tab" => self.push_text('\t'),
            "emdash" => self.push_text('\u{2014}'),
            "endash" => self.push_text('\u{2013}'),
            "lquote" => self.push_text('\u{2018}'),
            "rquote" => self.push_text('\u{2019}'),
            "ldblquote" => self.push_text('\u{201C}'),
            "rdblquote" => self.push_text('\u{201D}'),
            "bullet" => self.push_text('\u{2022}'),
            "emspace" | "enspace" | "qmspace" => self.push_text(' '),
            "sect" | "page" | "column" => {
                self.flush_run();
                self.finish_para();
            }
            _ => {}
        }
    }

    /// Append a character of document text, recording the formatting in force when
    /// it starts a new run (`\tab`, `\uN` and the like must do this exactly as a
    /// plain character does, or the run inherits a stale formatting).
    fn push_text(&mut self, c: char) {
        if self.text.is_empty() {
            self.text_fmt = self.group().chr.clone();
        }
        self.text.push(c);
    }

    /// Change character formatting, flushing the text written under the old one.
    fn set_chr(&mut self, change: impl FnOnce(&mut CharFmt)) {
        let mut next = self.group().chr.clone();
        change(&mut next);
        if next != self.group().chr {
            self.flush_run();
            self.group().chr = next;
        }
    }

    fn character(&mut self, c: char) {
        match self.effective_dest() {
            Dest::Skip => {}
            Dest::FieldInst => self.inst.push(c),
            Dest::FontTable => {
                if c == ';' {
                    self.finish_font();
                } else {
                    self.font_name.push(c);
                }
            }
            Dest::ColorTable => {
                if c == ';' {
                    let entry = if self.color_seen {
                        Some(format!("#{:02x}{:02x}{:02x}", self.color_rgb[0], self.color_rgb[1], self.color_rgb[2]))
                    } else {
                        None
                    };
                    self.tables.colors.push(entry);
                    self.color_rgb = [0; 3];
                    self.color_seen = false;
                }
            }
            Dest::StyleSheet => {
                if c == ';' {
                    self.finish_style();
                } else {
                    self.style_name.push(c);
                }
            }
            Dest::ListTable | Dest::ListOverrideTable => {}
            Dest::Text | Dest::Pending | Dest::PendingStar => {
                if self.skip_chars > 0 {
                    self.skip_chars -= 1;
                    return;
                }
                self.push_text(c);
            }
        }
    }

    fn font_table_word(&mut self, word: &str, param: Option<i32>) {
        if word == "f" {
            self.font_index = param.map(|p| p.max(0) as usize);
            self.font_name.clear();
        }
    }

    fn finish_font(&mut self) {
        if let Some(index) = self.font_index.take() {
            let name = self.font_name.trim().to_string();
            if !name.is_empty() {
                self.tables.fonts.insert(index, name);
            }
        }
        self.font_name.clear();
    }

    fn color_table_word(&mut self, word: &str, param: Option<i32>) {
        let v = param.unwrap_or(0).clamp(0, 255) as u8;
        match word {
            "red" => self.color_rgb[0] = v,
            "green" => self.color_rgb[1] = v,
            "blue" => self.color_rgb[2] = v,
            _ => return,
        }
        self.color_seen = true;
    }

    fn finish_style(&mut self) {
        if let Some(index) = self.style_index.take() {
            let name = self.style_name.trim().to_string();
            if !name.is_empty() {
                self.tables.styles.insert(index, name);
            }
        }
        self.style_name.clear();
    }

    fn list_table_word(&mut self, word: &str, param: Option<i32>) {
        match word {
            "list" => self.tables.lists.push(ListDef::default()),
            "listlevel" => {
                if let Some(list) = self.tables.lists.last_mut() {
                    list.ordered_levels.push(true);
                }
            }
            "levelnfc" | "levelnfcn" => {
                if let Some(level) = self.tables.lists.last_mut().and_then(|l| l.ordered_levels.last_mut()) {
                    // 23 is a bullet; every other numbering format counts as ordered.
                    *level = param != Some(23);
                }
            }
            "listid" => {
                if let Some(list) = self.tables.lists.last_mut() {
                    list.id = param;
                }
            }
            _ => {}
        }
    }

    /// Turn the pending text into a run carrying the formatting it was written under.
    fn flush_run(&mut self) {
        if self.text.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.text);
        let fmt = self.text_fmt.clone();
        let marks = self.marks_for(&fmt);
        if text.chars().any(|c| !c.is_whitespace()) {
            if !fmt.bold {
                self.all_bold = false;
            }
            if self.para_size.is_none() {
                self.para_size = fmt.size;
            }
        }
        self.runs.push(Run { text, marks });
    }

    fn marks_for(&self, fmt: &CharFmt) -> Vec<Mark> {
        let mut marks = Vec::new();
        let monospace = fmt
            .font
            .and_then(|f| self.tables.fonts.get(&f))
            .map(|name| {
                let n = name.to_ascii_lowercase();
                n.contains("courier") || n.contains("mono") || n.contains("menlo") || n.contains("consolas")
            })
            .unwrap_or(false);
        if monospace {
            // `code` excludes the other formatting marks in the schema.
            marks.push(Mark::Code);
        } else {
            if fmt.bold {
                marks.push(Mark::Bold);
            }
            if fmt.italic {
                marks.push(Mark::Italic);
            }
            if fmt.underline {
                marks.push(Mark::Underline);
            }
            if fmt.strike {
                marks.push(Mark::Strike);
            }
            if let Some(url) = &fmt.link {
                marks.push(Mark::Link { href: url.clone() });
            }
        }
        if fmt.superscript {
            marks.push(Mark::Superscript);
        }
        if fmt.subscript {
            marks.push(Mark::Subscript);
        }
        if let Some(color) = fmt.color.filter(|&i| i > 0).and_then(|i| self.tables.colors.get(i)).and_then(|c| c.clone()) {
            if color != "#000000" {
                marks.push(Mark::TextColor { color });
            }
        }
        if let Some(index) = fmt.highlight.filter(|&i| i > 0) {
            let color = self.tables.colors.get(index).and_then(|c| c.clone());
            marks.push(Mark::Highlight { color });
        }
        marks
    }

    fn finish_para(&mut self) {
        let runs = std::mem::take(&mut self.runs);
        let all_bold = self.all_bold && runs.iter().any(|r| r.text.chars().any(|c| !c.is_whitespace()));
        let size = self.para_size.take();
        self.all_bold = true;
        let fmt = self.stack.last().map(|g| g.para.clone()).unwrap_or_default();
        self.paras.push(Para { runs, fmt, all_bold, size });
    }
}

// ── Paragraphs to blocks ─────────────────────────────────────────────

/// Convert RTF bytes into the blocks a content document is built from.
pub fn rtf_to_blocks(rtf: &[u8]) -> Vec<Block> {
    let tokens = tokenize(rtf);
    let mut interp = Interp::new();
    interp.run(&tokens);
    let Interp { tables, paras, .. } = interp;
    paras_to_blocks(paras, &tables)
}

/// The plain text of the RTF: block texts joined by newlines.
pub fn rtf_to_text(rtf: &[u8]) -> String {
    rtf_to_blocks(rtf).iter().map(Block::plain_text).collect::<Vec<_>>().join("\n")
}

fn paras_to_blocks(mut paras: Vec<Para>, tables: &Tables) -> Vec<Block> {
    for para in &mut paras {
        for run in &mut para.runs {
            run.text = strip_scrivener_tags(&run.text);
        }
        para.runs.retain(|r| !r.text.is_empty());
        merge_adjacent_runs(&mut para.runs);
    }
    while paras.last().map_or(false, |p| p.runs.iter().all(|r| r.text.trim().is_empty())) {
        paras.pop();
    }

    let body_size = dominant_size(&paras);
    let mut out: Vec<Block> = Vec::new();
    let mut lists = ListStack::default();

    for para in paras {
        if let Some((ls, level)) = para.fmt.list {
            let ordered = tables
                .overrides
                .get(&ls)
                .and_then(|id| tables.lists.iter().find(|l| l.id == Some(*id)))
                .and_then(|l| l.ordered_levels.get(level.max(0) as usize).copied())
                .unwrap_or(false);
            let block = Block::Paragraph { runs: para.runs, align: para.fmt.align, indent: 0 };
            lists.push_item(&mut out, ls, level.max(0), ordered, block);
            continue;
        }
        lists.flush(&mut out);
        out.push(plain_block(para, tables, body_size));
    }
    lists.flush(&mut out);
    out
}

/// A non-list paragraph: a heading when its style says so or it is bold and larger
/// than the body text; otherwise a paragraph with its alignment and indent.
fn plain_block(para: Para, tables: &Tables, body_size: Option<i32>) -> Block {
    let styled_level = para
        .fmt
        .style
        .and_then(|s| tables.styles.get(&s))
        .and_then(|name| heading_level_from_style(name));
    let size_level = match (para.size, body_size) {
        (Some(size), Some(body)) if para.all_bold && size > body => {
            let ratio = size as f32 / body as f32;
            Some(if ratio >= 1.5 { 1 } else if ratio >= 1.25 { 2 } else { 3 })
        }
        _ => None,
    };
    if let Some(level) = styled_level.or(size_level) {
        return Block::Heading { level, runs: para.runs };
    }
    let indent = if para.fmt.left_indent >= 360 { (para.fmt.left_indent as f32 / 720.0).round() as u32 } else { 0 };
    Block::Paragraph { runs: para.runs, align: para.fmt.align, indent }
}

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

/// The most common font size across the document's text, weighted by length.
fn dominant_size(paras: &[Para]) -> Option<i32> {
    let mut weights: HashMap<i32, usize> = HashMap::new();
    for para in paras {
        if let Some(size) = para.size {
            let len: usize = para.runs.iter().map(|r| r.text.len()).sum();
            *weights.entry(size).or_insert(0) += len;
        }
    }
    weights.into_iter().max_by_key(|(size, w)| (*w, -*size)).map(|(size, _)| size)
}

fn merge_adjacent_runs(runs: &mut Vec<Run>) {
    let mut merged: Vec<Run> = Vec::with_capacity(runs.len());
    for run in runs.drain(..) {
        if let Some(last) = merged.last_mut() {
            if last.marks == run.marks {
                last.text.push_str(&run.text);
                continue;
            }
        }
        merged.push(run);
    }
    *runs = merged;
}

/// Builds nested lists from a run of list paragraphs: one open list per level, the
/// innermost last. A deeper level nests under the last item of the level above; a
/// shallower level closes the deeper lists into their parents first.
#[derive(Default)]
struct ListStack {
    open: Vec<OpenList>,
}

struct OpenList {
    ls: i32,
    level: i32,
    ordered: bool,
    items: Vec<ListItem>,
}

impl OpenList {
    fn into_block(self) -> Block {
        if self.ordered {
            Block::OrderedList { start: 1, items: self.items }
        } else {
            Block::BulletList { items: self.items }
        }
    }
}

impl ListStack {
    fn push_item(&mut self, out: &mut Vec<Block>, ls: i32, level: i32, ordered: bool, block: Block) {
        // Close lists deeper than this level, and a same-level list of another kind
        // or another list id (a new list started right after the previous one).
        while let Some(top) = self.open.last() {
            let same = top.level == level && top.ls == ls && top.ordered == ordered;
            if top.level > level || (top.level == level && !same) {
                self.close_top(out);
            } else {
                break;
            }
        }
        if self.open.last().map_or(true, |top| top.level < level) {
            self.open.push(OpenList { ls, level, ordered, items: Vec::new() });
        }
        self.open.last_mut().expect("just ensured").items.push(ListItem { blocks: vec![block] });
    }

    fn close_top(&mut self, out: &mut Vec<Block>) {
        let Some(list) = self.open.pop() else { return };
        let block = list.into_block();
        match self.open.last_mut() {
            Some(parent) => {
                if parent.items.is_empty() {
                    parent.items.push(ListItem { blocks: Vec::new() });
                }
                parent.items.last_mut().expect("ensured").blocks.push(block);
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

/// Extract the URL from a field instruction like `HYPERLINK "https://example.com"`.
fn parse_hyperlink_url(inst: &str) -> Option<String> {
    let pos = inst.find("HYPERLINK")?;
    let text = inst[pos + "HYPERLINK".len()..].trim();
    if let Some(rest) = text.strip_prefix('"') {
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    } else {
        text.split_whitespace().next().map(|s| s.trim_end_matches('}').to_string())
    }
}

/// Strip Scrivener's inline placeholder tags, opening (`<$Scr_Ps::0>`) and closing
/// (`<!$Scr_Ps::0>`) alike.
fn strip_scrivener_tags(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let open = rest.find("<$Scr_");
        let close = rest.find("<!$Scr_");
        let start = match (open, close) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => break,
        };
        result.push_str(&rest[..start]);
        match rest[start..].find('>') {
            Some(end) => rest = &rest[start + end + 1..],
            None => {
                rest = &rest[start..];
                break;
            }
        }
    }
    result.push_str(rest);
    result
}

/// Decode a Windows-1252 byte to a Unicode char.
fn decode_windows_1252(byte: u8) -> char {
    match byte {
        0x80 => '\u{20AC}',
        0x82 => '\u{201A}',
        0x83 => '\u{0192}',
        0x84 => '\u{201E}',
        0x85 => '\u{2026}',
        0x86 => '\u{2020}',
        0x87 => '\u{2021}',
        0x88 => '\u{02C6}',
        0x89 => '\u{2030}',
        0x8A => '\u{0160}',
        0x8B => '\u{2039}',
        0x8C => '\u{0152}',
        0x8E => '\u{017D}',
        0x91 => '\u{2018}',
        0x92 => '\u{2019}',
        0x93 => '\u{201C}',
        0x94 => '\u{201D}',
        0x95 => '\u{2022}',
        0x96 => '\u{2013}',
        0x97 => '\u{2014}',
        0x98 => '\u{02DC}',
        0x99 => '\u{2122}',
        0x9A => '\u{0161}',
        0x9B => '\u{203A}',
        0x9C => '\u{0153}',
        0x9E => '\u{017E}',
        0x9F => '\u{0178}',
        b => b as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs_of(block: &Block) -> &[Run] {
        match block {
            Block::Paragraph { runs, .. } | Block::Heading { runs, .. } => runs,
            other => panic!("expected a text block, got {other:?}"),
        }
    }

    #[test]
    fn simple_paragraphs() {
        let rtf = br#"{\rtf1\ansi\deff0
{\fonttbl{\f0\fnil Calibri;}}
{\colortbl;\red0\green0\blue0;}
\pard Hello World\par Second line}"#;
        let blocks = rtf_to_blocks(rtf);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].plain_text(), "Hello World");
        assert_eq!(blocks[1].plain_text(), "Second line");
    }

    #[test]
    fn bold_italic_underline_strike_runs() {
        let rtf = br"{\rtf1 Normal {\b bold} and {\i italic} and \ul under\ulnone  and {\strike gone} text}";
        let blocks = rtf_to_blocks(rtf);
        let runs = runs_of(&blocks[0]);
        let find = |t: &str| runs.iter().find(|r| r.text.trim() == t).unwrap_or_else(|| panic!("no run {t:?} in {runs:?}"));
        assert_eq!(find("bold").marks, vec![Mark::Bold]);
        assert_eq!(find("italic").marks, vec![Mark::Italic]);
        assert_eq!(find("under").marks, vec![Mark::Underline]);
        assert_eq!(find("gone").marks, vec![Mark::Strike]);
        assert_eq!(blocks[0].plain_text(), "Normal bold and italic and under and gone text");
    }

    /// A run that starts with `\tab` (or any control-word character) carries the
    /// formatting in force at that point, not the previous run's.
    #[test]
    fn a_run_starting_with_a_tab_keeps_its_own_formatting() {
        let rtf = br"{\rtf1{\colortbl;\red0\green0\blue0;\red255\green0\blue0;}\pard\plain {\cf2 first}\par\plain {\strike\strikec0\cf2 \tab struck}\par}";
        let blocks = rtf_to_blocks(rtf);
        let run = &runs_of(&blocks[1])[0];
        assert_eq!(run.text, "\tstruck");
        assert_eq!(run.marks, vec![Mark::Strike, Mark::TextColor { color: "#ff0000".into() }]);
    }

    #[test]
    fn scrivener_style_bold_toggles_with_b1_and_b0() {
        let rtf = br"{\rtf1 \pard\plain {\f1\fs24\b1\i0 To Do}\par\plain {\f0\fs24\b0\i0 item}\par}";
        let blocks = rtf_to_blocks(rtf);
        assert_eq!(runs_of(&blocks[0])[0].marks, vec![Mark::Bold]);
        assert!(runs_of(&blocks[1])[0].marks.is_empty());
    }

    #[test]
    fn hyperlink_field() {
        let rtf = br#"{\rtf1 Click {\field{\*\fldinst HYPERLINK "https://example.com"}{\fldrslt here}} now}"#;
        let blocks = rtf_to_blocks(rtf);
        let runs = runs_of(&blocks[0]);
        let link = runs.iter().find(|r| r.text.trim() == "here").expect("link text run");
        assert_eq!(link.marks, vec![Mark::Link { href: "https://example.com".into() }]);
        assert_eq!(blocks[0].plain_text(), "Click here now");
    }

    #[test]
    fn scrivener_tags_are_stripped_including_closing_ones() {
        let rtf = br"{\rtf1 <$Scr_Ps::0>Hello World<!$Scr_Ps::0> and <$Scr_H::4>more<!$Scr_H::4>}";
        assert_eq!(rtf_to_text(rtf), "Hello World and more");
    }

    #[test]
    fn unicode_escapes_skip_their_fallback() {
        let rtf = br"{\rtf1\ansi\uc1 it\u8217\'92s \u9679\'3F done \'e9}";
        assert_eq!(rtf_to_text(rtf), "it\u{2019}s \u{25CF} done \u{e9}");
    }

    #[test]
    fn colours_become_text_colour_marks_except_black() {
        let rtf = br"{\rtf1{\colortbl;\red0\green0\blue0;\red255\green255\blue255;\red251\green4\blue7;}\pard {\cf1 black} {\cf3 red} {\cf0 auto}\par}";
        let runs = runs_of(&rtf_to_blocks(rtf)[0]).to_vec();
        assert!(runs.iter().find(|r| r.text.trim() == "black").unwrap().marks.is_empty());
        assert_eq!(
            runs.iter().find(|r| r.text.trim() == "red").unwrap().marks,
            vec![Mark::TextColor { color: "#fb0407".into() }]
        );
        assert!(runs.iter().find(|r| r.text.trim() == "auto").unwrap().marks.is_empty());
    }

    #[test]
    fn highlight_and_super_sub() {
        let rtf = br"{\rtf1{\colortbl;\red0\green0\blue0;\red255\green255\blue0;}\pard {\highlight2 hi} x{\super 2} H{\sub 2}O\par}";
        let runs = runs_of(&rtf_to_blocks(rtf)[0]).to_vec();
        assert_eq!(runs.iter().find(|r| r.text.trim() == "hi").unwrap().marks, vec![Mark::Highlight { color: Some("#ffff00".into()) }]);
        assert_eq!(runs.iter().find(|r| r.text == "2" && r.marks.contains(&Mark::Superscript)).map(|r| r.marks.clone()), Some(vec![Mark::Superscript]));
        assert!(runs.iter().any(|r| r.text == "2" && r.marks == vec![Mark::Subscript]));
    }

    #[test]
    fn scrivener_bullet_list_groups_into_a_nested_list() {
        let rtf = br"{\rtf1\ansi\uc1
{\*\listtable
{\list\listtemplateid1{\listlevel\levelnfc23\levelnfcn23{\leveltext\'01\u9679\'3F;}{\levelnumbers;}\fi-360\li720}{\listlevel\levelnfc0\levelnfcn0{\leveltext\'02\'01.;}{\levelnumbers\'01;}\fi-360\li1440}{\listname ;}\listid7}}
{\*\listoverridetable{\listoverride\listid7\listoverridecount0\ls1}}
\pard\plain Before\par
\pard\plain\li720\fi-720 \ls1\ilvl0{\listtext\f0\fs24	\u9679\'3F	}{\f0 one}
\par\pard\plain\li720\fi-720 \ls1\ilvl0{\listtext	\u9679\'3F	}{\f0 two}
\par\pard\plain\li1440\fi-720 \ls1\ilvl1{\listtext	1.	}{\f0 two a}
\par\pard\plain\li1440\fi-720 \ls1\ilvl1{\listtext	2.	}{\f0 two b}
\par\pard\plain\li720\fi-720 \ls1\ilvl0{\listtext	\u9679\'3F	}{\f0 three}
\par\pard\plain After\par}";
        let blocks = rtf_to_blocks(rtf);
        assert_eq!(blocks.len(), 3, "{blocks:#?}");
        assert_eq!(blocks[0].plain_text(), "Before");
        let Block::BulletList { items } = &blocks[1] else { panic!("expected a bullet list, got {:?}", blocks[1]) };
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].blocks, vec![Block::plain("one")]);
        assert_eq!(items[1].blocks.len(), 2, "two carries the nested list: {:?}", items[1]);
        assert_eq!(items[1].blocks[0], Block::plain("two"));
        let Block::OrderedList { items: nested, .. } = &items[1].blocks[1] else { panic!("expected an ordered nested list") };
        assert_eq!(nested.iter().map(|i| i.blocks[0].plain_text()).collect::<Vec<_>>(), vec!["two a", "two b"]);
        assert_eq!(items[2].blocks, vec![Block::plain("three")]);
        assert_eq!(blocks[2].plain_text(), "After");
    }

    #[test]
    fn stylesheet_headings_and_bold_larger_paragraphs() {
        let rtf = br"{\rtf1{\stylesheet{\s0 Normal;}{\s2 Heading 2;}}
\pard\s2\fs24 Styled heading\par
\pard\plain\fs24 body text body text\par
\pard\plain {\fs36\b1 Big bold}\par
\pard\plain {\fs24\b1 Bold body}\par}";
        let blocks = rtf_to_blocks(rtf);
        assert!(matches!(&blocks[0], Block::Heading { level: 2, .. }), "{:?}", blocks[0]);
        assert!(matches!(&blocks[1], Block::Paragraph { .. }));
        assert!(matches!(&blocks[2], Block::Heading { level: 1, .. }), "{:?}", blocks[2]);
        assert!(matches!(&blocks[3], Block::Paragraph { .. }), "bold at body size stays a paragraph: {:?}", blocks[3]);
        assert_eq!(runs_of(&blocks[3])[0].marks, vec![Mark::Bold]);
    }

    #[test]
    fn alignment_and_indent() {
        let rtf = br"{\rtf1 \pard\qc centred\par\pard\li720 indented\par\pard plain\par}";
        let blocks = rtf_to_blocks(rtf);
        assert!(matches!(&blocks[0], Block::Paragraph { align: Align::Center, indent: 0, .. }), "{:?}", blocks[0]);
        assert!(matches!(&blocks[1], Block::Paragraph { align: Align::Left, indent: 1, .. }), "{:?}", blocks[1]);
        assert!(matches!(&blocks[2], Block::Paragraph { align: Align::Left, indent: 0, .. }));
    }

    #[test]
    fn tables_flatten_to_tab_separated_rows_and_pictures_are_skipped() {
        let rtf = br"{\rtf1 \trowd\cellx100\cellx200 \pard\intbl a\cell \pard\intbl b\cell \row \pard {\*\shppict{\pict\pngblip 89504e470d}} after\par}";
        let blocks = rtf_to_blocks(rtf);
        assert_eq!(blocks[0].plain_text(), "a\tb\t");
        assert_eq!(blocks[1].plain_text(), " after");
        assert!(!rtf_to_text(rtf).contains("89504e"));
    }

    #[test]
    fn monospace_font_becomes_code() {
        let rtf = br"{\rtf1{\fonttbl{\f0\fnil Calibri;}{\f1\fmodern Courier New;}}\pard {\f0 text }{\f1\b code}\par}";
        let runs = runs_of(&rtf_to_blocks(rtf)[0]).to_vec();
        assert_eq!(runs.iter().find(|r| r.text.trim() == "code").unwrap().marks, vec![Mark::Code]);
    }
}
