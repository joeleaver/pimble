//! The node document: one yrs `Doc` per node holding its rich text, its place
//! in the tree, its metadata, and a root reserved for a plugin's JSON
//! (docs/NODE_DOCUMENT_CONTRACT.md section 1). Everything a person can edit
//! about a node is in here and co-edited: this is what `CLAUDE.md`'s
//! "everything is co-editable" means in bytes.
//!
//! Roots: `content` and `meta` belong to rinch-editor-collab (the rich text and
//! its format tag; Pimble never writes `meta`); `node`, `children` and `data`
//! are Pimble's. A `NodeDoc` that has no `node` root yet is a content-only
//! document (a phase 2a `ContentDoc` on disk, or one made by the importer
//! before the tree places it); [`NodeDoc::init`] gives it one.
//!
//! Layout of Pimble's roots:
//! ```text
//! root map "node": { node_type: String, title: String,
//!                    parent_id: String | absent (the store's root),
//!                    created_at: String (rfc3339), modified_at: String (rfc3339),
//!                    deleted_at: String (rfc3339) | absent (the tombstone),
//!                    became: String (a pimble: link) | absent,
//!                    attached_to: String (node id) | absent (an attachment),
//!                    tags: Array<String>, custom: Map<String, String (json)> }
//! root array "children": [String (node id), ...]
//! root map "data": arbitrary JSON as nested Maps, Arrays and Texts
//! ```
//! Every root is resolved with its type when the document is built, before any
//! bytes are applied: yrs sends no type for a root, and a root it has never
//! been asked for by type reports no change (its events have no shape), which
//! would blind [`NodeDoc::apply_update`] to a peer's first write into it.
//! Timestamps are the caller's (`now`, rfc3339); this crate never reads a
//! clock, so what two replicas write is decided by their callers alone.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use pimble_core::metanode::Metanode;
use pimble_core::{Node, NodeId, NodeMetadata, PimbleUrl, StoreAccess};
use yrs::{ArrayRef, BranchID, Doc, MapRef, Out, ReadTxn, StateVector, Transact};

use crate::blocks::{blocks_from_plain_text, Block};
use crate::content_doc::{decode_update, diff_since, snapshot_from_blocks, transaction_changed};
use crate::error::{CrdtError, Result};

// The document's surface by root: construction and bytes here; the rich text
// (`content`), its Markdown edits (`markdown_edits`) and splitting it
// (`split`); the `node` and `children` roots (`structure`, and `transactions`
// for the writes `Tree` makes); the plugin root (`data`) and the board text
// read from it (`boards`).
mod boards;
mod content;
mod data;
mod markdown_edits;
mod placing;
mod split;
mod structure;
mod transactions;

pub use data::{DataEdit, DataStep};
pub use markdown_edits::{Outline, SplitPart, SplitParts};
pub use split::SplitOff;

/// The root map holding a node's structure and metadata.
pub const ROOT_NODE: &str = "node";
/// The root array holding a node's child ids, in order.
pub const ROOT_CHILDREN: &str = "children";
/// The root map reserved for a plugin node type's JSON.
pub const ROOT_DATA: &str = "data";

/// rinch-editor-collab's roots (its `projection.rs`), named here to resolve
/// them by type and to tell a content change from a structural one. The
/// format tag it writes under `meta` is what says a document holds a
/// projection at all.
const ROOT_CONTENT: &str = "content";
const ROOT_META: &str = "meta";
const META_FORMAT: &str = "format";

const NODE_TYPE: &str = "node_type";
const TITLE: &str = "title";
const PARENT_ID: &str = "parent_id";
/// The parent this document was last placed under by an operation that also
/// edited the lists (a create, move, undelete, plant, or a repair's own
/// rewrite): written with `parent_id` in the same transaction. A `parent_id`
/// that differs from it was written on its own, which is the shape repair
/// judges (docs/MOVE_CONTRACT.md "Repair"); one that agrees is honoured by
/// every device and never rewritten, which is what keeps repair convergent.
const PLACED_UNDER: &str = "placed_under";
const CREATED_AT: &str = "created_at";
const MODIFIED_AT: &str = "modified_at";
const DELETED_AT: &str = "deleted_at";
/// On a transplanted original's tombstone: the `pimble:` link of the node it
/// became (docs/LINKS_CONTRACT.md "`became`"). Never cleared: a live node's
/// is not followed.
const BECAME: &str = "became";
/// On an attachment (docs/DISCUSSIONS_CONTRACT.md section 1): the node it
/// belongs to. Written in the transaction that initialises the document,
/// rewritten only by a transplant of its host. An attachment has no
/// `parent_id` and is in no list; the tree's repair never places it.
const ATTACHED_TO: &str = "attached_to";
const TAGS: &str = "tags";
const CUSTOM: &str = "custom";
/// A node field is the `node` root's key `field:<name>` (docs/SHEETS_CONTRACT.md
/// section 8, decision 1): one co-edited entry per field, apart from Pimble's
/// own keys, and no container made on first use (two devices adding a
/// node's first field while apart would each make one, and one would be lost).
pub const FIELD_PREFIX: &str = "field:";

