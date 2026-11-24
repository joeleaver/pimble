//! Wave 3: watches (docs/NOTIFICATIONS_CONTRACT.md section 3), on the
//! integration harness of `tests/integration.rs`: the choices and the
//! registered documents, writes gathered per watcher only through what the
//! watcher holds, counted in spells of editing, one notice after five quiet
//! minutes, the switch, and the writes an owner's relay reports.

use super::*;

use pimble_cloud::watches::{all_interest, interest_of, record_relay_writes, record_writes, watch_pass};
use pimble_core::watch::SPELL_GAP_MS;

async fn put_json(stack: &Stack, cookie: &str, path: &str, body: Value) -> reqwest::Response {
    stack.http.put(format!("{}{path}", stack.base_url)).header("Cookie", cookie).json(&body).send().await.unwrap()
}

async fn get_json(stack: &Stack, cookie: &str, path: &str) -> Value {
    stack.http.get(format!("{}{path}", stack.base_url)).header("Cookie", cookie).send().await.unwrap().json().await.unwrap()
}

async fn uuid_of(stack: &Stack, email: &str) -> String {
    stack.app_state.db.find_user_by_email(email).await.unwrap().expect("an account").user_uuid
}

/// A write to `doc` by `writer`, its document reached by `roots`, made now.
fn write(doc: &str, writer: &str, roots: &[&str]) -> pimble_cloud::watches::Write {
    pimble_cloud::watches::Write { doc: doc.into(), writer: writer.into(), count: 1, roots: roots.iter().map(|r| r.to_string()).collect(), last_ms: 0 }
}

/// The watch notices in `cookie`'s inbox, opened with `notice`.
async fn watch_notices(stack: &Stack, cookie: &str, notice: &pimble_crypto::NoticeKey, user: &str) -> Vec<pimble_rpc::Notice> {
    let inbox = get_json(stack, cookie, "/inbox").await;
    inbox["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            let plain = pimble_crypto::open_notice(item["sealed"].as_str().unwrap(), notice, user).unwrap();
            serde_json::from_slice::<pimble_rpc::Notice>(&plain).unwrap()
        })
        .filter(|n| n.kind == pimble_rpc::NoticeKind::Watch)
        .collect()
}

