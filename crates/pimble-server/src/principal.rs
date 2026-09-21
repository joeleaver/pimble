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
use pimble_core::{NodeId, StoreAccess, StoreId};
use pimble_rpc::forbidden_error;

/// What a reader's write, or a write to a `Read` replica, answers with:
/// `-32004` and [`StoreAccess::READ_ONLY_REFUSAL`] as the whole message,
/// with no `Forbidden: ` in front, because the sentence is shown to the
/// person as it is (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles").
pub fn read_only_error() -> ErrorObjectOwned {
    ErrorObjectOwned::owned(pimble_rpc::RpcError::Forbidden(String::new()).code(), StoreAccess::READ_ONLY_REFUSAL, None::<()>)
}

/// What a scoped principal gets for a document outside its scope, whether
/// or not the document exists: a member of one share must not learn what
/// else the store holds, so the answer is the same for a document that is
/// not theirs and for one that is not there.
pub fn no_grant_for_document_error() -> ErrorObjectOwned {
    forbidden_error(NO_GRANT_FOR_DOCUMENT)
}

/// The document refusal's sentence, for a caller that has to tell it from a
/// failure (a vault link leaves a refused document for later, and keeps
/// the link up).
pub const NO_GRANT_FOR_DOCUMENT: &str = "no grant for this document";

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
        grants: HashMap<StoreId, Grant>,
    },
}

/// One grant in a token: a role on a whole store, or a role on each of the
/// subtrees a share's recipient was given (docs/NODE_DOCUMENT_CONTRACT.md
/// section 5: a role per shared root, so a reader of one folder who edits
/// another is neither the lesser nor the greater of the two on both). A
/// scoped grant reaches exactly the documents in the store's scope sets for
/// its roots (`RpcHandler`'s scope check); the role of the root whose set
/// holds a document says what the member may do with it, and a document in
/// two of the member's scopes takes the wider role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grant {
    Whole(Role),
    Scoped(HashMap<NodeId, Role>),
}

impl Grant {
    pub fn whole(role: Role) -> Self {
        Grant::Whole(role)
    }

    /// A share's recipient: a role per shared root.
    pub fn scoped(roots: impl IntoIterator<Item = (NodeId, Role)>) -> Self {
        Grant::Scoped(roots.into_iter().collect())
    }

    pub fn is_scoped(&self) -> bool {
        matches!(self, Grant::Scoped(_))
    }

    /// Whether anything this grant reaches may be used for `needed`: the
    /// store-level question. For a scoped grant the per-document check
    /// still decides (an editor of one folder is not one of another).
    pub fn allows(&self, needed: Access) -> bool {
        match self {
            Grant::Whole(role) => role.allows(needed),
            Grant::Scoped(roots) => roots.values().any(|role| role.allows(needed)),
        }
    }

    /// The scope roots whose role covers `needed`, in id order (so two
    /// connections with the same grant name them alike); `None` for the
    /// whole store.
    pub fn roots_allowing(&self, needed: Access) -> Option<Vec<NodeId>> {
        match self {
            Grant::Whole(_) => None,
            Grant::Scoped(roots) => {
                let mut ids: Vec<NodeId> = roots.iter().filter(|(_, role)| role.allows(needed)).map(|(id, _)| *id).collect();
                ids.sort_by_key(|id| id.to_string());
                Some(ids)
            }
        }
    }
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
            Some(grant) if grant.allows(needed) => Ok(()),
            // Only a reader fails `allows`, and a reader's write is refused
            // with the one sentence every reader refusal carries.
            Some(_) => Err(read_only_error()),
            None => Err(forbidden_error(format!("no grant for store {}", store_id))),
        },
    }
}

/// The check for an RPC that is an owner's to call (`setScope`, `getScopes`:
/// the scope sets are authorization metadata the owner's devices publish,
/// docs/NODE_DOCUMENT_CONTRACT.md section 5): `Service`, or a whole-store
/// grant whose role is `Owner`. A reader is refused as a reader is
/// everywhere; anyone else is told whose call it is.
pub fn authorize_owner(principal: &Principal, store_id: StoreId, operation: &str) -> Result<(), ErrorObjectOwned> {
    match principal {
        Principal::Service => Ok(()),
        Principal::User { grants, .. } => match grants.get(&store_id) {
            Some(Grant::Whole(Role::Owner)) => Ok(()),
            Some(_) => Err(forbidden_error(format!("{} is only available to an owner of store {}", operation, store_id))),
            None => Err(forbidden_error(format!("no grant for store {}", store_id))),
        },
    }
}

