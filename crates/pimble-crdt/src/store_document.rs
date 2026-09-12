//! CRDT-backed store document for tree structure and node metadata
//!
//! A single Automerge document per store containing:
//! - Tree structure (parent-child relationships, children ordering)
//! - Node metadata (title, type, tags, custom fields, timestamps)
//! - Node existence (create/delete)

use std::collections::HashMap;

use automerge::{transaction::Transactable, AutoCommit, ObjType, ReadDoc};
use pimble_core::NodeId;

use crate::error::{CrdtError, Result};

/// Information about a node read from the store document
#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub id: NodeId,
    pub parent_id: Option<NodeId>,
    pub node_type: String,
    pub title: String,
    pub tags: Vec<String>,
    pub custom: HashMap<String, serde_json::Value>,
    pub created_at: String,
    pub modified_at: String,
}

/// Issues found during tree validation
#[derive(Debug, Clone)]
pub enum TreeIssue {
    /// Node references a parent that doesn't exist
    OrphanNode { node_id: NodeId, missing_parent: NodeId },
    /// A child ID in a parent's children list doesn't exist
    MissingChild { parent_id: NodeId, child_id: NodeId },
    /// A child appears in multiple parents' children lists
    DuplicateChild { child_id: NodeId, parents: Vec<NodeId> },
    /// Cycle detected in the tree
    Cycle { node_ids: Vec<NodeId> },
}

/// A CRDT-backed store document managing tree structure and node metadata.
///
/// Schema:
/// ```text
/// Root Map {
///   "root_node_id": Str,
///   "nodes": Map {
///     "<uuid>": Map {
///       "parent_id": Str | Null,
///       "node_type": Str,
///       "children": List [ Str, ... ],
///       "title": Str,
///       "tags": List [ Str, ... ],
///       "custom": Map { key: ScalarValue },
///       "created_at": Str (ISO 8601),
///       "modified_at": Str (ISO 8601),
///     }
///   }
/// }
/// ```
#[derive(Debug)]
pub struct StoreDocument {
    doc: AutoCommit,
}

impl StoreDocument {
    /// Create a new store document with a root folder node.
    pub fn new(name: &str, root_node_id: NodeId) -> Result<Self> {
        let mut doc = AutoCommit::new();

        // Set root node ID
        doc.put(automerge::ROOT, "root_node_id", root_node_id.to_string())?;

        // Create nodes map
        let nodes_id = doc.put_object(automerge::ROOT, "nodes", ObjType::Map)?;

        // Create root node entry
        let now = chrono::Utc::now().to_rfc3339();
        let root_key = root_node_id.to_string();
        let node_obj = doc.put_object(&nodes_id, &root_key, ObjType::Map)?;
        // parent_id is null for root
        doc.put(&node_obj, "node_type", "folder")?;
        let _children = doc.put_object(&node_obj, "children", ObjType::List)?;
        doc.put(&node_obj, "title", name)?;
        let _tags = doc.put_object(&node_obj, "tags", ObjType::List)?;
        let _custom = doc.put_object(&node_obj, "custom", ObjType::Map)?;
        doc.put(&node_obj, "created_at", &now)?;
        doc.put(&node_obj, "modified_at", &now)?;

        Ok(Self { doc })
    }

    /// Load a store document from bytes.
    pub fn load(bytes: &[u8]) -> Result<Self> {
        let doc = AutoCommit::load(bytes)?;
        Ok(Self { doc })
    }

    /// Save the store document to bytes.
    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    /// Get the root node ID.
    pub fn root_node_id(&self) -> Result<NodeId> {
        match self.doc.get(automerge::ROOT, "root_node_id")? {
            Some((automerge::Value::Scalar(s), _)) => match s.as_ref() {
                automerge::ScalarValue::Str(s) => {
                    NodeId::parse(s).map_err(|e| CrdtError::Serialization(e.to_string()))
                }
                _ => Err(CrdtError::TypeMismatch {
                    expected: "string".into(),
                    actual: format!("{:?}", s),
                }),
            },
            _ => Err(CrdtError::KeyNotFound("root_node_id".into())),
        }
    }

    /// Get the ObjId for the "nodes" map.
    fn nodes_map(&self) -> Result<automerge::ObjId> {
        match self.doc.get(automerge::ROOT, "nodes")? {
            Some((automerge::Value::Object(ObjType::Map), id)) => Ok(id),
            _ => Err(CrdtError::KeyNotFound("nodes".into())),
        }
    }

