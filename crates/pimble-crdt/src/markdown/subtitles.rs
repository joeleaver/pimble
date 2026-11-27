//! Subtitles in Markdown. Markdown has no syntax for a paragraph's role and
//! rinch writes none, so a subtitle is its paragraph with [`SUBTITLE_MARK`] at
//! the end: `*by Ann* {.subtitle}`. The same words meant as text are written
//! `\\{.subtitle}`.
//!
//! Both directions pass the mark through rinch's reader and writer as a
//! private-use character, so an escaped mark, and one inside code, stay the
//! words they are.

use rinch_editor_core::serialize::markdown::doc_to_markdown;
use rinch_editor_core::{AttrValue, Fragment, Node, Schema};

use super::collab;
use crate::error::Result;
use crate::text_roles::{is_subtitle, ROLE_ATTR, SUBTITLE};

/// What follows a subtitle's text in Markdown.
pub const SUBTITLE_MARK: &str = "{.subtitle}";

/// Stands for a mark that gives its paragraph the role.
const ROLE: &str = "\u{E0A0}";
/// Stands for the mark's words ending a paragraph that is no subtitle.
const WORDS: &str = "\u{E0A1}";

/// `md` with [`ROLE`] in place of each unescaped mark that ends a line or a
/// table cell.
pub(super) fn mark(md: &str) -> String {
    md.split_inclusive('\n')
        .map(|line| {
            let mut out = String::with_capacity(line.len());
            let mut copied = 0;
            for (at, found) in line.match_indices(SUBTITLE_MARK) {
                let after = line[at + found.len()..].trim_start();
                let ends = after.is_empty() || after.starts_with('|');
                if ends && !line[..at].ends_with('\\') {
                    out.push_str(&line[copied..at]);
                    out.push_str(ROLE);
                    copied = at + found.len();
                }
            }
            out.push_str(&line[copied..]);
            out
        })
        .collect()
}

/// `node` as parsed from marked Markdown: a paragraph ending in [`ROLE`] has
/// the role and loses it and the space before it; a [`ROLE`] anywhere else
/// (in code, before a line break inside a paragraph) is the words again.
pub(super) fn read(schema: &Schema, node: &Node) -> Result<Node> {
    if let Some(text) = node.text() {
        return match text.contains(ROLE) {
            true => schema
                .text_with_marks(&text.replace(ROLE, SUBTITLE_MARK), node.marks().to_vec())
                .map_err(collab),
            false => Ok(node.clone()),
        };
    }
    let mut children: Vec<Node> = node.content().iter().cloned().collect();
    let mut role = false;
    if node.type_name() == "paragraph" {
        if let Some((before, marks)) = children.last().and_then(|last| {
            Some((
                last.text()?.strip_suffix(ROLE)?.trim_end().to_string(),
                last.marks().to_vec(),
            ))
        }) {
            role = true;
            children.pop();
            if !before.is_empty() {
                children.push(schema.text_with_marks(&before, marks).map_err(collab)?);
            }
        }
    }
    let children = Fragment::from_children(
        children
            .iter()
            .map(|child| read(schema, child))
            .collect::<Result<Vec<_>>>()?,
    );
    if !role {
        return Ok(node.copy_with_content(children));
    }
    let attrs = node.attrs().with(ROLE_ATTR, AttrValue::from(SUBTITLE));
    schema
        .create_node("paragraph", attrs, children)
        .map_err(collab)
}

/// `doc` as Markdown with its subtitles marked.
pub(super) fn write(doc: &Node) -> String {
    match prepared(&Schema::starter_kit(), doc) {
        Some(doc) => doc_to_markdown(&doc)
            .replace(ROLE, SUBTITLE_MARK)
            .replace(WORDS, &format!("\\{SUBTITLE_MARK}")),
        None => doc_to_markdown(doc),
    }
}

/// `node` with a stand-in appended to each subtitle, and one in place of the
/// mark's words ending any other paragraph; `None` when it has neither.
fn prepared(schema: &Schema, node: &Node) -> Option<Node> {
    if node.type_name() == "paragraph" {
        let mut children: Vec<Node> = node.content().iter().cloned().collect();
        let words = children.last().and_then(|last| {
            // In inline code the words are code, which is read as written.
            if last.marks().iter().any(|m| m.type_name() == "code") {
                return None;
            }
            let before = last.text()?.strip_suffix(SUBTITLE_MARK)?;
            schema
                .text_with_marks(&format!("{before}{WORDS}"), last.marks().to_vec())
                .ok()
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
            let stand_in = if children.is_empty() {
                ROLE.to_string()
            } else {
                format!(" {ROLE}")
            };
            children.push(schema.text(&stand_in).ok()?);
        }
        return Some(node.copy_with_content(Fragment::from_children(children)));
    }
    if node.is_textblock() {
        return None;
    }
    let mut changed = false;
    let children: Vec<Node> = node
        .content()
        .iter()
        .map(|child| match prepared(schema, child) {
            Some(new) => {
                changed = true;
                new
            }
            None => child.clone(),
        })
        .collect();
    changed.then(|| node.copy_with_content(Fragment::from_children(children)))
}

#[cfg(test)]
mod tests;
