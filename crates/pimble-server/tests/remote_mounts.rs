//! End-to-end tests for remote mounts (docs/history/REMOTE_MOUNTS_CONTRACT.md):
//! a mount whose source store is not on this server resolves by replicating
//! that source and then resolving locally. Like `sync.rs`, these run two or
//! three real `PimbleServer`s on `127.0.0.1:0` in one process and drive
//! them through `PimbleClient` over real WebSocket connections — the
//! resolution under test creates a replica, which is a sync link, which is
//! a `PimbleClient`.
//!
//! Every server gets its own temp replicas and credentials directories
//! (`ServerConfig::replicas_dir`, `ServerConfig::credentials_path`), so
//! nothing here writes into the real data or config directory and no test
//! depends on `XDG_DATA_HOME`.
//!
//! One thing these tests have to undo deliberately: two servers in one test
//! process share a filesystem, which two machines do not. A mount ref
//! records `source_path`, and `StoreManager::ensure_store_open` tries that
//! path before any remote — so the "other" server would just open the first
//! server's own store directory and the remote resolution under test would
//! never run. [`drop_source_path`] removes the hint before the mount ref
//! replicates, putting the resolving server where a second machine really
//! is: the remote is all it has.

use std::path::{Path, PathBuf};
use std::time::Duration;

use jsonrpsee::core::client::Subscription;
use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, MountRef, MountState, NodeId, RemoteEndpoint, StoreId, SyncState};
use pimble_rpc::{StoreChangeKind, StoreChangedNotification};
use pimble_server::{PimbleServer, ServerConfig};
use url::Url;

/// Poll `cond` every 50ms until it returns `true` or `timeout` elapses.
/// Returns whether it converged in time.
async fn wait_until<F, Fut>(timeout: Duration, mut cond: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if cond().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A running server, a client connected to it, and the temp directories it
/// keeps replicas and saved credentials in (bound here so they outlive the
/// server).
struct Server {
    server: PimbleServer,
    client: PimbleClient,
    replicas_dir: PathBuf,
    _replicas: tempfile::TempDir,
    _credentials: tempfile::TempDir,
}

impl Server {
    /// This server's URL, as a remote would name it.
    fn url(&self) -> Url {
        format!("http://{}", self.server.addr()).parse().unwrap()
    }

    /// An endpoint pointing at this server with no credential, the shape
    /// every `sync.json` and every `MountRef::source_remote` has.
    fn endpoint(&self) -> RemoteEndpoint {
        RemoteEndpoint { url: self.url(), auth: AuthMethod::None }
    }
}

/// Start a server on an OS-assigned loopback port, with `token` required if
/// given, and connect a client that presents it.
async fn start_server(token: Option<&str>) -> Server {
    let replicas = tempfile::tempdir().unwrap();
    let credentials = tempfile::tempdir().unwrap();
    let mut server = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        auth_token: token.map(String::from),
        credentials_path: Some(credentials.path().join("credentials.json")),
        replicas_dir: Some(replicas.path().to_path_buf()),
    });
    server.start().await.expect("server starts");

    let auth = match token {
        Some(token) => AuthMethod::Bearer { token: token.to_string() },
        None => AuthMethod::None,
    };
    let client = PimbleClient::connect_with_auth(format!("http://{}", server.addr()), &auth)
        .await
        .expect("client connects");

    Server {
        replicas_dir: replicas.path().to_path_buf(),
        server,
        client,
        _replicas: replicas,
        _credentials: credentials,
    }
}

/// Replace a mount node's `MountRef`, in the store that holds it, by
/// rewriting the `mount_ref` custom metadata field the node carries.
async fn repoint_mount(client: &PimbleClient, store_id: StoreId, mount_node: NodeId, mount_ref: &MountRef) {
    let node = client.get_node(store_id, mount_node).await.expect("the mount node exists");
    let mut metadata = node.metadata.clone();
    metadata
        .custom
        .insert("mount_ref".to_string(), serde_json::to_value(mount_ref).unwrap());
    client
        .update_node_metadata(store_id, mount_node, metadata)
        .await
        .expect("the mount node's metadata updates");
}