/// Will chooses watches (sealed, versioned: a stale version is 409) and
/// registers a document. Olga's writes to it, in two spells of editing a
/// minute apart, are gathered, and once she has been quiet five minutes one
/// notice reaches Will: sealed to his notice key, the store, the document,
/// the writer and two spells, no title or text; his own writes and other
/// documents tell him nothing. Its email says who and where, and nothing of
/// the note.
#[tokio::test(flavor = "multi_thread")]
async fn writes_to_a_watched_note_become_one_notice_after_five_quiet_minutes() {
    let stack = skip_without_rhypedb!();
    let (_olga_cookie, will_cookie, will_id, store_id, root, notice) = a_share_with_notice_keys(&stack, "watch").await;
    let olga_id = uuid_of(&stack, "olga.watch@example.com").await;
    let (doc, other) = (node_id(), node_id());

    let held = get_json(&stack, &will_cookie, "/me/watches").await;
    assert_eq!(held["choices"]["version"], 0);
    assert!(held["choices"]["sealed"].is_null());
    let resp = put_json(&stack, &will_cookie, "/me/watches/choices", json!({ "version": 0, "sealed": "c2VhbGVk" })).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.json::<Value>().await.unwrap()["version"], 1);
    assert_eq!(put_json(&stack, &will_cookie, "/me/watches/choices", json!({ "version": 0, "sealed": "b3RoZXI=" })).await.status(), 409, "another device changed them first");
    assert_eq!(put_json(&stack, &will_cookie, &format!("/me/watches/stores/{store_id}"), json!({ "docs": [doc], "version": 0 })).await.status(), 409, "computed from old choices");
    assert_eq!(put_json(&stack, &will_cookie, &format!("/me/watches/stores/{store_id}"), json!({ "docs": ["not an id"], "version": 1 })).await.status(), 400);
    assert_eq!(put_json(&stack, &will_cookie, &format!("/me/watches/stores/{store_id}"), json!({ "docs": [doc, doc], "version": 1 })).await.status(), 200);
    let held = get_json(&stack, &will_cookie, "/me/watches").await;
    assert_eq!(held["choices"], json!({ "version": 1, "sealed": "c2VhbGVk" }));
    assert_eq!(held["stores"], json!([{ "store_id": store_id, "docs": [doc] }]));

    let state = &stack.app_state;
    let t0 = now_ms();
    let reached = [root.as_str()];
    assert_eq!(record_writes(state, &store_id, &[write(&doc, &olga_id, &reached)], t0).await.unwrap(), 1);
    assert_eq!(record_writes(state, &store_id, &[write(&doc, &olga_id, &reached)], t0 + 10_000).await.unwrap(), 1, "the same spell");
    let theirs = [write(&doc, &olga_id, &reached), write(&doc, &will_id, &reached), write(&other, &olga_id, &reached)];
    assert_eq!(record_writes(state, &store_id, &theirs, t0 + MIN).await.unwrap(), 1, "not his own, not a note he does not watch");
    assert_eq!(watch_pass(state, t0 + 5 * MIN).await.unwrap(), 0, "she was quiet four minutes");
    assert_eq!(watch_pass(state, t0 + 6 * MIN + 1).await.unwrap(), 1);
    assert_eq!(watch_pass(state, t0 + 20 * MIN).await.unwrap(), 0, "told once");

    let inbox = get_json(&stack, &will_cookie, "/inbox").await;
    let items = inbox["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["from"], olga_id.as_str());
    assert_eq!(items[0]["root"], root.as_str(), "told through his share");
    let told = &watch_notices(&stack, &will_cookie, &notice, &will_id).await[0];
    assert_eq!(told.from, olga_id);
    assert_eq!(told.node.to_string(), doc);
    assert_eq!(told.store.to_string(), store_id);
    assert_eq!(told.count, 2, "two spells of editing, not four writes");
    assert!(told.snippet.is_empty());
    assert_eq!(told.from_name, "olga.watch");

    // Unseen on a device for fifteen minutes it is emailed, as any notice;
    // this one was drained, so a second one is what is emailed.
    record_writes(state, &store_id, &[write(&doc, &olga_id, &reached)], t0 + 10 * MIN).await.unwrap();
    assert_eq!(watch_pass(state, t0 + 16 * MIN).await.unwrap(), 1);
    let before = mail_count(&stack, "will.watch@example.com");
    assert_eq!(pimble_cloud::notify::email_pass(state, now_ms() + 16 * MIN).await.unwrap(), 1);
    assert_eq!(mail_count(&stack, "will.watch@example.com"), before + 1);
    let mail = last_mail(&stack, "will.watch@example.com");
    assert!(mail.contains("olga.watch edited a note you watch in Recipes"), "{mail}");
    assert!(!mail.contains("Family"), "not the owner's name for the store: {mail}");
}

/// A member who holds the store through a share hears only of documents
/// that share reaches: a write whose roots do not name it gathers nothing,
/// and one gathered through it is not told once the share is no longer
/// theirs, even when they hold the store through another share.
#[tokio::test(flavor = "multi_thread")]
async fn a_scoped_watcher_hears_only_of_what_their_share_reaches() {
    let stack = skip_without_rhypedb!();
    let (olga_cookie, will_cookie, will_id, store_id, root, notice) = a_share_with_notice_keys(&stack, "watchscope").await;
    let olga_id = uuid_of(&stack, "olga.watchscope@example.com").await;
    let (inside, outside) = (node_id(), node_id());
    let other_share = node_id();
    assert_eq!(put_json(&stack, &will_cookie, &format!("/me/watches/stores/{store_id}"), json!({ "docs": [inside, outside], "version": 0 })).await.status(), 200);

    let state = &stack.app_state;
    let t0 = now_ms();
    assert_eq!(record_writes(state, &store_id, &[write(&outside, &olga_id, &[])], t0).await.unwrap(), 0, "no share reaches it: the parent of his root, say");
    assert_eq!(record_writes(state, &store_id, &[write(&outside, &olga_id, &[&other_share])], t0).await.unwrap(), 0, "another share reaches it, not his");
    assert_eq!(record_writes(state, &store_id, &[write(&inside, &olga_id, &[&other_share, &root.to_uppercase()])], t0).await.unwrap(), 1, "his share reaches it");

    // His share moves: Olga shares another folder with him and takes the
    // first one back. What was gathered through it is not told.
    let resp = put_member_with(&stack, &olga_cookie, &store_id, json!({ "email": "will.watchscope@example.com", "role": "editor", "root": other_share, "name": "Garden" })).await;
    assert_eq!(resp.status(), 200);
    let resp = stack.http.delete(format!("{}/stores/{store_id}/members/{will_id}?root={root}", stack.base_url)).header("Cookie", &olga_cookie).send().await.unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
    assert_eq!(watch_pass(state, t0 + 10 * MIN).await.unwrap(), 0);
    assert!(watch_notices(&stack, &will_cookie, &notice, &will_id).await.is_empty());

    // Through the share he holds now, he hears.
    assert_eq!(record_writes(state, &store_id, &[write(&inside, &olga_id, &[&other_share])], t0 + 11 * MIN).await.unwrap(), 1);
    assert_eq!(watch_pass(state, t0 + 20 * MIN).await.unwrap(), 1);
    let inbox = get_json(&stack, &will_cookie, "/inbox").await;
    assert_eq!(inbox["items"][0]["root"], other_share.as_str());
}

