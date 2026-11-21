//! Hashtags in the text look like hashtags (docs/HASHTAGS_CONTRACT.md "Next:
//! hashtags look like hashtags in the text").
//!
//! Nothing is written into the document: [`HashtagsPlugin`] gives each
//! hashtag's range an `Inline` decoration with the class [`CLASS`], which the
//! editor stylesheet colors (color only, so the text lays out exactly as
//! before and the caret, selection, typing and deleting are untouched). The
//! ranges come from `pimble_crdt::hashtags::scan`, the one rule the note's
//! tags are read with, over the editor's own document: each textblock's
//! characters with inline code and inline atoms masked, code blocks skipped.
//!
//! The plugin keeps what it found per top-level block and, after an edit,
//! scans again only the top-level blocks that are not the same as before
//! (the editor's document shares every block an edit did not touch), and a
//! block whose text holds no `#` costs only a look at its strings.
//!
//! Ctrl+click (Cmd+click on macOS) on a hashtag searches for it
//! ([`clicked`]), from the pane's release as a click on a canvas in the text
//! is (`text_boards::pane_mouseup`).

use std::cell::Cell;
use std::rc::Rc;

use rinch::prelude::*;
use rinch_editor_core::state::{EditorState, Transaction};
use rinch_editor_core::{Attrs, Decoration, DecorationSet, Node as EditorNode, Plugin, PluginKey, Pos};

use crate::panes::PaneId;
use crate::state::AppStore;

/// The class a hashtag's characters carry in the editor.
pub const CLASS: &str = "pimble-hashtag";

const PLUGIN_KEY: PluginKey = PluginKey("pimble.hashtags");

/// The editor's names for what the scanner masks or skips.
const CODE_MARK: &str = "code";
const CODE_BLOCK: &str = "code_block";
/// What an inline atom stands for in the characters given to the scanner
/// (`pimble_crdt::hashtags`'s placeholder for one in yrs text).
const ATOM: char = '\u{FFFC}';

thread_local! {
    /// When a press last followed a link (ms): its release, which comes
    /// within a moment, is not a hashtag's click.
    static LINK_FOLLOWED: Cell<f64> = const { Cell::new(f64::NEG_INFINITY) };
}

/// What the plugin found: per top-level block of the document, the hashtags'
/// ranges measured from the position before that block, and every range in
/// document positions. Cloning shares both.
#[derive(Debug, Clone, Default)]
struct Found {
    blocks: Rc<Vec<Rc<[(usize, usize)]>>>,
    ranges: Rc<Vec<(usize, usize)>>,
}

impl Found {
    /// What `doc` holds, reusing `prev`'s answer for every top-level block
    /// `doc` shares with `prev_doc` at its start or its end.
    fn of(doc: &EditorNode, prev: Option<(&EditorNode, &Found)>) -> Found {
        let n = doc.child_count();
        let mut blocks: Vec<Rc<[(usize, usize)]>> = Vec::with_capacity(n);
        match prev {
            Some((old, found)) if found.blocks.len() == old.child_count() => {
                let m = old.child_count();
                let mut head = 0;
                while head < n && head < m && doc.child(head).same_ref(old.child(head)) {
                    head += 1;
                }
                let mut tail = 0;
                while tail < n - head && tail < m - head && doc.child(n - 1 - tail).same_ref(old.child(m - 1 - tail)) {
                    tail += 1;
                }
                blocks.extend(found.blocks[..head].iter().cloned());
                blocks.extend((head..n - tail).map(|i| block_ranges(doc.child(i)).into()));
                blocks.extend(found.blocks[m - tail..].iter().cloned());
            }
            _ => blocks.extend((0..n).map(|i| block_ranges(doc.child(i)).into())),
        }
        let mut ranges = Vec::new();
        let mut at = 0;
        for (i, found) in blocks.iter().enumerate() {
            ranges.extend(found.iter().map(|&(from, to)| (at + from, at + to)));
            at += doc.child(i).node_size();
        }
        Found { blocks: Rc::new(blocks), ranges: Rc::new(ranges) }
    }
}