/// Drop a mount's `source_path` hint (see the module doc for why these
/// tests have to), returning the mount ref as it now reads.
async fn drop_source_path(client: &PimbleClient, store_id: StoreId, mount_node: NodeId) -> MountRef {
    let node = client.get_node(store_id, mount_node).await.expect("the mount node exists");
    let mut mount_ref = node.mount_ref().expect("a mount node carries a mount ref");
    mount_ref.source_path = None;
    repoint_mount(client, store_id, mount_node, &mount_ref).await;
    mount_ref
}

/// Wait for a `MountStateChanged` notification for `node_id` whose state
/// `want` accepts, up to `timeout`, returning it. Every other notification
/// on the subscription (the tree updates a sync link produces, above all)
/// is skipped.
async fn wait_for_mount_state<F>(
    sub: &mut Subscription<StoreChangedNotification>,
    node_id: NodeId,
    timeout: Duration,
    want: F,
) -> Option<MountState>
where
    F: Fn(&MountState) -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, sub.next()).await {
            Ok(Some(Ok(notification))) => {
                if let StoreChangeKind::MountStateChanged { node_id: changed, state } = notification.change_kind {
                    if changed == node_id && want(&state) {
                        return Some(state);
                    }
                }
            }
            Ok(Some(Err(_))) | Ok(None) | Err(_) => return None,
        }
    }
}

/// The children a mount resolves to, or an empty list while it cannot be
/// resolved (`getChildren` on an unresolved mount is an error by decision
/// 7, which is exactly what a `wait_until` wants to keep polling through).
async fn mount_children(client: &PimbleClient, store_id: StoreId, mount_node: NodeId) -> (Option<StoreId>, Vec<NodeId>) {
    match client.get_children(store_id, mount_node).await {
        Ok((children_store, children)) => (Some(children_store), children.into_iter().map(|c| c.id).collect()),
        Err(_) => (None, Vec::new()),
    }
}

/// A URL nothing listens on: a loopback port bound just long enough to
/// learn it is free, then released.
fn dead_url() -> Url {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{}", addr).parse().unwrap()
}

/// R holds store B (one document under its root). L holds a replica of B
/// and a local store A whose root holds a mount of B's root.
struct Fixture {
    r: Server,
    l: Server,
    b_id: StoreId,
    b_root: NodeId,
    b_doc: NodeId,
    a_id: StoreId,
    a_root: NodeId,
    mount_node: NodeId,
    mount_ref: MountRef,
    b_replica_path: PathBuf,
    _r_dir: tempfile::TempDir,
    _l_dir: tempfile::TempDir,
}

async fn setup_fixture() -> Fixture {
    let r = start_server(None).await;
    let r_dir = tempfile::tempdir().unwrap();
    let (b_id, b_root) = r.client.create_store(r_dir.path().join("b.pimble"), "B Store").await.unwrap();
    let b_doc = r.client.create_node(b_id, Some(b_root), "document", "B Doc").await.unwrap();

    let l = start_server(None).await;
    let b_replica = l
        .client
        .add_remote_store(r.endpoint(), b_id, None)
        .await
        .expect("L adds B from R as a replica");
    let b_replica_path = b_replica.local_path().cloned().expect("a replica has a local path");

    let l_dir = tempfile::tempdir().unwrap();
    let (a_id, a_root) = l.client.create_store(l_dir.path().join("a.pimble"), "A Store").await.unwrap();
    let (mount_node, mount_ref) = l
        .client
        .create_mount(a_id, a_root, b_id, b_root, Some("B Store".to_string()))
        .await
        .expect("mounting B's root into A succeeds");

    Fixture {
        r,
        l,
        b_id,
        b_root,
        b_doc,
        a_id,
        a_root,
        mount_node,
        mount_ref,
        b_replica_path,
        _r_dir: r_dir,
        _l_dir: l_dir,
    }
}

