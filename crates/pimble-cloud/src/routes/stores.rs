//! `GET/POST /stores`, `DELETE /stores/{id}`, the members sub-resource, the
//! invitations sub-resource, and the key-grants sub-resource
//! (docs/CRYPTO_CONTRACT.md, Phase 2a; docs/NODE_DOCUMENT_CONTRACT.md section
//! 5 for what a share is).
//!
//! "A member" is two things: a `Grant` (an account that can reach the store
//! now) and an `Invitation` (an address that will get one the moment it has a
//! verified account — see [`claim_invitations`], which every verification and
//! every login runs). `PUT members` picks between them by whether the address
//! already has a usable account, and says which it did in the `status` field
//! of its answer.
//!
//! Every one of those carries a **scope**: the empty string for the whole
//! store, a node id for a share of the subtree under it. A share is not a
//! store of its own any more (the cut that made one is gone, along with
//! `HostedStore.share`); it is a grant on the owner's own hosted store,
//! rooted at a node. So every endpoint below takes a `root` — a `root=` query
//! parameter where there is no body, a `root` field where there is — and
//! absent means the whole store, exactly what every call meant before shares
//! had scopes.
//!
//! A share also has a **name of its own** (Joe, 2026-09-21), typed by the
//! owner and required by `PUT members` whenever there is a `root`. It is what
//! a recipient's store list and the sharing mails call the share, so the name
//! of the store it sits in never reaches them; it is stored on every grant
//! and invitation of that `(store, root)`, so renaming a share renames it for
//! everyone who holds it.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use pimble_core::{NodeId, StoreId, StoreKind};
use pimble_crypto::{AccountPublicKeys, CryptoError, KeyEnvelope};

use crate::db::{GrantRow, HostedStoreRow, UserRow, TIER_HOSTED, TIER_RELAY};
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

/// The scope that is the whole store rather than one shared subtree.
const WHOLE_STORE: &str = "";

/// The share name a whole-store grant or invitation carries: none. A store is
/// named by its own `name`.
const NO_SHARE_NAME: &str = "";

/// What a share with no name of its own is called. A share written before
/// shares had names (or by anything that somehow skipped `PUT members`) still
/// must not be shown to a recipient as the store it lives in: the owner's
/// store name is not theirs to see (Joe, 2026-09-21).
const UNNAMED_SHARE: &str = "Shared folder";

/// A `?root=<node id>` on an endpoint that has no body to carry one. Absent
/// (or empty) is the whole store.
#[derive(Deserialize)]
pub struct ScopeQuery {
    #[serde(default)]
    root: String,
}

/// A scope as this service stores and compares it: `""` for the whole store,
/// or a node id in exactly one spelling. A root is an id, so it is parsed and
/// re-rendered rather than trusted as typed — otherwise the same node in two
/// spellings (uppercase hex, braces) would be two different shares that never
/// find each other's grants, envelopes or invitations.
fn normalize_root(raw: &str) -> CloudResult<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(WHOLE_STORE.to_string());
    }
    let node_id = NodeId::parse(raw).map_err(|e| CloudError::BadRequest(format!("root is not a valid node id: {e}")))?;
    Ok(node_id.to_string())
}

/// `root` as an API value: `null` for the whole store, the node id for a
/// share. Every response says the scope this way, so a client never has to
/// tell "the whole store" from "a share of a node called empty string".
fn root_view(root: &str) -> Option<String> {
    if root.is_empty() {
        None
    } else {
        Some(root.to_string())
    }
}

fn store_kind_str(kind: StoreKind) -> &'static str {
    match kind {
        StoreKind::Plain => "plain",
        StoreKind::Vault => "vault",
    }
}

fn to_json(field: &'static str, value: &impl Serialize) -> CloudResult<String> {
    serde_json::to_string(value).map_err(|e| CloudError::Internal(format!("serializing {field}: {e}")))
}

