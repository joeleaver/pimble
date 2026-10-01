//! The connection to the Pimble server (docs/MCP_CONTRACT.md "Which server it
//! talks to"): the one on this computer by the rule the app follows
//! (`pimble_server::local`), joined when it runs and started when it does not,
//! or the server `PIMBLE_SERVER` names.

use std::sync::Arc;
use std::time::Duration;

use pimble_client::PimbleClient;
use pimble_core::AuthMethod;
use pimble_server::{local, PimbleServer};
use tokio::sync::Mutex;

/// How long a tool call waits for a lost server to come back (the app that
/// owned it quit, and this process starts or joins the next one).
const RECONNECT_WAIT: Duration = Duration::from_secs(10);

pub struct Connection {
    inner: Mutex<Inner>,
}

struct Inner {
    client: Option<Arc<PimbleClient>>,
    /// The server this process started, when it did.
    owned: Option<PimbleServer>,
}

impl Connection {
    pub fn new() -> Self {
        Self { inner: Mutex::new(Inner { client: None, owned: None }) }
    }

    /// A live client: the one held, or a new connection when there is none or it
    /// closed. A server this process starts opens the saved store list, so the
    /// LLM sees what the person sees in the explorer.
    pub async fn client(&self) -> Result<Arc<PimbleClient>, String> {
        let mut inner = self.inner.lock().await;
        if let Some(client) = &inner.client {
            if client.is_connected() {
                return Ok(client.clone());
            }
            tracing::warn!("the connection to the Pimble server closed; reconnecting");
        }
        let connected = tokio::time::timeout(RECONNECT_WAIT, connect(&mut inner.owned)).await;
        let client = match connected {
            Ok(Ok(client)) => Arc::new(client),
            Ok(Err(e)) => return Err(format!("Pimble's server could not be reached or started: {e}")),
            Err(_) => return Err("Pimble's server did not come back within 10 seconds; try again.".into()),
        };
        if inner.owned.is_some() {
            open_saved_stores(&client).await;
        }
        inner.client = Some(client.clone());
        Ok(client)
    }

    /// Stop the server this process owns, writing every document it holds.
    pub async fn shut_down(&self) {
        let mut inner = self.inner.lock().await;
        inner.client = None;
        local::shut_down(&mut inner.owned).await;
    }
}

async fn connect(owned: &mut Option<PimbleServer>) -> Result<PimbleClient, String> {
    if let Ok(url) = std::env::var("PIMBLE_SERVER") {
        // Some other server, named: never started here.
        let token = std::env::var("PIMBLE_TOKEN").ok().filter(|t| !t.is_empty());
        let client = match token {
            Some(token) => PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token }).await,
            None => PimbleClient::connect(&url).await,
        };
        return client.map_err(|e| e.to_string());
    }
    local::reconnect(owned).await
}

async fn open_saved_stores(client: &PimbleClient) {
    for path in local::load_open_stores() {
        match client.open_store(&path).await {
            Ok(store) => tracing::info!("opened {} ({})", store.name, path),
            Err(e) => tracing::warn!("could not open the saved store {path}: {e}"),
        }
    }
}
