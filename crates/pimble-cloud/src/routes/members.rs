//! The members of a store and its shares: `GET/PUT /stores/{id}/members`,
//! `DELETE /stores/{id}/members/{user}`, `DELETE
//! /stores/{id}/invitations/{email}`, and claiming invitations at
//! verification and login (docs/NODE_DOCUMENT_CONTRACT.md section 5 for what
//! a share is; `super::stores`' module documentation for members, scopes and
//! share names).
//!
//! Every grant that is made, changed or removed here tells
//! `AppState::watch_interest` that the store's watchers' shares may have
//! changed (docs/NOTIFICATIONS_CONTRACT.md section 3): the hosted server
//! judges a watched write against the roots its watchers hold.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use pimble_crypto::AccountPublicKeys;

use super::stores::{
    is_store_owner, normalize_root, owner_count, require_live_store, require_owner, require_role_for_scope, root_view, stored_name, ScopeQuery, LAST_OWNER_ERROR,
    NO_SHARE_NAME, ROLES, UNNAMED_SHARE, WHOLE_STORE,
};
use crate::db::{GrantRow, HostedStoreRow, UserRow};
use crate::error::{CloudError, CloudResult};
use crate::mail::{invitation_email, shared_with_you_email};
use crate::session::AuthedUser;
use crate::state::AppState;
use tokio::sync::OwnedMutexGuard;

/// One row of `GET /stores/{id}/members`: either a member (a `Grant`,
/// `status: "active"`) or an address that has been invited and has no account
/// yet (an `Invitation`, `status: "invited"`).
#[derive(Serialize)]
pub struct MemberView {
    /// `null` for an invitation: there is no account to name yet.
    user_id: Option<String>,
    /// Answered to an owner only, `""` to everyone else: members of a share
    /// never see each other's emails (docs/PRESENCE_CONTRACT.md section 3,
    /// Joe 2026-10-22). An invitation is listed to owners alone anyway.
    email: String,
    /// What this member goes by in this scope; `null` for an invitation.
    name: Option<String>,
    role: String,
    /// The scope this membership is of: the shared node, or `null` for the
    /// whole store. Always the `root` the request asked about.
    root: Option<String>,
    /// `"active"` or `"invited"`. The desktop's third state ("waiting for the
    /// key") is not a status here — it is an active member with
    /// `has_key: false`, which is what the owner's key sweep looks for.
    status: &'static str,
    /// Whether a `KeyGrant` exists for this member on this store: whether
    /// they have been handed the store key yet. Always false for an
    /// invitation.
    has_key: bool,
    /// What an owner needs to wrap the store key to this member. Only ever
    /// filled in for an owner caller, and only for an active member — an
    /// invitation has no keys, and a non-owner member has no business
    /// collecting everyone else's ("invitations and `public_keys` only when
    /// the caller is an owner").
    public_keys: Option<AccountPublicKeys>,
    /// The member's notice key's public half, to every member alike: a
    /// public key, and what anyone in the share seals a notice with.
    #[serde(skip_serializing_if = "Option::is_none")]
    notice_key: Option<String>,
}

/// `GET /stores/{id}/members` — the scope's members, and the scope's own
/// name beside them, so an owner's Share dialog can show what this share is
/// called without asking anywhere else.
#[derive(Serialize)]
pub struct MembersResponse {
    members: Vec<MemberView>,
    /// The share's name, [`UNNAMED_SHARE`] when a scoped row carries none,
    /// and `null` for a whole-store listing (a store is named by `GET
    /// /stores`, and never named here to somebody who only holds a share).
    share_name: Option<String>,
    /// Everyone who reaches this scope, with the name they go by in it: the
    /// members of the scope and everyone holding the whole store (its
    /// owners above all). What a device turns the `sub` on a presence frame,
    /// a discussion message or a board element into a name with
    /// (docs/PRESENCE_CONTRACT.md section 3). No emails.
    people: Vec<PersonView>,
}