/// The hashtags of top-level block `block`: `(from, to, tag)`, positions
/// measured from the position before it, `from..to` covering the `#` and the
/// tag.
fn block_hashtags(block: &EditorNode) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    if block.is_textblock() {
        textblock_hashtags(block, 1, &mut out);
        return out;
    }
    // Inside it, a textblock at content position `pos` starts its own
    // content at 1 (the block's opening) + `pos` + 1 (its own opening).
    block.nodes_between(0, block.content_size(), &mut |node, pos, _| {
        if node.is_textblock() {
            textblock_hashtags(node, pos + 2, &mut out);
            return false;
        }
        true
    });
    out
}

fn block_ranges(block: &EditorNode) -> Vec<(usize, usize)> {
    block_hashtags(block).into_iter().map(|(from, to, _)| (from, to)).collect()
}

/// The hashtags of textblock `textblock`, whose content starts at `start`.
/// A code block has none; a textblock with no `#` is not read further.
fn textblock_hashtags(textblock: &EditorNode, start: usize, out: &mut Vec<(usize, usize, String)>) {
    if textblock.type_name() == CODE_BLOCK {
        return;
    }
    let runs = (0..textblock.child_count()).map(|i| textblock.child(i));
    if !runs.clone().any(|run| run.text().is_some_and(|t| t.contains('#'))) {
        return;
    }
    let mut chars = Vec::new();
    let mut masked = Vec::new();
    for run in runs {
        match run.text() {
            Some(text) => {
                let code = run.marks().iter().any(|m| m.type_name() == CODE_MARK);
                for c in text.chars() {
                    chars.push(c);
                    masked.push(code);
                }
            }
            // An inline atom (an image, a hard break): one place per
            // position it takes, never text.
            None => {
                for _ in 0..run.node_size() {
                    chars.push(ATOM);
                    masked.push(true);
                }
            }
        }
    }
    out.extend(pimble_crdt::hashtags::scan(&chars, &masked).into_iter().map(|f| (start + f.start, start + f.end, f.tag)));
}

/// The hashtag at position `pos` of `doc` (from its `#` up to and including
/// the position right after its last character), as written, without `#`.
pub fn hashtag_at(doc: &EditorNode, pos: usize) -> Option<String> {
    let mut at = 0;
    for i in 0..doc.child_count() {
        let block = doc.child(i);
        let size = block.node_size();
        if pos <= at + size {
            if let Some((_, _, tag)) = block_hashtags(block).into_iter().find(|&(from, to, _)| at + from <= pos && pos <= at + to) {
                return Some(tag);
            }
        }
        if pos < at + size {
            break;
        }
        at += size;
    }
    None
}

/// The editor plugin that colors hashtags. Added to every pane's editor
/// before any content (adding a plugin resets history).
pub struct HashtagsPlugin;

impl Plugin for HashtagsPlugin {
    fn key(&self) -> PluginKey {
        PLUGIN_KEY
    }

    fn init_state(&self, doc: &EditorNode) -> Option<Rc<dyn std::any::Any>> {
        Some(Rc::new(Found::of(doc, None)))
    }

    fn apply(&self, tr: &Transaction, old: &EditorState, new_doc: &EditorNode, prev: Option<&dyn std::any::Any>) -> Option<Rc<dyn std::any::Any>> {
        let prev = prev.and_then(|p| p.downcast_ref::<Found>());
        if let (Some(prev), false) = (prev, tr.doc_changed()) {
            return Some(Rc::new(prev.clone()));
        }
        Some(Rc::new(Found::of(new_doc, prev.map(|p| (&old.doc, p)))))
    }

    fn decorations(&self, state: &EditorState) -> DecorationSet {
        let Some(found) = state.plugin_state::<Found>(PLUGIN_KEY) else { return DecorationSet::empty() };
        if found.ranges.is_empty() {
            return DecorationSet::empty();
        }
        let size = state.doc.content_size();
        DecorationSet::new(
            found
                .ranges
                .iter()
                .filter(|&&(from, to)| from < to && to <= size)
                .map(|&(from, to)| Decoration::inline(Pos(from), Pos(to), Attrs::new().with("class", CLASS)))
                .collect(),
        )
    }
}

