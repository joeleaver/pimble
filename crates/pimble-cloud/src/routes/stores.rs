//! `GET/POST /stores`, `DELETE /stores/{id}`, the members sub-resource, the
//! invitations sub-resource, and the key-grants sub-resource
//! (docs/CRYPTO_CONTRACT.md, Phase 2a; docs/SHARING_CONTRACT.md, Phase 2b).
//!
//! Phase 2b turns "a member" into two things: a `Grant` (an account that can
//! reach the store now) and an `Invitation` (an address that will get one the
//! moment it has a verified account — see [`claim_invitations`], which every
//! verification and every login runs). `PUT members` picks between them by
//! whether the address already has a usable account, and says which it did in
//! the `status` field of its answer.

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use pimble_core::{StoreId, StoreKind};
use pimble_crypto::{AccountPublicKeys, CryptoError, KeyEnvelope};

use crate::db::{HostedStoreRow, UserRow};
use crate::error::{CloudError, CloudResult};
use crate::mail::{invitation_email, shared_with_you_email};
use crate::session::AuthedUser;
use crate::state::AppState;

/// `pimble_crypto::verify_envelope` needs no private key, so the accounts
/// service (which only ever holds public keys) can authenticate an envelope
/// it relays (docs/CRYPTO_CONTRACT.md, `PUT /stores/{id}/keys`). A bad
/// signature or a signer that doesn't match `expected_signer` are both
/// `CryptoError::BadSignature` — both are the caller presenting an envelope
/// they can't prove they made, so both map to 401.
fn verify_envelope_signature(envelope: &KeyEnvelope, expected_signer: &str) -> CloudResult<()> {
    pimble_crypto::verify_envelope(envelope, expected_signer).map_err(|e| match e {
        CryptoError::BadSignature => CloudError::Unauthorized("envelope signature does not verify".to_string()),
        other => CloudError::BadRequest(format!("envelope is invalid: {other}")),
    })
}

fn rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).unwrap_or_default().to_rfc3339()
}

const ROLES: [&str; 3] = ["owner", "editor", "reader"];

fn store_kind_str(kind: StoreKind) -> &'static str {
    match kind {
        StoreKind::Plain => "plain",
        StoreKind::Vault => "vault",
    }
}

fn to_json(field: &'static str, value: &impl Serialize) -> CloudResult<String> {
    serde_json::to_string(value).map_err(|e| CloudError::Internal(format!("serializing {field}: {e}")))
}

#[derive(Serialize)]
pub struct StoreView {
    store_id: String,
    name: String,
    role: String,
    kind: String,
    created_at: String,
    /// Phase 2b (docs/SHARING_CONTRACT.md): this vault store is a share
    /// mirror of somebody's node, not a store in its own right.
    share: bool,
    /// An owner's email, when the caller is not an owner — what the recipient
    /// of a share is shown ("shared by <email>"). `null` for one's own store.
    shared_by: Option<String>,
}

/// The `shared_by` of a store as `caller_rid` sees it: an owner's email when
/// they are not an owner themselves, `None` when they are. Costs a query per
/// store the caller doesn't own, which is what `GET /stores` pays; a store
/// with several owners shows the first one the grant listing yields (any one
/// of them answers "who shared this with me").
async fn shared_by(state: &AppState, store: &HostedStoreRow, caller_role: &str) -> CloudResult<Option<String>> {
    if caller_role == "owner" {
        return Ok(None);
    }
    for grant in state.db.grants_for_store(store.rid).await? {
        if grant.role == "owner" {
            if let Some(owner) = state.db.get_user(grant.user_rid).await? {
                return Ok(Some(owner.email));
            }
        }
    }
    Ok(None)
}

fn store_view(store: &HostedStoreRow, role: &str, shared_by: Option<String>) -> StoreView {
    StoreView {
        store_id: store.store_id.clone(),
        name: store.name.clone(),
        role: role.to_string(),
        kind: store.kind.clone(),
        created_at: rfc3339(store.created_at_ms),
        share: store.share,
        shared_by,
    }
}

/// Look a store up by its external id, refusing (as 404) one that doesn't
/// exist or has been soft-deleted — a deleted store is gone as far as every
/// endpoint in this file is concerned.
async fn require_live_store(state: &AppState, store_id: &str) -> CloudResult<HostedStoreRow> {
    let store = state
        .db
        .find_hosted_store(store_id)
        .await?
        .ok_or_else(|| CloudError::NotFound("no such store".to_string()))?;
    if store.deleted {
        return Err(CloudError::NotFound("no such store".to_string()));
    }
    Ok(store)
}

