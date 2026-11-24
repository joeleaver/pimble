//! The block edits, model before to model after, and how they name what they touch.

use std::rc::Rc;

use rinch_editor_core::{Fragment, Node, Schema};

use super::edits::*;
use super::*;

fn schema() -> Rc<Schema> {
    Rc::new(Schema::starter_kit())
}

fn doc(md: &str) -> Node {
    parse(&schema(), md).unwrap()
}

// The edits as the tests below were first written against: by quote or heading
// name, answering the model after. What each says it did is tested in
// `echo_tests`.

fn ins(s: &Rc<Schema>, d: &Node, quote: &str, section: bool, md: &str) -> crate::Result<Node> {
    let at = if section { InsertAt::Section(HeadingTarget::Name(quote)) } else { InsertAt::Block(BlockTarget::quote(quote)) };
    insert_after(s, d, at, md).map(|(after, _)| after)
}

fn sect(s: &Rc<Schema>, d: &Node, heading: &str, md: &str) -> crate::Result<Node> {
    replace_section(s, d, HeadingTarget::Name(heading), md).map(|(after, _)| after)
}

fn text(s: &Rc<Schema>, d: &Node, quote: &str, new: &str) -> crate::Result<Node> {
    replace_text(s, d, Quote::alone(quote), new).map(|(after, _)| after)
}

fn block(s: &Rc<Schema>, d: &Node, quote: &str, md: &str) -> crate::Result<Node> {
    replace_block(s, d, BlockTarget::quote(quote), md).map(|(after, _)| after)
}

fn rmsec(s: &Rc<Schema>, d: &Node, heading: &str) -> crate::Result<Node> {
    remove_section(s, d, HeadingTarget::Name(heading)).map(|(after, _)| after)
}

fn content(s: &Rc<Schema>, d: &Node, md: &str) -> crate::Result<Vec<Node>> {
    replace_content(s, d, md).map(|(steps, _)| steps)
}

#[test]
fn append_after_content_and_instead_of_a_blank_document() {
    let s = schema();
    let after = append(&s, &doc("first"), "second").unwrap();
    assert_eq!(render(&after), "first\n\nsecond");
    let blank = s.branch("doc", Fragment::from_node(s.branch("paragraph", Fragment::empty()).unwrap())).unwrap();
    assert!(is_blank(&blank));
    assert_eq!(render(&append(&s, &blank, "only").unwrap()), "only");
    assert!(write_new(&s, &doc("taken"), "x").unwrap_err().to_string().contains("already has content"));
}

#[test]
fn insert_after_a_block_or_a_section() {
    let s = schema();
    let d = doc("# First\n\nf1\n\n## Sub\n\ns1\n\n# Second\n\nb1");
    assert_eq!(
        render(&ins(&s, &d, "f1", false, "new").unwrap()),
        "# First\n\nf1\n\nnew\n\n## Sub\n\ns1\n\n# Second\n\nb1"
    );
    assert_eq!(
        render(&ins(&s, &d, "Sub", true, "new").unwrap()),
        "# First\n\nf1\n\n## Sub\n\ns1\n\nnew\n\n# Second\n\nb1"
    );
    // First's section runs past its subheading to the next level-1 heading.
    assert_eq!(
        render(&ins(&s, &d, "First", true, "new").unwrap()),
        "# First\n\nf1\n\n## Sub\n\ns1\n\nnew\n\n# Second\n\nb1"
    );
    // And the last section runs to the end.
    assert_eq!(
        render(&ins(&s, &d, "Second", true, "new").unwrap()),
        "# First\n\nf1\n\n## Sub\n\ns1\n\n# Second\n\nb1\n\nnew"
    );
    assert!(ins(&s, &d, "f1", true, "x").unwrap_err().to_string().contains("No heading"));
}

