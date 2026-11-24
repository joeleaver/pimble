//! No task lists in the note editor.
//!
//! A task list is outside the collaboration scope (CLAUDE.md, "Rinch's collab
//! scope": task lists fail loudly), but rinch's base keymap binds Ctrl+Shift+7
//! (Cmd+Shift+7) to `toggleTaskList`. [`NoTaskListsPlugin`] replaces that
//! command by name (a later plugin's command of the same name wins, rinch's
//! `Plugin::commands`) with one that never applies, so the chord does
//! nothing: the binding still consumes the key, as every base binding does,
//! and nothing else hears it. The `[ ] ` shortcut is left out separately
//! (`crate::markdown_shortcuts`).

use rinch_editor_core::{Command, Plugin, PluginKey};

/// The base keymap's task-list command.
const TOGGLE_TASK_LIST: &str = "toggleTaskList";

/// Replaces `toggleTaskList` with a command that never applies. Added after
/// `create_editor`'s plugins, so its command is the one the registry keeps.
#[derive(Debug, Default)]
pub struct NoTaskListsPlugin;

impl Plugin for NoTaskListsPlugin {
    fn key(&self) -> PluginKey {
        PluginKey("pimble.no-task-lists")
    }

    fn commands(&self) -> Vec<(&'static str, Command)> {
        let never: Command = std::rc::Rc::new(|_, _| false);
        vec![(TOGGLE_TASK_LIST, never)]
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use rinch_editor_core::keymap::KeyBinding;
    use rinch_editor_core::selection::Selection;
    use rinch_editor_core::Pos;

    use super::*;
    use crate::rinch_editor::{create_editor, EditorHandle};

    fn note(with_plugin: bool) -> EditorHandle {
        let handle = create_editor();
        if with_plugin {
            assert!(handle.add_plugin(Rc::new(NoTaskListsPlugin)));
        }
        handle.load_html("<p>milk</p>");
        handle.set_selection(Selection::cursor(Pos(2)));
        handle
    }

    /// Ctrl+Shift+7 makes a task list in rinch's editor; in the note editor
    /// it is still the editor's key, and the paragraph stays a paragraph.
    #[test]
    fn ctrl_shift_7_makes_no_task_list() {
        let chord = || KeyBinding::parse("Mod-Shift-7").expect("a binding");

        let plain = note(false);
        assert_eq!(plain.dispatch_key(chord()), Some(true), "rinch binds the chord to a task list");
        assert_eq!(plain.doc().child(0).type_name(), "task_list");

        let note = note(true);
        assert_eq!(note.dispatch_key(chord()), Some(false), "the key is consumed and does nothing");
        assert_eq!(note.doc().child(0).type_name(), "paragraph");
        assert!(!note.command(TOGGLE_TASK_LIST));
    }
}
