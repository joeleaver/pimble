//! Building the `stores` claim every minted token carries (docs/
//! CLOUD_CONTRACT.md: `{ "claims": { "email": ..., "stores": { "<store-uuid>":
//! "owner", ... } } }`, and docs/NODE_DOCUMENT_CONTRACT.md section 5 for the
//! scoped form a share takes).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{json, Map, Value};

use crate::db::UserRow;
use crate::error::CloudResult;
use crate::state::AppState;

/// One store's entry in the claim, before it is rendered.
enum StoreClaim {
    /// A grant on the whole store: `"editor"`.
    WholeStore(String),
    /// Only scoped grants: `{ "roots": { "<node id>": "editor", ... } }` — a
    /// role per shared root, and no store-level role at all, since there is
    /// no role this user holds on the store as a whole.
    Scoped(BTreeMap<String, String>),
}

/// The roles this service mints. A role string it does not know is left out
/// of the token entirely rather than passed through: a row this service never
/// wrote must not be able to invent a permission, and a Pimble server reading
/// a name it cannot interpret is not something to rely on either way.
fn is_known_role(role: &str) -> bool {
    matches!(role, "owner" | "editor" | "reader")
}

/// `{ "<store-uuid>": "<role>", ... }` for every non-deleted store `user` has
/// a whole-store grant on, and `{ "<store-uuid>": { "roots": { "<node id>":
/// "<role>", ... } } }` for every store where all they hold is shares, each
/// share with its own role (Joe, 2026-09-21; docs/NODE_DOCUMENT_CONTRACT.md
/// section 5). A whole-store grant wins over any scoped one on the same
/// store: it already covers every node in it.
///
/// A grant on a store that's since been soft-deleted is skipped defensively —
/// `DELETE /stores/{id}` already removes its grants, so this only matters if
/// that ever partially fails.
pub async fn stores_claim(state: &AppState, user_rid: u64) -> CloudResult<Value> {
    Ok(stores_for_token(state, user_rid).await?.claim)
}

/// What one pass over a user's grants says about their token: the `stores`
/// claim, and which of those stores are not on the hosted Pimble server.
pub struct TokenStores {
    /// The `stores` claim — see [`stores_claim`].
    pub claim: Value,
    /// The ids of the relay-tier stores the claim names (docs/
    /// RELAY_CONTRACT.md), sorted, each once however many shares of it the
    /// user holds. `POST /token` answers these with the relay's URL for each,
    /// since the token's own `rpc_url` does not serve them. Exactly the
    /// claim's relayed stores and no others: a grant the claim leaves out
    /// (an unknown role, a deleted store) names no endpoint either.
    pub relayed: Vec<String>,
}

/// [`stores_claim`], plus which of the claimed stores are relayed — read off
/// the same store rows, so minting a token costs no second pass.
pub async fn stores_for_token(state: &AppState, user_rid: u64) -> CloudResult<TokenStores> {
    let grants = state.db.grants_for_user(user_rid).await?;
    // One liveness query per store, not per grant: a user with five shares on
    // one store would otherwise ask about it five times. `Some(relayed)` for
    // a live store, `None` for one that is deleted or gone.
    let mut live: HashMap<u64, Option<bool>> = HashMap::new();
    let mut claims: BTreeMap<String, StoreClaim> = BTreeMap::new();
    let mut relayed: BTreeSet<String> = BTreeSet::new();

    for grant in grants {
        if !is_known_role(&grant.role) {
            continue;
        }
        let store_is = match live.get(&grant.store_rid) {
            Some(known) => *known,
            None => {
                let known = state.db.get_hosted_store(grant.store_rid).await?.filter(|store| !store.deleted).map(|store| store.is_relayed());
                live.insert(grant.store_rid, known);
                known
            }
        };
        let Some(is_relayed) = store_is else { continue };
        if is_relayed {
            relayed.insert(grant.store_uuid.clone());
        }

        match claims.get_mut(&grant.store_uuid) {
            // A whole-store grant already covers everything this store could
            // add; nothing narrows it.
            Some(StoreClaim::WholeStore(_)) => {}
            Some(StoreClaim::Scoped(roots)) => {
                if grant.is_whole_store() {
                    claims.insert(grant.store_uuid, StoreClaim::WholeStore(grant.role));
                } else {
                    // One grant per (user, store, root) is this service's own
                    // uniqueness rule, so no second grant can disagree about
                    // a root's role.
                    roots.insert(grant.root, grant.role);
                }
            }
            None => {
                let claim = if grant.is_whole_store() {
                    StoreClaim::WholeStore(grant.role)
                } else {
                    StoreClaim::Scoped(BTreeMap::from([(grant.root, grant.role)]))
                };
                claims.insert(grant.store_uuid, claim);
            }
        }
    }

    let mut map = Map::with_capacity(claims.len());
    for (store_uuid, claim) in claims {
        let value = match claim {
            StoreClaim::WholeStore(role) => Value::String(role),
            StoreClaim::Scoped(roots) => json!({ "roots": roots }),
        };
        map.insert(store_uuid, value);
    }
    Ok(TokenStores { claim: Value::Object(map), relayed: relayed.into_iter().collect() })
}

/// The full `claims` object for `user`'s token: `{ email, stores }`.
pub async fn claims_for_user(state: &AppState, user: &UserRow) -> CloudResult<Value> {
    Ok(claims_and_relayed_for_user(state, user).await?.0)
}

/// [`claims_for_user`], and beside it the relay-tier stores those claims name
/// ([`TokenStores::relayed`]) — what `POST /token` needs to say where each of
/// them is reached. The claims are the same object either way: the relay tier
/// adds nothing to a token.
pub async fn claims_and_relayed_for_user(state: &AppState, user: &UserRow) -> CloudResult<(Value, Vec<String>)> {
    let stores = stores_for_token(state, user.rid).await?;
    Ok((json!({ "email": user.email, "stores": stores.claim }), stores.relayed))
}