#[test]
fn replace_section_keeps_the_heading_and_stops_at_its_level() {
    let s = schema();
    let d = doc("# A\n\na1\n\n## A.1\n\na11\n\n# B\n\nb1");
    assert_eq!(render(&sect(&s, &d, "A.1", "fresh").unwrap()), "# A\n\na1\n\n## A.1\n\nfresh\n\n# B\n\nb1");
    assert_eq!(render(&sect(&s, &d, "B", "- x\n- y").unwrap()), "# A\n\na1\n\n## A.1\n\na11\n\n# B\n\n- x\n- y");
}

#[test]
fn a_section_is_named_by_its_text_or_its_heading_path() {
    let s = schema();
    let d = doc("# Design\n\nd\n\n## The county's edges\n\nedges\n\n## Roads\n\nr\n\n# History\n\n## The county's edges\n\nold edges\n\n## The county's edges, revisited\n\nnew");
    // Several headings read the same: the refusal lists their paths.
    let err = sect(&s, &d, "The county's edges", "x").unwrap_err().to_string();
    assert!(err.contains("\"Design > The county's edges\"") && err.contains("\"History > The county's edges\""), "{err}");
    // A path names one, with "/" or " > ", in any case and punctuation.
    for name in ["Design/The county's edges", "Design > The county's edges", "design / the countys edges"] {
        let h = find_heading(&d, name).unwrap();
        assert_eq!(h.path, vec!["Design".to_string(), "The county's edges".to_string()], "{name}");
    }
    // An exact match beats one that only contains it.
    assert_eq!(find_heading(&d, "History/The county's edges").unwrap().index, 7);
    assert_eq!(render(&sect(&s, &d, "Design/The county's edges", "fresh").unwrap()).matches("fresh").count(), 1);
    // A unique part of a heading still names it, as before.
    assert_eq!(find_heading(&d, "revisited").unwrap().text, "The county's edges, revisited");
    assert!(find_heading(&d, "Design/Nowhere").unwrap_err().to_string().contains("No heading"));
    assert_eq!(loose("The County’s Edges!"), "the countys edges");
}

#[test]
fn cutting_sections_keeps_the_rest_and_puts_links_where_they_were() {
    let s = schema();
    let d = doc("intro\n\n# A\n\na1\n\n## A.1\n\na11\n\n# B\n\nb1");
    assert_eq!(render(&cut_sections(&s, &d, 1, Some("- [A](pimble:x/y)")).unwrap()), "intro\n\n- [A](pimble:x/y)");
    assert_eq!(render(&cut_sections(&s, &d, 2, None).unwrap()), "intro\n\n# A\n\na1\n\n# B\n\nb1");
    assert!(cut_sections(&s, &d, 3, None).is_err());
}

#[test]
fn a_quote_must_name_exactly_one_place() {
    let s = schema();
    let d = doc("apple pie\n\napple tart");
    assert!(text(&s, &d, "apple", "pear").unwrap_err().to_string().contains("appears 2 times"));
    assert!(text(&s, &d, "plum", "pear").unwrap_err().to_string().contains("No paragraph"));
    assert!(sect(&s, &d, "apple pie", "x").unwrap_err().to_string().contains("No heading"));
}

#[test]
fn replace_text_keeps_marks_outside_the_quote() {
    let s = schema();
    let d = doc("plain **bold words here** end");
    let after = text(&s, &d, "words", "terms").unwrap();
    assert_eq!(render(&after), "plain **bold terms here** end");
    // The quote spans the bold; only "plain" differs, so the bold stays bold.
    let after = text(&s, &d, "plain bo", "Plainly bo").unwrap();
    assert_eq!(render(&after), "Plainly **bold words here** end");
    // Text that differs across a mark boundary takes the first replaced char's marks.
    let after = text(&s, &d, "plain bold", "a new").unwrap();
    assert_eq!(render(&after), "a new **words here** end");
    let after = text(&s, &d, "here** end", "x").map(|d| render(&d));
    assert!(after.is_err(), "markdown syntax is not in the text");
    // The space before "here" is bold in the document; Markdown writes whitespace
    // at a mark's edge outside it, and a paragraph's trailing space not at all.
    // (Deleting "here end" by itself is too short a quote to delete by: it goes
    // with the text before it given back.)
    let after = text(&s, &d, "s here end", "s ").unwrap();
    assert_eq!(render(&after), "plain **bold words**");
}

