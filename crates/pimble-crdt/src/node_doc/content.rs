//! A node's rich text (the `content` root): plain text, index units, links,
//! mentions, pictures, anchors and blocks read from it, and the edits that
//! replace or extend it.

use std::collections::HashMap;

use pimble_core::{Anchor, IndexUnit};
use yrs::{Any, Array, ArrayRef, GetString, Map, MapRef, Out, Transact, TransactionMut};

use crate::blobs::{blob_refs_of_model, BlobRef};
use crate::blocks::Block;
use crate::content_doc::{
    append_delta, blocks_of_projection, fresh_snapshot, join_units, project, replacement_delta, units_of_projection,
};
use crate::error::{CrdtError, Result};
use crate::links::{self, links_of_model, LinkRef, Resolved, Spot};
use crate::markdown;

use super::boards::{board_cards, text_boards, BOARD};
use super::{NodeDoc, META_FORMAT};

impl NodeDoc {
    // ── Content ──────────────────────────────────────────────────────────

    /// Replace the content with one paragraph per line of `text`, as an edit
    /// of the existing document (it shares history with every copy). Returns
    /// the update the edit produced.
    ///
    /// A document with no projection yet (no format tag in `meta`: a node
    /// whose content was never written, whether or not its `node` root is)
    /// is seeded the way [`NodeDoc::from_plain_text`] builds one, and the
    /// update is that whole snapshot. `ContentDoc` decides the same by an
    /// empty state vector; here the `node` root may hold structs while the
    /// content holds none, so the tag is what counts.
    pub fn replace_plain_text(&mut self, text: &str) -> Result<Vec<u8>> {
        let seed = !self.has_projection();
        let delta = replacement_delta(&self.save(), seed, text)?;
        self.apply_update(&delta)?;
        Ok(delta)
    }

    /// Append `blocks` after the content, as an edit of its history (what
    /// `pimble-cli append-link` sends; the importer's `from_blocks` is for a
    /// node whose content was never written). Answers the delta applied.
    pub fn append_blocks(&mut self, blocks: &[Block]) -> Result<Vec<u8>> {
        let seed = !self.has_projection();
        let delta = append_delta(&self.save(), seed, blocks)?;
        self.apply_update(&delta)?;
        Ok(delta)
    }

    /// The content as plain text, paragraphs joined with newlines.
    pub fn text(&self) -> String {
        join_units(&self.units())
    }

    /// [`NodeDoc::text`] of a serialized document, without keeping it.
    pub fn text_of(bytes: &[u8]) -> String {
        Self::load(bytes).map(|doc| doc.text()).unwrap_or_default()
    }

    /// The content as index units (prose, headings, code, ...), for the search
    /// index and the chunker. `[]` when there is no projection yet.
    pub fn units(&self) -> Vec<IndexUnit> {
        if !self.has_projection() {
            return Vec::new();
        }
        units_of_projection(&self.save(), "NodeDoc::units")
    }

    /// [`NodeDoc::units`] of a serialized document.
    pub fn units_of(bytes: &[u8]) -> Vec<IndexUnit> {
        Self::load(bytes).map(|doc| doc.units()).unwrap_or_default()
    }

    /// Every `pimble:` link in the content, in document order
    /// (docs/LINKS_CONTRACT.md "Where a link lives"). `[]` when there is no
    /// projection yet.
    ///
    /// A board's node cards are links too (docs/IMAGE_MARKUP_CONTRACT.md
    /// section 4, "node card": "it is a `pimble:` link, so it ... counts as a
    /// backlink"): after the content's links come the cards of the node's own
    /// board and of the boards of marked-up pictures in its text, path
    /// `f:board/<element>` or `f:boards/<board>/<element>`, the card's title
    /// as the text.
    pub fn links(&self) -> Vec<LinkRef> {
        let mut out = if self.has_projection() {
            project(&self.save(), "NodeDoc::links").map(|model| links_of_model(&model)).unwrap_or_default()
        } else {
            Vec::new()
        };
        self.board_card_links(&mut out);
        out
    }

    /// The node cards on this node's boards, in element id order per board.
    fn board_card_links(&self, out: &mut Vec<LinkRef>) {
        let txn = self.doc.transact();
        if let Some(Out::YMap(board)) = self.data.get(&txn, BOARD) {
            board_cards(&txn, &board, "f:board", out);
        }
        for (id, board) in text_boards(&txn, &self.data) {
            board_cards(&txn, &board, &format!("f:boards/{id}"), out);
        }
    }

