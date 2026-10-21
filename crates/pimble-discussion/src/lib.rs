//! Discussions (docs/DISCUSSIONS_CONTRACT.md): threaded conversations that
//! live in a discussion node's own document. One crate for the app (desktop
//! and web), the server's search index and the MCP server, native and
//! wasm32, no UI:
//!
//! - [`doc`]: the discussion document over yrs (contract section 2). Every
//!   write is one transaction and answers an ordinary yrs update of the
//!   node's document, which goes through `applyEdit` like any other edit.
//! - [`body`]: a message's formatted text (bold, italic, links), and the
//!   Markdown a person types it as.
//! - [`personal`]: the personal document (read state, places), one per
//!   person, never shared.
//! - [`thread`]: the messages as a read thread, in the order every device
//!   shows them, with the tie-breaks for what a merge can leave.

pub mod body;
pub mod doc;
pub mod personal;
pub mod thread;

pub use body::{Body, Span};
pub use doc::{new_content, Anchor, Author, Discussion, DiscussionError, DiscussionTxn, Edited, Stamp, ROOT_KEY};
pub use personal::{Personal, Place};
pub use thread::{DiscussionSnapshot, MessageSnap, Resolved, ThreadItem};
