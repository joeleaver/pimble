//! A password change signs out the account's other sessions, and the session
//! that made it stays signed in (Joe, 2026-11-25: "Sign out the others";
//! docs/ONE_CODEBASE_CONTRACT.md "Built: wave 5, part 4"). A recovery ends
//! every session. Both happen in the one update that writes the new
//! password, through `User.session_epoch`.

use super::*;

/// What `/me` and `/token` answer a session, sent as the cookie or as a
/// bearer header.
enum Credential<'a> {
    Cookie(&'a str),
    Bearer(&'a str),
}

impl Credential<'_> {
    fn on(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self {
            Credential::Cookie(cookie) => request.header("Cookie", *cookie),
            Credential::Bearer(session) => request.bearer_auth(session),
        }
    }
}

async fn me_status(stack: &Stack, credential: &Credential<'_>) -> u16 {
    credential.on(stack.http.get(format!("{}/me", stack.base_url))).send().await.unwrap().status().as_u16()
}

async fn token_status(stack: &Stack, credential: &Credential<'_>) -> u16 {
    credential.on(stack.http.post(format!("{}/token", stack.base_url))).json(&json!({})).send().await.unwrap().status().as_u16()
}

async fn auth_key_of(stack: &Stack, email: &str, password: &str) -> String {
    let kdf = fetch_kdf(stack, email).await;
    pimble_crypto::encode_auth_key(&pimble_crypto::derive_password_keys(password, &kdf).unwrap().auth_key)
}