    /// [`NodeDoc::links`] of a serialized document.
    pub fn links_of(bytes: &[u8]) -> Vec<LinkRef> {
        Self::load(bytes).map(|doc| doc.links()).unwrap_or_default()
    }

    /// Every mention of a person in the content, in document order
    /// (docs/NOTIFICATIONS_CONTRACT.md section 1). `[]` when there is no
    /// projection yet.
    pub fn mentions(&self) -> Vec<pimble_core::mention::Mention> {
        if !self.has_projection() {
            return Vec::new();
        }
        project(&self.save(), "NodeDoc::mentions").map(|model| crate::mentions::of_model(&model)).unwrap_or_default()
    }

    /// The mentions `update` would add to this document's content, each with
    /// its id, leaving out `writer`'s own account; this document is not
    /// changed. The hook a writer's device notifies from (wave 1): it passes
    /// the updates its own edits made, and keeps the ids it has notified, so
    /// redoing a mention it already told someone about tells nobody again
    /// (`pimble_core::mention::added`).
    pub fn mentions_added_by(&self, update: &[u8], writer: Option<&str>) -> Result<Vec<pimble_core::mention::Mention>> {
        let mut after = Self::load(&self.save())?;
        after.apply_update(update)?;
        Ok(pimble_core::mention::added(&self.mentions(), &after.mentions(), writer))
    }

    /// Every picture the content names that is kept in a store's blobs: each
    /// `image` whose `src` is a `pimble-blob:` URL, in document order, at any
    /// depth (docs/IMAGES_CONTRACT.md "On disk"). `[]` when there is no
    /// projection yet.
    pub fn blob_refs(&self) -> Vec<BlobRef> {
        if !self.has_projection() {
            return Vec::new();
        }
        project(&self.save(), "NodeDoc::blob_refs").map(|model| blob_refs_of_model(&model)).unwrap_or_default()
    }

    /// [`NodeDoc::blob_refs`] of a serialized document.
    pub fn blob_refs_of(bytes: &[u8]) -> Vec<BlobRef> {
        Self::load(bytes).map(|doc| doc.blob_refs()).unwrap_or_default()
    }

    /// Every blob this document names anywhere, each once, sorted: the
    /// pictures in its text ([`NodeDoc::blob_refs`]) and every
    /// `pimble-blob:` string under its `data` root (a canvas element's
    /// picture, in `data.board` or a canvas in text's `data["board:<id>"]`,
    /// and whatever a plugin keeps). This is what replication copies and
    /// what judges who may read a blob (docs/IMAGES_CONTRACT.md
    /// "Replication", "Who may read a blob").
    pub fn pictures(&self) -> Vec<pimble_core::BlobUrl> {
        let mut out: std::collections::BTreeSet<pimble_core::BlobUrl> = self.blob_refs().into_iter().map(|r| r.url).collect();
        crate::blobs::blob_urls_in_json(&self.data_json(), &mut out);
        out.into_iter().collect()
    }

