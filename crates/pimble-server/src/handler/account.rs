//! The account page's RPCs (`pimble_rpc::account`, docs/ONE_CODEBASE_CONTRACT.md
//! "Built: wave 5, part 3"): a new password and a new recovery code, made
//! where the account's unwrapped keys are (the browser's worker server, or
//! the desktop's embedded one), and a whole store's members, changed by an
//! owner.
//!
//! The account keys never change: a password or a recovery code is only what
//! wraps them. Each change is one request to the accounts service, which
//! stores the new hash, parameters and wrapped keys in one update, so at every
//! moment the account opens with exactly one password (and one code). Nothing
//! on this server depends on the password: the keystore holds the keys
//! unwrapped and the session, and both stay. Every other session of the
//! account ends with a password change, in the service's same update (Joe,
//! 2026-11-25; docs/ONE_CODEBASE_CONTRACT.md "Built: wave 5, part 4").
//!
//! No password, auth key or code is logged or kept; the new recovery code
//! leaves this server once, in the answer.

use jsonrpsee::types::ErrorObjectOwned;
use pimble_core::StoreId;
use pimble_rpc::{to_rpc_error, CloudStoreMembersResponse, MemberRole, ShareMember, ShareMemberStatus};
use tracing::{info, warn};

use super::{hosted_plain, RpcHandler};
use crate::cloud::{CloudError, MemberView, Refusal};
use crate::keystore::SignedInAccount;

/// The shortest password the account pages accept (`web/src/pages`).
pub(crate) const MIN_PASSWORD: usize = 8;

const NOT_SIGNED_IN: &str = "Sign in to Pimble Cloud first.";
const LAST_OWNER: &str = "A store always keeps at least one owner. Make someone else an owner first.";
const OWNERS_ONLY: &str = "Only an owner of this store can see and change who holds it.";
/// `cloudNewRecoveryCode` when its request went out and no answer came back:
/// the service may hold the new code, which this computer never showed.
pub(crate) const RECOVERY_CODE_OUTCOME_UNKNOWN: &str =
    "Pimble Cloud did not answer, so this computer cannot tell whether the new recovery code was saved. Your old recovery code may have stopped working, so make a new one now.";

/// A refusal of the accounts service's on a store's members, said as a
/// sentence: the ones this page meets in its own words, any other in the
/// service's.
fn refusal(e: CloudError) -> ErrorObjectOwned {
    match &e {
        CloudError::Status { status: 409, .. } => to_rpc_error(LAST_OWNER),
        CloudError::Status { status: 403, .. } => to_rpc_error(OWNERS_ONLY),
        CloudError::Status { status: 401, .. } => to_rpc_error(Refusal::SignedOut),
        CloudError::Status { status: 429, .. } => to_rpc_error(Refusal::TooFast),
        CloudError::Status { status, .. } if (500..600).contains(status) => to_rpc_error(Refusal::Unavailable),
        _ => account_refusal(e),
    }
}

/// A refusal of a call on the account's own material (`GET /me/keys`,
/// `POST /me/password`, `POST /me/recovery-code`), which
/// `crate::cloud::with_session` has already said as a sentence where it
/// knows one; a 400 is said in the service's words.
fn account_refusal(e: CloudError) -> ErrorObjectOwned {
    match &e {
        // The service's refusals in its own sentence: a bad request, and a
        // change refused because the account changed while it waited.
        CloudError::Status { status: 400 | 409, body, .. } => to_rpc_error(crate::cloud::service_message(body).unwrap_or_else(|| e.to_string())),
        _ => to_rpc_error(e),
    }
}

/// How a whole store's member or invitation reads: an encrypted store's
/// member with no copy of its key yet waits for it; a plain store has none.
fn member_row(member: &MemberView, encrypted: bool) -> Option<ShareMember> {
    let status = if !member.is_active() {
        ShareMemberStatus::Invited
    } else if encrypted && !member.has_key {
        ShareMemberStatus::WaitingForKey
    } else {
        ShareMemberStatus::Active
    };
    // A role this build does not know is not shown as one it does.
    Some(ShareMember { email: member.email.clone(), role: MemberRole::parse(&member.role)?, status, name: member.name.clone().unwrap_or_default() })
}

impl RpcHandler {
    async fn account_signed_in(&self) -> Result<SignedInAccount, ErrorObjectOwned> {
        self.keystore.account().await.ok_or_else(|| to_rpc_error(NOT_SIGNED_IN))
    }

