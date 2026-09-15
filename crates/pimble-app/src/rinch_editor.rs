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
pub use rinch::prelude::{create_editor, Editor, EditorHandle};

#[cfg(not(feature = "native"))]
pub use rinch_web::{create_editor, Editor, EditorHandle};
