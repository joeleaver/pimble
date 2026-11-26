//! `POST /me/recovery-code`: a new recovery code replaces the old one's
//! material, proved by the current password the way a password change is,
//! and every change of an account's recovery code mails the account (Joe,
//! 2026-11-25: "Password and an email"). A recovery sets a new code too, and
//! mails the same.

use super::*;

/// A new code for `keys` and the body that sets it, proved by
/// `current_auth_key` (left out when `None`).
fn new_code_body(keys: &AccountKeys, current_auth_key: Option<&str>) -> (String, Value) {
    let code = pimble_crypto::generate_recovery_code();
    let params = KdfParams { salt: KdfParams::generate().salt, m_cost: pimble_crypto::KDF_M_COST_KIB, t_cost: pimble_crypto::KDF_T_COST, p_cost: pimble_crypto::KDF_P_COST };
    let kek = pimble_crypto::derive_recovery_kek(&code, &params).unwrap();
    let mut body = json!({ "recovery_salt": params.salt, "recovery_key_blob": pimble_crypto::wrap_account_keys(keys, &kek).unwrap() });
    if let Some(key) = current_auth_key {
        body["current_auth_key"] = json!(key);
    }
    (code, body)
}

async fn post_new_code(stack: &Stack, cookie: &str, body: &Value) -> reqwest::Response {
    stack.http.post(format!("{}/me/recovery-code", stack.base_url)).header("Cookie", cookie).json(body).send().await.unwrap()
}

/// What the "recovery code changed" mail says first.
const CHANGED: &str = "The recovery code of your Pimble account was just replaced";

/// Asserts `step` mailed `email` exactly one mail, the "changed" one.
fn mailed_once(stack: &Stack, email: &str, before: usize, step: &str) {
    assert_eq!(mail_count(stack, email), before + 1, "{step}: exactly one mail");
    assert!(last_mail(stack, email).contains(CHANGED), "{step}: the mail says the recovery code changed");
}

/// Asks for a recovery link and opens the account with `code`.
async fn recovers_with(stack: &Stack, email: &str, code: &str) -> (String, AccountKeys) {
    let resp = stack.http.post(format!("{}/recover/start", stack.base_url)).json(&json!({ "email": email })).send().await.unwrap();
    assert_eq!(resp.status(), 202);
    let token = extract_recovery_token(stack, email);
    let (_, keys) = fetch_and_unwrap_recovery_material(stack, &token, code).await;
    (token, keys)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_recovery_code_recovers_the_account_and_mails_it_once() {
    let stack = skip_without_rhypedb!();
    let (email, password) = ("change-recovery@example.com", "recovery code owner password!!");
    let (_, keys) = signup_with_material(&stack, email, password).await;
    let (_, cookie) = verify_then_login(&stack, email, password).await;
    let before = mail_count(&stack, email);
    let key = auth_key_of(&stack, email, password).await;
    let (code, body) = new_code_body(&keys, Some(&key));
    assert_eq!(post_new_code(&stack, &cookie, &body).await.status(), 200);
    mailed_once(&stack, email, before, "a new code");
    let mail = last_mail(&stack, email);
    assert!(!mail.contains(&code), "the mail carries no secret");
    assert!(mail.contains("If you did not"), "and says what to do if the person did not make the change");

    // The new code really recovers the account now, and the recovery, which
    // sets a code of its own, mails once more.
    let (token, opened) = recovers_with(&stack, email, &code).await;
    assert_eq!(opened.encryption_secret, keys.encryption_secret);
    assert_eq!(opened.signing_secret, keys.signing_secret);
    let before = mail_count(&stack, email);
    complete_recovery_with_new_material(&stack, &token, &opened, "a password after recovery").await;
    mailed_once(&stack, email, before, "a recovery's new code");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_recovery_code_without_the_current_password_is_refused() {
    let stack = skip_without_rhypedb!();
    let (email, password) = ("unproved-code@example.com", "the account's password");
    let (_, keys, old_code) = signup_with_material_and_recovery_code(&stack, email, password).await;
    let (_, cookie) = verify_then_login(&stack, email, password).await;
    let before = mail_count(&stack, email);

    let (_, no_password) = new_code_body(&keys, None);
    let resp = post_new_code(&stack, &cookie, &no_password).await;
    assert!(resp.status().is_client_error(), "no password: refused, got {}", resp.status());
    let (_, empty) = new_code_body(&keys, Some(""));
    let resp = post_new_code(&stack, &cookie, &empty).await;
    assert_eq!(resp.status(), 401, "an empty password is a wrong one");
    let wrong_key = auth_key_of(&stack, email, "not the password").await;
    let (_, wrong) = new_code_body(&keys, Some(&wrong_key));
    let resp = post_new_code(&stack, &cookie, &wrong).await;
    assert_eq!(resp.status(), 401);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "wrong_current_password");

    assert_eq!(mail_count(&stack, email), before, "nothing changed, nothing mailed");
    let (_, opened) = recovers_with(&stack, email, &old_code).await;
    assert_eq!(opened.encryption_secret, keys.encryption_secret, "the old code still recovers the account");
}
