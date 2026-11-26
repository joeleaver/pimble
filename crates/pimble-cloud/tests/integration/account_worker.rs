//! The account page on the server that holds the keys
//! (docs/ONE_CODEBASE_CONTRACT.md, "Built: wave 5, part 3"): a device's
//! Pimble server, signed in to this real accounts service, changes the
//! password (`cloudChangePassword`), makes a new recovery code
//! (`cloudNewRecoveryCode`) and changes a whole store's members
//! (`cloudStore*`). The browser's worker server runs the same handler.
//!
//! Each change is checked the way a person meets it: by logging in with the
//! new password, recovering with the new code, and opening the store key a
//! new owner was handed. A failure part way, at the one request that commits
//! a password change, leaves the account opening with exactly one password.

use super::*;

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use pimble_client::PimbleClient;
use pimble_rpc::{MemberRole, ShareMemberStatus};

/// A device: its own Pimble server, tokenless on loopback, as the desktop's
/// embedded one and the browser's worker are, and a client of it.
struct Device {
    server: pimble_server::PimbleServer,
    client: PimbleClient,
    dir: tempfile::TempDir,
}

impl Device {
    async fn signed_in(service_url: &str, email: &str, password: &str) -> Device {
        let dir = tempfile::tempdir().unwrap();
        let mut server = pimble_server::PimbleServer::with_config(pimble_server::ServerConfig::in_dir(dir.path()));
        server.start().await.expect("the device's server starts");
        let client = PimbleClient::connect(format!("http://{}", server.addr())).await.expect("a client connects");
        client.cloud_sign_in(service_url, email, password).await.expect("the device signs in");
        Device { server, client, dir }
    }

    async fn stop(mut self) {
        self.server.stop().await.unwrap();
    }
}

/// The accounts service's base URL, as a device is signed in to it.
fn service_url(stack: &Stack) -> String {
    stack.base_url.trim_end_matches("/api/v1").to_string()
}

async fn login_status(stack: &Stack, email: &str, password: &str) -> u16 {
    let kdf = fetch_kdf(stack, email).await;
    let auth_key = pimble_crypto::encode_auth_key(&pimble_crypto::derive_password_keys(password, &kdf).unwrap().auth_key);
    stack.http.post(format!("{}/login", stack.base_url)).json(&json!({ "email": email, "auth_key": auth_key })).send().await.unwrap().status().as_u16()
}

/// Log in with `password` and open the account keys it wraps, as a device
/// signing in does: whether they are `keys`.
async fn password_opens(stack: &Stack, email: &str, password: &str, keys: &AccountKeys) -> bool {
    if login_status(stack, email, password).await != 200 {
        return false;
    }
    let (_, cookie) = login(stack, email, password).await;
    let held: Value = stack.http.get(format!("{}/me/keys", stack.base_url)).header("Cookie", &cookie).send().await.unwrap().json().await.unwrap();
    let kdf: KdfParams = serde_json::from_value(held["kdf"].clone()).unwrap();
    let blob: AccountKeyBlob = serde_json::from_value(held["account_key_blob"].clone()).unwrap();
    let derived = pimble_crypto::derive_password_keys(password, &kdf).unwrap();
    pimble_crypto::unwrap_account_keys(&blob, &derived.kek).is_ok_and(|opened| opened.encryption_secret == keys.encryption_secret && opened.signing_secret == keys.signing_secret)
}

