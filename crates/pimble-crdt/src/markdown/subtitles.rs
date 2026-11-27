//! Subtitles in Markdown. Markdown has no syntax for a paragraph's role and
//! rinch writes none, so a subtitle is its paragraph with [`SUBTITLE_MARK`] at
//! the end: `*by Ann* {.subtitle}`. The same words meant as text are written
//! `\\{.subtitle}`.
//!
//! Both directions pass the mark through rinch's reader and writer as a
//! stand-in character the text does not hold, so an escaped mark, and one
//! inside code, stay the words they are.
//!
//! A table rinch writes as HTML needs none of this: its paragraphs carry
//! `data-role` themselves, and its text is read as it is written.

use rinch_editor_core::serialize::markdown::doc_to_markdown;
use rinch_editor_core::{AttrValue, Fragment, Node, Schema};

use super::collab;
use crate::error::Result;
use crate::text_roles::{is_subtitle, ROLE_ATTR, SUBTITLE};

/// What follows a subtitle's text in Markdown.
pub const SUBTITLE_MARK: &str = "{.subtitle}";

/// The characters standing for the mark while rinch reads or writes: two of
/// the last private-use plane that the text at hand does not hold.
#[derive(Debug, Clone, Copy)]
pub(super) struct StandIns {
    /// Stands for a mark that gives its paragraph the role.
    role: char,
    /// Stands for the mark's words ending a paragraph that is no subtitle.
    words: char,
}

impl StandIns {
    pub(super) fn absent_from(text: &str) -> StandIns {
        let mut free = ('\u{10F000}'..='\u{10FFFD}').filter(|c| !text.contains(*c));
        // A text holding all four thousand of them is nobody's note; the
        // plane's ends then stand in.
        let role = free.next().unwrap_or('\u{10F000}');
        let words = free.next().unwrap_or('\u{10FFFD}');
        StandIns { role, words }
    }

    /// `md` with the role's stand-in in place of each unescaped mark that ends
    /// a line or a table cell, outside HTML tables.
    pub(super) fn mark(self, md: &str) -> String {
        let mut html = false;
        md.split_inclusive('\n')
            .map(|line| {
                html |= line.trim_start().starts_with("<table");
                if html {
                    html = !line.contains("</table>");
                    return line.to_string();
                }
                let mut out = String::with_capacity(line.len());
                let mut copied = 0;
                for (at, found) in line.match_indices(SUBTITLE_MARK) {
                    let after = line[at + found.len()..].trim_start();
                    let ends = after.is_empty() || after.starts_with('|');
                    if ends && !line[..at].ends_with('\\') {
                        out.push_str(&line[copied..at]);
                        out.push(self.role);
                        copied = at + found.len();
                    }
                }
                out.push_str(&line[copied..]);
                out
            })
            .collect()
    }

    /// `node` as parsed from marked Markdown: a paragraph ending in the role's
    /// stand-in has the role and loses it and the space before it; a stand-in
    /// anywhere else (in code, before a line break inside a paragraph) is the
    /// words again.
    pub(super) fn read(self, schema: &Schema, node: &Node) -> Result<Node> {
        if let Some(text) = node.text() {
            return match text.contains(self.role) {
                true => schema.text_with_marks(&text.replace(self.role, SUBTITLE_MARK), node.marks().to_vec()).map_err(collab),
                false => Ok(node.clone()),
            };
        }
        let mut children: Vec<Node> = node.content().iter().cloned().collect();
        let mut role = false;
        if node.type_name() == "paragraph" {
            if let Some((before, marks)) =
                children.last().and_then(|last| Some((last.text()?.strip_suffix(self.role)?.trim_end().to_string(), last.marks().to_vec())))
            {
                role = true;
                children.pop();
                if !before.is_empty() {
                    children.push(schema.text_with_marks(&before, marks).map_err(collab)?);
                }
            }
        }
        let children = Fragment::from_children(children.iter().map(|child| self.read(schema, child)).collect::<Result<Vec<_>>>()?);
        if !role {
            return Ok(node.copy_with_content(children));
        }
        let attrs = node.attrs().with(ROLE_ATTR, AttrValue::from(SUBTITLE));
        schema.create_node("paragraph", attrs, children).map_err(collab)
    }

    /// `node` with the role's stand-in appended to each subtitle, and the
    /// words' in place of the mark's words ending any other paragraph; `None`
    /// when it has neither.
    fn prepared(self, schema: &Schema, root: &Node, node: &Node) -> Option<Node> {
        if node.type_name() == "paragraph" {
            let mut children: Vec<Node> = node.content().iter().cloned().collect();
            let words = children.last().and_then(|last| {
                // In inline code the words are code, which is read as written.
                if last.marks().iter().any(|m| m.type_name() == "code") {
                    return None;
                }
                // Blank space after them does not stop them ending the line.
                let text = last.text()?;
                let kept = text.trim_end();
                let before = kept.strip_suffix(SUBTITLE_MARK)?;
                let space = &text[kept.len()..];
                schema.text_with_marks(&format!("{before}{}{space}", self.words), last.marks().to_vec()).ok()
            });
            let subtitle = is_subtitle(node);
            if words.is_none() && !subtitle {
                return None;
            }
            if let Some(words) = words {
                *children.last_mut().expect("the words are its last run") = words;
            }
            if subtitle {
                // A run of its own, with no marks, its space inside it so the
                // writer closes the marks before it: `**by** {.subtitle}`.
                let space = if children.is_empty() { "" } else { " " };
                children.push(schema.text(&format!("{space}{}", self.role)).ok()?);
            }
            return Some(node.copy_with_content(Fragment::from_children(children)));
        }
        if node.is_textblock() || (node.type_name() == "table" && written_as_html(root, node)) {
            return None;
        }
        let mut changed = false;
        let children: Vec<Node> = node
            .content()
            .iter()
            .map(|child| match self.prepared(schema, root, child) {
                Some(new) => {
                    changed = true;
                    new
                }
                None => child.clone(),
            })
            .collect();
        changed.then(|| node.copy_with_content(Fragment::from_children(children)))
    }
}

/// Whether rinch writes `table` as an HTML `<table>` and not a pipe table.
fn written_as_html(root: &Node, table: &Node) -> bool {
    let alone = root.copy_with_content(Fragment::from_children(vec![table.clone()]));
    doc_to_markdown(&alone).trim_start().starts_with("<table")
}

/// `doc` as Markdown with its subtitles marked.
pub(super) fn write(doc: &Node) -> String {
    let plain = doc_to_markdown(doc);
    let stand_ins = StandIns::absent_from(&plain);
    match stand_ins.prepared(&Schema::starter_kit(), doc, doc) {
        Some(doc) => doc_to_markdown(&doc).replace(stand_ins.role, SUBTITLE_MARK).replace(stand_ins.words, &format!("\\{SUBTITLE_MARK}")),
        None => plain,
    }
}

#[cfg(test)]
mod tests;