    /// Every picture this document names that is kept in store `from`,
    /// renamed as kept in `to` under the same blob id: in the text's image
    /// atoms and in every string under `data` (a canvas's pictures). What a
    /// transplant does to the copy it plants, whose blobs it has copied into
    /// `to` (docs/IMAGES_CONTRACT.md "The shape"). Answers the update, or
    /// `None` when nothing named one.
    pub fn move_pictures(&mut self, from: pimble_core::StoreId, to: pimble_core::StoreId) -> Result<Option<Vec<u8>>> {
        use pimble_core::BlobUrl;
        let moved = move |src: &str| -> Option<String> {
            let (bare, rest) = src.split_at(src.find('#').unwrap_or(src.len()));
            let url = BlobUrl::parse(bare).filter(|url| url.store == from)?;
            Some(format!("{}{}", BlobUrl::new(to, url.blob), rest))
        };

        // The text: image atoms whose `src` names `from`.
        let mut updates: Vec<Vec<u8>> = Vec::new();
        if self.has_projection() && self.blob_refs().iter().any(|r| r.url.store == from) {
            fn rewrite(node: &rinch_editor_core::Node, schema: &rinch_editor_core::Schema, moved: &dyn Fn(&str) -> Option<String>) -> Result<rinch_editor_core::Node> {
                if node.type_name() == "image" {
                    if let Some(src) = node.attrs().get_str("src").and_then(moved) {
                        let attrs = node.attrs().with("src", src);
                        let image = schema
                            .create_node("image", attrs, node.content().clone())
                            .map_err(|e| CrdtError::Collab(e.to_string()))?
                            .with_marks(node.marks().to_vec());
                        return Ok(image);
                    }
                    return Ok(node.clone());
                }
                if node.is_leaf() || node.is_text() {
                    return Ok(node.clone());
                }
                let children = node.content().iter().map(|c| rewrite(c, schema, moved)).collect::<Result<Vec<_>>>()?;
                Ok(node.copy_with_content(rinch_editor_core::Fragment::from_children(children)))
            }
            match self.markdown_edit(|schema, doc| rewrite(doc, schema, &moved)) {
                Ok(delta) => updates.push(delta),
                Err(CrdtError::Refused(_)) => {}
                Err(e) => return Err(e),
            }
        }

        // The data: any string, wherever it sits.
        let (_, update) = self.edit(|d, txn| {
            fn any_moved(any: &Any, moved: &dyn Fn(&str) -> Option<String>) -> Option<Any> {
                match any {
                    Any::String(s) => moved(s).map(|s| Any::String(s.into())),
                    Any::Array(items) => {
                        let new: Vec<Option<Any>> = items.iter().map(|item| any_moved(item, moved)).collect();
                        new.iter().any(Option::is_some).then(|| {
                            Any::Array(items.iter().zip(new).map(|(old, new)| new.unwrap_or_else(|| old.clone())).collect::<Vec<_>>().into())
                        })
                    }
                    Any::Map(fields) => {
                        let mut changed = false;
                        let mut out = HashMap::new();
                        for (key, value) in fields.iter() {
                            match any_moved(value, moved) {
                                Some(new) => {
                                    changed = true;
                                    out.insert(key.clone(), new);
                                }
                                None => {
                                    out.insert(key.clone(), value.clone());
                                }
                            }
                        }
                        changed.then(|| Any::Map(std::sync::Arc::new(out)))
                    }
                    _ => None,
                }
            }
            fn walk_map(txn: &mut TransactionMut, map: &MapRef, moved: &dyn Fn(&str) -> Option<String>) {
                let entries: Vec<(String, Out)> = map.iter(txn).map(|(k, v)| (k.to_string(), v)).collect();
                for (key, value) in entries {
                    match value {
                        Out::Any(any) => {
                            if let Some(new) = any_moved(&any, moved) {
                                map.insert(txn, key, new);
                            }
                        }
                        Out::YText(text) => {
                            let old = text.get_string(txn);
                            if let Some(new) = moved(&old) {
                                let len = yrs::Text::len(&text, txn);
                                yrs::Text::remove_range(&text, txn, 0, len);
                                yrs::Text::insert(&text, txn, 0, &new);
                            }
                        }
                        Out::YMap(inner) => walk_map(txn, &inner, moved),
                        Out::YArray(inner) => walk_array(txn, &inner, moved),
                        _ => {}
                    }
                }
            }
            fn walk_array(txn: &mut TransactionMut, array: &ArrayRef, moved: &dyn Fn(&str) -> Option<String>) {
                let items: Vec<Out> = array.iter(txn).collect();
                for (index, item) in items.into_iter().enumerate() {
                    match item {
                        Out::Any(any) => {
                            if let Some(new) = any_moved(&any, moved) {
                                array.remove(txn, index as u32);
                                array.insert(txn, index as u32, new);
                            }
                        }
                        Out::YText(text) => {
                            let old = text.get_string(txn);
                            if let Some(new) = moved(&old) {
                                let len = yrs::Text::len(&text, txn);
                                yrs::Text::remove_range(&text, txn, 0, len);
                                yrs::Text::insert(&text, txn, 0, &new);
                            }
                        }
                        Out::YMap(inner) => walk_map(txn, &inner, moved),
                        Out::YArray(inner) => walk_array(txn, &inner, moved),
                        _ => {}
                    }
                }
            }
            walk_map(txn, &d.data, &moved);
            Ok(())
        })?;
        updates.extend(update);
        Ok(match updates.len() {
            0 => None,
            1 => updates.pop(),
            _ => Some(yrs::merge_updates_v1(&updates).map_err(|e| CrdtError::Serialization(e.to_string()))?),
        })
    }

