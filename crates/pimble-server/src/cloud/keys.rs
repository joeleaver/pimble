//! A store's key envelopes: `GET`/`PUT /stores/{id}/keys`, and whose
//! signature on one is believed.

use pimble_crypto::KeyEnvelope;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{endpoint, get, put, root_query, Http, Result};

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

/// The caller's own key envelopes for a scope: the store key without a
/// `root`, that share's key with one; and the signers to believe.
pub async fn get_store_keys(http: &dyn Http, base_url: &str, session: &str, store_id: &str, root: Option<&pimble_core::NodeId>) -> Result<KeyGrantsResponse> {
    get(http, &endpoint(base_url, &format!("/api/v1/stores/{store_id}/keys{}", root_query(root))), Some(session)).await
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
    http: &dyn Http,
    base_url: &str,
    session: &str,
    store_id: &str,
    user_id: &str,
    key_id: Uuid,
    envelope: &KeyEnvelope,
    root: Option<&pimble_core::NodeId>,
) -> Result<()> {
    let body = PutStoreKeysRequest { envelopes: vec![EnvelopeUpsert { user_id, key_id: key_id.to_string(), envelope, root: root.map(|r| r.to_string()) }] };
    let _: serde_json::Value = put(http, &endpoint(base_url, &format!("/api/v1/stores/{store_id}/keys")), session, &body).await?;
    Ok(())
}