/// The scope roots of `principal`'s grant on `store_id` whose role covers
/// `needed`: `None` for a principal that reaches the whole store (`Service`,
/// or a whole-store grant), `Some(roots)` for a share's member, who reaches
/// exactly the documents in those roots' scope sets
/// (docs/NODE_DOCUMENT_CONTRACT.md section 5). With `Access::Read` that is
/// every root of the grant. Call after [`authorize`]: a principal with no
/// grant at all reads as unscoped here, and `authorize` is what refuses it.
pub fn scope_roots_of(principal: &Principal, store_id: StoreId, needed: Access) -> Option<Vec<NodeId>> {
    match principal {
        Principal::Service => None,
        Principal::User { grants, .. } => grants.get(&store_id).and_then(|grant| grant.roots_allowing(needed)),
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
        grants.insert(s, Grant::whole(Role::Reader));
        let user = Principal::User { sub: "u".into(), email: "u@example.com".into(), grants };

        assert!(authorize(&user, s, Access::Read).is_ok());
        let refusal = authorize(&user, s, Access::Write).unwrap_err();
        assert_eq!(refusal.code(), -32004);
        assert_eq!(refusal.message(), StoreAccess::READ_ONLY_REFUSAL, "the sentence alone, as the person sees it");
        assert_eq!(StoreAccess::refusal_in(refusal.message()), Some(StoreAccess::READ_ONLY_REFUSAL));
    }

    #[test]
    fn only_a_whole_store_owner_or_the_service_is_an_owner() {
        let s = store();
        let user = |grant: Grant| Principal::User { sub: "u".into(), email: "u@example.com".into(), grants: HashMap::from([(s, grant)]) };
        assert!(authorize_owner(&Principal::Service, s, "setScope").is_ok());
        assert!(authorize_owner(&user(Grant::whole(Role::Owner)), s, "setScope").is_ok());
        for grant in [Grant::whole(Role::Editor), Grant::whole(Role::Reader), Grant::scoped([(NodeId::new(), Role::Editor)]), Grant::scoped([(NodeId::new(), Role::Owner)])] {
            let refusal = authorize_owner(&user(grant), s, "setScope").unwrap_err();
            assert_eq!(refusal.code(), -32004);
        }
        assert_eq!(authorize_owner(&user(Grant::whole(Role::Owner)), store(), "setScope").unwrap_err().code(), -32004);
    }

    #[test]
    fn a_scoped_grant_names_its_roots_by_role_and_a_whole_one_none() {
        let s = store();
        let (read_root, edit_root) = (NodeId::new(), NodeId::new());
        let mut grants = HashMap::new();
        grants.insert(s, Grant::scoped([(read_root, Role::Reader), (edit_root, Role::Editor)]));
        let member = Principal::User { sub: "u".into(), email: "u@example.com".into(), grants };
        // At the store level an editor of one root may write; which
        // documents is the per-document check's business.
        assert!(authorize(&member, s, Access::Read).is_ok());
        assert!(authorize(&member, s, Access::Write).is_ok());
        let mut every_root = vec![read_root, edit_root];
        every_root.sort_by_key(|id| id.to_string());
        assert_eq!(scope_roots_of(&member, s, Access::Read), Some(every_root));
        assert_eq!(scope_roots_of(&member, s, Access::Write), Some(vec![edit_root]));
        assert_eq!(scope_roots_of(&member, store(), Access::Read), None, "no grant reads as unscoped; authorize refuses it first");

        // A reader of every root is a reader: the sentence, at the door.
        let readers = Principal::User {
            sub: "u".into(),
            email: "u@example.com".into(),
            grants: HashMap::from([(s, Grant::scoped([(read_root, Role::Reader)]))]),
        };
        assert_eq!(authorize(&readers, s, Access::Write).unwrap_err().message(), StoreAccess::READ_ONLY_REFUSAL);
        assert_eq!(scope_roots_of(&readers, s, Access::Write), Some(Vec::new()));

        let mut grants = HashMap::new();
        grants.insert(s, Grant::whole(Role::Editor));
        let whole = Principal::User { sub: "u".into(), email: "u@example.com".into(), grants };
        assert_eq!(scope_roots_of(&whole, s, Access::Read), None);
        assert_eq!(scope_roots_of(&Principal::Service, s, Access::Write), None);

        let document_refusal = no_grant_for_document_error();
        assert_eq!(document_refusal.code(), -32004);
        assert_eq!(document_refusal.message(), "Forbidden: no grant for this document");
        assert!(document_refusal.message().contains(NO_GRANT_FOR_DOCUMENT));
    }

    #[test]
    fn an_editor_may_read_and_write() {
        let s = store();
        let mut grants = HashMap::new();
        grants.insert(s, Grant::whole(Role::Editor));
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
        grants.insert(a, Grant::whole(Role::Reader));
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
