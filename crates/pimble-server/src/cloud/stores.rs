//! Hosted and relayed stores: `POST`/`GET`/`DELETE /stores`, how the
//! account holds one ([`HeldAs`]), and the relay's and the JWKS's URLs on
//! the accounts service's origin.

use serde::{Deserialize, Serialize};

use super::{delete, endpoint, get, post, Http, Result};


#[derive(Serialize)]
struct CreateStoreRequest<'a> {
    name: &'a str,
    kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    store_id: Option<&'a str>,
    /// `"relay"` for a store shared from this computer
    /// (docs/RELAY_CONTRACT.md); left out for a hosted one, the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    tier: Option<&'a str>,
}

/// `StoreView::tier` of a store hosted on Pimble Cloud, and what a row
/// without the field reads as.
pub const TIER_HOSTED: &str = "hosted";
/// `StoreView::tier` of a store served from its owner's computer through
/// Pimble Cloud's relay.
pub const TIER_RELAY: &str = "relay";

pub(super) fn default_tier() -> String {
    TIER_HOSTED.to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct StoreView {
    pub store_id: String,
    pub name: String,
    pub role: String,
    pub kind: String,
    pub created_at: String,
    /// The shared node when the grant is a share of one subtree
    /// (docs/NODE_DOCUMENT_CONTRACT.md section 5), absent for a whole-store
    /// grant. Defaulted so a service from before scopes still parses.
    #[serde(default)]
    pub root: Option<String>,
    /// An owner's name in this share when the account is not an owner of
    /// the store (an email from a service from before names).
    #[serde(default)]
    pub shared_by: Option<String>,
    /// [`TIER_HOSTED`] or [`TIER_RELAY`] (docs/RELAY_CONTRACT.md). Defaulted
    /// so a service from before the relay still parses: hosted.
    #[serde(default = "default_tier")]
    pub tier: String,
}

impl StoreView {
    pub fn is_relayed(&self) -> bool {
        self.tier == TIER_RELAY
    }

    /// The shared node as a `NodeId`, if the row names one it can parse.
    pub fn scope_root(&self) -> Option<pimble_core::NodeId> {
        self.root.as_deref().and_then(|r| pimble_core::NodeId::parse(r).ok())
    }

    /// What a device with this grant may change (`Store::access`): a reader
    /// reads, a commenter writes discussions, everyone else edits what they
    /// reach.
    pub fn access(&self) -> pimble_core::StoreAccess {
        pimble_core::StoreAccess::of_role(&self.role)
    }
}

/// How the signed-in account holds one store, read off its `GET /stores`
/// rows (one per grant): the whole of it, or the shares of it it was given
/// (docs/NODE_DOCUMENT_CONTRACT.md section 5). What `sync.json` and the
/// manifest of the replica here are written from, at `cloudAddHostedStore`
/// and again at every connect of its vault link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldAs {
    /// A share's name for a share (the owner's store name never reaches a
    /// recipient), the store's otherwise. `None` when no row names the store.
    pub name: Option<String>,
    /// The shared nodes; empty for the whole store. A whole-store grant
    /// wins over any share of the same store, as it does in the token.
    pub roots: Vec<pimble_core::NodeId>,
    /// The roots among them held as a reader or a commenter, when others are
    /// edited (or, all of them, when some are held as a commenter).
    pub read_only_roots: Vec<pimble_core::NodeId>,
    /// Among `read_only_roots`, the ones held as a commenter.
    pub comment_roots: Vec<pimble_core::NodeId>,
    /// `Read` when nothing the account holds of the store may be written.
    pub access: pimble_core::StoreAccess,
    pub shared_by: Option<String>,
}

impl HeldAs {
    /// One's own store, just hosted.
    pub fn owner() -> Self {
        Self { name: None, roots: Vec::new(), read_only_roots: Vec::new(), comment_roots: Vec::new(), access: pimble_core::StoreAccess::Full, shared_by: None }
    }