/// One row of `GET /stores`: one per grant, so a store somebody holds two
/// shares of is two rows, and a store they hold outright is one.
#[derive(Serialize)]
pub struct StoreView {
    store_id: String,
    /// The store's name for a whole-store grant, and the **share's** name for
    /// a scoped one — never the store's, which a recipient has no business
    /// learning (Joe, 2026-09-21: "a share has a name of its own").
    name: String,
    role: String,
    kind: String,
    /// `"hosted"` or `"relay"` (docs/RELAY_CONTRACT.md): whether the store's
    /// encrypted twin is on Pimble Cloud, or the store is served by its
    /// owner's machine through the relay and reached at its own `rpc_url`
    /// (`POST /token`'s `stores`).
    tier: String,
    created_at: String,
    /// The node this row is rooted at, for a share; `null` for a whole-store
    /// grant (docs/NODE_DOCUMENT_CONTRACT.md section 5).
    root: Option<String>,
    /// An owner's email, when the caller is not an owner — what the recipient
    /// of a share is shown ("shared by <email>"). `null` for one's own store.
    shared_by: Option<String>,
}

/// The `shared_by` of a row as its holder sees it: an owner's email unless
/// this grant is their own whole-store ownership, `None` when it is. Costs a
/// query per store the caller doesn't own outright, which is what `GET
/// /stores` pays; a store with several owners shows the first one the grant
/// listing yields (any one of them answers "who shared this with me").
async fn shared_by(state: &AppState, store: &HostedStoreRow, grant: &GrantRow) -> CloudResult<Option<String>> {
    if grant.role == "owner" && grant.is_whole_store() {
        return Ok(None);
    }
    for owner in state.db.grants_for_store(store.rid).await? {
        if owner.role == "owner" && owner.is_whole_store() {
            if let Some(owner) = state.db.get_user(owner.user_rid).await? {
                return Ok(Some(owner.email));
            }
        }
    }
    Ok(None)
}

/// What a grant's row is called: the store's name when the grant is of the
/// whole store, the share's own name when it is of one node, and
/// [`UNNAMED_SHARE`] when a scoped grant somehow carries none — a recipient
/// is never shown the name of the store their share sits in.
fn row_name(store: &HostedStoreRow, grant: &GrantRow) -> String {
    if grant.is_whole_store() {
        store.name.clone()
    } else if grant.share_name.is_empty() {
        UNNAMED_SHARE.to_string()
    } else {
        grant.share_name.clone()
    }
}

