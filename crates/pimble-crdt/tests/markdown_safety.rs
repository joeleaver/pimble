//! The block edits cannot silently destroy content (docs/MCP_CONTRACT.md "Naming
//! what an edit touches", "What an edit says it did", "Dry runs"): a short or
//! number-like quote removes nothing, a block can be named by its number, a
//! quote that appears several times is refused with every place listed unless
//! `occurrence` picks one, every edit says what it took out, and a preview says
//! exactly that while writing nothing.

use pimble_crdt::markdown::{BlockTarget, Echo, HeadingTarget, InsertAt, MarkdownEdit, Narrowing, Occurrence, Quote, EXCERPT};
use pimble_crdt::NodeDoc;

fn note(md: &str) -> NodeDoc {
    let mut doc = NodeDoc::new();
    doc.init("document", "Note", None, "2026-11-23T00:00:00Z").unwrap();
    doc.write_markdown(md).unwrap();
    doc
}

fn refusal(doc: &mut NodeDoc, edit: MarkdownEdit<'_>) -> String {
    let before = doc.save();
    let err = doc.edit_markdown(edit).unwrap_err().to_string();
    assert_eq!(doc.save(), before, "a refusal writes nothing");
    err
}

fn remove(block: BlockTarget<'_>) -> MarkdownEdit<'_> {
    MarkdownEdit::ReplaceBlock { block, markdown: "" }
}

/// A block named by a quote as replace_block and insert_after name one: with
/// `occurrence` and `block_index` on offer.
fn quoted(quote: &str, occurrence: Option<Occurrence>) -> BlockTarget<'_> {
    BlockTarget::Quote(Quote { text: quote, occurrence, narrowing: Narrowing::OccurrenceOrBlockIndex })
}

fn by_quote(quote: &str) -> BlockTarget<'_> {
    quoted(quote, None)
}

/// A quote as replace_text names one: with `occurrence` on offer.
fn text_quote(quote: &str, occurrence: Option<Occurrence>) -> Quote<'_> {
    Quote { text: quote, occurrence, narrowing: Narrowing::Occurrence }
}

/// Blocks 0-4; block 2 holds "13" and block 4 is the thirteenth line's words.
const LIST: &str = "# Shopping\n\nApples for the pie.\n\nWe need 13 eggs for the cake.\n\nFlour, the strong kind.\n\nAsk about block 13 at the shop.";

/// The incident of 2026-11-23: "13" meant block 13 and removed the paragraph
/// holding "13". Now it removes nothing and says what it would have removed.
#[test]
fn a_number_like_quote_removes_nothing_and_says_what_it_would_have() {
    let mut doc = note(LIST);
    let err = refusal(&mut doc, remove(by_quote("13")));
    assert!(err.contains("reads like a block number"), "{err}");
    assert!(err.contains("block 2") && err.contains("block 4"), "both blocks holding \"13\" are listed: {err}");
    assert!(err.contains("block_index"), "{err}");
    for quote in ["#13", "13.", "4-6"] {
        let err = refusal(&mut doc, remove(by_quote(quote)));
        assert!(err.contains("reads like a block number"), "{quote}: {err}");
    }
    // A block number that exists is offered.
    let mut short = note("one\n\ntwo\n\nthree");
    let err = refusal(&mut short, remove(by_quote("2")));
    assert!(err.contains("It names no block") && err.contains("If you meant block 2, give `block_index: 2`"), "{err}");
}

#[test]
fn a_short_quote_removes_nothing() {
    let mut doc = note(LIST);
    let err = refusal(&mut doc, remove(by_quote("Apples")));
    assert!(err.contains("shorter than 12 characters"), "{err}");
    assert!(err.contains("It would have removed block 1 (\"Apples for the pie.\")"), "{err}");
    assert!(err.contains("`block_index: 1`"), "{err}");
    // Replacing by a short quote is still fine, and so is removing by a long one.
    doc.edit_markdown(MarkdownEdit::ReplaceBlock { block: by_quote("Apples"), markdown: "Pears for the pie." }).unwrap();
    let (_, echo) = doc.edit_markdown(remove(by_quote("Flour, the strong"))).unwrap();
    assert_eq!(echo[0].from, 3);
    assert!(!doc.markdown().unwrap().contains("Flour"));
}

