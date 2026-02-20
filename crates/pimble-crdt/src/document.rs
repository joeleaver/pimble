//! CRDT Document wrapper around Automerge

use automerge::{transaction::Transactable, AutoCommit, Change, ChangeHash, ReadDoc};

use crate::error::{CrdtError, Result};

/// A CRDT document backed by Automerge
///
/// This provides a high-level interface for working with Automerge documents,
/// handling serialization, change tracking, and merging.
#[derive(Debug)]
pub struct CrdtDocument {
    doc: AutoCommit,
}

impl CrdtDocument {
    /// Create a new empty document
    pub fn new() -> Self {
        Self {
            doc: AutoCommit::new(),
        }
    }

    /// Load a document from bytes
    pub fn load(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() {
            return Ok(Self::new());
        }
        let doc = AutoCommit::load(bytes)?;
        Ok(Self { doc })
    }

    /// Save the document to bytes
    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    /// Get the current heads (for sync)
    pub fn get_heads(&mut self) -> Vec<ChangeHash> {
        self.doc.get_heads()
    }

    /// Get changes since the given heads
    pub fn get_changes_since(&mut self, heads: &[ChangeHash]) -> Vec<Change> {
        self.doc
            .get_changes(heads)
            .into_iter()
            .cloned()
            .collect()
    }

    /// Apply a change to the document
    pub fn apply_change(&mut self, change: Change) -> Result<()> {
        self.doc.apply_changes(vec![change])?;
        Ok(())
    }

    /// Apply multiple changes
    pub fn apply_changes(&mut self, changes: Vec<Change>) -> Result<()> {
        self.doc.apply_changes(changes)?;
        Ok(())
    }

    /// Merge another document into this one
    pub fn merge(&mut self, other: &mut CrdtDocument) -> Result<()> {
        self.doc.merge(&mut other.doc)?;
        Ok(())
    }

    /// Fork this document (create an independent copy)
    pub fn fork(&mut self) -> Self {
        Self {
            doc: self.doc.fork(),
        }
    }

    /// Set a string value at the root level
    pub fn set_string(&mut self, key: &str, value: &str) -> Result<()> {
        self.doc.put(automerge::ROOT, key, value)?;
        Ok(())
    }

    /// Get a string value from the root level
    pub fn get_string(&self, key: &str) -> Result<Option<String>> {
        match self.doc.get(automerge::ROOT, key)? {
            Some((value, _)) => match value {
                automerge::Value::Scalar(s) => match s.as_ref() {
                    automerge::ScalarValue::Str(s) => Ok(Some(s.to_string())),
                    _ => Err(CrdtError::TypeMismatch {
                        expected: "string".to_string(),
                        actual: format!("{:?}", s),
                    }),
                },
                _ => Err(CrdtError::TypeMismatch {
                    expected: "string".to_string(),
                    actual: "object".to_string(),
                }),
            },
            None => Ok(None),
        }
    }

    /// Set an integer value at the root level
    pub fn set_int(&mut self, key: &str, value: i64) -> Result<()> {
        self.doc.put(automerge::ROOT, key, value)?;
        Ok(())
    }

    /// Get an integer value from the root level
    pub fn get_int(&self, key: &str) -> Result<Option<i64>> {
        match self.doc.get(automerge::ROOT, key)? {
            Some((value, _)) => match value {
                automerge::Value::Scalar(s) => match s.as_ref() {
                    automerge::ScalarValue::Int(i) => Ok(Some(*i)),
                    automerge::ScalarValue::Uint(u) => Ok(Some(*u as i64)),
                    _ => Err(CrdtError::TypeMismatch {
                        expected: "integer".to_string(),
                        actual: format!("{:?}", s),
                    }),
                },
                _ => Err(CrdtError::TypeMismatch {
                    expected: "integer".to_string(),
                    actual: "object".to_string(),
                }),
            },
            None => Ok(None),
        }
    }

    /// Set a boolean value at the root level
    pub fn set_bool(&mut self, key: &str, value: bool) -> Result<()> {
        self.doc.put(automerge::ROOT, key, value)?;
        Ok(())
    }

    /// Get a boolean value from the root level
    pub fn get_bool(&self, key: &str) -> Result<Option<bool>> {
        match self.doc.get(automerge::ROOT, key)? {
            Some((value, _)) => match value {
                automerge::Value::Scalar(s) => match s.as_ref() {
                    automerge::ScalarValue::Boolean(b) => Ok(Some(*b)),
                    _ => Err(CrdtError::TypeMismatch {
                        expected: "boolean".to_string(),
                        actual: format!("{:?}", s),
                    }),
                },
                _ => Err(CrdtError::TypeMismatch {
                    expected: "boolean".to_string(),
                    actual: "object".to_string(),
                }),
            },
            None => Ok(None),
        }
    }

    /// Delete a key from the root level
    pub fn delete(&mut self, key: &str) -> Result<()> {
        self.doc.delete(automerge::ROOT, key)?;
        Ok(())
    }

    /// Check if a key exists at the root level
    pub fn contains_key(&self, key: &str) -> bool {
        self.doc.get(automerge::ROOT, key).ok().flatten().is_some()
    }

    /// Get the underlying AutoCommit document for advanced operations
    pub fn inner(&self) -> &AutoCommit {
        &self.doc
    }

    /// Get mutable access to the underlying AutoCommit document
    pub fn inner_mut(&mut self) -> &mut AutoCommit {
        &mut self.doc
    }
}

impl Default for CrdtDocument {
    fn default() -> Self {
        Self::new()
    }
}

// Note: CrdtDocument cannot implement Clone because fork() requires &mut self
// Use fork() explicitly when you need to copy a document

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_document() {
        let doc = CrdtDocument::new();
        assert!(!doc.contains_key("test"));
    }

    #[test]
    fn test_set_get_string() {
        let mut doc = CrdtDocument::new();
        doc.set_string("title", "Hello World").unwrap();
        assert_eq!(doc.get_string("title").unwrap(), Some("Hello World".to_string()));
    }

    #[test]
    fn test_save_load() {
        let mut doc = CrdtDocument::new();
        doc.set_string("key", "value").unwrap();

        let bytes = doc.save();
        let loaded = CrdtDocument::load(&bytes).unwrap();
        assert_eq!(loaded.get_string("key").unwrap(), Some("value".to_string()));
    }

    #[test]
    fn test_merge() {
        let mut doc1 = CrdtDocument::new();
        doc1.set_string("key1", "value1").unwrap();

        let mut doc2 = doc1.fork(); // fork requires &mut self
        doc2.set_string("key2", "value2").unwrap();

        doc1.merge(&mut doc2).unwrap();
        assert_eq!(doc1.get_string("key1").unwrap(), Some("value1".to_string()));
        assert_eq!(doc1.get_string("key2").unwrap(), Some("value2".to_string()));
    }
}
