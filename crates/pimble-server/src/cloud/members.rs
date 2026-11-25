//! A store's members and invitations, as the accounts service answers them.

use pimble_crypto::AccountPublicKeys;
use serde::{Deserialize, Serialize};

use super::{delete, endpoint, get, put, root_query, Http, Result};

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
    /// `""` unless the caller is an owner of the store: members of a share
    /// never see each other's emails (docs/PRESENCE_CONTRACT.md section 3).
    pub email: String,
    /// What the member goes by in this scope; `None` for an invitation, and
    /// from a service from before names.
    #[serde(default)]
    pub name: Option<String>,
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
    /// Everyone who reaches the scope and the name they go by in it
    /// (docs/PRESENCE_CONTRACT.md section 3). Empty from a service from
    /// before names.
    #[serde(default)]
    pub people: Vec<PersonView>,
}

/// One account that reaches a scope, as any member of it sees them: an id
/// and a name, never an email.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonView {
    pub user_id: String,
    pub name: String,
    #[serde(default)]
    pub role: String,
    /// Their notice key's public half (docs/NOTIFICATIONS_CONTRACT.md).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice_key: Option<String>,
}



pub async fn list_members(http: &dyn Http, base_url: &str, session: &str, store_id: &str, root: Option<&pimble_core::NodeId>) -> Result<MembersResponse> {
    get(http, &endpoint(base_url, &format!("/api/v1/stores/{store_id}/members{}", root_query(root))), Some(session)).await
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
    http: &dyn Http,
    base_url: &str,
    session: &str,
    store_id: &str,
    email: &str,
    role: &str,
    root: Option<&pimble_core::NodeId>,
    name: Option<&str>,
) -> Result<MemberView> {
    let body = PutMemberRequest { email, role, root: root.map(|r| r.to_string()), name };
    put(http, &endpoint(base_url, &format!("/api/v1/stores/{store_id}/members")), session, &body).await
}

pub async fn delete_member(http: &dyn Http, base_url: &str, session: &str, store_id: &str, user_id: &str, root: Option<&pimble_core::NodeId>) -> Result<()> {
    delete(http, &endpoint(base_url, &format!("/api/v1/stores/{store_id}/members/{user_id}{}", root_query(root))), session).await
}

/// Withdraw an invitation; `200` whether or not there was one.
pub async fn delete_invitation(http: &dyn Http, base_url: &str, session: &str, store_id: &str, email: &str, root: Option<&pimble_core::NodeId>) -> Result<()> {
    let encoded_email: String = url::form_urlencoded::byte_serialize(email.as_bytes()).collect();
    delete(http, &endpoint(base_url, &format!("/api/v1/stores/{store_id}/invitations/{encoded_email}{}", root_query(root))), session).await
}