#[test]
fn a_block_is_named_by_its_number() {
    let mut doc = note(LIST);
    let (_, echo) = doc.edit_markdown(remove(BlockTarget::index(2))).unwrap();
    assert_eq!(
        echo,
        vec![Echo {
            from: 2,
            to: 3,
            old: "We need 13 eggs for the cake.".into(),
            before: Some("Apples for the pie.".into()),
            after: Some("Flour, the strong kind.".into()),
            in_block: false,
        }]
    );
    assert_eq!(doc.markdown().unwrap(), "# Shopping\n\nApples for the pie.\n\nFlour, the strong kind.\n\nAsk about block 13 at the shop.");
    let err = refusal(&mut doc, remove(BlockTarget::index(9)));
    assert_eq!(err, "This node has blocks 0 to 3, so there is no block 9.");
    // Inserting after a block by its number, and after a section by its heading's.
    doc.edit_markdown(MarkdownEdit::InsertAfter { at: InsertAt::Block(BlockTarget::index(1)), markdown: "Sugar." }).unwrap();
    let (_, echo) = doc.edit_markdown(MarkdownEdit::InsertAfter { at: InsertAt::Section(HeadingTarget::index(0)), markdown: "The end." }).unwrap();
    assert_eq!((echo[0].from, echo[0].to, echo[0].after.clone()), (5, 5, None), "after the last block");
    assert!(doc.markdown().unwrap().starts_with("# Shopping\n\nApples for the pie.\n\nSugar.\n\nFlour"));
    assert!(doc.markdown().unwrap().ends_with("at the shop.\n\nThe end."));
}

#[test]
fn a_section_is_named_by_its_headings_number() {
    let mut doc = note("intro\n\n# A\n\na1\n\n## A.1\n\na11\n\n# B\n\nb1");
    let err = refusal(&mut doc, MarkdownEdit::RemoveSection { heading: HeadingTarget::index(2) });
    assert!(err.starts_with("Block 2 is a paragraph, not a heading (\"a1\")"), "{err}");
    let (_, echo) = doc.edit_markdown(MarkdownEdit::RemoveSection { heading: HeadingTarget::index(1) }).unwrap();
    assert_eq!((echo[0].from, echo[0].to), (1, 5));
    assert_eq!(echo[0].old, "# A a1 ## A.1 a11");
    assert_eq!((echo[0].before.as_deref(), echo[0].after.as_deref()), (Some("intro"), Some("B")));
    assert_eq!(doc.markdown().unwrap(), "intro\n\n# B\n\nb1");
    let (_, echo) = doc.edit_markdown(MarkdownEdit::ReplaceSection { heading: HeadingTarget::index(1), markdown: "b2" }).unwrap();
    assert_eq!((echo[0].from, echo[0].to, echo[0].old.as_str()), (2, 3, "b1"));
}

/// Three paragraphs that read the same: no quote can tell them apart, so the
/// refusal lists all three and `occurrence` picks one.
#[test]
fn an_ambiguous_quote_lists_every_place_and_occurrence_picks_one() {
    let same = "The same words here.";
    let md = format!("# Notes\n\n{same}\n\n{same}\n\n{same}");
    let mut doc = note(&md);
    let err = refusal(&mut doc, remove(by_quote(same)));
    assert!(err.contains("appears 3 times"), "{err}");
    for (n, block) in [(1, 1), (2, 2), (3, 3)] {
        assert!(err.contains(&format!("({n}) block {block}: \"{same}\"")), "{err}");
    }
    assert!(err.contains("`occurrence` (1 to 3, or \"last\")") && err.contains("`block_index`"), "{err}");
    let err = refusal(&mut doc, remove(quoted(same, Some(Occurrence::Nth(4)))));
    assert!(err.contains("there is no occurrence 4"), "{err}");
    let err = refusal(&mut doc, remove(quoted(same, Some(Occurrence::Nth(0)))));
    assert!(err.contains("counts from 1"), "{err}");

    let edit = MarkdownEdit::ReplaceBlock { block: quoted(same, Some(Occurrence::Nth(2))), markdown: "The middle one." };
    let (_, echo) = doc.edit_markdown(edit).unwrap();
    assert_eq!(echo[0].from, 2);
    assert_eq!(doc.markdown().unwrap(), format!("# Notes\n\n{same}\n\nThe middle one.\n\n{same}"));
    let (_, echo) = doc.edit_markdown(remove(quoted(same, Some(Occurrence::Last)))).unwrap();
    assert_eq!(echo[0].from, 3);
    assert_eq!(doc.markdown().unwrap(), format!("# Notes\n\n{same}\n\nThe middle one."));
}