/// One account that reaches a scope, as anyone in it may see them.
#[derive(Serialize)]
pub struct PersonView {
    user_id: String,
    name: String,
    role: String,
    /// Their notice key's public half (docs/NOTIFICATIONS_CONTRACT.md
    /// section 2): what another member seals a notice to them with. Absent
    /// until they have one.
    #[serde(skip_serializing_if = "Option::is_none")]
    notice_key: Option<String>,
}

/// Everyone reaching `root`: the whole-store holders, then its own members
/// (one row per account; a whole-store grant wins).
async fn people_of_scope(state: &AppState, names: &Names, grants: &[GrantRow], root: &str) -> CloudResult<Vec<PersonView>> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let reaching = grants.iter().filter(|g| g.is_whole_store()).chain(grants.iter().filter(|g| !root.is_empty() && g.root == root));
    for grant in reaching {
        if !seen.insert(grant.user_rid) {
            continue;
        }
        if let Some(user) = state.db.get_user(grant.user_rid).await? {
            let notice_key = Some(user.notice_public_key.clone()).filter(|k| !k.is_empty());
            out.push(PersonView { name: names.name_of(&user, root), user_id: user.user_uuid, role: grant.role.clone(), notice_key });
        }
    }
    Ok(out)
}

/// The names chosen in one store's shares (docs/PRESENCE_CONTRACT.md section
/// 3), read once per request.
pub(crate) struct Names {
    chosen: std::collections::HashMap<(u64, String), String>,
}

impl Names {
    pub(crate) async fn of_store(state: &AppState, store_rid: u64) -> CloudResult<Self> {
        let chosen = state
            .db
            .share_names_for_store(store_rid)
            .await?
            .into_iter()
            .filter(|row| !row.name.trim().is_empty())
            .map(|row| ((row.user_rid, row.root), row.name))
            .collect();
        Ok(Self { chosen })
    }

    /// What `user` goes by in the scope `root`: the name they chose there,
    /// else their default name.
    pub(crate) fn name_of(&self, user: &UserRow, root: &str) -> String {
        self.chosen.get(&(user.rid, root.to_string())).cloned().unwrap_or_else(|| user.default_name())
    }

    /// The name `user` chose in `root` itself, `""` for none.
    pub(crate) fn chosen(&self, user_rid: u64, root: &str) -> String {
        self.chosen.get(&(user_rid, root.to_string())).cloned().unwrap_or_default()
    }
}

const STATUS_ACTIVE: &str = "active";
const STATUS_INVITED: &str = "invited";

/// Whether `user_rid` has been handed the key of this scope of `store_rid`
/// (any key id — a rotated key has several, and holding any of them means the
/// sweep has reached this member).
async fn has_key(state: &AppState, user_rid: u64, store_rid: u64, root: &str) -> CloudResult<bool> {
    Ok(!state.db.key_grants_for_user_store_and_root(user_rid, store_rid, root).await?.is_empty())
}