fn contains_bytes(dir: &Path, needle: &[u8]) -> bool {
    std::fs::read_dir(dir).into_iter().flatten().flatten().any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            contains_bytes(&path, needle)
        } else {
            std::fs::read(&path).is_ok_and(|bytes| bytes.windows(needle.len()).any(|w| w == needle))
        }
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn the_device_changes_the_password_and_the_new_one_opens_the_same_keys() {
    let stack = skip_without_rhypedb!();
    let (email, old, new) = ("changer@example.com", "the first password", "the second password");
    let (_, keys) = signup_with_material(&stack, email, old).await;
    let (_, other_session) = verify_then_login(&stack, email, old).await;
    let device = Device::signed_in(&service_url(&stack), email, old).await;
    let other_device = Device::signed_in(&service_url(&stack), email, old).await;

    let wrong = device.client.cloud_change_password("not the password", new).await.expect_err("a wrong current password is refused");
    assert!(wrong.to_string().contains("That is not your current password."), "{wrong}");
    let short = device.client.cloud_change_password(old, "short").await.expect_err("a short password is refused");
    assert!(short.to_string().contains("at least 8 characters"), "{short}");
    assert!(password_opens(&stack, email, old, &keys).await, "a refused change changes nothing");

    device.client.cloud_change_password(old, new).await.expect("the password changes");
    assert_eq!(login_status(&stack, email, old).await, 401, "the old password no longer logs in");
    assert!(password_opens(&stack, email, new, &keys).await, "the new password logs in and opens the same account keys");

    // This device made the change and stays signed in (Joe, 2026-11-25:
    // "Sign out the others"); its session still answers.
    let status = device.client.cloud_status().await.unwrap();
    assert!(status.signed_in);
    assert_eq!(status.email.as_deref(), Some(email));
    device.client.cloud_list_hosted_stores().await.expect("this device's session still works");
    device.client.cloud_new_recovery_code(new).await.expect("and still changes the account");

    // Every other session is signed out: one made over HTTP, and another
    // device's, which says so in a sentence and asks to sign in again.
    let me = stack.http.get(format!("{}/me", stack.base_url)).header("Cookie", &other_session).send().await.unwrap();
    assert_eq!(me.status(), 401, "another session is signed out");
    let ended = other_device.client.cloud_change_password(new, "a third password").await.expect_err("another device's session has ended");
    assert_eq!(ended.to_string(), "Your Pimble Cloud sign-in has ended. Sign in again.");
    assert!(password_opens(&stack, email, new, &keys).await, "and its attempt changed nothing");
    other_device.client.cloud_sign_in(&service_url(&stack), email, new).await.expect("it signs in again with the new password");
    other_device.client.cloud_list_hosted_stores().await.expect("and works again");
    other_device.stop().await;

    assert!(!contains_bytes(device.dir.path(), old.as_bytes()) && !contains_bytes(device.dir.path(), new.as_bytes()), "no password reaches the device's disk");
    device.stop().await;
}

/// What the proxy does with the `POST` it was made for.
const PASS: u8 = 0;
/// Answer 502 without sending it on: the change never reached the service.
const REFUSE: u8 = 1;
/// Send it on, then answer 502: the change was made and its answer lost.
const LOSE_REPLY: u8 = 2;

