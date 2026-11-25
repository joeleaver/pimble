use std::collections::HashMap;

use super::*;
use pimble_themes::Builtins;

/// A tree as `walk` reads it: id -> (label, theme, parent, spaced).
struct Tree(HashMap<NodeId, Held>);

impl Tree {
    fn new() -> Tree {
        Tree(HashMap::new())
    }

    fn add(&mut self, label: &str, theme: Option<&str>, parent: Option<NodeId>, spaced: bool) -> NodeId {
        let node_id = NodeId::new();
        self.0.insert(
            node_id,
            Held {
                step: Step { node_id, label: label.to_string(), theme: theme.map(String::from) },
                parent_id: parent,
                spaced,
            },
        );
        node_id
    }

    fn walk(&self, node_id: NodeId) -> Walk {
        walk(node_id, |id| self.0.get(&id).cloned(), &Builtins)
    }
}

#[test]
fn the_nearest_ancestor_naming_a_theme_decides_and_the_walk_stops_there() {
    let mut tree = Tree::new();
    let root = tree.add("Root", Some("builtin:code"), None, false);
    let novel = tree.add("Novel", Some("builtin:book"), Some(root), false);
    let chapter = tree.add("Chapter", None, Some(novel), true);
    let walk = tree.walk(chapter);
    assert_eq!(walk.steps.len(), 2, "nothing above the theme found is read");
    assert!(walk.missing.is_none());
    let in_force = in_force_of(&walk, &Builtins);
    assert_eq!(in_force.id, "builtin:book");
    assert_eq!(in_force.origin, Origin::Named { depth: 1 });
    assert_eq!(in_force_sentence(&[], &in_force, &walk), "Book, from 'Novel'");
}

#[test]
fn a_node_naming_its_own_theme_says_set_here() {
    let mut tree = Tree::new();
    let root = tree.add("Root", Some("builtin:book"), None, false);
    let note = tree.add("Note", Some("builtin:manuscript"), Some(root), false);
    let walk = tree.walk(note);
    let in_force = in_force_of(&walk, &Builtins);
    assert_eq!(in_force.id, "builtin:manuscript");
    assert_eq!(in_force_sentence(&[], &in_force, &walk), "Manuscript, set here");
}

#[test]
fn with_no_theme_named_a_spaced_note_reads_as_document_and_others_as_default() {
    let mut tree = Tree::new();
    let root = tree.add("Root", None, None, false);
    let spaced = tree.add("Spaced", None, Some(root), true);
    let compact = tree.add("Compact", None, Some(root), false);
    let spaced_walk = tree.walk(spaced);
    assert_eq!(in_force_of(&spaced_walk, &Builtins).id, pimble_themes::DOCUMENT);
    assert_eq!(in_force_of(&spaced_walk, &Builtins).origin, Origin::SpacedParagraphs);
    let compact_walk = tree.walk(compact);
    assert_eq!(in_force_of(&compact_walk, &Builtins).id, pimble_themes::DEFAULT);
    // Only the note's own paragraphs count, not an ancestor's.
    let child = tree.add("Child", None, Some(spaced), false);
    assert_eq!(in_force_of(&tree.walk(child), &Builtins).id, pimble_themes::DEFAULT);
}

#[test]
fn a_theme_above_a_spaced_note_replaces_the_fallback() {
    let mut tree = Tree::new();
    let root = tree.add("Root", Some("builtin:code"), None, false);
    let note = tree.add("Note", None, Some(root), true);
    assert_eq!(in_force_of(&tree.walk(note), &Builtins).id, "builtin:code");
}

#[test]
fn a_name_no_theme_answers_is_skipped_and_said() {
    let mut tree = Tree::new();
    let root = tree.add("Root", Some("builtin:report"), None, false);
    let folder = tree.add("Folder", Some("a-theme-node-this-device-lacks"), Some(root), false);
    let note = tree.add("Note", None, Some(folder), false);
    let walk = tree.walk(note);
    let in_force = in_force_of(&walk, &Builtins);
    assert_eq!(in_force.id, "builtin:report");
    assert_eq!(in_force.unresolved, vec![(1, "a-theme-node-this-device-lacks".to_string())]);
    assert_eq!(
        unresolved_sentences(&in_force, &walk),
        vec!["The theme that 'Folder' names is not one this device has, so the one above it is used.".to_string()]
    );
}

#[test]
fn an_ancestor_the_app_does_not_hold_is_named_for_asking() {
    let mut tree = Tree::new();
    let absent = NodeId::new();
    let folder = tree.add("Folder", None, Some(absent), false);
    let note = tree.add("Note", None, Some(folder), false);
    let walk = tree.walk(note);
    assert_eq!(walk.missing, Some(absent));
    assert_eq!(walk.steps.len(), 2);
    // A node the app does not hold at all has nothing to ask for yet.
    assert_eq!(tree.walk(NodeId::new()), Walk::default());
}

#[test]
fn a_loop_of_parents_ends_the_walk() {
    let mut tree = Tree::new();
    let a = NodeId::new();
    let b = tree.add("B", None, Some(a), false);
    tree.0.insert(
        a,
        Held { step: Step { node_id: a, label: "A".to_string(), theme: None }, parent_id: Some(b), spaced: false },
    );
    let walk = tree.walk(b);
    assert_eq!(walk.steps.len(), 2);
    assert!(walk.missing.is_none());
}

#[test]
fn classes_name_each_modifier_on_the_block() {
    let drawn = Drawn { style: String::new(), modifiers: vec!["para-spaced", "measured"] };
    assert_eq!(
        classes(WRAP_CLASS, &drawn),
        "pimble-editor__content-wrap pimble-editor__content-wrap--para-spaced pimble-editor__content-wrap--measured"
    );
    assert_eq!(classes(WRAP_CLASS, &Drawn::default()), WRAP_CLASS);
}

#[test]
fn a_wrap_names_its_scheme_after_the_theme() {
    let drawn = Drawn { style: String::new(), modifiers: vec!["measured"] };
    assert_eq!(
        wrap_classes(&drawn, true),
        "pimble-editor__content-wrap pimble-editor__content-wrap--measured pimble-editor__content-wrap--dark"
    );
    assert_eq!(
        wrap_classes(&Drawn::default(), false),
        "pimble-editor__content-wrap pimble-editor__content-wrap--light"
    );
}

#[test]
fn a_new_theme_takes_a_name_no_theme_has() {
    assert_eq!(free_name("New theme", &[]), "New theme");
    assert_eq!(free_name("New theme", &["new theme".into(), "new theme 2".into()]), "New theme 3");
}
