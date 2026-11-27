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
//! ([`BEFORE_SUBTITLE`]), and the alignment someone chose for the heading a
//! subtitle is under ([`title_class`]).

use std::rc::Rc;

use pimble_crdt::text_roles::{is_subtitle, ROLE_ATTR, SUBTITLE};
use rinch_editor_core::state::{EditorState, Transaction};
use rinch_editor_core::{DataAttrSplit, Decoration, DecorationSet, Node as EditorNode, Plugin, PluginKey, Pos};

use crate::rinch_editor::EditorHandle;

const PLUGIN_KEY: PluginKey = PluginKey("pimble.paragraph_roles");

/// The class of a paragraph that holds pictures and nothing to read.
pub const PICTURE_BLOCK: &str = "pimble-picture-block";
/// The class of a heading or subtitle with a subtitle right after it.
pub const BEFORE_SUBTITLE: &str = "pimble-before-subtitle";

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
    // The alignment chosen for the heading the current run of subtitles is
    // under.
    let mut title: Option<&'static str> = None;
    blocks
        .iter()
        .enumerate()
        .map(|(i, block)| {
            let heading = block.type_name() == "heading";
            let subtitle = is_subtitle(block);
            let mut classes = Vec::new();
            if heading {
                title = block.attrs().get_str("text_align").and_then(title_class);
            } else if subtitle {
                classes.extend(title);
            } else {
                title = None;
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
    handle.current_block_type().as_deref() == Some("paragraph") && handle.block_data_attr(ROLE_ATTR).as_deref() == Some(SUBTITLE)
}

/// The toolbar's Subtitle button: makes the blocks at the selection
/// subtitles (a heading or code block becomes a paragraph first), or, on a
/// subtitle, ordinary paragraphs again.
pub(crate) fn toggle_subtitle(handle: &EditorHandle) {
    if subtitle_active(handle) {
        handle.set_block_data_attr(ROLE_ATTR, None);
        return;
    }
    if handle.current_block_type().as_deref() != Some("paragraph") {
        crate::toolbar::set_plain_paragraph(handle);
    }
    handle.set_block_data_attr(ROLE_ATTR, Some(SUBTITLE));
}

#[cfg(test)]
mod tests;
