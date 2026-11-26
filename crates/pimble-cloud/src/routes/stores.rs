//! `GET/POST /stores` and `DELETE /stores/{id}`, and what the members
//! (`super::members`) and key-grants (`super::store_keys`) sub-resources
//! share with them (docs/CRYPTO_CONTRACT.md, Phase 2a;
//! docs/NODE_DOCUMENT_CONTRACT.md section 5 for what a share is).
//!
//! "A member" is two things: a `Grant` (an account that can reach the store
//! now) and an `Invitation` (an address that will get one the moment it has a
//! verified account — see `super::members::claim_invitations`, which every verification and
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

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use pimble_core::{NodeId, StoreId, StoreKind};
use pimble_crypto::{CryptoError, KeyEnvelope};

use super::members::Names;
use crate::db::{GrantRow, HostedStoreRow, TIER_HOSTED, TIER_RELAY};
use crate::error::{CloudError, CloudResult};
use crate::session::AuthedUser;
use crate::state::AppState;

/// `pimble_crypto::verify_envelope` needs no private key, so the accounts
/// service (which only ever holds public keys) can authenticate an envelope
/// it relays (docs/CRYPTO_CONTRACT.md, `PUT /stores/{id}/keys`). A bad
/// signature or a signer that doesn't match `expected_signer` are both
/// `CryptoError::BadSignature` — both are the caller presenting an envelope
/// they can't prove they made, so both map to 401.
pub(super) fn verify_envelope_signature(envelope: &KeyEnvelope, expected_signer: &str) -> CloudResult<()> {
    pimble_crypto::verify_envelope(envelope, expected_signer).map_err(|e| match e {
        CryptoError::BadSignature => CloudError::Unauthorized("envelope signature does not verify".to_string()),
        other => CloudError::BadRequest(format!("envelope is invalid: {other}")),
    })
}

fn rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).unwrap_or_default().to_rfc3339()
}

pub(super) const ROLES: [&str; 4] = ["owner", "editor", "commenter", "reader"];

/// The scope that is the whole store rather than one shared subtree.
pub(super) const WHOLE_STORE: &str = "";

/// The share name a whole-store grant or invitation carries: none. A store is
/// named by its own `name`.
pub(super) const NO_SHARE_NAME: &str = "";

/// What a share with no name of its own is called. A share written before
/// shares had names (or by anything that somehow skipped `PUT members`) still
/// must not be shown to a recipient as the store it lives in: the owner's
/// store name is not theirs to see (Joe, 2026-09-21).
pub(super) const UNNAMED_SHARE: &str = "Shared folder";

/// A `?root=<node id>` on an endpoint that has no body to carry one. Absent
/// (or empty) is the whole store.
#[derive(Deserialize)]
pub struct ScopeQuery {
    #[serde(default)]
    pub(super) root: String,
}

