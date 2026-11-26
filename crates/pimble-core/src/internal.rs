//! Internal nodes, hidden by their kind (docs/THEMES_CONTRACT.md section
//! 3.1a): nodes of the tree that are there for something else (a theme a
//! tree reads in) rather than to be read themselves. A node is internal
//! because of its type, never because of a mark on it; each kind is shown
//! or hidden on its own, by a per-device view preference.
//!
//! Hidden means not shown, never not there: an internal node still
//! replicates, encrypts, is reached by shares and opens by link. What leaves
//! it out is each place that lists nodes for a person (the explorer,
//! search, metanode sources, the MCP's listings), through [`hidden`].

use crate::node::node_types;

/// A kind of internal node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum InternalKind {
    /// Document themes (`node_type = "theme"`).
    Theme,
}

impl InternalKind {
    /// Every kind, in the order View > "Show Internal" lists them.
    pub const ALL: [InternalKind; 1] = [InternalKind::Theme];

    /// The kind's name in menus ("Themes").
    pub fn name(self) -> &'static str {
        match self {
            InternalKind::Theme => "Themes",
        }
    }

    /// How a count of hidden nodes of this kind reads ("3 themes hidden").
    pub fn count_text(self, count: usize) -> String {
        match (self, count) {
            (InternalKind::Theme, 1) => "1 theme hidden".to_string(),
            (InternalKind::Theme, n) => format!("{n} themes hidden"),
        }
    }

    /// How a count of this kind's nodes in one place reads ("2 themes
    /// here."), as the explorer's group of them says it.
    pub fn here_text(self, count: usize) -> String {
        match (self, count) {
            (InternalKind::Theme, 1) => "1 theme here.".to_string(),
            (InternalKind::Theme, n) => format!("{n} themes here."),
        }
    }

    /// The kind as stored in a view preference and named by the MCP
    /// (`"themes"`).
    pub fn id(self) -> &'static str {
        match self {
            InternalKind::Theme => "themes",
        }
    }

    /// The kind named by [`InternalKind::id`].
    pub fn from_id(id: &str) -> Option<InternalKind> {
        InternalKind::ALL.into_iter().find(|kind| kind.id() == id)
    }

    /// Why nothing may be put inside a node of this kind: an internal node
    /// holds nothing, so what the explorer and search leave out with it is
    /// only ever the node itself.
    pub fn holds_nothing(self) -> &'static str {
        match self {
            InternalKind::Theme => "Nothing goes inside a theme.",
        }
    }

    /// Why a drop into this kind's group of the explorer (Internal >
    /// Themes) is refused when it carries something else: the group shows
    /// the parent's nodes of this kind and nothing more.
    pub fn only_here(self) -> &'static str {
        match self {
            InternalKind::Theme => "Only themes go in Themes.",
        }
    }

    /// How a node of this kind is made, as a refusal to make one in its
    /// group of the explorer says it.
    pub fn how_made(self) -> &'static str {
        match self {
            InternalKind::Theme => "Node > New Theme makes a theme.",
        }
    }

    /// The Tabler icon a shown node of this kind is marked with.
    pub fn icon(self) -> &'static str {
        match self {
            InternalKind::Theme => "palette",
        }
    }

    /// Its place in [`InternalKind::ALL`].
    pub fn index(self) -> usize {
        match self {
            InternalKind::Theme => 0,
        }
    }
}

/// The internal kind of a node of type `node_type`, if it is internal.
pub fn internal_kind(node_type: &str) -> Option<InternalKind> {
    match node_type {
        node_types::THEME => Some(InternalKind::Theme),
        _ => None,
    }
}

/// Whether a node of type `node_type` is left out of a listing that shows
/// the internal kinds `shown`.
pub fn hidden(node_type: &str, shown: &[InternalKind]) -> bool {
    internal_kind(node_type).is_some_and(|kind| !shown.contains(&kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn themes_are_internal_and_nothing_else_is() {
        assert_eq!(internal_kind(node_types::THEME), Some(InternalKind::Theme));
        for plain in [node_types::DOCUMENT, node_types::FOLDER, node_types::BOARD, node_types::SHEET, node_types::DISCUSSION, node_types::MOUNT, ""] {
            assert_eq!(internal_kind(plain), None, "{plain}");
            assert!(!hidden(plain, &[]));
        }
        assert!(hidden(node_types::THEME, &[]));
        assert!(!hidden(node_types::THEME, &[InternalKind::Theme]));
    }

    #[test]
    fn kinds_name_themselves() {
        for kind in InternalKind::ALL {
            assert_eq!(InternalKind::from_id(kind.id()), Some(kind));
            assert_eq!(InternalKind::ALL[kind.index()], kind);
        }
        assert_eq!(InternalKind::Theme.count_text(1), "1 theme hidden");
        assert_eq!(InternalKind::Theme.count_text(12), "12 themes hidden");
        assert_eq!(InternalKind::from_id("nope"), None);
        assert_eq!(InternalKind::Theme.here_text(1), "1 theme here.");
        assert_eq!(InternalKind::Theme.here_text(4), "4 themes here.");
        assert_eq!(InternalKind::Theme.only_here(), "Only themes go in Themes.");
    }
}