    /// Get the ObjId for a specific node entry.
    fn node_obj(&self, node_id: NodeId) -> Result<automerge::ObjId> {
        let nodes = self.nodes_map()?;
        let key = node_id.to_string();
        match self.doc.get(&nodes, &key)? {
            Some((automerge::Value::Object(ObjType::Map), id)) => Ok(id),
            _ => Err(CrdtError::KeyNotFound(format!("node {}", node_id))),
        }
    }

    /// Get the children list ObjId for a node.
    fn children_list(&self, node_obj: &automerge::ObjId) -> Result<automerge::ObjId> {
        match self.doc.get(node_obj, "children")? {
            Some((automerge::Value::Object(ObjType::List), id)) => Ok(id),
            _ => Err(CrdtError::KeyNotFound("children".into())),
        }
    }

    /// Add a new node to the store document.
    pub fn add_node(
        &mut self,
        id: NodeId,
        parent_id: Option<NodeId>,
        node_type: &str,
        title: &str,
    ) -> Result<()> {
        let nodes = self.nodes_map()?;
        let key = id.to_string();

        // Check if node already exists
        if self.doc.get(&nodes, &key)?.is_some() {
            return Err(CrdtError::Serialization(format!("Node {} already exists", id)));
        }

        let now = chrono::Utc::now().to_rfc3339();

        // Create node map entry
        let node_obj = self.doc.put_object(&nodes, &key, ObjType::Map)?;
        if let Some(pid) = parent_id {
            self.doc.put(&node_obj, "parent_id", pid.to_string())?;
        }
        self.doc.put(&node_obj, "node_type", node_type)?;
        let _children = self.doc.put_object(&node_obj, "children", ObjType::List)?;
        self.doc.put(&node_obj, "title", title)?;
        let _tags = self.doc.put_object(&node_obj, "tags", ObjType::List)?;
        let _custom = self.doc.put_object(&node_obj, "custom", ObjType::Map)?;
        self.doc.put(&node_obj, "created_at", &now)?;
        self.doc.put(&node_obj, "modified_at", &now)?;

        // Append to parent's children list
        if let Some(pid) = parent_id {
            let parent_obj = self.node_obj(pid)?;
            let parent_children = self.children_list(&parent_obj)?;
            let len = self.doc.length(&parent_children);
            self.doc.insert(&parent_children, len, id.to_string())?;
        }

        Ok(())
    }

    /// Remove a node from the store document.
    pub fn remove_node(&mut self, id: NodeId) -> Result<()> {
        // Get parent ID first
        let parent_id = self.get_parent_id(id)?;

        // Remove from parent's children list
        if let Some(pid) = parent_id {
            self.remove_from_children_list(pid, id)?;
        }

        // Remove from nodes map
        let nodes = self.nodes_map()?;
        let key = id.to_string();
        self.doc.delete(&nodes, &key)?;

        Ok(())
    }

    /// Move a node to a new parent at a specific position.
    pub fn move_node(
        &mut self,
        id: NodeId,
        new_parent_id: NodeId,
        position: Option<usize>,
    ) -> Result<()> {
        // Get old parent
        let old_parent_id = self.get_parent_id(id)?
            .ok_or_else(|| CrdtError::Serialization("Cannot move root node".into()))?;

        // Remove from old parent's children
        self.remove_from_children_list(old_parent_id, id)?;

        // Add to new parent's children at position
        let new_parent_obj = self.node_obj(new_parent_id)?;
        let children = self.children_list(&new_parent_obj)?;
        let len = self.doc.length(&children);
        let pos = position.map(|p| p.min(len)).unwrap_or(len);
        self.doc.insert(&children, pos, id.to_string())?;

        // Update parent_id on the node
        let node_obj = self.node_obj(id)?;
        self.doc.put(&node_obj, "parent_id", new_parent_id.to_string())?;

        // Update modified_at
        let now = chrono::Utc::now().to_rfc3339();
        self.doc.put(&node_obj, "modified_at", &now)?;

        Ok(())
    }

    /// Set the title of a node.
    pub fn set_title(&mut self, id: NodeId, title: &str) -> Result<()> {
        let node_obj = self.node_obj(id)?;
        self.doc.put(&node_obj, "title", title)?;
        let now = chrono::Utc::now().to_rfc3339();
        self.doc.put(&node_obj, "modified_at", &now)?;
        Ok(())
    }