/// R holds stores A and B, both ordinary local stores, and A's root holds a
/// mount of B's root whose `source_path` hint has been dropped. Returns R,
/// the ids, and the directory both stores live in (bound by the caller so
/// it can restart a server over the same directories).
async fn setup_two_local_stores_on_one_server(
    token: Option<&str>,
) -> (Server, tempfile::TempDir, StoreId, NodeId, NodeId, StoreId, NodeId, NodeId) {
    let r = start_server(token).await;
    let dir = tempfile::tempdir().unwrap();
    let (b_id, b_root) = r.client.create_store(dir.path().join("b.pimble"), "B Store").await.unwrap();
    let b_doc = r.client.create_node(b_id, Some(b_root), "document", "B Doc").await.unwrap();
    let (a_id, a_root) = r.client.create_store(dir.path().join("a.pimble"), "A Store").await.unwrap();
    let (mount_node, mount_ref) = r
        .client
        .create_mount(a_id, a_root, b_id, b_root, Some("B Store".to_string()))
        .await
        .unwrap();
    // B is an ordinary store on R, not a replica of anything, so nothing
    // fills `source_remote` (decision 2); the mounting store's own remote
    // is what resolves this mount elsewhere (decision 1).
    assert!(mount_ref.source_remote.is_none(), "B is not a replica on R, so the mount names no remote");
    drop_source_path(&r.client, a_id, mount_node).await;

    (r, dir, a_id, a_root, mount_node, b_id, b_root, b_doc)
}

// ── 1. `createMount` records `source_remote` ─────────────────────────

#[tokio::test]
async fn create_mount_records_the_sources_remote() {
    let f = setup_fixture().await;

    assert_eq!(
        f.mount_ref.source_remote,
        Some(f.r.url()),
        "the source is a replica of R, so the mount ref must name R"
    );
    assert_eq!(
        f.mount_ref.source_path.as_deref(),
        Some(f.b_replica_path.as_path()),
        "the mount ref must also keep the source's local path"
    );
    assert_eq!(f.mount_ref.source_store, f.b_id);
    assert_eq!(f.mount_ref.source_node, f.b_root);

    let live = wait_until(Duration::from_secs(5), || async {
        matches!(f.l.client.get_mount_state(f.a_id, f.mount_node).await, Ok((MountState::Live, _)))
    })
    .await;
    assert!(live, "a mount of an open, synced source is Live");
}

// ── 2. Resolution through `source_remote` ────────────────────────────

#[tokio::test]
async fn a_mount_resolves_through_its_source_remote() {
    let f = setup_fixture().await;
    drop_source_path(&f.l.client, f.a_id, f.mount_node).await;

    let m = start_server(None).await;
    m.client
        .add_remote_store(f.l.endpoint(), f.a_id, None)
        .await
        .expect("M adds A from L as a replica");

    // The mount node itself has to replicate before it can be resolved.
    assert!(
        wait_until(Duration::from_secs(10), || async {
            m.client
                .get_children(f.a_id, f.a_root)
                .await
                .map(|(_, children)| children.iter().any(|c| c.id == f.mount_node))
                .unwrap_or(false)
        })
        .await,
        "A's mount node should reach M"
    );

    let mut sub = m.client.subscribe_store_changes(f.a_id).await.unwrap();

    let (state, mount_ref) = m.client.get_mount_state(f.a_id, f.mount_node).await.unwrap();
    assert!(matches!(state, MountState::Connecting), "the first resolution starts the replica: {:?}", state);
    assert_eq!(mount_ref.source_remote, Some(f.r.url()));

    assert!(
        wait_until(Duration::from_secs(10), || async {
            matches!(m.client.get_mount_state(f.a_id, f.mount_node).await, Ok((MountState::Live, _)))
        })
        .await,
        "the mount should go Live once its source has been replicated"
    );

    let stores = m.client.list_stores().await.unwrap();
    let b = stores.iter().find(|s| s.id == f.b_id).expect("M should now hold B");
    assert!(b.is_replica, "the source M created for the mount is a replica");

    assert!(
        wait_until(Duration::from_secs(10), || async {
            let (children_store, children) = mount_children(&m.client, f.a_id, f.mount_node).await;
            children_store == Some(f.b_id) && children.contains(&f.b_doc)
        })
        .await,
        "getChildren on the mount should return B's children, addressed by B"
    );

    assert!(
        wait_for_mount_state(&mut sub, f.mount_node, Duration::from_secs(10), |s| matches!(s, MountState::Live))
            .await
            .is_some(),
        "a MountStateChanged Live should have arrived on A's subscription"
    );
}

// ── 3. Resolution through the mounting store's own remote ────────────

