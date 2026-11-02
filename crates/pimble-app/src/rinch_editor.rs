//! The rich-text editor, from whichever rinch backend this build uses.
//!
//! `EditorHandle`, `create_editor` and the `Editor` component are the *same*
//! types on both targets — `rinch_editor_view`'s, with the same
//! `rinch-editor-collab` yrs adapter underneath. Only the door differs: the
//! desktop reaches them through `rinch::prelude` (the `desktop` +
//! `collaboration` features) and the browser through `rinch_web` (its
//! `collaboration` feature), because rinch's own `collaboration` implies
//! `desktop` and so cannot be enabled in a web build.
//!
//! Everything else in this crate imports them from here, which is what keeps
//! `editor.rs` — the one place collaboration is wired — free of `cfg`.

#[cfg(feature = "native")]
pub use rinch::prelude::{
    create_editor, Editor, EditorHandle, EditorKey, LinkClick, LinkHover, MarginAnchor, MarginLine, NodeViewCtx, NodeViewRegistration, NodeViewSpec, ScrollAlign,
};
#[cfg(feature = "native")]
pub use rinch::editor::{set_image_source, ImageHover, ImageInputSource, SelectionAnchor};

#[cfg(not(feature = "native"))]
pub use rinch_web::{
    create_editor, set_image_source, Editor, EditorHandle, EditorKey, ImageHover, ImageInputSource, LinkClick, LinkHover, MarginAnchor, MarginLine, NodeViewCtx,
    NodeViewRegistration, NodeViewSpec, ScrollAlign, SelectionAnchor,
};

/// An embed atom's attributes for the editor (`pimble_crdt::markdown::Embed`
/// says what each one carries).
pub fn embed_attrs(embed: &pimble_crdt::markdown::Embed) -> rinch_editor_core::Attrs {
    embed.attrs().into_iter().fold(rinch_editor_core::Attrs::new(), |a, (k, v)| a.with(k, v.as_str()))
}

/// What an embed atom in the editor shows, if it is one of Pimble's.
pub fn embed_of(attrs: &rinch_editor_core::Attrs) -> Option<pimble_crdt::markdown::Embed> {
    pimble_crdt::markdown::Embed::from_attrs(|name| attrs.get_str(name))
}
