//! Building the `stores` claim every minted token carries (docs/
//! CLOUD_CONTRACT.md: `{ "claims": { "email": ..., "stores": { "<store-uuid>":
//! "owner", ... } } }`).

use serde_json::{json, Map, Value};

use crate::db::UserRow;
use crate::error::CloudResult;
use crate::state::AppState;

/// `{ "<store-uuid>": "<role>", ... }` for every non-deleted store `user`
/// has a grant on. A grant on a store that's since been soft-deleted is
/// skipped defensively — `DELETE /stores/{id}` already removes its grants,
/// so this only matters if that ever partially fails.
pub async fn stores_claim(state: &AppState, user_rid: u64) -> CloudResult<Value> {
    let grants = state.db.grants_for_user(user_rid).await?;
    let mut map = Map::with_capacity(grants.len());
    for grant in grants {
        let live = match state.db.get_hosted_store(grant.store_rid).await? {
            Some(store) => !store.deleted,
            None => false,
        };
        if live {
            map.insert(grant.store_uuid, Value::String(grant.role));
        }
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