/// `POST /me/password` with `credential`, from `current` to `new`.
async fn change_password(stack: &Stack, credential: &Credential<'_>, keys: &AccountKeys, current_auth_key: &str, new: &str) -> reqwest::Response {
    let kdf = KdfParams::generate();
    let derived = pimble_crypto::derive_password_keys(new, &kdf).unwrap();
    let body = json!({
        "current_auth_key": current_auth_key,
        "auth_key": pimble_crypto::encode_auth_key(&derived.auth_key),
        "kdf": kdf,
        "account_key_blob": pimble_crypto::wrap_account_keys(keys, &derived.kek).unwrap(),
    });
    credential.on(stack.http.post(format!("{}/me/password", stack.base_url))).json(&body).send().await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_password_change_signs_out_the_other_sessions_and_keeps_its_own() {
    let stack = skip_without_rhypedb!();
    let (email, old, new) = ("others-out@example.com", "the first password", "the second password");
    let (_, keys) = signup_with_material(&stack, email, old).await;
    let (_, changing_cookie) = verify_then_login(&stack, email, old).await;
    let (_, other_cookie) = login(&stack, email, old).await;
    let (other_login, _) = login(&stack, email, old).await;
    let other_bearer = other_login["session"].as_str().unwrap().to_string();
    let (changing, cookie, bearer) = (Credential::Cookie(&changing_cookie), Credential::Cookie(&other_cookie), Credential::Bearer(&other_bearer));
    for credential in [&changing, &cookie, &bearer] {
        assert_eq!(me_status(&stack, credential).await, 200, "every session works before the change");
        assert_eq!(token_status(&stack, credential).await, 200);
    }

    // A refused change ends nothing.
    let refused = change_password(&stack, &changing, &keys, "not the current key", new).await;
    assert_eq!(refused.status(), 401);
    let refused: Value = refused.json().await.unwrap();
    assert_eq!(refused["error"], "wrong_current_password", "told apart from a session refused");
    assert_eq!(me_status(&stack, &cookie).await, 200, "a refused change signs nobody out");

    let current = auth_key_of(&stack, email, old).await;
    assert_eq!(change_password(&stack, &changing, &keys, &current, new).await.status(), 200);

    // The other sessions are refused, as the cookie and as a bearer, and
    // mint no token.
    for credential in [&cookie, &bearer] {
        assert_eq!(me_status(&stack, credential).await, 401, "another session is signed out");
        assert_eq!(token_status(&stack, credential).await, 401, "and mints no token");
        let keys_answer = credential.on(stack.http.get(format!("{}/me/keys", stack.base_url))).send().await.unwrap();
        assert_eq!(keys_answer.status(), 401);
    }
    let ended: Value = cookie.on(stack.http.get(format!("{}/me", stack.base_url))).send().await.unwrap().json().await.unwrap();
    assert_eq!(ended["error"], "unauthorized", "an ended session reads like any invalid one");

    // The session that changed it is still signed in.
    assert_eq!(me_status(&stack, &changing).await, 200, "the changing session's /me works");
    assert_eq!(token_status(&stack, &changing).await, 200, "and it mints tokens");

    // The rows of the ended sessions are gone; a sign-in with the new
    // password makes a session that works.
    let user = stack.app_state.db.find_user_by_email(email).await.unwrap().unwrap();
    assert_eq!(stack.app_state.db.sessions_for_user(user.rid).await.unwrap().len(), 1, "only the changing session is left");
    let (_, fresh) = login(&stack, email, new).await;
    assert_eq!(me_status(&stack, &Credential::Cookie(&fresh)).await, 200);
    assert_eq!(token_status(&stack, &Credential::Cookie(&fresh)).await, 200);
    assert_eq!(me_status(&stack, &changing).await, 200, "a later sign-in ends nothing");

    // A second change from the fresh session ends the first changer's.
    let current = auth_key_of(&stack, email, new).await;
    assert_eq!(change_password(&stack, &Credential::Cookie(&fresh), &keys, &current, old).await.status(), 200);
    assert_eq!(me_status(&stack, &changing).await, 401, "the earlier changer is now one of the others");
    assert_eq!(me_status(&stack, &Credential::Cookie(&fresh)).await, 200);
}

/// A session whose password was checked before the change, and that is
/// written only after it (a sign-in racing the change), carries the old epoch
/// and is ended with the rest.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_made_for_the_old_password_is_ended_by_the_change() {
    let stack = skip_without_rhypedb!();
    let (email, old, new) = ("racing-login@example.com", "the first password", "the second password");
    let (_, keys) = signup_with_material(&stack, email, old).await;
    let (_, changing_cookie) = verify_then_login(&stack, email, old).await;
    let before = stack.app_state.db.find_user_by_email(email).await.unwrap().unwrap();

    let current = auth_key_of(&stack, email, old).await;
    assert_eq!(change_password(&stack, &Credential::Cookie(&changing_cookie), &keys, &current, new).await.status(), 200);

    // What `login` would write had it read the account before the change.
    let (token, token_hash) = pimble_cloud::auth::new_session_token();
    let expires_at_ms = chrono::Utc::now().timestamp_millis() + 60 * 60 * 1000;
    stack.app_state.db.create_session(before.rid, &token_hash, expires_at_ms, before.session_epoch).await.unwrap();
    assert_eq!(me_status(&stack, &Credential::Bearer(&token)).await, 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recovery_ends_every_session() {
    let stack = skip_without_rhypedb!();
    let (email, password) = ("recovered-out@example.com", "a password to forget");
    let (_, keys, code) = signup_with_material_and_recovery_code(&stack, email, password).await;
    let (_, cookie) = verify_then_login(&stack, email, password).await;
    let (other, _) = login(&stack, email, password).await;
    let bearer = other["session"].as_str().unwrap().to_string();

    let resp = stack.http.post(format!("{}/recover/start", stack.base_url)).json(&json!({ "email": email })).send().await.unwrap();
    assert_eq!(resp.status(), 202);
    let token = extract_recovery_token(&stack, email);
    let (_, opened) = fetch_and_unwrap_recovery_material(&stack, &token, &code).await;
    assert_eq!(opened.encryption_secret, keys.encryption_secret);
    complete_recovery_with_new_material(&stack, &token, &opened, "a password after recovery").await;

    for credential in [Credential::Cookie(&cookie), Credential::Bearer(&bearer)] {
        assert_eq!(me_status(&stack, &credential).await, 401, "no session survives a recovery");
        assert_eq!(token_status(&stack, &credential).await, 401);
    }
    let user = stack.app_state.db.find_user_by_email(email).await.unwrap().unwrap();
    assert!(stack.app_state.db.sessions_for_user(user.rid).await.unwrap().is_empty());
    let (_, fresh) = login(&stack, email, "a password after recovery").await;
    assert_eq!(me_status(&stack, &Credential::Cookie(&fresh)).await, 200);
}