/// `GET /api/v1/stores/{id}/members[?root=<node id>]` — the members and
/// invitations of one scope. An owner sees any scope of their store; a scoped
/// member sees their own scope and nothing else (no whole-store listing, no
/// other share, no public keys).
pub async fn list_members(
    State(state): State<AppState>,
    authed: AuthedUser,
    Path(store_id): Path<String>,
    Query(scope): Query<ScopeQuery>,
) -> CloudResult<Json<MembersResponse>> {
    let store = require_live_store(&state, &store_id).await?;
    let root = normalize_root(&scope.root)?;
    require_role_for_scope(&state, &store, authed.user.rid, &root).await?;
    let caller_is_owner = is_store_owner(&state, &store, authed.user.rid).await?;
    let names = Names::of_store(&state, store.rid).await?;

    // Every grant and invitation of one scope carries the same name (see
    // [`rename_share`]), so the first one that has a name is the share's.
    let mut share_name = String::new();
    let grants = state.db.grants_for_store(store.rid).await?;
    let people = people_of_scope(&state, &names, &grants, &root).await?;
    let mut out = Vec::with_capacity(grants.len());
    for grant in grants.into_iter().filter(|g| g.root == root) {
        if share_name.is_empty() {
            share_name = grant.share_name.clone();
        }
        if let Some(user) = state.db.get_user(grant.user_rid).await? {
            let public_keys = if caller_is_owner { user.public_keys() } else { None };
            out.push(MemberView {
                name: Some(names.name_of(&user, &root)),
                user_id: Some(user.user_uuid),
                email: if caller_is_owner { user.email } else { String::new() },
                role: grant.role,
                root: root_view(&root),
                status: STATUS_ACTIVE,
                has_key: has_key(&state, grant.user_rid, store.rid, &root).await?,
                public_keys,
                notice_key: Some(user.notice_public_key.clone()).filter(|k| !k.is_empty()),
            });
        }
    }
    // Who has been asked but hasn't turned up yet is the owner's business
    // alone: a reader of a shared folder learns nothing about which addresses
    // its owner tried.
    if caller_is_owner {
        for invitation in state.db.invitations_for_store(store.rid).await?.into_iter().filter(|i| i.root == root) {
            if share_name.is_empty() {
                share_name = invitation.share_name.clone();
            }
            out.push(MemberView {
                name: None,
                user_id: None,
                email: invitation.email,
                role: invitation.role,
                root: root_view(&root),
                status: STATUS_INVITED,
                has_key: false,
                public_keys: None,
                notice_key: None,
            });
        }
    }
    // A whole-store listing has no share name at all; a scoped one always
    // answers with something, since the store's name is not a stand-in here.
    let share_name = if root.is_empty() {
        None
    } else if share_name.is_empty() {
        Some(UNNAMED_SHARE.to_string())
    } else {
        Some(share_name)
    };
    Ok(Json(MembersResponse { members: out, share_name, people }))
}

#[derive(Deserialize)]
pub struct PutMemberRequest {
    pub email: String,
    pub role: String,
    /// The node to share: this member gets `role` on it and the documents
    /// under it, and on nothing else in the store. Absent means the whole
    /// store, as every call meant before shares had scopes.
    #[serde(default)]
    pub root: Option<String>,
    /// What the owner calls this share — **required** with a `root` (Joe,
    /// 2026-09-21: a share has a name of its own, and it is all a recipient
    /// is ever told about where it lives). Stored on the grant or invitation
    /// and used in the mail. Meaningless without a `root`, where the store's
    /// own name is the name, and ignored there.
    #[serde(default)]
    pub name: Option<String>,
}

/// A store may hold so many members plus invitations and no more
/// (docs/SHARING_CONTRACT.md). Checked only before adding a new one: changing
/// an existing member's role, or re-inviting an address already invited,
/// never grows the store and is never refused by this.
async fn require_room_for_one_more(state: &AppState, store: &HostedStoreRow) -> CloudResult<()> {
    let members = state.db.grants_for_store(store.rid).await?.len();
    let invited = state.db.invitations_for_store(store.rid).await?.len();
    let cap = state.config.max_members_per_store;
    if members + invited >= cap {
        return Err(CloudError::Conflict(format!("this store already has {cap} members and invitations, the maximum")));
    }
    Ok(())
}

