//! Naming what an edit removes surely (docs/MCP_CONTRACT.md "Naming what an
//! edit touches", the review of 2026-11-24): a heading removed or emptied by a
//! short part of its text, a quote twice in one block, overlapping places, a
//! block number read before the note changed (`expect`), text deleted by a
//! short quote, and refusals that advise only what the caller takes.

use pimble_crdt::markdown::{BlockTarget, HeadingTarget, InsertAt, MarkdownEdit, Narrowing, Occurrence, Quote};
use pimble_crdt::NodeDoc;

fn note(md: &str) -> NodeDoc {
    let mut doc = NodeDoc::new();
    doc.init("document", "Note", None, "2026-11-24T00:00:00Z").unwrap();
    doc.write_markdown(md).unwrap();
    doc
}

fn refusal(doc: &mut NodeDoc, edit: MarkdownEdit<'_>) -> String {
    let before = doc.save();
    let err = doc.edit_markdown(edit).unwrap_err().to_string();
    assert_eq!(doc.save(), before, "a refusal writes nothing");
    err
}

/// A quote as replace_block and insert_after take one.
fn tool_quote(text: &str, occurrence: Option<Occurrence>) -> BlockTarget<'_> {
    BlockTarget::Quote(Quote { text, occurrence, narrowing: Narrowing::OccurrenceOrBlockIndex })
}