    /// [`account_refusal`], and a session the service no longer takes (a
    /// password changed on another device ends this one) marked as ended, as
    /// minting a token does (`crate::grant::mint`), so the account's status
    /// says to sign in again.
    fn refused(&self, e: CloudError) -> ErrorObjectOwned {
        if matches!(e, CloudError::Refused { refusal: Refusal::SignedOut, .. }) && !self.keystore.session_expired() {
            self.keystore.mark_session_expired();
            warn!("Pimble Cloud refused this computer's sign-in (it expired, or was signed out elsewhere): sign in again");
            self.cloud_changed();
        }
        account_refusal(e)
    }

    /// The current password proved here: it must open the wrapped keys the
    /// service holds (so a typo costs no request), and its `auth_key` is what
    /// the service checks again, as login does. Refused as
    /// [`Refusal::WrongCurrentPassword`].
    async fn prove_current_password(&self, account: &SignedInAccount, current_password: String) -> Result<String, ErrorObjectOwned> {
        let held = crate::cloud::me_keys(self.http(), &account.url, &account.session).await.map_err(|e| self.refused(e))?;
        if held.public_keys != account.keys.public_keys() {
            // The service's keys are not the ones this server holds: a
            // rewrap of these would lock the account out of its own.
            return Err(to_rpc_error("This browser holds other keys than your account's. Sign out and sign in again, then try again."));
        }
        let (current_kdf, current_blob) = (held.kdf, held.account_key_blob);
        let proved = self
            .runtime()
            .run_heavy(move || -> Result<_, String> {
                let current = pimble_crypto::derive_password_keys(&current_password, &current_kdf).map_err(|e| e.to_string())?;
                if pimble_crypto::unwrap_account_keys(&current_blob, &current.kek).is_err() {
                    return Ok(None);
                }
                Ok(Some(pimble_crypto::encode_auth_key(&current.auth_key)))
            })
            .await
            .map_err(to_rpc_error)?
            .map_err(to_rpc_error)?;
        proved.ok_or_else(|| to_rpc_error(Refusal::WrongCurrentPassword))
    }

    /// `cloudChangePassword`, proved by the current password
    /// ([`Self::prove_current_password`]).
    pub(super) async fn change_password(&self, current_password: String, new_password: String) -> Result<(), ErrorObjectOwned> {
        if new_password.chars().count() < MIN_PASSWORD {
            return Err(to_rpc_error(format!("A password needs at least {MIN_PASSWORD} characters.")));
        }
        let account = self.account_signed_in().await?;
        let current_auth_key = self.prove_current_password(&account, current_password).await?;
        info!("Changing the Pimble Cloud password");

        let keys = pimble_crypto::AccountKeys { encryption_secret: account.keys.encryption_secret, signing_secret: account.keys.signing_secret };
        let material = self
            .runtime()
            .run_heavy(move || pimble_crypto::new_password_material(&keys, &new_password).map_err(|e| e.to_string()))
            .await
            .map_err(to_rpc_error)?
            .map_err(to_rpc_error)?;

        crate::cloud::change_password(self.http(), &account.url, &account.session, &current_auth_key, &material.auth_key, &material.kdf, &material.account_key_blob)
            .await
            .map_err(|e| self.refused(e))?;
        info!("The Pimble Cloud password was changed; the account's other sessions ended with it");
        Ok(())
    }

    /// `cloudNewRecoveryCode`: the code made here, the keys wrapped under it,
    /// the old code's material replaced; the code answered once. Proved by
    /// the current password, here and again by the service, which mails the
    /// account that its code changed (Joe, 2026-11-25).
    ///
    /// When the request went out and no answer came back, the service may
    /// have taken the new code, which was never shown, and the old one then
    /// opens nothing: the answer says so ([`RECOVERY_CODE_OUTCOME_UNKNOWN`]).
    /// A refusal the service answered with wrote nothing and keeps its own
    /// sentence.
    pub(super) async fn new_recovery_code(&self, current_password: String) -> Result<String, ErrorObjectOwned> {
        if current_password.is_empty() {
            return Err(to_rpc_error("Enter your current password to make a new recovery code."));
        }
        let account = self.account_signed_in().await?;
        let current_auth_key = self.prove_current_password(&account, current_password).await?;
        info!("Making a new Pimble Cloud recovery code");
        let keys = pimble_crypto::AccountKeys { encryption_secret: account.keys.encryption_secret, signing_secret: account.keys.signing_secret };
        let mut material = self
            .runtime()
            .run_heavy(move || pimble_crypto::new_recovery_material(&keys).map_err(|e| e.to_string()))
            .await
            .map_err(to_rpc_error)?
            .map_err(to_rpc_error)?;
        crate::cloud::change_recovery_code(self.http(), &account.url, &account.session, &current_auth_key, &material.recovery_salt, &material.recovery_key_blob)
            .await
            .map_err(|e| {
                if crate::cloud::outcome_unknown(&e) {
                    warn!(error = %e, "The new recovery code's request got no answer; the old code may have stopped working");
                    to_rpc_error(RECOVERY_CODE_OUTCOME_UNKNOWN)
                } else {
                    self.refused(e)
                }
            })?;
        info!("The Pimble Cloud recovery code was replaced");
        Ok(std::mem::take(&mut material.code))
    }

