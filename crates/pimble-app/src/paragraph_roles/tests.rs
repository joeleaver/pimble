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
        tr.replace_with(0, state.doc.content_size(), doc.content().clone()).unwrap();
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
    assert!(handle.state().doc.content().children().iter().all(is_subtitle));
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
    let handle = note("## T\n\none {.subtitle}\n\ntwo {.subtitle}\n\nthree {.subtitle}\n\nText.\n\nstray {.subtitle}");
    let under = format!("{} {BEFORE_SUBTITLE}", UNDER[1]);
    assert_eq!(drawn(&handle), [BEFORE_SUBTITLE, under.as_str(), under.as_str(), UNDER[1], "", ""]);
}

#[test]
fn a_chosen_heading_alignment_reaches_every_subtitle_under_it() {
    let handle = note("# T\n\none {.subtitle}\n\ntwo {.subtitle}\n\nText.\n\nstray {.subtitle}");
    caret(&handle, 2);
    assert!(handle.command("setTextAlignCenter"));
    let under = format!("{} pimble-title-center", UNDER[0]);
    assert_eq!(drawn(&handle), [BEFORE_SUBTITLE.to_string(), format!("{under} {BEFORE_SUBTITLE}"), under, String::new(), String::new()]);
}

/// Selects from `from` to `to`.
fn select(handle: &EditorHandle, from: usize, to: usize) {
    handle.update(|state| {
        let mut tr = state.tr();
        tr.set_selection(Selection::text(Pos(from), Pos(to)));
        Some(tr)
    });
}

#[test]
fn the_button_on_a_heading_and_a_paragraph_makes_both_subtitles_in_one_step() {
    let md = "## Ab\n\ncd\n\n```\nef\n```";
    let handle = note(md);
    // The selection ends in the paragraph, then in the code block.
    for to in [6, 10] {
        select(&handle, 1, to);
        assert!(!subtitle_active(&handle));
        toggle_subtitle(&handle);
        assert!(subtitle_active(&handle));
        let state = handle.state();
        let touched = if to == 6 { 2 } else { 3 };
        for (i, block) in state.doc.content().children().iter().enumerate() {
            assert_eq!(is_subtitle(block), i < touched, "{to} {i}");
            assert_eq!(block.type_name() == "paragraph", i < touched || i == 1, "{to} {i}");
        }
        assert!(handle.command("undo"));
        assert_eq!(written(&handle), md);
    }
}

#[test]
fn a_selection_that_is_partly_subtitles_becomes_all_subtitles() {
    let handle = note("ab {.subtitle}\n\ncd");
    select(&handle, 1, 6);
    assert!(!subtitle_active(&handle));
    toggle_subtitle(&handle);
    assert_eq!(written(&handle), "ab {.subtitle}\n\ncd {.subtitle}");
    toggle_subtitle(&handle);
    assert_eq!(written(&handle), "ab\n\ncd");
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
    assert_eq!(ParagraphRolesPlugin.decorations(&handle.state()).iter().count(), 1);
    caret(&handle, 1);
    handle.update(|state| {
        let mut tr = state.tr();
        tr.insert_text("see ").unwrap();
        Some(tr)
    });
    assert_eq!(ParagraphRolesPlugin.decorations(&handle.state()).iter().count(), 0);
}
