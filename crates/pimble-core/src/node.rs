//! Node types - the fundamental unit of content in Pimble

use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

/// Unique identifier for a node
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub Uuid);

impl NodeId {
    /// Create a new random NodeId
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Create a NodeId from an existing UUID
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Parse a NodeId from a string
    pub fn parse(s: &str) -> Result<Self, uuid::Error> {
        Ok(Self(Uuid::parse_str(s)?))
    }

    /// Get the underlying UUID
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for NodeId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A Node is the fundamental unit of content in Pimble
///
/// Nodes form trees within Stores. Each node has:
/// - A unique identifier
/// - An optional parent (root nodes have no parent)
/// - A type that determines how content is interpreted
/// - Metadata (title, timestamps, tags)
/// - Content stored as CRDT data
/// - Ordered children
/// - Links to other nodes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    /// Unique identifier for this node
    pub id: NodeId,

    /// Parent node ID (None for root nodes)
    pub parent_id: Option<NodeId>,

    /// Node type identifier (e.g., "document", "folder", "image")
    pub node_type: String,

    /// Node metadata
    pub metadata: NodeMetadata,

    /// Raw CRDT content bytes (Automerge document)
    /// This is managed by pimble-crdt
    #[serde(with = "serde_bytes_base64")]
    pub content: Vec<u8>,

    /// Ordered list of child node IDs
    pub children: Vec<NodeId>,

    /// Links from this node to other nodes
    pub links: Vec<NodeLink>,
}

impl Node {
    /// Create a new node with the given type
    pub fn new(node_type: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: NodeId::new(),
            parent_id: None,
            node_type: node_type.into(),
            metadata: NodeMetadata {
                title: String::new(),
                created_at: now,
                modified_at: now,
                tags: Vec::new(),
                custom: HashMap::new(),
            },
            content: Vec::new(),
            children: Vec::new(),
            links: Vec::new(),
        }
    }

    /// Create a new folder node
    pub fn folder(title: impl Into<String>) -> Self {
        let mut node = Self::new("folder");
        node.metadata.title = title.into();
        node
    }

    /// Create a new document node
    pub fn document(title: impl Into<String>) -> Self {
        let mut node = Self::new("document");
        node.metadata.title = title.into();
        node
    }

    /// Set the parent of this node
    pub fn with_parent(mut self, parent_id: NodeId) -> Self {
        self.parent_id = Some(parent_id);
        self
    }

    /// Update the modified timestamp to now
    pub fn touch(&mut self) {
        self.metadata.modified_at = Utc::now();
    }

    /// Add a child node ID
    pub fn add_child(&mut self, child_id: NodeId) {
        if !self.children.contains(&child_id) {
            self.children.push(child_id);
            self.touch();
        }
    }

    /// Remove a child node ID
    pub fn remove_child(&mut self, child_id: &NodeId) -> bool {
        if let Some(pos) = self.children.iter().position(|id| id == child_id) {
            self.children.remove(pos);
            self.touch();
            true
        } else {
            false
        }
    }

    /// Add a link to another node
    pub fn add_link(&mut self, link: NodeLink) {
        self.links.push(link);
        self.touch();
    }
}

/// Metadata associated with a node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeMetadata {
    /// Display title for the node
    pub title: String,

    /// When the node was created
    pub created_at: DateTime<Utc>,

    /// When the node was last modified
    pub modified_at: DateTime<Utc>,

    /// Tags for categorization and search
    pub tags: Vec<String>,

    /// Custom metadata fields
    pub custom: HashMap<String, serde_json::Value>,
}

/// A link from one node to another
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeLink {
    /// Where the link points to
    pub target: LinkTarget,

    /// Type of link (e.g., "reference", "embed", "related")
    pub link_type: String,

    /// Optional anchor within the source node
    pub source_anchor: Option<String>,
}

impl NodeLink {
    /// Create a simple reference link to another node
    pub fn reference(target_id: NodeId) -> Self {
        Self {
            target: LinkTarget::Node(target_id),
            link_type: "reference".to_string(),
            source_anchor: None,
        }
    }

    /// Create an embed link to another node
    pub fn embed(target_id: NodeId) -> Self {
        Self {
            target: LinkTarget::Node(target_id),
            link_type: "embed".to_string(),
            source_anchor: None,
        }
    }

    /// Create a deep link to a specific location within a node
    pub fn deep(target_id: NodeId, anchor: impl Into<String>) -> Self {
        Self {
            target: LinkTarget::Deep {
                node_id: target_id,
                anchor: anchor.into(),
            },
            link_type: "reference".to_string(),
            source_anchor: None,
        }
    }

    /// Create an external link
    pub fn external(url: Url) -> Self {
        Self {
            target: LinkTarget::External(url),
            link_type: "external".to_string(),
            source_anchor: None,
        }
    }
}

/// Target of a link
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LinkTarget {
    /// Link to another node
    Node(NodeId),

    /// Deep link to a specific location within a node
    Deep {
        node_id: NodeId,
        /// Anchor identifier (e.g., "paragraph:3", "text:100-150")
        anchor: String,
    },

    /// Link to an external URL
    External(Url),
}

impl LinkTarget {
    /// Get the target node ID if this is an internal link
    pub fn node_id(&self) -> Option<NodeId> {
        match self {
            LinkTarget::Node(id) => Some(*id),
            LinkTarget::Deep { node_id, .. } => Some(*node_id),
            LinkTarget::External(_) => None,
        }
    }
}

/// Well-known node types
pub mod node_types {
    /// Folder node - container with no content, just children
    pub const FOLDER: &str = "folder";

    /// Document node - markdown/rich text content
    pub const DOCUMENT: &str = "document";

    /// Store node - entry point to a subtree (mounted store)
    pub const STORE: &str = "store";

    /// Image node - image content with optional annotations
    pub const IMAGE: &str = "image";

    /// Canvas node - freeform visual content
    pub const CANVAS: &str = "canvas";
}

/// Helper module for base64 encoding of bytes in serde
mod serde_bytes_base64 {
    use serde::{Deserialize, Deserializer, Serializer};
    use serde::de::Error;

    pub fn serialize<S>(bytes: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        serializer.serialize_str(&encoded)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        use base64::Engine;
        let s = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(&s)
            .map_err(|e| Error::custom(format!("base64 decode error: {}", e)))
    }
}