    /// Every picture written with its canvas in its URL (`<src>#canvas=<id>`,
    /// as Markdown writes one) takes the id into its atom's `data-board` and
    /// keeps the bare URL, the form a canvas in text is stored in
    /// (docs/CANVAS_CONTRACT.md). Each canvas named must be in `data` already
    /// (`data["board:<id>"]`). What an importer that built its blocks with
    /// such URLs calls once the canvases are made. `None` when none was.
    pub fn adopt_canvas_urls(&mut self) -> Result<Option<Vec<u8>>> {
        match self.markdown_edit(|_, doc| markdown::with_canvas_attrs(doc)) {
            Ok(delta) => Ok(Some(delta)),
            Err(CrdtError::Refused(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// [`NodeDoc::pictures`] of a serialized document.
    pub fn pictures_of(bytes: &[u8]) -> Vec<pimble_core::BlobUrl> {
        Self::load(bytes).map(|doc| doc.pictures()).unwrap_or_default()
    }

    /// A deep-link anchor at `spot` (docs/LINKS_CONTRACT.md "The URL"):
    /// `None` when the content has no such textblock.
    pub fn anchor_at(&self, spot: Spot) -> Option<Anchor> {
        links::anchor_at(&self.doc, &self.content(), spot)
    }

    /// A sticky index at the start of `quote` in the text (one place, read
    /// once): what a discussion anchored there stores (docs/DISCUSSIONS_CONTRACT.md
    /// section 6). Plain yrs bytes, which rinch's editor resolves too.
    pub fn sticky_at_quote(&self, quote: &str) -> Result<Vec<u8>> {
        let spot = links::quote_spot(&self.doc, &self.content(), quote).map_err(CrdtError::Refused)?;
        self.anchor_at(spot)
            .and_then(|a| a.sticky)
            .ok_or_else(|| CrdtError::Refused("That place in the text cannot be named.".into()))
    }

    /// Every textblock's text, in document order: what to hand
    /// [`NodeDoc::sticky_after_change`] once the document was edited.
    pub fn textblock_texts(&self) -> Vec<String> {
        links::texts(&self.doc, &self.content())
    }

    /// A sticky index at the end of what an edit wrote, from the textblocks
    /// as they read before it ([`NodeDoc::textblock_texts`]): past the last
    /// character inserted, or where text was removed. Where an agent's caret
    /// is drawn (docs/PRESENCE_CONTRACT.md section 8). `None` when the text
    /// did not change.
    pub fn sticky_after_change(&self, before: &[String]) -> Option<Vec<u8>> {
        let spot = links::end_of_change(before, &self.textblock_texts())?;
        self.anchor_at(spot)?.sticky
    }

    /// Where `anchor` is in the content now: by its sticky index, else by its
    /// quote, else `None` (docs/LINKS_CONTRACT.md "Following a link").
    pub fn resolve_anchor(&self, anchor: &Anchor) -> Option<Resolved> {
        links::resolve_anchor(&self.doc, &self.content(), anchor)
    }

    /// The `content` root (resolved when the document was built, so this
    /// opens no transaction of its own that could meet a caller's).
    fn content(&self) -> ArrayRef {
        self.content.clone()
    }

    /// The content as [`Block`]s, the reverse of [`NodeDoc::from_blocks`]: `[]`
    /// when there is no projection yet, an error when the content cannot be
    /// projected or holds something `Block` has no variant for. `Block` cannot
    /// say everything the editor can (see `blocks::read_doc`), so this is for
    /// reading a document from outside the editor, never for copying one.
    pub fn blocks(&self) -> Result<Vec<Block>> {
        if !self.has_projection() {
            return Ok(Vec::new());
        }
        blocks_of_projection(&self.save())
    }

    /// The content as a NEW document's bytes (its own history, nothing that
    /// was deleted, everything the editor's model holds: see
    /// `content_doc::fresh_snapshot` for why a transplant carries content this
    /// way). `None` when there is no projection yet, which the new node keeps
    /// as it is: no projection, seeded by its first edit like any other.
    pub(crate) fn fresh_content(&self) -> Result<Option<Vec<u8>>> {
        if !self.has_projection() {
            return Ok(None);
        }
        fresh_snapshot(&self.save()).map(Some)
    }

    /// Whether rinch-editor-collab has written its format tag: the content
    /// roots hold a projection an editor can join.
    pub(super) fn has_projection(&self) -> bool {
        let txn = self.doc.transact();
        matches!(self.meta.get(&txn, META_FORMAT), Some(Out::Any(Any::String(_))))
    }
}
