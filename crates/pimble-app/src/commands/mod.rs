//! Every `BackendCommand`, answered with `BackendEvent`s: the app's thin
//! adapter onto the one API (docs/API_CONTRACT.md wave 5).
//!
//! A command that reads or writes a store is a call on [`Pimble`]
//! ([`api_command`]): its documents are held there, kept current by the
//! store's subscription, and every write goes through it. What only a
//! Pimble server's operator does (open, close and create stores, replicas
//! and sync links, the cloud account, sharing, agents) is the server's own
//! RPCs on the connection to this client's own server ([`host_command`]),
//! the `Host` side of decision 2. The desktop's `backend` module and the web
//! app's loop own the API and that connection, and both call
//! [`process_command`]; a store's changes reach the UI through
//! [`change_events`], from whoever listens to the API.
//!
//! The modules split it by concern: [`api_command`] answers reads and edits
//! of documents itself and hands what changes a tree's shape to `structure`;
//! [`host_command`] answers stores, replicas, sync links and agents itself
//! and hands the Pimble Cloud account, notifications and sharing to `cloud`;
//! `changes` is what the API hears, as events.

mod api;
mod changes;
mod cloud;
mod host;
mod structure;
mod themes;
#[cfg(all(test, feature = "native"))]
mod theme_tests;

pub use api::{api_command, presence_frame};
pub use changes::{change_events, shown};
pub use host::host_command;
pub use themes::{as_themes, copy_of, customized_name};

use std::future::Future;
use std::sync::Arc;

use crossbeam_channel::Sender;
use pimble_api::{ApiError, Pimble};
use pimble_client::PimbleClient;

use crate::protocol::{BackendCommand, BackendEvent};

/// Run `fut` alongside the command loop, for a subscription that then feeds
/// events in for as long as it lives.
///
/// Native has a tokio runtime to put it on; the browser has the page's own task
/// queue, where nothing is `Send` and nothing needs to be.
#[cfg(feature = "native")]
fn spawn_task(fut: impl Future<Output = ()> + Send + 'static) {
    tokio::spawn(fut);
}

#[cfg(not(feature = "native"))]
fn spawn_task(fut: impl Future<Output = ()> + 'static) {
    wasm_bindgen_futures::spawn_local(fut);
}

/// What a command runs against.
pub struct Context<'a> {
    /// The API: every command that reads or writes a store.
    pub api: &'a Arc<Pimble>,
    /// The connection to this client's own server, for what only its
    /// operator does; `None` while there is none.
    pub host: Option<Arc<PimbleClient>>,
    pub event_tx: &'a Sender<BackendEvent>,
    pub signal_ui: &'a Arc<dyn Fn() + Send + Sync>,
    /// This window's id, what its views' writes are sent as.
    pub client_id: &'a str,
}

impl Context<'_> {
    fn emit(&self, event: BackendEvent) {
        let _ = self.event_tx.try_send(event);
        (self.signal_ui)();
    }
}

/// An API error as the app shows it: the sentence, and a store whose owner
/// is away says so as a notice, the way the app recognises a sentence meant
/// to be read as it is (`events::show_notice`, reached by `Forbidden: `; the
/// server's own refusals already read that way).
pub fn error_message(e: &ApiError) -> String {
    match e {
        ApiError::OwnerOffline { sentence } => format!("Forbidden: {sentence}"),
        other => other.sentence(),
    }
}

fn failed(e: ApiError) -> Option<BackendEvent> {
    Some(BackendEvent::Error { message: error_message(&e) })
}

/// Answer `cmd`: through the API when it reads or writes a store, otherwise
/// on the host connection.
/// A failure of a request a modal waits on is answered as `Failed`, naming
/// it (`crate::protocol::Asked`).
pub async fn process_command(ctx: &Context<'_>, cmd: BackendCommand) -> Option<BackendEvent> {
    let asked = crate::protocol::Asked::of(&cmd);
    // What was asked, for the log when it fails: an error the window shows
    // ("Node not found: ...") says nothing of which request met it. Cut
    // short, since a command may carry a document's bytes.
    let what = described(&cmd, 300);
    let answer = match api_command(ctx, cmd).await {
        Ok(event) => event,
        Err(cmd) => host_command(ctx, cmd).await,
    };
    if let Some(BackendEvent::Error { message }) = &answer {
        tracing::warn!("{what} failed: {message}");
    }
    crate::protocol::Asked::answer(asked, answer)
}

/// `value`'s `Debug` text, at most `limit` bytes of it: formatting stops
/// there, so a command carrying a document's bytes costs no more than a short one.
fn described(value: &impl std::fmt::Debug, limit: usize) -> String {
    struct Bounded {
        text: String,
        limit: usize,
    }
    impl std::fmt::Write for Bounded {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            for c in s.chars() {
                if self.text.len() + c.len_utf8() > self.limit {
                    self.text.push_str("...");
                    return Err(std::fmt::Error);
                }
                self.text.push(c);
            }
            Ok(())
        }
    }
    let mut out = Bounded { text: String::new(), limit };
    let _ = std::fmt::write(&mut out, format_args!("{value:?}"));
    out.text
}

#[cfg(test)]
mod tests {
    use super::described;

    #[test]
    fn a_command_is_described_short_whatever_it_carries() {
        let short = described(&("GetNode", 7), 300);
        assert_eq!(short, "(\"GetNode\", 7)");
        let long = described(&vec![0u8; 100_000], 40);
        assert!(long.len() <= 43 && long.ends_with("..."), "{long}");
        let wide = described(&"ééééé", 4);
        assert!(wide.starts_with("\"é") && wide.ends_with("..."), "{wide}");
    }
}
