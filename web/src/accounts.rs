//! The accounts service, typed.
//!
//! Every endpoint under "Accounts service endpoints" in
//! `docs/CRYPTO_CONTRACT.md`, plus the phase-1 ones the account page still
//! uses (members, store deletion, resend verification).
//!
//! Two shapes here are deliberately forgiving on the way in. A key blob and a
//! key envelope are JSON objects in this file's requests, but the contract
//! describes the columns behind them as "JSON string", so a service that
//! stores and echoes them as strings is read back just as happily
//! ([`lenient`]). Nothing is forgiving on the way out: what this page sends is
//! always the object.

use pimble_crypto::{AccountKeyBlob, AccountPublicKeys, KdfParams, KeyEnvelope, KeyId};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::http::{self, ApiError, ApiResult};

/// Percent-encode a query parameter value. Only the characters that would
/// otherwise end the value or start another parameter need escaping here, but
/// escaping everything outside the unreserved set is simpler to be sure of.
fn query_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// A field the service may hand back as an object or as a string holding that
/// object's JSON. Both parse to the same `T`.
fn lenient<T: DeserializeOwned>(value: &Value, what: &'static str) -> ApiResult<T> {
    let parsed = match value {
        Value::String(s) => serde_json::from_str::<T>(s),
        other => serde_json::from_value::<T>(other.clone()),
    };
    parsed.map_err(|e| ApiError::local(format!("the {what} the server sent did not parse: {e}")))
}

// ── Sign-up, sign-in, the session ───────────────────────────────────────────

/// `GET /api/v1/kdf?email=` — the salt and costs to derive this email's keys
/// with. An unknown email gets a deterministic decoy, so a failure here never
/// means "no such account".
pub async fn kdf(email: &str) -> ApiResult<KdfParams> {
    http::get(&format!("/api/v1/kdf?email={}", query_escape(email))).await
}

/// The signup body: a password never appears in it, only what the password
/// derived to (`auth_key`) and what it wrapped (`account_key_blob`).
#[derive(Debug, Serialize)]
pub struct SignupRequest {
    pub email: String,
    pub auth_key: String,
    pub kdf: KdfParams,
    pub public_keys: AccountPublicKeys,
    pub account_key_blob: AccountKeyBlob,
    /// The recovery code's own Argon2id salt (base64url), whose KEK wraps the
    /// same account keys a second time.
    pub recovery_salt: String,
    pub recovery_key_blob: AccountKeyBlob,
}

/// `POST /api/v1/signup`. Answers 202 and sends a verification mail; it never
/// starts a session, so there is nothing in the answer worth keeping.
pub async fn signup(request: &SignupRequest) -> ApiResult<()> {
    http::post::<_, Value>("/api/v1/signup", request).await.map(|_| ())
}

#[derive(Debug, Serialize)]
struct LoginRequest<'a> {
    email: &'a str,
    auth_key: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserView {
    pub id: String,
    pub email: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoginResponse {
    pub user: UserView,
}

/// `POST /api/v1/login`. The server sees `auth_key`, never the password.
pub async fn login(email: &str, auth_key: &str) -> ApiResult<LoginResponse> {
    http::post("/api/v1/login", &LoginRequest { email, auth_key }).await
}

/// `POST /api/v1/logout`.
pub async fn logout() -> ApiResult<()> {
    http::post_empty::<Value>("/api/v1/logout").await.map(|_| ())
}

/// `GET /api/v1/me` — who this session belongs to.
pub async fn me() -> ApiResult<UserView> {
    http::get("/api/v1/me").await
}

#[derive(Debug, Serialize)]
struct ResendRequest<'a> {
    email: &'a str,
}

/// `POST /api/v1/resend-verification`. Always 202, whatever the address is.
pub async fn resend_verification(email: &str) -> ApiResult<()> {
    http::post::<_, Value>("/api/v1/resend-verification", &ResendRequest { email })
        .await
        .map(|_| ())
}

/// What `GET /api/v1/me/keys` answers: never the recovery blob.
#[derive(Debug, Clone)]
pub struct MyKeys {
    pub public_keys: AccountPublicKeys,
    pub kdf: KdfParams,
    pub account_key_blob: AccountKeyBlob,
}

/// `GET /api/v1/me/keys` — the wrapped account keys, which only the password's
/// KEK opens. The server holds this and cannot read it.
pub async fn my_keys() -> ApiResult<MyKeys> {
    let raw: Value = http::get("/api/v1/me/keys").await?;
    Ok(MyKeys {
        public_keys: lenient(&raw["public_keys"], "public keys")?,
        kdf: lenient(&raw["kdf"], "KDF parameters")?,
        account_key_blob: lenient(&raw["account_key_blob"], "account key blob")?,
    })
}

