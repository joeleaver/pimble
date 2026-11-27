use rinch_editor_core::{Node, Schema};

use super::SUBTITLE_MARK;
use crate::content_doc::project;
use crate::markdown::{parse, render};
use crate::text_roles::is_subtitle;
use crate::NodeDoc;

fn read(md: &str) -> Node {
    parse(&Schema::starter_kit(), md).unwrap()
}

/// Each textblock's text and whether it is a subtitle.
fn blocks(doc: &Node) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    doc.nodes_between(0, doc.content_size(), &mut |node, _, _| {
        if node.is_textblock() {
            let text = node.content().iter().filter_map(Node::text).collect();
            out.push((text, is_subtitle(node)));
        }
        true
    });
    out
}

/// `md` reads as it says, and writes back as the very same Markdown, which
/// reads as the same document.
fn round_trip(md: &str, expected: &[(&str, bool)]) {
    let doc = read(md);
    let expected: Vec<(String, bool)> = expected.iter().map(|(t, s)| (t.to_string(), *s)).collect();
    assert_eq!(blocks(&doc), expected, "{md:?}");
    let written = render(&doc);
    assert_eq!(written.trim_end(), md.trim_end());
    assert_eq!(blocks(&read(&written)), expected, "{written:?}");
}

#[test]
fn a_subtitle_is_its_paragraph_with_the_mark_after_it() {
    round_trip(
        "# The Bear\n\nby Joe's Robots {.subtitle}\n\nOnce.",
        &[
            ("The Bear", false),
            ("by Joe's Robots", true),
            ("Once.", false),
        ],
    );
}

#[test]
fn marks_close_before_the_mark() {
    round_trip("*by Ann.* {.subtitle}", &[("by Ann.", true)]);
    round_trip("by **Ann** {.subtitle}", &[("by Ann", true)]);
}

#[test]
fn stacked_and_nested_subtitles() {
    round_trip(
        "A tale {.subtitle}\n\nby Ann {.subtitle}",
        &[("A tale", true), ("by Ann", true)],
    );
    round_trip("> by Ann {.subtitle}", &[("by Ann", true)]);
    round_trip("- by Ann {.subtitle}", &[("by Ann", true)]);
}

#[test]
fn a_subtitle_in_a_table_cell_is_read() {
    let doc = read("| a {.subtitle} | b |\n| --- | --- |\n| c | d {.subtitle} |");
    assert_eq!(
        blocks(&doc),
        [
            ("a".into(), true),
            ("b".into(), false),
            ("c".into(), false),
            ("d".into(), true)
        ]
    );
    assert_eq!(blocks(&read(&render(&doc))), blocks(&doc));
}

#[test]
fn the_words_escaped_or_in_code_stay_words() {
    round_trip("Type \\{.subtitle}", &[("Type {.subtitle}", false)]);
    round_trip("Type `{.subtitle}`", &[("Type {.subtitle}", false)]);
    round_trip("```\nx {.subtitle}\n```", &[("x {.subtitle}", false)]);
    round_trip(
        "Type \\{.subtitle} {.subtitle}",
        &[("Type {.subtitle}", true)],
    );
}

#[test]
fn the_mark_elsewhere_in_a_line_and_after_a_heading_is_words() {
    assert_eq!(
        blocks(&read("{.subtitle} is the mark")),
        [(format!("{SUBTITLE_MARK} is the mark"), false)]
    );
    assert_eq!(
        blocks(&read("# Title {.subtitle}")),
        [(format!("Title {SUBTITLE_MARK}"), false)]
    );
}

/// A node's document keeps the role through the edits that touch blocks, and
/// through the fresh copy a transplant carries.
#[test]
fn a_node_s_edits_and_its_fresh_copy_keep_the_role() {
    let mut node = NodeDoc::new();
    node.write_markdown("# T\n\nby Ann\n\nOnce.").unwrap();
    let read_back = |node: &NodeDoc| node.markdown().unwrap().trim_end().to_string();

    // Only the role differs, both ways: still a change that is written.
    node.replace_content_markdown("# T\n\nby Ann {.subtitle}\n\nOnce.")
        .unwrap();
    assert_eq!(read_back(&node), "# T\n\nby Ann {.subtitle}\n\nOnce.");
    node.replace_content_markdown("# T\n\nby Ann\n\nOnce.")
        .unwrap();
    assert_eq!(read_back(&node), "# T\n\nby Ann\n\nOnce.");
    node.replace_block_markdown("by Ann", "by Ann {.subtitle}")
        .unwrap();
    node.replace_text("by Ann", "by Anne").unwrap();
    assert_eq!(read_back(&node), "# T\n\nby Anne {.subtitle}\n\nOnce.");

    let fresh = node.fresh_content().unwrap().unwrap();
    let doc = project(&fresh, "test").unwrap();
    assert_eq!(blocks(&doc)[1], ("by Anne".to_string(), true));
}