    /// Set the tags of a node.
    pub fn set_tags(&mut self, id: NodeId, tags: &[String]) -> Result<()> {
        let node_obj = self.node_obj(id)?;
        // Replace tags list
        let tags_list = self.doc.put_object(&node_obj, "tags", ObjType::List)?;
        for (i, tag) in tags.iter().enumerate() {
            self.doc.insert(&tags_list, i, tag.as_str())?;
        }
        let now = chrono::Utc::now().to_rfc3339();
        self.doc.put(&node_obj, "modified_at", &now)?;
        Ok(())
    }

    /// Set a custom metadata field on a node.
    pub fn set_custom(&mut self, id: NodeId, key: &str, value: &serde_json::Value) -> Result<()> {
        let node_obj = self.node_obj(id)?;
        let custom = match self.doc.get(&node_obj, "custom")? {
            Some((automerge::Value::Object(ObjType::Map), id)) => id,
            _ => return Err(CrdtError::KeyNotFound("custom".into())),
        };
        // Store as JSON string for simplicity with complex values
        let json_str = serde_json::to_string(value)
            .map_err(|e| CrdtError::Serialization(e.to_string()))?;
        self.doc.put(&custom, key, json_str)?;
        let now = chrono::Utc::now().to_rfc3339();
        self.doc.put(&node_obj, "modified_at", &now)?;
        Ok(())
    }

    /// Get node info from the store document.
    pub fn get_node_info(&self, id: NodeId) -> Result<NodeInfo> {
        let node_obj = self.node_obj(id)?;

        let parent_id = self.get_parent_id(id)?;
        let node_type = self.get_str_field(&node_obj, "node_type")?;
        let title = self.get_str_field(&node_obj, "title")?;
        let created_at = self.get_str_field(&node_obj, "created_at")?;
        let modified_at = self.get_str_field(&node_obj, "modified_at")?;

        // Read tags
        let tags = match self.doc.get(&node_obj, "tags")? {
            Some((automerge::Value::Object(ObjType::List), tags_id)) => {
                let len = self.doc.length(&tags_id);
                let mut tags = Vec::with_capacity(len);
                for i in 0..len {
                    if let Some((automerge::Value::Scalar(s), _)) = self.doc.get(&tags_id, i)? {
                        if let automerge::ScalarValue::Str(tag) = s.as_ref() {
                            tags.push(tag.to_string());
                        }
                    }
                }
                tags
            }
            _ => Vec::new(),
        };

        // Read custom fields
        let custom = match self.doc.get(&node_obj, "custom")? {
            Some((automerge::Value::Object(ObjType::Map), custom_id)) => {
                let mut custom = HashMap::new();
                let keys = self.doc.keys(&custom_id);
                for key in keys {
                    if let Some((automerge::Value::Scalar(s), _)) = self.doc.get(&custom_id, &key)? {
                        if let automerge::ScalarValue::Str(val) = s.as_ref() {
                            if let Ok(parsed) = serde_json::from_str(val) {
                                custom.insert(key, parsed);
                            }
                        }
                    }
                }
                custom
            }
            _ => HashMap::new(),
        };

        Ok(NodeInfo {
            id,
            parent_id,
            node_type,
            title,
            tags,
            custom,
            created_at,
            modified_at,
        })
    }

    /// Get the ordered children of a node.
    pub fn get_children(&self, id: NodeId) -> Result<Vec<NodeId>> {
        let node_obj = self.node_obj(id)?;
        let children = self.children_list(&node_obj)?;
        let len = self.doc.length(&children);
        let mut result = Vec::with_capacity(len);
        for i in 0..len {
            if let Some((automerge::Value::Scalar(s), _)) = self.doc.get(&children, i)? {
                if let automerge::ScalarValue::Str(child_id_str) = s.as_ref() {
                    if let Ok(child_id) = NodeId::parse(child_id_str) {
                        result.push(child_id);
                    }
                }
            }
        }
        Ok(result)
    }

