//! A store always keeps an owner, even when two owners drop each other at the
//! same moment: every change that can take an owner away counts the owners
//! and writes under the store's owner lock (`AppState::store_owner_locks`).
//! Each test fires both requests at once and asserts that exactly one wins.

use super::*;

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

const ROUNDS: usize = 3;

#[tokio::test(flavor = "multi_thread")]
async fn two_owners_demoting_each_other_at_once_leave_one_owner() {
    let stack = skip_without_rhypedb!();
    let (a, b) = ("demote-a@example.com", "demote-b@example.com");
    let (_, a_cookie) = signup_verify_login(&stack, a, "owner a password!").await;
    let (_, b_cookie) = signup_verify_login(&stack, b, "owner b password!").await;
    for round in 0..ROUNDS {
        let store_id = two_owners(&stack, &a_cookie, b, &format!("Demoted {round}")).await;
        let (first, second) = tokio::join!(demote(&stack, &a_cookie, &store_id, b).send(), demote(&stack, &b_cookie, &store_id, a).send());
        exactly_one_wins(first.unwrap(), second.unwrap(), "demotions").await;
        assert_eq!(owners_of(&stack, &store_id).await, 1, "round {round}: an owner remains");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_owners_removing_each_other_at_once_leave_one_owner() {
    let stack = skip_without_rhypedb!();
    let (a, b) = ("remove-a@example.com", "remove-b@example.com");
    let (_, a_cookie) = signup_verify_login(&stack, a, "owner a password!").await;
    let (_, b_cookie) = signup_verify_login(&stack, b, "owner b password!").await;
    let (a_id, b_id) = (user_id(&stack, &a_cookie).await, user_id(&stack, &b_cookie).await);
    for round in 0..ROUNDS {
        let store_id = two_owners(&stack, &a_cookie, b, &format!("Removed {round}")).await;
        let (first, second) = tokio::join!(remove(&stack, &a_cookie, &store_id, &b_id).send(), remove(&stack, &b_cookie, &store_id, &a_id).send());
        exactly_one_wins(first.unwrap(), second.unwrap(), "removals").await;
        assert_eq!(owners_of(&stack, &store_id).await, 1, "round {round}: an owner remains");
    }
}

/// Leaving (removing oneself) while demoting the other owner: the two can
/// only both succeed by leaving nobody.
#[tokio::test(flavor = "multi_thread")]
async fn an_owner_leaving_while_demoting_the_other_leaves_one_owner() {
    let stack = skip_without_rhypedb!();
    let (a, b) = ("leave-a@example.com", "leave-b@example.com");
    let (_, a_cookie) = signup_verify_login(&stack, a, "owner a password!").await;
    let (_, b_cookie) = signup_verify_login(&stack, b, "owner b password!").await;
    let a_id = user_id(&stack, &a_cookie).await;
    for round in 0..ROUNDS {
        let store_id = two_owners(&stack, &a_cookie, b, &format!("Left {round}")).await;
        let (first, second) = tokio::join!(remove(&stack, &a_cookie, &store_id, &a_id).send(), demote(&stack, &b_cookie, &store_id, a).send());
        let (first, second) = (first.unwrap(), second.unwrap());
        // Both may win when the demotion goes first: `a` is an editor then,
        // and an editor leaving drops no owner.
        let statuses = [first.status().as_u16(), second.status().as_u16()];
        assert!(statuses.contains(&200), "round {round}: one of them wins, got {statuses:?}");
        assert_eq!(owners_of(&stack, &store_id).await, 1, "round {round}: an owner remains, got {statuses:?}");
    }
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
    let mut tokens = Vec::new();
    for email in [a, b] {
        let resp = stack.http.post(format!("{}/recover/start", stack.base_url)).json(&json!({ "email": email })).send().await.unwrap();
        assert_eq!(resp.status(), 202);
        tokens.push(extract_recovery_token(&stack, email));
    }
    let delete = |token: &str| stack.http.post(format!("{}/recover/{token}/delete-account", stack.base_url)).send();
    let (first, second) = tokio::join!(delete(&tokens[0]), delete(&tokens[1]));
    assert_eq!(first.unwrap().status(), 200);
    assert_eq!(second.unwrap().status(), 200);
    let store = stack.app_state.db.find_hosted_store(&store_id).await.unwrap().unwrap();
    assert!(store.deleted, "the store went with its last owner");
}
