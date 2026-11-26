//! The environment (the stub, the relay, `H`) and the fixtures the tests start from.

use super::*;

// ── The environment ───────────────────────────────────────────────────────

pub(crate) fn cheap_kdf_params() -> KdfParams {
    let mut params = KdfParams::generate();
    params.m_cost = 8 * 1024;
    params.t_cost = 1;
    params
}

pub(crate) fn stub_account(email: &str, password: &str) -> StubAccount {
    let kdf = cheap_kdf_params();
    let account_keys = AccountKeys::generate();
    let password_keys = derive_password_keys(password, &kdf).expect("derive password keys");
    let account_key_blob = wrap_account_keys(&account_keys, &password_keys.kek).expect("wrap account keys");
    StubAccount {
        kdf,
        user_id: Uuid::new_v4().to_string(),
        email: email.to_string(),
        session: format!("sess-{}", Uuid::new_v4()),
        account_key_blob,
        account_public_keys: account_keys.public_keys(),
    }
}

pub(crate) const ALICE: (&str, &str) = ("alice@example.com", "correct horse battery staple");
pub(crate) const BOB: (&str, &str) = ("bob@example.com", "a different horse entirely");
pub(crate) const CAROL: (&str, &str) = ("carol@example.com", "a third horse altogether");

pub(crate) struct Env {
    pub(crate) stub_url: String,
    pub(crate) stub_addr: std::net::SocketAddr,
    pub(crate) stub: StubState,
    pub(crate) _h: PimbleServer,
    /// A client to `H` with the service token, to see that it holds nothing.
    pub(crate) h_admin: PimbleClient,
    pub(crate) h_stores_dir: PathBuf,
    pub(crate) h_dir: tempfile::TempDir,
}

impl Env {
    pub(crate) async fn sign_in(&self, client: &PimbleClient, who: (&str, &str)) {
        client.cloud_sign_in(&self.stub_url, who.0, who.1).await.expect("sign in");
    }

    /// `who` holds another store as well (hosted, and nobody's business
    /// here), so the account's general token names two stores, as most
    /// people's does. The relay takes no such token for either.
    pub(crate) fn holds_another_store(&self, who: (&str, &str)) {
        let mut s = self.stub.inner.lock().unwrap();
        let user_id = s.account_by_email(Some(who.0)).user_id.clone();
        s.stores.push(StubStoreRow {
            user_id,
            store_id: Uuid::new_v4().to_string(),
            name: "Something else entirely".into(),
            kind: "vault".into(),
            tier: "hosted".into(),
            created_at: chrono::Utc::now().to_rfc3339(),
            role: "owner".into(),
            root: None,
            shared_by: None,
        });
    }

    /// Where members reach `store_id`: the relay's endpoint for it.
    pub(crate) fn relay_url(&self, store_id: StoreId) -> String {
        format!("ws://{}/api/v1/relay/{}", self.stub_addr, store_id)
    }

    /// `who`'s token for one relayed store, and the account's general one.
    pub(crate) fn tokens(&self, who: (&str, &str), store_id: StoreId) -> (String, String) {
        let s = self.stub.inner.lock().unwrap();
        let account = s.account_by_email(Some(who.0));
        let general = make_jwt(&s.signing_key, &s.kid, &s.issuer, &account.user_id, &account.email, &s.stores_claim(&account.user_id));
        (s.store_token(account, &store_id.to_string()).expect("the account holds a grant on the store"), general)
    }

    /// Nothing of any store is on Pimble Cloud's disk: `H` has no store, its
    /// stores directory no entry, and none of `needles` is anywhere under it.
    pub(crate) async fn assert_nothing_hosted(&self, needles: &[&str]) {
        assert!(self.h_admin.list_stores().await.unwrap().is_empty(), "the hosted server holds no store");
        assert_eq!(tfs::read_dir(&self.h_stores_dir).unwrap().len(), 0, "and its stores directory gained nothing");
        for needle in needles {
            assert!(!contains_bytes_recursive(self.h_dir.path(), needle.as_bytes()), "{needle:?} reached the hosted server's disk");
        }
    }
}

pub(crate) async fn spawn_env() -> Env {
    let signing_key = fresh_signing_key();
    let issuer = "https://cloud.test".to_string();

    let h_dir = tempfile::tempdir().unwrap();
    let h_stores_dir = h_dir.path().join("stores");
    tfs::create_dir_all(&h_stores_dir).unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stub_addr = listener.local_addr().unwrap();

    let state: StubState = Arc::new(Stub {
        inner: Mutex::new(StubInner {
            accounts: vec![stub_account(ALICE.0, ALICE.1), stub_account(BOB.0, BOB.1), stub_account(CAROL.0, CAROL.1)],
            stores: Vec::new(),
            key_grants: HashMap::new(),
            signing_key,
            kid: "test-kid".to_string(),
            issuer: issuer.clone(),
            h_client: None,
            h_rpc_url: String::new(),
            h_stores_dir: h_stores_dir.clone(),
            own_addr: stub_addr.to_string(),
            create_store_bodies: Vec::new(),
        }),
        relay: RelayStub::default(),
    });

    let router = stub_router(Arc::clone(&state));
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let stub_url = format!("http://{stub_addr}");

    let h_service_token = "h-service-token".to_string();
    let mut h = PimbleServer::with_config(ServerConfig {
        auth_token: Some(h_service_token.clone()),
        jwks_url: Some(format!("{stub_url}/jwks.json").parse().unwrap()),
        jwt_issuer: Some(issuer),
        ..ServerConfig::in_dir(h_dir.path())
    });
    h.start().await.expect("H starts");
    let h_url = format!("http://{}", h.addr());
    let h_client = PimbleClient::connect_with_auth(&h_url, &AuthMethod::Bearer { token: h_service_token.clone() }).await.expect("service client connects to H");
    let h_admin = PimbleClient::connect_with_auth(&h_url, &AuthMethod::Bearer { token: h_service_token }).await.expect("admin client connects to H");
    {
        let mut s = state.inner.lock().unwrap();
        s.h_client = Some(Arc::new(h_client));
        s.h_rpc_url = format!("ws://{}", h.addr());
    }

    Env { stub_url, stub_addr, stub: state, _h: h, h_admin, h_stores_dir, h_dir }
}