    /// Whether `store_id` is encrypted, refusing unless the account owns the
    /// whole of it.
    async fn owned_store(&self, account: &SignedInAccount, store_id: StoreId) -> Result<bool, ErrorObjectOwned> {
        let rows = crate::cloud::list_stores(self.http(), &account.url, &account.session).await.map_err(to_rpc_error)?;
        if !crate::cloud::is_owner_of(&rows, store_id) {
            return Err(to_rpc_error(OWNERS_ONLY));
        }
        let id = store_id.to_string();
        Ok(!rows.iter().any(|row| row.store_id.eq_ignore_ascii_case(&id) && row.kind == hosted_plain::PLAIN_KIND))
    }

    async fn whole_store_answer(&self, account: &SignedInAccount, store_id: StoreId, encrypted: bool) -> Result<CloudStoreMembersResponse, ErrorObjectOwned> {
        let listing = crate::cloud::list_members(self.http(), &account.url, &account.session, &store_id.to_string(), None).await.map_err(refusal)?;
        let members = listing.members.iter().filter_map(|member| member_row(member, encrypted)).collect();
        Ok(CloudStoreMembersResponse { store_id, members })
    }

    /// `cloudStoreMembers`.
    pub(super) async fn store_members(&self, store_id: StoreId) -> Result<CloudStoreMembersResponse, ErrorObjectOwned> {
        let account = self.account_signed_in().await?;
        let encrypted = self.owned_store(&account, store_id).await?;
        self.whole_store_answer(&account, store_id, encrypted).await
    }

    /// `cloudStoreSetMember`. An encrypted store is held only by accounts it
    /// can seal its key to, so an address with no account is refused there
    /// before anything is granted, and a new member is handed the key at
    /// once (the old page's order: nobody holds a grant they cannot read).
    pub(super) async fn store_set_member(&self, store_id: StoreId, email: &str, role: MemberRole) -> Result<CloudStoreMembersResponse, ErrorObjectOwned> {
        let email = email.trim();
        if !email.contains('@') {
            return Err(to_rpc_error("Enter an email address."));
        }
        let account = self.account_signed_in().await?;
        let encrypted = self.owned_store(&account, store_id).await?;
        let store = store_id.to_string();
        // Only an address that holds nothing of the store yet is looked up:
        // a role change needs no lookup, and lookups are rate limited.
        let held = encrypted && {
            let listing = crate::cloud::list_members(self.http(), &account.url, &account.session, &store, None).await.map_err(refusal)?;
            listing.members.iter().any(|member| member.user_id.is_some() && member.email.eq_ignore_ascii_case(email))
        };
        if encrypted && !held {
            crate::cloud::lookup_user(self.http(), &account.url, &account.session, email).await.map_err(|e| match e.status() {
                Some(404) => to_rpc_error("No verified Pimble account has that address. An encrypted store is shared only with an account, whose key can be sealed to it."),
                _ => refusal(e),
            })?;
        }
        info!("Giving a member the role {} on store {}", role.as_str(), store_id);
        let member = crate::cloud::put_member(self.http(), &account.url, &account.session, &store, email, role.as_str(), None, None).await.map_err(refusal)?;
        if encrypted && member.is_active() && !member.has_key {
            self.hand_store_key(&account, store_id, &member).await.map_err(|e| {
                warn!("Store {}: a member was added but the store's key could not be sealed to them: {}", store_id, e);
                to_rpc_error(format!("{email} holds the store now, but its key could not be handed to them from here ({e}). Add them again to try once more."))
            })?;
        }
        self.whole_store_answer(&account, store_id, encrypted).await
    }