/// What the hosted server is told to tell: per store, the documents
/// registered there and the roots of the shares their watchers hold it
/// through (none needed for a watcher holding it whole); nothing for a
/// relayed store, which the hosted server does not serve.
#[tokio::test(flavor = "multi_thread")]
async fn the_hosted_server_is_told_only_the_watched_documents_and_their_watchers_shares() {
    let stack = skip_without_rhypedb!();
    let (olga_cookie, will_cookie, _will_id, store_id, root, _notice) = a_share_with_notice_keys(&stack, "watchinterest").await;
    let state = &stack.app_state;
    assert!(all_interest(state).await.unwrap().is_empty(), "nothing watched, nothing told");
    let (his, hers) = (node_id(), node_id());
    assert_eq!(put_json(&stack, &will_cookie, &format!("/me/watches/stores/{store_id}"), json!({ "docs": [his], "version": 0 })).await.status(), 200);
    assert_eq!(put_json(&stack, &olga_cookie, &format!("/me/watches/stores/{store_id}"), json!({ "docs": [hers, his], "version": 0 })).await.status(), 200);

    let interest = interest_of(state, &store_id).await.unwrap().expect("a store id");
    let mut docs: Vec<String> = interest.docs.iter().map(|d| d.to_string()).collect();
    docs.sort();
    let mut want = vec![his.clone(), hers.clone()];
    want.sort();
    assert_eq!(docs, want, "each document once, nobody's name with it");
    assert_eq!(interest.roots.iter().map(|r| r.to_string()).collect::<Vec<_>>(), vec![root.clone()], "Will's share; Olga holds it whole");
    assert_eq!(all_interest(state).await.unwrap(), vec![interest]);

    let relayed = create_store_with_body(&stack, &olga_cookie, json!({ "name": "", "kind": "vault", "store_id": node_id(), "tier": "relay" })).await;
    let relayed_id = relayed["store_id"].as_str().unwrap().to_string();
    assert_eq!(put_json(&stack, &olga_cookie, &format!("/me/watches/stores/{relayed_id}"), json!({ "docs": [hers], "version": 0 })).await.status(), 200);
    assert!(interest_of(state, &relayed_id).await.unwrap().unwrap().docs.is_empty(), "the hosted server does not serve it");
    assert_eq!(all_interest(state).await.unwrap().len(), 1);

    // Watches off for Will: Olga still watches both, and his share's root
    // is no longer named.
    assert_eq!(put_json(&stack, &will_cookie, "/me/notifications", json!({ "watches": false })).await.status(), 200);
    let interest = interest_of(state, &store_id).await.unwrap().unwrap();
    assert_eq!(interest.docs.len(), 2);
    assert!(interest.roots.is_empty());
}

