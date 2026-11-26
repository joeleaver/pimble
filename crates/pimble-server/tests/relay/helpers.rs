//! Small generic helpers, devices and the JWTs the stub signs.

use super::*;

// ── Small generic helpers (mirrors tests/share.rs) ────────────────────────

pub(crate) async fn node_text(client: &PimbleClient, store_id: StoreId, node_id: NodeId) -> String {
    match client.get_node(store_id, node_id).await {
        Ok(node) => NodeDoc::text_of(&node.content),
        Err(_) => String::new(),
    }
}

pub(crate) async fn node_title(client: &PimbleClient, store_id: StoreId, node_id: NodeId) -> String {
    client.get_node(store_id, node_id).await.map(|node| node.metadata.title).unwrap_or_default()
}

pub(crate) async fn child_ids(client: &PimbleClient, store_id: StoreId, node_id: NodeId) -> Vec<NodeId> {
    client.get_children(store_id, node_id).await.map(|(_, children)| children.into_iter().map(|n| n.id).collect()).unwrap_or_default()
}

pub(crate) async fn rename(client: &PimbleClient, store_id: StoreId, node_id: NodeId, title: &str) {
    client.edit_node(store_id, node_id, "rename", |doc, now| doc.set_title(title, now)).await.expect("rename");
}

/// Text typed into a node: a fresh document's state merged in, which reads
/// as another paragraph.
pub(crate) async fn write_text(client: &PimbleClient, store_id: StoreId, node_id: NodeId, client_id: &str, text: &str) {
    let doc = NodeDoc::from_plain_text(text).unwrap();
    let changes = base64::engine::general_purpose::STANDARD.encode(doc.save());
    client.apply_edit(store_id, node_id, client_id, EditOperation::IncrementalChanges { changes }).await.expect("the text applies");
}

pub(crate) fn sorted(ids: Vec<NodeId>) -> Vec<String> {
    let mut out: Vec<String> = ids.into_iter().map(|id| id.to_string()).collect();
    out.sort();
    out
}

pub(crate) fn contains_bytes_recursive(dir: &Path, needle: &[u8]) -> bool {
    let Ok(entries) = tfs::read_dir(dir) else { return false };
    for path in entries {
        if tfs::is_dir(&path) {
            if contains_bytes_recursive(&path, needle) {
                return true;
            }
        } else if let Ok(bytes) = tfs::read(&path) {
            if bytes.windows(needle.len()).any(|w| w == needle) {
                return true;
            }
        }
    }
    false
}

/// How often a test device's key sweep runs, in place of the minute.
pub(crate) const SWEEP_EVERY: Duration = Duration::from_secs(1);

/// Every path a device writes to is inside its own temp directory: the
/// keystore, the credentials, the replicas, and the twins of what it shares
/// from "this computer".
pub(crate) fn device_config(dir: &Path) -> ServerConfig {
    ServerConfig { share_sweep_interval: Some(SWEEP_EVERY), ..ServerConfig::in_dir(dir) }
}

pub(crate) async fn start_local_server() -> (PimbleServer, PimbleClient, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let (server, client) = start_device_in(dir.path()).await;
    (server, client, dir)
}

/// A device started (or started again) over `dir`.
pub(crate) async fn start_device_in(dir: &Path) -> (PimbleServer, PimbleClient) {
    let mut server = PimbleServer::with_config(device_config(dir));
    server.start().await.expect("local server starts");
    let client = PimbleClient::connect(format!("http://{}", server.addr())).await.expect("client connects");
    (server, client)
}

pub(crate) fn twin_dir(device_dir: &Path, store_id: StoreId) -> PathBuf {
    device_dir.join("relay").join(format!("{store_id}.pimble"))
}

/// The twin's epoch as it is on disk: its manifest's creation time.
pub(crate) fn twin_created_at(device_dir: &Path, store_id: StoreId) -> Option<String> {
    let manifest = tfs::read(twin_dir(device_dir, store_id).join("manifest.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_slice(&manifest).ok()?;
    manifest["created_at"].as_str().map(str::to_string)
}

/// How many documents the twin holds a log of.
pub(crate) fn twin_doc_count(device_dir: &Path, store_id: StoreId) -> usize {
    tfs::read_dir(twin_dir(device_dir, store_id).join("vault")).map(|entries| entries.iter().filter(|p| tfs::is_dir(p)).count()).unwrap_or(0)
}

// ── JWTs ──────────────────────────────────────────────────────────────────

pub(crate) fn fresh_signing_key() -> SigningKey {
    use rand::RngCore;
    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);
    SigningKey::from_bytes(&seed)
}

pub(crate) fn make_jwt(signing_key: &SigningKey, kid: &str, issuer: &str, sub: &str, email: &str, stores_claim: &serde_json::Map<String, serde_json::Value>) -> String {
    let header = json!({ "alg": "EdDSA", "kid": kid });
    let payload = json!({
        "iss": issuer,
        "sub": sub,
        "aud": "pimble",
        "exp": chrono::Utc::now().timestamp() + 300,
        "claims": { "email": email, "stores": stores_claim },
    });
    let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
    let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
    let signing_input = format!("{header_b64}.{payload_b64}");
    let signature = signing_key.sign(signing_input.as_bytes());
    format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
}

/// The store ids a token's `stores` claim names, if its signature is the
/// stub's own.
pub(crate) fn verified_stores(signing_key: &SigningKey, token: &str) -> Option<Vec<String>> {
    let mut parts = token.split('.');
    let (header, payload, signature) = (parts.next()?, parts.next()?, parts.next()?);
    let signature = ed25519_dalek::Signature::from_slice(&URL_SAFE_NO_PAD.decode(signature).ok()?).ok()?;
    signing_key.verifying_key().verify(format!("{header}.{payload}").as_bytes(), &signature).ok()?;
    let payload: serde_json::Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
    Some(payload["claims"]["stores"].as_object()?.keys().cloned().collect())
}