    /// Seal the store's current key to `member` and upload the envelope.
    async fn hand_store_key(&self, account: &SignedInAccount, store_id: StoreId, member: &MemberView) -> Result<(), String> {
        let (Some(user_id), Some(public_keys)) = (&member.user_id, &member.public_keys) else {
            return Err("the accounts service did not give their keys".to_string());
        };
        let key_id = crate::vault_link::fetch_scope_keys(self, account, store_id, &[])
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "this account holds no key for the store".to_string())?;
        let key = self.keystore.store_key(store_id, key_id).await.ok_or_else(|| "this account holds no key for the store".to_string())?;
        let envelope = pimble_crypto::wrap_key(&key, key_id, public_keys, &account.keys, &format!("store:{store_id}")).map_err(|e| e.to_string())?;
        crate::cloud::put_store_key(self.http(), &account.url, &account.session, &store_id.to_string(), user_id, key_id, &envelope, None).await.map_err(|e| e.to_string())
    }

    /// `cloudStoreRemoveMember`: a member's grant, or an address's
    /// invitation. The store's last owner stays (the accounts service
    /// refuses it).
    pub(super) async fn store_remove_member(&self, store_id: StoreId, email: &str) -> Result<CloudStoreMembersResponse, ErrorObjectOwned> {
        let email = email.trim();
        let account = self.account_signed_in().await?;
        let encrypted = self.owned_store(&account, store_id).await?;
        let store = store_id.to_string();
        let listing = crate::cloud::list_members(self.http(), &account.url, &account.session, &store, None).await.map_err(refusal)?;
        let member = listing
            .members
            .iter()
            .find(|member| member.email.eq_ignore_ascii_case(email))
            .ok_or_else(|| to_rpc_error(format!("{email} does not hold this store and has no invitation to it.")))?;
        info!("Removing a member of store {}", store_id);
        match &member.user_id {
            Some(user_id) => crate::cloud::delete_member(self.http(), &account.url, &account.session, &store, user_id, None).await,
            None => crate::cloud::delete_invitation(self.http(), &account.url, &account.session, &store, &member.email, None).await,
        }
        .map_err(refusal)?;
        self.whole_store_answer(&account, store_id, encrypted).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(status: &str, has_key: bool, role: &str) -> MemberView {
        MemberView { user_id: Some("u".into()), email: "m@example.com".into(), name: None, role: role.into(), status: status.into(), has_key, public_keys: None }
    }

    #[test]
    fn a_whole_stores_member_reads_by_its_kind() {
        assert_eq!(member_row(&view("active", false, "owner"), true).unwrap().status, ShareMemberStatus::WaitingForKey);
        assert_eq!(member_row(&view("active", false, "owner"), false).unwrap().status, ShareMemberStatus::Active);
        assert_eq!(member_row(&view("invited", false, "reader"), false).unwrap().status, ShareMemberStatus::Invited);
        assert_eq!(member_row(&view("active", true, "owner"), true).unwrap().role, MemberRole::Owner);
        assert!(member_row(&view("active", true, "emperor"), true).is_none());
    }

    #[test]
    fn the_last_owner_and_a_wrong_role_are_sentences() {
        let conflict = CloudError::Status { url: "u".into(), status: 409, body: r#"{"error":"conflict","message":"cannot remove the last owner of a store"}"#.into() };
        assert_eq!(refusal(conflict).message(), LAST_OWNER);
        let bad = CloudError::Status { url: "u".into(), status: 400, body: r#"{"error":"bad_request","message":"an owner must already have a verified Pimble account; invite them as an editor, commenter or reader"}"#.into() };
        assert!(refusal(bad).message().starts_with("An owner must already have"));
        let fast = CloudError::Status { url: "u".into(), status: 429, body: r#"{"error":"rate_limited","message":"too many lookups; slow down"}"#.into() };
        assert_eq!(refusal(fast).message(), Refusal::TooFast.to_string());
        let ended = CloudError::Status { url: "u".into(), status: 401, body: r#"{"error":"unauthorized","message":"invalid session"}"#.into() };
        assert_eq!(refusal(ended).message(), Refusal::SignedOut.to_string());
        let failed = CloudError::Status { url: "u".into(), status: 502, body: "Bad Gateway".into() };
        assert_eq!(refusal(failed).message(), Refusal::Unavailable.to_string());
    }

    #[test]
    fn a_refusal_on_the_accounts_own_material_is_a_sentence() {
        let wrong = crate::cloud::with_session(CloudError::Status {
            url: "u".into(),
            status: 401,
            body: r#"{"error":"wrong_current_password","message":"current password is incorrect"}"#.into(),
        });
        assert_eq!(account_refusal(wrong).message(), "That is not your current password.");
        let bad = CloudError::Status { url: "u".into(), status: 400, body: r#"{"error":"bad_request","message":"auth_key must not be empty"}"#.into() };
        assert_eq!(account_refusal(bad).message(), "Auth_key must not be empty.");
    }
}
