//! The identity attached to a connection and the store-scoped authorization
//! check every RPC that names a store runs against it
//! (docs/CLOUD_CONTRACT.md "B: pimble-server" items 4-5).
//!
//! **How a [`Principal`] reaches a handler method.** [`crate::auth::AuthMiddleware`]
//! resolves one per HTTP request (in the tower layer wired onto jsonrpsee
//! with `Server::builder().set_http_middleware(...)`) and inserts it into
//! the request's `http::Extensions` before the request reaches jsonrpsee's
//! dispatch. For a WebSocket connection jsonrpsee reads that `Extensions`
//! once, off the upgrade request, and reuses it for every call on the
//! connection (`jsonrpsee-server`'s `transport/ws.rs`); for a plain HTTP
//! POST it reads it per request. Every RPC that needs a principal declares
//! `with_extensions` on its `#[method(...)]`/`#[subscription(...)]`
//! attribute in `pimble-rpc`'s trait (this is a jsonrpsee 0.24 feature,
//! chosen over a separate RPC middleware layer because it needs no new
//! crate and reads the same `Extensions` type the HTTP layer already uses);
//! that attribute only changes the *generated server trait*
//! (`PimbleApiServer`) by inserting an `ext: &jsonrpsee::Extensions`
//! parameter right after `&self` — the base trait `pimble_rpc::PimbleApi`
//! that `pimble-client` also implements is untouched, so this never affects
//! the client's generated API. A handler method reads its principal with
//! [`principal_of`].
//!
//! A connection that arrived with no verifier configured at all (no static
//! token, no JWT issuer/JWKS) is always [`Principal::Service`] — matching
//! today's "loopback and tokenless" embedded-app-server posture, where
//! anything reaching the server at all is already trusted.

use std::collections::HashMap;

use jsonrpsee::types::ErrorObjectOwned;
use jsonrpsee::Extensions;
use pimble_core::StoreId;
use pimble_rpc::forbidden_error;

/// Who is making this call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    /// This server's own operator: the embedded app server (always, since
    /// it configures no verifier), the CLI holding the static token, or the
    /// accounts service holding it on the hosted deployment. May do
    /// anything, including the server-lifecycle operations no `User`
    /// principal may ever call (`createStore`, `openStore`, `closeStore`,
    /// `addRemoteStore`, `setStoreSync`, `removeReplica`, `listRemoteStores`).
    Service,
    /// A verified end user, from a JWT's `sub`/`claims.email`/`claims.stores`
    /// (docs/CLOUD_CONTRACT.md "Identity and grants"). `grants` is exactly
    /// what the token said at mint time; a store not in it is invisible to
    /// this connection regardless of what role it might have on the
    /// accounts service by the time the token is checked.
    User {
        sub: String,
        email: String,
        grants: HashMap<StoreId, Role>,
    },
}

/// A user's access to one store, from lowest to highest: `Reader` <
/// `Editor` < `Owner`. Ordering matters only for display; [`Role::allows`]
/// is the actual authorization rule; a role that itself grants store
/// membership management (`Owner`) is fully out of scope for this server —
/// grants live in the accounts service, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Reader,
    Editor,
    Owner,
}

impl Role {
    /// Parse a JWT claim's role string (`"owner"`, `"editor"`, `"reader"`,
    /// lowercase per docs/CLOUD_CONTRACT.md's sample token). Anything else
    /// is not a role this server recognizes; the caller drops the grant
    /// rather than guessing, so an unknown value at worst withholds access
    /// rather than granting something backwards-compatibly unintended.
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "owner" => Some(Role::Owner),
            "editor" => Some(Role::Editor),
            "reader" => Some(Role::Reader),
            _ => None,
        }
    }

    /// Whether this role covers `needed` (docs/CLOUD_CONTRACT.md "B:
    /// pimble-server" item 5): every role may read; only `Editor` and
    /// `Owner` may write.
    pub fn allows(self, needed: Access) -> bool {
        match needed {
            Access::Read => true,
            Access::Write => matches!(self, Role::Editor | Role::Owner),
        }
    }
}

/// What a store operation needs of the caller's role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// The principal jsonrpsee attached to this call's `Extensions`, or the
/// least-privileged principal (a user with no grants at all) if somehow
/// none was — which fails every `authorize`/`authorize_service_only` call
/// closed rather than open. In practice every connection always has one:
/// [`crate::auth::AuthMiddleware`] inserts either a resolved credential's
/// principal or, with no verifier configured, `Principal::Service`, before
/// any request reaches jsonrpsee's dispatch.
pub fn principal_of(ext: &Extensions) -> Principal {
    ext.get::<Principal>().cloned().unwrap_or_else(|| Principal::User {
        sub: String::new(),
        email: String::new(),
        grants: HashMap::new(),
    })
}