/// A key on a list owner's `node` root, `left:<child id>`: an operation (a
/// move away, a deletion, a transplant) took that child out of this list,
/// and none has put it back since (docs/MOVE_CONTRACT.md "Repair", 2026-11-19).
/// It is the evidence, in the list's own document, that a device holding an
/// older copy of the child (which still names this parent) must not put it
/// back: before, such a device and one holding the newer copy undid each
/// other's repair three times a second for as long as both were connected.
pub const LEFT_PREFIX: &str = "left:";

/// What the `node` root holds, read out as plain values.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NodeFields {
    pub node_type: String,
    pub title: String,
    /// Absent on a store's root node.
    pub parent_id: Option<NodeId>,
    /// The parent `parent_id` was last written together with (see
    /// `PLACED_UNDER`); absent on the root and on a document from before it
    /// was recorded.
    pub placed_under: Option<NodeId>,
    pub created_at: String,
    pub modified_at: String,
    /// A tombstone (docs/NODE_DOCUMENT_CONTRACT.md section 2): set when the
    /// node was deleted, cleared by an undelete. The document stays.
    pub deleted_at: Option<String>,
    /// On a transplanted original: the node it became (see `BECAME`).
    pub became: Option<PimbleUrl>,
    /// On an attachment: the node it belongs to (see `ATTACHED_TO`).
    pub attached_to: Option<NodeId>,
    /// The entered tags: what `node.tags` stores (docs/HASHTAGS_CONTRACT.md).
    pub tags: Vec<String>,
    /// The #hashtags in the node's text, each once as first written, in the
    /// order they first appear: worked out from the content when read, never
    /// stored. Empty from [`NodeDoc::stored_fields`], which does not read the
    /// text.
    pub text_tags: Vec<String>,
    pub custom: HashMap<String, serde_json::Value>,
    /// The node's fields, name and value as typed, ordered by name.
    pub fields: Vec<(String, String)>,
    /// The viewer a metanode is shown with (`pimble_core::metanode::VIEWER_KEY`).
    pub metanode_viewer: Option<String>,
    /// A metanode's source as stored (`pimble_core::metanode::SOURCE_KEY`).
    pub metanode_source: Option<String>,
}

impl NodeFields {
    /// The node these fields describe and nothing more: content and
    /// children empty, `access` `Full` (a judgement the server that answers
    /// makes per caller, never a property of the stored node). The one place
    /// a document's stored timestamps become a `NodeMetadata`'s, so every
    /// reader of a document, on every side, reads them the same way
    /// ([`parse_timestamp`]). What wants the content or the children reads
    /// the tree for them (`Tree::get_children`, `NodeDoc::save`).
    ///
    /// The node's tags are the entered tags and then the text tags no
    /// entered tag already is (docs/HASHTAGS_CONTRACT.md "Where it shows"),
    /// and `metadata.text_tags` says which tags are in the text.
    pub fn into_node(self, id: NodeId) -> Node {
        Node {
            id,
            parent_id: self.parent_id,
            node_type: self.node_type,
            metadata: NodeMetadata {
                title: self.title,
                created_at: parse_timestamp(&self.created_at),
                modified_at: parse_timestamp(&self.modified_at),
                tags: crate::hashtags::union(&self.tags, &self.text_tags),
                text_tags: self.text_tags,
                custom: self.custom,
                fields: self.fields,
                metanode: Metanode::from_stored(self.metanode_viewer.as_deref(), self.metanode_source.as_deref()),
            },
            content: Vec::new(),
            children: Vec::new(),
            became: self.became,
            attached_to: self.attached_to,
            attachments: 0,
            access: StoreAccess::Full,
        }
    }
}

impl NodeFields {
    /// Every tag of the node: the entered tags, then the text tags no entered
    /// tag already is (what `into_node` puts in `metadata.tags`).
    pub fn all_tags(&self) -> Vec<String> {
        crate::hashtags::union(&self.tags, &self.text_tags)
    }
}

/// A stored RFC 3339 timestamp (`created_at`, `modified_at`) as a
/// `DateTime`, defaulting to now on a document too old or too damaged to
/// parse.
pub fn parse_timestamp(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw).map(|dt| dt.with_timezone(&Utc)).unwrap_or_else(|_| Utc::now())
}

