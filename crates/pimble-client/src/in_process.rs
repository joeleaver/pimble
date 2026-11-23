//! A connection to a server in the same process (docs/ONE_CODEBASE_CONTRACT.md
//! seam A): JSON-RPC text over a pair of channels to
//! `pimble_rpc::dispatch::session::serve`, with no socket. The client is the
//! same `PimbleClient` a WebSocket gives, so everything built on it works
//! over either; wave 3 puts a browser `MessagePort` where the channels are.

use jsonrpsee::core::async_trait;
use jsonrpsee::core::client::{ClientBuilder, ReceivedMessage, TransportReceiverT, TransportSenderT};
use pimble_rpc::dispatch::session::ClientEnd;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use url::Url;

use crate::PimbleClient;

/// The in-process server has gone (its end of the channel pair dropped).
#[derive(Debug, thiserror::Error)]
#[error("the in-process Pimble server has gone")]
pub struct ServerGone;

struct Sender(UnboundedSender<String>);
struct Receiver(UnboundedReceiver<String>);

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl TransportSenderT for Sender {
    type Error = ServerGone;

    async fn send(&mut self, msg: String) -> Result<(), ServerGone> {
        self.0.send(msg).map_err(|_| ServerGone)
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl TransportReceiverT for Receiver {
    type Error = ServerGone;

    async fn receive(&mut self) -> Result<ReceivedMessage, ServerGone> {
        self.0.recv().await.map(ReceivedMessage::Text).ok_or(ServerGone)
    }
}

/// What [`PimbleClient::url`] answers for an in-process connection.
pub const IN_PROCESS_URL: &str = "pimble-in-process://local/";

impl PimbleClient {
    /// A client of the server on the other end of `end` (made with
    /// `pimble_rpc::dispatch::session::channel`, the other end served by
    /// `session::serve`). Natively it needs a tokio runtime, as every
    /// `PimbleClient` does; in a browser it runs on `spawn_local`.
    pub fn in_process(end: ClientEnd) -> Self {
        let ClientEnd { to_server, from_server } = end;
        let builder = ClientBuilder::default();
        #[cfg(not(target_arch = "wasm32"))]
        let client = builder.build_with_tokio(Sender(to_server), Receiver(from_server));
        #[cfg(target_arch = "wasm32")]
        let client = builder.build_with_wasm(Sender(to_server), Receiver(from_server));
        Self::from_rpc_client(client, Url::parse(IN_PROCESS_URL).expect("a valid URL"))
    }
}