    /// `None` when no row names the store (nothing is known of how it is
    /// held; the hosted server's answer to the token decides what really is).
    pub fn from_rows(rows: &[StoreView], store_id: pimble_core::StoreId) -> Option<Self> {
        let id = store_id.to_string();
        let rows: Vec<&StoreView> = rows.iter().filter(|row| row.store_id == id).collect();
        if let Some(whole) = rows.iter().find(|row| row.root.is_none()) {
            return Some(Self {
                name: Some(whole.name.clone()),
                roots: Vec::new(),
                read_only_roots: Vec::new(),
                comment_roots: Vec::new(),
                access: whole.access(),
                shared_by: whole.shared_by.clone(),
            });
        }
        let shares: Vec<(pimble_core::NodeId, &StoreView)> = rows.iter().filter_map(|row| Some((row.scope_root()?, *row))).collect();
        let (_, first) = shares.first()?;
        let reads = |row: &StoreView| row.access() != pimble_core::StoreAccess::Full;
        let comments = |row: &StoreView| row.access() == pimble_core::StoreAccess::Comment;
        let all_read = shares.iter().all(|(_, row)| reads(row));
        let any_comment = shares.iter().any(|(_, row)| comments(row));
        let read_only: Vec<pimble_core::NodeId> = shares.iter().filter(|(_, row)| reads(row)).map(|(root, _)| *root).collect();
        // One share is called what its owner called it. Several shares of one
        // store have several names and the replica has one row: it says whose
        // they are, and each shared folder shows its own title beneath.
        let name = match (shares.len(), &first.shared_by) {
            (1, _) => first.name.clone(),
            (_, Some(email)) => format!("Shared by {email}"),
            (_, None) => "Shared with you".to_string(),
        };
        Some(Self {
            name: Some(name),
            roots: shares.iter().map(|(root, _)| *root).collect(),
            // Every root only read: the store's access says it, unless some
            // are a commenter's, when the roots say which.
            read_only_roots: if all_read && !any_comment { Vec::new() } else { read_only },
            comment_roots: shares.iter().filter(|(_, row)| comments(row)).map(|(root, _)| *root).collect(),
            // The widest role among the shares (`StoreAccess::widest`, the
            // browser's rule too): any root edited makes the store editable,
            // and the writes under a read-only root are refused one at a time.
            access: pimble_core::StoreAccess::widest(shares.iter().map(|(_, row)| row.access())).unwrap_or_default(),
            shared_by: first.shared_by.clone(),
        })
    }
}

/// Whether the account's rows say it is an owner of `store_id`: a
/// whole-store grant with the owner role. Sharing is an owner's to do (the
/// accounts service takes `PUT members` and the hosted server `setScope`
/// from nobody else), so the owner's side of it runs on an owner's devices
/// only.
pub fn is_owner_of(rows: &[StoreView], store_id: pimble_core::StoreId) -> bool {
    let id = store_id.to_string();
    rows.iter().any(|row| row.store_id == id && row.root.is_none() && row.role == "owner")
}

pub async fn create_store(http: &dyn Http, base_url: &str, session: &str, name: &str, kind: &str, store_id: Option<&str>) -> Result<StoreView> {
    post(http, &endpoint(base_url, "/api/v1/stores"), Some(session), &CreateStoreRequest { name, kind, store_id, tier: None }).await
}

/// Record a store that is shared from this computer (docs/RELAY_CONTRACT.md):
/// its id, the owner's grant, and nothing else. The name sent is empty, since
/// nothing on Pimble Cloud needs the owner's name for it, and the accounts
/// service makes no call to the hosted server for it.
pub async fn create_relayed_store(http: &dyn Http, base_url: &str, session: &str, store_id: &str) -> Result<StoreView> {
    post(http, &endpoint(base_url, "/api/v1/stores"), Some(session), &CreateStoreRequest { name: "", kind: "vault", store_id: Some(store_id), tier: Some(TIER_RELAY) }).await
}

