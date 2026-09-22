//! Pimble CRDT - yrs documents for nodes and the tree over them
//!
//! This crate provides:
//! - The node document (`NodeDoc`): one yrs document per node holding its
//!   rich text, its place in the tree, its metadata and a plugin's JSON
//!   (docs/NODE_DOCUMENT_CONTRACT.md)
//! - The tree over node documents (`Tree`): the operations, validation and
//!   repair, each edit reported per document (`TreeEdit`); a node never leaves
//!   a share, so a move that would is a transplant (docs/MOVE_CONTRACT.md)
//! - The previous layout, until wave 2 switches the workspace over: per-node
//!   content documents (`ContentDoc`) and the store document (`StoreDocument`)
//! - Shared error types

pub mod blocks;
pub mod content_doc;
pub mod error;
pub mod node_doc;
pub mod store_document;
pub mod tree;
mod sync_util;

pub use blocks::{blocks_from_plain_text, Align, Block, ListItem, Mark, Run};
pub use content_doc::*;
pub use error::*;
pub use node_doc::{NodeDoc, NodeFields, NodeUpdateEffect, ROOT_CHILDREN, ROOT_DATA, ROOT_NODE};
pub use store_document::*;
pub use tree::{Cutting, CuttingNode, Tree, TreeEdit};
pub use sync_util::{advance_state_vector, empty_state_vector, state_vector_exceeds};
