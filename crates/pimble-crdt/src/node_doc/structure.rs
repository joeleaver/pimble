//! A node's structure and metadata (the `node` and `children` roots): its
//! fields read and written, its metanode keys, its timestamps, its tombstone
//! and its list of children.

use pimble_core::metanode::{Metanode, Source, SOURCE_KEY, VIEWER_KEY};
use pimble_core::NodeId;
use yrs::{Doc, Map, ReadTxn, Transact, TransactionMut};

use crate::error::{CrdtError, Result};

use super::{string_value, NodeDoc, NodeFields, CREATED_AT, FIELD_PREFIX, MODIFIED_AT, ROOT_NODE};

impl NodeDoc {
    // ── Structure and metadata (the `node` and `children` roots) ─────────

    /// Whether the `node` root has been written (by [`NodeDoc::init`] here or
    /// on any peer).
    pub fn is_initialised(&self) -> bool {
        self.node.len(&self.doc.transact()) > 0
    }

    /// Write the `node` root for a node placed in a tree. An error when it is
    /// already initialised: a second `init` would fight the first.
    pub fn init(&mut self, node_type: &str, title: &str, parent_id: Option<NodeId>, now: &str) -> Result<()> {
        self.edit(|d, txn| d.write_init(txn, node_type, title, parent_id, now)).map(|_| ())
    }

    /// The `node` root as values. An error when not initialised. A
    /// `parent_id` this crate cannot parse reads as absent, so garbage in that
    /// field makes a detached node (which repair puts under the root), never
    /// a node the tree cannot read.
    ///
    /// `text_tags` is read from the content (docs/HASHTAGS_CONTRACT.md);
    /// [`NodeDoc::stored_fields`] skips that walk.
    pub fn fields(&self) -> Result<NodeFields> {
        let txn = self.doc.transact();
        let mut fields = self.read_fields(&txn)?;
        fields.text_tags = crate::hashtags::tags_in(&txn, &self.content);
        Ok(fields)
    }

    /// [`NodeDoc::fields`] without reading the text: `text_tags` empty. For
    /// what reads a node's place and stored keys only (the tree's walks,
    /// repair, the notifications a merge derives), where a walk of every
    /// document's text would be wasted.
    pub fn stored_fields(&self) -> Result<NodeFields> {
        let txn = self.doc.transact();
        self.read_fields(&txn)
    }

    /// The #hashtags in the content, each once as first written, in the order
    /// they first appear (docs/HASHTAGS_CONTRACT.md "What counts as a
    /// hashtag"): not in code blocks or inline code.
    pub fn text_tags(&self) -> Vec<String> {
        let txn = self.doc.transact();
        crate::hashtags::tags_in(&txn, &self.content)
    }

    /// How many times each text tag is written in the content, as
    /// [`NodeDoc::text_tags`] names them: what Properties says of one.
    pub fn text_tag_counts(&self) -> Vec<(String, usize)> {
        let txn = self.doc.transact();
        let mut out: Vec<(String, usize)> = Vec::new();
        for tag in crate::hashtags::all_in(&txn, &self.content) {
            match out.iter_mut().find(|(t, _)| crate::hashtags::same_tag(t, &tag)) {
                Some((_, n)) => *n += 1,
                None => out.push((tag, 1)),
            }
        }
        out
    }

    /// Delete every occurrence of the hashtag `tag` (any case) from the text,
    /// each with one adjacent space (docs/HASHTAGS_CONTRACT.md "Removing
    /// one"): plain deletions of the text runs, so the edit merges with
    /// anyone typing. Answers how many occurrences went (0 changes nothing).
    pub fn remove_hashtag(&mut self, tag: &str) -> Result<usize> {
        let tag = tag.trim().trim_start_matches('#').trim().to_string();
        if tag.is_empty() {
            return Ok(0);
        }
        let (removed, _) = self.edit(|d, txn| Ok(crate::hashtags::remove(txn, &d.content, std::slice::from_ref(&tag))))?;
        Ok(removed)
    }

    /// Set the node's tags to `wanted` (docs/HASHTAGS_CONTRACT.md "Removing
    /// one"): the entered tags become the ones still wanted plus each new one
    /// (a text tag is never copied into them), and each text tag left out of
    /// `wanted` is removed from the text as [`NodeDoc::remove_hashtag`]
    /// removes one. One transaction; nothing is written when nothing differs.
    pub fn set_all_tags(&mut self, wanted: &[String], now: &str) -> Result<()> {
        self.edit_node(|d, txn| d.write_all_tags(txn, wanted, now)).map(|_| ())
    }