/// Gathering a write and taking batches out are one step each: writes
/// gathered while passes take batches lose no spell of editing, and the
/// same first write gathered at once from many places makes one batch.
#[tokio::test(flavor = "multi_thread")]
async fn writes_gathered_while_a_pass_takes_batches_are_all_told() {
    let stack = skip_without_rhypedb!();
    let (_olga_cookie, will_cookie, will_id, store_id, root, notice) = a_share_with_notice_keys(&stack, "watchrace").await;
    let olga_id = uuid_of(&stack, "olga.watchrace@example.com").await;
    let doc = node_id();
    assert_eq!(put_json(&stack, &will_cookie, &format!("/me/watches/stores/{store_id}"), json!({ "docs": [doc], "version": 0 })).await.status(), 200);
    let state = stack.app_state.clone();
    let t0 = now_ms();

    let at_once: Vec<_> = (0..8)
        .map(|_| {
            let (state, store_id, w) = (state.clone(), store_id.clone(), write(&doc, &olga_id, &[&root]));
            tokio::spawn(async move { record_writes(&state, &store_id, &[w], t0).await })
        })
        .collect();
    for task in at_once {
        assert_eq!(task.await.unwrap().unwrap(), 1);
    }
    assert_eq!(state.db.watch_batches_of(&will_id).await.unwrap().len(), 1, "one batch");

    const SPELLS: i64 = 30;
    let writer = {
        let (state, store_id, w) = (state.clone(), store_id.clone(), write(&doc, &olga_id, &[&root]));
        tokio::spawn(async move {
            for i in 1..=SPELLS {
                record_writes(&state, &store_id, std::slice::from_ref(&w), t0 + i * SPELL_GAP_MS).await.unwrap();
            }
        })
    };
    let passes = {
        let state = state.clone();
        tokio::spawn(async move {
            let mut queued = 0;
            for _ in 0..SPELLS {
                queued += watch_pass(&state, t0 + 1000 * MIN).await.unwrap();
            }
            queued
        })
    };
    writer.await.unwrap();
    passes.await.unwrap();
    watch_pass(&state, t0 + 1000 * MIN).await.unwrap();
    let told: u32 = watch_notices(&stack, &will_cookie, &notice, &will_id).await.iter().map(|n| n.count).sum();
    assert_eq!(i64::from(told), SPELLS + 1, "every spell told once");
}

/// The switch: off deletes the registered documents and what was gathered,
/// empties the choices and moves their version on (never back, so a device
/// still holding older choices cannot write them again), refuses new ones
/// and gathers nothing; `/me` shows it. The whole switch off does the same.
#[tokio::test(flavor = "multi_thread")]
async fn the_watches_switch_deletes_the_watches_and_stops_gathering() {
    let stack = skip_without_rhypedb!();
    let (_olga_cookie, will_cookie, _will_id, store_id, root, _notice) = a_share_with_notice_keys(&stack, "watchoff").await;
    let olga_id = uuid_of(&stack, "olga.watchoff@example.com").await;
    let doc = node_id();
    let reached = [root.as_str()];
    let me = get_json(&stack, &will_cookie, "/me").await;
    assert_eq!(me["notification_watches"], true, "on until turned off");
    assert_eq!(put_json(&stack, &will_cookie, "/me/watches/choices", json!({ "version": 0, "sealed": "c2VhbGVk" })).await.status(), 200);
    assert_eq!(put_json(&stack, &will_cookie, &format!("/me/watches/stores/{store_id}"), json!({ "docs": [doc], "version": 1 })).await.status(), 200);
    let t0 = now_ms();
    assert_eq!(record_writes(&stack.app_state, &store_id, &[write(&doc, &olga_id, &reached)], t0).await.unwrap(), 1);

    let resp = put_json(&stack, &will_cookie, "/me/notifications", json!({ "watches": false })).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.json::<Value>().await.unwrap()["watches"], false);
    assert_eq!(get_json(&stack, &will_cookie, "/me").await["notification_watches"], false);
    let held = get_json(&stack, &will_cookie, "/me/watches").await;
    assert_eq!(held["choices"], json!({ "version": 2 }), "emptied, and the version moved on");
    assert_eq!(held["stores"], json!([]));
    assert_eq!(watch_pass(&stack.app_state, t0 + 10 * MIN).await.unwrap(), 0, "what was gathered is gone");
    assert_eq!(put_json(&stack, &will_cookie, "/me/watches/choices", json!({ "version": 2, "sealed": "c2VhbGVk" })).await.status(), 403);
    assert_eq!(put_json(&stack, &will_cookie, &format!("/me/watches/stores/{store_id}"), json!({ "docs": [doc], "version": 2 })).await.status(), 403);

    // Back on: a device that still holds the choices at version 1 (or
    // never had any) is refused, and reads them again.
    assert_eq!(put_json(&stack, &will_cookie, "/me/notifications", json!({ "watches": true })).await.status(), 200);
    for stale in [0, 1] {
        assert_eq!(put_json(&stack, &will_cookie, "/me/watches/choices", json!({ "version": stale, "sealed": "c3RhbGU=" })).await.status(), 409);
        assert_eq!(put_json(&stack, &will_cookie, &format!("/me/watches/stores/{store_id}"), json!({ "docs": [doc], "version": stale })).await.status(), 409);
    }
    assert_eq!(put_json(&stack, &will_cookie, &format!("/me/watches/stores/{store_id}"), json!({ "docs": [doc], "version": 2 })).await.status(), 200);

    // Everything off: the watches go with the rest, and the version on.
    assert_eq!(put_json(&stack, &will_cookie, "/me/notifications", json!({ "on": false })).await.status(), 200);
    let held = get_json(&stack, &will_cookie, "/me/watches").await;
    assert_eq!(held["stores"], json!([]));
    assert_eq!(held["choices"]["version"], 2, "nothing chosen since: nothing to empty");
    assert_eq!(record_writes(&stack.app_state, &store_id, &[write(&doc, &olga_id, &reached)], now_ms()).await.unwrap(), 0);
}

