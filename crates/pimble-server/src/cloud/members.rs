//! A store's members and invitations, as the accounts service answers them.

use pimble_crypto::AccountPublicKeys;
use serde::{Deserialize, Serialize};

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

