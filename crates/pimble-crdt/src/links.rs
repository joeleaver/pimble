//! Links in a node's content, and deep-link anchors into it
//! (docs/LINKS_CONTRACT.md).
//!
//! A link is rinch's `link` mark with a `pimble:` href, so what a node links to
//! is read out of its content ([`links_of_model`]); nothing stores it twice.
//!
//! An anchor is a spot in the content: a yrs `StickyIndex` into one textblock's
//! `text`, plus a quote of the text that follows. Both sides of the wire shape
//! (`rinch-editor-collab`'s projection, see its module doc) are read with yrs
//! directly here, so an anchor made by the editor and one made here are the
//! same bytes: a plain `StickyIndex`, v1-encoded.

use pimble_core::{Anchor, PimbleUrl};
use rinch_editor_core::Node as ModelNode;
use yrs::types::text::TextRef;
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Array, ArrayRef, Assoc, Doc, GetString, IndexedSequence, Map, MapRef, Out, ReadTxn, StickyIndex, Transact};

/// A link found in a node's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRef {
    /// Where it points.
    pub target: PimbleUrl,
    /// The top-level block it sits in, as `"b:{ordinal}"` (the same locator as
    /// the node's index units).
    pub path: String,
    /// The linked words.
    pub text: String,
}

/// A spot in a node's content: the `textblock`-th textblock in document order
/// (depth first, so a list item's paragraphs count where they appear) and a
/// character offset into its text. Inline atoms (an image, a hard break) are
/// one character, as they are in the editor's model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spot {
    pub textblock: usize,
    pub offset: usize,
}

/// How an anchor was found again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolved {
    /// By its sticky index: the spot moved with the text around it.
    Sticky(Spot),
    /// By its quote, the sticky index no longer resolving (its block was
    /// deleted, or the content has a new history since a transplant).
    Quote(Spot),
}

impl Resolved {
    pub fn spot(self) -> Spot {
        match self {
            Resolved::Sticky(spot) | Resolved::Quote(spot) => spot,
        }
    }
}

const LINK_MARK: &str = "link";
const HREF: &str = "href";
const TEXT: &str = "text";
const CONTENT: &str = "content";

/// Every `pimble:` link in `doc` (the editor model of a node's content), in
/// document order. Adjacent text runs carrying the same href (a link whose
/// words are partly bold) are one link. External links are not returned.
pub(crate) fn links_of_model(doc: &ModelNode) -> Vec<LinkRef> {
    let mut out = Vec::new();
    for (ordinal, block) in doc.content().iter().enumerate() {
        let path = format!("b:{ordinal}");
        // (href, words) of the run being read; ended by any other text.
        let mut open: Option<(String, String)> = None;
        collect_links(block, &path, &mut open, &mut out);
        flush(&path, &mut open, &mut out);
    }
    out
}

fn collect_links(node: &ModelNode, path: &str, open: &mut Option<(String, String)>, out: &mut Vec<LinkRef>) {
    if let Some(text) = node.text() {
        let href = node
            .marks()
            .iter()
            .find(|m| m.type_name() == LINK_MARK)
            .and_then(|m| m.attrs.get_str(HREF))
            .filter(|href| PimbleUrl::parse(href).is_some());
        match (href, open.as_mut()) {
            (Some(href), Some((current, words))) if current == href => words.push_str(text),
            (Some(href), _) => {
                flush(path, open, out);
                *open = Some((href.to_string(), text.to_string()));
            }
            (None, _) => flush(path, open, out),
        }
        return;
    }
    if node.is_textblock() {
        // A link never runs from one textblock into the next.
        flush(path, open, out);
    }
    for child in node.content().iter() {
        collect_links(child, path, open, out);
    }
    if node.is_textblock() {
        flush(path, open, out);
    }
}

fn flush(path: &str, open: &mut Option<(String, String)>, out: &mut Vec<LinkRef>) {
    if let Some((href, text)) = open.take() {
        if let Some(target) = PimbleUrl::parse(&href) {
            out.push(LinkRef { target, path: path.to_string(), text });
        }
    }
}

