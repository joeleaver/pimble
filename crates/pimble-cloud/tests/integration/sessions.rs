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

// ── Races: every change reads the account again under its lock ───────────

/// A `/recover/{token}/complete` body with fresh material: the password
/// `new_password` and a new recovery code. Its `kdf.salt` and
/// `recovery_salt` tell afterwards whose material the account holds.
fn recovery_body(keys: &AccountKeys, new_password: &str) -> Value {
    let kdf = KdfParams::generate();
    let derived = pimble_crypto::derive_password_keys(new_password, &kdf).unwrap();
    let recovery_params = KdfParams::generate();
    let recovery_kek = pimble_crypto::derive_recovery_kek(&pimble_crypto::generate_recovery_code(), &recovery_params).unwrap();
    json!({
        "auth_key": pimble_crypto::encode_auth_key(&derived.auth_key),
        "kdf": kdf,
        "account_key_blob": pimble_crypto::wrap_account_keys(keys, &derived.kek).unwrap(),
        "recovery_salt": recovery_params.salt,
        "recovery_key_blob": pimble_crypto::wrap_account_keys(keys, &recovery_kek).unwrap(),
    })
}

/// A `/me/recovery-code` body with a new code.
fn recovery_code_body(keys: &AccountKeys, current_auth_key: &str) -> Value {
    let params = KdfParams::generate();
    let kek = pimble_crypto::derive_recovery_kek(&pimble_crypto::generate_recovery_code(), &params).unwrap();
    json!({ "current_auth_key": current_auth_key, "recovery_salt": params.salt, "recovery_key_blob": pimble_crypto::wrap_account_keys(keys, &kek).unwrap() })
}

/// A recovery token for `email` (one per address per minute).
async fn recovery_token(stack: &Stack, email: &str) -> String {
    let resp = stack.http.post(format!("{}/recover/start", stack.base_url)).json(&json!({ "email": email })).send().await.unwrap();
    assert_eq!(resp.status(), 202);
    extract_recovery_token(stack, email)
}

/// The loser of a race is refused in a sentence: by the session check at
/// the door (401) or by the account read again under the lock (409).
async fn refused_in_a_sentence(resp: reqwest::Response, what: &str) {
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap();
    assert!(status == 401 || status == 409, "{what}: refused, got {status} {body}");
    if status == 409 {
        let message = body["message"].as_str().unwrap_or_default();
        assert!(message.contains("nothing was changed"), "{what}: the refusal says so: {body}");
    }
}

const RACE_ROUNDS: usize = 3;

/// A password change racing a recovery never undoes the recovery: the
/// account ends with the recovery's password, the changing session is
/// signed out, and the epoch never goes down.
#[tokio::test(flavor = "multi_thread")]
async fn a_password_change_racing_a_recovery_never_undoes_it() {
    let stack = skip_without_rhypedb!();
    let password = "the first password";
    for round in 0..RACE_ROUNDS {
        // An account per round: a recovery mail goes once a minute.
        let email = &format!("change-vs-recovery-{round}@example.com");
        let (_, keys) = signup_with_material(&stack, email, password).await;
        let (_, cookie) = verify_then_login(&stack, email, password).await;
        let token = recovery_token(&stack, email).await;
        let before = stack.app_state.db.find_user_by_email(email).await.unwrap().unwrap().session_epoch;
        let current_key = auth_key_of(&stack, email, password).await;
        let recovered = format!("recovered in round {round}");
        let body = recovery_body(&keys, &recovered);
        let recover = stack.http.post(format!("{}/recover/{token}/complete", stack.base_url)).json(&body).send();
        let changing = Credential::Cookie(&cookie);
        let (change, recover) = tokio::join!(change_password(&stack, &changing, &keys, &current_key, "the change's password"), recover);
        assert_eq!(recover.unwrap().status(), 200, "round {round}: the recovery always completes");
        if change.status() != 200 {
            refused_in_a_sentence(change, "the change").await;
        }
        let user = stack.app_state.db.find_user_by_email(email).await.unwrap().unwrap();
        assert_eq!(user.kdf_salt.as_deref(), body["kdf"]["salt"].as_str(), "round {round}: the recovery's password is the account's");
        assert_eq!(user.recovery_salt.as_deref(), body["recovery_salt"].as_str(), "round {round}: and its recovery code");
        assert!(user.session_epoch > before, "round {round}: the epoch only grows");
        assert_eq!(me_status(&stack, &changing).await, 401, "round {round}: the changing session is signed out");
    }
}

