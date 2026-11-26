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
//!
//! Every write of a grant (a member added or changed, a store created for its
//! owner, a key given, an invitation claimed at login) takes the account's
//! lock first (`AppState::lock_live_account`), which an account deletion
//! holds from listing the account's grants until the account is gone. The
//! tests at the end hold a deletion right after that listing, send each of
//! those writes for the account, and let the deletion go once the write is
//! queued on the lock: the write finds the account gone and leaves nothing
//! behind it.

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

/// Waits until a request is queued on the account lock of the account at
/// `email`, which another request holds.
async fn queued_on_account(stack: &Stack, email: &str) {
    let rid = stack.app_state.db.find_user_by_email(email).await.unwrap().expect("the account").rid;
    let locks = &stack.app_state.account_locks;
    tokio::time::timeout(DEADLINE, async {
        while locks.waiting(&rid.to_string()) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the request waits for the account's lock");
}

/// `resp` is the refusal of a grant for an account deleted while it waited.
async fn refused_as_deleted(resp: reqwest::Response, what: &str) {
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(status, 409, "{what}: refused, got {status} {body}");
    assert_eq!(body["message"], pimble_cloud::routes::ACCOUNT_DELETED_REFUSAL, "{what}: said in a sentence");
}

/// Holds `a`'s deletion (by `token`) right after it listed its grants, sends
/// `change` (a grant made or changed for `a`) and lets the deletion go once
/// `change` waits for `a`'s account lock; the deletion's answer and the
/// change's.
async fn change_during_deletion(stack: &Stack, a: &str, token: &str, change: reqwest::RequestBuilder) -> (reqwest::Response, reqwest::Response) {
    let hold = stack.app_state.checkpoints.hold(Checkpoint::AccountGrantsListed);
    let deletion = tokio::spawn(delete_account(stack, token).send());
    tokio::time::timeout(DEADLINE, hold.arrived(1)).await.expect("the deletion lists a's grants");
    let change = tokio::spawn(change.send());
    queued_on_account(stack, a).await;
    stack.app_state.checkpoints.disarm(Checkpoint::AccountGrantsListed);
    let deleted = tokio::time::timeout(DEADLINE, deletion).await.expect("the deletion answers").unwrap().unwrap();
    let changed = tokio::time::timeout(DEADLINE, change).await.expect("the change answers").unwrap().unwrap();
    (deleted, changed)
}

/// An account deletion lists the account's grants before it takes any
/// store's lock. A co-owner demoting the account in that gap waits for the
/// account's lock, which the deletion holds, and is refused once the account
/// is gone; the deletion judges the grant as it is then, so the co-owner's
/// store stays theirs.
#[tokio::test(flavor = "multi_thread")]
async fn an_account_deleted_while_its_co_owner_demotes_it_leaves_the_co_owners_store() {
    let stack = skip_without_rhypedb!();
    let (a, b) = ("demoted-gone-a@example.com", "demoted-gone-b@example.com");
    let (_, a_cookie) = signup_verify_login(&stack, a, "owner a password!").await;
    let (_, b_cookie) = signup_verify_login(&stack, b, "owner b password!").await;
    let store_id = two_owners(&stack, &a_cookie, b, "Kept by b").await;
    let tokens = recovery_tokens(&stack, &[a]).await;

    let (deleted, demoted) = change_during_deletion(&stack, a, &tokens[0], demote(&stack, &b_cookie, &store_id, a)).await;
    assert_eq!(deleted.status(), 200);
    refused_as_deleted(demoted, "the demotion").await;

    let store = stack.app_state.db.find_hosted_store(&store_id).await.unwrap().unwrap();
    assert!(!store.deleted, "b's store survives a's deletion");
    let b_user = stack.app_state.db.find_user_by_email(b).await.unwrap().expect("b's account");
    let b_grants = stack.app_state.db.grants_for_user_and_store(b_user.rid, store.rid).await.unwrap();
    assert!(b_grants.iter().any(|g| g.role == "owner" && g.is_whole_store()), "b is still the store's owner, got {b_grants:?}");
    assert_eq!(owners_of(&stack, &store_id).await, 1, "b is the one owner");
}

/// b makes a an owner of b's own store while a's deletion has listed a's
/// grants: the grant waits for a's account lock and is refused once a is
/// gone, so b's store never counts a deleted account as an owner, and b, its
/// one live owner, cannot leave it ownerless.
#[tokio::test(flavor = "multi_thread")]
async fn a_grant_made_while_its_account_is_deleted_does_not_survive_it() {
    let stack = skip_without_rhypedb!();
    let (a, b) = ("granted-gone-a@example.com", "granted-gone-b@example.com");
    let (_, a_cookie) = signup_verify_login(&stack, a, "owner a password!").await;
    let (_, b_cookie) = signup_verify_login(&stack, b, "owner b password!").await;
    let b_id = user_id(&stack, &b_cookie).await;
    let s1 = create_store_with_body(&stack, &a_cookie, json!({ "name": "a's own" })).await["store_id"].as_str().unwrap().to_string();
    let s2 = create_store_with_body(&stack, &b_cookie, json!({ "name": "b's own" })).await["store_id"].as_str().unwrap().to_string();
    let tokens = recovery_tokens(&stack, &[a]).await;

    let made_owner = stack.http.put(format!("{}/stores/{s2}/members", stack.base_url)).header("Cookie", &b_cookie).json(&json!({ "email": a, "role": "owner" }));
    let (deleted, granted) = change_during_deletion(&stack, a, &tokens[0], made_owner).await;
    assert_eq!(deleted.status(), 200);
    refused_as_deleted(granted, "the grant").await;

    assert!(stack.app_state.db.find_user_by_email(a).await.unwrap().is_none(), "a is deleted");
    let s1_row = stack.app_state.db.find_hosted_store(&s1).await.unwrap().unwrap();
    assert!(s1_row.deleted, "a's own store went with its one owner");
    let s2_row = stack.app_state.db.find_hosted_store(&s2).await.unwrap().unwrap();
    assert!(!s2_row.deleted);
    for grant in stack.app_state.db.grants_for_store(s2_row.rid).await.unwrap() {
        assert!(stack.app_state.db.get_user(grant.user_rid).await.unwrap().is_some(), "every grant on b's store is a live account's, got {grant:?}");
    }
    assert_eq!(owners_of(&stack, &s2).await, 1, "b is the one owner");
    let left = remove(&stack, &b_cookie, &s2, &b_id).send().await.unwrap();
    assert_eq!(left.status(), 409, "b cannot leave b's store with no live owner");
    assert_eq!(owners_of(&stack, &s2).await, 1);
}

/// `email`'s account row id, while it exists.
async fn rid_of(stack: &Stack, email: &str) -> u64 {
    stack.app_state.db.find_user_by_email(email).await.unwrap().expect("the account").rid
}

/// Nothing is left that names the deleted account `rid`: no grant, no key,
/// no session.
async fn nothing_left_for(stack: &Stack, rid: u64, what: &str) {
    assert!(stack.app_state.db.get_user(rid).await.unwrap().is_none(), "{what}: the account is deleted");
    let grants = stack.app_state.db.grants_for_user(rid).await.unwrap();
    assert!(grants.is_empty(), "{what}: no grant names the deleted account, got {grants:?}");
    let keys = stack.app_state.db.key_grants_for_user(rid).await.unwrap();
    assert!(keys.is_empty(), "{what}: no key is held for the deleted account, got {} key grant(s)", keys.len());
    let sessions = stack.app_state.db.sessions_for_user(rid).await.unwrap();
    assert!(sessions.is_empty(), "{what}: no session is left for the deleted account, got {} session(s)", sessions.len());
}

/// a creates a store while a's deletion has listed a's grants: the creation
/// waits for a's account lock and makes nothing, so no store is left whose
/// one owner is a deleted account.
#[tokio::test(flavor = "multi_thread")]
async fn a_store_made_while_its_account_is_deleted_is_not_made() {
    let stack = skip_without_rhypedb!();
    let a = "created-gone-a@example.com";
    let (_, a_cookie) = signup_verify_login(&stack, a, "owner a password!").await;
    let a_rid = rid_of(&stack, a).await;
    let tokens = recovery_tokens(&stack, &[a]).await;

    let create = stack.http.post(format!("{}/stores", stack.base_url)).header("Cookie", &a_cookie).json(&json!({ "name": "Made too late" }));
    let (deleted, created) = change_during_deletion(&stack, a, &tokens[0], create).await;
    assert_eq!(deleted.status(), 200);
    let status = created.status().as_u16();
    let body: Value = created.json().await.unwrap();
    assert_eq!(status, 409, "the creation is refused, got {status} {body}");
    assert_eq!(body["message"], "This account was deleted while the store waited to be made, so nothing was made.", "said in a sentence");
    nothing_left_for(&stack, a_rid, "the creation").await;
}

/// b, the owner, gives a (a member) the store's key while a's deletion has
/// listed a's keys: the key waits for a's account lock and is refused once a
/// is gone, so no key is held for an account that is no more.
#[tokio::test(flavor = "multi_thread")]
async fn a_key_given_while_its_account_is_deleted_is_not_kept() {
    let stack = skip_without_rhypedb!();
    let (a, b) = ("keyed-gone-a@example.com", "keyed-gone-b@example.com");
    let (_, a_keys) = signup_with_material(&stack, a, "member a password!").await;
    let (a_login, _a_cookie) = verify_then_login(&stack, a, "member a password!").await;
    let (_, b_keys) = signup_with_material(&stack, b, "owner b password!").await;
    let (_, b_cookie) = verify_then_login(&stack, b, "owner b password!").await;
    let a_id = a_login["user"]["id"].as_str().unwrap().to_string();
    let a_rid = rid_of(&stack, a).await;
    let store_id = create_store_with_body(&stack, &b_cookie, json!({ "name": "Keyed", "kind": "vault" })).await["store_id"].as_str().unwrap().to_string();
    assert_eq!(put_member(&stack, &b_cookie, &store_id, a, "editor").await.status(), 200, "a is a member");
    let tokens = recovery_tokens(&stack, &[a]).await;

    let key_id = uuid::Uuid::new_v4();
    let envelope = wrap_store_key(&pimble_crypto::SymmetricKey::generate(), key_id, &store_id, &b_keys, &a_keys.public_keys());
    let give = stack
        .http
        .put(format!("{}/stores/{store_id}/keys", stack.base_url))
        .header("Cookie", &b_cookie)
        .json(&json!({ "envelopes": [ { "user_id": a_id, "key_id": key_id.to_string(), "envelope": envelope } ] }));
    let (deleted, given) = change_during_deletion(&stack, a, &tokens[0], give).await;
    assert_eq!(deleted.status(), 200);
    let status = given.status().as_u16();
    let body: Value = given.json().await.unwrap();
    assert_eq!(status, 409, "the key is refused, got {status} {body}");
    assert_eq!(body["message"], "That account was deleted while this change waited, so no key was given to it.", "said in a sentence");
    nothing_left_for(&stack, a_rid, "the key").await;
    let store = stack.app_state.db.find_hosted_store(&store_id).await.unwrap().unwrap();
    assert!(!store.deleted, "b's store survives a's deletion");
    assert_eq!(owners_of(&stack, &store_id).await, 1, "b is still its owner");
}

/// a logs in, which claims the invitations standing for a's address, while
/// a's deletion has listed a's grants: the login waits for a's account lock,
/// finds the account gone, and claims nothing and opens no session; the
/// invitation is left for the address's next account.
#[tokio::test(flavor = "multi_thread")]
async fn an_invitation_claimed_while_its_account_is_deleted_is_left_standing() {
    let stack = skip_without_rhypedb!();
    let (a, b) = ("claimed-gone-a@example.com", "claimed-gone-b@example.com");
    let a_password = "member a password!";
    signup_verify_login(&stack, a, a_password).await;
    let (_, b_cookie) = signup_verify_login(&stack, b, "owner b password!").await;
    let a_rid = rid_of(&stack, a).await;
    let b_rid = rid_of(&stack, b).await;
    let store_id = create_store_with_body(&stack, &b_cookie, json!({ "name": "Invited" })).await["store_id"].as_str().unwrap().to_string();
    let store = stack.app_state.db.find_hosted_store(&store_id).await.unwrap().unwrap();
    // An invitation that stands for a's address although a has an account:
    // one sent before a's account existed whose claim at verification
    // failed, which every login tries again.
    stack.app_state.db.create_invitation(store.rid, &store_id, a, "editor", "", "", b_rid).await.unwrap();
    let tokens = recovery_tokens(&stack, &[a]).await;

    let auth_key = auth_key_of(&stack, a, a_password).await;
    let log_in = stack.http.post(format!("{}/login", stack.base_url)).json(&json!({ "email": a, "auth_key": auth_key }));
    let (deleted, logged_in) = change_during_deletion(&stack, a, &tokens[0], log_in).await;
    assert_eq!(deleted.status(), 200);
    let status = logged_in.status().as_u16();
    let body: Value = logged_in.json().await.unwrap();
    assert_eq!(status, 401, "no login into a deleted account, got {status} {body}");
    nothing_left_for(&stack, a_rid, "the claim").await;
    let standing = stack.app_state.db.invitations_for_email(a).await.unwrap();
    assert_eq!(standing.len(), 1, "the invitation is left for the address's next account");
    assert_eq!(standing[0].store_rid, store.rid);
}
