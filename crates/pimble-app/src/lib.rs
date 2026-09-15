//! Pimble's rinch application, as a library.
//!
//! The desktop binary (`src/main.rs`, feature `native`) and the browser app
//! (`web/`, the separate trunk workspace) are two entry points onto the *same*
//! UI. Everything they share lives here:
//!
//! - [`app::build_view`] makes the application state and the root component.
//!   [`app::run`] is the desktop entry point: it wraps that component in a
//!   window, a menu bar and the theme. The web entry point hands the same
//!   component to `rinch_web::mount`.
//! - [`protocol`] is the seam between the UI and whatever is behind it: the UI
//!   sends [`BackendCommand`]s and consumes [`BackendEvent`]s, and knows
//!   nothing else. The desktop implementation is [`backend`] (a tokio thread
//!   with an embedded `PimbleServer`); the web app runs the same loop on
//!   `wasm_bindgen_futures::spawn_local` against a hosted server.
//!
//! The collaboration invariants in `CLAUDE.md` hold on both: one editor pane,
//! one thread-local `EditorHandle`, edits travelling as yrs bytes through
//! `BroadcastChanges` and `RemoteChanges`, and never a document model in the
//! sync path.

pub mod app;
pub mod appearance;
pub mod commands;
pub mod editor;
pub mod events;
pub mod persistence;
pub mod protocol;
pub mod rinch_editor;
pub mod state;
pub mod styles;
pub mod toolbar;

/// The desktop backend: a background thread with a tokio runtime, an embedded
/// [`pimble_server::PimbleServer`] and the WebSocket client that talks to it.
/// The browser build has no equivalent here: `web/` drives
/// [`commands::process_command`] from its own `spawn_local` loop against a
/// hosted server.
#[cfg(feature = "native")]
pub mod backend;

#[cfg(not(any(feature = "native", feature = "web")))]
compile_error!(
    "pimble-app needs one of its two backends: `native` for the desktop shell \
     (the embedded server, the window, menus) or `web` for the browser build."
);

pub use protocol::{BackendCommand, BackendEvent, BackendHandle};