/// Sends one of the two sharing mails, at most one per (store, address) per
/// minute (docs/SHARING_CONTRACT.md). A repeat inside that minute is silent
/// but changes nothing else: the grant or invitation it accompanies is
/// already written.
///
/// A send that fails is logged, not returned: unlike signup (where a mail
/// failure means the account is unusable and 502 is the honest answer), the
/// membership this mail announces is already committed, and the owner's app
/// would show an error for a share that really was created. The recipient
/// finds it in their own store list regardless — the mail is a courtesy.
///
/// `share_name` is the name the owner typed for the share, and empty for a
/// whole-store membership — where the store's own name is what the mail says,
/// because that is what the recipient is being given.
async fn send_share_mail(state: &AppState, store: &HostedStoreRow, share_name: &str, inviter_email: &str, recipient_email: &str, invited: bool) {
    let key = format!("{}:{}", store.store_id, recipient_email.trim().to_lowercase());
    if !state.share_mail_rate_limit.try_acquire(&key) {
        tracing::info!(email = %recipient_email, store_id = %store.store_id, "a sharing mail went to this address for this store within the last minute; nothing sent");
        return;
    }
    // The mails one-line and cap whatever they are handed, so a name typed by
    // an owner is no more dangerous here than the store name beside it.
    let name = if share_name.is_empty() { store.name.as_str() } else { share_name };
    let base = state.config.public_url.trim_end_matches('/');
    // What the mail may promise depends on where the notes are kept.
    let kept = if store.is_relayed() { crate::mail::Kept::OnOwnersComputer } else { crate::mail::Kept::OnPimbleCloud };
    let (subject, text, html) = if invited {
        let email_param: String = url::form_urlencoded::byte_serialize(recipient_email.trim().as_bytes()).collect();
        invitation_email(inviter_email, name, &format!("{base}/app/signup?email={email_param}"), kept)
    } else {
        shared_with_you_email(inviter_email, name, &format!("{base}/app/"), kept)
    };
    if let Err(e) = state.mailer.send(recipient_email, &subject, &text, &html).await {
        tracing::warn!(email = %recipient_email, error = %e, "sending the sharing email failed; the membership it announces stands");
    }
}

pub async fn put_member(
    State(state): State<AppState>,
    authed: AuthedUser,
    Path(store_id): Path<String>,
    Json(req): Json<PutMemberRequest>,
) -> CloudResult<Json<MemberView>> {
    let store = require_live_store(&state, &store_id).await?;
    // Who is an owner, and how many there are, is read and written under the
    // store's lock (see `AppState::store_owner_locks`): the caller's own
    // ownership too, which another owner may be taking away at this moment.
    let owners = state.store_owner_locks.lock(&store.store_id).await;
    require_owner(&state, &store, authed.user.rid).await?;

    if !ROLES.contains(&req.role.as_str()) {
        return Err(CloudError::BadRequest(format!("role must be one of {ROLES:?}")));
    }
    let root = normalize_root(req.root.as_deref().unwrap_or(WHOLE_STORE))?;
    if !root.is_empty() && req.role == "owner" {
        // Owner is a role on a store — it can delete the thing and every
        // other owner's access to it. A share is a subtree of somebody
        // else's store; there is nothing there to own.
        return Err(CloudError::BadRequest("a share's members are editors, commenters or readers; owner is a role on the whole store".to_string()));
    }
    // A share is named by its owner and by nothing else: without a name there
    // is nothing to call it in the recipient's store list, and the store's
    // own name must never stand in. A whole-store membership is of the store,
    // which has a name already, so a `name` there is ignored.
    let share_name = if root.is_empty() {
        NO_SHARE_NAME.to_string()
    } else {
        stored_name(req.name.as_deref().unwrap_or_default(), "a share's name")?
    };
    let email = req.email.trim();
    if email.is_empty() || !email.contains('@') {
        return Err(CloudError::BadRequest("email is not valid".to_string()));
    }

    // "A verified account" for this purpose means one a key can actually be
    // wrapped to: verified, with key material. An unverified or legacy row is
    // invited instead, and the invitation becomes a grant the moment that
    // address really has an account (see `claim_invitations`).
    let target = state.db.find_user_by_email(email).await?.filter(|u| u.verified && u.has_key_material());

    match target {
        Some(target) => grant_member(&state, &store, owners, &authed.user, target, &req.role, &root, &share_name).await,
        None => invite_member(&state, &store, owners, &authed.user, email, &req.role, &root, &share_name).await,
    }
}