    /// List all node IDs in the store document.
    pub fn list_node_ids(&self) -> Result<Vec<NodeId>> {
        let nodes = self.nodes_map()?;
        let keys = self.doc.keys(&nodes);
        let mut ids = Vec::new();
        for key in keys {
            if let Ok(id) = NodeId::parse(&key) {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    /// Check if a node exists in the store document.
    pub fn has_node(&self, id: NodeId) -> bool {
        self.node_obj(id).is_ok()
    }

    /// Get the current heads (for sync protocol).
    pub fn get_heads(&mut self) -> Vec<automerge::ChangeHash> {
        self.doc.get_heads()
    }

    /// Get the underlying AutoCommit document for advanced operations.
    pub fn inner(&self) -> &AutoCommit {
        &self.doc
    }

    /// Get mutable access to the underlying AutoCommit document.
    pub fn inner_mut(&mut self) -> &mut AutoCommit {
        &mut self.doc
    }

    /// Fork this document (create an independent copy for merge testing).
    pub fn fork(&mut self) -> Self {
        Self {
            doc: self.doc.fork(),
        }
    }

    /// Merge another store document into this one.
    pub fn merge(&mut self, other: &mut StoreDocument) -> Result<()> {
        self.doc.merge(&mut other.doc)?;
        Ok(())
    }

    /// Validate the tree structure and return any issues found.
    pub fn validate_tree(&self) -> Result<Vec<TreeIssue>> {
        let mut issues = Vec::new();
        let node_ids = self.list_node_ids()?;
        let node_set: std::collections::HashSet<NodeId> = node_ids.iter().copied().collect();

        // Track which parent each child belongs to
        let mut child_to_parents: HashMap<NodeId, Vec<NodeId>> = HashMap::new();

        for &node_id in &node_ids {
            // Check parent exists
            if let Ok(Some(parent_id)) = self.get_parent_id_result(node_id) {
                if !node_set.contains(&parent_id) {
                    issues.push(TreeIssue::OrphanNode {
                        node_id,
                        missing_parent: parent_id,
                    });
                }
            }

            // Check children exist
            if let Ok(children) = self.get_children(node_id) {
                for child_id in children {
                    if !node_set.contains(&child_id) {
                        issues.push(TreeIssue::MissingChild {
                            parent_id: node_id,
                            child_id,
                        });
                    }
                    child_to_parents
                        .entry(child_id)
                        .or_default()
                        .push(node_id);
                }
            }
        }

        // Check for children in multiple parents
        for (child_id, parents) in &child_to_parents {
            if parents.len() > 1 {
                issues.push(TreeIssue::DuplicateChild {
                    child_id: *child_id,
                    parents: parents.clone(),
                });
            }
        }

        Ok(issues)
    }

    // ── Private helpers ─────────────────────────────────────────────

    fn get_parent_id(&self, id: NodeId) -> Result<Option<NodeId>> {
        self.get_parent_id_result(id)
    }

    fn get_parent_id_result(&self, id: NodeId) -> Result<Option<NodeId>> {
        let node_obj = self.node_obj(id)?;
        match self.doc.get(&node_obj, "parent_id")? {
            Some((automerge::Value::Scalar(s), _)) => match s.as_ref() {
                automerge::ScalarValue::Str(pid_str) => {
                    let pid = NodeId::parse(pid_str)
                        .map_err(|e| CrdtError::Serialization(e.to_string()))?;
                    Ok(Some(pid))
                }
                automerge::ScalarValue::Null => Ok(None),
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    fn get_str_field(&self, obj: &automerge::ObjId, key: &str) -> Result<String> {
        match self.doc.get(obj, key)? {
            Some((automerge::Value::Scalar(s), _)) => match s.as_ref() {
                automerge::ScalarValue::Str(s) => Ok(s.to_string()),
                _ => Err(CrdtError::TypeMismatch {
                    expected: "string".into(),
                    actual: format!("{:?}", s),
                }),
            },
            _ => Ok(String::new()),
        }
    }

    fn remove_from_children_list(&mut self, parent_id: NodeId, child_id: NodeId) -> Result<()> {
        let parent_obj = self.node_obj(parent_id)?;
        let children = self.children_list(&parent_obj)?;
        let len = self.doc.length(&children);
        let child_str = child_id.to_string();

        for i in (0..len).rev() {
            if let Some((automerge::Value::Scalar(s), _)) = self.doc.get(&children, i)? {
                if let automerge::ScalarValue::Str(val) = s.as_ref() {
                    if val == &child_str {
                        self.doc.delete(&children, i)?;
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    /// Update the modified_at timestamp of a node to now.
    pub fn touch_modified(&mut self, id: NodeId) -> Result<()> {
        let node_obj = self.node_obj(id)?;
        let now = chrono::Utc::now().to_rfc3339();
        self.doc.put(&node_obj, "modified_at", &now)?;
        Ok(())
    }

    /// Set the node type of a node.
    pub fn set_node_type(&mut self, id: NodeId, node_type: &str) -> Result<()> {
        let node_obj = self.node_obj(id)?;
        self.doc.put(&node_obj, "node_type", node_type)?;
        Ok(())
    }

    /// Set the parent_id of a node (used during migration).
    pub fn set_parent_id(&mut self, id: NodeId, parent_id: Option<NodeId>) -> Result<()> {
        let node_obj = self.node_obj(id)?;
        if let Some(pid) = parent_id {
            self.doc.put(&node_obj, "parent_id", pid.to_string())?;
        }
        Ok(())
    }

    /// Append a child to a node's children list (used during migration).
    pub fn append_child(&mut self, parent_id: NodeId, child_id: NodeId) -> Result<()> {
        let parent_obj = self.node_obj(parent_id)?;
        let children = self.children_list(&parent_obj)?;
        let len = self.doc.length(&children);
        self.doc.insert(&children, len, child_id.to_string())?;
        Ok(())
    }

    /// Set timestamps on a node (used during migration).
    pub fn set_timestamps(&mut self, id: NodeId, created_at: &str, modified_at: &str) -> Result<()> {
        let node_obj = self.node_obj(id)?;
        self.doc.put(&node_obj, "created_at", created_at)?;
        self.doc.put(&node_obj, "modified_at", modified_at)?;
        Ok(())
    }

    /// Add a bare node entry (no parent link, no children append) for migration.
    pub fn add_node_bare(
        &mut self,
        id: NodeId,
        node_type: &str,
        title: &str,
        created_at: &str,
        modified_at: &str,
    ) -> Result<()> {
        let nodes = self.nodes_map()?;
        let key = id.to_string();
        let node_obj = self.doc.put_object(&nodes, &key, ObjType::Map)?;
        self.doc.put(&node_obj, "node_type", node_type)?;
        let _children = self.doc.put_object(&node_obj, "children", ObjType::List)?;
        self.doc.put(&node_obj, "title", title)?;
        let _tags = self.doc.put_object(&node_obj, "tags", ObjType::List)?;
        let _custom = self.doc.put_object(&node_obj, "custom", ObjType::Map)?;
        self.doc.put(&node_obj, "created_at", created_at)?;
        self.doc.put(&node_obj, "modified_at", modified_at)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_store_document() {
        let root_id = NodeId::new();
        let doc = StoreDocument::new("Test Store", root_id).unwrap();
        assert_eq!(doc.root_node_id().unwrap(), root_id);
        assert!(doc.has_node(root_id));

        let info = doc.get_node_info(root_id).unwrap();
        assert_eq!(info.title, "Test Store");
        assert_eq!(info.node_type, "folder");
        assert!(info.parent_id.is_none());
    }

    #[test]
    fn test_add_and_get_node() {
        let root_id = NodeId::new();
        let mut doc = StoreDocument::new("Store", root_id).unwrap();

        let child_id = NodeId::new();
        doc.add_node(child_id, Some(root_id), "document", "My Doc").unwrap();

        assert!(doc.has_node(child_id));
        let info = doc.get_node_info(child_id).unwrap();
        assert_eq!(info.title, "My Doc");
        assert_eq!(info.node_type, "document");
        assert_eq!(info.parent_id, Some(root_id));

        let children = doc.get_children(root_id).unwrap();
        assert_eq!(children, vec![child_id]);
    }

    #[test]
    fn test_remove_node() {
        let root_id = NodeId::new();
        let mut doc = StoreDocument::new("Store", root_id).unwrap();

        let child_id = NodeId::new();
        doc.add_node(child_id, Some(root_id), "document", "Doc").unwrap();
        doc.remove_node(child_id).unwrap();

        assert!(!doc.has_node(child_id));
        let children = doc.get_children(root_id).unwrap();
        assert!(children.is_empty());
    }

    #[test]
    fn test_move_node() {
        let root_id = NodeId::new();
        let mut doc = StoreDocument::new("Store", root_id).unwrap();

        let folder_a = NodeId::new();
        let folder_b = NodeId::new();
        let child = NodeId::new();

        doc.add_node(folder_a, Some(root_id), "folder", "A").unwrap();
        doc.add_node(folder_b, Some(root_id), "folder", "B").unwrap();
        doc.add_node(child, Some(folder_a), "document", "Doc").unwrap();

        assert_eq!(doc.get_children(folder_a).unwrap(), vec![child]);
        assert!(doc.get_children(folder_b).unwrap().is_empty());

        doc.move_node(child, folder_b, None).unwrap();

        assert!(doc.get_children(folder_a).unwrap().is_empty());
        assert_eq!(doc.get_children(folder_b).unwrap(), vec![child]);

        let info = doc.get_node_info(child).unwrap();
        assert_eq!(info.parent_id, Some(folder_b));
    }

    #[test]
    fn test_move_node_at_position() {
        let root_id = NodeId::new();
        let mut doc = StoreDocument::new("Store", root_id).unwrap();

        let child_a = NodeId::new();
        let child_b = NodeId::new();
        let child_c = NodeId::new();
        let folder = NodeId::new();

        doc.add_node(folder, Some(root_id), "folder", "Folder").unwrap();
        doc.add_node(child_a, Some(folder), "document", "A").unwrap();
        doc.add_node(child_b, Some(folder), "document", "B").unwrap();
        doc.add_node(child_c, Some(folder), "document", "C").unwrap();

        // Move C to position 0
        doc.move_node(child_c, folder, Some(0)).unwrap();
        let children = doc.get_children(folder).unwrap();
        assert_eq!(children, vec![child_c, child_a, child_b]);
    }

    #[test]
    fn test_set_title() {
        let root_id = NodeId::new();
        let mut doc = StoreDocument::new("Store", root_id).unwrap();

        doc.set_title(root_id, "Renamed Store").unwrap();
        let info = doc.get_node_info(root_id).unwrap();
        assert_eq!(info.title, "Renamed Store");
    }

    #[test]
    fn test_set_tags() {
        let root_id = NodeId::new();
        let mut doc = StoreDocument::new("Store", root_id).unwrap();

        let child = NodeId::new();
        doc.add_node(child, Some(root_id), "document", "Doc").unwrap();

        doc.set_tags(child, &["tag1".into(), "tag2".into()]).unwrap();
        let info = doc.get_node_info(child).unwrap();
        assert_eq!(info.tags, vec!["tag1", "tag2"]);
    }

    #[test]
    fn test_set_custom() {
        let root_id = NodeId::new();
        let mut doc = StoreDocument::new("Store", root_id).unwrap();

        let child = NodeId::new();
        doc.add_node(child, Some(root_id), "document", "Doc").unwrap();

        let val = serde_json::json!(true);
        doc.set_custom(child, "explicit_title", &val).unwrap();
        let info = doc.get_node_info(child).unwrap();
        assert_eq!(info.custom.get("explicit_title"), Some(&val));
    }

    #[test]
    fn test_save_load() {
        let root_id = NodeId::new();
        let mut doc = StoreDocument::new("Store", root_id).unwrap();

        let child = NodeId::new();
        doc.add_node(child, Some(root_id), "document", "Doc").unwrap();

        let bytes = doc.save();
        let loaded = StoreDocument::load(&bytes).unwrap();

        assert_eq!(loaded.root_node_id().unwrap(), root_id);
        assert!(loaded.has_node(child));
        let info = loaded.get_node_info(child).unwrap();
        assert_eq!(info.title, "Doc");
    }

    #[test]
    fn test_list_node_ids() {
        let root_id = NodeId::new();
        let mut doc = StoreDocument::new("Store", root_id).unwrap();

        let child1 = NodeId::new();
        let child2 = NodeId::new();
        doc.add_node(child1, Some(root_id), "document", "A").unwrap();
        doc.add_node(child2, Some(root_id), "document", "B").unwrap();

        let ids = doc.list_node_ids().unwrap();
        assert_eq!(ids.len(), 3); // root + 2 children
        assert!(ids.contains(&root_id));
        assert!(ids.contains(&child1));
        assert!(ids.contains(&child2));
    }

    #[test]
    fn test_concurrent_merge() {
        let root_id = NodeId::new();
        let mut doc1 = StoreDocument::new("Store", root_id).unwrap();

        // Fork
        let mut doc2 = doc1.fork();

        // Concurrent changes: each adds a child to root
        let child_a = NodeId::new();
        let child_b = NodeId::new();
        doc1.add_node(child_a, Some(root_id), "document", "A").unwrap();
        doc2.add_node(child_b, Some(root_id), "document", "B").unwrap();

        // Merge
        doc1.merge(&mut doc2).unwrap();

        // Both children should appear
        let children = doc1.get_children(root_id).unwrap();
        assert_eq!(children.len(), 2);
        assert!(children.contains(&child_a));
        assert!(children.contains(&child_b));
    }

    #[test]
    fn test_validate_tree_clean() {
        let root_id = NodeId::new();
        let mut doc = StoreDocument::new("Store", root_id).unwrap();

        let child = NodeId::new();
        doc.add_node(child, Some(root_id), "document", "Doc").unwrap();

        let issues = doc.validate_tree().unwrap();
        assert!(issues.is_empty());
    }
}
