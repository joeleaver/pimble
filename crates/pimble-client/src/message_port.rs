//! A connection to the Pimble server in a browser's worker over a
//! `MessagePort` (docs/ONE_CODEBASE_CONTRACT.md seam A, section 4): the same
//! JSON-RPC text as every other transport, one string per message, and the
//! same `PimbleClient`. The worker's side reads its end of the channel with
//! [`PortText`] too and hands it to `pimble_rpc::dispatch::session::serve_with`
//! (crates/pimble-worker).
//!
//! A `MessagePort` says nothing when the other side goes: a worker that has
//! stopped leaves this client's calls unanswered. Noticing that (the leader
//! tab's lock passing) is the page's, in wave 4.

use jsonrpsee::core::async_trait;
use jsonrpsee::core::client::{ClientBuilder, ReceivedMessage, TransportReceiverT, TransportSenderT};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use url::Url;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{MessageEvent, MessagePort};

use crate::PimbleClient;

/// What [`PimbleClient::url`] answers for a connection over a port.
pub const MESSAGE_PORT_URL: &str = "pimble-message-port://worker/";

/// The port failed, or its other side stopped listening.
#[derive(Debug, thiserror::Error)]
#[error("the Pimble server's message port: {0}")]
pub struct PortError(String);

/// The text messages arriving on a `MessagePort`, in order. Listening starts
/// when this is made (`onmessage`, which also starts the port) and stops when
/// it is dropped. A message that is not a string is not JSON-RPC text: it is
/// skipped and logged.
pub struct PortText {
    port: MessagePort,
    rx: UnboundedReceiver<String>,
    // Kept alive for as long as the port is read.
    _listener: Closure<dyn FnMut(MessageEvent)>,
}

impl PortText {
    pub fn listen(port: &MessagePort) -> Self {
        let (tx, rx) = unbounded_channel();
        let listener = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| match event.data().as_string() {
            Some(text) => {
                let _ = tx.send(text);
            }
            None => tracing::warn!("a message that is not JSON-RPC text arrived on the Pimble server's port; skipped"),
        });
        port.set_onmessage(Some(listener.as_ref().unchecked_ref()));
        Self { port: port.clone(), rx, _listener: listener }
    }

    /// The next message; `None` once nothing can arrive any more.
    pub async fn next(&mut self) -> Option<String> {
        self.rx.recv().await
    }

    /// As a stream, for `session::serve_with`.
    pub fn into_stream(self) -> impl futures::Stream<Item = String> + Unpin {
        Box::pin(futures::stream::unfold(self, |mut port| async move { port.next().await.map(|text| (text, port)) }))
    }
}

impl Drop for PortText {
    fn drop(&mut self) {
        self.port.set_onmessage(None);
    }
}

/// Send one message of JSON-RPC text on `port`.
pub fn post_text(port: &MessagePort, text: &str) -> Result<(), PortError> {
    port.post_message(&JsValue::from_str(text)).map_err(|e| PortError(e.as_string().unwrap_or_else(|| format!("{e:?}"))))
}

struct Sender(MessagePort);
struct Receiver(PortText);

#[async_trait(?Send)]
impl TransportSenderT for Sender {
    type Error = PortError;

    async fn send(&mut self, msg: String) -> Result<(), PortError> {
        post_text(&self.0, &msg)
    }

    async fn close(&mut self) -> Result<(), PortError> {
        self.0.close();
        Ok(())
    }
}

#[async_trait(?Send)]
impl TransportReceiverT for Receiver {
    type Error = PortError;

    async fn receive(&mut self) -> Result<ReceivedMessage, PortError> {
        self.0.next().await.map(ReceivedMessage::Text).ok_or_else(|| PortError("closed".to_string()))
    }
}

impl PimbleClient {
    /// A client of the server listening on the other end of `port` (one end
    /// of a `MessageChannel`, the other handed to the worker). Runs on
    /// `spawn_local`.
    pub fn over_message_port(port: MessagePort) -> Self {
        let receiver = Receiver(PortText::listen(&port));
        let client = ClientBuilder::default().build_with_wasm(Sender(port), receiver);
        Self::from_rpc_client(client, Url::parse(MESSAGE_PORT_URL).expect("a valid URL"))
    }
}