fn store_view(store: &HostedStoreRow, grant: &GrantRow, shared_by: Option<String>) -> StoreView {
    StoreView {
        store_id: store.store_id.clone(),
        name: row_name(store, grant),
        role: grant.role.clone(),
        kind: store.kind.clone(),
        tier: store.tier.clone(),
        created_at: rfc3339(store.created_at_ms),
        root: root_view(&grant.root),
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

/// The caller's role on `root` — a whole-store grant covers every scope in
/// the store, a scoped grant covers exactly its own node — or `Forbidden`
/// when they hold nothing that reaches it. Asking about the whole store
/// (`root` empty) takes a whole-store grant: a member of one share must not
/// learn about the rest of the store through an endpoint that answers for all
/// of it.
async fn require_role_for_scope(state: &AppState, store: &HostedStoreRow, user_rid: u64, root: &str) -> CloudResult<String> {
    let grants = state.db.grants_for_user_and_store(user_rid, store.rid).await?;
    if let Some(whole) = grants.iter().find(|g| g.is_whole_store()) {
        return Ok(whole.role.clone());
    }
    grants
        .into_iter()
        .find(|g| !root.is_empty() && g.root == root)
        .map(|g| g.role)
        .ok_or_else(|| CloudError::Forbidden("not a member of this store".to_string()))
}

/// Whether the caller owns the whole store — the only kind of owner there is
/// (`put_member` refuses `owner` for a scope), and what every "only an owner
/// can do this" check below means.
async fn is_store_owner(state: &AppState, store: &HostedStoreRow, user_rid: u64) -> CloudResult<bool> {
    Ok(state.db.find_grant(user_rid, store.rid, WHOLE_STORE).await?.is_some_and(|g| g.role == "owner"))
}

/// `Forbidden` unless the caller is an owner of `store`.
async fn require_owner(state: &AppState, store: &HostedStoreRow, user_rid: u64) -> CloudResult<()> {
    if is_store_owner(state, store, user_rid).await? {
        Ok(())
    } else {
        Err(CloudError::Forbidden("only an owner can do this".to_string()))
    }
}

/// A store must always have at least one owner. Both the endpoint that
/// removes a grant and the one that changes its role reach this: a store
/// cannot end up ownerless either by removing its last owner outright or by
/// demoting them to `editor`/`reader`. It is about whole-store owners only —
/// a share has no owner to be the last of.
const LAST_OWNER_ERROR: &str = "cannot remove the last owner of a store";

async fn owner_count(state: &AppState, store_rid: u64) -> CloudResult<usize> {
    Ok(state.db.grants_for_store(store_rid).await?.into_iter().filter(|g| g.role == "owner" && g.is_whole_store()).count())
}

/// One row per grant: a store the caller holds outright is one row with no
/// `root`; a store they hold two shares of is two rows, each naming its node
/// and who shared it.
pub async fn list_stores(State(state): State<AppState>, authed: AuthedUser) -> CloudResult<Json<Vec<StoreView>>> {
    let grants = state.db.grants_for_user(authed.user.rid).await?;
    let mut out = Vec::with_capacity(grants.len());
    for grant in grants {
        if let Some(store) = state.db.get_hosted_store(grant.store_rid).await? {
            if !store.deleted {
                let shared_by = shared_by(&state, &store, &grant).await?;
                out.push(store_view(&store, &grant, shared_by));
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
    /// Gone: a share was briefly a vault store of its own, and is now a
    /// scoped grant on the owner's own store (docs/NODE_DOCUMENT_CONTRACT.md
    /// section 5). Read only so a client still sending it is told, rather
    /// than quietly creating a store nothing will ever share — and because
    /// "nothing is hosted unless the person asked for it" (`CLAUDE.md`)
    /// makes a store created as a side effect of sharing exactly the wrong
    /// thing to do silently.
    #[serde(default)]
    pub share: Option<bool>,
    /// `"hosted"` (default) or `"relay"` (docs/RELAY_CONTRACT.md). A relayed
    /// store is recorded here — so grants, invitations and key envelopes have
    /// something to hang off — and created nowhere: the hosted Pimble server
    /// is never told about it.
    #[serde(default)]
    pub tier: StoreTier,
}

/// Where a store's encrypted twin lives (docs/RELAY_CONTRACT.md).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StoreTier {
    /// On the hosted Pimble server: the person asked for it to be hosted.
    #[default]
    Hosted,
    /// On the owner's own machine, reached through the relay: nothing of the
    /// store is on Pimble Cloud.
    Relay,
}

impl StoreTier {
    fn as_str(self) -> &'static str {
        match self {
            StoreTier::Hosted => TIER_HOSTED,
            StoreTier::Relay => TIER_RELAY,
        }
    }
}

/// How long a store's (or a share's) name may be. A name is visible to this
/// service, goes out in sharing mails, and is shown in every member's app, so
/// an unbounded one is somebody else's problem by definition. Generous — no
/// name anybody means is this long — and eight times what a mail will show
/// (`mail::MAIL_NAME_MAX_CHARS`).
const MAX_STORE_NAME_CHARS: usize = 200;

/// A name as this service stores it: the same one-line rule the mails apply
/// (no control characters — a stored `\r\n` would be a header injection
/// waiting for the next mail that quotes it — whitespace collapsed, trimmed),
/// then refused if it is empty or longer than [`MAX_STORE_NAME_CHARS`].
/// `what` names the field in the error, since both a store's name and a
/// share's come through here.
fn stored_name(raw: &str, what: &str) -> CloudResult<String> {
    let name = stored_name_or_empty(raw, what)?;
    if name.is_empty() {
        return Err(CloudError::BadRequest(format!("{what} must not be empty")));
    }
    Ok(name)
}

/// [`stored_name`] without the refusal of an empty one: what a relayed
/// store's name goes through. The owner's device sends the empty string for
/// it (docs/RELAY_CONTRACT.md: "nothing on Pimble Cloud needs the owner's
/// name for it"), and that is stored as it came; a name that is sent is still
/// kept to one line and bounded, for the same reasons as any other.
fn stored_name_or_empty(raw: &str, what: &str) -> CloudResult<String> {
    let name = crate::mail::one_line(raw);
    if name.chars().count() > MAX_STORE_NAME_CHARS {
        return Err(CloudError::BadRequest(format!("{what} must be at most {MAX_STORE_NAME_CHARS} characters")));
    }
    Ok(name)
}

pub async fn create_store(State(state): State<AppState>, authed: AuthedUser, Json(req): Json<CreateStoreRequest>) -> CloudResult<Json<StoreView>> {
    // Before anything is created: a caller still asking for a share store is
    // asking for something that no longer exists, and must hear so rather
    // than get a store they never meant to host.
    if req.share.is_some() {
        return Err(CloudError::BadRequest(
            "`share` is gone: a share is a scoped grant on your own store now — create the store, then PUT a member with the node's `root`".to_string(),
        ));
    }
    let store_id = req
        .store_id
        .as_deref()
        .map(StoreId::parse)
        .transpose()
        .map_err(|e| CloudError::BadRequest(format!("store_id is not a valid id: {e}")))?;
    let kind_str = store_kind_str(req.kind);

    // A chosen id may already have a row. A live one is somebody's store and
    // is refused. A deleted one is what "stop hosting" and "stop sharing from
    // this computer" leave behind (`DELETE /stores/{id}` only marks the row,
    // and `store_id` is `@unique`), and the same store may be hosted or
    // relayed again — in either tier, whichever it was before — so that row is
    // taken over below rather than left to fail the insert for ever.
    let previous = match store_id {
        Some(id) => state.db.find_hosted_store(&id.as_uuid().to_string()).await?,
        None => None,
    };
    if previous.as_ref().is_some_and(|row| !row.deleted) {
        return Err(CloudError::Conflict("a store with this id already exists".to_string()));
    }

    let (store_id_str, name, dir_name) = match req.tier {
        StoreTier::Hosted => {
            let name = stored_name(&req.name, "name")?;
            let (created_store_id, dir_name) = state.pimble.create_store(&name, req.kind, store_id).await?;
            (created_store_id.as_uuid().to_string(), name, dir_name)
        }
        // A relayed store is recorded and nothing else (docs/
        // RELAY_CONTRACT.md): the hosted Pimble server is never called, so
        // nothing of the store — not a directory, not its name — reaches
        // Pimble Cloud's disk by this request. The store already exists on
        // its owner's machine, so the id is theirs to say; and what members
        // reach through the relay is the vault twin that machine serves, so
        // no other kind can be relayed.
        StoreTier::Relay => {
            let store_id = store_id.ok_or_else(|| {
                CloudError::BadRequest("a relayed store is recorded under its own id: `store_id` is required with `tier: \"relay\"`".to_string())
            })?;
            if req.kind != StoreKind::Vault {
                return Err(CloudError::BadRequest(
                    "a relayed store is served as an encrypted vault: `kind` must be \"vault\" with `tier: \"relay\"`".to_string(),
                ));
            }
            (store_id.as_uuid().to_string(), stored_name_or_empty(&req.name, "name")?, String::new())
        }
    };
    let hosted = match previous {
        Some(deleted_row) => {
            // Nothing that named the old store may come back to life with the
            // id. `DELETE /stores/{id}` already removed all of it; an account
            // deletion (`recover_delete_account`) removes only that account's
            // own rows, so another member's grant can outlive the store.
            state.db.delete_grants_for_store(deleted_row.rid).await?;
            state.db.delete_key_grants_for_store(deleted_row.rid).await?;
            state.db.delete_invitations_for_store(deleted_row.rid).await?;
            state.db.revive_hosted_store(deleted_row.rid, &name, &dir_name, kind_str, req.tier.as_str()).await?
        }
        None => state.db.create_hosted_store(&store_id_str, &name, &dir_name, kind_str, req.tier.as_str()).await?,
    };
    // The whole store, so no share name: this row is named by the store.
    let grant = state.db.create_grant(authed.user.rid, hosted.rid, &store_id_str, "owner", WHOLE_STORE, NO_SHARE_NAME).await?;
    // The creator owns the whole store, so never "shared by" anyone.
    Ok(Json(store_view(&hosted, &grant, None)))
}

pub async fn delete_store(State(state): State<AppState>, authed: AuthedUser, Path(store_id): Path<String>) -> CloudResult<Json<Value>> {
    let store = require_live_store(&state, &store_id).await?;
    require_owner(&state, &store, authed.user.rid).await?;
    // Everything that names the store goes with it: nobody keeps a grant, an
    // envelope, or a standing invitation to a store that no longer exists
    // (docs/NODE_DOCUMENT_CONTRACT.md section 7, `DELETE /stores/{id}`).
    state.db.delete_grants_for_store(store.rid).await?;
    state.db.delete_key_grants_for_store(store.rid).await?;
    state.db.delete_invitations_for_store(store.rid).await?;
    state.db.mark_store_deleted(store.rid).await?;

    // A relayed store was never on the hosted server, so there is nothing
    // there to delete and it is not asked (docs/RELAY_CONTRACT.md). What
    // there may be is a tunnel serving it right now: members still hold
    // tokens that name the store, so the relay stops piping to it at once
    // rather than when those tokens run out.
    if store.is_relayed() {
        state.relay.withdraw_store(&store.store_id);
        return Ok(Json(json!({})));
    }

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

/// One row of `GET /stores/{id}/members`: either a member (a `Grant`,
/// `status: "active"`) or an address that has been invited and has no account
/// yet (an `Invitation`, `status: "invited"`).
#[derive(Serialize)]
pub struct MemberView {
    /// `null` for an invitation: there is no account to name yet.
    user_id: Option<String>,
    email: String,
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

    // Every grant and invitation of one scope carries the same name (see
    // [`rename_share`]), so the first one that has a name is the share's.
    let mut share_name = String::new();
    let grants = state.db.grants_for_store(store.rid).await?;
    let mut out = Vec::with_capacity(grants.len());
    for grant in grants.into_iter().filter(|g| g.root == root) {
        if share_name.is_empty() {
            share_name = grant.share_name.clone();
        }
        if let Some(user) = state.db.get_user(grant.user_rid).await? {
            let public_keys = if caller_is_owner { user.public_keys() } else { None };
            out.push(MemberView {
                user_id: Some(user.user_uuid),
                email: user.email,
                role: grant.role,
                root: root_view(&root),
                status: STATUS_ACTIVE,
                has_key: has_key(&state, grant.user_rid, store.rid, &root).await?,
                public_keys,
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
                user_id: None,
                email: invitation.email,
                role: invitation.role,
                root: root_view(&root),
                status: STATUS_INVITED,
                has_key: false,
                public_keys: None,
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
    Ok(Json(MembersResponse { members: out, share_name }))
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
    let (subject, text, html) = if invited {
        let email_param: String = url::form_urlencoded::byte_serialize(recipient_email.trim().as_bytes()).collect();
        invitation_email(inviter_email, name, &format!("{base}/app/signup?email={email_param}"))
    } else {
        shared_with_you_email(inviter_email, name, &format!("{base}/app/"))
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
    let root = normalize_root(req.root.as_deref().unwrap_or(WHOLE_STORE))?;
    if !root.is_empty() && req.role == "owner" {
        // Owner is a role on a store — it can delete the thing and every
        // other owner's access to it. A share is a subtree of somebody
        // else's store; there is nothing there to own.
        return Err(CloudError::BadRequest("a share's members are editors or readers; owner is a role on the whole store".to_string()));
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
        Some(target) => grant_member(&state, &store, &authed.user, target, &req.role, &root, &share_name).await,
        None => invite_member(&state, &store, &authed.user, email, &req.role, &root, &share_name).await,
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
/// needs to wrap the scope's key straight away.
async fn grant_member(
    state: &AppState,
    store: &HostedStoreRow,
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
            // can be that owner.
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

    send_share_mail(state, store, share_name, &caller.email, &target.email, false).await;
    let has_key = has_key(state, target.rid, store.rid, root).await?;
    Ok(Json(MemberView {
        user_id: Some(target.user_uuid.clone()),
        // The caller is an owner (`put_member` required it), and these are
        // exactly what they need to wrap the scope's key to the new member
        // without a second round trip.
        public_keys: target.public_keys(),
        email: target.email,
        role: role.to_string(),
        root: root_view(root),
        status: STATUS_ACTIVE,
        has_key,
    }))
}

/// `PUT members` for an address with no usable account yet: an invitation,
/// upserted so the same address is never invited twice to the same scope.
async fn invite_member(
    state: &AppState,
    store: &HostedStoreRow,
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
        return Err(CloudError::BadRequest("an owner must already have a verified Pimble account; invite them as an editor or reader".to_string()));
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

    send_share_mail(state, store, share_name, &caller.email, email, true).await;
    Ok(Json(MemberView {
        user_id: None,
        email: email.to_string(),
        role: role.to_string(),
        root: root_view(root),
        status: STATUS_INVITED,
        has_key: false,
        public_keys: None,
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
/// believed — the store's owners. A
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

/// `GET /api/v1/stores/{id}/keys[?root=<node id>]` — the caller's own
/// envelopes for one scope of this store (the store key without a `root`,
/// that share's key with one), never another member's, plus the owners whose
/// signatures on them are legitimate.
pub async fn get_store_keys(
    State(state): State<AppState>,
    authed: AuthedUser,
    Path(store_id): Path<String>,
    Query(scope): Query<ScopeQuery>,
) -> CloudResult<Json<KeyGrantsResponse>> {
    let store = require_live_store(&state, &store_id).await?;
    let root = normalize_root(&scope.root)?;
    require_role_for_scope(&state, &store, authed.user.rid, &root).await?;

    let grants = state.db.key_grants_for_user_store_and_root(authed.user.rid, store.rid, &root).await?;
    let mut envelopes = Vec::with_capacity(grants.len());
    for grant in grants {
        let envelope: KeyEnvelope =
            serde_json::from_str(&grant.envelope).map_err(|e| CloudError::Internal(format!("stored envelope is not valid JSON: {e}")))?;
        envelopes.push(KeyGrantView { key_id: grant.key_id, envelope });
    }

    let mut signers = Vec::new();
    for grant in state.db.grants_for_store(store.rid).await? {
        if grant.role != "owner" || !grant.is_whole_store() {
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
    /// Which scope key this envelope carries: the shared node, or absent for
    /// the store key.
    #[serde(default)]
    pub root: Option<String>,
}

#[derive(Deserialize)]
pub struct PutStoreKeysRequest {
    pub envelopes: Vec<EnvelopeUpsert>,
}

/// `PUT /api/v1/stores/{id}/keys` — upserts one or more (user, key id, scope)
/// envelopes. Ownership rule: an owner may set anyone's; a member may set
/// their own. Every envelope must be signed by the CALLER (whoever is
/// distributing the key), verified against the caller's own
/// `public_signing_key`; the recipient checks it against the `signers` of
/// `GET .../keys`, which are the store's owners.
pub async fn put_store_keys(
    State(state): State<AppState>,
    authed: AuthedUser,
    Path(store_id): Path<String>,
    Json(req): Json<PutStoreKeysRequest>,
) -> CloudResult<Json<Value>> {
    let store = require_live_store(&state, &store_id).await?;
    let caller_grants = state.db.grants_for_user_and_store(authed.user.rid, store.rid).await?;
    if caller_grants.is_empty() {
        return Err(CloudError::Forbidden("not a member of this store".to_string()));
    }
    let caller_is_owner = caller_grants.iter().any(|g| g.role == "owner" && g.is_whole_store());
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
        if item.user_id != authed.user.user_uuid && !caller_is_owner {
            return Err(CloudError::Forbidden("only an owner can set another member's keys".to_string()));
        }
        let root = normalize_root(item.root.as_deref().unwrap_or(WHOLE_STORE))?;
        verify_envelope_signature(&item.envelope, caller_signing_key)?;

        let target = state
            .db
            .find_user_by_uuid(&item.user_id)
            .await?
            .ok_or_else(|| CloudError::NotFound(format!("no such user: {}", item.user_id)))?;
        // The key only goes to somebody the scope is already shared with: a
        // whole-store grant reaches every scope, a scoped one only its own.
        let covered = state
            .db
            .grants_for_user_and_store(target.rid, store.rid)
            .await?
            .into_iter()
            .any(|g| g.is_whole_store() || g.root == root);
        if !covered {
            return Err(CloudError::BadRequest(format!("{} is not a member of this scope of the store", item.user_id)));
        }

        let envelope_json = to_json("envelope", &item.envelope)?;
        match state.db.find_key_grant(target.rid, store.rid, &item.key_id, &root).await? {
            Some(existing) => state.db.update_key_grant_envelope(existing.rid, &envelope_json).await?,
            None => {
                state.db.create_key_grant(target.rid, store.rid, &store.store_id, &item.key_id, &root, &envelope_json).await?;
            }
        }
    }
    Ok(Json(json!({})))
}
