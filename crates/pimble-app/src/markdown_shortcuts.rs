//! Markdown shortcuts in the note editor (Joe, 2026-11-24: "Turn them on").
//!
//! Typing `# ` makes a heading, `- ` a bullet list, `1. ` a numbered list,
//! `> ` a quote, ```` ``` ```` a code block, `---` a rule, and `**x**`,
//! `*x*`, `~~x~~`, `==x==` and `` `x` `` the marks they stand for. These are
//! rinch's markdown input rules (rinch #2210, off in an editor from
//! `create_editor`); each one is an ordinary edit of the document, so it
//! travels through the collaboration session like any typing.
//!
//! Pimble does not use rinch's `MarkdownInputRulesPlugin` as it is
//! (`EditorHandle::set_markdown_shortcuts`): its set includes `[ ] ` for a
//! task list, which is outside the collaboration scope (CLAUDE.md, "Rinch's
//! collab scope": task lists fail loudly). [`MarkdownShortcutsPlugin`] is
//! rinch's set without that rule. Every rule it keeps is pinned by
//! [`KEPT`]: a rinch update that adds a shortcut fails the test below until
//! someone has judged whether what it makes is inside the scope.

use rinch_editor_core::input_rules::{markdown_input_rules, task_list_input_rule, InputRule};
use rinch_editor_core::{Plugin, PluginKey};

/// The note editor's markdown shortcuts: rinch's markdown input rules less
/// the task list's.
#[derive(Debug, Default)]
pub struct MarkdownShortcutsPlugin;

impl Plugin for MarkdownShortcutsPlugin {
    fn key(&self) -> PluginKey {
        PluginKey("pimble.markdown-shortcuts")
    }

    fn input_rules(&self) -> Vec<InputRule> {
        shortcuts()
    }
}

/// rinch's markdown input rules, without the one that makes a task list.
fn shortcuts() -> Vec<InputRule> {
    let task_list = task_list_input_rule().pattern.as_str().to_owned();
    markdown_input_rules().into_iter().filter(|rule| rule.pattern.as_str() != task_list).collect()
}

/// The patterns of the rules Pimble keeps, in rinch's order, each making
/// something inside the collaboration scope: headings, a code block, a
/// horizontal rule, a block quote, bullet and numbered lists, and the bold,
/// italic, strike, highlight and inline-code marks.
#[cfg(test)]
const KEPT: &[&str] = &[
    r"^(#{1,6})\s$",
    r"^```$",
    r"^(?:---|\*\*\*)$",
    r"^\s*>\s$",
    r"^\s*([-+*])\s$",
    r"^(\d+)\.\s$",
    r"(?:^|\s)(\*\*([^\s*](?:[^*]*[^\s*])?)\*\*)$",
    r"(?:^|\s)(__([^\s_](?:[^_]*[^\s_])?)__)$",
    r"(?:^|\s)(\*([^\s*](?:[^*]*[^\s*])?)\*)$",
    r"(?:^|\s)(_([^\s_](?:[^_]*[^\s_])?)_)$",
    r"(?:^|\s)(~~([^\s~](?:[^~]*[^\s~])?)~~)$",
    r"(?:^|\s)(==([^\s=](?:[^=]*[^\s=])?)==)$",
    r"(?:^|\s)(`([^\s`](?:[^`]*[^\s`])?)`)$",
];

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use rinch_editor_core::selection::Selection;
    use rinch_editor_core::Pos;

    use super::*;
    use crate::rinch_editor::{create_editor, EditorHandle};

    fn editor() -> EditorHandle {
        let handle = create_editor();
        assert!(handle.add_plugin(Rc::new(MarkdownShortcutsPlugin)));
        handle.set_selection(Selection::cursor(Pos(1)));
        handle
    }

    /// Typed one character at a time, as the keyboard does: a rule fires on
    /// the character that completes it.
    fn type_keys(handle: &EditorHandle, text: &str) {
        for c in text.chars() {
            assert!(handle.insert_text(&c.to_string()), "typing {c:?} changed the document");
        }
    }

    /// The text of the document's first block, through any wrapping.
    fn first_text(handle: &EditorHandle) -> String {
        fn collect(node: &rinch_editor_core::Node, out: &mut String) {
            if let Some(text) = node.text() {
                out.push_str(text);
            }
            for i in 0..node.child_count() {
                collect(node.child(i), out);
            }
        }
        let mut text = String::new();
        collect(handle.doc().child(0), &mut text);
        text
    }

    fn first_block(handle: &EditorHandle) -> String {
        handle.doc().child(0).type_name().to_owned()
    }

    /// The rules kept are exactly the ones judged to be inside the scope; a
    /// rule rinch adds or changes shows up here first.
    #[test]
    fn the_kept_shortcuts_are_the_judged_ones() {
        let patterns: Vec<String> = shortcuts().iter().map(|r| r.pattern.as_str().to_owned()).collect();
        assert_eq!(patterns, KEPT);
        let task_list = task_list_input_rule().pattern.as_str().to_owned();
        assert!(markdown_input_rules().iter().any(|r| r.pattern.as_str() == task_list), "rinch still has the task list rule this module leaves out");
    }

    #[test]
    fn hash_space_makes_a_heading() {
        let handle = editor();
        type_keys(&handle, "## Title");
        assert_eq!(first_block(&handle), "heading");
        assert_eq!(handle.doc().child(0).attrs().get("level").and_then(|v| v.as_int()), Some(2));
        assert_eq!(first_text(&handle), "Title");
    }

    #[test]
    fn dash_space_makes_a_bullet_list() {
        let handle = editor();
        type_keys(&handle, "- item");
        assert_eq!(first_block(&handle), "bullet_list");
        assert_eq!(first_text(&handle), "item");
    }

    /// A task list is outside the collaboration scope: `[ ] ` stays text.
    #[test]
    fn a_checkbox_stays_text() {
        let handle = editor();
        type_keys(&handle, "[ ] ");
        assert_eq!(first_block(&handle), "paragraph");
        assert_eq!(first_text(&handle), "[ ] ");
    }

    /// A shortcut is an ordinary edit: in a collaborating editor it goes out
    /// as a delta, and a peer that applies it has the heading too.
    #[test]
    fn a_shortcut_reaches_a_collaborator() {
        let host = editor();
        let outbox = Rc::new(RefCell::new(Vec::<Vec<u8>>::new()));
        let out = outbox.clone();
        host.start_collaboration_host(move |delta| out.borrow_mut().push(delta)).expect("a session");
        let peer = create_editor();
        peer.start_collaboration_guest(&host.collab_snapshot().unwrap(), |_| {}).expect("a guest session");

        host.set_selection(Selection::cursor(Pos(1)));
        type_keys(&host, "# Plan");
        assert_eq!(first_block(&host), "heading");
        for delta in outbox.borrow_mut().drain(..) {
            assert!(peer.collab_receive(&delta), "the peer takes the delta");
        }
        assert_eq!(first_block(&peer), "heading");
        assert_eq!(first_text(&peer), "Plan");
    }
}
