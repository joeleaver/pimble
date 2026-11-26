//! The note editor's keys that Pimble binds itself.
//!
//! rinch's editor keeps only the keys Chrome's own editing has (rinch #3062):
//! its formatting chords are gone (the commands still run by name), and Tab
//! leaves the editor unless the app asks for indenting. Pimble binds back what
//! it promises and nothing more:
//!
//! - Ctrl+Shift+B (Cmd+Shift+B on a Mac) is the toolbar's Block Quote button,
//!   whose tooltip names it: the same toggle, through the same code
//!   (`toolbar::toggle_quote`).
//! - Tab and Shift+Tab indent and outdent in lists and move between table
//!   cells, as they did before rinch made leaving the editor the default.
//!
//! The chords Pimble never advertised (code, strike, headings, lists,
//! alignment) stay unbound, so they reach the app's menus; Ctrl+Shift+E is
//! Focus Explorer alone now that rinch's center-align no longer claims it.

use rinch_editor_core::{Command, KeyBinding, Plugin, PluginKey};

use crate::panes::PaneId;
use crate::rinch_editor::{EditorHandle, TabBehavior};

/// The command Ctrl+Shift+B runs.
const TOGGLE_QUOTE: &str = "pimble.toggleQuote";

/// Pimble's chords for one pane's editor.
pub(crate) struct EditorKeysPlugin {
    pub pane: PaneId,
}

impl Plugin for EditorKeysPlugin {
    fn key(&self) -> PluginKey {
        PluginKey("pimble.editor-keys")
    }

    fn keymap(&self) -> Vec<(KeyBinding, &'static str)> {
        KeyBinding::parse("Mod-Shift-b").map(|k| (k, TOGGLE_QUOTE)).into_iter().collect()
    }

    fn commands(&self) -> Vec<(&'static str, Command)> {
        // A command runs while the editor is mid-dispatch: the toolbar's
        // code reads the handle, so it waits a turn.
        let pane = self.pane;
        let toggle: Command = std::rc::Rc::new(move |_state, dispatch| {
            if dispatch.is_some() {
                crate::links::later(move |store| crate::toolbar::toggle_quote(store, pane));
            }
            true
        });
        vec![(TOGGLE_QUOTE, toggle)]
    }
}

/// Give `handle` (pane `pane`'s editor) Pimble's keys.
pub(crate) fn install(handle: &EditorHandle, pane: PaneId) {
    handle.set_tab_behavior(TabBehavior::IndentInListsAndTables);
    handle.add_plugin(std::rc::Rc::new(EditorKeysPlugin { pane }));
}

#[cfg(test)]
mod tests {
    use rinch_editor_core::keymap::KeyBinding;
    use rinch_editor_core::selection::Selection;
    use rinch_editor_core::Pos;

    use super::*;
    use crate::rinch_editor::create_editor;

    /// Ctrl+Shift+B is the editor's again: the key is taken (the toggle itself
    /// runs a turn later, through the toolbar's code, which needs the app's
    /// store and is covered by the toolbar's tests), and the chords Pimble
    /// never advertised are still left to the app.
    #[test]
    fn ctrl_shift_b_is_the_editors_and_the_rest_are_not() {
        let handle = create_editor();
        install(&handle, PaneId::ALL[0]);
        handle.load_html("<p>quote me</p>");
        let chord = |s: &str| KeyBinding::parse(s).expect("a binding");
        // Consumed (`Some`); the toggle's own transaction comes a turn
        // later, so the dispatch itself applied nothing.
        assert!(handle.dispatch_key(chord("Mod-Shift-b")).is_some(), "the editor takes it");
        for unbound in ["Mod-e", "Mod-Shift-s", "Mod-Shift-8", "Mod-Shift-e", "Mod-Alt-1"] {
            assert_eq!(handle.dispatch_key(chord(unbound)), None, "{unbound} is not the editor's");
        }
    }

    /// Tab indents a list item, as it did before rinch's default moved the
    /// focus out of the editor.
    #[test]
    fn tab_indents_in_a_list() {
        let handle = create_editor();
        install(&handle, PaneId::ALL[0]);
        handle.load_html("<ul><li><p>one</p></li><li><p>two</p></li></ul>");
        // Inside "two": <ul>(0) <li>(1) <p>(2) one(3..6) </p></li>(6, 7)
        // <li>(8) <p>(9) t(10).
        handle.set_selection(Selection::cursor(Pos(11)));
        let items = |h: &EditorHandle| h.doc().child(0).child_count();
        assert_eq!(items(&handle), 2);
        assert!(handle.tab_key(false), "Tab is the editor's in a list");
        assert_eq!(items(&handle), 1, "two went under one");
        assert_eq!(handle.doc().child(0).child(0).child(1).type_name(), "bullet_list");
        assert!(handle.tab_key(true), "Shift+Tab is too");
        assert_eq!(items(&handle), 2, "two came back out");

        // Outside a list Tab still leaves the editor (WCAG 2.1.2).
        handle.load_html("<p>plain</p>");
        handle.set_selection(Selection::cursor(Pos(2)));
        assert!(!handle.tab_key(false));
    }
}
