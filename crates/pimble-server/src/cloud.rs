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

async fn delete(url: &str, bearer: &str) -> Result<()> {
    let resp = client()
        .delete(url)
        .bearer_auth(bearer)
        .send()
        .await
        .map_err(|e| CloudError::Request { url: url.to_string(), source: e })?;
    let _: serde_json::Value = finish(url, resp).await?;
    Ok(())
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
    /// The shared node when the grant is a share of one subtree
    /// (docs/NODE_DOCUMENT_CONTRACT.md section 5), absent for a whole-store
    /// grant. Defaulted so a service from before scopes still parses.
    #[serde(default)]
    pub root: Option<String>,
    /// An owner's email when the account is not an owner of the store.
    #[serde(default)]
    pub shared_by: Option<String>,
}

impl StoreView {
    /// The shared node as a `NodeId`, if the row names one it can parse.
    pub fn scope_root(&self) -> Option<pimble_core::NodeId> {
        self.root.as_deref().and_then(|r| pimble_core::NodeId::parse(r).ok())
    }

    /// What a device with this grant may change (`Store::access`): a reader
    /// reads, everyone else edits what they reach.
    pub fn access(&self) -> pimble_core::StoreAccess {
        if self.role == "reader" {
            pimble_core::StoreAccess::Read
        } else {
            pimble_core::StoreAccess::Full
        }
    }
}

/// How the signed-in account holds one store, read off its `GET /stores`
/// rows (one per grant): the whole of it, or the shares of it it was given
/// (docs/NODE_DOCUMENT_CONTRACT.md section 5). What `sync.json` and the
/// manifest of the replica here are written from, at `cloudAddHostedStore`
/// and again at every connect of its vault link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldAs {
    /// A share's name for a share (the owner's store name never reaches a
    /// recipient), the store's otherwise. `None` when no row names the store.
    pub name: Option<String>,
    /// The shared nodes; empty for the whole store. A whole-store grant
    /// wins over any share of the same store, as it does in the token.
    pub roots: Vec<pimble_core::NodeId>,
    /// The roots among them held as a reader, when others are edited.
    pub read_only_roots: Vec<pimble_core::NodeId>,
    /// `Read` when nothing the account holds of the store may be written.
    pub access: pimble_core::StoreAccess,
    pub shared_by: Option<String>,
}

impl HeldAs {
    /// One's own store, just hosted.
    pub fn owner() -> Self {
        Self { name: None, roots: Vec::new(), read_only_roots: Vec::new(), access: pimble_core::StoreAccess::Full, shared_by: None }
    }

    /// `None` when no row names the store (nothing is known of how it is
    /// held; the hosted server's answer to the token decides what really is).
    pub fn from_rows(rows: &[StoreView], store_id: pimble_core::StoreId) -> Option<Self> {
        let id = store_id.to_string();
        let rows: Vec<&StoreView> = rows.iter().filter(|row| row.store_id == id).collect();
        if let Some(whole) = rows.iter().find(|row| row.root.is_none()) {
            return Some(Self {
                name: Some(whole.name.clone()),
                roots: Vec::new(),
                read_only_roots: Vec::new(),
                access: whole.access(),
                shared_by: whole.shared_by.clone(),
            });
        }
        let shares: Vec<(pimble_core::NodeId, &StoreView)> = rows.iter().filter_map(|row| Some((row.scope_root()?, *row))).collect();
        let (_, first) = shares.first()?;
        let reads = |row: &StoreView| row.access() == pimble_core::StoreAccess::Read;
        let all_read = shares.iter().all(|(_, row)| reads(row));
        Some(Self {
            name: Some(first.name.clone()),
            roots: shares.iter().map(|(root, _)| *root).collect(),
            read_only_roots: if all_read { Vec::new() } else { shares.iter().filter(|(_, row)| reads(row)).map(|(root, _)| *root).collect() },
            access: if all_read { pimble_core::StoreAccess::Read } else { pimble_core::StoreAccess::Full },
            shared_by: first.shared_by.clone(),
        })
    }
}

/// Whether the account's rows say it is an owner of `store_id`: a
/// whole-store grant with the owner role. Sharing is an owner's to do (the
/// accounts service takes `PUT members` and the hosted server `setScope`
/// from nobody else), so the owner's side of it runs on an owner's devices
/// only.
pub fn is_owner_of(rows: &[StoreView], store_id: pimble_core::StoreId) -> bool {
    let id = store_id.to_string();
    rows.iter().any(|row| row.store_id == id && row.root.is_none() && row.role == "owner")
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

/// An account whose signature on an envelope for this store is to be
/// believed: an owner. A share's recipient is handed the key by whichever
/// of the owner's devices gets to it, so "signed by me" is not the only
/// legitimate signer any more.
#[derive(Debug, Clone, Deserialize)]
pub struct SignerView {
    pub user_id: String,
    pub email: String,
    pub public_signing_key: String,
}

#[derive(Deserialize)]
pub struct KeyGrantsResponse {
    pub envelopes: Vec<KeyGrantView>,
    /// Defaulted so a service from before sharing still parses; then only
    /// the account's own signature is believed.
    #[serde(default)]
    pub signers: Vec<SignerView>,
}

impl KeyGrantsResponse {
    /// The signer an envelope may be verified against: the account's own
    /// signing key, or a listed signer's, whichever the envelope names; an
    /// envelope naming neither is checked against the account's own and so
    /// refused by `unwrap_key`'s signature check.
    pub fn expected_signer<'a>(&'a self, envelope: &'a KeyEnvelope, own_signer: &'a str) -> &'a str {
        if envelope.signer == own_signer {
            return own_signer;
        }
        self.signers
            .iter()
            .find(|s| s.public_signing_key == envelope.signer)
            .map(|s| s.public_signing_key.as_str())
            .unwrap_or(own_signer)
    }
}