#[test]
fn replace_text_takes_an_occurrence_and_echoes_the_quote_in_place() {
    let mut doc = note("one fish, two fish, red fish");
    let err = refusal(&mut doc, MarkdownEdit::ReplaceText { quote: text_quote("fish", None), text: "cat" });
    assert!(err.contains("\"fish\" appears 3 times") && err.contains("(3) block 0: \"one fish, two fish, red fish\""), "{err}");
    let edit = MarkdownEdit::ReplaceText { quote: text_quote("fish", Some(Occurrence::Nth(2))), text: "cat" };
    let (_, echo) = doc.edit_markdown(edit).unwrap();
    assert_eq!(doc.markdown().unwrap(), "one fish, two cat, red fish");
    let e = &echo[0];
    assert_eq!((e.from, e.to, e.old.as_str(), e.in_block), (0, 1, "fish", true));
    assert_eq!((e.before.as_deref(), e.after.as_deref()), (Some("one fish, two "), Some(", red fish")));
}

#[test]
fn the_echo_keeps_the_first_characters_and_says_it_cut() {
    let long = "word ".repeat(60);
    let mut doc = note(&format!("short\n\n{}\n\nend", long.trim()));
    let (_, echo) = doc.edit_markdown(remove(BlockTarget::index(1))).unwrap();
    assert_eq!(echo[0].old.chars().count(), EXCERPT + 1);
    assert!(echo[0].old.ends_with('…') && echo[0].old.starts_with("word word"));
}

#[test]
fn replace_content_echoes_each_changed_stretch() {
    let mut doc = note("one\n\ntwo\n\nthree\n\nfour");
    let (_, echo) = doc.edit_markdown(MarkdownEdit::ReplaceContent { markdown: "one\n\n2\n\nthree" }).unwrap();
    let said: Vec<(usize, usize, &str)> = echo.iter().map(|e| (e.from, e.to, e.old.as_str())).collect();
    assert_eq!(said, vec![(1, 2, "two"), (3, 4, "four")]);
}

/// A dry run is the edit judged and not made: the same echo, the same
/// refusals, and the document untouched.
#[test]
fn a_dry_run_says_what_the_edit_would_do_and_writes_nothing() {
    let edits = [
        remove(BlockTarget::index(2)),
        MarkdownEdit::ReplaceText { quote: text_quote("strong", None), text: "weak" },
        MarkdownEdit::RemoveSection { heading: HeadingTarget::Name("Shopping") },
        MarkdownEdit::ReplaceSection { heading: HeadingTarget::Name("Shopping"), markdown: "nothing" },
        MarkdownEdit::ReplaceContent { markdown: "# Shopping\n\nbread" },
        MarkdownEdit::InsertAfter { at: InsertAt::Block(by_quote("Flour")), markdown: "Salt." },
    ];
    for edit in edits {
        let doc = note(LIST);
        let before = doc.save();
        let preview = doc.preview_markdown(edit).unwrap();
        assert_eq!(doc.save(), before, "{edit:?} wrote");
        let mut made = NodeDoc::load(&before).unwrap();
        let (_, echo) = made.edit_markdown(edit).unwrap();
        assert_eq!(preview, echo, "{edit:?}");
    }
    let doc = note(LIST);
    let err = doc.preview_markdown(remove(by_quote("13"))).unwrap_err().to_string();
    assert!(err.contains("reads like a block number"), "{err}");
    let same = MarkdownEdit::ReplaceText { quote: text_quote("strong", None), text: "strong" };
    assert!(doc.preview_markdown(same).unwrap_err().to_string().contains("would change nothing"));
}
