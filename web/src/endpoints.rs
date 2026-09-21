//! Every Pimble server this page talks to, and which store is on which.
//!
//! There is no "the" server here (docs/CRYPTO_CONTRACT.md, "Endpoint-agnostic").
//! A store is opened through the endpoint the session named for it, and each
//! endpoint has its own URL, its own credential and its own `PimbleClient`.
//! `POST /api/v1/token` names the session's own URL, where every hosted store
//! is, and one more for each store served from its owner's computer through
//! Pimble Cloud's relay (docs/RELAY_CONTRACT.md): `stores[]`, each with a
//! token that names that store alone, which is the only credential the relay
//! takes.
//!
//! The connection state lives here too — the client, whether it has proved
//! itself, how long to wait before trying again — so the backend loop
//! supervises endpoints by name rather than holding one privileged connection.
//! It supervises all of them: the session's, which everything waits for, and
//! each relayed store's ([`Endpoints::relay_urls`]), which is down whenever
//! its owner's computer is and must never hold anything else up.

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
    /// When the next attempt may be made (the page's clock, ms), for an
    /// endpoint whose outages the loop does not sit out: a relayed store's,
    /// which is down for as long as its owner's computer is. `0.0` is "now".
    pub next_attempt_at: f64,
}

impl Endpoint {
    fn new(token: String) -> Self {
        Self {
            token,
            client: None,
            connected_at: None,
            backoff_ms: RECONNECT_MIN_MS,
            reported_failure: false,
            next_attempt_at: 0.0,
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

        // An endpoint nothing names any more (a share this account no longer
        // holds) is let go, and its connection with it.
        let session_url = self.session_url.clone();
        let store_url = &self.store_url;
        self.by_url.retain(|url, _| *url == session_url || store_url.values().any(|named| named == url));
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

    /// Every endpoint but the session's own: one per store served from its
    /// owner's computer (docs/RELAY_CONTRACT.md), in a stable order.
    pub fn relay_urls(&self) -> Vec<String> {
        let mut urls: Vec<String> = self.store_url.values().filter(|url| **url != self.session_url).cloned().collect();
        urls.sort();
        urls.dedup();
        urls
    }

    /// The stores the session said are served at `url`, in a stable order.
    /// Never the session's own endpoint's: its stores are whatever it lists.
    pub fn stores_at(&self, url: &str) -> Vec<StoreId> {
        let mut stores: Vec<StoreId> = self.store_url.iter().filter(|(_, named)| *named == url).map(|(id, _)| *id).collect();
        stores.sort_by_key(|id| id.to_string());
        stores
    }

    /// Whether `store_id` is served somewhere other than the session's own
    /// endpoint: from its owner's computer, through the relay.
    pub fn is_relayed(&self, store_id: StoreId) -> bool {
        self.store_url.get(&store_id).is_some_and(|url| *url != self.session_url)
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
    /// For the session's endpoint between two passes of the supervisor. A
    /// relayed store's endpoint is never connected on demand: it is down for
    /// as long as its owner's computer is, and the supervisor's backoff is
    /// what keeps that from being one attempt per keystroke.
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
            endpoint.next_attempt_at = 0.0;
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

    /// The relayed stores' endpoints are the ones the loop keeps up beside
    /// the session's: one per URL, each with the stores it serves, and gone
    /// when a refreshed session stops naming them.
    #[test]
    fn the_relayed_stores_endpoints_are_listed_for_the_supervisor() {
        let (first, second, hosted) = (StoreId::new(), StoreId::new(), StoreId::new());
        let relayed = |store_id: StoreId| StoreEndpoint {
            store_id,
            rpc_url: format!("wss://pimble.example/api/v1/relay/{store_id}"),
            token: Some(format!("token-for-{store_id}")),
        };
        let mut endpoints = Endpoints::from_session(&session("wss://a/rpc", vec![relayed(first), relayed(second)]));

        let mut expected = vec![relayed(first).rpc_url, relayed(second).rpc_url];
        expected.sort();
        assert_eq!(endpoints.relay_urls(), expected);
        assert_eq!(endpoints.stores_at(&relayed(first).rpc_url), vec![first]);
        assert!(endpoints.stores_at("wss://a/rpc").is_empty());
        assert!(endpoints.is_relayed(first));
        assert!(!endpoints.is_relayed(hosted));
        // Each presents the token minted for that store alone.
        assert_eq!(endpoints.get(&relayed(second).rpc_url).unwrap().token, format!("token-for-{second}"));

        // A share this account no longer holds: its endpoint goes.
        endpoints.adopt(&session("wss://a/rpc", vec![relayed(first)]));
        assert_eq!(endpoints.relay_urls(), vec![relayed(first).rpc_url]);
        assert!(endpoints.get(&relayed(second).rpc_url).is_none());
        assert!(!endpoints.is_relayed(second));
        assert!(endpoints.get("wss://a/rpc").is_some());
    }

    /// A store named at the session's own URL is not a second endpoint.
    #[test]
    fn a_store_named_at_the_session_s_own_url_is_not_relayed() {
        let store_id = StoreId::new();
        let endpoints = Endpoints::from_session(&session(
            "wss://a/rpc",
            vec![StoreEndpoint { store_id, rpc_url: "wss://a/rpc".into(), token: None }],
        ));
        assert!(endpoints.relay_urls().is_empty());
        assert!(!endpoints.is_relayed(store_id));
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