/// `?root=<node id>` where a scope is meant (a share), nothing for the whole
/// store: how every scoped endpoint of the accounts service takes it.
fn root_query(root: Option<&pimble_core::NodeId>) -> String {
    root.map(|root| format!("?root={root}")).unwrap_or_default()
}

/// The caller's own key envelopes for a scope: the store key without a
/// `root`, that share's key with one; and the signers to believe.
pub async fn get_store_keys(base_url: &str, session: &str, store_id: &str, root: Option<&pimble_core::NodeId>) -> Result<KeyGrantsResponse> {
    get(&endpoint(base_url, &format!("/api/v1/stores/{store_id}/keys{}", root_query(root))), Some(session)).await
}

#[derive(Serialize)]
struct EnvelopeUpsert<'a> {
    user_id: &'a str,
    key_id: String,
    envelope: &'a KeyEnvelope,
    /// The share the key is for; absent for the store key.
    #[serde(skip_serializing_if = "Option::is_none")]
    root: Option<String>,
}

#[derive(Serialize)]
struct PutStoreKeysRequest<'a> {
    envelopes: Vec<EnvelopeUpsert<'a>>,
}

/// Upload one (user, key id) envelope for `store_id`, signed by the caller
/// (verified by the accounts service against the caller's own signing key):
/// the store key's, or with `root` that share's key.
pub async fn put_store_key(
    base_url: &str,
    session: &str,
    store_id: &str,
    user_id: &str,
    key_id: Uuid,
    envelope: &KeyEnvelope,
    root: Option<&pimble_core::NodeId>,
) -> Result<()> {
    let body = PutStoreKeysRequest { envelopes: vec![EnvelopeUpsert { user_id, key_id: key_id.to_string(), envelope, root: root.map(|r| r.to_string()) }] };
    let _: serde_json::Value = put(&endpoint(base_url, &format!("/api/v1/stores/{store_id}/keys")), session, &body).await?;
    Ok(())
}

// ── GET/PUT/DELETE /stores/{id}/members, DELETE /stores/{id}/invitations ──
//
// A share is a scoped grant (docs/NODE_DOCUMENT_CONTRACT.md section 5;
// `pimble-cloud`'s README, "Sharing"): every call here takes the shared node
// as `root`, and means the whole store without one.

/// One member or invitation of a scope, as `GET`/`PUT .../members` answer.
#[derive(Debug, Clone, Deserialize)]
pub struct MemberView {
    /// `None` for an invitation: there is no account to name yet.
    #[serde(default)]
    pub user_id: Option<String>,
    pub email: String,
    pub role: String,
    /// `"active"` (a grant) or `"invited"` (an invitation).
    pub status: String,
    /// Whether the member has been handed the scope's key: what the owner's
    /// key sweep looks for.
    #[serde(default)]
    pub has_key: bool,
    /// Filled in for an owner caller and an active member only.
    #[serde(default)]
    pub public_keys: Option<AccountPublicKeys>,
}

impl MemberView {
    pub fn is_active(&self) -> bool {
        self.status == "active"
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct MembersResponse {
    pub members: Vec<MemberView>,
    /// The share's name; `None` for a whole-store listing.
    #[serde(default)]
    pub share_name: Option<String>,
}

pub async fn list_members(base_url: &str, session: &str, store_id: &str, root: Option<&pimble_core::NodeId>) -> Result<MembersResponse> {
    get(&endpoint(base_url, &format!("/api/v1/stores/{store_id}/members{}", root_query(root))), Some(session)).await
}

#[derive(Serialize)]
struct PutMemberRequest<'a> {
    email: &'a str,
    role: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    root: Option<String>,
    /// The share's name, required with a `root`.
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
}

/// Give `email` a role on a scope: a grant when the address has an account
/// (the answer then carries its `public_keys`), an invitation when not.
pub async fn put_member(
    base_url: &str,
    session: &str,
    store_id: &str,
    email: &str,
    role: &str,
    root: Option<&pimble_core::NodeId>,
    name: Option<&str>,
) -> Result<MemberView> {
    let body = PutMemberRequest { email, role, root: root.map(|r| r.to_string()), name };
    put(&endpoint(base_url, &format!("/api/v1/stores/{store_id}/members")), session, &body).await
}

pub async fn delete_member(base_url: &str, session: &str, store_id: &str, user_id: &str, root: Option<&pimble_core::NodeId>) -> Result<()> {
    delete(&endpoint(base_url, &format!("/api/v1/stores/{store_id}/members/{user_id}{}", root_query(root))), session).await
}

/// Withdraw an invitation; `200` whether or not there was one.
pub async fn delete_invitation(base_url: &str, session: &str, store_id: &str, email: &str, root: Option<&pimble_core::NodeId>) -> Result<()> {
    let encoded_email: String = url::form_urlencoded::byte_serialize(email.as_bytes()).collect();
    delete(&endpoint(base_url, &format!("/api/v1/stores/{store_id}/invitations/{encoded_email}{}", root_query(root))), session).await
}