/// What merging a peer's update changed (decision 8 of
/// docs/history/HARDENING_CONTRACT.md, per root): `changed` is `false` exactly
/// when every part of the update was already reflected here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NodeUpdateEffect {
    pub changed: bool,
    /// The `node` or `children` root changed.
    pub structure: bool,
    /// The `content` root changed.
    pub content: bool,
    /// The `data` root changed.
    pub data: bool,
}

/// One node's document. See the module doc.
pub struct NodeDoc {
    doc: Doc,
    node: MapRef,
    children: ArrayRef,
    data: MapRef,
    meta: MapRef,
    content: ArrayRef,
}

#[cfg(test)]
thread_local! {
    /// Test only: while set, every new document on this thread takes the next
    /// client id from here instead of a random one, so a seeded test replays
    /// the same merges (yrs orders concurrent inserts by client id).
    static NEXT_CLIENT_ID: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Test only: from here on, documents made on this thread take client ids
/// `first`, `first + 1`, ... (see `NEXT_CLIENT_ID`).
#[cfg(test)]
pub(crate) fn seed_client_ids(first: u64) {
    NEXT_CLIENT_ID.with(|next| next.set(Some(first)));
}

impl NodeDoc {
    // ── Construction and bytes (the `ContentDoc` surface, kept) ──────────

    /// An empty document: empty content, no `node` root yet.
    pub fn new() -> Self {
        #[allow(unused_mut)]
        let mut options = yrs::Options { offset_kind: yrs::OffsetKind::Utf16, ..Default::default() };
        #[cfg(test)]
        NEXT_CLIENT_ID.with(|next| {
            if let Some(id) = next.get() {
                options.client_id = yrs::ClientID::new(id);
                next.set(Some(id + 1));
            }
        });
        let doc = Doc::with_options(options);
        // Resolving a root opens its own transaction, so all five are resolved
        // here, before any caller can hold one (see the module doc for why the
        // collab roots are resolved at all). A root with no items adds nothing
        // to the saved bytes, so an empty `NodeDoc` saves as an empty document.
        let node = doc.get_or_insert_map(ROOT_NODE);
        let children = doc.get_or_insert_array(ROOT_CHILDREN);
        let data = doc.get_or_insert_map(ROOT_DATA);
        let meta = doc.get_or_insert_map(ROOT_META);
        let content = doc.get_or_insert_array(ROOT_CONTENT);
        Self { doc, node, children, data, meta, content }
    }

    /// Load from `bytes`, a yrs v1 update (a full snapshot or any update).
    /// Empty bytes are [`NodeDoc::new`].
    pub fn load(bytes: &[u8]) -> Result<Self> {
        let mut doc = Self::new();
        if !bytes.is_empty() {
            doc.apply_update(bytes)?;
        }
        Ok(doc)
    }

    /// One paragraph per line of `text`; no `node` root yet.
    pub fn from_plain_text(text: &str) -> Result<Self> {
        Self::from_blocks(&blocks_from_plain_text(text))
    }

    /// Content from blocks (the importer's way in); no `node` root yet.
    pub fn from_blocks(blocks: &[Block]) -> Result<Self> {
        Self::load(&snapshot_from_blocks(blocks)?)
    }

    /// Full snapshot: the v1 update encoding of the whole document from an
    /// empty state vector. Round-trips through [`NodeDoc::load`].
    pub fn save(&self) -> Vec<u8> {
        self.doc.transact().encode_state_as_update_v1(&StateVector::default())
    }

    /// This document's state vector, v1-encoded.
    pub fn state_vector(&self) -> Vec<u8> {
        crate::sync_util::canonical_state_vector(&self.doc.transact().state_vector())
    }

    /// Everything this document has that a peer at `state_vector` lacks.
    pub fn diff_since(&self, state_vector: &[u8]) -> Result<Vec<u8>> {
        diff_since(&self.doc, state_vector)
    }

    /// Merge a peer's v1 update (a delta, a reconciliation diff, or a whole
    /// snapshot), reporting what it changed and in which roots.
    ///
    /// `changed` comes from the merging transaction's before/after state and
    /// delete set, as `ContentDoc` computes it. The roots come from the same
    /// transaction: at commit yrs walks from every type an insert or delete
    /// touched up to its root and records the chain
    /// (`TransactionMut::changed_parent_types`), so a root's name there means
    /// something under it changed, however deep. An update yrs parks because
    /// it depends on structs not seen yet reports nothing now; its changes are
    /// reported by the transaction that finally integrates it.
    pub fn apply_update(&mut self, update: &[u8]) -> Result<NodeUpdateEffect> {
        let update = decode_update(update)?;
        let mut txn = self.doc.transact_mut();
        txn.apply_update(update).map_err(|e| CrdtError::Yrs(e.to_string()))?;
        txn.commit();
        let mut effect = NodeUpdateEffect { changed: transaction_changed(&txn), ..Default::default() };
        for branch in txn.changed_parent_types() {
            if let BranchID::Root(name) = branch.id() {
                match &*name {
                    ROOT_NODE | ROOT_CHILDREN => effect.structure = true,
                    ROOT_CONTENT | ROOT_META => effect.content = true,
                    ROOT_DATA => effect.data = true,
                    _ => {}
                }
            }
        }
        Ok(effect)
    }

    /// What an update yrs parked is waiting for (the encoded state vector of
    /// the first missing struct per client), or `None` when nothing is
    /// parked. A parked update stays invisible until the structs before it
    /// arrive: a document left like this shows a peer's edits only in part.
    pub fn missing(&self) -> Option<Vec<u8>> {
        let txn = self.doc.transact();
        let pending = txn.store().pending_update()?;
        Some(crate::sync_util::canonical_state_vector(&pending.missing))
    }

    /// Set aside what waits parked in this document
    /// (docs/CONVERGENCE_CONTRACT.md section 5): the parked update and any
    /// deletes waiting with it are dropped, and the document holds exactly
    /// what it had integrated. Answers what the dropped update waited for
    /// (as [`NodeDoc::missing`] said it), or `None` when nothing was parked.
    ///
    /// Nothing integrated changes: the state vector and the delete digest are
    /// what they were, so no peer is sent anything because of it. What is
    /// dropped is gone from this document's saved bytes too ([`NodeDoc::save`]
    /// carries a parked update, which is why a document loaded again parks it
    /// again until it is set aside). Should what it waited for ever arrive,
    /// the device that holds it sends everything after this document's
    /// vector, the set-aside structs included.
    pub fn set_aside_parked(&mut self) -> Option<Vec<u8>> {
        use yrs::WriteTxn;
        let missing = self.missing()?;
        let mut txn = self.doc.transact_mut();
        txn.prune_pending();
        Some(missing)
    }

    /// A digest of what this document has deleted, for a state report
    /// (docs/CONVERGENCE_CONTRACT.md): two documents with one state vector
    /// differ only if their delete sets do, and a state vector says nothing of
    /// those. The same delete set gives the same digest on every device: the
    /// clients in order, each one's deleted clock ranges sorted and joined
    /// where they touch, so how the store happens to have split or merged its
    /// blocks (garbage collection replaces a deleted item's content and may
    /// merge neighbours, keeping its clocks) changes nothing. SHA-256 over
    /// `client u64 | count u32 | (start u32 | end u32)*` per client, little
    /// endian.
    pub fn delete_set_digest(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let snapshot = self.doc.transact().snapshot();
        let mut hasher = Sha256::new();
        for (client, ranges) in snapshot.delete_set.iter() {
            let mut spans: Vec<(u32, u32)> = ranges.iter().map(|r| (r.start, r.end)).filter(|(s, e)| s < e).collect();
            spans.sort_unstable();
            let mut joined: Vec<(u32, u32)> = Vec::with_capacity(spans.len());
            for (start, end) in spans {
                match joined.last_mut() {
                    Some(last) if start <= last.1 => last.1 = last.1.max(end),
                    _ => joined.push((start, end)),
                }
            }
            if joined.is_empty() {
                continue;
            }
            hasher.update(client.get().to_le_bytes());
            hasher.update((joined.len() as u32).to_le_bytes());
            for (start, end) in joined {
                hasher.update(start.to_le_bytes());
                hasher.update(end.to_le_bytes());
            }
        }
        hasher.finalize().into()
    }

    /// Whether the peer lacks something of ours, given its state vector and
    /// the diff it sent (docs/history/HARDENING_CONTRACT.md decision 7).
    pub fn diff_if_peer_lacks_it(&self, remote_sv: &[u8], remote_diff: &[u8]) -> Result<Option<Vec<u8>>> {
        let local_diff = self.diff_since(remote_sv)?;
        if crate::sync_util::peer_lacks_something(&local_diff, remote_diff)? {
            Ok(Some(local_diff))
        } else {
            Ok(None)
        }
    }
}

impl Default for NodeDoc {
    fn default() -> Self {
        Self::new()
    }
}

// The server holds node documents across `.await`s, as it held `ContentDoc`
// and `StoreDocument`; the root handles kept here are `Send` only with yrs's
// `sync` feature, which rinch-editor-collab turns on. Fail here, not there.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<NodeDoc>();
};

fn string_value(out: Option<Out>) -> Option<String> {
    match out {
        Some(Out::Any(any)) => String::try_from(any).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod split_tests;