/// A watcher no longer in the store is told nothing; the documents can be
/// registered only in a store the account holds.
#[tokio::test(flavor = "multi_thread")]
async fn watches_need_a_grant() {
    let stack = skip_without_rhypedb!();
    let (olga_cookie, will_cookie, will_id, store_id, root, _notice) = a_share_with_notice_keys(&stack, "watchgrant").await;
    let olga_id = uuid_of(&stack, "olga.watchgrant@example.com").await;
    let doc = node_id();
    let (_, stranger) = signup_verify_login(&stack, "stranger.watchgrant@example.com", "a stranger's password!!").await;
    assert_eq!(put_json(&stack, &stranger, &format!("/me/watches/stores/{store_id}"), json!({ "docs": [doc], "version": 0 })).await.status(), 403);
    assert_eq!(put_json(&stack, &will_cookie, &format!("/me/watches/stores/{store_id}"), json!({ "docs": [doc], "version": 0 })).await.status(), 200);

    let resp = stack.http.delete(format!("{}/stores/{store_id}/members/{will_id}?root={root}", stack.base_url)).header("Cookie", &olga_cookie).send().await.unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
    let t0 = now_ms();
    assert_eq!(record_writes(&stack.app_state, &store_id, &[write(&doc, &olga_id, &[&root])], t0).await.unwrap(), 0);
    assert_eq!(watch_pass(&stack.app_state, t0 + 10 * MIN).await.unwrap(), 0);
}

/// Only the owner of a relayed store reports its relay's writes; a hosted
/// store's come from the hosted server. A write whose writer the store was
/// never shared with is refused and gathers nothing.
#[tokio::test(flavor = "multi_thread")]
async fn only_a_relayed_stores_owner_reports_its_writes_and_only_its_members() {
    let stack = skip_without_rhypedb!();
    let (olga_cookie, will_cookie, will_id, store_id, _root, _notice) = a_share_with_notice_keys(&stack, "watchrelay").await;
    let olga_id = uuid_of(&stack, "olga.watchrelay@example.com").await;
    let doc = node_id();
    let post = |cookie: &str, body: Value| stack.http.post(format!("{}/watches/writes", stack.base_url)).header("Cookie", cookie).json(&body).send();
    let report = json!({ "store_id": store_id, "report": node_id(), "writes": [{ "doc": doc, "writer": will_id, "count": 1 }] });
    assert_eq!(post(&olga_cookie, report.clone()).await.unwrap().status(), 400, "a hosted store");

    let relayed = create_store_with_body(&stack, &olga_cookie, json!({ "name": "", "kind": "vault", "store_id": node_id(), "tier": "relay" })).await;
    let relayed_id = relayed["store_id"].as_str().unwrap().to_string();
    let report = json!({ "store_id": relayed_id, "report": node_id(), "writes": [{ "doc": doc, "writer": will_id, "count": 1 }] });
    assert_eq!(post(&will_cookie, report.clone()).await.unwrap().status(), 403, "not its owner");
    let resp = post(&olga_cookie, report).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.json::<Value>().await.unwrap(), json!({ "gathered": 0, "refused": 1 }), "Will was never shared this store");
    let hers = json!({ "store_id": relayed_id, "report": node_id(), "writes": [{ "doc": doc, "writer": olga_id, "count": 3, "roots": [], "last_ms": now_ms() }] });
    let resp = post(&olga_cookie, hers).await.unwrap();
    assert_eq!(resp.json::<Value>().await.unwrap(), json!({ "gathered": 0, "refused": 0 }), "hers, and nobody watches it");
    let bad = json!({ "store_id": relayed_id, "report": node_id(), "writes": [{ "doc": "x", "writer": will_id }] });
    assert_eq!(post(&olga_cookie, bad).await.unwrap().status(), 400);
    let bad_root = json!({ "store_id": relayed_id, "report": node_id(), "writes": [{ "doc": doc, "writer": olga_id, "roots": ["not a root"] }] });
    assert_eq!(post(&olga_cookie, bad_root).await.unwrap().status(), 400);
    let no_id = json!({ "store_id": relayed_id, "report": "not an id", "writes": [] });
    assert_eq!(post(&olga_cookie, no_id).await.unwrap().status(), 400, "a report is named by an id");
}

