//! Subtitles and pictures in a note's text (docs/THEMES_CONTRACT.md
//! "Subtitles and pictures").
//!
//! A subtitle is a paragraph with the role [`pimble_crdt::text_roles::SUBTITLE`]
//! (`data-role`, which rinch keeps on the block): the toolbar's Subtitle
//! button gives and takes it, and Enter at its end starts an ordinary
//! paragraph. Nothing else about a note says how it looks; the theme does.
//!
//! [`ParagraphRolesPlugin`] also tells the theme's stylesheet what it cannot
//! select for itself, as classes on blocks (node decorations, which are no
//! part of the document): a paragraph holding only pictures
//! ([`PICTURE_BLOCK`]), the heading or subtitle a subtitle follows
//! ([`BEFORE_SUBTITLE`]), and for a subtitle under a heading, that heading's
//! level ([`UNDER`]) and the alignment someone chose for it ([`title_class`]).

use std::rc::Rc;

use pimble_crdt::text_roles::{is_subtitle, ROLE_ATTR, SUBTITLE};
use rinch_editor_core::state::{EditorState, Transaction};
use rinch_editor_core::{AttrValue, Attrs, DataAttrSplit, Decoration, DecorationSet, Node as EditorNode, Plugin, PluginKey, Pos};

use crate::rinch_editor::EditorHandle;

const PLUGIN_KEY: PluginKey = PluginKey("pimble.paragraph_roles");

/// The class of a paragraph that holds pictures and nothing to read.
pub const PICTURE_BLOCK: &str = "pimble-picture-block";
/// The class of a heading or subtitle with a subtitle right after it.
pub const BEFORE_SUBTITLE: &str = "pimble-before-subtitle";

/// The classes of a subtitle under a heading of level 1 to 6, whether it
/// follows the heading or another subtitle that does.
pub(crate) const UNDER: [&str; 6] =
    ["pimble-under-h1", "pimble-under-h2", "pimble-under-h3", "pimble-under-h4", "pimble-under-h5", "pimble-under-h6"];

/// The class of a subtitle under a heading whose alignment someone chose:
/// the subtitle is aligned the same.
pub(crate) fn title_class(align: &str) -> Option<&'static str> {
    Some(match align {
        "left" => "pimble-title-left",
        "center" => "pimble-title-center",
        "right" => "pimble-title-right",
        "justify" => "pimble-title-justify",
        _ => return None,
    })
}

/// Whether `node` is a paragraph of one picture or more, and at most blank
/// space between them.
pub(crate) fn is_picture_block(node: &EditorNode) -> bool {
    if node.type_name() != "paragraph" {
        return false;
    }
    let mut pictures = false;
    for child in node.content().iter() {
        match child.text() {
            Some(text) if text.chars().all(char::is_whitespace) => {}
            None if child.type_name() == "image" => pictures = true,
            _ => return false,
        }
    }
    pictures
}

