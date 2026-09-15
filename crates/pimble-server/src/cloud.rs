//! HTTP calls to the Pimble Cloud accounts service
//! (docs/CRYPTO_CONTRACT.md "Accounts service endpoints (C)"), used by the
//! `cloud*` RPC handlers (`crate::handler`) and by
//! [`crate::vault_link::VaultLink`] to mint a fresh JWT before every
//! connect. Plain `reqwest` JSON calls, mirroring the request/response
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

use pimble_crypto::{AccountKeyBlob, AccountPublicKeys, KdfParams, KeyEnvelope};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    #[error("could not reach the cloud service at {url}: {source}")]
    Request { url: String, source: reqwest::Error },
    #[error("cloud service at {url} answered {status}: {body}")]
    Status { url: String, status: u16, body: String },
    #[error("cloud service response from {url} could not be parsed: {source}")]
    Decode { url: String, source: reqwest::Error },
}

pub type Result<T> = std::result::Result<T, CloudError>;

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .expect("a reqwest client with only a timeout set always builds")
}

async fn finish<T: for<'de> Deserialize<'de>>(url: &str, resp: reqwest::Response) -> Result<T> {
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(CloudError::Status { url: url.to_string(), status: status.as_u16(), body });
    }
    resp.json::<T>().await.map_err(|e| CloudError::Decode { url: url.to_string(), source: e })
}

async fn get<T: for<'de> Deserialize<'de>>(url: &str, bearer: Option<&str>) -> Result<T> {
    let mut req = client().get(url);
    if let Some(token) = bearer {
        req = req.bearer_auth(token);
    }
    let resp = req.send().await.map_err(|e| CloudError::Request { url: url.to_string(), source: e })?;
    finish(url, resp).await
}

async fn post<B: Serialize, T: for<'de> Deserialize<'de>>(url: &str, bearer: Option<&str>, body: &B) -> Result<T> {
    let mut req = client().post(url).json(body);
    if let Some(token) = bearer {
        req = req.bearer_auth(token);
    }
    let resp = req.send().await.map_err(|e| CloudError::Request { url: url.to_string(), source: e })?;
    finish(url, resp).await
}

async fn put<B: Serialize, T: for<'de> Deserialize<'de>>(url: &str, bearer: &str, body: &B) -> Result<T> {
    let resp = client()
        .put(url)
        .bearer_auth(bearer)
        .json(body)
        .send()
        .await
        .map_err(|e| CloudError::Request { url: url.to_string(), source: e })?;
    finish(url, resp).await
}

fn endpoint(base_url: &str, path: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), path)
}

// ── GET /kdf ──────────────────────────────────────────────────────────────

pub async fn kdf(base_url: &str, email: &str) -> Result<KdfParams> {
    let encoded_email: String = url::form_urlencoded::byte_serialize(email.as_bytes()).collect();
    get(&endpoint(base_url, &format!("/api/v1/kdf?email={encoded_email}")), None).await
}

// ── POST /login ───────────────────────────────────────────────────────────

#[derive(Serialize)]
struct LoginRequest<'a> {
    email: &'a str,
    auth_key: &'a str,
}

#[derive(Deserialize)]
pub struct UserView {
    pub id: String,
    pub email: String,
}

#[derive(Deserialize)]
pub struct LoginResponse {
    pub user: UserView,
    pub session: String,
}

pub async fn login(base_url: &str, email: &str, auth_key: &str) -> Result<LoginResponse> {
    post(&endpoint(base_url, "/api/v1/login"), None, &LoginRequest { email, auth_key }).await
}

// ── GET /me/keys ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct MeKeysResponse {
    pub public_keys: AccountPublicKeys,
    pub kdf: KdfParams,
    pub account_key_blob: AccountKeyBlob,
}

pub async fn me_keys(base_url: &str, session: &str) -> Result<MeKeysResponse> {
    get(&endpoint(base_url, "/api/v1/me/keys"), Some(session)).await
}

// ── POST /token ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TokenResponse {
    pub token: String,
    #[allow(dead_code)]
    pub exp: i64,
    pub rpc_url: String,
}

/// Mint a fresh JWT for the Pimble RPC server (docs/CRYPTO_CONTRACT.md: "the
/// link mints a JWT through `POST /api/v1/token` before every connect").
pub async fn mint_token(base_url: &str, session: &str) -> Result<TokenResponse> {
    post(&endpoint(base_url, "/api/v1/token"), Some(session), &serde_json::json!({})).await
}

// ── POST /stores, GET /stores ─────────────────────────────────────────────

#[derive(Serialize)]
struct CreateStoreRequest<'a> {
    name: &'a str,
    kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    store_id: Option<&'a str>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StoreView {
    pub store_id: String,
    pub name: String,
    pub role: String,
    pub kind: String,
    pub created_at: String,
}

pub async fn create_store(base_url: &str, session: &str, name: &str, kind: &str, store_id: Option<&str>) -> Result<StoreView> {
    post(&endpoint(base_url, "/api/v1/stores"), Some(session), &CreateStoreRequest { name, kind, store_id }).await
}

pub async fn list_stores(base_url: &str, session: &str) -> Result<Vec<StoreView>> {
    get(&endpoint(base_url, "/api/v1/stores"), Some(session)).await
}

// ── GET/PUT /stores/{id}/keys ─────────────────────────────────────────────

#[derive(Deserialize)]
pub struct KeyGrantView {
    pub key_id: String,
    pub envelope: KeyEnvelope,
}

#[derive(Deserialize)]
pub struct KeyGrantsResponse {
    pub envelopes: Vec<KeyGrantView>,
}

pub async fn get_store_keys(base_url: &str, session: &str, store_id: &str) -> Result<KeyGrantsResponse> {
    get(&endpoint(base_url, &format!("/api/v1/stores/{store_id}/keys")), Some(session)).await
}

#[derive(Serialize)]
struct EnvelopeUpsert<'a> {
    user_id: &'a str,
    key_id: String,
    envelope: &'a KeyEnvelope,
}

#[derive(Serialize)]
struct PutStoreKeysRequest<'a> {
    envelopes: Vec<EnvelopeUpsert<'a>>,
}

/// Upload one (user, key id) envelope for `store_id`, signed by the caller
/// (verified by the accounts service against the caller's own signing key).
pub async fn put_store_key(base_url: &str, session: &str, store_id: &str, user_id: &str, key_id: Uuid, envelope: &KeyEnvelope) -> Result<()> {
    let body = PutStoreKeysRequest { envelopes: vec![EnvelopeUpsert { user_id, key_id: key_id.to_string(), envelope }] };
    let _: serde_json::Value = put(&endpoint(base_url, &format!("/api/v1/stores/{store_id}/keys")), session, &body).await?;
    Ok(())
}
