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

// ── Recovery, and changing what the password is ─────────────────────────────

#[derive(Debug, Serialize)]
struct EmailRequest<'a> {
    email: &'a str,
}

/// `POST /api/v1/recover/start`. Always 202, whatever the address is: an
/// answer that varied would say who has an account here.
pub async fn recover_start(email: &str) -> ApiResult<()> {
    http::post::<_, Value>("/api/v1/recover/start", &EmailRequest { email })
        .await
        .map(|_| ())
}

/// What a recovery token buys: the wrapped keys, and the parameters for the
/// one KEK that opens them.
///
/// This is the only place the recovery blob is ever served, and only to
/// whoever holds the emailed token. It is still useless without the code.
#[derive(Debug, Clone)]
pub struct RecoveryMaterial {
    pub email: String,
    /// The recovery code's Argon2id parameters, salt included.
    pub kdf: KdfParams,
    pub recovery_key_blob: AccountKeyBlob,
    pub public_keys: AccountPublicKeys,
}

/// `GET /api/v1/recover/{token}`. A 404 means the link is unknown, expired or
/// already used.
pub async fn recover_material(token: &str) -> ApiResult<RecoveryMaterial> {
    let raw: Value = http::get(&format!("/api/v1/recover/{}", query_escape(token))).await?;

    // The salt travels beside the costs rather than inside them, because the
    // server stores it on the user.
    let costs = &raw["recovery_kdf"];
    let salt = raw["recovery_salt"]
        .as_str()
        .ok_or_else(|| ApiError::local("the recovery salt was missing"))?
        .to_string();
    let kdf = KdfParams {
        salt,
        m_cost: costs["m_cost"].as_u64().unwrap_or(pimble_crypto::KDF_M_COST_KIB as u64) as u32,
        t_cost: costs["t_cost"].as_u64().unwrap_or(pimble_crypto::KDF_T_COST as u64) as u32,
        p_cost: costs["p_cost"].as_u64().unwrap_or(pimble_crypto::KDF_P_COST as u64) as u32,
    };

    Ok(RecoveryMaterial {
        email: raw["email"].as_str().unwrap_or_default().to_string(),
        kdf,
        recovery_key_blob: lenient(&raw["recovery_key_blob"], "recovery key blob")?,
        public_keys: lenient(&raw["public_keys"], "public keys")?,
    })
}

/// Everything the client rotated while it held the account keys: the password
/// material and a fresh recovery code's.
#[derive(Debug, Serialize)]
pub struct RecoverCompleteRequest {
    pub auth_key: String,
    pub kdf: KdfParams,
    pub account_key_blob: AccountKeyBlob,
    pub recovery_salt: String,
    pub recovery_key_blob: AccountKeyBlob,
}

/// `POST /api/v1/recover/{token}/complete`. Consumes the token and drops every
/// session, so the answer starts none: the next step is signing in.
pub async fn recover_complete(token: &str, request: &RecoverCompleteRequest) -> ApiResult<()> {
    http::post::<_, Value>(
        &format!("/api/v1/recover/{}/complete", query_escape(token)),
        request,
    )
    .await
    .map(|_| ())
}

/// `POST /api/v1/recover/{token}/delete-account`, for someone who cannot
/// recover. There is nothing else this token can do for them.
pub async fn recover_delete_account(token: &str) -> ApiResult<()> {
    http::post_empty::<Value>(&format!(
        "/api/v1/recover/{}/delete-account",
        query_escape(token)
    ))
    .await
    .map(|_| ())
}

#[derive(Debug, Serialize)]
pub struct ChangePasswordRequest {
    /// The current password's `auth_key`, checked the way login checks one.
    pub current_auth_key: String,
    pub auth_key: String,
    pub kdf: KdfParams,
    pub account_key_blob: AccountKeyBlob,
}

/// `POST /api/v1/me/password`.
pub async fn change_password(request: &ChangePasswordRequest) -> ApiResult<()> {
    http::post::<_, Value>("/api/v1/me/password", request).await.map(|_| ())
}

#[derive(Debug, Serialize)]
pub struct NewRecoveryCodeRequest {
    pub recovery_salt: String,
    pub recovery_key_blob: AccountKeyBlob,
}

/// `POST /api/v1/me/recovery-code`, replacing the old code's material with a
/// new one the client wrapped while holding the keys.
pub async fn replace_recovery_code(request: &NewRecoveryCodeRequest) -> ApiResult<()> {
    http::post::<_, Value>("/api/v1/me/recovery-code", request).await.map(|_| ())
}

// ── Stores and members ──────────────────────────────────────────────────────

