//! The account's own material and the lookup an owner adds a member with:
//! `POST /me/password`, `POST /me/recovery-code`, `GET /users/lookup`
//! (docs/CRYPTO_CONTRACT.md "Phase 2a-2"). The bodies carry an `auth_key`
//! and wrapped keys; nothing here logs a body.

use pimble_crypto::{AccountKeyBlob, AccountPublicKeys, KdfParams};
use serde::{Deserialize, Serialize};

use super::{endpoint, get, post, Http, Result};

#[derive(Serialize)]
struct ChangePasswordBody<'a> {
    current_auth_key: &'a str,
    auth_key: &'a str,
    kdf: &'a KdfParams,
    account_key_blob: &'a AccountKeyBlob,
}

/// Replace the password's material: the service checks `current_auth_key`
/// as login does (401 when it is wrong), then stores the new hash, the
/// parameters and the wrapped keys in one update.
pub async fn change_password(
    http: &dyn Http,
    base_url: &str,
    session: &str,
    current_auth_key: &str,
    auth_key: &str,
    kdf: &KdfParams,
    account_key_blob: &AccountKeyBlob,
) -> Result<()> {
    let body = ChangePasswordBody { current_auth_key, auth_key, kdf, account_key_blob };
    let _: serde_json::Value = post(http, &endpoint(base_url, "/api/v1/me/password"), Some(session), &body).await?;
    Ok(())
}

#[derive(Serialize)]
struct ChangeRecoveryCodeBody<'a> {
    recovery_salt: &'a str,
    recovery_key_blob: &'a AccountKeyBlob,
}

/// Replace the recovery code's material in one update.
pub async fn change_recovery_code(http: &dyn Http, base_url: &str, session: &str, recovery_salt: &str, recovery_key_blob: &AccountKeyBlob) -> Result<()> {
    let body = ChangeRecoveryCodeBody { recovery_salt, recovery_key_blob };
    let _: serde_json::Value = post(http, &endpoint(base_url, "/api/v1/me/recovery-code"), Some(session), &body).await?;
    Ok(())
}

/// A verified account, as `GET /users/lookup` answers it.
#[derive(Debug, Clone, Deserialize)]
pub struct UserLookup {
    pub id: String,
    pub public_keys: AccountPublicKeys,
}

/// The verified account with this address; the service answers 404 for an
/// address with none.
pub async fn lookup_user(http: &dyn Http, base_url: &str, session: &str, email: &str) -> Result<UserLookup> {
    let encoded_email: String = url::form_urlencoded::byte_serialize(email.as_bytes()).collect();
    get(http, &endpoint(base_url, &format!("/api/v1/users/lookup?email={encoded_email}")), Some(session)).await
}

/// The sentence the accounts service put in a refusal's `message`, made a
/// sentence (a capital and a full stop), when it gave one.
pub fn service_message(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let message = value.get("message")?.as_str()?.trim();
    let mut chars = message.chars();
    let first = chars.next()?;
    let mut sentence: String = first.to_uppercase().chain(chars).collect();
    if !sentence.ends_with('.') {
        sentence.push('.');
    }
    Some(sentence)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_service_message_becomes_a_sentence() {
        let body = r#"{"error":"bad_request","message":"an owner must already have a verified Pimble account; invite them as an editor, commenter or reader"}"#;
        assert_eq!(
            service_message(body).as_deref(),
            Some("An owner must already have a verified Pimble account; invite them as an editor, commenter or reader.")
        );
        assert_eq!(service_message("not json"), None);
        assert_eq!(service_message(r#"{"message":""}"#), None);
    }
}