fn remove(block: BlockTarget<'_>) -> MarkdownEdit<'_> {
    MarkdownEdit::ReplaceBlock { block, markdown: "" }
}

const PLAN: &str = "# Plan\n\nintro\n\n## Phase 3\n\nbuild it\n\n## 3\n\nthe third\n\n# Budget 2026\n\nmoney";

#[test]
fn a_heading_named_by_a_short_part_of_its_text_is_not_removed() {
    let mut doc = note("# Plan\n\nintro\n\n## Phase 3\n\nbuild it\n\n# Notes\n\nmore");
    let err = refusal(&mut doc, MarkdownEdit::RemoveSection { heading: HeadingTarget::Name("3") });
    assert!(err.contains("reads like a block number") && err.contains("\"Phase 3\" (block 2)"), "{err}");
    assert!(err.contains("give the heading's whole text or its block number, 2"), "{err}");
    let err = refusal(&mut doc, MarkdownEdit::ReplaceSection { heading: HeadingTarget::Name("Phase"), markdown: "" });
    assert!(err.contains("shorter than 12 characters") && err.contains("to empty that section"), "{err}");
    // Replacing a section with something goes by any name; so does a long part.
    doc.edit_markdown(MarkdownEdit::ReplaceSection { heading: HeadingTarget::Name("3"), markdown: "rebuilt" }).unwrap();
    // The whole text, in any case, or its block number, is sure.
    doc.edit_markdown(MarkdownEdit::RemoveSection { heading: HeadingTarget::Name("phase 3") }).unwrap();
    assert_eq!(doc.markdown().unwrap(), "# Plan\n\nintro\n\n# Notes\n\nmore");
    doc.edit_markdown(MarkdownEdit::RemoveSection { heading: HeadingTarget::index(2) }).unwrap();
    assert_eq!(doc.markdown().unwrap(), "# Plan\n\nintro");
}

#[test]
fn a_heading_whose_whole_text_is_short_is_removed_by_it() {
    let mut doc = note(PLAN);
    // "3" is the whole text of one heading: the exact match wins and is sure.
    let (_, echo) = doc.edit_markdown(MarkdownEdit::RemoveSection { heading: HeadingTarget::Name("3") }).unwrap();
    assert_eq!((echo[0].from, echo[0].to), (4, 6));
    let mut doc = note(PLAN);
    doc.edit_markdown(MarkdownEdit::RemoveSection { heading: HeadingTarget::Name("Plan/3") }).unwrap();
    assert!(doc.markdown().unwrap().contains("Phase 3") && !doc.markdown().unwrap().contains("the third"));
    // A path whose last segment is only part of a heading's text is not sure.
    let mut doc = note(PLAN);
    let err = refusal(&mut doc, MarkdownEdit::ReplaceSection { heading: HeadingTarget::Name("Plan/Phase"), markdown: " " });
    assert!(err.contains("\"Phase 3\" (block 2)"), "{err}");
    let err = refusal(&mut doc, MarkdownEdit::RemoveSection { heading: HeadingTarget::Name("2026") });
    assert!(err.contains("\"Budget 2026\" (block 6)"), "{err}");
}

#[test]
fn a_quote_twice_in_one_block_is_ambiguous() {
    let md = "# List\n\n- eggs for the cake\n- more eggs for the cake\n\nend";
    let mut doc = note(md);
    let quote = "eggs for the cake";
    let err = refusal(&mut doc, remove(tool_quote(quote, None)));
    assert!(err.contains("appears 2 times"), "{err}");
    assert!(err.contains("(1) block 1: \"eggs for the cake\"") && err.contains("(2) block 1: \"more eggs for the cake\""), "{err}");
    assert_eq!(doc.markdown().unwrap(), md, "the list is still there");
    // `occurrence` picks one place, and the block it is in is the list.
    let (_, echo) = doc.edit_markdown(remove(tool_quote(quote, Some(Occurrence::Nth(2))))).unwrap();
    assert_eq!((echo[0].from, echo[0].to), (1, 2));
    // replace_text counts the same places.
    let mut doc = note(md);
    let text = |occurrence| MarkdownEdit::ReplaceText {
        quote: Quote { text: "eggs", occurrence, narrowing: Narrowing::Occurrence },
        text: "flour",
    };
    assert!(refusal(&mut doc, text(None)).contains("appears 2 times"));
    doc.edit_markdown(text(Some(Occurrence::Last))).unwrap();
    assert!(doc.markdown().unwrap().contains("- more flour for the cake"));
}

#[test]
fn overlapping_places_count_and_each_can_be_chosen() {
    let mut doc = note("aaa");
    let text = |occurrence| MarkdownEdit::ReplaceText {
        quote: Quote { text: "aa", occurrence, narrowing: Narrowing::Occurrence },
        text: "b",
    };
    let err = refusal(&mut doc, text(None));
    assert!(err.contains("\"aa\" appears 2 times") && err.contains("(1) block 0") && err.contains("(2) block 0"), "{err}");
    let mut first = note("aaa");
    first.edit_markdown(text(Some(Occurrence::Nth(1)))).unwrap();
    assert_eq!(first.markdown().unwrap(), "ba");
    doc.edit_markdown(text(Some(Occurrence::Nth(2)))).unwrap();
    assert_eq!(doc.markdown().unwrap(), "ab");
}

#[test]
fn expect_catches_a_block_number_read_before_the_note_changed() {
    let mut doc = note("# Shopping\n\nApples for the pie.\n\nFlour, the strong kind.");
    let stale = |expect| remove(BlockTarget::Index { index: 1, expect: Some(expect) });
    let err = refusal(&mut doc, stale("Flour"));
    assert!(err.starts_with("Nothing was changed: block 1 does not contain \"Flour\""), "{err}");
    assert!(err.contains("it is a paragraph that reads \"Apples for the pie.\"") && err.contains("outline or blocks"), "{err}");
    assert!(refusal(&mut doc, stale("")).contains("leave it out"));
    let (_, echo) = doc.edit_markdown(stale("Apples")).unwrap();
    assert_eq!(echo[0].old, "Apples for the pie.");
    // A heading by number checks the heading, before it judges anything else.
    let heading = |expect| MarkdownEdit::RemoveSection { heading: HeadingTarget::Index { index: 0, expect: Some(expect) } };
    assert!(refusal(&mut doc, heading("Groceries")).contains("block 0 does not contain \"Groceries\""));
    let insert = MarkdownEdit::InsertAfter {
        at: InsertAt::Section(HeadingTarget::Index { index: 0, expect: Some("Shop") }),
        markdown: "Salt.",
    };
    doc.edit_markdown(insert).unwrap();
    doc.edit_markdown(heading("Shopping")).unwrap();
    assert_eq!(doc.markdown().unwrap(), "");
}

#[test]
fn deleting_text_by_a_short_quote_is_refused_and_the_words_around_it_work() {
    let mut doc = note("We need 13 eggs for the cake.");
    let delete = |text| MarkdownEdit::ReplaceText { quote: Quote::alone(text), text: "" };
    let err = refusal(&mut doc, delete("13 "));
    assert!(err.contains("reads like a block number") && err.contains("block 0"), "{err}");
    assert!(err.contains("quote it with the words either side"), "{err}");
    assert!(refusal(&mut doc, delete("eggs ")).contains("shorter than 12 characters"));
    // The sure way deletes exactly the same.
    doc.replace_text("need 13 eggs", "need eggs").unwrap();
    assert_eq!(doc.markdown().unwrap(), "We need eggs for the cake.");
    // A long enough quote deletes by itself.
    doc.edit_markdown(delete(" for the cake")).unwrap();
    assert_eq!(doc.markdown().unwrap(), "We need eggs.");
}

#[test]
fn an_ambiguous_quote_is_refused_with_advice_the_caller_can_follow() {
    let md = "milk here\n\nmilk there";
    // A caller with nothing else to choose by (a sheet's `after_text`).
    let mut doc = note(md);
    let err = doc.insert_markdown_after("milk", false, "x").unwrap_err().to_string();
    assert!(err.contains("Quote more of the block so the quote appears once."), "{err}");
    assert!(!err.contains("occurrence") && !err.contains("block_index"), "{err}");
    // replace_text takes `occurrence` and no block number.
    let edit = MarkdownEdit::ReplaceText { quote: Quote { text: "milk", occurrence: None, narrowing: Narrowing::Occurrence }, text: "oat" };
    let err = refusal(&mut doc, edit);
    assert!(err.contains("`occurrence`") && !err.contains("block_index"), "{err}");
    // replace_block takes both.
    let err = refusal(&mut doc, MarkdownEdit::ReplaceBlock { block: tool_quote("milk", None), markdown: "x" });
    assert!(err.contains("`occurrence`") && err.contains("`block_index`"), "{err}");
    // A short quote removing a block, from a caller without block numbers:
    // the advice is a longer quote.
    let err = refusal(&mut doc, remove(BlockTarget::quote("milk here")));
    assert!(err.contains("quote at least 12 characters of it") && !err.contains("block_index"), "{err}");
}
