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
        Ok(pimble_crdt::ContentDoc::text_of(content))
    }

    fn index_units(&self, content: &[u8]) -> Result<Vec<pimble_core::IndexUnit>> {
        Ok(pimble_crdt::ContentDoc::units_of(content))
    }

    fn validate(&self, _content: &[u8]) -> Result<ValidationResult> {
        Ok(ValidationResult::ok())
    }

    fn init_content(&self) -> Result<Vec<u8>> {
        Ok(pimble_crdt::ContentDoc::new().save())
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

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_core::UnitKind;

    #[test]
    fn document_plugin_index_units_delegates_to_content_doc() {
        let content = pimble_crdt::ContentDoc::from_plain_text("hello\nworld").unwrap();
        let units = DocumentPlugin.index_units(&content.save()).unwrap();
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].kind, UnitKind::Prose);
        assert_eq!(units[0].text, "hello");
        assert_eq!(units[1].text, "world");
    }

    #[test]
    fn document_plugin_empty_content_yields_no_units() {
        let empty = pimble_crdt::ContentDoc::new().save();
        // An empty ContentDoc still projects to one empty paragraph.
        let units = DocumentPlugin.index_units(&empty).unwrap();
        assert!(units.iter().all(|u| u.text.is_empty()));
    }

    #[test]
    fn folder_plugin_uses_default_index_units_impl() {
        let units = FolderPlugin.index_units(b"anything").unwrap();
        assert!(units.is_empty());
    }

    /// A plugin that only implements the required trait methods gets a working
    /// `index_units` for free via the default implementation.
    struct MinimalPlugin;

    impl NodePlugin for MinimalPlugin {
        fn info(&self) -> PluginInfo {
            PluginInfo {
                id: "test.minimal".into(),
                name: "Minimal".into(),
                version: "0.0.0".into(),
                node_type: "minimal".into(),
                description: String::new(),
            }
        }
        fn node_type(&self) -> &str {
            "minimal"
        }
        fn schema(&self) -> NodeSchema {
            NodeSchema {
                version: 1,
                fields: vec![],
            }
        }
        fn render(&self, _content: &[u8]) -> Result<RenderOutput> {
            unimplemented!()
        }
        fn extract_text(&self, content: &[u8]) -> Result<String> {
            Ok(String::from_utf8_lossy(content).into_owned())
        }
        fn validate(&self, _content: &[u8]) -> Result<ValidationResult> {
            Ok(ValidationResult::ok())
        }
        fn init_content(&self) -> Result<Vec<u8>> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn default_index_units_wraps_extract_text_as_one_prose_unit() {
        let units = MinimalPlugin.index_units(b"plain text").unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].kind, UnitKind::Prose);
        assert_eq!(units[0].path, "b:0");
        assert_eq!(units[0].text, "plain text");
    }

    #[test]
    fn default_index_units_empty_text_yields_no_units() {
        let units = MinimalPlugin.index_units(b"").unwrap();
        assert!(units.is_empty());
    }
}
