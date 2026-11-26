//! What a document will be once an update is merged into it, as every
//! peer and every later open sees it: the merged state saved and loaded
//! again, which is what the store persists and what a peer that syncs
//! from it receives.
//!
//! yrs can merge an update into a state that does not survive its own
//! encoding. An update whose struct the document holds in part, under a
//! header naming a parent other than the held part's, is merged with its
//! new part linked beside the held part (its origin) but keeping the
//! header's parent; the saved bytes write the new part by its origin
//! alone, so loading them puts it where the held part is. The merged
//! document then reads one way in memory and another once loaded. No
//! honest encoder writes such an update (a struct's header is its first
//! item's, and the held first item is the same item), so an update whose
//! merge reads differently from its reload is refused: whatever accepted
//! it would disagree with its own disk and with every peer that loads it.

use yrs::types::ToJson;
use yrs::{Any, Transact};

use super::NodeDoc;
use crate::error::{CrdtError, Result};

/// The refusal of an update whose merge does not survive saving.
pub const READS_DIFFERENTLY_ONCE_SAVED: &str =
    "This update reads differently once saved than as it merges, so no copy of the document would agree with another; nothing was written.";

impl NodeDoc {
    /// `held` (or an empty document) with `update` merged, as it reads once
    /// saved and loaded again; `held` is not touched. Refused with
    /// [`READS_DIFFERENTLY_ONCE_SAVED`] when the merge reads differently in
    /// memory than once loaded ([`round_trip`](self) has why). Costs a save
    /// and two loads of the whole document: for judging the updates that
    /// need it, never for every keystroke.
    pub fn merged_as_saved(held: Option<&NodeDoc>, update: &[u8]) -> Result<NodeDoc> {
        let mut merged = match held {
            Some(doc) => NodeDoc::load(&doc.save())?,
            None => NodeDoc::new(),
        };
        merged.apply_update(update)?;
        let reloaded = NodeDoc::load(&merged.save())?;
        if !merged.reads_as(&reloaded) {
            return Err(CrdtError::Refused(READS_DIFFERENTLY_ONCE_SAVED.into()));
        }
        Ok(reloaded)
    }

    /// Whether every root of this document reads as `other`'s does.
    fn reads_as(&self, other: &NodeDoc) -> bool {
        let (mine, theirs) = (self.doc.transact(), other.doc.transact());
        same(&self.node.to_json(&mine), &other.node.to_json(&theirs))
            && same(&self.children.to_json(&mine), &other.children.to_json(&theirs))
            && same(&self.data.to_json(&mine), &other.data.to_json(&theirs))
            && same(&self.meta.to_json(&mine), &other.meta.to_json(&theirs))
            && same(&self.content.to_json(&mine), &other.content.to_json(&theirs))
    }
}

/// `a` and `b` are the same value, a NaN being the same as a NaN (a value
/// read twice is the same value, and `Any`'s own equality says otherwise).
fn same(a: &Any, b: &Any) -> bool {
    match (a, b) {
        (Any::Number(x), Any::Number(y)) => x == y || (x.is_nan() && y.is_nan()),
        (Any::Array(x), Any::Array(y)) => x.len() == y.len() && x.iter().zip(y.iter()).all(|(x, y)| same(x, y)),
        (Any::Map(x), Any::Map(y)) => x.len() == y.len() && x.iter().all(|(key, x)| y.get(key).is_some_and(|y| same(x, y))),
        (a, b) => a == b,
    }
}
