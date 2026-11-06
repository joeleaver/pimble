//! Splitting a text node in two at the caret, and joining the node below into
//! one (docs/SPLIT_CONTRACT.md). Each is made of the API's ordinary writes (one
//! `createNode`, `applyEdit`s, moves, one delete), so every client sees them
//! as it sees any edit.

use pimble_core::{node_types, NodeId, PimbleUrl, Place};
use pimble_crdt::NodeDoc;

use crate::backend::NodeInit;
use crate::{Addr, ApiError, Pimble};

/// What a split made: the new node, and the updates this client's own views
/// of each node take (the server relays a write to everyone but its sender).
#[derive(Debug, Clone)]
pub struct Split {
    pub new: Addr,
    pub kept_delta: Option<Vec<u8>>,
}

/// What a join did: the node joined in (now deleted, its links leading to
/// the node it joined), and the update for this client's own views.
#[derive(Debug, Clone)]
pub struct Joined {
    pub gone: Addr,
    pub delta: Option<Vec<u8>>,
}

/// The parent of a text node that can be split or joined, or why it cannot.
fn text_parent(doc: &NodeDoc) -> Result<NodeId, ApiError> {
    let fields = doc.fields()?;
    if fields.node_type != node_types::DOCUMENT {
        return Err(ApiError::refused("Only a text note can be split or joined."));
    }
    if fields.deleted_at.is_some() {
        return Err(ApiError::refused("That note was deleted."));
    }
    fields.parent_id.ok_or_else(|| ApiError::refused("A store's top node cannot be split or joined."))
}

impl Pimble {
    /// Split `addr` at `at` (a sticky index made at the editor's caret): the
    /// text from there on becomes a new text node right below it, titled
    /// `title` (the selection, when there was one) or named from its text.
    /// Its children stay where they are. The new node is made first, so a
    /// refusal there leaves the note as it was.
    pub async fn split_node(&self, addr: Addr, at: Vec<u8>, title: Option<String>) -> Result<Split, ApiError> {
        let before = self.doc(addr).await?;
        let parent = text_parent(&before)?;
        // What the new node holds, judged on a copy: a refusal writes nothing.
        let off = NodeDoc::load(&before.save()).map_err(ApiError::unreadable)?.split_off(&at)?;
        // A title given is the note's name, as one typed in the explorer is;
        // without one it is named from its text.
        let custom = match &title {
            Some(_) => vec![(pimble_core::custom_keys::EXPLICIT_TITLE.to_string(), serde_json::Value::Bool(true))],
            None => Vec::new(),
        };
        let init = NodeInit { node_type: node_types::DOCUMENT.to_string(), title: title.unwrap_or_default(), content: Some(off.rest.clone()), custom };
        let new = self.create(Addr { store: addr.store, node: parent }, Place::After(addr.node), init).await?;
        if !off.keys.is_empty() {
            let keys = off.keys.clone();
            self.edit(new, move |doc, _| -> Result<(), ApiError> {
                doc.copy_data_keys(&before, &keys);
                Ok(())
            })
            .await?;
        }
        let kept = self.edit(addr, move |doc, _| doc.split_off(&at).map(|_| ())).await?;
        Ok(Split { new, kept_delta: kept.delta })
    }

    /// Join the node right below `addr` (its next sibling) into it: that
    /// node's text goes after this one's, its children after this one's, and
    /// it is deleted (undoable; links to it lead here).
    pub async fn join_next(&self, addr: Addr) -> Result<Joined, ApiError> {
        let mine = self.doc(addr).await?;
        let parent = text_parent(&mine)?;
        let siblings = self.doc(Addr { store: addr.store, node: parent }).await?.children();
        let below = siblings
            .iter()
            .position(|id| *id == addr.node)
            .and_then(|i| siblings.get(i + 1).copied())
            .ok_or_else(|| ApiError::refused("There is no note below this one to join."))?;
        let below = Addr { store: addr.store, node: below };
        let theirs = self.doc(below).await?;
        text_parent(&theirs).map_err(|_| ApiError::refused("The node below is not a text note, so it cannot be joined."))?;
        let children = theirs.children();
        let edited = self
            .edit(addr, move |doc, _| doc.append_content_of(&theirs))
            .await?;
        for child in children {
            self.move_node(Addr { store: addr.store, node: child }, addr, Place::Last).await?;
        }
        let here = PimbleUrl::node(addr.store, addr.node);
        self.edit(below, move |doc, _| -> Result<(), ApiError> {
            doc.set_became(&here);
            Ok(())
        })
        .await?;
        self.delete(below).await?;
        Ok(Joined { gone: below, delta: edited.delta })
    }
}
