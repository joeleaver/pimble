//! Error types for pimble-plugins

use thiserror::Error;

#[derive(Error, Debug)]
pub enum PluginError {
    #[error("Plugin not found: {0}")]
    NotFound(String),

    #[error("Plugin load error: {0}")]
    LoadError(String),

    #[error("Plugin execution error: {0}")]
    ExecutionError(String),

    #[error("Invalid plugin: {0}")]
    InvalidPlugin(String),

    #[error("WASM error: {0}")]
    Wasm(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, PluginError>;