/// A proxy in front of the accounts service that passes everything but a
/// `POST` to `held` (a password change, a new recovery code), which goes as
/// `mode` says.
async fn spawn_proxy(target: String, held: &'static str, mode: Arc<AtomicU8>) -> (String, tokio::task::JoinHandle<()>) {
    use axum::body::Bytes;
    use axum::extract::State as AxState;
    use axum::http::{HeaderMap, Method, StatusCode, Uri};
    use axum::response::IntoResponse;

    async fn forward(AxState((target, held, mode)): AxState<(String, &'static str, Arc<AtomicU8>)>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> axum::response::Response {
        let path = uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or_default();
        let is_change = method == Method::POST && uri.path() == held;
        let how = if is_change { mode.load(Ordering::SeqCst) } else { PASS };
        if how == REFUSE {
            return StatusCode::BAD_GATEWAY.into_response();
        }
        let mut request = reqwest::Client::new().request(reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap(), format!("{target}{path}")).body(body.to_vec());
        for name in ["authorization", "cookie", "content-type"] {
            if let Some(value) = headers.get(name) {
                request = request.header(name, value.as_bytes());
            }
        }
        let answer = request.send().await.unwrap();
        if how == LOSE_REPLY {
            return StatusCode::BAD_GATEWAY.into_response();
        }
        let status = StatusCode::from_u16(answer.status().as_u16()).unwrap();
        let content_type = answer.headers().get("content-type").map(|v| v.to_str().unwrap_or("").to_string());
        let bytes = answer.bytes().await.unwrap();
        let mut response = (status, bytes.to_vec()).into_response();
        if let Some(content_type) = content_type {
            response.headers_mut().insert("content-type", content_type.parse().unwrap());
        }
        response
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = axum::Router::new().fallback(forward).with_state((target, held, mode));
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://{addr}"), task)
}

/// A change that fails part way leaves the account opening with one
/// password: the old one when the change never arrived, the new one when it
/// arrived and only its answer was lost. Never neither, never a password
/// whose wrapped keys are another's.
#[tokio::test(flavor = "multi_thread")]
async fn a_password_change_that_fails_part_way_leaves_one_password_that_opens_the_keys() {
    let stack = skip_without_rhypedb!();
    let (email, old, new) = ("partway@example.com", "the first password", "the second password");
    let (_, keys) = signup_with_material(&stack, email, old).await;
    verify_then_login(&stack, email, old).await;
    let mode = Arc::new(AtomicU8::new(PASS));
    let (proxy_url, proxy) = spawn_proxy(service_url(&stack), "/api/v1/me/password", Arc::clone(&mode)).await;
    let device = Device::signed_in(&proxy_url, email, old).await;

    mode.store(REFUSE, Ordering::SeqCst);
    device.client.cloud_change_password(old, new).await.expect_err("the change did not arrive");
    assert!(password_opens(&stack, email, old, &keys).await, "the old password still opens the account");
    assert_eq!(login_status(&stack, email, new).await, 401, "and the new one does not");
    assert_eq!(device.client.cloud_status().await.unwrap().email.as_deref(), Some(email), "the device stays signed in");

    mode.store(LOSE_REPLY, Ordering::SeqCst);
    device.client.cloud_change_password(old, new).await.expect_err("the change's answer was lost");
    assert!(password_opens(&stack, email, new, &keys).await, "the change was made whole: the new password opens the account");
    assert_eq!(login_status(&stack, email, old).await, 401, "and the old one does not");

    // Asked again with the old password, as a person would: refused in a
    // sentence; with the new one as the current, it goes through.
    mode.store(PASS, Ordering::SeqCst);
    let again = device.client.cloud_change_password(old, new).await.expect_err("the old password is no longer the current one");
    assert!(again.to_string().contains("That is not your current password."), "{again}");
    device.client.cloud_change_password(new, old).await.expect("the new password is the current one");
    assert!(password_opens(&stack, email, old, &keys).await);

    device.stop().await;
    proxy.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_recovery_code_recovers_the_account_and_the_old_one_does_not() {
    let stack = skip_without_rhypedb!();
    let (email, password) = ("recodes@example.com", "a password to forget");
    let (_, keys, old_code) = signup_with_material_and_recovery_code(&stack, email, password).await;
    verify_then_login(&stack, email, password).await;
    let device = Device::signed_in(&service_url(&stack), email, password).await;

    // Without the current password, or with a wrong one, nothing is asked of
    // the service and the old code stands (Joe, 2026-11-25).
    let mails = mail_count(&stack, email);
    let empty = device.client.cloud_new_recovery_code("").await.expect_err("no password");
    assert!(empty.to_string().contains("Enter your current password to make a new recovery code."), "{empty}");
    let wrong = device.client.cloud_new_recovery_code("not the password").await.expect_err("a wrong password");
    assert!(wrong.to_string().contains("That is not your current password."), "{wrong}");
    assert!(code_opens(&stack, email, &old_code).await, "a refused code changes nothing");
    assert_eq!(mail_count(&stack, email), mails, "and mails nothing");

    let code = device.client.cloud_new_recovery_code(password).await.expect("a new code");
    assert_ne!(code, old_code);
    assert_eq!(mail_count(&stack, email), mails + 1, "a change mails the account once");
    assert!(!contains_bytes(device.dir.path(), code.as_bytes()), "the code is not kept on the device's disk");

    let resp = stack.http.post(format!("{}/recover/start", stack.base_url)).json(&json!({ "email": email })).send().await.unwrap();
    assert_eq!(resp.status(), 202);
    let token = extract_recovery_token(&stack, email);
    let (served, _) = fetch_and_unwrap_recovery_material(&stack, &token, &code).await;
    let params = KdfParams {
        salt: served["recovery_salt"].as_str().unwrap().to_string(),
        m_cost: served["recovery_kdf"]["m_cost"].as_u64().unwrap() as u32,
        t_cost: served["recovery_kdf"]["t_cost"].as_u64().unwrap() as u32,
        p_cost: served["recovery_kdf"]["p_cost"].as_u64().unwrap() as u32,
    };
    let blob: AccountKeyBlob = serde_json::from_value(served["recovery_key_blob"].clone()).unwrap();
    let old_kek = pimble_crypto::derive_recovery_kek(&old_code, &params).unwrap();
    assert!(pimble_crypto::unwrap_account_keys(&blob, &old_kek).is_err(), "the old code no longer opens the account");

    // Recovered with the new code: a new password, which logs in and opens
    // the same keys.
    let kek = pimble_crypto::derive_recovery_kek(&code, &params).unwrap();
    let opened = pimble_crypto::unwrap_account_keys(&blob, &kek).expect("the new code opens the account keys");
    complete_recovery_with_new_material(&stack, &token, &opened, "a password after recovery").await;
    assert!(password_opens(&stack, email, "a password after recovery", &keys).await);
    device.stop().await;
}

/// The recovery material the service serves for `email` now, opened with
/// `code`: whether that code recovers the account.
async fn code_opens(stack: &Stack, email: &str, code: &str) -> bool {
    let user = stack.app_state.db.find_user_by_email(email).await.unwrap().unwrap();
    let params = KdfParams {
        salt: user.recovery_salt.clone().unwrap(),
        m_cost: pimble_crypto::KDF_M_COST_KIB,
        t_cost: pimble_crypto::KDF_T_COST,
        p_cost: pimble_crypto::KDF_P_COST,
    };
    let blob: AccountKeyBlob = serde_json::from_str(user.recovery_key_blob.as_deref().unwrap()).unwrap();
    let kek = pimble_crypto::derive_recovery_kek(code, &params).unwrap();
    pimble_crypto::unwrap_account_keys(&blob, &kek).is_ok()
}

/// A new recovery code whose answer was lost may have been taken, and the
/// old code then opens nothing: the device says so, in a sentence that asks
/// for a new one. A refusal the service answered keeps its own sentence and
/// leaves the code as it was.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_recovery_code_whose_answer_is_lost_says_to_make_another() {
    let stack = skip_without_rhypedb!();
    let (email, password) = ("lost-code@example.com", "a password to keep");
    let (_, _, old_code) = signup_with_material_and_recovery_code(&stack, email, password).await;
    verify_then_login(&stack, email, password).await;
    let mode = Arc::new(AtomicU8::new(LOSE_REPLY));
    let (proxy_url, proxy) = spawn_proxy(service_url(&stack), "/api/v1/me/recovery-code", Arc::clone(&mode)).await;
    let device = Device::signed_in(&proxy_url, email, password).await;

    let lost = device.client.cloud_new_recovery_code(password).await.expect_err("the answer was lost");
    assert!(lost.to_string().contains("Your old recovery code may have stopped working, so make a new one now."), "{lost}");
    assert!(!code_opens(&stack, email, &old_code).await, "the service took the code nobody saw: the old one opens nothing");

    // Made again, as the sentence asks: a code that recovers the account.
    mode.store(PASS, Ordering::SeqCst);
    let code = device.client.cloud_new_recovery_code(password).await.expect("a new code");
    assert!(code_opens(&stack, email, &code).await);

    // Refused by the service itself (the device's session was ended by a
    // password change elsewhere): its own sentence, and the code stands.
    let other = Device::signed_in(&service_url(&stack), email, password).await;
    other.client.cloud_change_password(password, "a password from elsewhere").await.expect("changed elsewhere");
    let refused = device.client.cloud_new_recovery_code(password).await.expect_err("this sign-in has ended");
    assert!(refused.to_string().contains("Your Pimble Cloud sign-in has ended. Sign in again."), "{refused}");
    assert!(!refused.to_string().contains("may have stopped working"), "{refused}");
    assert!(code_opens(&stack, email, &code).await, "nothing was written");

    other.stop().await;
    device.stop().await;
    proxy.abort();
}

fn role_of(members: &[pimble_rpc::ShareMember], email: &str) -> Option<(MemberRole, ShareMemberStatus)> {
    members.iter().find(|m| m.email == email).map(|m| (m.role, m.status))
}

#[tokio::test(flavor = "multi_thread")]
async fn an_owner_makes_another_owner_and_a_store_always_keeps_one() {
    let stack = skip_without_rhypedb!();
    let (alice, bob) = (("owner-a@example.com", "alice's password"), ("owner-b@example.com", "bob's password"));
    let (_, alice_keys) = signup_with_material(&stack, alice.0, alice.1).await;
    let (login_body, cookie) = verify_then_login(&stack, alice.0, alice.1).await;
    let alice_id = login_body["user"]["id"].as_str().unwrap().to_string();
    let (_, bob_keys) = signup_with_material(&stack, bob.0, bob.1).await;
    let (_, bob_cookie) = verify_then_login(&stack, bob.0, bob.1).await;

    // An encrypted store with its key sealed to Alice, as hosting makes one.
    let store = create_store_with_body(&stack, &cookie, json!({ "name": "Family", "kind": "vault" })).await;
    let store_id = store["store_id"].as_str().unwrap().to_string();
    let store_key = pimble_crypto::SymmetricKey::generate();
    let key_id = uuid::Uuid::new_v4();
    let envelope = wrap_store_key(&store_key, key_id, &store_id, &alice_keys, &alice_keys.public_keys());
    let resp = put_keys(&stack, &cookie, &store_id, json!([{ "user_id": alice_id, "key_id": key_id.to_string(), "envelope": envelope }])).await;
    assert_eq!(resp.status(), 200);
    let store = pimble_core::StoreId::parse(&store_id).unwrap();

    let device = Device::signed_in(&service_url(&stack), alice.0, alice.1).await;
    let listed = device.client.cloud_store_members(store).await.expect("an owner lists the store's members");
    assert_eq!(role_of(&listed.members, alice.0), Some((MemberRole::Owner, ShareMemberStatus::Active)));

    // Bob made an owner: he holds the store and a copy of its key he opens.
    let listed = device.client.cloud_store_set_member(store, bob.0, MemberRole::Owner).await.expect("an owner makes another owner");
    assert_eq!(role_of(&listed.members, bob.0), Some((MemberRole::Owner, ShareMemberStatus::Active)));
    let held: Value = keys_response(&stack, &bob_cookie, &store_id, None).await.json().await.unwrap();
    let bobs: pimble_crypto::KeyEnvelope = serde_json::from_value(held["envelopes"][0]["envelope"].clone()).unwrap();
    let opened = pimble_crypto::unwrap_key(&bobs, &bob_keys, &alice_keys.public_keys().signing).expect("Bob opens the store key");
    assert_eq!(opened.0, store_key.0);

    // Bob an editor again; then Alice is the last owner, and stays one.
    let listed = device.client.cloud_store_set_member(store, bob.0, MemberRole::Editor).await.expect("a role changes");
    assert_eq!(role_of(&listed.members, bob.0), Some((MemberRole::Editor, ShareMemberStatus::Active)));
    let demoted = device.client.cloud_store_set_member(store, alice.0, MemberRole::Reader).await.expect_err("the last owner is not demoted");
    assert!(demoted.to_string().contains("A store always keeps at least one owner."), "{demoted}");
    let removed = device.client.cloud_store_remove_member(store, alice.0).await.expect_err("the last owner is not removed");
    assert!(removed.to_string().contains("A store always keeps at least one owner."), "{removed}");

    // An encrypted store is not given to an address with no account. The
    // service takes one lookup per 200 ms per caller; Bob's was moments ago.
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let nobody = device.client.cloud_store_set_member(store, "nobody@example.com", MemberRole::Reader).await.expect_err("no account, no key");
    assert!(nobody.to_string().contains("No verified Pimble account has that address."), "{nobody}");

    let listed = device.client.cloud_store_remove_member(store, bob.0).await.expect("a member is removed");
    assert_eq!(role_of(&listed.members, bob.0), None);
    assert_eq!(role_of(&listed.members, alice.0), Some((MemberRole::Owner, ShareMemberStatus::Active)));

    // A plain store takes an invitation; an owner must have an account.
    let plain = create_store_with_body(&stack, &cookie, json!({ "name": "Recipes", "kind": "plain" })).await;
    let plain = pimble_core::StoreId::parse(plain["store_id"].as_str().unwrap()).unwrap();
    let listed = device.client.cloud_store_set_member(plain, "later@example.com", MemberRole::Editor).await.expect("an invitation");
    assert_eq!(role_of(&listed.members, "later@example.com"), Some((MemberRole::Editor, ShareMemberStatus::Invited)));
    let owner = device.client.cloud_store_set_member(plain, "someone@example.com", MemberRole::Owner).await.expect_err("an owner needs an account");
    assert!(owner.to_string().contains("An owner must already have a verified Pimble account"), "{owner}");
    let listed = device.client.cloud_store_remove_member(plain, "later@example.com").await.expect("an invitation is withdrawn");
    assert_eq!(role_of(&listed.members, "later@example.com"), None);

    // Someone who does not own the store is told so.
    let bobs_device = Device::signed_in(&service_url(&stack), bob.0, bob.1).await;
    let refused = bobs_device.client.cloud_store_members(store).await.expect_err("only an owner");
    assert!(refused.to_string().contains("Only an owner of this store"), "{refused}");

    bobs_device.stop().await;
    device.stop().await;
}

/// A refused sign-in is a whole sentence on the device's server, the one the
/// browser's sign-in page and the desktop's Account modal both show; an
/// address with no account reads exactly as a wrong password.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_sign_in_is_a_sentence_that_does_not_say_whether_the_account_exists() {
    let stack = skip_without_rhypedb!();
    let (email, password) = ("refused@example.com", "the right password");
    signup_with_material(&stack, email, password).await;
    let dir = tempfile::tempdir().unwrap();
    let mut server = pimble_server::PimbleServer::with_config(pimble_server::ServerConfig::in_dir(dir.path()));
    server.start().await.expect("the device's server starts");
    let client = PimbleClient::connect(format!("http://{}", server.addr())).await.expect("a client connects");
    let url = service_url(&stack);

    let unverified = client.cloud_sign_in(&url, email, password).await.expect_err("not verified yet");
    assert_eq!(unverified.to_string(), "Check your inbox: this address has not been verified yet.");
    let wrong_unverified = client.cloud_sign_in(&url, email, "not the password").await.expect_err("a wrong password");
    assert_eq!(wrong_unverified.to_string(), "That email and password do not match.", "unverified is said only once the password matched");

    verify_then_login(&stack, email, password).await;
    let wrong = client.cloud_sign_in(&url, email, "not the password").await.expect_err("a wrong password");
    assert_eq!(wrong.to_string(), "That email and password do not match.");
    let unknown = client.cloud_sign_in(&url, "nobody-here@example.com", "any password").await.expect_err("no such account");
    assert_eq!(unknown.to_string(), wrong.to_string(), "an unknown address reads as a wrong password");
    assert!(!client.cloud_status().await.unwrap().signed_in, "nothing was signed in");

    client.cloud_sign_in(&url, email, password).await.expect("the right password signs in");
    server.stop().await.unwrap();
}