/// Two password changes at once, from two sessions that both checked the
/// old password: one wins, the other writes nothing, and only the winner's
/// session stays.
#[tokio::test(flavor = "multi_thread")]
async fn two_password_changes_at_once_leave_one_password() {
    let stack = skip_without_rhypedb!();
    let (email, password) = ("two-changes@example.com", "the first password");
    let (_, keys) = signup_with_material(&stack, email, password).await;
    let (_, _) = verify_then_login(&stack, email, password).await;
    let mut current = password.to_string();
    for round in 0..RACE_ROUNDS {
        let (_, first_cookie) = login(&stack, email, &current).await;
        let (_, second_cookie) = login(&stack, email, &current).await;
        let key = auth_key_of(&stack, email, &current).await;
        let (first, second) = (Credential::Cookie(&first_cookie), Credential::Cookie(&second_cookie));
        let (first_new, second_new) = (format!("first, round {round}"), format!("second, round {round}"));
        let (a, b) = tokio::join!(change_password(&stack, &first, &keys, &key, &first_new), change_password(&stack, &second, &keys, &key, &second_new));
        let statuses = [a.status().as_u16(), b.status().as_u16()];
        assert_eq!(statuses.iter().filter(|s| **s == 200).count(), 1, "round {round}: exactly one wins, got {statuses:?}");
        let (winner, loser, winning, losing) =
            if a.status() == 200 { (&first, &second, first_new, second_new) } else { (&second, &first, second_new, first_new) };
        refused_in_a_sentence(if a.status() == 200 { b } else { a }, "the second change").await;
        assert_eq!(me_status(&stack, winner).await, 200, "round {round}: the winner stays signed in");
        assert_eq!(me_status(&stack, loser).await, 401, "round {round}: the loser is one of the others");
        let login_status = |password: String| {
            let stack = &stack;
            async move {
                let kdf = fetch_kdf(stack, email).await;
                let auth_key = pimble_crypto::encode_auth_key(&pimble_crypto::derive_password_keys(&password, &kdf).unwrap().auth_key);
                stack.http.post(format!("{}/login", stack.base_url)).json(&json!({ "email": email, "auth_key": auth_key })).send().await.unwrap().status()
            }
        };
        assert_eq!(login_status(losing).await, 401, "round {round}: the loser's password was never written");
        assert_eq!(login_status(winning.clone()).await, 200, "round {round}: the winner's is the account's");
        current = winning;
    }
}

/// A new recovery code made from a session a recovery is ending at that
/// moment never replaces the code the recovery set.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_recovery_code_racing_a_recovery_never_replaces_its_code() {
    let stack = skip_without_rhypedb!();
    let password = "the first password";
    for round in 0..RACE_ROUNDS {
        // An account per round: a recovery mail goes once a minute.
        let email = &format!("code-vs-recovery-{round}@example.com");
        let (_, keys) = signup_with_material(&stack, email, password).await;
        let (_, cookie) = verify_then_login(&stack, email, password).await;
        let current = auth_key_of(&stack, email, password).await;
        let token = recovery_token(&stack, email).await;
        let recovered = format!("recovered in round {round}");
        let body = recovery_body(&keys, &recovered);
        let recover = stack.http.post(format!("{}/recover/{token}/complete", stack.base_url)).json(&body).send();
        let new_code = stack.http.post(format!("{}/me/recovery-code", stack.base_url)).header("Cookie", &cookie).json(&recovery_code_body(&keys, &current)).send();
        let (recover, new_code) = tokio::join!(recover, new_code);
        assert_eq!(recover.unwrap().status(), 200, "round {round}: the recovery always completes");
        let new_code = new_code.unwrap();
        if new_code.status() != 200 {
            refused_in_a_sentence(new_code, "the new code").await;
        }
        let user = stack.app_state.db.find_user_by_email(email).await.unwrap().unwrap();
        // A code made before the recovery is replaced by it; one made after
        // is refused. Either way the recovery's code is the account's.
        assert_eq!(user.recovery_salt.as_deref(), body["recovery_salt"].as_str(), "round {round}: the recovery's code is the account's");
    }
}