#[tokio::test]
async fn a_mount_resolves_through_the_mounting_stores_remote() {
    let (r, _r_dir, a_id, a_root, mount_node, b_id, _b_root, b_doc) =
        setup_two_local_stores_on_one_server(None).await;

    let l = start_server(None).await;
    l.client.add_remote_store(r.endpoint(), a_id, None).await.expect("L adds A from R");

    assert!(
        wait_until(Duration::from_secs(10), || async {
            l.client
                .get_children(a_id, a_root)
                .await
                .map(|(_, children)| children.iter().any(|c| c.id == mount_node))
                .unwrap_or(false)
        })
        .await,
        "A's mount node should reach L"
    );

    let mut sub = l.client.subscribe_store_changes(a_id).await.unwrap();

    let (state, _) = l.client.get_mount_state(a_id, mount_node).await.unwrap();
    assert!(matches!(state, MountState::Connecting), "the first resolution starts the replica: {:?}", state);

    assert!(
        wait_until(Duration::from_secs(10), || async {
            matches!(l.client.get_mount_state(a_id, mount_node).await, Ok((MountState::Live, _)))
        })
        .await,
        "the mount should go Live once B has been replicated from A's own remote"
    );

    let stores = l.client.list_stores().await.unwrap();
    let b = stores.iter().find(|s| s.id == b_id).expect("L should now hold B");
    assert!(b.is_replica);

    assert!(
        wait_until(Duration::from_secs(10), || async {
            let (children_store, children) = mount_children(&l.client, a_id, mount_node).await;
            children_store == Some(b_id) && children.contains(&b_doc)
        })
        .await,
        "getChildren on the mount should return B's children, addressed by B"
    );

    assert!(
        wait_for_mount_state(&mut sub, mount_node, Duration::from_secs(10), |s| matches!(s, MountState::Live))
            .await
            .is_some(),
        "a MountStateChanged Live should have arrived on A's subscription"
    );
}

// ── 4. Cached ────────────────────────────────────────────────────────

#[tokio::test]
async fn a_mount_whose_source_link_is_down_is_cached_and_still_readable() {
    let (mut r, r_dir, a_id, _a_root, mount_node, b_id, _b_root, b_doc) =
        setup_two_local_stores_on_one_server(None).await;
    let a_path = r_dir.path().join("a.pimble");
    let b_path = r_dir.path().join("b.pimble");

    let l = start_server(None).await;
    l.client.add_remote_store(r.endpoint(), a_id, None).await.unwrap();
    assert!(
        wait_until(Duration::from_secs(15), || async {
            matches!(l.client.get_mount_state(a_id, mount_node).await, Ok((MountState::Live, _)))
        })
        .await,
        "the mount should be Live before the remote goes away"
    );

    let mut sub = l.client.subscribe_store_changes(a_id).await.unwrap();
    r.server.stop().await.expect("R stops");

    let cached = wait_for_mount_state(&mut sub, mount_node, Duration::from_secs(10), |s| {
        matches!(s, MountState::Cached { .. })
    })
    .await;
    assert!(cached.is_some(), "a MountStateChanged Cached should arrive once B's link notices R is gone");

    let (state, _) = l.client.get_mount_state(a_id, mount_node).await.unwrap();
    assert!(matches!(state, MountState::Cached { .. }), "expected Cached, got {:?}", state);

    let (children_store, children) = mount_children(&l.client, a_id, mount_node).await;
    assert_eq!(children_store, Some(b_id), "the offline copy still answers getChildren");
    assert!(children.contains(&b_doc), "the offline copy still has B's children");

    // The same stores, a new server, a new port: relinking B's replica to
    // it brings the mount back.
    let r2 = start_server(None).await;
    r2.client.open_store(&b_path).await.expect("R2 serves B's directory");
    r2.client.open_store(&a_path).await.expect("R2 serves A's directory");
    l.client.set_store_sync(b_id, Some(r2.endpoint())).await.expect("B's replica relinks to R2");

    assert!(
        wait_until(Duration::from_secs(10), || async {
            matches!(l.client.get_mount_state(a_id, mount_node).await, Ok((MountState::Live, _)))
        })
        .await,
        "the mount should be Live again once its source's link reconnects"
    );
}