/// Remove a store's record: its grants, invitations and key envelopes go
/// with it, and a relayed store is withdrawn from the relay at once.
pub async fn delete_store(http: &dyn Http, base_url: &str, session: &str, store_id: &str) -> Result<()> {
    delete(http, &endpoint(base_url, &format!("/api/v1/stores/{store_id}")), session).await
}

/// `path` on the accounts service's origin as a WebSocket URL: `https` is
/// `wss`, `http` is `ws`.
fn ws_endpoint(base_url: &str, path: &str) -> std::result::Result<url::Url, String> {
    let mut url: url::Url = endpoint(base_url, path).parse().map_err(|e| format!("{base_url} is not a URL: {e}"))?;
    let scheme = match url.scheme() {
        "https" | "wss" => "wss",
        "http" | "ws" => "ws",
        other => return Err(format!("{base_url}: cannot open a WebSocket over {other}")),
    };
    url.set_scheme(scheme).map_err(|_| format!("{base_url}: cannot open a WebSocket there"))?;
    Ok(url)
}

/// The owner's tunnel (docs/RELAY_CONTRACT.md): `wss://<account url>/api/v1/relay`.
pub fn relay_tunnel_url(base_url: &str) -> std::result::Result<url::Url, String> {
    ws_endpoint(base_url, "/api/v1/relay")
}

/// Where members reach a relayed store, as far as this device can say from
/// the URL it signed in at (the accounts service names the real one in
/// `/token`'s `stores`, from its own public URL). Written to a relayed
/// store's `sync.json` for the record; nothing connects to it from here.
pub fn relay_member_url(base_url: &str, store_id: pimble_core::StoreId) -> std::result::Result<url::Url, String> {
    ws_endpoint(base_url, &format!("/api/v1/relay/{store_id}"))
}

/// The accounts service's JWKS, which a relay face verifies members' tokens
/// against, as the hosted server does.
pub fn jwks_url(base_url: &str) -> std::result::Result<url::Url, String> {
    endpoint(base_url, "/api/v1/.well-known/jwks.json").parse().map_err(|e| format!("{base_url} is not a URL: {e}"))
}

pub async fn list_stores(http: &dyn Http, base_url: &str, session: &str) -> Result<Vec<StoreView>> {
    get(http, &endpoint(base_url, "/api/v1/stores"), Some(session)).await
}

// ── GET/PUT /stores/{id}/keys ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_relay_is_reached_over_a_websocket_on_the_accounts_services_origin() {
        let store = pimble_core::StoreId::new();
        assert_eq!(relay_tunnel_url("https://pimble.app").unwrap().as_str(), "wss://pimble.app/api/v1/relay");
        assert_eq!(relay_tunnel_url("http://127.0.0.1:18090/").unwrap().as_str(), "ws://127.0.0.1:18090/api/v1/relay");
        assert_eq!(relay_member_url("https://pimble.app/", store).unwrap().as_str(), format!("wss://pimble.app/api/v1/relay/{store}"));
        assert_eq!(jwks_url("https://pimble.app").unwrap().as_str(), "https://pimble.app/api/v1/.well-known/jwks.json");
        assert!(relay_tunnel_url("ftp://pimble.app").is_err());
    }

    /// A row from before tiers is a hosted one.
    #[test]
    fn a_store_row_without_a_tier_is_hosted() {
        let row: StoreView = serde_json::from_value(serde_json::json!({ "store_id": "s", "name": "n", "role": "owner", "kind": "vault", "created_at": "" })).unwrap();
        assert!(!row.is_relayed());
        let row: StoreView = serde_json::from_value(serde_json::json!({ "store_id": "s", "name": "", "role": "owner", "kind": "vault", "tier": "relay", "created_at": "" })).unwrap();
        assert!(row.is_relayed());
    }
}