/// `GET /api/v1/users/lookup?email=` — a verified user's public keys, for
/// wrapping a store key to them. 404 for anyone else.
#[derive(Debug, Clone, Deserialize)]
pub struct UserLookup {
    pub id: String,
    pub public_keys: AccountPublicKeys,
}

pub async fn lookup_user(email: &str) -> ApiResult<UserLookup> {
    http::get(&format!("/api/v1/users/lookup?email={}", query_escape(email))).await
}

// ── Stores and members ──────────────────────────────────────────────────────

/// One store this account has a grant on.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StoreView {
    pub store_id: String,
    pub name: String,
    pub role: String,
    #[serde(default)]
    pub created_at: String,
    /// `plain` or `vault`. Absent on a service that predates the kind column,
    /// which means plain.
    #[serde(default = "default_kind")]
    pub kind: String,
}

fn default_kind() -> String {
    "plain".to_string()
}

pub async fn list_stores() -> ApiResult<Vec<StoreView>> {
    http::get("/api/v1/stores").await
}

#[derive(Debug, Serialize)]
struct CreateStoreRequest<'a> {
    name: &'a str,
    kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    store_id: Option<&'a str>,
}

/// `POST /api/v1/stores`. `kind` is `vault` for an encrypted store, which is
/// what the account page creates by default.
pub async fn create_store(name: &str, kind: &str, store_id: Option<&str>) -> ApiResult<StoreView> {
    http::post("/api/v1/stores", &CreateStoreRequest { name, kind, store_id }).await
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MemberView {
    pub user_id: String,
    pub email: String,
    pub role: String,
}

pub async fn list_members(store_id: &str) -> ApiResult<Vec<MemberView>> {
    http::get(&format!("/api/v1/stores/{store_id}/members")).await
}

#[derive(Debug, Serialize)]
struct PutMemberRequest<'a> {
    email: &'a str,
    role: &'a str,
}

pub async fn put_member(store_id: &str, email: &str, role: &str) -> ApiResult<MemberView> {
    http::put(
        &format!("/api/v1/stores/{store_id}/members"),
        &PutMemberRequest { email, role },
    )
    .await
}

pub async fn delete_member(store_id: &str, user_id: &str) -> ApiResult<()> {
    http::delete::<Value>(&format!("/api/v1/stores/{store_id}/members/{user_id}"))
        .await
        .map(|_| ())
}

// ── Key envelopes ───────────────────────────────────────────────────────────

/// One envelope as the service stores it: whose it is, which key it carries,
/// and the sealed key itself.
#[derive(Debug, Clone, Serialize)]
pub struct EnvelopeUpload {
    pub user_id: String,
    pub key_id: KeyId,
    pub envelope: KeyEnvelope,
}

#[derive(Debug, Serialize)]
struct PutKeysRequest {
    envelopes: Vec<EnvelopeUpload>,
}

/// `GET /api/v1/stores/{id}/keys` — the caller's own envelopes.
///
/// Tolerant of both plausible shapes (`{ envelopes: [...] }` and a bare
/// array), and of an envelope carried as an object or as its JSON text: B and
/// C are landing their halves concurrently and this page would rather work
/// against either than insist.
pub async fn store_keys(store_id: &str) -> ApiResult<Vec<KeyEnvelope>> {
    let raw: Value = http::get(&format!("/api/v1/stores/{store_id}/keys")).await?;
    let list = match &raw {
        Value::Array(items) => items.clone(),
        Value::Object(map) => match map.get("envelopes") {
            Some(Value::Array(items)) => items.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };

    let mut out = Vec::with_capacity(list.len());
    for item in &list {
        // An entry is either the envelope itself or a row wrapping one.
        let candidate = match item {
            Value::Object(map) if map.contains_key("envelope") => &map["envelope"],
            other => other,
        };
        out.push(lenient::<KeyEnvelope>(candidate, "key envelope")?);
    }
    Ok(out)
}

/// `PUT /api/v1/stores/{id}/keys` — upsert envelopes, each signed by this
/// account's signing key.
pub async fn put_store_keys(store_id: &str, envelopes: Vec<EnvelopeUpload>) -> ApiResult<()> {
    http::put::<_, Value>(
        &format!("/api/v1/stores/{store_id}/keys"),
        &PutKeysRequest { envelopes },
    )
    .await
    .map(|_| ())
}
