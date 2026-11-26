//! The account page's RPCs, answered by the server that holds the account's
//! unwrapped keys (docs/ONE_CODEBASE_CONTRACT.md, "Built: wave 5, part 3"):
//! the browser's worker server, and the desktop's embedded one. A password
//! change and a new recovery code wrap the account keys again where they
//! are; a whole store's members are listed and changed by an owner, as the
//! old browser page did through the accounts service. Every one is
//! `Service`-only.
//!
//! Secrets cross this seam in two places only: the passwords of a change
//! and the current password that proves a new recovery code (to the server,
//! as `cloudSignIn`'s does), and the new recovery code (from the server,
//! once). Their `Debug` prints neither.

use pimble_core::StoreId;
use serde::{Deserialize, Serialize};

use crate::{MemberRole, ShareMember};

/// `cloudChangePassword`: the current password proves the change, as login
/// would; the new one wraps the account keys from now on.
#[derive(Clone, Serialize, Deserialize)]
pub struct CloudChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

impl std::fmt::Debug for CloudChangePasswordRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudChangePasswordRequest").finish_non_exhaustive()
    }
}

/// `cloudNewRecoveryCode`: the current password proves it, as it proves a
/// password change (Joe, 2026-11-25).
#[derive(Clone, Serialize, Deserialize)]
pub struct CloudNewRecoveryCodeRequest {
    pub current_password: String,
}

impl std::fmt::Debug for CloudNewRecoveryCodeRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudNewRecoveryCodeRequest").finish_non_exhaustive()
    }
}

/// `cloudNewRecoveryCode`'s answer: the new code, shown to the person once.
/// The server keeps no copy and logs none; the old code no longer opens the
/// account.
#[derive(Clone, Serialize, Deserialize)]
pub struct CloudNewRecoveryCodeResponse {
    pub code: String,
}

impl std::fmt::Debug for CloudNewRecoveryCodeResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudNewRecoveryCodeResponse").finish_non_exhaustive()
    }
}

/// Names a store held whole (`cloudStoreMembers`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudStoreRef {
    pub store_id: StoreId,
}

/// `cloudStoreSetMember`: give an address a role on the whole store, or
/// change the role it has. `Owner` is allowed: an owner may make another
/// owner (Joe, 2026-11-25), who must already have an account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudStoreSetMemberRequest {
    pub store_id: StoreId,
    pub email: String,
    pub role: MemberRole,
}

/// `cloudStoreRemoveMember`: a member, or an address's invitation, by address.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudStoreRemoveMemberRequest {
    pub store_id: StoreId,
    pub email: String,
}

/// Everyone who holds the whole store, and the invitations to it, as the
/// accounts service reports them now. Shares of the store are not listed:
/// they are the Share dialog's.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudStoreMembersResponse {
    pub store_id: StoreId,
    pub members: Vec<ShareMember>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_prints_no_secret() {
        let change = CloudChangePasswordRequest { current_password: "old-secret-1".into(), new_password: "new-secret-2".into() };
        let printed = format!("{change:?}");
        assert!(!printed.contains("old-secret-1") && !printed.contains("new-secret-2"));
        let code = CloudNewRecoveryCodeResponse { code: "ABCD-EFGH".into() };
        assert!(!format!("{code:?}").contains("ABCD"));
    }
}
