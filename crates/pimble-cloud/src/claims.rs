//! Building the `stores` claim every minted token carries (docs/
//! CLOUD_CONTRACT.md: `{ "claims": { "email": ..., "stores": { "<store-uuid>":
//! "owner", ... } } }`, and docs/NODE_DOCUMENT_CONTRACT.md section 5 for the
//! scoped form a share takes).

use std::collections::{BTreeMap, HashMap};

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
    let grants = state.db.grants_for_user(user_rid).await?;
    // One liveness query per store, not per grant: a user with five shares on
    // one store would otherwise ask about it five times.
    let mut live: HashMap<u64, bool> = HashMap::new();
    let mut claims: BTreeMap<String, StoreClaim> = BTreeMap::new();

    for grant in grants {
        if !is_known_role(&grant.role) {
            continue;
        }
        let is_live = match live.get(&grant.store_rid) {
            Some(known) => *known,
            None => {
                let known = match state.db.get_hosted_store(grant.store_rid).await? {
                    Some(store) => !store.deleted,
                    None => false,
                };
                live.insert(grant.store_rid, known);
                known
            }
        };
        if !is_live {
            continue;
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
    Ok(Value::Object(map))
}

/// The full `claims` object for `user`'s token: `{ email, stores }`.
pub async fn claims_for_user(state: &AppState, user: &UserRow) -> CloudResult<Value> {
    Ok(json!({
        "email": user.email,
        "stores": stores_claim(state, user.rid).await?,
    }))
}
