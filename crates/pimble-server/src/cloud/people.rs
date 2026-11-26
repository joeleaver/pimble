//! Names and shares (docs/PRESENCE_CONTRACT.md section 3): `GET`/`PUT
//! /me`, `GET /me/shares`, `PUT /me/shares/{store}`.

use serde::{Deserialize, Serialize};

use super::stores::default_tier;
use super::{endpoint, get, put, Http, Result};

/// `GET /api/v1/me`.
#[derive(Debug, Clone, Deserialize)]
pub struct MeView {
    pub id: String,
    pub email: String,
    /// The default name chosen, `""` for none.
    #[serde(default)]
    pub display_name: String,
    /// What the account is called where nothing more particular applies.
    #[serde(default)]
    pub name: String,
    /// Whether notifications are on for the account
    /// (docs/NOTIFICATIONS_CONTRACT.md section 6); a service from before
    /// them says nothing, and they are on.
    #[serde(default = "on")]
    pub notifications: bool,
    /// Wave 2: the email fallback; on unless turned off.
    #[serde(default = "on")]
    pub notification_email: bool,
    /// Wave 2: web push; on unless turned off.
    #[serde(default = "on")]
    pub notification_push: bool,
    /// Wave 2: the service's VAPID public key, none when it sends no pushes.
    #[serde(default)]
    pub push_key: Option<String>,
    /// Wave 3: watched notes; on unless turned off.
    #[serde(default = "on")]
    pub notification_watches: bool,
}

fn on() -> bool {
    true
}

pub async fn me(http: &dyn Http, base_url: &str, session: &str) -> Result<MeView> {
    get(http, &endpoint(base_url, "/api/v1/me"), Some(session)).await
}

pub async fn put_display_name(http: &dyn Http, base_url: &str, session: &str, display_name: &str) -> Result<()> {
    let _: serde_json::Value = put(http, &endpoint(base_url, "/api/v1/me"), session, &serde_json::json!({ "display_name": display_name })).await?;
    Ok(())
}

/// One row of `GET /api/v1/me/shares`: a share the account takes part in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareView {
    pub store_id: String,
    #[serde(default)]
    pub root: Option<String>,
    pub name: String,
    pub role: String,
    pub owned: bool,
    #[serde(default)]
    pub owner_name: Option<String>,
    pub kind: String,
    #[serde(default = "default_tier")]
    pub tier: String,
    /// The name chosen for this share, `""` for none.
    #[serde(default)]
    pub my_name: String,
    pub appears_as: String,
}

impl ShareView {
    pub fn scope_root(&self) -> Option<pimble_core::NodeId> {
        self.root.as_deref().and_then(|r| pimble_core::NodeId::parse(r).ok())
    }
}

pub async fn list_shares(http: &dyn Http, base_url: &str, session: &str) -> Result<Vec<ShareView>> {
    get(http, &endpoint(base_url, "/api/v1/me/shares"), Some(session)).await
}

pub async fn put_share_name(http: &dyn Http, base_url: &str, session: &str, store_id: &str, root: Option<&pimble_core::NodeId>, name: &str) -> Result<()> {
    let body = serde_json::json!({ "root": root.map(|r| r.to_string()), "name": name });
    let _: serde_json::Value = put(http, &endpoint(base_url, &format!("/api/v1/me/shares/{store_id}")), session, &body).await?;
    Ok(())
}
