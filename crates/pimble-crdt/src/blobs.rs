//! The pictures a node's content names (docs/IMAGES_CONTRACT.md "On disk").
//!
//! A picture is rinch's `image` atom with a `pimble-blob:` `src`, so what a
//! node references is read out of its content ([`blob_refs_of_model`]), as its
//! links are; nothing stores it twice.

use pimble_core::BlobUrl;
use rinch_editor_core::Node as ModelNode;

/// A picture found in a node's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobRef {
    /// The blob that holds its bytes.
    pub url: BlobUrl,
    /// The image's `alt` text; empty when it has none.
    pub alt: String,
    /// The top-level block it sits in, as `"b:{ordinal}"` (the same locator as
    /// the node's index units and its links).
    pub path: String,
}

const IMAGE: &str = "image";
const SRC: &str = "src";
const ALT: &str = "alt";

/// Every image in `doc` (the editor model of a node's content) whose `src` is
/// a Pimble blob URL, in document order and at any depth (a list item, a
/// quote, a table cell). An image named twice is returned twice; images
/// loaded from anywhere else (`https:`) are not returned.
pub(crate) fn blob_refs_of_model(doc: &ModelNode) -> Vec<BlobRef> {
    let mut out = Vec::new();
    for (ordinal, block) in doc.content().iter().enumerate() {
        collect(block, &format!("b:{ordinal}"), &mut out);
    }
    out
}

fn collect(node: &ModelNode, path: &str, out: &mut Vec<BlobRef>) {
    if node.type_name() == IMAGE {
        if let Some(url) = node.attrs().get_str(SRC).and_then(BlobUrl::parse) {
            let alt = node.attrs().get_str(ALT).unwrap_or_default().to_string();
            out.push(BlobRef { url, alt, path: path.to_string() });
        }
        return;
    }
    for child in node.content().iter() {
        collect(child, path, out);
    }
}

#[cfg(test)]
mod tests {
    use pimble_core::{BlobId, StoreId};

    use crate::blocks::{Block, Image, Inline, ListItem, Mark, Run, TableCell, TableRow};
    use crate::NodeDoc;

    fn picture(url: &BlobUrl, alt: &str) -> Inline {
        Inline::Image(Image::new(url.to_string(), alt))
    }

    use super::*;

    #[test]
    fn blob_refs_finds_images_inside_a_list_a_quote_and_a_table_cell() {
        let store = StoreId::new();
        let [top, listed, nested, quoted, celled, linked] = std::array::from_fn(|_| BlobUrl::new(store, BlobId::new()));
        let doc = NodeDoc::from_blocks(&[
            Block::paragraph(vec![Inline::Text(Run::plain("look: ")), picture(&top, "a cat"), Inline::Text(Run::plain(" and on"))]),
            Block::plain("nothing here"),
            Block::BulletList {
                items: vec![ListItem {
                    blocks: vec![
                        Block::paragraph(vec![picture(&listed, "in a list")]),
                        Block::OrderedList { start: 1, items: vec![ListItem { blocks: vec![Block::paragraph(vec![picture(&nested, "")])] }] },
                    ],
                }],
            },
            Block::Blockquote { blocks: vec![Block::paragraph(vec![Inline::Text(Run::plain("said: ")), picture(&quoted, "in a quote")])] },
            Block::Table {
                rows: vec![
                    TableRow { cells: vec![TableCell::header(vec![Block::plain("name")]), TableCell::header(vec![Block::plain("picture")])] },
                    TableRow { cells: vec![TableCell::new(vec![Block::plain("cat")]), TableCell::new(vec![Block::paragraph(vec![picture(&celled, "in a cell")])])] },
                ],
            },
            Block::paragraph(vec![
                // Not a blob: loaded from the web, as rinch loads it.
                Inline::Image(Image::new("https://example.com/cat.png", "elsewhere")),
                // A linked picture is a picture; the same blob again is a second reference.
                Inline::Image(Image { marks: vec![Mark::Link { href: "https://example.com".into() }], ..Image::new(linked.to_string(), "linked") }),
                picture(&top, "a cat, again"),
            ]),
        ])
        .unwrap();

        let expected = vec![
            BlobRef { url: top, alt: "a cat".into(), path: "b:0".into() },
            BlobRef { url: listed, alt: "in a list".into(), path: "b:2".into() },
            BlobRef { url: nested, alt: String::new(), path: "b:2".into() },
            BlobRef { url: quoted, alt: "in a quote".into(), path: "b:3".into() },
            BlobRef { url: celled, alt: "in a cell".into(), path: "b:4".into() },
            BlobRef { url: linked, alt: "linked".into(), path: "b:5".into() },
            BlobRef { url: top, alt: "a cat, again".into(), path: "b:5".into() },
        ];
        assert_eq!(doc.blob_refs(), expected);
        // A replica loading the bytes reads the same references.
        assert_eq!(NodeDoc::blob_refs_of(&doc.save()), expected);
    }

    #[test]
    fn a_document_with_no_content_or_no_pictures_names_no_blobs() {
        assert!(NodeDoc::new().blob_refs().is_empty());
        assert!(NodeDoc::from_blocks(&[Block::plain("words")]).unwrap().blob_refs().is_empty());
        assert!(NodeDoc::blob_refs_of(b"not a document").is_empty());
    }

    #[test]
    fn a_deleted_picture_is_no_longer_named() {
        let url = BlobUrl::new(StoreId::new(), BlobId::new());
        let mut doc = NodeDoc::from_blocks(&[Block::paragraph(vec![picture(&url, "going")])]).unwrap();
        assert_eq!(doc.blob_refs().len(), 1);
        doc.replace_plain_text("only words now").unwrap();
        assert!(doc.blob_refs().is_empty());
    }
}