/// Alice's store, never hosted:
///
/// ```text
/// root
/// ├── Holiday (folder)        <- what gets shared
/// │   ├── Packing (document, "socks and a map")
/// │   └── Tickets (folder)
/// │       └── Train (document)
/// └── Diary (document, "PRIVATE-DIARY-TEXT")
/// ```
pub(crate) struct Fixture {
    pub(crate) store_id: StoreId,
    pub(crate) root_id: NodeId,
    pub(crate) shared: NodeId,
    pub(crate) inside: NodeId,
    pub(crate) deeper: NodeId,
    pub(crate) leaf: NodeId,
    pub(crate) private: NodeId,
}

pub(crate) const STORE_NAME: &str = "Alice's Secret Notebook";

pub(crate) async fn plain_fixture(env: &Env, alice: &PimbleClient, alice_dir: &Path) -> Fixture {
    env.sign_in(alice, ALICE).await;
    let (store_id, root_id) = alice.create_store(alice_dir.join("a.pimble"), STORE_NAME).await.unwrap();
    let shared = alice.create_node(store_id, Some(root_id), "folder", "Holiday").await.unwrap();
    let inside = alice.create_node(store_id, Some(shared), "document", "Packing").await.unwrap();
    let deeper = alice.create_node(store_id, Some(shared), "folder", "Tickets").await.unwrap();
    let leaf = alice.create_node(store_id, Some(deeper), "document", "Train").await.unwrap();
    let private = alice.create_node(store_id, Some(root_id), "document", "Diary").await.unwrap();
    // A person's content edit earns a `modified_at` stamp on the server's
    // flush debounce (750 ms). It lands before the store is relayed.
    let mut changes = alice.subscribe_store_changes(store_id).await.unwrap();
    write_text(alice, store_id, inside, "seed", "socks and a map").await;
    write_text(alice, store_id, private, "seed", "PRIVATE-DIARY-TEXT").await;
    wait_stamped(&mut changes, &[inside, private]).await;
    Fixture { store_id, root_id, shared, inside, deeper, leaf, private }
}

/// The store shared from Alice's computer, its link reconciled and the
/// tunnel announcing it.
pub(crate) async fn relayed_fixture(env: &Env, alice: &PimbleClient, alice_dir: &Path) -> Fixture {
    let fx = plain_fixture(env, alice, alice_dir).await;
    alice.cloud_relay_store(fx.store_id).await.expect("an unhosted store is shared from this computer");
    let up = wait_until(Duration::from_secs(20), || async {
        matches!(alice.get_store_sync_response(fx.store_id).await.map(|a| a.state), Ok(SyncState::Synced { .. })) && env.stub.relay.serves(fx.store_id)
    })
    .await;
    assert!(up, "the link to the twin syncs and the relay is told the store is served");
    fx
}

/// The folder shared, with `members` invited.
pub(crate) async fn share_with(alice: &PimbleClient, fx: &Fixture, members: &[((&str, &str), MemberRole)]) {
    let answer = alice.cloud_share_node(fx.store_id, fx.shared, "Holiday Plans").await.expect("a relayed store's folder is shared exactly as a hosted one's");
    assert!(matches!(answer.share.state, SyncState::Synced { .. }), "the wraps and the scope are on the twin when the call answers: {:?}", answer.share.state);
    for (who, role) in members {
        alice.cloud_share_invite(fx.store_id, fx.shared, who.0, *role).await.expect("inviting a member");
    }
}

/// A member's device: signed in, the share added, its first document pulled
/// through the relay.
pub(crate) async fn member_device(env: &Env, who: (&str, &str), fx: &Fixture) -> (PimbleServer, PimbleClient, tempfile::TempDir) {
    let (server, client, dir) = start_local_server().await;
    env.sign_in(&client, who).await;
    let store = client.cloud_add_hosted_store(fx.store_id).await.expect("a relayed share is added like any hosted store");
    assert_eq!(store.relay, RelaySide::Member, "{}'s replica says it is served from its owner's computer", who.0);
    assert_eq!(store.sync_mode, StoreKind::Vault);
    // However large the scope has grown (two 20 MiB documents by the time a
    // third member joins in the large-document test), and however loaded
    // the machine: slower, never short of it.
    let pulled = wait_until(SETTLE_DEADLINE, || async { node_text(&client, fx.store_id, fx.inside).await.contains("socks and a map") }).await;
    assert!(pulled, "{}'s replica pulls the scope through the relay and reads it with the share's key", who.0);
    (server, client, dir)
}

pub(crate) async fn link_state(client: &PimbleClient, store_id: StoreId) -> SyncState {
    client.get_store_sync_response(store_id).await.map(|answer| answer.state).unwrap_or(SyncState::Offline)
}
