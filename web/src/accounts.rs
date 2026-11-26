//! The accounts service, typed: the calls the pages that run before any
//! server make (signing up, resending the verification mail, signing out of
//! the cookie session, and recovery; `docs/CRYPTO_CONTRACT.md`, "Accounts
//! service endpoints"). Signing in, the account's keys, stores, members and
//! names go through this browser's own server (`crate::worker_mode`).
//!
//! One shape here is deliberately forgiving on the way in. A key blob is a
//! JSON object in this file's requests, but the contract describes the
//! columns behind it as "JSON string", so a service that stores and echoes
//! it as a string is read back just as happily ([`lenient`]). Nothing is
//! forgiving on the way out: what this page sends is always the object.

use pimble_crypto::{AccountKeyBlob, AccountPublicKeys, KdfParams};
use serde::de::DeserializeOwned;
use serde::Serialize;
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

// ── Sign-up, the session ────────────────────────────────────────────────────

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

/// `POST /api/v1/logout`.
pub async fn logout() -> ApiResult<()> {
    http::post_empty::<Value>("/api/v1/logout").await.map(|_| ())
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

// ── Recovery ────────────────────────────────────────────────────────────────

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
