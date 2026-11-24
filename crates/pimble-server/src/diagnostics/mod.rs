//! Alpha diagnostics (docs/DIAGNOSTICS_CONTRACT.md): what this computer's
//! Pimble sends so that a tester's problem can be read from both ends.
//!
//! Two hosts record, with the same envelope, the same choice of log lines and
//! the same scrubbing (`pimble_core::diagnostics`):
//!
//! - **The desktop's programs** (`native`): on unless the person turned it off
//!   (`<config dir>/pimble/diagnostics.json`, `{"enabled": false}`), and only
//!   in a build that carries the send key (`PIMBLE_DIAGNOSTICS_KEY` when it
//!   was built, or at run time). Queued as NDJSON files under
//!   `<data dir>/pimble/diagnostics/`, sent every minute (gzip,
//!   `POST /v1/records`), each file deleted once the server has it; at most
//!   [`MAX_QUEUED_BYTES`] wait, the oldest dropped first. Turned off, the
//!   queue is deleted and nothing is recorded.
//! - **The server in a browser's worker** (`worker`, over [`on_disk`]):
//!   queued on the server's own disk (OPFS, sealed, or memory), sent through
//!   the host's `Http` to the accounts service's
//!   `/api/v1/diagnostics/records`, which adds the send key (a browser never
//!   carries it); the page says whether it is on (`setDiagnostics`)
//!   (docs/ONE_CODEBASE_CONTRACT.md section 6, item 10).
//!
//! Every record goes through [`pimble_core::diagnostics::Scrubber`] before it
//! is queued: the home folder is `~`, email addresses, secrets and known
//! titles are taken out.

use std::fmt::Write as _;

use pimble_core::diagnostics::{is_info_target, Severity, INFO_TARGETS};
use tracing::field::{Field, Visit};
use tracing::Level;

pub mod on_disk;

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
mod queue;
#[cfg(not(target_arch = "wasm32"))]
mod report;
#[cfg(not(target_arch = "wasm32"))]
mod system;
#[cfg(not(target_arch = "wasm32"))]
pub use native::*;
#[cfg(not(target_arch = "wasm32"))]
pub use report::send_report;

#[cfg(target_arch = "wasm32")]
mod worker;
#[cfg(target_arch = "wasm32")]
pub use worker::*;

/// Whether a log line is recorded at all, and against which budget: warnings
/// and errors from anywhere, `info` from the sync and sharing targets, never
/// this module's own lines.
pub fn severity(level: &Level, target: &str) -> Option<Severity> {
    if target.starts_with("pimble_server::diagnostics") {
        return None;
    }
    if *level <= Level::WARN {
        return Some(Severity::Warning);
    }
    (*level == Level::INFO && is_info_target(target, INFO_TARGETS)).then_some(Severity::Info)
}

/// A `tracing` event's message and fields as one line of text.
#[derive(Default)]
pub struct Message(pub String);

impl Visit for Message {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        } else {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        } else {
            let _ = write!(self.0, " {}={value}", field.name());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warnings_from_anywhere_and_info_from_sync_and_sharing() {
        assert_eq!(severity(&Level::WARN, "wgpu_hal::vulkan"), Some(Severity::Warning));
        assert_eq!(severity(&Level::ERROR, "pimble_app::editor"), Some(Severity::Warning));
        assert_eq!(severity(&Level::INFO, "pimble_server::vault_link"), Some(Severity::Info));
        assert_eq!(severity(&Level::INFO, "pimble_server::share"), Some(Severity::Info));
        assert_eq!(severity(&Level::INFO, "pimble_plugin_host::registry"), None);
        assert_eq!(severity(&Level::DEBUG, "pimble_server::vault_link"), None);
        assert_eq!(severity(&Level::WARN, "pimble_server::diagnostics"), None, "never its own lines");
    }
}