// ── 5. Unavailable with a reason ─────────────────────────────────────

#[tokio::test]
async fn a_mount_nothing_can_reach_reports_why() {
    let l = start_server(None).await;
    let dir = tempfile::tempdir().unwrap();
    let (s_id, s_root) = l.client.create_store(dir.path().join("s.pimble"), "S Store").await.unwrap();
    let (a_id, a_root) = l.client.create_store(dir.path().join("a.pimble"), "A Store").await.unwrap();
    let (mount_node, _) = l.client.create_mount(a_id, a_root, s_id, s_root, None).await.unwrap();

    // Repoint the mount at a store nobody has, on a port nothing listens
    // on, with no path hint: every resolution step must fail.
    let dead = dead_url();
    let unreachable = MountRef {
        source_store: StoreId::new(),
        source_node: NodeId::new(),
        source_path: None,
        source_remote: Some(dead.clone()),
    };
    repoint_mount(&l.client, a_id, mount_node, &unreachable).await;

    let mut sub = l.client.subscribe_store_changes(a_id).await.unwrap();

    let (state, _) = l.client.get_mount_state(a_id, mount_node).await.unwrap();
    assert!(matches!(state, MountState::Connecting), "the first resolution tries the remote: {:?}", state);

    let unavailable = wait_for_mount_state(&mut sub, mount_node, Duration::from_secs(10), |s| {
        matches!(s, MountState::Unavailable { .. })
    })
    .await
    .expect("a MountStateChanged Unavailable should arrive once the attempt fails");
    let MountState::Unavailable { reason } = unavailable else { unreachable!() };
    let reason = reason.expect("the reason says why, not just that");
    let host = dead.as_str().trim_end_matches('/');
    assert!(reason.contains(host), "the reason should name the URL it could not reach, got: {}", reason);

    // Resolution always retries (decision 3's "the next resolution attempt
    // retries"), so asking again starts a fresh attempt rather than
    // repeating the verdict.
    let (state, _) = l.client.get_mount_state(a_id, mount_node).await.unwrap();
    assert!(matches!(state, MountState::Connecting), "a second getMountState tries again: {:?}", state);
}

// ── 6. `last_sync` survives a restart ────────────────────────────────

#[tokio::test]
async fn last_sync_persists_so_a_reopened_source_is_cached_not_connecting() {
    let mut f = setup_fixture().await;

    // `sync.json` holds the link's `last_sync` and never a credential.
    let sync_json: serde_json::Value = wait_until(Duration::from_secs(10), || async {
        std::fs::read_to_string(f.b_replica_path.join("sync.json"))
            .ok()
            .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
            .map(|v| v.get("last_sync").is_some())
            .unwrap_or(false)
    })
    .await
    .then(|| {
        let json = std::fs::read_to_string(f.b_replica_path.join("sync.json")).unwrap();
        serde_json::from_str(&json).unwrap()
    })
    .expect("sync.json should record last_sync once the link has synced");

    assert_eq!(sync_json["remote"]["auth"]["method"], "none", "sync.json never stores a credential");
    let persisted: chrono::DateTime<chrono::Utc> = sync_json["last_sync"]
        .as_str()
        .expect("last_sync is a timestamp string")
        .parse()
        .expect("last_sync parses");

    f.l.client.close_store(f.b_id).await.expect("the replica closes");
    f.r.server.stop().await.expect("R stops");
    f.l.client.open_store(&f.b_replica_path).await.expect("the replica reopens");

    assert!(
        wait_until(Duration::from_secs(10), || async {
            matches!(f.l.client.get_store_sync(f.b_id).await, Ok((_, SyncState::Offline)))
        })
        .await,
        "with the remote down, the restarted link should end up Offline"
    );

    let (state, _) = f.l.client.get_mount_state(f.a_id, f.mount_node).await.unwrap();
    match state {
        MountState::Cached { last_sync } => assert_eq!(last_sync, persisted, "Cached should carry the persisted time"),
        other => panic!("expected Cached with the persisted last_sync, got {:?}", other),
    }
}

// ── 7. Credentials ───────────────────────────────────────────────────

