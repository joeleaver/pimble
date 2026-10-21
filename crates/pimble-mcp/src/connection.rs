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

    /// A connection that is already made, to a server a test started.
    #[cfg(test)]
    pub fn with_client(client: PimbleClient) -> Self {
        Self { inner: Mutex::new(Inner { client: Some(Arc::new(client)), owned: None }) }
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
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err("Pimble's server did not come back within 10 seconds; try again.".into()),
        };
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

/// What every tool answers when this process was started without an agent's
/// key (docs/AGENTS_CONTRACT.md: setups made by hand stop until scoped).
pub const NOT_AN_AGENT: &str =
    "Pimble needs this app set up as an agent: in Pimble, Account > AI Agents... > New Agent.";

/// The agent key this process was started with (`PIMBLE_AGENT_KEY`, which
/// the agent's client config passes in its environment, never its command line).
fn agent_key() -> Option<String> {
    std::env::var("PIMBLE_AGENT_KEY").ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
}

/// The agent this process acts for, from its key (`pmbl_agent_<id>_…`): what
/// its edits are attributed to.
pub fn agent_id() -> Option<String> {
    let key = agent_key()?;
    let (id, _) = key.strip_prefix("pmbl_agent_")?.split_once('_')?;
    Some(id.to_string())
}

/// A connection that acts as this process's agent. The server judges every
/// call by the agent's grants; the person's own (`Service`) connection is
/// used only to start or join the server and to open the saved store list
/// when this process starts it, as the app does.
async fn connect(owned: &mut Option<PimbleServer>) -> Result<PimbleClient, String> {
    if let Ok(url) = std::env::var("PIMBLE_SERVER") {
        // Some other server, named: never started here. A server token
        // (`PIMBLE_TOKEN`) is a deliberate credential for that server, kept
        // for development; an agent key is presented the same way.
        let token = agent_key().or_else(|| std::env::var("PIMBLE_TOKEN").ok().filter(|t| !t.is_empty()));
        let Some(token) = token else { return Err(NOT_AN_AGENT.into()) };
        return PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token }).await.map_err(|e| e.to_string());
    }
    let Some(key) = agent_key() else { return Err(NOT_AN_AGENT.into()) };
    let service = local::reconnect(owned).await.map_err(|e| format!("Pimble's server could not be reached or started: {e}"))?;
    if owned.is_some() {
        open_saved_stores(&service).await;
    }
    PimbleClient::connect_with_auth(local::server_url(), &AuthMethod::Bearer { token: key }).await.map_err(|e| {
        let e = e.to_string();
        if e.contains("401") {
            "This agent was removed in Pimble, or its key is wrong: set it up again in Account > AI Agents...".to_string()
        } else {
            format!("Pimble's server refused this agent: {e}")
        }
    })
}

async fn open_saved_stores(client: &PimbleClient) {
    for path in local::load_open_stores() {
        match client.open_store(&path).await {
            Ok(store) => tracing::info!("opened {} ({})", store.name, path),
            Err(e) => tracing::warn!("could not open the saved store {path}: {e}"),
        }
    }
}
