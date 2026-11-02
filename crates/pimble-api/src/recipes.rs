//! What a new node of each kind starts as (docs/API_CONTRACT.md decision 7):
//! its type, icon, `explicit_title` and first content, made into one
//! [`NodeInit`] so it is written in the `createNode` that makes it. A node
//! made in two writes (create, then edit) shows half-made to everyone in
//! between, and two devices filling one node's first content would each
//! fill it. Plugin node types will register recipes the same way.

use pimble_core::{custom_keys, node_types};
use pimble_crdt::NodeDoc;
use pimble_discussion::{Anchor, Author, Body, Discussion, Stamp};

use crate::backend::NodeInit;
use crate::ApiError;

/// The icon a new discussion starts with, attached to a node or a node of
/// its own.
pub const DISCUSSION_ICON: &str = "messages";

fn explicit() -> (String, serde_json::Value) {
    (custom_keys::EXPLICIT_TITLE.to_string(), serde_json::json!(true))
}

fn icon(name: &str) -> (String, serde_json::Value) {
    (custom_keys::ICON.to_string(), serde_json::json!(name))
}

/// A text document titled `title`, its content `markdown` when given
/// (checked first: what Markdown Pimble cannot hold is refused, and nothing
/// is made).
pub fn document(title: &str, markdown: Option<&str>) -> Result<NodeInit, ApiError> {
    let content = match markdown.filter(|md| !md.trim().is_empty()) {
        Some(md) => {
            pimble_crdt::markdown::check(md)?;
            let mut doc = NodeDoc::new();
            doc.write_markdown(md)?;
            Some(doc.save())
        }
        None => None,
    };
    Ok(NodeInit { node_type: node_types::DOCUMENT.to_string(), title: title.to_string(), content, custom: vec![explicit()] })
}

/// A sheet node: `content` its tables (a caller that built them), or the
/// first table every new sheet has (`pimble_sheet::ops::new_sheet_content`).
pub fn sheet(title: &str, content: Option<Vec<u8>>) -> Result<NodeInit, ApiError> {
    let content = match content {
        Some(c) => c,
        None => pimble_sheet::ops::new_sheet_content()?,
    };
    Ok(NodeInit {
        node_type: node_types::SHEET.to_string(),
        title: title.to_string(),
        content: Some(content),
        custom: vec![icon(pimble_sheet::ops::SHEET_ICON), explicit()],
    })
}

/// A canvas node whose board `content` was made with
/// `BoardTxn::start_canvas` (and whatever was drawn on it at once), its
/// icon saying whether it started from a picture.
pub fn canvas(title: &str, content: Vec<u8>, from_picture: bool) -> NodeInit {
    NodeInit {
        node_type: node_types::BOARD.to_string(),
        title: title.to_string(),
        content: Some(content),
        custom: vec![icon(pimble_board::recipe::canvas_icon(from_picture)), explicit()],
    }
}

/// The first message of a discussion: who wrote it, when, and its body.
pub struct FirstMessage<'a> {
    pub author: Option<&'a Author>,
    pub at: &'a str,
    pub body: &'a Body,
}

/// A discussion, attached to a node or a node of its own (the same recipe:
/// which it is, is where it is created), anchored at a place in its host's
/// text when `anchor` says, with its first message when given.
pub fn discussion(topic: &str, anchor: Option<&Anchor>, first: Option<FirstMessage<'_>>) -> Result<NodeInit, ApiError> {
    let mut content = pimble_discussion::new_content(anchor);
    if let Some(first) = first {
        // The containers and the first message, written in the create.
        let seed = Discussion::standalone();
        {
            use yrs::updates::decoder::Decode;
            use yrs::Transact;
            let update = yrs::Update::decode_v1(&content).map_err(|e| ApiError::failed(e.to_string()))?;
            seed.doc().transact_mut().apply_update(update).map_err(|e| ApiError::failed(e.to_string()))?;
        }
        seed.edit(|t| t.post(Stamp::new(first.author, first.at), None, first.body)).result?;
        content = {
            use yrs::{ReadTxn, StateVector, Transact};
            seed.doc().transact().encode_state_as_update_v1(&StateVector::default())
        };
    }
    Ok(NodeInit {
        node_type: node_types::DISCUSSION.to_string(),
        title: topic.to_string(),
        content: Some(content),
        custom: vec![icon(DISCUSSION_ICON), explicit()],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_is_its_content_and_refuses_what_markdown_cannot_hold() {
        let init = document("Plans", Some("# Plans\n\nFirst")).unwrap();
        let doc = NodeDoc::load(init.content.as_deref().unwrap()).unwrap();
        assert_eq!(doc.textblock_texts(), vec!["Plans".to_string(), "First".to_string()]);
        assert!(init.custom.iter().any(|(k, _)| k == custom_keys::EXPLICIT_TITLE));
        assert!(document("Empty", Some("  ")).unwrap().content.is_none());
        assert!(matches!(document("Tasks", Some("- [ ] a task")), Err(ApiError::Refused { .. })));
    }

    #[test]
    fn a_discussion_carries_its_icon_and_first_message() {
        let body = Body::from_markdown("Hello");
        let init = discussion("Topic", None, Some(FirstMessage { author: None, at: "2026-11-02T00:00:00.000Z", body: &body })).unwrap();
        assert!(init.custom.contains(&icon(DISCUSSION_ICON)));
        let mut doc = NodeDoc::new();
        doc.apply_update(init.content.as_deref().unwrap()).unwrap();
        assert_eq!(Discussion::of_node(&doc).read().live_count(), 1);
    }
}