/// The `text` of every textblock under the `content` root, in document order.
fn textblocks<T: ReadTxn>(txn: &T, content: &ArrayRef) -> Vec<TextRef> {
    let mut out = Vec::new();
    walk(txn, content, &mut out);
    out
}

fn walk<T: ReadTxn>(txn: &T, list: &ArrayRef, out: &mut Vec<TextRef>) {
    for item in list.iter(txn) {
        let Out::YMap(block) = item else { continue };
        match block_child(txn, &block) {
            Some(Out::YText(text)) => out.push(text),
            Some(Out::YArray(children)) => walk(txn, &children, out),
            _ => {}
        }
    }
}

fn block_child<T: ReadTxn>(txn: &T, block: &MapRef) -> Option<Out> {
    block.get(txn, TEXT).or_else(|| block.get(txn, CONTENT))
}

/// UTF-16 offset of character `chars` in `s`, clamped to its end.
fn utf16_of(s: &str, chars: usize) -> u32 {
    s.chars().take(chars).map(|c| c.len_utf16() as u32).sum()
}

/// Character offset of UTF-16 offset `units` in `s`, clamped to its end.
fn chars_of(s: &str, units: u32) -> usize {
    let mut seen = 0u32;
    let mut chars = 0;
    for c in s.chars() {
        if seen >= units {
            break;
        }
        seen += c.len_utf16() as u32;
        chars += 1;
    }
    chars
}

/// An anchor at `spot` in `doc`'s content (`content` its root array): the
/// sticky index of that spot and the text that follows it in its textblock.
/// `None` when the document has no such textblock.
pub(crate) fn anchor_at(doc: &Doc, content: &ArrayRef, spot: Spot) -> Option<Anchor> {
    let txn = doc.transact();
    let text = textblocks(&txn, content).into_iter().nth(spot.textblock)?;
    let string = text.get_string(&txn);
    let offset = spot.offset.min(string.chars().count());
    let index = utf16_of(&string, offset);
    // Stick to the character after the spot; at the end of the text, to the
    // one before it (and to the text itself when it is empty).
    let sticky = text
        .sticky_index(&txn, index, Assoc::After)
        .or_else(|| text.sticky_index(&txn, index, Assoc::Before))
        .map(|s| s.encode_v1());
    let after: String = string.chars().skip(offset).collect();
    Some(Anchor::new(sticky, &after))
}

/// Where `anchor` is in `doc`'s content now.
///
/// The sticky index when the quote still reads there (or there is no quote).
/// Otherwise the first place the quote reads: the characters the sticky index
/// named were deleted, and the words were put somewhere else (a paragraph cut
/// and pasted is a delete and an insert, and yrs resolves a deleted character
/// to where it was). Otherwise the sticky index, whose words were edited in
/// place. `None` when neither finds anything.
pub(crate) fn resolve_anchor(doc: &Doc, content: &ArrayRef, anchor: &Anchor) -> Option<Resolved> {
    let txn = doc.transact();
    let blocks = textblocks(&txn, content);
    let texts: Vec<String> = blocks.iter().map(|text| text.get_string(&txn)).collect();
    let sticky = anchor.sticky.as_deref().and_then(|bytes| resolve_sticky(&txn, &blocks, &texts, bytes));
    let reads_at = |spot: &Spot| {
        let after: String = texts[spot.textblock].chars().skip(spot.offset).collect();
        after.starts_with(&anchor.quote)
    };
    if let Some(spot) = sticky.filter(reads_at) {
        return Some(Resolved::Sticky(spot));
    }
    let quoted = (!anchor.quote.is_empty())
        .then(|| {
            texts.iter().enumerate().find_map(|(textblock, string)| {
                let at = string.find(&anchor.quote)?;
                Some(Spot { textblock, offset: string[..at].chars().count() })
            })
        })
        .flatten();
    quoted.map(Resolved::Quote).or(sticky.map(Resolved::Sticky))
}