/// An `Extensions` carrying `Principal::Service`, for a handler method to
/// pass to another handler method it calls internally (`removeReplica`
/// calling `closeStore`). A sync link applying a remote's already-authorized
/// change goes through `RpcHandler::apply_node_update_from`, which
/// authorises nothing, for the same reason. These are server-
/// internal calls that never went through the HTTP-edge auth layer at all,
/// so there is no real `Principal` to forward — the operation triggering
/// them was already authorized (or is itself `Service`-only) at its own
/// entry point.
pub fn service_extensions() -> Extensions {
    let mut ext = Extensions::new();
    ext.insert(Principal::Service);
    ext
}

/// The top-of-method check for every RPC that names a store
/// (docs/CLOUD_CONTRACT.md "B: pimble-server" item 5): `Service` may do
/// anything; a `User` needs a grant on `store_id` that covers `needed`.
pub fn authorize(principal: &Principal, store_id: StoreId, needed: Access) -> Result<(), ErrorObjectOwned> {
    match principal {
        Principal::Service => Ok(()),
        Principal::User { grants, .. } => match grants.get(&store_id) {
            Some(role) if role.allows(needed) => Ok(()),
            Some(_) => Err(forbidden_error(format!(
                "store {} does not grant the role this operation needs",
                store_id
            ))),
            None => Err(forbidden_error(format!("no grant for store {}", store_id))),
        },
    }
}

/// The top-of-method check for an RPC only `Principal::Service` may call at
/// all (`createStore`, `openStore`, `closeStore`, `addRemoteStore`,
/// `setStoreSync`, `removeReplica`, `listRemoteStores`): none of these name
/// a store a grant could cover (a store to create doesn't exist yet; the
/// replica-management calls spend this server's own saved credentials
/// against a remote the caller names, not a grant-checked local store).
pub fn authorize_service_only(principal: &Principal, operation: &str) -> Result<(), ErrorObjectOwned> {
    match principal {
        Principal::Service => Ok(()),
        Principal::User { .. } => Err(forbidden_error(format!("{} is only available to this server's own operator", operation))),
    }
}

/// The subset of `store_ids` a principal may read: every id, unmodified,
/// for `Service`; only the ones with a grant for a `User`. Used by
/// `listStores` and `search`'s empty-`stores`-means-everything case
/// (docs/CLOUD_CONTRACT.md "B: pimble-server" item 5: "`listStores` returns
/// only the stores the principal may read").
pub fn readable<'a>(principal: &Principal, store_ids: impl IntoIterator<Item = &'a StoreId>) -> Vec<StoreId> {
    match principal {
        Principal::Service => store_ids.into_iter().copied().collect(),
        Principal::User { grants, .. } => store_ids.into_iter().filter(|id| grants.contains_key(id)).copied().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn store() -> StoreId {
        StoreId::from_uuid(Uuid::new_v4())
    }

    #[test]
    fn service_may_do_anything() {
        assert!(authorize(&Principal::Service, store(), Access::Write).is_ok());
        assert!(authorize_service_only(&Principal::Service, "openStore").is_ok());
    }

    #[test]
    fn a_reader_may_read_but_not_write() {
        let s = store();
        let mut grants = HashMap::new();
        grants.insert(s, Role::Reader);
        let user = Principal::User { sub: "u".into(), email: "u@example.com".into(), grants };

        assert!(authorize(&user, s, Access::Read).is_ok());
        assert!(authorize(&user, s, Access::Write).is_err());
    }

    #[test]
    fn an_editor_may_read_and_write() {
        let s = store();
        let mut grants = HashMap::new();
        grants.insert(s, Role::Editor);
        let user = Principal::User { sub: "u".into(), email: "u@example.com".into(), grants };

        assert!(authorize(&user, s, Access::Read).is_ok());
        assert!(authorize(&user, s, Access::Write).is_ok());
    }

    #[test]
    fn an_unknown_store_is_forbidden() {
        let user = Principal::User { sub: "u".into(), email: "u@example.com".into(), grants: HashMap::new() };
        assert!(authorize(&user, store(), Access::Read).is_err());
    }

    #[test]
    fn a_user_may_never_call_a_service_only_operation() {
        let user = Principal::User { sub: "u".into(), email: "u@example.com".into(), grants: HashMap::new() };
        assert!(authorize_service_only(&user, "createStore").is_err());
    }

    #[test]
    fn readable_filters_for_a_user_but_not_for_service() {
        let a = store();
        let b = store();
        let mut grants = HashMap::new();
        grants.insert(a, Role::Reader);
        let user = Principal::User { sub: "u".into(), email: "u@example.com".into(), grants };

        assert_eq!(readable(&user, [&a, &b]), vec![a]);
        let mut service_result = readable(&Principal::Service, [&a, &b]);
        service_result.sort_by_key(|id| id.to_string());
        let mut expected = vec![a, b];
        expected.sort_by_key(|id| id.to_string());
        assert_eq!(service_result, expected);
    }

    #[test]
    fn role_parses_lowercase_names_only() {
        assert_eq!(Role::parse("owner"), Some(Role::Owner));
        assert_eq!(Role::parse("editor"), Some(Role::Editor));
        assert_eq!(Role::parse("reader"), Some(Role::Reader));
        assert_eq!(Role::parse("Owner"), None);
        assert_eq!(Role::parse("admin"), None);
    }
}
