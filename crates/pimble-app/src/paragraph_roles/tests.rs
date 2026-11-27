use std::rc::Rc;

use pimble_crdt::markdown;
use pimble_crdt::text_roles::is_subtitle;
use rinch_editor_core::selection::Selection;
use rinch_editor_core::Pos;

use super::*;
use crate::rinch_editor::create_editor;

/// A note's editor holding `md`.
fn note(md: &str) -> EditorHandle {
    let handle = create_editor();
    assert!(handle.add_plugin(Rc::new(ParagraphRolesPlugin)));
    handle.update(|state| {
        let doc = markdown::parse(state.schema(), md).unwrap();
        let mut tr = state.tr();
        tr.replace_with(0, state.doc.content_size(), doc.content().clone())
            .unwrap();
        Some(tr)
    });
    handle
}

/// Puts the caret at `pos`.
fn caret(handle: &EditorHandle, pos: usize) {
    handle.update(|state| {
        let mut tr = state.tr();
        tr.set_selection(Selection::cursor(Pos(pos)));
        Some(tr)
    });
}

fn written(handle: &EditorHandle) -> String {
    markdown::render(&handle.state().doc).trim_end().to_string()
}

/// Each top-level block's classes.
fn drawn(handle: &EditorHandle) -> Vec<String> {
    classes(handle.state().doc.content().children())
}

#[test]
fn enter_at_the_end_of_a_subtitle_starts_an_ordinary_paragraph() {
    let handle = note("by Ann {.subtitle}");
    caret(&handle, 7);
    assert!(handle.command("splitBlock"), "Enter");
    let state = handle.state();
    let blocks = state.doc.content().children();
    assert_eq!(blocks.len(), 2);
    assert!(is_subtitle(&blocks[0]));
    assert!(!is_subtitle(&blocks[1]));
}

#[test]
fn enter_inside_a_subtitle_leaves_two() {
    let handle = note("by Ann {.subtitle}");
    caret(&handle, 3);
    assert!(handle.command("splitBlock"));
    assert!(handle
        .state()
        .doc
        .content()
        .children()
        .iter()
        .all(is_subtitle));
}

#[test]
fn the_button_gives_the_role_and_takes_it_back() {
    let handle = note("by Ann");
    caret(&handle, 2);
    assert!(!subtitle_active(&handle));
    toggle_subtitle(&handle);
    assert!(subtitle_active(&handle));
    assert_eq!(written(&handle), "by Ann {.subtitle}");
    toggle_subtitle(&handle);
    assert!(!subtitle_active(&handle));
    assert_eq!(written(&handle), "by Ann");
}

#[test]
fn the_button_turns_a_heading_into_a_subtitle_and_a_heading_drops_the_role() {
    let handle = note("## by Ann");
    caret(&handle, 2);
    toggle_subtitle(&handle);
    assert_eq!(written(&handle), "by Ann {.subtitle}");
    assert!(handle.command("setHeading2"));
    assert_eq!(written(&handle), "## by Ann");
}

#[test]
fn a_subtitle_and_what_it_follows_are_told_apart() {
    let handle = note("# T\n\none {.subtitle}\n\ntwo {.subtitle}\n\nText.\n\nstray {.subtitle}");
    assert_eq!(
        drawn(&handle),
        [BEFORE_SUBTITLE, BEFORE_SUBTITLE, "", "", ""]
    );
}

#[test]
fn a_chosen_heading_alignment_reaches_the_subtitles_under_it() {
    let handle = note("# T\n\none {.subtitle}\n\ntwo {.subtitle}\n\nText.\n\nstray {.subtitle}");
    caret(&handle, 2);
    assert!(handle.command("setTextAlignCenter"));
    assert_eq!(
        drawn(&handle),
        [
            BEFORE_SUBTITLE.to_string(),
            format!("pimble-title-center {BEFORE_SUBTITLE}"),
            "pimble-title-center".into(),
            String::new(),
            String::new()
        ]
    );
}

const PICTURE: &str = "![a](pimble-blob:00000000-0000-0000-0000-000000000001/ab)";

#[test]
fn a_paragraph_of_pictures_alone_is_a_picture_block() {
    let md = format!("{PICTURE}\n\n{PICTURE} {PICTURE}\n\nsee {PICTURE}\n\n> {PICTURE}\n\n&nbsp;");
    let handle = note(&md);
    assert_eq!(drawn(&handle), [PICTURE_BLOCK, PICTURE_BLOCK, "", "", ""]);
    let state = handle.state();
    let decorations = ParagraphRolesPlugin.decorations(&state);
    // The two at the top level, and the one in the quote.
    assert_eq!(decorations.iter().count(), 3, "{decorations:?}");
}

#[test]
fn typing_beside_a_picture_takes_the_class_away() {
    let handle = note(PICTURE);
    assert_eq!(
        ParagraphRolesPlugin
            .decorations(&handle.state())
            .iter()
            .count(),
        1
    );
    caret(&handle, 1);
    handle.update(|state| {
        let mut tr = state.tr();
        tr.insert_text("see ").unwrap();
        Some(tr)
    });
    assert_eq!(
        ParagraphRolesPlugin
            .decorations(&handle.state())
            .iter()
            .count(),
        0
    );
}
