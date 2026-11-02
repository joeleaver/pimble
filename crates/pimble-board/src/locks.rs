//! Locks (docs/IMAGE_MARKUP_CONTRACT.md section 5): a guard against
//! accidents, for everyone, not a permission. [`crate::BoardTxn`] refuses what
//! a lock guards, so the app, the MCP and anything else writing a board get
//! the same refusals and the same sentences; the reads here are the same
//! rules over a [`BoardSnapshot`], for a caller deciding what to offer before
//! it writes (what a press can pick, which layer a tool draws in).

use crate::doc::{BoardError, LockedLayer, Result};
use crate::snapshot::{BoardSnapshot, Element, Layer};

/// The sentences a person reads for each refusal (the `Display` of the
/// matching [`BoardError`]); a caller with something to say before it
/// writes says the same words.
pub const ELEMENT_LOCKED: &str = "That is locked. Unlock it in the layers panel or with Arrange > Unlock.";
pub const LAYER_LOCKED: &str = "That layer is locked. Unlock it, or pick another layer to draw in.";
pub const LAYER_LOCKED_AGAINST_REMOVAL: &str = "That layer is locked. Unlock it to delete or merge it.";
pub const EVERY_LAYER_LOCKED: &str = "Every layer is locked. Unlock one, or add a layer to draw in.";

/// The one judgement of an element edit: refused when the element is locked
/// or the layer it is drawn in is (`layer`, when that layer is locked). The
/// layer's lock is named first: unlocking the element would not help.
pub(crate) fn judge_element(id: &str, element_locked: bool, layer: Option<LockedLayer>) -> Result<()> {
    match (element_locked, layer) {
        (_, Some(layer)) => Err(BoardError::ElementLocked { id: id.to_string(), layer: Some(layer) }),
        (true, None) => Err(BoardError::ElementLocked { id: id.to_string(), layer: None }),
        (false, None) => Ok(()),
    }
}

fn locked(layer: &Layer) -> LockedLayer {
    LockedLayer { id: layer.id.clone(), name: layer.name.clone() }
}

/// What [`crate::BoardTxn::delete_elements`] did: the ids it deleted and the
/// locked ones it kept.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Deleted {
    pub deleted: Vec<String>,
    pub kept: Vec<String>,
}

impl Deleted {
    /// The sentence that says what was kept, when anything was.
    pub fn kept_sentence(&self) -> Option<String> {
        match self.kept.len() {
            0 => None,
            1 => Some("1 locked element was kept. Unlock it to delete it.".to_string()),
            n => Some(format!("{n} locked elements were kept. Unlock them to delete them.")),
        }
    }
}

impl BoardSnapshot {
    /// Whether element `id` takes edits: it exists, and neither it nor the
    /// layer it is drawn in is locked.
    pub fn editable(&self, id: &str) -> bool {
        self.judge_edit(id).is_ok()
    }

    /// Element `id` for an edit, or the refusal [`crate::BoardTxn`] would
    /// give.
    pub fn judge_edit(&self, id: &str) -> Result<&Element> {
        let e = self.get(id).ok_or_else(|| BoardError::NoElement(id.to_string()))?;
        judge_element(id, e.raw.locked, self.layer(&e.layer).filter(|l| l.locked).map(locked))?;
        Ok(e)
    }

    /// Whether the layer an element is drawn in is locked (what a press, a
    /// marquee or Select All passes over: a locked picture layer is the
    /// screenshot under the marks).
    pub fn in_locked_layer(&self, e: &Element) -> bool {
        self.layer(&e.layer).is_some_and(|l| l.locked)
    }

    /// The layer something new goes into: `picked` when given (refused when
    /// it is locked), else the top layer that is not locked. The same rule
    /// [`crate::BoardTxn::add_element`] follows when no layer is named.
    pub fn layer_for_new(&self, picked: Option<&str>) -> Result<&Layer> {
        match picked {
            Some(id) => {
                let layer = self.layer(id).ok_or_else(|| BoardError::NoLayer(id.to_string()))?;
                if layer.locked {
                    return Err(BoardError::LayerLocked(locked(layer)));
                }
                Ok(layer)
            }
            None => self.layers.iter().rev().find(|l| !l.locked).ok_or(BoardError::EveryLayerLocked),
        }
    }
}
