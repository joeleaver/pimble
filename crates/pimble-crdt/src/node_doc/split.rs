//! Splitting a node's text in two and joining two nodes' text
//! (docs/SPLIT_CONTRACT.md).

use pimble_core::PimbleUrl;
use yrs::{Map, Transact};

use crate::content_doc::{model_edit_delta, model_of};
use crate::error::{CrdtError, Result};
use crate::markdown;

use super::data::out_to_in;
use super::NodeDoc;

/// What splitting a node's text leaves for the new node: its content, a fresh
/// projection with a history of its own, and the `data` keys (`board:<id>`,
/// `sheet:<id>`) its canvases and sheets live under (docs/SPLIT_CONTRACT.md).
#[derive(Debug, Clone)]
pub struct SplitOff {
    pub rest: Vec<u8>,
    pub keys: Vec<String>,
}

impl NodeDoc {
    /// Cut the text from `sticky` (a sticky index the editor made at its caret)
    /// to the end out of this node, with the `data` of the canvases and sheets in
    /// what was cut, and answer what the new node is made of. Where the split
    /// falls: at the caret in a paragraph or heading (at a block's edge, between
    /// blocks, so neither half begins or ends with an empty one); inside a
    /// table, before the whole table, which never splits. Refused when either
    /// half would be empty.
    pub fn split_off(&mut self, sticky: &[u8]) -> Result<SplitOff> {
        use rinch_editor_collab::CollabSession;
        let collab = |e: rinch_editor_collab::CollabError| CrdtError::Collab(e.to_string());
        if !self.has_projection() {
            return Err(CrdtError::Refused("This note has no text to split.".into()));
        }
        let schema = std::rc::Rc::new(rinch_editor_core::Schema::starter_kit());
        let mut session = CollabSession::from_bytes(&self.save()).map_err(collab)?;
        let model = session.projected_doc(&schema).map_err(collab)?;
        let at = session
            .resolve_sticky(&model, sticky)
            .ok_or_else(|| CrdtError::Refused("That place in the text is gone, so nothing was split.".into()))?;
        let at = split_point(&model, at.0)?;
        let end = model.content_size();
        let (before, after) = (model.cut(0, at), model.cut(at, end));
        let empty = |half: &rinch_editor_core::Node| half.child_count() == 0 || markdown::is_blank(half);
        if empty(&before) {
            return Err(CrdtError::Refused("Nothing comes before the caret, so there is nothing to split off.".into()));
        }
        if empty(&after) {
            return Err(CrdtError::Refused("Nothing comes after the caret, so there is nothing to split off.".into()));
        }
        let rest = {
            let state = rinch_editor_core::EditorState::create(schema.clone(), after.clone(), rinch_editor_core::default_plugins());
            CollabSession::new(&state).map_err(collab)?.snapshot()
        };
        let keys: Vec<String> = data_keys_named(&after).into_iter().filter(|k| !data_keys_named(&before).contains(k)).collect();
        session.record_local(&schema, &model, &before).map_err(collab)?;
        let delta = session.save_incremental().map_err(collab)?;
        self.apply_update(&delta)?;
        let mut txn = self.doc.transact_mut();
        for key in &keys {
            self.data.remove(&mut txn, key);
        }
        Ok(SplitOff { rest, keys })
    }

    /// Put `other`'s text after this node's (instead of it, while this one is
    /// blank), as an edit of this node's own text: what joining the node below
    /// into this one writes. The `data` of `other`'s canvases and sheets comes
    /// too.
    pub fn append_content_of(&mut self, other: &NodeDoc) -> Result<()> {
        if !other.has_projection() {
            return Ok(());
        }
        let theirs = model_of(&other.save(), false)?;
        if markdown::is_blank(&theirs) {
            return Ok(());
        }
        let seed = !self.has_projection();
        let delta = model_edit_delta(&self.save(), seed, |_schema, mine| {
            if markdown::is_blank(mine) {
                return Ok(theirs.clone());
            }
            Ok(mine.copy_with_content(mine.content().append(theirs.content())))
        })?;
        self.apply_update(&delta)?;
        let keys = data_keys_named(&theirs);
        self.copy_data_keys(other, &keys);
        Ok(())
    }

    /// The `data` entries `keys` of `from`, copied into this node with their
    /// yrs types (a canvas's elements, a sheet's rows). A key `from` lacks is
    /// left out.
    pub fn copy_data_keys(&mut self, from: &NodeDoc, keys: &[String]) {
        let theirs = from.doc.transact();
        let mut txn = self.doc.transact_mut();
        for key in keys {
            if let Some(value) = from.data.get(&theirs, key) {
                self.data.insert(&mut txn, key.clone(), out_to_in(&theirs, value));
            }
        }
    }

    /// On a node joined into another, the node it became: links to it lead
    /// there (docs/LINKS_CONTRACT.md "`became`").
    pub fn set_became(&mut self, url: &PimbleUrl) {
        let mut txn = self.doc.transact_mut();
        self.write_became(&mut txn, url);
    }
}

/// Where a split at `at` falls (see [`NodeDoc::split_off`]).
fn split_point(model: &rinch_editor_core::Node, at: usize) -> Result<usize> {
    let resolved = model.resolve(rinch_editor_core::Pos(at)).map_err(|e| CrdtError::Collab(e.to_string()))?;
    // A table never splits: the split goes before it.
    if (1..=resolved.depth()).any(|d| resolved.node(d).type_name() == "table") {
        return Ok(resolved.before(1).unwrap_or(at));
    }
    let depth = resolved.depth();
    let parent = resolved.parent();
    if depth >= 1 && parent.is_textblock() {
        if resolved.parent_offset() == 0 {
            return Ok(resolved.before(depth).unwrap_or(at));
        }
        if resolved.parent_offset() == parent.content_size() {
            return Ok(resolved.after(depth).unwrap_or(at));
        }
    }
    Ok(at)
}

/// The `data` keys the canvases and sheets in `model` live under, in the
/// order they appear, each once.
pub(super) fn data_keys_named(model: &rinch_editor_core::Node) -> Vec<String> {
    fn walk(node: &rinch_editor_core::Node, out: &mut Vec<String>) {
        for (attr, prefix) in [("data-board", "board:"), (markdown::embed_attrs::SHEET, "sheet:")] {
            if let Some(id) = node.attrs().get_str(attr).filter(|id| !id.is_empty()) {
                let key = format!("{prefix}{id}");
                if !out.contains(&key) {
                    out.push(key);
                }
            }
        }
        for child in node.content().iter() {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(model, &mut out);
    out
}