/// One **grant** this account holds, not one store: a store somebody holds
/// two shares of is two rows with the same `store_id` and different `root`s
/// (docs/NODE_DOCUMENT_CONTRACT.md section 5, and `crates/pimble-cloud/README.md`
/// "Sharing").
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StoreView {
    pub store_id: String,
    /// The store's name for a whole-store grant, and the **share's own name**
    /// for a scoped one. The owner's store name never reaches a recipient, so
    /// this is the only name a scoped row may be shown under — the hosted
    /// server's `listStores` still answers with the store's.
    pub name: String,
    pub role: String,
    #[serde(default)]
    pub created_at: String,
    /// `plain` or `vault`. Absent on a service that predates the kind column,
    /// which means plain.
    #[serde(default = "default_kind")]
    pub kind: String,
    /// The shared node when this row is a share's grant; absent for a
    /// whole-store one, and on a service from before shares had scopes.
    #[serde(default)]
    pub root: Option<String>,
    /// An owner's email when this row is somebody else's share; absent for
    /// one's own store.
    #[serde(default)]
    pub shared_by: Option<String>,
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
    /// `None` for an invitation: an address asked but with no account yet, so
    /// there is nothing to name and nothing to remove by id.
    #[serde(default)]
    pub user_id: Option<String>,
    pub email: String,
    pub role: String,
    /// The scope this membership is of: the shared node, or `None` for the
    /// whole store.
    #[serde(default)]
    pub root: Option<String>,
    /// `"active"` (a grant) or `"invited"`. Absent on a service from before
    /// invitations, where every row was a grant.
    #[serde(default = "default_status")]
    pub status: String,
    /// Whether this member has been handed the scope's key yet. An active
    /// member with `false` is the desktop's "waiting for the key".
    #[serde(default)]
    pub has_key: bool,
}

fn default_status() -> String {
    "active".to_string()
}

/// What `GET /stores/{id}/members` answers now: the rows, and the scope's own
/// name beside them (`None` for a whole-store listing, which `GET /stores`
/// names instead).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct MembersView {
    #[serde(default)]
    pub members: Vec<MemberView>,
    #[serde(default)]
    pub share_name: Option<String>,
}

/// `GET /api/v1/stores/{id}/members[?root=]`.
///
/// The answer used to be a bare array and is now an object; both are read, so
/// this page works against a service on either side of that change. Anything
/// else is the object's own parse error, which says what did not fit.
pub async fn list_members(store_id: &str, root: Option<&str>) -> ApiResult<MembersView> {
    let raw: Value = http::get(&format!(
        "/api/v1/stores/{store_id}/members{}",
        root_query(root)
    ))
    .await?;
    match raw {
        Value::Array(_) => Ok(MembersView {
            members: serde_json::from_value(raw)
                .map_err(|e| ApiError::local(format!("the member list the server sent did not parse: {e}")))?,
            share_name: None,
        }),
        other => serde_json::from_value(other)
            .map_err(|e| ApiError::local(format!("the member list the server sent did not parse: {e}"))),
    }
}

/// `?root=<node id>` where a scope is meant (a share), nothing for the whole
/// store: how every scoped endpoint of the accounts service takes it.
fn root_query(root: Option<&str>) -> String {
    root.map(|root| format!("?root={}", query_escape(root))).unwrap_or_default()
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
    /// Which scope key this envelope carries: the shared node for a share's
    /// key, absent for the store key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}

/// An account whose signature on a key envelope is worth believing besides
/// one's own: the store's owners, as `GET /stores/{id}/keys` lists them. A
/// recipient is handed a share's key by its owner, so "signed by me" is not
/// the only legitimate signature any more.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SignerView {
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub email: String,
    pub public_signing_key: String,
}

/// The caller's own envelopes for one scope, and who may have signed them.
#[derive(Debug, Clone, Default)]
pub struct ScopeKeys {
    pub envelopes: Vec<KeyEnvelope>,
    pub signers: Vec<SignerView>,
}

#[derive(Debug, Serialize)]
struct PutKeysRequest {
    envelopes: Vec<EnvelopeUpload>,
}

/// `GET /api/v1/stores/{id}/keys[?root=]` — the caller's own envelopes for one
/// scope (the store key without a `root`, that share's key with one), and the
/// signers whose signature on them is legitimate.
///
/// Tolerant of both plausible shapes (`{ envelopes: [...] }` and a bare
/// array), and of an envelope carried as an object or as its JSON text: B and
/// C are landing their halves concurrently and this page would rather work
/// against either than insist. A service from before sharing sends no
/// `signers`, and then only the account's own signature is believed.
pub async fn store_keys(store_id: &str, root: Option<&str>) -> ApiResult<ScopeKeys> {
    let raw: Value = http::get(&format!(
        "/api/v1/stores/{store_id}/keys{}",
        root_query(root)
    ))
    .await?;
    let list = match &raw {
        Value::Array(items) => items.clone(),
        Value::Object(map) => match map.get("envelopes") {
            Some(Value::Array(items)) => items.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };

    let mut envelopes = Vec::with_capacity(list.len());
    for item in &list {
        // An entry is either the envelope itself or a row wrapping one.
        let candidate = match item {
            Value::Object(map) if map.contains_key("envelope") => &map["envelope"],
            other => other,
        };
        envelopes.push(lenient::<KeyEnvelope>(candidate, "key envelope")?);
    }

    let signers = match raw.get("signers") {
        Some(value) => lenient::<Vec<SignerView>>(value, "key signers")?,
        None => Vec::new(),
    };
    Ok(ScopeKeys { envelopes, signers })
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