fn resolve_sticky<T: ReadTxn>(txn: &T, blocks: &[TextRef], texts: &[String], bytes: &[u8]) -> Option<Spot> {
    let sticky = StickyIndex::decode_v1(bytes).ok()?;
    let offset = sticky.get_offset(txn)?;
    // Only a textblock that is still in the document counts: a deleted
    // block's text keeps its branch, and yrs resolves into it happily.
    let textblock = blocks.iter().position(|text| {
        let branch: &yrs::branch::Branch = text.as_ref();
        yrs::branch::BranchPtr::from(branch) == offset.branch
    })?;
    Some(Spot { textblock, offset: chars_of(&texts[textblock], offset.index) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::{Block, ListItem, Mark, Run};
    use crate::node_doc::NodeDoc;
    use pimble_core::{NodeId, StoreId};

    fn url() -> PimbleUrl {
        PimbleUrl::node(StoreId::new(), NodeId::new())
    }

    fn link(text: &str, to: &PimbleUrl) -> Run {
        Run::marked(text, vec![Mark::Link { href: to.to_string() }])
    }

    #[test]
    fn links_are_read_from_the_link_marks_in_the_text() {
        let (a, b) = (url(), url());
        let doc = NodeDoc::from_blocks(&[
            Block::paragraph(vec![
                Run::plain("see "),
                link("the plan", &a),
                // The same link continuing in bold is still one link.
                Run::marked(" and more", vec![Mark::Link { href: a.to_string() }, Mark::Bold]),
                Run::plain(", or "),
                Run::marked("the web", vec![Mark::Link { href: "https://example.com".into() }]),
            ]),
            Block::plain("nothing here"),
            Block::BulletList { items: vec![ListItem { blocks: vec![Block::paragraph(vec![link("b", &b), link("a again", &a)])] }] },
        ])
        .unwrap();

        let links = doc.links();
        assert_eq!(
            links,
            vec![
                LinkRef { target: a.clone(), path: "b:0".into(), text: "the plan and more".into() },
                LinkRef { target: b.clone(), path: "b:2".into(), text: "b".into() },
                LinkRef { target: a.clone(), path: "b:2".into(), text: "a again".into() },
            ],
            "external links are not returned; adjacent runs of one href are one link"
        );
        assert_eq!(NodeDoc::links_of(&doc.save()), links);
        assert!(NodeDoc::new().links().is_empty(), "no projection, no links");
    }

    #[test]
    fn a_link_in_one_paragraph_does_not_run_into_the_next() {
        let a = url();
        let doc = NodeDoc::from_blocks(&[Block::paragraph(vec![link("one", &a)]), Block::paragraph(vec![link("two", &a)])]).unwrap();
        let texts: Vec<String> = doc.links().into_iter().map(|l| l.text).collect();
        assert_eq!(texts, ["one", "two"]);
    }

    #[test]
    fn an_anchor_moves_with_its_text_through_edits_before_and_after_it() {
        let mut doc = NodeDoc::from_blocks(&[Block::plain("first"), Block::plain("hello brave new world")]).unwrap();
        let spot = Spot { textblock: 1, offset: 12 };
        let anchor = doc.anchor_at(spot).unwrap();
        assert_eq!(anchor.quote, "new world");
        assert!(anchor.sticky.is_some());
        assert_eq!(doc.resolve_anchor(&anchor), Some(Resolved::Sticky(spot)));

        // Typing before it and after it in its own paragraph, one edit at a
        // time as a person types (one replacement changing both ends would
        // rewrite the whole paragraph: see the next test).
        doc.replace_plain_text("first\noh hello brave new world").unwrap();
        doc.replace_plain_text("first\noh hello brave new world, today").unwrap();
        let moved = Spot { textblock: 1, offset: 15 };
        assert_eq!(doc.resolve_anchor(&anchor), Some(Resolved::Sticky(moved)));

        // Through the URL and back, as a link carries it.
        let (store, node) = (StoreId::new(), NodeId::new());
        let carried = PimbleUrl::parse(&PimbleUrl::deep(store, node, anchor.clone()).to_string()).unwrap();
        assert_eq!(doc.resolve_anchor(carried.anchor.as_ref().unwrap()), Some(Resolved::Sticky(moved)));
    }

    #[test]
    fn an_anchor_whose_words_moved_elsewhere_follows_the_quote() {
        let mut doc = NodeDoc::from_blocks(&[Block::plain("first"), Block::plain("hello brave new world")]).unwrap();
        let anchor = doc.anchor_at(Spot { textblock: 1, offset: 12 }).unwrap();
        // The paragraph's characters deleted and the words written again
        // further down: the sticky index lands where they were, the quote
        // where they are.
        doc.replace_plain_text("a new first line\nfirst\noh hello brave new world, today").unwrap();
        assert_eq!(doc.resolve_anchor(&anchor), Some(Resolved::Quote(Spot { textblock: 2, offset: 15 })));
    }

    #[test]
    fn an_anchor_whose_words_were_edited_in_place_keeps_its_spot() {
        let mut doc = NodeDoc::from_blocks(&[Block::plain("hello brave new world")]).unwrap();
        let anchor = doc.anchor_at(Spot { textblock: 0, offset: 12 }).unwrap();
        doc.replace_plain_text("hello brave old world").unwrap();
        assert_eq!(doc.resolve_anchor(&anchor), Some(Resolved::Sticky(Spot { textblock: 0, offset: 12 })));
    }

    #[test]
    fn an_anchor_counts_characters_not_utf16_units() {
        let doc = NodeDoc::from_blocks(&[Block::plain("🎉🎉 party")]).unwrap();
        let spot = Spot { textblock: 0, offset: 3 };
        let anchor = doc.anchor_at(spot).unwrap();
        assert_eq!(anchor.quote, "party");
        assert_eq!(doc.resolve_anchor(&anchor), Some(Resolved::Sticky(spot)));
    }

    #[test]
    fn an_anchor_at_the_end_of_a_text_or_in_an_empty_one_resolves() {
        let doc = NodeDoc::from_blocks(&[Block::plain("end"), Block::plain("")]).unwrap();
        for spot in [Spot { textblock: 0, offset: 3 }, Spot { textblock: 1, offset: 0 }] {
            let anchor = doc.anchor_at(spot).unwrap();
            assert_eq!(doc.resolve_anchor(&anchor), Some(Resolved::Sticky(spot)), "{spot:?}");
        }
        assert_eq!(doc.anchor_at(Spot { textblock: 2, offset: 0 }), None, "no such textblock");
    }

    #[test]
    fn a_list_items_paragraph_is_a_textblock_in_document_order() {
        let doc = NodeDoc::from_blocks(&[
            Block::plain("before"),
            Block::BulletList { items: vec![ListItem { blocks: vec![Block::plain("one")] }, ListItem { blocks: vec![Block::plain("two")] }] },
            Block::plain("after"),
        ])
        .unwrap();
        assert_eq!(doc.anchor_at(Spot { textblock: 2, offset: 0 }).unwrap().quote, "two");
        assert_eq!(doc.anchor_at(Spot { textblock: 3, offset: 1 }).unwrap().quote, "fter");
    }

    #[test]
    fn a_transplanted_copy_is_found_by_the_quote() {
        let doc = NodeDoc::from_blocks(&[Block::plain("intro"), Block::plain("the part that matters")]).unwrap();
        let anchor = doc.anchor_at(Spot { textblock: 1, offset: 4 }).unwrap();
        // A transplant's content is a new history: the sticky index names
        // nothing in it.
        let copy = NodeDoc::load(&doc.fresh_content().unwrap().unwrap()).unwrap();
        assert_eq!(copy.resolve_anchor(&anchor), Some(Resolved::Quote(Spot { textblock: 1, offset: 4 })));
    }

    #[test]
    fn an_anchor_whose_text_is_gone_resolves_to_nothing() {
        let doc = NodeDoc::from_blocks(&[Block::plain("keep"), Block::plain("vanishing words")]).unwrap();
        let anchor = doc.anchor_at(Spot { textblock: 1, offset: 0 }).unwrap();
        let other = NodeDoc::from_blocks(&[Block::plain("keep")]).unwrap();
        assert_eq!(other.resolve_anchor(&anchor), None);
        assert_eq!(other.resolve_anchor(&Anchor { sticky: Some(vec![1, 2, 3]), quote: String::new() }), None, "garbage bytes");
    }
}
