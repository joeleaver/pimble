//! Plugin interface definitions

use serde::{Deserialize, Serialize};

use crate::PluginError;

/// Plugin information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    /// Unique plugin identifier
    pub id: String,

    /// Human-readable name
    pub name: String,

    /// Version string
    pub version: String,

    /// Node type this plugin handles
    pub node_type: String,

    /// Description
    pub description: String,
}

/// Schema definition for node content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeSchema {
    /// Schema version
    pub version: u32,

    /// Field definitions
    pub fields: Vec<SchemaField>,
}

/// A field in a node schema
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaField {
    /// Field name
    pub name: String,

    /// Field type
    pub field_type: FieldType,

    /// Whether this field is required
    pub required: bool,

    /// Default value (JSON)
    pub default: Option<serde_json::Value>,
}

/// Field types supported in schemas
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    String,
    Text,
    Integer,
    Float,
    Boolean,
    DateTime,
    NodeRef,
    Array(Box<FieldType>),
    Object(Vec<SchemaField>),
}

/// Output from plugin rendering
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderOutput {
    /// Widget tree definition (JSON)
    pub widgets: serde_json::Value,

    /// Actions available on this content
    pub actions: Vec<PluginAction>,
}

/// An action a plugin can perform
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginAction {
    /// Action identifier
    pub id: String,

    /// Display label
    pub label: String,

    /// Icon (optional)
    pub icon: Option<String>,

    /// Keyboard shortcut (optional)
    pub shortcut: Option<String>,
}

/// Result of content validation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    /// Whether the content is valid
    pub valid: bool,

    /// Validation errors
    pub errors: Vec<ValidationError>,
}

/// A validation error
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationError {
    /// Field path (e.g., "content.text")
    pub path: String,

    /// Error message
    pub message: String,
}

impl ValidationResult {
    pub fn ok() -> Self {
        Self {
            valid: true,
            errors: Vec::new(),
        }
    }

    pub fn error(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            valid: false,
            errors: vec![ValidationError {
                path: path.into(),
                message: message.into(),
            }],
        }
    }
}

/// Trait that plugins must implement
///
/// This will be used with WASM plugins in Phase 6
pub trait NodePlugin: Send + Sync {
    /// Get plugin information
    fn info(&self) -> PluginInfo;

    /// Get the node type this plugin handles
    fn node_type(&self) -> &str;

    /// Get the schema for this node type
    fn schema(&self) -> NodeSchema;

    /// Render node content for display
    fn render(&self, content: &[u8]) -> Result<RenderOutput, PluginError>;

    /// Extract searchable text from content
    fn extract_text(&self, content: &[u8]) -> Result<String, PluginError>;

    /// Validate content against schema
    fn validate(&self, content: &[u8]) -> Result<ValidationResult, PluginError>;

    /// Initialize content for a new node
    fn init_content(&self) -> Result<Vec<u8>, PluginError>;
}