/// A relay report sent again (its answer lost) is taken once; a member is
/// named as a write's writer only for a document their share reaches.
#[tokio::test(flavor = "multi_thread")]
async fn a_relay_report_is_taken_once_and_names_a_member_only_inside_their_share() {
    let stack = skip_without_rhypedb!();
    let (olga_cookie, _will_cookie, will_id, _hosted_id, _root, _notice) = a_share_with_notice_keys(&stack, "watchreport").await;
    let olga_id = uuid_of(&stack, "olga.watchreport@example.com").await;
    let relayed = create_store_with_body(&stack, &olga_cookie, json!({ "name": "", "kind": "vault", "store_id": node_id(), "tier": "relay" })).await;
    let relayed_id = relayed["store_id"].as_str().unwrap().to_string();
    let garden = node_id();
    let resp = put_member_with(&stack, &olga_cookie, &relayed_id, json!({ "email": "will.watchreport@example.com", "role": "editor", "root": garden, "name": "Garden" })).await;
    assert_eq!(resp.status(), 200);
    let doc = node_id();
    assert_eq!(put_json(&stack, &olga_cookie, &format!("/me/watches/stores/{relayed_id}"), json!({ "docs": [doc], "version": 0 })).await.status(), 200);
    let post = |body: Value| stack.http.post(format!("{}/watches/writes", stack.base_url)).header("Cookie", &olga_cookie).json(&body).send();

    // Will holds the Garden share; the document is outside it.
    let mut sent = Vec::new();
    let mut named = || { let id = node_id(); sent.push(id.clone()); id };
    let outside = json!({ "store_id": relayed_id, "report": named(), "writes": [{ "doc": doc, "writer": will_id, "roots": [], "last_ms": now_ms() }] });
    let resp = post(outside).await.unwrap();
    assert_eq!(resp.json::<Value>().await.unwrap(), json!({ "gathered": 0, "refused": 1 }), "not named for a write his share does not reach");
    let other_root = json!({ "store_id": relayed_id, "report": named(), "writes": [{ "doc": doc, "writer": will_id, "roots": [node_id()], "last_ms": now_ms() }] });
    assert_eq!(post(other_root).await.unwrap().json::<Value>().await.unwrap()["refused"], 1, "nor through a share he does not hold");

    // Inside it: gathered for Olga, once however often the report is sent.
    let report = named();
    let inside = json!({ "store_id": relayed_id, "report": report, "writes": [{ "doc": doc, "writer": will_id, "roots": [garden], "last_ms": now_ms() - 10 * 60 * 1000 }] });
    assert_eq!(post(inside.clone()).await.unwrap().json::<Value>().await.unwrap(), json!({ "gathered": 1, "refused": 0 }));
    assert_eq!(post(inside).await.unwrap().json::<Value>().await.unwrap(), json!({ "gathered": 0, "refused": 0, "again": true }), "the same report again");
    let later = json!({ "store_id": relayed_id, "report": named(), "writes": [{ "doc": doc, "writer": will_id, "roots": [garden], "last_ms": now_ms() }] });
    assert_eq!(post(later).await.unwrap().json::<Value>().await.unwrap()["gathered"], 1);
    let batches = stack.app_state.db.watch_batches_of(&olga_id).await.unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].count, 2, "two spells of editing, ten minutes apart, the first counted once");

    // Every report taken is remembered, refused writes and all, until it
    // is older than the forgetting; the database is shared with the other
    // tests, so the reports are judged by their ids, not by counts.
    let db = &stack.app_state.db;
    for id in &sent {
        assert!(db.relay_report_taken(id).await.unwrap(), "{id} remembered");
    }
    db.forget_relay_reports_before(now_ms() - 60 * MIN).await.unwrap();
    for id in &sent {
        assert!(db.relay_report_taken(id).await.unwrap(), "{id} is newer than the forgetting");
    }
    assert!(db.forget_relay_reports_before(now_ms() + 1).await.unwrap() >= sent.len());
    for id in &sent {
        assert!(!db.relay_report_taken(id).await.unwrap(), "{id} forgotten");
    }
}

