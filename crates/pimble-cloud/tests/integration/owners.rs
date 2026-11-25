//! A store always keeps an owner, even when two owners drop each other at the
//! same moment: every change that can take an owner away counts the owners
//! and writes under the store's owner lock (`AppState::store_owner_locks`).
//!
//! Two requests fired together rarely interleave where it matters, so each
//! race here is made to overlap: the first request is held at a checkpoint
//! inside the service (`pimble_cloud::checkpoints`, the `test-hooks`
//! feature) right after it counted the owners, and let go only once the
//! second has counted too or is provably queued behind the first's lock.
//! With the locks gone every test here fails; with them, exactly one wins.

use super::*;
use pimble_cloud::checkpoints::Checkpoint;

/// The `/me` id of the account behind `cookie`.
async fn user_id(stack: &Stack, cookie: &str) -> String {
    let me: Value = stack.http.get(format!("{}/me", stack.base_url)).header("Cookie", cookie).send().await.unwrap().json().await.unwrap();
    me["id"].as_str().unwrap().to_string()
}

/// A store `a` created and shared whole with `b` as a second owner.
async fn two_owners(stack: &Stack, a_cookie: &str, b_email: &str, name: &str) -> String {
    let store = create_store_with_body(stack, a_cookie, json!({ "name": name })).await;
    let store_id = store["store_id"].as_str().unwrap().to_string();
    let resp = stack
        .http
        .put(format!("{}/stores/{store_id}/members", stack.base_url))
        .header("Cookie", a_cookie)
        .json(&json!({ "email": b_email, "role": "owner" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "the second owner is added");
    assert_eq!(owners_of(stack, &store_id).await, 2);
    store_id
}

/// How many whole-store owners `store_id` has, read from the database.
async fn owners_of(stack: &Stack, store_id: &str) -> usize {
    let store = stack.app_state.db.find_hosted_store(store_id).await.unwrap().unwrap();
    stack.app_state.db.grants_for_store(store.rid).await.unwrap().into_iter().filter(|g| g.role == "owner" && g.is_whole_store()).count()
}

fn demote(stack: &Stack, cookie: &str, store_id: &str, email: &str) -> reqwest::RequestBuilder {
    stack
        .http
        .put(format!("{}/stores/{store_id}/members", stack.base_url))
        .header("Cookie", cookie)
        .json(&json!({ "email": email, "role": "editor" }))
}

fn remove(stack: &Stack, cookie: &str, store_id: &str, user_id: &str) -> reqwest::RequestBuilder {
    stack.http.delete(format!("{}/stores/{store_id}/members/{user_id}", stack.base_url)).header("Cookie", cookie)
}

/// Every wait in these tests ends by this.
const DEADLINE: Duration = Duration::from_secs(20);

/// Sends `first` and holds it right after it counted the owners; then sends
/// `second` and lets `first` go once `second` has counted too (nothing kept
/// it out: the race the locks exist for) or waits for the lock on
/// `store_id` that `first` holds. Both answers, in that order.
async fn overlapped(stack: &Stack, store_id: &str, first: reqwest::RequestBuilder, second: reqwest::RequestBuilder) -> (reqwest::Response, reqwest::Response) {
    let hold = stack.app_state.checkpoints.hold(Checkpoint::OwnersCounted);
    let first = tokio::spawn(first.send());
    tokio::time::timeout(DEADLINE, hold.arrived(1)).await.expect("the first request counts the owners");
    let second = tokio::spawn(second.send());
    let locks = &stack.app_state.store_owner_locks;
    tokio::time::timeout(DEADLINE, async {
        while hold.arrivals() < 2 && locks.waiting(store_id) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the second request counts the owners or waits for the first's lock");
    stack.app_state.checkpoints.disarm(Checkpoint::OwnersCounted);
    let first = tokio::time::timeout(DEADLINE, first).await.expect("the first request answers").unwrap().unwrap();
    let second = tokio::time::timeout(DEADLINE, second).await.expect("the second request answers").unwrap().unwrap();
    (first, second)
}

/// Exactly one of two answers is a success; the other is the last-owner
/// refusal, or a 403 when the loser had already stopped being an owner by
/// the time its request was judged.
async fn exactly_one_wins(first: reqwest::Response, second: reqwest::Response, what: &str) {
    let statuses = [first.status().as_u16(), second.status().as_u16()];
    assert_eq!(statuses.iter().filter(|s| **s == 200).count(), 1, "{what}: exactly one wins, got {statuses:?}");
    for resp in [first, second] {
        if resp.status() != 200 {
            let status = resp.status().as_u16();
            let body: Value = resp.json().await.unwrap();
            assert!(status == 409 || status == 403, "{what}: the loser is refused, got {status} {body}");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_owners_demoting_each_other_at_once_leave_one_owner() {
    let stack = skip_without_rhypedb!();
    let (a, b) = ("demote-a@example.com", "demote-b@example.com");
    let (_, a_cookie) = signup_verify_login(&stack, a, "owner a password!").await;
    let (_, b_cookie) = signup_verify_login(&stack, b, "owner b password!").await;
    let store_id = two_owners(&stack, &a_cookie, b, "Demoted").await;
    let (first, second) = overlapped(&stack, &store_id, demote(&stack, &a_cookie, &store_id, b), demote(&stack, &b_cookie, &store_id, a)).await;
    exactly_one_wins(first, second, "demotions").await;
    assert_eq!(owners_of(&stack, &store_id).await, 1, "an owner remains");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_owners_removing_each_other_at_once_leave_one_owner() {
    let stack = skip_without_rhypedb!();
    let (a, b) = ("remove-a@example.com", "remove-b@example.com");
    let (_, a_cookie) = signup_verify_login(&stack, a, "owner a password!").await;
    let (_, b_cookie) = signup_verify_login(&stack, b, "owner b password!").await;
    let (a_id, b_id) = (user_id(&stack, &a_cookie).await, user_id(&stack, &b_cookie).await);
    let store_id = two_owners(&stack, &a_cookie, b, "Removed").await;
    let (first, second) = overlapped(&stack, &store_id, remove(&stack, &a_cookie, &store_id, &b_id), remove(&stack, &b_cookie, &store_id, &a_id)).await;
    exactly_one_wins(first, second, "removals").await;
    assert_eq!(owners_of(&stack, &store_id).await, 1, "an owner remains");
}

/// One owner leaving (removing themself) while demoting the other owner: the
/// two can only both succeed by leaving nobody.
#[tokio::test(flavor = "multi_thread")]
async fn an_owner_leaving_while_demoting_the_other_leaves_one_owner() {
    let stack = skip_without_rhypedb!();
    let (a, b) = ("leave-a@example.com", "leave-b@example.com");
    let (_, a_cookie) = signup_verify_login(&stack, a, "owner a password!").await;
    let (_, _b_cookie) = signup_verify_login(&stack, b, "owner b password!").await;
    let a_id = user_id(&stack, &a_cookie).await;
    let store_id = two_owners(&stack, &a_cookie, b, "Left").await;
    let (first, second) = overlapped(&stack, &store_id, remove(&stack, &a_cookie, &store_id, &a_id), demote(&stack, &a_cookie, &store_id, b)).await;
    exactly_one_wins(first, second, "leaving and demoting").await;
    assert_eq!(owners_of(&stack, &store_id).await, 1, "an owner remains");
}

/// The recovery tokens of `emails`, each asked for through `/recover/start`.
async fn recovery_tokens(stack: &Stack, emails: &[&str]) -> Vec<String> {
    let mut tokens = Vec::new();
    for email in emails {
        let resp = stack.http.post(format!("{}/recover/start", stack.base_url)).json(&json!({ "email": email })).send().await.unwrap();
        assert_eq!(resp.status(), 202);
        tokens.push(extract_recovery_token(stack, email));
    }
    tokens
}

fn delete_account(stack: &Stack, token: &str) -> reqwest::RequestBuilder {
    stack.http.post(format!("{}/recover/{token}/delete-account", stack.base_url))
}

/// Two co-owners deleting their accounts through recovery at once: the store
/// is deleted with the last of them, never left live with no owner.
#[tokio::test(flavor = "multi_thread")]
async fn two_owners_deleting_their_accounts_at_once_delete_the_store() {
    let stack = skip_without_rhypedb!();
    let (a, b) = ("gone-a@example.com", "gone-b@example.com");
    let (_, a_cookie) = signup_verify_login(&stack, a, "owner a password!").await;
    let (_, _b_cookie) = signup_verify_login(&stack, b, "owner b password!").await;
    let store_id = two_owners(&stack, &a_cookie, b, "Both gone").await;
    let tokens = recovery_tokens(&stack, &[a, b]).await;
    let (first, second) = overlapped(&stack, &store_id, delete_account(&stack, &tokens[0]), delete_account(&stack, &tokens[1])).await;
    assert_eq!(first.status(), 200);
    assert_eq!(second.status(), 200);
    let store = stack.app_state.db.find_hosted_store(&store_id).await.unwrap().unwrap();
    assert!(store.deleted, "the store went with its last owner");
}

/// An account deletion lists the account's grants before it takes any
/// store's lock. A co-owner demoting the account in that gap leaves it an
/// editor, and the deletion must judge the grant as it is then: deleting
/// the store because the stale listing said "owner" and only the demoter is
/// left would take the demoter's store from them.
#[tokio::test(flavor = "multi_thread")]
async fn an_account_deleted_while_its_co_owner_demotes_it_leaves_the_co_owners_store() {
    let stack = skip_without_rhypedb!();
    let (a, b) = ("demoted-gone-a@example.com", "demoted-gone-b@example.com");
    let (_, a_cookie) = signup_verify_login(&stack, a, "owner a password!").await;
    let (_, b_cookie) = signup_verify_login(&stack, b, "owner b password!").await;
    let store_id = two_owners(&stack, &a_cookie, b, "Kept by b").await;
    let tokens = recovery_tokens(&stack, &[a]).await;

    let hold = stack.app_state.checkpoints.hold(Checkpoint::AccountGrantsListed);
    let deletion = tokio::spawn(delete_account(&stack, &tokens[0]).send());
    tokio::time::timeout(DEADLINE, hold.arrived(1)).await.expect("the deletion lists a's grants");
    let demoted = tokio::time::timeout(DEADLINE, demote(&stack, &b_cookie, &store_id, a).send()).await.expect("the demotion answers").unwrap();
    assert_eq!(demoted.status(), 200, "b demotes a while a's deletion waits");
    stack.app_state.checkpoints.disarm(Checkpoint::AccountGrantsListed);
    let deleted = tokio::time::timeout(DEADLINE, deletion).await.expect("the deletion answers").unwrap().unwrap();
    assert_eq!(deleted.status(), 200);

    let store = stack.app_state.db.find_hosted_store(&store_id).await.unwrap().unwrap();
    assert!(!store.deleted, "b's store survives a's deletion");
    let b_user = stack.app_state.db.find_user_by_email(b).await.unwrap().expect("b's account");
    let b_grants = stack.app_state.db.grants_for_user_and_store(b_user.rid, store.rid).await.unwrap();
    assert!(b_grants.iter().any(|g| g.role == "owner" && g.is_whole_store()), "b is still the store's owner, got {b_grants:?}");
    assert_eq!(owners_of(&stack, &store_id).await, 1, "b is the one owner");
}