#[tokio::test]
async fn resolution_uses_saved_credentials_and_says_when_they_are_missing() {
    let (r, _r_dir, a_id, a_root, mount_node, b_id, _b_root, _b_doc) =
        setup_two_local_stores_on_one_server(Some("r-secret")).await;

    let l = start_server(None).await;
    // Adding A with the token is what saves it for R's origin; nothing
    // after this ever sends a credential in a request.
    l.client
        .add_remote_store(
            RemoteEndpoint { url: r.url(), auth: AuthMethod::Bearer { token: "r-secret".into() } },
            a_id,
            None,
        )
        .await
        .expect("L adds A from R with the token");

    assert!(
        wait_until(Duration::from_secs(15), || async {
            matches!(l.client.get_mount_state(a_id, mount_node).await, Ok((MountState::Live, _)))
        })
        .await,
        "the mount should resolve against the token-protected R using the saved credential"
    );
    let stores = l.client.list_stores().await.unwrap();
    assert!(stores.iter().any(|s| s.id == b_id && s.is_replica), "B should have been replicated to L");

    // A second token-protected server L has never authenticated to.
    let t = start_server(Some("t-secret")).await;
    let dir = tempfile::tempdir().unwrap();
    let (s_id, s_root) = l.client.create_store(dir.path().join("s.pimble"), "S Store").await.unwrap();
    let (c_id, c_root) = l.client.create_store(dir.path().join("c.pimble"), "C Store").await.unwrap();
    let (stranger, _) = l.client.create_mount(c_id, c_root, s_id, s_root, None).await.unwrap();
    repoint_mount(
        &l.client,
        c_id,
        stranger,
        &MountRef {
            source_store: StoreId::new(),
            source_node: NodeId::new(),
            source_path: None,
            source_remote: Some(t.url()),
        },
    )
    .await;

    let mut sub = l.client.subscribe_store_changes(c_id).await.unwrap();
    let (state, _) = l.client.get_mount_state(c_id, stranger).await.unwrap();
    assert!(matches!(state, MountState::Connecting), "{:?}", state);

    let unavailable = wait_for_mount_state(&mut sub, stranger, Duration::from_secs(10), |s| {
        matches!(s, MountState::Unavailable { .. })
    })
    .await
    .expect("a mount naming a server L has no credential for goes Unavailable");
    let MountState::Unavailable { reason } = unavailable else { unreachable!() };
    let reason = reason.unwrap_or_default();
    assert!(
        reason.contains("refused the credentials"),
        "the reason should say the remote refused the credentials, got: {}",
        reason
    );

    let _ = a_root;
}

// ── 8. No duplicate replica ──────────────────────────────────────────

#[tokio::test]
async fn two_mounts_of_one_source_produce_one_replica() {
    let (r, _r_dir, a_id, a_root, first_mount, b_id, b_root, _b_doc) =
        setup_two_local_stores_on_one_server(None).await;
    let (second_mount, _) = r
        .client
        .create_mount(a_id, a_root, b_id, b_root, Some("B again".to_string()))
        .await
        .unwrap();
    drop_source_path(&r.client, a_id, second_mount).await;

    let l = start_server(None).await;
    l.client.add_remote_store(r.endpoint(), a_id, None).await.unwrap();
    assert!(
        wait_until(Duration::from_secs(10), || async {
            l.client
                .get_children(a_id, a_root)
                .await
                .map(|(_, children)| {
                    children.iter().any(|c| c.id == first_mount) && children.iter().any(|c| c.id == second_mount)
                })
                .unwrap_or(false)
        })
        .await,
        "both mount nodes should reach L"
    );

    let (first, second) = tokio::join!(
        l.client.get_mount_state(a_id, first_mount),
        l.client.get_mount_state(a_id, second_mount),
    );
    assert!(matches!(first.unwrap().0, MountState::Connecting));
    assert!(matches!(second.unwrap().0, MountState::Connecting));

    assert!(
        wait_until(Duration::from_secs(10), || async {
            matches!(l.client.get_mount_state(a_id, first_mount).await, Ok((MountState::Live, _)))
                && matches!(l.client.get_mount_state(a_id, second_mount).await, Ok((MountState::Live, _)))
        })
        .await,
        "both mounts should end up Live"
    );

    let stores = l.client.list_stores().await.unwrap();
    assert_eq!(
        stores.iter().filter(|s| s.id == b_id).count(),
        1,
        "two concurrent resolutions of one source must produce one replica"
    );
    assert_eq!(
        replica_dirs_for(&l.replicas_dir, b_id),
        1,
        "and one directory for it"
    );
}