    /// [`NodeDoc::set_all_tags`] in a transaction of the caller's.
    pub(crate) fn write_all_tags(&self, txn: &mut TransactionMut, wanted: &[String], now: &str) -> Result<()> {
        let fields = self.read_fields(&*txn)?;
        let text = crate::hashtags::tags_in(&*txn, &self.content);
        let (entered, remove) = crate::hashtags::split_wanted(wanted, &fields.tags, &text);
        if entered != fields.tags {
            self.write_tags(txn, &entered, now)?;
        }
        if crate::hashtags::remove(txn, &self.content, &remove) > 0 {
            self.stamp_modified(txn, now);
        }
        Ok(())
    }

    pub fn set_title(&mut self, title: &str, now: &str) -> Result<()> {
        self.edit_node(|d, txn| d.write_title(txn, title, now)).map(|_| ())
    }

    pub fn set_node_type(&mut self, node_type: &str, now: &str) -> Result<()> {
        self.edit_node(|d, txn| d.write_node_type(txn, node_type, now)).map(|_| ())
    }

    /// `None` removes a stored parent (the node becomes a root).
    pub fn set_parent_id(&mut self, parent_id: Option<NodeId>, now: &str) -> Result<()> {
        self.edit_node(|d, txn| {
            d.write_parent(txn, parent_id);
            d.stamp_modified(txn, now);
            Ok(())
        })
        .map(|_| ())
    }

    /// Replace the tags wholesale.
    pub fn set_tags(&mut self, tags: &[String], now: &str) -> Result<()> {
        self.edit_node(|d, txn| d.write_tags(txn, tags, now)).map(|_| ())
    }

    /// Stored as JSON text, as the store document stored `custom`: a value is
    /// replaced whole, and two replicas setting the same key converge on one
    /// of them. Field-by-field co-editing is what `data` is for.
    pub fn set_custom(&mut self, key: &str, value: &serde_json::Value, now: &str) -> Result<()> {
        self.edit_node(|d, txn| d.write_custom(txn, key, value, now)).map(|_| ())
    }