/// A scope as this service stores and compares it: `""` for the whole store,
/// or a node id in exactly one spelling. A root is an id, so it is parsed and
/// re-rendered rather than trusted as typed — otherwise the same node in two
/// spellings (uppercase hex, braces) would be two different shares that never
/// find each other's grants, envelopes or invitations.
pub(super) fn normalize_root(raw: &str) -> CloudResult<String> {
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
pub(super) fn root_view(root: &str) -> Option<String> {
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

pub(super) fn to_json(field: &'static str, value: &impl Serialize) -> CloudResult<String> {
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
    /// An owner's name in this share, when the caller is not an owner — what
    /// the recipient of a share is shown ("shared by <name>"). Never an email
    /// (docs/PRESENCE_CONTRACT.md section 3). `null` for one's own store.
    shared_by: Option<String>,
}

/// The `shared_by` of a row as its holder sees it: an owner's name in the
/// row's scope unless
/// this grant is their own whole-store ownership, `None` when it is. Costs a
/// query per store the caller doesn't own outright, which is what `GET
/// /stores` pays; a store with several owners shows the first one the grant
/// listing yields (any one of them answers "who shared this with me").
pub(crate) async fn shared_by(state: &AppState, store: &HostedStoreRow, grant: &GrantRow) -> CloudResult<Option<String>> {
    if grant.role == "owner" && grant.is_whole_store() {
        return Ok(None);
    }
    for owner in state.db.grants_for_store(store.rid).await? {
        if owner.role == "owner" && owner.is_whole_store() {
            if let Some(owner) = state.db.get_user(owner.user_rid).await? {
                let names = Names::of_store(state, store.rid).await?;
                return Ok(Some(names.name_of(&owner, &grant.root)));
            }
        }
    }
    Ok(None)
}

/// What a grant's row is called: the store's name when the grant is of the
/// whole store, the share's own name when it is of one node, and
/// [`UNNAMED_SHARE`] when a scoped grant somehow carries none — a recipient
/// is never shown the name of the store their share sits in.
pub(super) fn row_name(store: &HostedStoreRow, grant: &GrantRow) -> String {
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
pub(super) async fn require_live_store(state: &AppState, store_id: &str) -> CloudResult<HostedStoreRow> {
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
pub(super) async fn require_role_for_scope(state: &AppState, store: &HostedStoreRow, user_rid: u64, root: &str) -> CloudResult<String> {
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
pub(super) async fn is_store_owner(state: &AppState, store: &HostedStoreRow, user_rid: u64) -> CloudResult<bool> {
    Ok(state.db.find_grant(user_rid, store.rid, WHOLE_STORE).await?.is_some_and(|g| g.role == "owner"))
}

/// `Forbidden` unless the caller is an owner of `store`.
pub(super) async fn require_owner(state: &AppState, store: &HostedStoreRow, user_rid: u64) -> CloudResult<()> {
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
pub(super) const LAST_OWNER_ERROR: &str = "cannot remove the last owner of a store";

pub(super) async fn owner_count(state: &AppState, store_rid: u64) -> CloudResult<usize> {
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
pub(super) fn stored_name(raw: &str, what: &str) -> CloudResult<String> {
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
pub(super) fn stored_name_or_empty(raw: &str, what: &str) -> CloudResult<String> {
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
    // The owner's grant is written under the creator's account lock, as every
    // grant is (`AppState::lock_live_account`), taken before anything is
    // made: an account being deleted makes no store it would own once gone.
    let (_account, live) = state.lock_live_account(authed.user.rid).await?;
    if live.is_none() {
        return Err(CloudError::Conflict("This account was deleted while the store waited to be made, so nothing was made.".to_string()));
    }

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
            state.db.delete_share_names_for_store(deleted_row.rid).await?;
            state.db.revive_hosted_store(deleted_row.rid, &name, &dir_name, kind_str, req.tier.as_str()).await?
        }
        None => state.db.create_hosted_store(&store_id_str, &name, &dir_name, kind_str, req.tier.as_str()).await?,
    };
    // The whole store, so no share name: this row is named by the store.
    let grant = state.db.create_grant(authed.user.rid, hosted.rid, &store_id_str, "owner", WHOLE_STORE, NO_SHARE_NAME).await?;
    // A store revived with the id has none of the old one's members: what
    // its watchers hold changed.
    state.watch_interest.changed([store_id_str.clone()]);
    // The creator owns the whole store, so never "shared by" anyone.
    Ok(Json(store_view(&hosted, &grant, None)))
}

pub async fn delete_store(State(state): State<AppState>, authed: AuthedUser, Path(store_id): Path<String>) -> CloudResult<Json<Value>> {
    let store = require_live_store(&state, &store_id).await?;
    // Under the owner lock: an owner being demoted or removed right now is
    // judged after that, not before (`AppState::store_owner_locks`).
    let _owners = state.store_owner_locks.lock(&store.store_id).await;
    require_owner(&state, &store, authed.user.rid).await?;
    // Everything that names the store goes with it: nobody keeps a grant, an
    // envelope, or a standing invitation to a store that no longer exists
    // (docs/NODE_DOCUMENT_CONTRACT.md section 7, `DELETE /stores/{id}`).
    state.db.delete_grants_for_store(store.rid).await?;
    state.db.delete_key_grants_for_store(store.rid).await?;
    state.db.delete_invitations_for_store(store.rid).await?;
    state.db.delete_share_names_for_store(store.rid).await?;
    state.db.mark_store_deleted(store.rid).await?;
    // Nobody holds it any more: nothing in it is told to watchers.
    state.watch_interest.changed([store.store_id.clone()]);

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
