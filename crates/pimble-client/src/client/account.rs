//! The account page's calls (`pimble_rpc::account`): a new password, a new
//! recovery code, and a whole store's members.

use pimble_core::StoreId;
use pimble_rpc::{
    CloudChangePasswordRequest, CloudNewRecoveryCodeRequest, CloudStoreMembersResponse, CloudStoreRef, CloudStoreRemoveMemberRequest, CloudStoreSetMemberRequest, MemberRole, PimbleApiClient,
};

use super::{rpc_error, PimbleClient};
use crate::error::Result;

impl PimbleClient {
    /// Wrap the account keys under `new_password`, proved by `current_password`.
    pub async fn cloud_change_password(&self, current_password: impl Into<String>, new_password: impl Into<String>) -> Result<()> {
        self.client
            .cloud_change_password(CloudChangePasswordRequest { current_password: current_password.into(), new_password: new_password.into() })
            .await
            .map_err(rpc_error)?;
        Ok(())
    }

    /// A new recovery code, proved by `current_password`, which replaces the
    /// old one; answered once.
    pub async fn cloud_new_recovery_code(&self, current_password: impl Into<String>) -> Result<String> {
        let request = CloudNewRecoveryCodeRequest { current_password: current_password.into() };
        Ok(self.client.cloud_new_recovery_code(request).await.map_err(rpc_error)?.code)
    }

    /// A whole store's members and invitations.
    pub async fn cloud_store_members(&self, store_id: StoreId) -> Result<CloudStoreMembersResponse> {
        self.client.cloud_store_members(CloudStoreRef { store_id }).await.map_err(rpc_error)
    }

    /// Give `email` `role` on the whole store, or change the role it has.
    pub async fn cloud_store_set_member(&self, store_id: StoreId, email: impl Into<String>, role: MemberRole) -> Result<CloudStoreMembersResponse> {
        self.client.cloud_store_set_member(CloudStoreSetMemberRequest { store_id, email: email.into(), role }).await.map_err(rpc_error)
    }

    /// Remove a whole store's member, or withdraw an invitation, by address.
    pub async fn cloud_store_remove_member(&self, store_id: StoreId, email: impl Into<String>) -> Result<CloudStoreMembersResponse> {
        self.client.cloud_store_remove_member(CloudStoreRemoveMemberRequest { store_id, email: email.into() }).await.map_err(rpc_error)
    }
}