/// Gives every grant and every invitation of `(store, root)` the name the
/// owner just typed. A share has one name, so renaming it for one member
/// renames it for all of them — including an address invited before the
/// rename, which claims the new name along with its root. Nothing to do for
/// the whole store, which is named by its own `name`.
async fn rename_share(state: &AppState, store: &HostedStoreRow, root: &str, share_name: &str) -> CloudResult<()> {
    if root.is_empty() {
        return Ok(());
    }
    for grant in state.db.grants_for_store(store.rid).await? {
        if grant.root == root && grant.share_name != share_name {
            state.db.update_grant_share_name(grant.rid, share_name).await?;
        }
    }
    for invitation in state.db.invitations_for_store(store.rid).await? {
        if invitation.root == root && invitation.share_name != share_name {
            state.db.update_invitation_share_name(invitation.rid, share_name).await?;
        }
    }
    Ok(())
}

/// `PUT members` for an address that has a usable account: a grant on this
/// scope, plus the "shared with you" mail and the public keys the caller
/// needs to wrap the scope's key straight away. `owners` is the store's
/// owner lock, held until the grant is written and let go before the mail.
#[allow(clippy::too_many_arguments)]
async fn grant_member(
    state: &AppState,
    store: &HostedStoreRow,
    owners: OwnedMutexGuard<()>,
    caller: &UserRow,
    target: UserRow,
    role: &str,
    root: &str,
    share_name: &str,
) -> CloudResult<Json<MemberView>> {
    match state.db.find_grant(target.rid, store.rid, root).await? {
        Some(existing) => {
            // Demoting the store's last owner to editor/reader is exactly as
            // forbidden as removing them outright (`delete_member` below) —
            // both leave the store with zero owners. Only a whole-store grant
            // can be that owner. The count is read under the store's lock,
            // after `existing`, so two owners demoting each other at once
            // cannot both count two.
            if existing.role == "owner" && role != "owner" && owner_count(state, store.rid).await? <= 1 {
                return Err(CloudError::Conflict(LAST_OWNER_ERROR.to_string()));
            }
            state.db.update_grant_role(existing.rid, role).await?;
        }
        None => {
            require_room_for_one_more(state, store).await?;
            state.db.create_grant(target.rid, store.rid, &store.store_id, role, root, share_name).await?;
        }
    }
    // A watcher of the store holding it through one more share, or holding
    // it otherwise than before: the hosted server judges writes against
    // what every watcher holds now.
    state.watch_interest.changed([store.store_id.clone()]);
    // The name the owner typed is this share's name, for everybody who holds
    // it — the one they just added and everyone who was already there.
    rename_share(state, store, root, share_name).await?;
    // An invitation for an address that now holds a grant on this scope has
    // nothing left to do (this is the same tidy-up `claim_invitations` does;
    // an owner adding somebody who signed up in the meantime reaches it from
    // the other side).
    if let Some(invitation) = state.db.find_invitation(store.rid, &target.email, root).await? {
        state.db.delete_invitation(invitation.rid).await?;
    }
    drop(owners);

    send_share_mail(state, store, share_name, &caller.email, &target.email, false).await;
    let has_key = has_key(state, target.rid, store.rid, root).await?;
    let name = Names::of_store(state, store.rid).await?.name_of(&target, root);
    Ok(Json(MemberView {
        name: Some(name),
        user_id: Some(target.user_uuid.clone()),
        // The caller is an owner (`put_member` required it), and these are
        // exactly what they need to wrap the scope's key to the new member
        // without a second round trip.
        public_keys: target.public_keys(),
        notice_key: Some(target.notice_public_key.clone()).filter(|k| !k.is_empty()),
        email: target.email,
        role: role.to_string(),
        root: root_view(root),
        status: STATUS_ACTIVE,
        has_key,
    }))
}