/// How many directories in `replicas_dir` belong to `store_id`.
fn replica_dirs_for(replicas_dir: &Path, store_id: StoreId) -> usize {
    let wanted = format!("{}.pimble", store_id);
    std::fs::read_dir(replicas_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy() == wanted)
                .count()
        })
        .unwrap_or(0)
}

// ── 10. Extra (found in the GUI pass): an implicitly opened replica links ──

/// A mount whose source is a replica this server holds on disk but does not
/// have open (after a restart, or after the replica was closed) resolves by
/// opening it from its `source_path`. That implicit open must start the
/// replica's sync link exactly as `openStore` would; otherwise the replica
/// sits `Offline` forever while the mount reads `Live`, since a source with
/// no link is `Live` by definition (decision 3).
#[tokio::test]
async fn resolving_a_mount_through_a_closed_replica_starts_its_link() {
    let f = setup_fixture().await;
    assert!(
        wait_until(Duration::from_secs(5), || async {
            matches!(f.l.client.get_store_sync(f.b_id).await, Ok((_, SyncState::Synced { .. })))
        })
        .await,
        "the replica links before we close it"
    );

    f.l.client.close_store(f.b_id).await.expect("the replica closes");
    assert!(
        !f.l.client.list_stores().await.unwrap().iter().any(|s| s.id == f.b_id),
        "closed means not open"
    );

    let (state, _) = f.l.client.get_mount_state(f.a_id, f.mount_node).await.unwrap();
    assert!(
        !matches!(state, MountState::Unavailable { .. }),
        "the replica on disk resolves the mount: {:?}",
        state
    );
    assert!(
        wait_until(Duration::from_secs(5), || async {
            matches!(f.l.client.get_store_sync(f.b_id).await, Ok((_, SyncState::Synced { .. })))
                && matches!(f.l.client.get_mount_state(f.a_id, f.mount_node).await, Ok((MountState::Live, _)))
        })
        .await,
        "the implicitly reopened replica must link again and the mount go Live"
    );
}

// ── 9. Extra (not in the contract's list): a deleted mount stops being
// notified about ────────────────────────────────────────────────────

/// Agent B found the app discarding `MountStateChanged` for a mount node it
/// had deleted, which it had to recognise and ignore to stop refetching
/// children that no longer exist. The contract tolerates the stale entry;
/// not sending it is cheaper for every client. A surviving mount of the
/// same source is the control: it must still be notified.
#[tokio::test]
async fn a_deleted_mount_is_no_longer_notified_about() {
    let f = setup_fixture().await;
    let doomed = f.mount_node;
    let (survivor, _) = f
        .l
        .client
        .create_mount(f.a_id, f.a_root, f.b_id, f.b_root, Some("B again".to_string()))
        .await
        .unwrap();

    // Both are recorded against B by their own resolution.
    for mount in [doomed, survivor] {
        assert!(matches!(f.l.client.get_mount_state(f.a_id, mount).await, Ok((MountState::Live, _))));
    }

    f.l.client.delete_node(f.a_id, doomed).await.expect("the mount node is deleted");

    let mut sub = f.l.client.subscribe_store_changes(f.a_id).await.unwrap();
    // Unlinking B is a link category transition, which is what fans mount
    // states out — with no remote to wait for.
    f.l.client.set_store_sync(f.b_id, None).await.expect("B's replica unlinks");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut saw_survivor = false;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, sub.next()).await {
            Ok(Some(Ok(notification))) => {
                if let StoreChangeKind::MountStateChanged { node_id, .. } = notification.change_kind {
                    assert_ne!(node_id, doomed, "a deleted mount must not be notified about");
                    if node_id == survivor {
                        saw_survivor = true;
                    }
                }
            }
            Ok(Some(Err(_))) | Ok(None) | Err(_) => break,
        }
    }
    assert!(saw_survivor, "the surviving mount of the same source must still be notified");
}
