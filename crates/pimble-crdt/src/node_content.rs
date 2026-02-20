//! Node content management using CRDT documents

use automerge::{transaction::Transactable, ObjType, ReadDoc};

use crate::{CrdtDocument, CrdtError, Result};

/// Content for a document node (markdown/rich text)
pub struct DocumentContent {
    doc: CrdtDocument,
}

impl DocumentContent {
    /// Key for the text content
    const TEXT_KEY: &'static str = "text";

    /// Create new empty document content
    pub fn new() -> Self {
        Self {
            doc: CrdtDocument::new(),
        }
    }

    /// Load document content from bytes
    pub fn load(bytes: &[u8]) -> Result<Self> {
        Ok(Self {
            doc: CrdtDocument::load(bytes)?,
        })
    }

    /// Save document content to bytes
    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    /// Get the text content
    pub fn get_text(&self) -> Result<String> {
        // Check if we have a text object
        let inner = self.doc.inner();
        match inner.get(automerge::ROOT, Self::TEXT_KEY)? {
            Some((value, id)) => match value {
                automerge::Value::Object(ObjType::Text) => {
                    Ok(inner.text(&id)?.to_string())
                }
                automerge::Value::Scalar(s) => match s.as_ref() {
                    automerge::ScalarValue::Str(s) => Ok(s.to_string()),
                    _ => Err(CrdtError::TypeMismatch {
                        expected: "text".to_string(),
                        actual: format!("{:?}", s),
                    }),
                },
                _ => Err(CrdtError::TypeMismatch {
                    expected: "text".to_string(),
                    actual: "object".to_string(),
                }),
            },
            None => Ok(String::new()),
        }
    }

    /// Set the text content (replaces all content)
    pub fn set_text(&mut self, text: &str) -> Result<()> {
        let inner = self.doc.inner_mut();

        // Delete existing text if any
        if inner.get(automerge::ROOT, Self::TEXT_KEY)?.is_some() {
            inner.delete(automerge::ROOT, Self::TEXT_KEY)?;
        }

        // Create a new text object
        let text_id = inner.put_object(automerge::ROOT, Self::TEXT_KEY, ObjType::Text)?;
        inner.splice_text(&text_id, 0, 0, text)?;

        Ok(())
    }

    /// Insert text at a position
    pub fn insert_text(&mut self, pos: usize, text: &str) -> Result<()> {
        let inner = self.doc.inner_mut();

        // Get or create the text object
        let text_id = match inner.get(automerge::ROOT, Self::TEXT_KEY)? {
            Some((automerge::Value::Object(ObjType::Text), id)) => id,
            Some(_) => {
                // Not a text object, recreate it
                inner.delete(automerge::ROOT, Self::TEXT_KEY)?;
                inner.put_object(automerge::ROOT, Self::TEXT_KEY, ObjType::Text)?
            }
            None => inner.put_object(automerge::ROOT, Self::TEXT_KEY, ObjType::Text)?,
        };

        inner.splice_text(&text_id, pos, 0, text)?;
        Ok(())
    }

    /// Delete text at a range
    pub fn delete_text(&mut self, pos: usize, len: usize) -> Result<()> {
        let inner = self.doc.inner_mut();

        match inner.get(automerge::ROOT, Self::TEXT_KEY)? {
            Some((automerge::Value::Object(ObjType::Text), id)) => {
                inner.splice_text(&id, pos, len as isize, "")?;
                Ok(())
            }
            _ => Ok(()), // No text to delete
        }
    }

    /// Get the underlying CRDT document
    pub fn document(&self) -> &CrdtDocument {
        &self.doc
    }

    /// Get mutable access to the underlying CRDT document
    pub fn document_mut(&mut self) -> &mut CrdtDocument {
        &mut self.doc
    }
}

impl Default for DocumentContent {
    fn default() -> Self {
        Self::new()
    }
}

/// Content for a folder node (no actual content, just metadata)
pub struct FolderContent {
    doc: CrdtDocument,
}

impl FolderContent {
    /// Create new folder content
    pub fn new() -> Self {
        Self {
            doc: CrdtDocument::new(),
        }
    }

    /// Load folder content from bytes
    pub fn load(bytes: &[u8]) -> Result<Self> {
        Ok(Self {
            doc: CrdtDocument::load(bytes)?,
        })
    }

    /// Save folder content to bytes
    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    /// Get the underlying CRDT document
    pub fn document(&self) -> &CrdtDocument {
        &self.doc
    }
}

impl Default for FolderContent {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_document_content() {
        let mut content = DocumentContent::new();
        content.set_text("Hello, World!").unwrap();
        assert_eq!(content.get_text().unwrap(), "Hello, World!");
    }

    #[test]
    fn test_document_insert() {
        let mut content = DocumentContent::new();
        content.set_text("Hello World").unwrap();
        content.insert_text(5, ",").unwrap();
        assert_eq!(content.get_text().unwrap(), "Hello, World");
    }

    #[test]
    fn test_document_delete() {
        let mut content = DocumentContent::new();
        content.set_text("Hello, World!").unwrap();
        content.delete_text(5, 2).unwrap();
        assert_eq!(content.get_text().unwrap(), "HelloWorld!");
    }

    #[test]
    fn test_document_save_load() {
        let mut content = DocumentContent::new();
        content.set_text("Test content").unwrap();

        let bytes = content.save();
        let loaded = DocumentContent::load(&bytes).unwrap();
        assert_eq!(loaded.get_text().unwrap(), "Test content");
    }
}
