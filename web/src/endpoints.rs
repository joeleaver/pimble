//! Every Pimble server this page talks to, and which store is on which.
//!
//! There is no "the" server here (docs/CRYPTO_CONTRACT.md, "Endpoint-agnostic").
//! A store is opened through the endpoint the session named for it, and each
//! endpoint has its own URL, its own credential and its own `PimbleClient`.
//! Today `POST /api/v1/token` names one URL and every store resolves to it, so
//! there is exactly one connection; when a relayed share arrives with its own
//! URL, it becomes a second entry and nothing else changes.
//!
//! The connection state lives here too — the client, whether it has proved
//! itself, how long to wait before trying again — so the backend loop
//! supervises endpoints by name rather than holding one privileged connection.

use std::collections::HashMap;
use std::sync::Arc;

use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, StoreId};

use crate::api::Session;

/// Backoff bounds for a failed connection attempt.
pub const RECONNECT_MIN_MS: i32 = 1_000;
pub const RECONNECT_MAX_MS: i32 = 30_000;

/// One server, its credential, and the connection to it. Keyed by its URL in
/// [`Endpoints`], which is why it does not carry one.
pub struct Endpoint {
    pub token: String,
    pub client: Option<Arc<PimbleClient>>,
    /// When the current connection was made, so the backoff is only forgotten
    /// once it has lasted. `None` once it has settled.
    pub connected_at: Option<f64>,
    pub backoff_ms: i32,
    /// Whether this outage has already been reported. A run of failed attempts
    /// says so once, not once per attempt.
    pub reported_failure: bool,
}

impl Endpoint {
    fn new(token: String) -> Self {
        Self {
            token,
            client: None,
            connected_at: None,
            backoff_ms: RECONNECT_MIN_MS,
            reported_failure: false,
        }
    }

    pub fn is_connected(&self) -> bool {
        self.client.as_ref().is_some_and(|c| c.is_connected())
    }

    pub fn auth(&self) -> AuthMethod {
        AuthMethod::Bearer { token: self.token.clone() }
    }
}

pub struct Endpoints {
    by_url: HashMap<String, Endpoint>,
    /// Which endpoint serves a store, for the stores that say so.
    store_url: HashMap<StoreId, String>,
    /// The endpoint the session itself named: where the store list comes from,
    /// and where a store with no endpoint of its own is served.
    session_url: String,
}

impl Endpoints {
    pub fn from_session(session: &Session) -> Self {
        let mut this = Self {
            by_url: HashMap::new(),
            store_url: HashMap::new(),
            session_url: session.rpc_url.clone(),
        };
        this.adopt(session);
        this
    }

    /// Take a freshly minted session: refresh every credential and re-read
    /// which store is served where.
    ///
    /// An endpoint that is already connected keeps its connection; only the
    /// token it will use next changes, which is what makes a token refresh
    /// invisible to a live socket.
    pub fn adopt(&mut self, session: &Session) {
        self.session_url = session.rpc_url.clone();
        self.upsert(&session.rpc_url, &session.token);

        self.store_url.clear();
        for store in &session.stores {
            let token = store.token.clone().unwrap_or_else(|| session.token.clone());
            self.upsert(&store.rpc_url, &token);
            self.store_url.insert(store.store_id, store.rpc_url.clone());
        }
    }

    fn upsert(&mut self, url: &str, token: &str) {
        match self.by_url.get_mut(url) {
            Some(endpoint) => endpoint.token = token.to_string(),
            None => {
                self.by_url.insert(url.to_string(), Endpoint::new(token.to_string()));
            }
        }
    }

    /// Where the store list comes from.
    pub fn session_url(&self) -> String {
        self.session_url.clone()
    }

    /// Which endpoint serves `store_id`.
    pub fn url_for(&self, store_id: StoreId) -> String {
        self.store_url
            .get(&store_id)
            .cloned()
            .unwrap_or_else(|| self.session_url.clone())
    }

    pub fn get(&self, url: &str) -> Option<&Endpoint> {
        self.by_url.get(url)
    }

    pub fn get_mut(&mut self, url: &str) -> Option<&mut Endpoint> {
        self.by_url.get_mut(url)
    }

    /// The connected client for `store_id`'s endpoint, if there is one.
    pub fn client_for(&self, store_id: StoreId) -> Option<Arc<PimbleClient>> {
        self.client_at(&self.url_for(store_id))
    }

    pub fn client_at(&self, url: &str) -> Option<Arc<PimbleClient>> {
        self.by_url.get(url).and_then(|e| e.client.clone())
    }

    /// Connect `url` if it is not connected, and hand back its client.
    ///
    /// For an endpoint the supervisor does not drive: a relayed store's server,
    /// reached the first time something asks for it.
    pub async fn ensure_connected(&mut self, url: &str) -> Result<Arc<PimbleClient>, String> {
        if let Some(client) = self.client_at(url).filter(|c| c.is_connected()) {
            return Ok(client);
        }
        let auth = match self.by_url.get(url) {
            Some(endpoint) => endpoint.auth(),
            None => return Err(format!("nothing known about {url}")),
        };
        let client = PimbleClient::connect_with_auth(url, &auth)
            .await
            .map_err(|e| e.to_string())?;
        let client = Arc::new(client);
        if let Some(endpoint) = self.by_url.get_mut(url) {
            endpoint.client = Some(client.clone());
        }
        Ok(client)
    }

    /// Drop the connection to every endpoint, so the next pass makes them
    /// again with whatever credential is current.
    pub fn disconnect_all(&mut self) {
        for endpoint in self.by_url.values_mut() {
            endpoint.client = None;
            endpoint.connected_at = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::StoreEndpoint;

    fn session(rpc_url: &str, stores: Vec<StoreEndpoint>) -> Session {
        Session {
            token: "session-token".into(),
            exp: 0,
            rpc_url: rpc_url.into(),
            stores,
        }
    }

    #[test]
    fn a_store_with_no_endpoint_of_its_own_uses_the_session_s() {
        let endpoints = Endpoints::from_session(&session("wss://a/rpc", Vec::new()));
        let anywhere = StoreId::new();
        assert_eq!(endpoints.url_for(anywhere), "wss://a/rpc");
        assert!(endpoints.get("wss://a/rpc").is_some());
    }

    #[test]
    fn a_store_served_elsewhere_gets_its_own_endpoint_and_token() {
        let relayed = StoreId::new();
        let endpoints = Endpoints::from_session(&session(
            "wss://a/rpc",
            vec![StoreEndpoint {
                store_id: relayed,
                rpc_url: "wss://relay/rpc".into(),
                token: Some("relay-token".into()),
            }],
        ));
        assert_eq!(endpoints.url_for(relayed), "wss://relay/rpc");
        assert_eq!(endpoints.get("wss://relay/rpc").unwrap().token, "relay-token");
        assert_eq!(endpoints.get("wss://a/rpc").unwrap().token, "session-token");
    }

    #[test]
    fn a_refreshed_session_replaces_tokens_without_dropping_connections() {
        let mut endpoints = Endpoints::from_session(&session("wss://a/rpc", Vec::new()));
        endpoints.get_mut("wss://a/rpc").unwrap().connected_at = Some(1.0);

        let mut fresh = session("wss://a/rpc", Vec::new());
        fresh.token = "newer".into();
        endpoints.adopt(&fresh);

        assert_eq!(endpoints.get("wss://a/rpc").unwrap().token, "newer");
        assert_eq!(endpoints.get("wss://a/rpc").unwrap().connected_at, Some(1.0));
    }
}
