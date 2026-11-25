//! The seam between the UI and whatever is behind it.
//!
//! The UI sends a [`BackendCommand`] and consumes a [`BackendEvent`], and knows
//! nothing else about how either travels. Behind it, every command that reads
//! or writes a store is a call on `pimble_api::Pimble` (`crate::commands`,
//! docs/API_CONTRACT.md wave 5), and every change to a store the API hears
//! comes back as events. Two implementations own the API and the channels:
//! the desktop's `backend` module (a background thread with a tokio runtime
//! and an embedded `PimbleServer`, behind the `native` feature) and the web
//! app's command loop on `wasm_bindgen_futures::spawn_local` against hosted
//! servers and the browser's vault.
//!
//! [`BackendHandle`] is the pair of channels itself, so both implementations
//! hand the UI the same thing. Only `BackendHandle::spawn` is native; `send`
//! and `try_recv` never block and work on every target.
//!
//! The commands live in `commands`, the events in `events`, and the names a
//! failure or a cloud outcome carries for the request it answers in `asked`.

mod asked;
mod commands;
mod events;
mod themes;

pub use asked::{Asked, CloudOp};
pub use commands::{BackendCommand, NodeInit, TagEdit};
pub use events::{BackendEvent, StoredBlob};
pub use themes::ThemeCommand;

use crossbeam_channel::{Receiver, Sender};

/// Who asked a `Search`, so its answer reaches them and nobody else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchPurpose {
    /// The search box and its results panel.
    Panel,
    /// The link picker (`[[` and Ctrl+L, docs/LINKS_CONTRACT.md).
    LinkPicker,
    /// The node picker of a query metanode's setup in this pane
    /// (docs/QUERY_METANODES_CONTRACT.md section 5).
    MetanodeSetup(crate::panes::PaneId),
}

/// Handle to communicate with the backend
#[derive(Clone)]
pub struct BackendHandle {
    pub cmd_tx: Sender<BackendCommand>,
    pub event_rx: Receiver<BackendEvent>,
}

impl BackendHandle {
    /// Send a command to the backend (non-blocking), ignoring result
    pub fn send(&self, cmd: BackendCommand) {
        if let Err(e) = self.cmd_tx.try_send(cmd) {
            tracing::error!("Backend channel send failed: {}", e);
        }
    }

    /// Try to receive an event (non-blocking)
    pub fn try_recv(&self) -> Option<BackendEvent> {
        self.event_rx.try_recv().ok()
    }
}
