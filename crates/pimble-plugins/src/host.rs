//! WASM plugin host

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use tracing::info;

use crate::error::{PluginError, Result};
use crate::interface::{NodePlugin, NodeSchema, PluginInfo, RenderOutput, ValidationResult};

/// Plugin host that manages WASM plugins
pub struct PluginHost {
    /// Registered plugins by node type
    plugins: HashMap<String, Arc<dyn NodePlugin>>,
}

impl PluginHost {
    /// Create a new plugin host
    pub fn new() -> Self {
        Self {
            plugins: HashMap::new(),
        }
    }

    /// Register a built-in plugin
    pub fn register(&mut self, plugin: impl NodePlugin + 'static) {
        let node_type = plugin.node_type().to_string();
        info!("Registering plugin for node type: {}", node_type);
        self.plugins.insert(node_type, Arc::new(plugin));
    }

    /// Load a WASM plugin from file
    pub async fn load_wasm(&mut self, _path: impl AsRef<Path>) -> Result<()> {
        // TODO: Implement WASM loading in Phase 6
        // Will use wasmtime to load and instantiate the plugin
        Err(PluginError::LoadError(
            "WASM plugins not yet implemented".to_string(),
        ))
    }

    /// Get a plugin by node type
    pub fn get(&self, node_type: &str) -> Option<Arc<dyn NodePlugin>> {
        self.plugins.get(node_type).cloned()
    }

    /// List all registered plugins
    pub fn list(&self) -> Vec<PluginInfo> {
        self.plugins.values().map(|p| p.info()).collect()
    }

    /// Check if a node type is supported
    pub fn supports(&self, node_type: &str) -> bool {
        self.plugins.contains_key(node_type)
    }
}

impl Default for PluginHost {
    fn default() -> Self {
        Self::new()
    }
}

/// Built-in document plugin
pub struct DocumentPlugin;

impl NodePlugin for DocumentPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            id: "builtin.document".to_string(),
            name: "Document".to_string(),
            version: "0.1.0".to_string(),
            node_type: "document".to_string(),
            description: "Markdown document with rich text support".to_string(),
        }
    }

    fn node_type(&self) -> &str {
        "document"
    }

    fn schema(&self) -> NodeSchema {
        use crate::interface::{FieldType, SchemaField};

        NodeSchema {
            version: 1,
            fields: vec![SchemaField {
                name: "text".to_string(),
                field_type: FieldType::Text,
                required: false,
                default: Some(serde_json::json!("")),
            }],
        }
    }

    fn render(&self, _content: &[u8]) -> Result<RenderOutput> {
        // TODO: Implement proper rendering
        Ok(RenderOutput {
            widgets: serde_json::json!({
                "type": "text_editor",
                "content": ""
            }),
            actions: vec![],
        })
    }

    fn extract_text(&self, content: &[u8]) -> Result<String> {
        // Load as CRDT document and extract text
        match pimble_crdt::DocumentContent::load(content) {
            Ok(doc) => doc.get_text().map_err(|e| PluginError::ExecutionError(e.to_string())),
            Err(e) => Err(PluginError::ExecutionError(e.to_string())),
        }
    }

    fn validate(&self, _content: &[u8]) -> Result<ValidationResult> {
        Ok(ValidationResult::ok())
    }

    fn init_content(&self) -> Result<Vec<u8>> {
        let mut doc = pimble_crdt::DocumentContent::new();
        Ok(doc.save())
    }
}

/// Built-in folder plugin
pub struct FolderPlugin;

impl NodePlugin for FolderPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            id: "builtin.folder".to_string(),
            name: "Folder".to_string(),
            version: "0.1.0".to_string(),
            node_type: "folder".to_string(),
            description: "Container node for organizing other nodes".to_string(),
        }
    }

    fn node_type(&self) -> &str {
        "folder"
    }

    fn schema(&self) -> NodeSchema {
        NodeSchema {
            version: 1,
            fields: vec![],
        }
    }

    fn render(&self, _content: &[u8]) -> Result<RenderOutput> {
        Ok(RenderOutput {
            widgets: serde_json::json!({
                "type": "folder_view"
            }),
            actions: vec![],
        })
    }

    fn extract_text(&self, _content: &[u8]) -> Result<String> {
        Ok(String::new())
    }

    fn validate(&self, _content: &[u8]) -> Result<ValidationResult> {
        Ok(ValidationResult::ok())
    }

    fn init_content(&self) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }
}

/// Create a plugin host with built-in plugins registered
pub fn create_default_host() -> PluginHost {
    let mut host = PluginHost::new();
    host.register(DocumentPlugin);
    host.register(FolderPlugin);
    host
}