/// `PUT members` for an address with no usable account yet: an invitation,
/// upserted so the same address is never invited twice to the same scope.
/// `owners` as in [`grant_member`].
#[allow(clippy::too_many_arguments)]
async fn invite_member(
    state: &AppState,
    store: &HostedStoreRow,
    owners: OwnedMutexGuard<()>,
    caller: &UserRow,
    email: &str,
    role: &str,
    root: &str,
    share_name: &str,
) -> CloudResult<Json<MemberView>> {
    if role == "owner" {
        // An owner can delete the store and every other owner's access to it;
        // handing that to an address nobody has proven belongs to a real
        // account is not something to do by typo. (A scoped `owner` is
        // refused earlier still, for everyone.)
        return Err(CloudError::BadRequest("an owner must already have a verified Pimble account; invite them as an editor, commenter or reader".to_string()));
    }
    match state.db.find_invitation(store.rid, email, root).await? {
        Some(existing) => {
            // Already invited: only the role can change, and neither the
            // store's cap nor the inviter's hourly quota is touched — nothing
            // new was created, and re-sending to somebody already invited is
            // not what those two limits are for.
            state.db.update_invitation_role(existing.rid, role).await?;
        }
        None => {
            require_room_for_one_more(state, store).await?;
            if !state.invite_quota.try_acquire(&caller.user_uuid) {
                return Err(CloudError::RateLimited("too many invitations in the last hour; try again later".to_string()));
            }
            state.db.create_invitation(store.rid, &store.store_id, email, role, root, share_name, caller.rid).await?;
        }
    }
    // As in `grant_member`: one name for the share, on every row of it.
    rename_share(state, store, root, share_name).await?;
    drop(owners);

    send_share_mail(state, store, share_name, &caller.email, email, true).await;
    Ok(Json(MemberView {
        name: None,
        user_id: None,
        email: email.to_string(),
        role: role.to_string(),
        root: root_view(root),
        status: STATUS_INVITED,
        has_key: false,
        public_keys: None,
        notice_key: None,
    }))
}

/// `DELETE /api/v1/stores/{id}/members/{user_id}[?root=<node id>]` — removes
/// one membership: of one share, or of the whole store. Removing somebody
/// from a share leaves any other share of theirs, and their whole-store grant
/// if they have one, exactly where it was.
pub async fn delete_member(
    State(state): State<AppState>,
    authed: AuthedUser,
    Path((store_id, user_id)): Path<(String, String)>,
    Query(scope): Query<ScopeQuery>,
) -> CloudResult<Json<Value>> {
    let store = require_live_store(&state, &store_id).await?;
    let root = normalize_root(&scope.root)?;
    // Everything below is read and written under the store's owner lock, so
    // the grant and the owner count are what is stored when it is deleted:
    // two owners removing each other (or one leaving while the other is
    // demoted) cannot both count two.
    let _owners = state.store_owner_locks.lock(&store.store_id).await;
    // Authorization before the lookup, so who exists is never leaked by which
    // error comes back: a caller with no grant is 403 whatever `user_id` they
    // name.
    require_role_for_scope(&state, &store, authed.user.rid, &root).await?;
    let caller_is_owner = is_store_owner(&state, &store, authed.user.rid).await?;

    let target = state
        .db
        .find_user_by_uuid(&user_id)
        .await?
        .ok_or_else(|| CloudError::NotFound("no such member".to_string()))?;
    // An owner may remove anyone; anyone may remove themself (leaving a share
    // is not something a recipient should have to ask for).
    if target.rid != authed.user.rid && !caller_is_owner {
        return Err(CloudError::Forbidden("only an owner can remove another member".to_string()));
    }
    let grant = state
        .db
        .find_grant(target.rid, store.rid, &root)
        .await?
        .ok_or_else(|| CloudError::NotFound("no such member".to_string()))?;

    if grant.role == "owner" && grant.is_whole_store() && owner_count(&state, store.rid).await? <= 1 {
        return Err(CloudError::Conflict(LAST_OWNER_ERROR.to_string()));
    }
    // The envelopes for this scope go with the grant: the service stops
    // handing this account that key. What they already decrypted is theirs —
    // a node leaving a share gets a fresh data key for what comes after
    // (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Keys").
    state.db.delete_key_grants_for_user_store_and_root(target.rid, store.rid, &root).await?;
    state.db.delete_grant(grant.rid).await?;
    // A watcher who held the store through this grant now holds it through
    // what is left, if anything: the hosted server is told again.
    state.watch_interest.changed([store.store_id.clone()]);
    Ok(Json(json!({})))
}

