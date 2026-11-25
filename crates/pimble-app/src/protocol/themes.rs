//! What the UI asks of a store's themes (docs/THEMES_CONTRACT.md sections
//! 3.1 and 6): [`ThemeCommand`], carried as `BackendCommand::Themes`.

use pimble_core::{NodeId, StoreId};

/// A store's theme nodes: listed, made, edited and copied in.
#[derive(Debug, Clone, PartialEq)]
pub enum ThemeCommand {
    /// Every theme node of the store. Answers `ThemesListed`.
    List { store_id: StoreId },
    /// Keys of a theme's `data.theme` set (to checked JSON) or removed, in
    /// one edit. Answers nothing of its own: the change arrives as every
    /// edit's does, and the store's themes are listed again.
    Edit { store_id: StoreId, node_id: NodeId, edits: Vec<(String, Option<serde_json::Value>)> },
    /// A new theme node named `name` (numbered when the store has a theme of
    /// that name) under the store's root, based on
    /// `base` (a built-in's id or a theme node's of this store) and setting
    /// nothing; applied to `apply_to` when given. Answers `ThemeMade`, which
    /// opens it in the focused pane.
    New { store_id: StoreId, name: String, base: Option<String>, apply_to: Option<NodeId> },
    /// Theme `from_node` of store `from_store` copied into `to_store`
    /// (section 6: its bases in its own store folded in, a built-in base
    /// kept, `copied_from` recorded), or the copy made before reused; then
    /// applied to `apply_to` when given. With `customize`, a new theme based
    /// on the copy is made and applied instead, as `New` makes one. Answers
    /// `ThemeMade` for the copy only when `customize` made one to open.
    Copy { from_store: StoreId, from_node: NodeId, to_store: StoreId, apply_to: Option<NodeId>, customize: bool },
}