/// The classes of each of `blocks`, siblings in their order: empty for a
/// block the stylesheet needs none on. One string a block, since a block
/// takes its classes from one decoration. The theme's preview draws the same.
pub(crate) fn classes(blocks: &[EditorNode]) -> Vec<String> {
    // The classes of the heading the current run of subtitles is under: its
    // level's, and its chosen alignment's.
    let mut title: [Option<&'static str>; 2] = [None, None];
    blocks
        .iter()
        .enumerate()
        .map(|(i, block)| {
            let heading = block.type_name() == "heading";
            let subtitle = is_subtitle(block);
            let mut classes = Vec::new();
            if heading {
                let level = block.attrs().get_int("level").unwrap_or(1).clamp(1, 6) as usize;
                title = [Some(UNDER[level - 1]), block.attrs().get_str("text_align").and_then(title_class)];
            } else if subtitle {
                classes.extend(title.into_iter().flatten());
            } else {
                title = [None, None];
            }
            if (heading || subtitle) && blocks.get(i + 1).is_some_and(is_subtitle) {
                classes.push(BEFORE_SUBTITLE);
            }
            if is_picture_block(block) {
                classes.push(PICTURE_BLOCK);
            }
            classes.join(" ")
        })
        .collect()
}

/// The blocks of a document that take classes, as `(from, to, classes)`
/// around each.
#[derive(Debug, Clone, Default)]
struct Marked(Vec<(usize, usize, String)>);

impl Marked {
    fn of(doc: &EditorNode) -> Marked {
        let mut marked = Marked::default();
        marked.children(doc, 0);
        marked
    }

    /// The blocks in `parent`, whose content starts at `start`.
    fn children(&mut self, parent: &EditorNode, start: usize) {
        let blocks = parent.content().children();
        let mut at = start;
        for (block, classes) in blocks.iter().zip(classes(blocks)) {
            let end = at + block.node_size();
            if !classes.is_empty() {
                self.0.push((at, end, classes));
            }
            if !block.is_textblock() && !block.is_leaf() {
                self.children(block, at + 1);
            }
            at = end;
        }
    }
}

/// Subtitles split as a title does, and the classes the theme's stylesheet
/// reads.
pub struct ParagraphRolesPlugin;

impl Plugin for ParagraphRolesPlugin {
    fn key(&self) -> PluginKey {
        PLUGIN_KEY
    }

    fn data_attr_splits(&self) -> Vec<(String, DataAttrSplit)> {
        // Enter at a subtitle's end starts an ordinary paragraph.
        vec![(ROLE_ATTR.to_string(), DataAttrSplit::CopyToText)]
    }

    fn init_state(&self, doc: &EditorNode) -> Option<Rc<dyn std::any::Any>> {
        Some(Rc::new(Marked::of(doc)))
    }

    fn apply(&self, tr: &Transaction, _old: &EditorState, new_doc: &EditorNode, prev: Option<&dyn std::any::Any>) -> Option<Rc<dyn std::any::Any>> {
        match prev.and_then(|p| p.downcast_ref::<Marked>()) {
            Some(prev) if !tr.doc_changed() => Some(Rc::new(prev.clone())),
            _ => Some(Rc::new(Marked::of(new_doc))),
        }
    }

    fn decorations(&self, state: &EditorState) -> DecorationSet {
        let Some(marked) = state.plugin_state::<Marked>(PLUGIN_KEY) else {
            return DecorationSet::empty();
        };
        if marked.0.is_empty() {
            return DecorationSet::empty();
        }
        DecorationSet::new(marked.0.iter().map(|(from, to, class)| Decoration::node(Pos(*from), Pos(*to), class.clone())).collect())
    }
}

/// Whether every textblock at the selection is a subtitle: the toolbar's
/// button reads as pressed.
pub(crate) fn subtitle_active(handle: &EditorHandle) -> bool {
    let state = handle.state();
    let mut found = None;
    for (from, to) in state.selection.ranges(&state.doc) {
        state.doc.nodes_between(from.0, to.0, &mut |node, _, _| {
            if node.is_textblock() {
                found = Some(found.unwrap_or(true) && is_subtitle(node));
            }
            true
        });
    }
    found == Some(true)
}

/// The toolbar's Subtitle button: makes the blocks at the selection
/// subtitles (a heading or code block becomes a paragraph first, keeping its
/// alignment and indent), or, when they all are, ordinary paragraphs again.
/// One transaction, so one undo.
pub(crate) fn toggle_subtitle(handle: &EditorHandle) {
    if subtitle_active(handle) {
        handle.set_block_data_attr(ROLE_ATTR, None);
        return;
    }
    handle.update(|state| {
        let paragraph = state.schema().node_type("paragraph")?.clone();
        let ranges = state.selection.ranges(&state.doc);
        // The textblocks that are no paragraphs, each by a position inside it.
        let mut others: Vec<(usize, Vec<(&str, AttrValue)>)> = Vec::new();
        for (from, to) in &ranges {
            state.doc.nodes_between(from.0, to.0, &mut |node, pos, _| {
                if node.is_textblock() && node.type_name() != "paragraph" {
                    let mut kept = Vec::new();
                    if let Some(align) = node.attrs().get_str("text_align") {
                        kept.push(("text_align", AttrValue::from(align)));
                    }
                    if let Some(indent) = node.attrs().get_int("indent") {
                        kept.push(("indent", AttrValue::Int(indent)));
                    }
                    others.push((pos + 1, kept));
                }
                true
            });
        }
        let mut tr = state.tr();
        // A block keeps its size when its type changes, so the positions hold.
        for (at, kept) in others {
            tr.set_block_type(at, at, paragraph.clone(), Attrs::from_iter(kept)).ok()?;
        }
        for (from, to) in ranges {
            tr.set_block_data_attr(from.0, to.0, ROLE_ATTR, Some(SUBTITLE)).ok()?;
        }
        tr.doc_changed().then_some(tr)
    });
}

#[cfg(test)]
mod tests;