/// `DELETE /api/v1/stores/{id}/invitations/{email}[?root=<node id>]` —
/// withdraw an invitation to one scope. 200 whether or not there was one, so
/// an owner clicking "remove" twice, or removing somebody who signed up and
/// was claimed in between, is not an error.
pub async fn delete_invitation(
    State(state): State<AppState>,
    authed: AuthedUser,
    Path((store_id, email)): Path<(String, String)>,
    Query(scope): Query<ScopeQuery>,
) -> CloudResult<Json<Value>> {
    let store = require_live_store(&state, &store_id).await?;
    require_owner(&state, &store, authed.user.rid).await?;
    let root = normalize_root(&scope.root)?;

    if let Some(invitation) = state.db.find_invitation(store.rid, &email, &root).await? {
        state.db.delete_invitation(invitation.rid).await?;
    }
    Ok(Json(json!({})))
}

// ── Claiming ─────────────────────────────────────────────────────────────

/// Turns every invitation standing for `user`'s address into a grant on the
/// scope it named, and deletes it either way. Run when an address becomes
/// verified and at every login, which between them covers both orders the
/// world can happen in: the invitation arriving before the account, and the
/// account arriving first but unverified.
///
/// An existing grant on that same scope wins — an owner who invited an
/// address and then added the account directly (or changed its role
/// afterwards) must not have that role overwritten by the older invitation.
async fn claim_invitations(state: &AppState, user: &UserRow) -> CloudResult<usize> {
    let mut claimed = 0usize;
    for invitation in state.db.invitations_for_email(&user.email).await? {
        let store = state.db.get_hosted_store(invitation.store_rid).await?;
        match store {
            Some(store) if !store.deleted => {
                if state.db.find_grant(user.rid, store.rid, &invitation.root).await?.is_none() {
                    // The share's name travels with its root: what the
                    // recipient's store list calls this was typed by the
                    // owner when they invited the address.
                    state
                        .db
                        .create_grant(user.rid, store.rid, &store.store_id, &invitation.role, &invitation.root, &invitation.share_name)
                        .await?;
                    state.watch_interest.changed([store.store_id.clone()]);
                    claimed += 1;
                }
            }
            // The store is gone (or was deleted while the invitation stood):
            // there is nothing to grant, and the row would otherwise sit
            // there forever.
            _ => {}
        }
        state.db.delete_invitation(invitation.rid).await?;
    }
    Ok(claimed)
}

/// [`claim_invitations`], logged rather than propagated. Both its call sites
/// (verification and login) have already done the thing the caller asked for
/// by the time this runs, and neither should fail — or, worse, leave an
/// account verified but unable to log in — because a share could not be
/// handed over. The next login tries again.
pub(crate) async fn claim_invitations_best_effort(state: &AppState, user: &UserRow) {
    match claim_invitations(state, user).await {
        Ok(0) => {}
        Ok(claimed) => tracing::info!(email = %user.email, count = claimed, "claimed invitation(s) as grants"),
        Err(e) => tracing::warn!(email = %user.email, error = %e, "claiming this address's invitations failed; the next login will try again"),
    }
}
