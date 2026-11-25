//! HTTP calls to the Pimble Cloud accounts service
//! (docs/CRYPTO_CONTRACT.md "Accounts service endpoints (C)"), used by the
//! `cloud*` RPC handlers (`crate::handler`) and by
//! [`crate::vault_link::VaultLink`] to mint a fresh JWT before every
//! connect. Plain JSON calls through the host's [`pimble_host::Http`]
//! (docs/ONE_CODEBASE_CONTRACT.md seam D: `reqwest` on the desktop), mirroring the request/response
//! shapes `pimble-cloud`'s `src/routes/{accounts,stores}.rs` actually
//! serve — this crate has no dependency on `pimble-cloud` (a service, not a
//! library), so the shapes are duplicated here deliberately, the same way
//! `crate::jwt` duplicates a JWT verifier rather than depending on
//! `pimble-cloud`'s issuer.
//!
//! Every call but [`kdf`] and [`login`] carries `Authorization: Bearer
//! <session>` — the long-lived session token `login` returns, not a minted
//! JWT (`pimble-cloud`'s `AuthedUser` extractor accepts either the session
//! cookie or that header; see its `src/session.rs`).
//!
//! One module per part of the service: [`sessions`] (signing in, the
//! session's token and the personal document), [`stores`] (hosted and
//! relayed stores, and how the account holds one), [`keys`] (a store's key
//! envelopes), [`people`] (names and shares), [`members`], [`account`] (the
//! password, the recovery code, lookups) and [`notices`].

use pimble_host::http::{Body, Http, HttpRequest, HttpResponse, Method};
use serde::{Deserialize, Serialize};

mod account;
mod keys;
mod members;
mod notices;
mod people;
mod sessions;
mod stores;

pub use account::*;
pub use keys::*;
pub use members::*;
pub use notices::*;
pub use people::*;
pub use sessions::*;
pub use stores::*;

#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    #[error("could not reach the cloud service at {url}: {source}")]
    Request { url: String, source: pimble_host::http::HttpError },
    #[error("cloud service at {url} answered {status}: {body}")]
    Status { url: String, status: u16, body: String },
    #[error("cloud service response from {url} could not be parsed: {source}")]
    Decode { url: String, source: serde_json::Error },
}

impl CloudError {
    /// The HTTP status the service answered with, when it answered at all.
    pub fn status(&self) -> Option<u16> {
        match self {
            CloudError::Status { status, .. } => Some(*status),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, CloudError>;

async fn send(http: &dyn Http, method: Method, url: &str, bearer: Option<&str>, json: Option<Vec<u8>>) -> Result<HttpResponse> {
    http.send(HttpRequest { method, url: url.to_string(), bearer: bearer.map(str::to_string), body: json.map(Body::json) })
        .await
        .map_err(|e| CloudError::Request { url: url.to_string(), source: e })
}

fn finish<T: for<'de> Deserialize<'de>>(url: &str, resp: HttpResponse) -> Result<T> {
    if !resp.is_success() {
        return Err(CloudError::Status { url: url.to_string(), status: resp.status, body: resp.text() });
    }
    serde_json::from_slice::<T>(&resp.body).map_err(|e| CloudError::Decode { url: url.to_string(), source: e })
}

fn json_of<B: Serialize>(body: &B) -> Vec<u8> {
    serde_json::to_vec(body).expect("a request body serializes")
}

async fn get<T: for<'de> Deserialize<'de>>(http: &dyn Http, url: &str, bearer: Option<&str>) -> Result<T> {
    finish(url, send(http, Method::Get, url, bearer, None).await?)
}

async fn post<B: Serialize, T: for<'de> Deserialize<'de>>(http: &dyn Http, url: &str, bearer: Option<&str>, body: &B) -> Result<T> {
    finish(url, send(http, Method::Post, url, bearer, Some(json_of(body))).await?)
}

async fn put<B: Serialize, T: for<'de> Deserialize<'de>>(http: &dyn Http, url: &str, bearer: &str, body: &B) -> Result<T> {
    finish(url, send(http, Method::Put, url, Some(bearer), Some(json_of(body))).await?)
}

async fn delete(http: &dyn Http, url: &str, bearer: &str) -> Result<()> {
    let _: serde_json::Value = finish(url, send(http, Method::Delete, url, Some(bearer), None).await?)?;
    Ok(())
}

fn endpoint(base_url: &str, path: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), path)
}

/// `?root=<node id>` where a scope is meant (a share), nothing for the whole
/// store: how every scoped endpoint of the accounts service takes it.
fn root_query(root: Option<&pimble_core::NodeId>) -> String {
    root.map(|root| format!("?root={root}")).unwrap_or_default()
}