/// A press on a link was claimed and followed (the link's own Ctrl+click):
/// the release that ends it searches nothing.
pub(crate) fn link_followed() {
    LINK_FOLLOWED.with(|f| f.set(crate::text_boards::now_ms()));
}

/// Whether the platform's "follow it" modifier was held, as a link's
/// `primary` says it: Cmd on macOS, Ctrl elsewhere. In the browser, where the
/// app does not know the platform, either.
fn primary(m: &rinch::core::ModifierState) -> bool {
    if cfg!(target_arch = "wasm32") {
        m.ctrl || m.meta
    } else if cfg!(target_os = "macos") {
        m.meta
    } else {
        m.ctrl
    }
}

/// The pointer was released in pane `pane`: when it was a Ctrl/Cmd+click
/// that put the caret in a hashtag, search for that hashtag. The press has
/// placed the caret by now (on the desktop and in the browser alike), so the
/// hashtag is the one at the caret, and only when the caret is where the
/// pointer is (a press on the pane's strip, or below the text, is not one).
pub(crate) fn clicked(store: AppStore, pane: PaneId) {
    let followed_at = LINK_FOLLOWED.with(|f| f.replace(f64::NEG_INFINITY));
    if crate::text_boards::now_ms() - followed_at < 1000.0 {
        return;
    }
    let ctx = rinch::core::get_click_context();
    if ctx.button != rinch::core::MouseButton::Left || !primary(&ctx.modifiers) || ctx.modifiers.shift || ctx.modifiers.alt {
        return;
    }
    let state = store.pane(pane);
    if !untracked(|| state.show_editor.get() && !state.show_board.get() && state.active_edit.with(Option::is_some)) {
        return;
    }
    let handle = crate::editor::editor(pane);
    let selection = handle.selection();
    if selection.from() != selection.to() {
        return;
    }
    let pos = selection.head();
    if handle.link_at(pos).is_some() {
        return;
    }
    let Some(caret) = handle.caret_rect(pos) else { return };
    let near = ctx.mouse_y >= caret.y - 2.0 && ctx.mouse_y <= caret.y + caret.height + 2.0 && (ctx.mouse_x - caret.x).abs() <= 40.0;
    if !near {
        return;
    }
    let Some(tag) = hashtag_at(&handle.doc(), pos.0) else { return };
    crate::app::search_for(store, format!("#{tag}"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use rinch_editor_core::{default_plugins, Fragment, Mark, Schema};

    fn schema() -> Rc<Schema> {
        Rc::new(Schema::starter_kit())
    }

    fn para(schema: &Schema, runs: Vec<EditorNode>) -> EditorNode {
        schema.branch("paragraph", Fragment::from_children(runs)).unwrap()
    }

    fn state_of(schema: Rc<Schema>, blocks: Vec<EditorNode>) -> EditorState {
        let doc = schema.branch(&schema.top_node, Fragment::from_children(blocks)).unwrap();
        let mut plugins = default_plugins();
        plugins.push(Rc::new(HashtagsPlugin) as Rc<dyn Plugin>);
        EditorState::create(schema, doc, plugins)
    }

    /// Each decorated range with the text it covers.
    fn decorated(state: &EditorState) -> Vec<(usize, usize, String)> {
        let doc = &state.doc;
        HashtagsPlugin
            .decorations(state)
            .iter()
            .map(|d| {
                assert_eq!(d.inline_class(), Some(CLASS));
                let (from, to) = d.range();
                let mut text = String::new();
                doc.nodes_between(from.0, to.0, &mut |node, pos, _| {
                    if let Some(t) = node.text() {
                        let skip = from.0.saturating_sub(pos);
                        let take = to.0.min(pos + node.node_size()) - pos.max(from.0);
                        text.extend(t.chars().skip(skip).take(take));
                    }
                    true
                });
                (from.0, to.0, text)
            })
            .collect()
    }

    #[test]
    fn hashtags_are_decorated_and_code_is_not() {
        let s = schema();
        let code = Mark::new(s.mark_type("code").unwrap().clone(), Attrs::new());
        let first = para(&s, vec![s.text("Plans for #pimble and ").unwrap(), s.text_with_marks("#notatag", vec![code]).unwrap(), s.text(" (#Work).").unwrap()]);
        let block = s.branch("code_block", Fragment::from_node(s.text("#nope in code").unwrap())).unwrap();
        let heading = s.create_node("heading", Attrs::new().with("level", 2i64), Fragment::from_node(s.text("#heading tag").unwrap())).unwrap();
        let list = s
            .branch("bullet_list", Fragment::from_node(s.branch("list_item", Fragment::from_node(para(&s, vec![s.text("item #nested").unwrap()]))).unwrap()))
            .unwrap();
        let plain = para(&s, vec![s.text("no tags here, a#b, #1").unwrap()]);
        let state = state_of(s, vec![first, block, heading, list, plain]);
        let got: Vec<String> = decorated(&state).into_iter().map(|(_, _, t)| t).collect();
        assert_eq!(got, ["#pimble", "#Work", "#heading", "#nested"]);
        // The first paragraph's content starts at 1: `#pimble` is chars 10..17.
        assert_eq!(decorated(&state)[0].0, 11);
        assert_eq!(hashtag_at(&state.doc, 11).as_deref(), Some("pimble"));
        assert_eq!(hashtag_at(&state.doc, 18).as_deref(), Some("pimble"), "right after its last character");
        assert_eq!(hashtag_at(&state.doc, 10), None);
        let nested = decorated(&state)[3].clone();
        assert_eq!(hashtag_at(&state.doc, nested.0 + 3).as_deref(), Some("nested"));
        // Inside the inline code: nothing.
        // `#notatag` is chars 22..30 of the paragraph, so positions 23..31.
        assert_eq!(hashtag_at(&state.doc, 26), None);
        assert_eq!(hashtag_at(&state.doc, 33).as_deref(), Some("Work"));
    }

    #[test]
    fn an_edit_rescans_only_what_changed_and_keeps_the_ranges_in_place() {
        let s = schema();
        let state = state_of(s.clone(), vec![para(&s, vec![s.text("first").unwrap()]), para(&s, vec![s.text("see #later").unwrap()])]);
        assert_eq!(decorated(&state).into_iter().map(|(_, _, t)| t).collect::<Vec<_>>(), ["#later"]);
        let second_before = state.plugin_state::<Found>(PLUGIN_KEY).unwrap().blocks[1].clone();
        // Typing a hashtag at the start of the first block moves the second's.
        let mut tr = state.tr();
        tr.set_selection(rinch_editor_core::selection::Selection::cursor(Pos(1)));
        let state = state.apply(tr);
        let mut tr = state.tr();
        tr.insert_text("#now ").unwrap();
        let state = state.apply(tr);
        assert_eq!(decorated(&state).into_iter().map(|(_, _, t)| t).collect::<Vec<_>>(), ["#now", "#later"]);
        // The untouched block's answer is the one found before.
        let found = state.plugin_state::<Found>(PLUGIN_KEY).unwrap();
        assert_eq!(found.blocks.len(), 2);
        assert!(Rc::ptr_eq(&found.blocks[1], &second_before), "the second block was not scanned again");
        // Typing into a hashtag lengthens it; a space ends it.
        let mut tr = state.tr();
        tr.set_selection(rinch_editor_core::selection::Selection::cursor(Pos(5)));
        let state = state.apply(tr);
        let mut tr = state.tr();
        tr.insert_text("ish").unwrap();
        let state = state.apply(tr);
        assert_eq!(decorated(&state).into_iter().map(|(_, _, t)| t).collect::<Vec<_>>(), ["#nowish", "#later"]);
    }

    #[test]
    fn a_document_with_no_hash_has_no_decorations() {
        let s = schema();
        let state = state_of(s.clone(), vec![para(&s, vec![s.text("nothing to see").unwrap()])]);
        assert!(HashtagsPlugin.decorations(&state).is_empty());
    }
}