/// A relay report gathered whose remembering then failed is sent again:
/// none of its writes is gathered twice, not even one whose notice was
/// queued in between, and the batch's record of it is forgotten with it.
#[tokio::test(flavor = "multi_thread")]
async fn a_relay_report_gathered_but_not_remembered_tells_its_writes_once() {
    let stack = skip_without_rhypedb!();
    let (olga_cookie, will_cookie, will_id, _hosted_id, _root, notice) = a_share_with_notice_keys(&stack, "watchresend").await;
    let olga_id = uuid_of(&stack, "olga.watchresend@example.com").await;
    let relayed = create_store_with_body(&stack, &olga_cookie, json!({ "name": "", "kind": "vault", "store_id": node_id(), "tier": "relay" })).await;
    let relayed_id = relayed["store_id"].as_str().unwrap().to_string();
    let garden = node_id();
    let resp = put_member_with(&stack, &olga_cookie, &relayed_id, json!({ "email": "will.watchresend@example.com", "role": "editor", "root": garden, "name": "Garden" })).await;
    assert_eq!(resp.status(), 200);
    let doc = node_id();
    assert_eq!(put_json(&stack, &will_cookie, &format!("/me/watches/stores/{relayed_id}"), json!({ "docs": [doc], "version": 0 })).await.status(), 200);
    let state = &stack.app_state;
    let report = node_id();
    let olgas = pimble_cloud::watches::Write { last_ms: now_ms() - 20 * MIN, ..write(&doc, &olga_id, &[&garden]) };

    // The first sending gathered it, and then remembering the report failed.
    assert_eq!(record_relay_writes(state, &relayed_id, &report, std::slice::from_ref(&olgas), now_ms()).await.unwrap(), 1);
    assert!(!state.db.relay_report_taken(&report).await.unwrap());
    watch_pass(state, now_ms()).await.unwrap();
    assert_eq!(watch_notices(&stack, &will_cookie, &notice, &will_id).await.len(), 1, "told");

    // Sent again, directly and through the route: nothing more is gathered.
    assert_eq!(record_relay_writes(state, &relayed_id, &report, std::slice::from_ref(&olgas), now_ms()).await.unwrap(), 0);
    let body = json!({ "store_id": relayed_id, "report": report, "writes": [{ "doc": doc, "writer": olga_id, "roots": [garden], "last_ms": olgas.last_ms }] });
    let resp = stack.http.post(format!("{}/watches/writes", stack.base_url)).header("Cookie", &olga_cookie).json(&body).send().await.unwrap();
    assert_eq!(resp.json::<Value>().await.unwrap(), json!({ "gathered": 0, "refused": 0 }));
    assert!(state.db.relay_report_taken(&report).await.unwrap(), "remembered this time");
    watch_pass(state, now_ms()).await.unwrap();
    assert_eq!(watch_notices(&stack, &will_cookie, &notice, &will_id).await.len(), 1, "never told twice");

    // The taken batch stays, with no spells, for as long as its report is
    // remembered; another report's write is gathered into it as usual.
    let batches = state.db.watch_batches_of(&will_id).await.unwrap();
    assert_eq!((batches.len(), batches[0].count, batches[0].reports.contains_key(&report)), (1, 0, true));
    let another = node_id();
    assert_eq!(record_relay_writes(state, &relayed_id, &another, std::slice::from_ref(&olgas), now_ms()).await.unwrap(), 1);
    assert_eq!(state.db.watch_batches_of(&will_id).await.unwrap()[0].count, 1);
    state.db.forget_batch_reports_before(now_ms() + 1).await.unwrap();
    let batch = &state.db.watch_batches_of(&will_id).await.unwrap()[0];
    assert!(batch.reports.is_empty() && batch.count == 1, "the reports forgotten, the spell gathering kept");
}