    /// Set node field `name` to `value` as typed, or with `None` remove it.
    /// A field named the same in another case is that field (names are
    /// unique case-insensitively, as a table's columns are).
    pub fn set_field(&mut self, name: &str, value: Option<&str>, now: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(CrdtError::Serialization("A field needs a name.".into()));
        }
        let existing = self.node_fields().into_iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(n, _)| n);
        let key = format!("{FIELD_PREFIX}{}", existing.as_deref().unwrap_or(name));
        self.edit_node(|d, txn| {
            match value {
                Some(v) => {
                    d.node.insert(txn, key.as_str(), v.to_string());
                }
                None => {
                    d.node.remove(txn, &key);
                }
            }
            d.stamp_modified(txn, now);
            Ok(())
        })
        .map(|_| ())
    }

    /// The first state of a new metanode shown with `viewer` over `source`
    /// (docs/METANODES_CONTRACT.md): the two keys alone, as an update the
    /// create merges into the new node's document, so the node is a metanode
    /// from the edit that makes it (`pimble_api::recipes::metanode`). It is
    /// written without the `node` root's other keys, which the create
    /// writes; their merge is the create's own.
    pub fn metanode_seed(viewer: &str, source: &Source) -> Vec<u8> {
        let seed = NodeDoc::new();
        {
            let mut txn = seed.doc.transact_mut();
            seed.node.insert(&mut txn, VIEWER_KEY, viewer.trim().to_string());
            seed.node.insert(&mut txn, SOURCE_KEY, source.to_stored());
        }
        seed.save()
    }

    /// Make the node a metanode shown with `viewer`
    /// (docs/METANODES_CONTRACT.md), or
    /// with `None` an ordinary node again. The source is left as it is, so a
    /// node shown as an outline and then with a viewer again keeps it.
    pub fn set_metanode_viewer(&mut self, viewer: Option<&str>, now: &str) -> Result<()> {
        let viewer = viewer.map(str::trim).filter(|v| !v.is_empty());
        // Setting what is there writes nothing, not even the time.
        if self.stored_string(VIEWER_KEY).as_deref() == viewer {
            return Ok(());
        }
        self.edit_node(|d, txn| {
            match viewer {
                Some(v) => {
                    d.node.insert(txn, VIEWER_KEY, v.to_string());
                }
                None => {
                    d.node.remove(txn, VIEWER_KEY);
                }
            }
            d.stamp_modified(txn, now);
            Ok(())
        })
        .map(|_| ())
    }

    /// Set which nodes the metanode shows, or with `None` go back to its
    /// children (the key removed).
    pub fn set_metanode_source(&mut self, source: Option<&Source>, now: &str) -> Result<()> {
        if self.stored_string(SOURCE_KEY) == source.map(Source::to_stored) {
            return Ok(());
        }
        self.edit_node(|d, txn| {
            match source {
                Some(source) => {
                    d.node.insert(txn, SOURCE_KEY, source.to_stored());
                }
                None => {
                    d.node.remove(txn, SOURCE_KEY);
                }
            }
            d.stamp_modified(txn, now);
            Ok(())
        })
        .map(|_| ())
    }

    /// The metanode the node is, when it is one.
    pub fn metanode(&self) -> Option<Metanode> {
        Metanode::from_stored(self.stored_string(VIEWER_KEY).as_deref(), self.stored_string(SOURCE_KEY).as_deref())
    }

    /// A string key of the `node` root as stored.
    fn stored_string(&self, key: &str) -> Option<String> {
        let txn = self.doc.transact();
        string_value(self.node.get(&txn, key))
    }

    /// The node's own fields (`field:<name>` keys), ordered by name.
    pub fn node_fields(&self) -> Vec<(String, String)> {
        let txn = self.doc.transact();
        self.read_node_fields(&txn)
    }

    pub(super) fn read_node_fields<T: ReadTxn>(&self, txn: &T) -> Vec<(String, String)> {
        let mut fields: Vec<(String, String)> = self
            .node
            .iter(txn)
            .filter_map(|(key, value)| Some((key.strip_prefix(FIELD_PREFIX)?.to_string(), string_value(Some(value))?)))
            .collect();
        fields.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()).then(a.0.cmp(&b.0)));
        fields
    }

    /// An update setting field `name` (exactly as named; `None` removes it)
    /// of a node whose document the caller does not hold: made on a blank
    /// document, it merges into the node's own, since the `node` root is a
    /// root of every node document. Only for a field the node does not have
    /// yet: over an existing value it merges as concurrent with it, and either
    /// may win. To change a value, `set_field` on a copy of the document.
    pub fn field_update(name: &str, value: Option<&str>) -> Vec<u8> {
        let doc = Doc::new();
        let node = doc.get_or_insert_map(ROOT_NODE);
        let mut txn = doc.transact_mut();
        let key = format!("{FIELD_PREFIX}{}", name.trim());
        match value {
            Some(v) => {
                node.insert(&mut txn, key.as_str(), v.to_string());
            }
            None => {
                node.remove(&mut txn, &key);
            }
        }
        txn.encode_update_v1()
    }

    /// Returns whether the key was there; an absent key writes nothing.
    pub fn remove_custom(&mut self, key: &str, now: &str) -> Result<bool> {
        self.edit_node(|d, txn| Ok(d.remove_custom_key(txn, key, now))).map(|(removed, _)| removed)
    }

    /// Both timestamps at once (the importer, the migration).
    pub fn set_timestamps(&mut self, created_at: &str, modified_at: &str) -> Result<()> {
        self.edit_node(|d, txn| {
            d.node.insert(txn, CREATED_AT, created_at.to_string());
            d.node.insert(txn, MODIFIED_AT, modified_at.to_string());
            Ok(())
        })
        .map(|_| ())
    }

    pub fn touch_modified(&mut self, now: &str) -> Result<()> {
        self.edit_node(|d, txn| {
            d.stamp_modified(txn, now);
            Ok(())
        })
        .map(|_| ())
    }

    /// Set the tombstone (`Some(now)`) or clear it (`None`, an undelete).
    pub fn set_deleted(&mut self, at: Option<&str>) -> Result<()> {
        self.edit_node(|d, txn| {
            d.write_deleted(txn, at);
            Ok(())
        })
        .map(|_| ())
    }

    /// The children as stored: duplicates, ids with no document, and deleted
    /// nodes included. `Tree` is what filters and repairs.
    pub fn children(&self) -> Vec<NodeId> {
        self.raw_children().into_iter().flatten().collect()
    }

    /// Insert `id` at `at` (clamped to the end).
    pub fn insert_child(&mut self, at: usize, id: NodeId) -> Result<()> {
        self.edit(|d, txn| {
            d.list_insert(txn, Some(at), id);
            Ok(())
        })
        .map(|_| ())
    }

    /// Remove every occurrence of `id`; returns whether there was one.
    pub fn remove_child(&mut self, id: NodeId) -> Result<bool> {
        self.edit(|d, txn| Ok(d.list_remove_all(txn, id))).map(|(removed, _)| removed)
    }

    /// Remove the entry at `at` (repair's surgical edit).
    pub fn remove_child_at(&mut self, at: usize) -> Result<()> {
        self.edit(|d, txn| d.list_remove_at(txn, at)).map(|_| ())
    }
}