/// The caller's role on `store`, or `Forbidden` if they have none.
async fn require_any_grant(state: &AppState, store: &HostedStoreRow, user_rid: u64) -> CloudResult<String> {
    state
        .db
        .find_grant(user_rid, store.rid)
        .await?
        .map(|g| g.role)
        .ok_or_else(|| CloudError::Forbidden("not a member of this store".to_string()))
}

/// `Forbidden` unless the caller is an owner of `store`.
async fn require_owner(state: &AppState, store: &HostedStoreRow, user_rid: u64) -> CloudResult<()> {
    match require_any_grant(state, store, user_rid).await? {
        role if role == "owner" => Ok(()),
        _ => Err(CloudError::Forbidden("only an owner can do this".to_string())),
    }
}

/// A store must always have at least one owner. Both the endpoint that
/// removes a grant and the one that changes its role reach this: a store
/// cannot end up ownerless either by removing its last owner outright or by
/// demoting them to `editor`/`reader`.
const LAST_OWNER_ERROR: &str = "cannot remove the last owner of a store";

async fn owner_count(state: &AppState, store_rid: u64) -> CloudResult<usize> {
    Ok(state.db.grants_for_store(store_rid).await?.into_iter().filter(|g| g.role == "owner").count())
}

pub async fn list_stores(State(state): State<AppState>, authed: AuthedUser) -> CloudResult<Json<Vec<StoreView>>> {
    let grants = state.db.grants_for_user(authed.user.rid).await?;
    let mut out = Vec::with_capacity(grants.len());
    for grant in grants {
        if let Some(store) = state.db.get_hosted_store(grant.store_rid).await? {
            if !store.deleted {
                let shared_by = shared_by(&state, &store, &grant.role).await?;
                out.push(store_view(&store, &grant.role, shared_by));
            }
        }
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct CreateStoreRequest {
    pub name: String,
    /// `"plain"` (default) or `"vault"` (docs/CRYPTO_CONTRACT.md).
    #[serde(default)]
    pub kind: StoreKind,
    /// A chosen store id (a desktop app hosting an existing local store
    /// under its own id); refused server-side if that id is already open.
    #[serde(default)]
    pub store_id: Option<String>,
    /// Phase 2b (docs/SHARING_CONTRACT.md): this store is a share mirror.
    /// Only a vault store can be one — a share holds nothing but ciphertext.
    #[serde(default)]
    pub share: bool,
}

/// How long a store's (or a share's) name may be. There was no limit before
/// Phase 2b: a name is visible to this service, goes out in sharing mails,
/// and is shown in every member's app, so an unbounded one is somebody else's
/// problem by definition. Generous — no name anybody means is this long —
/// and eight times what a mail will show (`mail::MAIL_NAME_MAX_CHARS`).
const MAX_STORE_NAME_CHARS: usize = 200;

pub async fn create_store(State(state): State<AppState>, authed: AuthedUser, Json(req): Json<CreateStoreRequest>) -> CloudResult<Json<StoreView>> {
    // The same one-line rule the mails apply, applied once here so the stored
    // name is the name: no control characters (a stored `\r\n` would be a
    // header injection waiting for the next mail that quotes it), whitespace
    // collapsed, trimmed as before.
    let name = crate::mail::one_line(&req.name);
    if name.is_empty() {
        return Err(CloudError::BadRequest("name must not be empty".to_string()));
    }
    if name.chars().count() > MAX_STORE_NAME_CHARS {
        return Err(CloudError::BadRequest(format!("name must be at most {MAX_STORE_NAME_CHARS} characters")));
    }
    if req.share && req.kind != StoreKind::Vault {
        return Err(CloudError::BadRequest("a share must be an encrypted store (kind: \"vault\")".to_string()));
    }
    let store_id = req
        .store_id
        .as_deref()
        .map(StoreId::parse)
        .transpose()
        .map_err(|e| CloudError::BadRequest(format!("store_id is not a valid id: {e}")))?;

    let (created_store_id, dir_name) = state.pimble.create_store(&name, req.kind, store_id).await?;
    let store_id_str = created_store_id.as_uuid().to_string();
    let kind_str = store_kind_str(req.kind);
    let hosted = state.db.create_hosted_store(&store_id_str, &name, &dir_name, kind_str, req.share).await?;
    state.db.create_grant(authed.user.rid, hosted.rid, &store_id_str, "owner").await?;
    // The creator is the owner, so never "shared by" anyone.
    Ok(Json(store_view(&hosted, "owner", None)))
}

pub async fn delete_store(State(state): State<AppState>, authed: AuthedUser, Path(store_id): Path<String>) -> CloudResult<Json<Value>> {
    let store = require_live_store(&state, &store_id).await?;
    require_owner(&state, &store, authed.user.rid).await?;
    // Everything that names the store goes with it: nobody keeps a grant, an
    // envelope, or a standing invitation to a store that no longer exists
    // (docs/SHARING_CONTRACT.md, `DELETE /stores/{id}`).
    state.db.delete_grants_for_store(store.rid).await?;
    state.db.delete_key_grants_for_store(store.rid).await?;
    state.db.delete_invitations_for_store(store.rid).await?;
    state.db.mark_store_deleted(store.rid).await?;

    // A vault store's ciphertext is deleted from the hosted server too — a
    // share's whole point is that "stop sharing" leaves nothing behind. The
    // row is already marked deleted above, so a hosted server that is down,
    // out of date, or refuses must not fail this request: the store is
    // unreachable either way (no grant names it any more), and retrying the
    // delete would 404 on the row it just removed.
    if store.kind == store_kind_str(StoreKind::Vault) {
        match StoreId::parse(&store.store_id) {
            Ok(id) => {
                if let Err(e) = state.pimble.delete_vault_store(id).await {
                    tracing::warn!(store_id = %store.store_id, error = %e, "deleting the hosted vault store failed; its accounts row is deleted regardless");
                }
            }
            Err(e) => tracing::warn!(store_id = %store.store_id, error = %e, "stored store_id is not a valid id; skipping the hosted vault delete"),
        }
    }
    Ok(Json(json!({})))
}

/// One row of `GET /stores/{id}/members` (docs/SHARING_CONTRACT.md): either a
/// member (a `Grant`, `status: "active"`) or an address that has been invited
/// and has no account yet (an `Invitation`, `status: "invited"`).
#[derive(Serialize)]
pub struct MemberView {
    /// `null` for an invitation: there is no account to name yet.
    user_id: Option<String>,
    email: String,
    role: String,
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
    /// collecting everyone else's (docs/SHARING_CONTRACT.md: "Invitations and
    /// `public_keys` only when the caller is an owner").
    public_keys: Option<AccountPublicKeys>,
}

const STATUS_ACTIVE: &str = "active";
const STATUS_INVITED: &str = "invited";

/// Whether `user_rid` has been handed `store_rid`'s key (any key id — a
/// rotated store has several, and holding any of them means the sweep has
/// reached this member).
async fn has_key(state: &AppState, user_rid: u64, store_rid: u64) -> CloudResult<bool> {
    Ok(!state.db.key_grants_for_user_and_store(user_rid, store_rid).await?.is_empty())
}

pub async fn list_members(State(state): State<AppState>, authed: AuthedUser, Path(store_id): Path<String>) -> CloudResult<Json<Vec<MemberView>>> {
    let store = require_live_store(&state, &store_id).await?;
    let caller_role = require_any_grant(&state, &store, authed.user.rid).await?;
    let caller_is_owner = caller_role == "owner";

    let grants = state.db.grants_for_store(store.rid).await?;
    let mut out = Vec::with_capacity(grants.len());
    for grant in grants {
        if let Some(user) = state.db.get_user(grant.user_rid).await? {
            let public_keys = if caller_is_owner { user.public_keys() } else { None };
            out.push(MemberView {
                user_id: Some(user.user_uuid),
                email: user.email,
                role: grant.role,
                status: STATUS_ACTIVE,
                has_key: has_key(&state, grant.user_rid, store.rid).await?,
                public_keys,
            });
        }
    }
    // Who has been asked but hasn't turned up yet is the owner's business
    // alone: a reader of a shared folder learns nothing about which addresses
    // its owner tried.
    if caller_is_owner {
        for invitation in state.db.invitations_for_store(store.rid).await? {
            out.push(MemberView {
                user_id: None,
                email: invitation.email,
                role: invitation.role,
                status: STATUS_INVITED,
                has_key: false,
                public_keys: None,
            });
        }
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct PutMemberRequest {
    pub email: String,
    pub role: String,
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
async fn send_share_mail(state: &AppState, store: &HostedStoreRow, inviter_email: &str, recipient_email: &str, invited: bool) {
    let key = format!("{}:{}", store.store_id, recipient_email.trim().to_lowercase());
    if !state.share_mail_rate_limit.try_acquire(&key) {
        tracing::info!(email = %recipient_email, store_id = %store.store_id, "a sharing mail went to this address for this store within the last minute; nothing sent");
        return;
    }
    let base = state.config.public_url.trim_end_matches('/');
    let (subject, text, html) = if invited {
        let email_param: String = url::form_urlencoded::byte_serialize(recipient_email.trim().as_bytes()).collect();
        invitation_email(inviter_email, &store.name, &format!("{base}/app/signup?email={email_param}"))
    } else {
        shared_with_you_email(inviter_email, &store.name, &format!("{base}/app/"))
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
    require_owner(&state, &store, authed.user.rid).await?;

    if !ROLES.contains(&req.role.as_str()) {
        return Err(CloudError::BadRequest(format!("role must be one of {ROLES:?}")));
    }
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
        Some(target) => grant_member(&state, &store, &authed.user, target, &req.role).await,
        None => invite_member(&state, &store, &authed.user, email, &req.role).await,
    }
}

/// `PUT members` for an address that has a usable account: a grant, as before
/// Phase 2b, plus the "shared with you" mail and the public keys the caller
/// needs to wrap the store key straight away.
async fn grant_member(state: &AppState, store: &HostedStoreRow, caller: &UserRow, target: UserRow, role: &str) -> CloudResult<Json<MemberView>> {
    match state.db.find_grant(target.rid, store.rid).await? {
        Some(existing) => {
            // Demoting the store's last owner to editor/reader is exactly as
            // forbidden as removing them outright (`delete_member` below) —
            // both leave the store with zero owners.
            if existing.role == "owner" && role != "owner" && owner_count(state, store.rid).await? <= 1 {
                return Err(CloudError::Conflict(LAST_OWNER_ERROR.to_string()));
            }
            state.db.update_grant_role(existing.rid, role).await?;
        }
        None => {
            require_room_for_one_more(state, store).await?;
            state.db.create_grant(target.rid, store.rid, &store.store_id, role).await?;
        }
    }
    // An invitation for an address that now holds a grant has nothing left to
    // do (this is the same tidy-up `claim_invitations` does; an owner adding
    // somebody who signed up in the meantime reaches it from the other side).
    if let Some(invitation) = state.db.find_invitation(store.rid, &target.email).await? {
        state.db.delete_invitation(invitation.rid).await?;
    }

    send_share_mail(state, store, &caller.email, &target.email, false).await;
    let has_key = has_key(state, target.rid, store.rid).await?;
    Ok(Json(MemberView {
        user_id: Some(target.user_uuid.clone()),
        // The caller is an owner (`put_member` required it), and these are
        // exactly what they need to wrap the store key to the new member
        // without a second round trip.
        public_keys: target.public_keys(),
        email: target.email,
        role: role.to_string(),
        status: STATUS_ACTIVE,
        has_key,
    }))
}

/// `PUT members` for an address with no usable account yet: an invitation,
/// upserted so the same address is never invited twice to the same store.
async fn invite_member(state: &AppState, store: &HostedStoreRow, caller: &UserRow, email: &str, role: &str) -> CloudResult<Json<MemberView>> {
    if role == "owner" {
        // An owner can delete the store and every other owner's access to it;
        // handing that to an address nobody has proven belongs to a real
        // account is not something to do by typo (docs/SHARING_CONTRACT.md:
        // "Role `owner` by invitation is refused").
        return Err(CloudError::BadRequest("an owner must already have a verified Pimble account; invite them as an editor or reader".to_string()));
    }
    match state.db.find_invitation(store.rid, email).await? {
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
            state.db.create_invitation(store.rid, &store.store_id, email, role, caller.rid).await?;
        }
    }

    send_share_mail(state, store, &caller.email, email, true).await;
    Ok(Json(MemberView {
        user_id: None,
        email: email.to_string(),
        role: role.to_string(),
        status: STATUS_INVITED,
        has_key: false,
        public_keys: None,
    }))
}

pub async fn delete_member(
    State(state): State<AppState>,
    authed: AuthedUser,
    Path((store_id, user_id)): Path<(String, String)>,
) -> CloudResult<Json<Value>> {
    let store = require_live_store(&state, &store_id).await?;
    // Authorization before the lookup, so who exists is never leaked by which
    // error comes back: a caller with no grant is 403 whatever `user_id` they
    // name.
    let caller_role = require_any_grant(&state, &store, authed.user.rid).await?;

    let target = state
        .db
        .find_user_by_uuid(&user_id)
        .await?
        .ok_or_else(|| CloudError::NotFound("no such member".to_string()))?;
    // An owner may remove anyone; anyone may remove themself (leaving a share
    // is not something a recipient should have to ask for).
    if target.rid != authed.user.rid && caller_role != "owner" {
        return Err(CloudError::Forbidden("only an owner can remove another member".to_string()));
    }
    let grant = state
        .db
        .find_grant(target.rid, store.rid)
        .await?
        .ok_or_else(|| CloudError::NotFound("no such member".to_string()))?;

    if grant.role == "owner" && owner_count(&state, store.rid).await? <= 1 {
        return Err(CloudError::Conflict(LAST_OWNER_ERROR.to_string()));
    }
    // The envelopes go with the grant: the service stops handing this account
    // the store key. What they already decrypted is theirs — key rotation is
    // Cut 2 (docs/SHARING_CONTRACT.md, "Known limits of this cut").
    state.db.delete_key_grants_for_user_and_store(target.rid, store.rid).await?;
    state.db.delete_grant(grant.rid).await?;
    Ok(Json(json!({})))
}

/// `DELETE /api/v1/stores/{id}/invitations/{email}` — withdraw an invitation
/// (docs/SHARING_CONTRACT.md). 200 whether or not there was one, so an owner
/// clicking "remove" twice, or removing somebody who signed up and was
/// claimed in between, is not an error.
pub async fn delete_invitation(
    State(state): State<AppState>,
    authed: AuthedUser,
    Path((store_id, email)): Path<(String, String)>,
) -> CloudResult<Json<Value>> {
    let store = require_live_store(&state, &store_id).await?;
    require_owner(&state, &store, authed.user.rid).await?;

    if let Some(invitation) = state.db.find_invitation(store.rid, &email).await? {
        state.db.delete_invitation(invitation.rid).await?;
    }
    Ok(Json(json!({})))
}

// ── Claiming (docs/SHARING_CONTRACT.md: "Claiming") ──────────────────────

/// Turns every invitation standing for `user`'s address into a grant, and
/// deletes it either way. Run when an address becomes verified and at every
/// login, which between them covers both orders the world can happen in: the
/// invitation arriving before the account, and the account arriving first but
/// unverified.
///
/// An existing grant wins — an owner who invited an address and then added
/// the account directly (or changed its role afterwards) must not have that
/// role overwritten by the older invitation.
async fn claim_invitations(state: &AppState, user: &UserRow) -> CloudResult<usize> {
    let mut claimed = 0usize;
    for invitation in state.db.invitations_for_email(&user.email).await? {
        let store = state.db.get_hosted_store(invitation.store_rid).await?;
        match store {
            Some(store) if !store.deleted => {
                if state.db.find_grant(user.rid, store.rid).await?.is_none() {
                    state.db.create_grant(user.rid, store.rid, &store.store_id, &invitation.role).await?;
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

// ── Key grants (docs/CRYPTO_CONTRACT.md "Accounts service endpoints") ────

#[derive(Serialize)]
pub struct KeyGrantView {
    key_id: String,
    envelope: KeyEnvelope,
}

/// One account whose signature on an envelope for this store is to be
/// believed (docs/SHARING_CONTRACT.md: "`signers`, the store's owners"). A
/// recipient is handed the store key by whichever of the owner's devices
/// sweeps first, so "the envelope is signed by me" is not enough to check
/// against any more — it has to be "signed by me or by an owner of this
/// store".
#[derive(Serialize)]
pub struct SignerView {
    user_id: String,
    email: String,
    public_signing_key: String,
}

#[derive(Serialize)]
pub struct KeyGrantsResponse {
    envelopes: Vec<KeyGrantView>,
    signers: Vec<SignerView>,
}

/// `GET /api/v1/stores/{id}/keys` — the caller's own envelopes for this
/// store, never another member's (any grant may read their own), plus the
/// owners whose signatures on them are legitimate.
pub async fn get_store_keys(State(state): State<AppState>, authed: AuthedUser, Path(store_id): Path<String>) -> CloudResult<Json<KeyGrantsResponse>> {
    let store = require_live_store(&state, &store_id).await?;
    require_any_grant(&state, &store, authed.user.rid).await?;

    let grants = state.db.key_grants_for_user_and_store(authed.user.rid, store.rid).await?;
    let mut envelopes = Vec::with_capacity(grants.len());
    for grant in grants {
        let envelope: KeyEnvelope =
            serde_json::from_str(&grant.envelope).map_err(|e| CloudError::Internal(format!("stored envelope is not valid JSON: {e}")))?;
        envelopes.push(KeyGrantView { key_id: grant.key_id, envelope });
    }

    let mut signers = Vec::new();
    for grant in state.db.grants_for_store(store.rid).await? {
        if grant.role != "owner" {
            continue;
        }
        if let Some(owner) = state.db.get_user(grant.user_rid).await? {
            // A public signing key is all a verifier needs; an owner without
            // one (a legacy row that has not been cleaned up yet) simply
            // cannot have signed anything, so leaving them out is right.
            if let Some(public_signing_key) = owner.public_signing_key.clone() {
                signers.push(SignerView { user_id: owner.user_uuid, email: owner.email, public_signing_key });
            }
        }
    }
    Ok(Json(KeyGrantsResponse { envelopes, signers }))
}

#[derive(Deserialize)]
pub struct EnvelopeUpsert {
    pub user_id: String,
    pub key_id: String,
    pub envelope: KeyEnvelope,
}

#[derive(Deserialize)]
pub struct PutStoreKeysRequest {
    pub envelopes: Vec<EnvelopeUpsert>,
}

/// `PUT /api/v1/stores/{id}/keys` — upserts one or more (user, key id)
/// envelopes. Ownership rule (docs/CRYPTO_CONTRACT.md): an owner or editor
/// may always set their OWN envelopes; only an owner may set another
/// member's. Every envelope must be signed by the CALLER (whoever is
/// distributing the key), verified against the caller's own
/// `public_signing_key` — see `crate::envelope` for the caveat that this
/// verification is a stand-in for `pimble_crypto::unwrap_key`'s sibling,
/// not yet implemented.
pub async fn put_store_keys(
    State(state): State<AppState>,
    authed: AuthedUser,
    Path(store_id): Path<String>,
    Json(req): Json<PutStoreKeysRequest>,
) -> CloudResult<Json<Value>> {
    let store = require_live_store(&state, &store_id).await?;
    let caller_role = require_any_grant(&state, &store, authed.user.rid).await?;
    if caller_role != "owner" && caller_role != "editor" {
        return Err(CloudError::Forbidden("only an owner or editor can set store keys".to_string()));
    }
    // A session only ever exists for a real Phase 2a account now (`login`
    // refuses a legacy no-key-material one outright), so this is always
    // `Some` in practice; treated as `Internal`, not a panic, if it somehow
    // isn't.
    let caller_signing_key = authed
        .user
        .public_signing_key
        .as_deref()
        .ok_or_else(|| CloudError::Internal("session exists for a user with no key material".to_string()))?;

    for item in &req.envelopes {
        if item.user_id != authed.user.user_uuid && caller_role != "owner" {
            return Err(CloudError::Forbidden("only an owner can set another member's keys".to_string()));
        }
        verify_envelope_signature(&item.envelope, caller_signing_key)?;

        let target = state
            .db
            .find_user_by_uuid(&item.user_id)
            .await?
            .ok_or_else(|| CloudError::NotFound(format!("no such user: {}", item.user_id)))?;
        state
            .db
            .find_grant(target.rid, store.rid)
            .await?
            .ok_or_else(|| CloudError::BadRequest(format!("{} is not a member of this store", item.user_id)))?;

        let envelope_json = to_json("envelope", &item.envelope)?;
        match state.db.find_key_grant(target.rid, store.rid, &item.key_id).await? {
            Some(existing) => state.db.update_key_grant_envelope(existing.rid, &envelope_json).await?,
            None => {
                state.db.create_key_grant(target.rid, store.rid, &store.store_id, &item.key_id, &envelope_json).await?;
            }
        }
    }
    Ok(Json(json!({})))
}
