//! No task lists in the note editor.
//!
//! A task list is outside the collaboration scope (CLAUDE.md, "Rinch's collab
//! scope": task lists fail loudly), but rinch's base plugin still offers the
//! `toggleTaskList` command by name, which anything that runs editor commands
//! could reach. (rinch's editor no longer binds Ctrl+Shift+7 to it, since
//! #3062 left it only Chrome's keys.) [`NoTaskListsPlugin`] replaces that
//! command by name (a later plugin's command of the same name wins, rinch's
//! `Plugin::commands`) with one that never applies. The `[ ] ` shortcut is
//! left out separately (`crate::markdown_shortcuts`).

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

    /// rinch's editor keeps no Ctrl+Shift+7 (rinch #3062), but its
    /// `toggleTaskList` still makes a task list by name; in the note editor
    /// the command does nothing and the paragraph stays a paragraph.
    #[test]
    fn no_task_list_by_key_or_by_name() {
        let chord = || KeyBinding::parse("Mod-Shift-7").expect("a binding");

        let plain = note(false);
        assert_eq!(plain.dispatch_key(chord()), None, "the chord is not the editor's");
        assert_eq!(plain.doc().child(0).type_name(), "paragraph");
        assert!(plain.command(TOGGLE_TASK_LIST), "rinch's command makes a task list");
        assert_eq!(plain.doc().child(0).type_name(), "task_list");

        let note = note(true);
        assert_eq!(note.dispatch_key(chord()), None, "the chord is not the editor's");
        assert!(!note.command(TOGGLE_TASK_LIST), "the note editor's never applies");
        assert_eq!(note.doc().child(0).type_name(), "paragraph");
    }
}