/// A write told late, between two spells of editing gathered already and
/// half a minute or more from both, is a spell of its own; one that closes
/// the gap between two makes them one.
#[tokio::test(flavor = "multi_thread")]
async fn a_late_write_between_two_spells_is_counted_where_it_falls() {
    let stack = skip_without_rhypedb!();
    let (olga_cookie, _will_cookie, will_id, store_id, _root, _notice) = a_share_with_notice_keys(&stack, "watchlate").await;
    let olga_id = uuid_of(&stack, "olga.watchlate@example.com").await;
    let doc = node_id();
    assert_eq!(put_json(&stack, &olga_cookie, &format!("/me/watches/stores/{store_id}"), json!({ "docs": [doc], "version": 0 })).await.status(), 200);
    let state = &stack.app_state;
    let now = now_ms();
    let at = |ms: i64| pimble_cloud::watches::Write { last_ms: ms, ..write(&doc, &will_id, &[]) };
    for ms in [now - 20 * MIN, now - 10 * MIN] {
        record_writes(state, &store_id, &[at(ms)], now).await.unwrap();
    }
    let count = || async { state.db.watch_batches_of(&olga_id).await.unwrap()[0].count };
    assert_eq!(count().await, 2);
    record_writes(state, &store_id, &[at(now - 15 * MIN)], now).await.unwrap();
    assert_eq!(count().await, 3, "five minutes from each: a spell of its own");
    record_writes(state, &store_id, &[at(now - 15 * MIN + 20_000), at(now - 15 * MIN - 20_000)], now).await.unwrap();
    assert_eq!(count().await, 3, "close to it: part of it");
    let batch = &state.db.watch_batches_of(&olga_id).await.unwrap()[0];
    assert_eq!(batch.spells.spans().len(), 3);
    assert_eq!((batch.first_at_ms, batch.last_at_ms), (now - 20 * MIN, now - 10 * MIN));
}

/// Every change to a grant, and its removal, has the hosted server told
/// again what the store's watchers hold: a role or scope changed, a member
/// removed, the store deleted.
#[tokio::test(flavor = "multi_thread")]
async fn every_grant_change_tells_the_hosted_server_again() {
    let stack = skip_without_rhypedb!();
    let (olga_cookie, _will_cookie, will_id, store_id, root, _notice) = a_share_with_notice_keys(&stack, "watchgrants").await;
    let changes = &stack.app_state.watch_interest;
    assert!(changes.take().contains(&store_id), "a new share");
    let resp = put_member_with(&stack, &olga_cookie, &store_id, json!({ "email": "will.watchgrants@example.com", "role": "reader", "root": root, "name": "Recipes" })).await;
    assert_eq!(resp.status(), 200);
    assert!(changes.take().contains(&store_id), "a role changed");
    let resp = put_member_with(&stack, &olga_cookie, &store_id, json!({ "email": "will.watchgrants@example.com", "role": "editor" })).await;
    assert_eq!(resp.status(), 200);
    assert!(changes.take().contains(&store_id), "the whole store granted");
    let resp = stack.http.delete(format!("{}/stores/{store_id}/members/{will_id}?root={root}", stack.base_url)).header("Cookie", &olga_cookie).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(changes.take().contains(&store_id), "a share removed: he holds the whole store now");
    let resp = stack.http.delete(format!("{}/stores/{store_id}/members/{will_id}", stack.base_url)).header("Cookie", &olga_cookie).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(changes.take().contains(&store_id), "removed altogether");
    let resp = stack.http.delete(format!("{}/stores/{store_id}", stack.base_url)).header("Cookie", &olga_cookie).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(changes.take().contains(&store_id), "the store deleted");
}