#[test]
fn replace_text_reaches_inside_lists() {
    let s = schema();
    let d = doc("- milk\n- eggs\n  - brown");
    assert_eq!(render(&text(&s, &d, "brown", "white").unwrap()), render(&doc("- milk\n- eggs\n  - white")));
    assert!(same_text(&text(&s, &d, "brown", "brown").unwrap(), &d));
}

#[test]
fn replace_block_re_levels_or_removes_one_block() {
    let s = schema();
    let d = doc("# Title\n\nintro\n\n## Old heading\n\nbody");
    assert_eq!(render(&block(&s, &d, "Old heading", "### New heading").unwrap()), "# Title\n\nintro\n\n### New heading\n\nbody");
    // Removing goes by its number, or a quote long enough to be sure of.
    assert_eq!(render(&replace_block(&s, &d, BlockTarget::index(2), "").unwrap().0), "# Title\n\nintro\n\nbody");
    let d = doc("# Title\n\nintro\n\n## The old heading\n\nbody");
    assert_eq!(render(&block(&s, &d, "The old heading", "").unwrap()), "# Title\n\nintro\n\nbody");
    assert_eq!(render(&block(&s, &d, "intro", "- a\n- b").unwrap()), "# Title\n\n- a\n- b\n\n## The old heading\n\nbody");
}

#[test]
fn remove_section_takes_the_heading_and_its_body() {
    let s = schema();
    let d = doc("# A\n\na1\n\n## A.x\n\nax\n\n# B\n\nb1");
    assert_eq!(render(&rmsec(&s, &d, "A.x").unwrap()), "# A\n\na1\n\n# B\n\nb1");
    assert_eq!(render(&rmsec(&s, &d, "B").unwrap()), "# A\n\na1\n\n## A.x\n\nax");
    let only = doc("# Only\n\ntext");
    assert!(is_blank(&rmsec(&s, &only, "Only").unwrap()));
}

#[test]
fn replace_content_keeps_unchanged_blocks_as_the_same_nodes() {
    let s = schema();
    let d = doc("one\n\ntwo\n\nthree\n\nfour");
    let steps = content(&s, &d, "one\n\n2\n\nthree\n\nfour\n\nfive").unwrap();
    let after = steps.last().unwrap();
    assert_eq!(render(after), "one\n\n2\n\nthree\n\nfour\n\nfive");
    assert!(after.child(0).same_ref(d.child(0)));
    assert!(after.child(2).same_ref(d.child(2)));
    assert!(after.child(3).same_ref(d.child(3)));
    assert_eq!(steps.len(), 2, "two separate stretches changed");
    // Reordering, deleting everything, and the same content.
    let swapped = content(&s, &d, "four\n\none\n\ntwo\n\nthree").unwrap();
    assert_eq!(render(swapped.last().unwrap()), "four\n\none\n\ntwo\n\nthree");
    assert!(is_blank(content(&s, &d, "").unwrap().last().unwrap()));
    assert!(content(&s, &d, "one\n\ntwo\n\nthree\n\nfour").unwrap().is_empty());
}

fn same_text(a: &Node, b: &Node) -> bool {
    render(a) == render(b)
}

#[test]
fn edits_leave_unnamed_blocks_the_same_nodes() {
    let s = schema();
    let d = doc("one\n\ntwo\n\nthree");
    let after = text(&s, &d, "two", "2").unwrap();
    assert!(after.child(0).same_ref(d.child(0)));
    assert!(!after.child(1).same_ref(d.child(1)));
    assert!(after.child(2).same_ref(d.child(2)));
}
