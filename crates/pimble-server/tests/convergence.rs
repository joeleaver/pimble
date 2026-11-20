//! End-to-end tests for wave 1 of docs/CONVERGENCE_CONTRACT.md: devices say,
//! sealed, what they hold of each document (`vaultReport`), and a device that
//! holds more than another's report says sends what that one lacks.
//!
//! The environment is `tests/common`: `H`, a real `PimbleServer` in JWT mode
//! holding the hosted twins; the stub accounts service; and one real
//! "desktop" `PimbleServer` per device.
//!
//! The failure these exist for (2026-11-19, "Shared with Will"): a member's
//! edit never reached the shared copy while that member's device believed it
//! had (`pushed_sv` in `vault-link.json`), and every later edit to the note
//! waited on every other device. The tests make exactly that state: an edit
//! made while the link is down, then the device's record rewritten to claim
//! the remote holds it, then the device started again.

mod common;
use common::*;

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use pimble_client::PimbleClient;
use pimble_core::{NodeId, StoreId, SyncState};
use pimble_crdt::NodeDoc;
use pimble_rpc::{EditOperation, MemberRole, StoreChangeKind, VaultDocId};
use pimble_server::{PimbleServer, ServerConfig};

/// How long a difference lasts before a test device says a note is out of
/// step, in place of five minutes.
const OUT_OF_STEP_AFTER: Duration = Duration::from_secs(2);

/// A device whose vault links report and compare, or (as every device did
/// before wave 1) do not.
fn config(dir: &Path, reports: bool) -> ServerConfig {
    ServerConfig { vault_reports: reports, out_of_step_after: Some(OUT_OF_STEP_AFTER), set_aside_after: SET_ASIDE_AFTER.with(|a| a.get()), ..device_config(dir) }
}

thread_local! {
    /// How long a parked update waits before it is set aside on the devices
    /// a test starts on this thread, in place of 24 hours (`None`: 24 hours).
    static SET_ASIDE_AFTER: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
}

async fn start_device(dir: &Path, reports: bool) -> (PimbleServer, PimbleClient) {
    let mut server = PimbleServer::with_config(config(dir, reports));
    server.start().await.expect("a device starts");
    let client = PimbleClient::connect(format!("http://{}", server.addr())).await.expect("a client connects");
    (server, client)
}

async fn is_synced(client: &PimbleClient, store_id: StoreId) -> bool {
    matches!(client.get_store_sync(store_id).await, Ok((_, SyncState::Synced { .. })))
}

/// Every document's head on `H`, summed: how many entries have ever been
/// appended to the store's logs.
async fn total_appends(env: &Env, store_id: StoreId) -> u64 {
    env.h_admin.vault_list_docs(store_id).await.unwrap().iter().map(|doc| doc.head).sum()
}

/// What `client`'s server holds of `node`, as a state vector.
async fn state_vector_of(client: &PimbleClient, store_id: StoreId, node: NodeId) -> Vec<u8> {
    let (answers, _) = client.sync_nodes(store_id, &[(node, pimble_crdt::empty_state_vector())], false).await.expect("syncNodes answers");
    answers.into_iter().find(|(id, _, _)| *id == node).map(|(_, _, sv)| sv).expect("the node is held")
}

/// Whether two encoded state vectors say the same: the encoding's order of
/// clients is not fixed, so equal vectors can differ as bytes.
fn same_vector(a: &[u8], b: &[u8]) -> bool {
    !pimble_crdt::state_vector_exceeds(a, b).unwrap_or(true) && !pimble_crdt::state_vector_exceeds(b, a).unwrap_or(true)
}

/// Alice's hosted store with its "Holiday" folder shared with Bob as an
/// editor, and Bob's device holding the share. Both devices report or not.
struct Shared {
    fx: Fixture,
    _alice_server: PimbleServer,
    alice: PimbleClient,
    _alice_dir: tempfile::TempDir,
    bob_server: PimbleServer,
    bob: PimbleClient,
    bob_dir: tempfile::TempDir,
}

async fn shared_with_bob(env: &Env, alice_reports: bool, bob_reports: bool) -> Shared {
    let alice_dir = tempfile::tempdir().unwrap();
    let (alice_server, alice) = start_device(alice_dir.path(), alice_reports).await;
    let fx = hosted_fixture(env, &alice, alice_dir.path()).await;
    alice.cloud_share_node(fx.store_id, fx.shared, "Holiday Plans").await.expect("sharing a folder");
    alice.cloud_share_invite(fx.store_id, fx.shared, BOB.0, MemberRole::Editor).await.expect("inviting Bob");

    let bob_dir = tempfile::tempdir().unwrap();
    let (bob_server, bob) = start_device(bob_dir.path(), bob_reports).await;
    env.sign_in(&bob, BOB).await;
    bob.cloud_add_hosted_store(fx.store_id).await.expect("Bob adds the share");
    let pulled = wait_until(Duration::from_secs(20), || async { node_text(&bob, fx.store_id, fx.inside).await.contains("socks and a map") }).await;
    assert!(pulled, "Bob's replica pulls the share");
    assert!(wait_until(Duration::from_secs(10), || is_synced(&bob, fx.store_id)).await, "Bob's link is up");
    Shared { fx, _alice_server: alice_server, alice, _alice_dir: alice_dir, bob_server, bob, bob_dir }
}

fn bob_replica(shared: &Shared) -> PathBuf {
    shared.bob_dir.path().join("replicas").join(format!("{}.pimble", shared.fx.store_id))
}

/// The 2026-11-19 state, made on purpose: Bob edits `inside` with the network
/// gone, his server stops, and his `vault-link.json` is rewritten to say the
/// remote holds everything his copy of `inside` does (`pushed_sv` at his full
/// state vector, nothing marked). Then the network comes back and Bob's device
/// starts again from its files. Without reports nothing will ever send the
/// edit: his link believes it was sent, and every other device never had it.
/// Answers Bob's restarted device.
async fn make_the_gap(env: &Env, shared: &mut Shared, reports: bool, words: &str) -> (PimbleServer, PimbleClient) {
    make_the_gap_with(env, shared, reports, words, None).await
}

/// [`make_the_gap`] with the edit given as an update (whose text contains
/// `words`) rather than made from `words`.
async fn make_the_gap_with(env: &Env, shared: &mut Shared, reports: bool, words: &str, edit: Option<&[u8]>) -> (PimbleServer, PimbleClient) {
    let (store_id, inside) = (shared.fx.store_id, shared.fx.inside);
    env.relay.cut();
    assert!(wait_until(Duration::from_secs(10), || async { !is_synced(&shared.bob, store_id).await }).await, "Bob's link notices the network is gone");
    match edit {
        Some(update) => apply(&shared.bob, store_id, inside, "bob-editor", update).await,
        None => seed_content(&shared.bob, store_id, inside, "bob-editor", words).await,
    }
    // The content flush and its `modified_at` stamp, both inside the vector.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let held = state_vector_of(&shared.bob, store_id, inside).await;
    shared.bob_server.stop().await.unwrap();

    // Bob's computer's own state, outside the store's folder
    // (docs/STORE_FOLDERS_CONTRACT.md rule 1).
    let progress_path = shared.bob_dir.path().join("stores").join(shared.fx.store_id.to_string()).join("vault-link.json");
    let mut progress: serde_json::Value = serde_json::from_slice(&std::fs::read(&progress_path).expect("Bob's link keeps a progress file")).unwrap();
    progress["pushed_sv"][inside.to_string()] = serde_json::Value::String(STANDARD.encode(&held));
    progress["dirty"] = serde_json::json!([]);
    progress["dirty_all"] = serde_json::Value::Bool(false);
    std::fs::write(&progress_path, serde_json::to_vec(&progress).unwrap()).unwrap();

    env.relay.restore();
    let (server, bob) = start_device(shared.bob_dir.path(), reports).await;
    bob.open_store(bob_replica(shared)).await.expect("Bob's replica opens again");
    assert!(node_text(&bob, store_id, inside).await.contains(words), "Bob kept his own edit");
    assert!(wait_until(Duration::from_secs(15), || is_synced(&bob, store_id)).await, "Bob's link is up again");
    (server, bob)
}

/// The failure of 2026-11-19, healed: Bob's edit, which his link believes it
/// sent, reaches Alice once the devices compare what they hold. Alice's
/// report of the note (made when she hosted the store) lacks Bob's structs,
/// and Bob answers it at his connect.
#[tokio::test]
async fn an_edit_a_device_wrongly_believes_it_sent_reaches_the_others_through_reports() {
    let env = spawn_env().await;
    let mut shared = shared_with_bob(&env, true, true).await;
    let (store_id, inside) = (shared.fx.store_id, shared.fx.inside);
    let reported = wait_until(Duration::from_secs(10), || async {
        env.h_admin.vault_reports(store_id, vec![VaultDocId::Node(inside)]).await.map(|r| r.len()).unwrap_or(0) >= 2
    })
    .await;
    assert!(reported, "both devices report the note at their connect");

    let (_bob_server, _bob) = make_the_gap(&env, &mut shared, true, "TWELVE STRUCTS FROM OCTOBER").await;
    let healed = wait_until(Duration::from_secs(10), || async { node_text(&shared.alice, store_id, inside).await.contains("TWELVE STRUCTS FROM OCTOBER") }).await;
    assert!(healed, "Alice must receive the edit Bob's link believed it had sent, got {:?}", node_text(&shared.alice, store_id, inside).await);

    // And then quiet: equal now, nobody answers anybody again.
    tokio::time::sleep(Duration::from_secs(7)).await;
    let settled = total_appends(&env, store_id).await;
    tokio::time::sleep(Duration::from_secs(7)).await;
    assert_eq!(total_appends(&env, store_id).await, settled, "once both hold the same, reports cause no appends");
}

/// The same state with reports off on both devices (every device before wave
/// 1): the edit never arrives. What shows the test above is about the gap and
/// not about anything else that sends it.
#[tokio::test]
async fn without_reports_an_edit_a_device_wrongly_believes_it_sent_never_arrives() {
    let env = spawn_env().await;
    let mut shared = shared_with_bob(&env, false, false).await;
    let (store_id, inside) = (shared.fx.store_id, shared.fx.inside);
    assert!(env.h_admin.vault_reports(store_id, vec![VaultDocId::Node(inside)]).await.unwrap().is_empty(), "nobody reports");

    let (_bob_server, _bob) = make_the_gap(&env, &mut shared, false, "TWELVE STRUCTS FROM OCTOBER").await;
    let arrived = wait_until(Duration::from_secs(12), || async { node_text(&shared.alice, store_id, inside).await.contains("TWELVE STRUCTS FROM OCTOBER") }).await;
    assert!(!arrived, "without reports nothing sends what a link believes it sent");
}

/// The live path, and a device from before reports beside new ones: Alice's
/// device does not report (an older Pimble), so when Bob comes back with his
/// gap nobody's report asks for anything and the edit stays where it is.
/// Then Alice adds the store on a second device that does report: it pulls
/// the log, lacks Bob's structs, says so, and Bob, hearing that report live
/// (`VaultReported`), sends them; both of Alice's devices have the edit.
#[tokio::test]
async fn a_report_heard_live_is_answered_and_a_device_without_reports_is_left_alone() {
    let env = spawn_env().await;
    let mut shared = shared_with_bob(&env, false, true).await;
    let (store_id, inside) = (shared.fx.store_id, shared.fx.inside);

    let (_bob_server, _bob) = make_the_gap(&env, &mut shared, true, "SENT WHEN ASKED").await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!node_text(&shared.alice, store_id, inside).await.contains("SENT WHEN ASKED"), "no report asks for it yet");

    let c_dir = tempfile::tempdir().unwrap();
    let (_c_server, second_device) = start_device(c_dir.path(), true).await;
    env.sign_in(&second_device, ALICE).await;
    second_device.cloud_add_hosted_store(store_id).await.expect("Alice's second device adds the store");
    let second = wait_until(Duration::from_secs(20), || async { node_text(&second_device, store_id, inside).await.contains("SENT WHEN ASKED") }).await;
    assert!(second, "the new device's report is answered live, got {:?}", node_text(&second_device, store_id, inside).await);
    let first = wait_until(Duration::from_secs(10), || async { node_text(&shared.alice, store_id, inside).await.contains("SENT WHEN ASKED") }).await;
    assert!(first, "and the device without reports reads it from the log like any append");
}

/// Two devices holding the same append nothing because of reports: not at
/// their connects, not after them, however many times they connect again.
#[tokio::test]
async fn two_devices_with_equal_state_append_nothing_because_of_reports() {
    let env = spawn_env().await;
    let a_dir = tempfile::tempdir().unwrap();
    let (_a_server, alice) = start_device(a_dir.path(), true).await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let b_dir = tempfile::tempdir().unwrap();
    let (_b_server, second) = start_device(b_dir.path(), true).await;
    env.sign_in(&second, ALICE).await;
    second.cloud_add_hosted_store(fx.store_id).await.expect("the second device adds the store");
    assert!(wait_until(Duration::from_secs(20), || async { node_text(&second, fx.store_id, fx.private).await.contains("PRIVATE-DIARY-TEXT") }).await);

    let docs: Vec<VaultDocId> = [fx.root_id, fx.shared, fx.inside, fx.deeper, fx.leaf, fx.private].into_iter().map(VaultDocId::Node).collect();
    let all_reported = wait_until(Duration::from_secs(15), || async {
        let reports = env.h_admin.vault_reports(fx.store_id, docs.clone()).await.unwrap_or_default();
        docs.iter().all(|doc| reports.iter().filter(|r| &r.doc_id == doc).count() == 2)
    })
    .await;
    assert!(all_reported, "both devices report every document: the test is about reports that happen");
    tokio::time::sleep(Duration::from_secs(7)).await;
    let before = total_appends(&env, fx.store_id).await;

    for _ in 0..2 {
        env.relay.cut();
        assert!(wait_until(Duration::from_secs(10), || async { !is_synced(&alice, fx.store_id).await && !is_synced(&second, fx.store_id).await }).await);
        env.relay.restore();
        assert!(wait_until(Duration::from_secs(20), || async { is_synced(&alice, fx.store_id).await && is_synced(&second, fx.store_id).await }).await);
        // The connect's comparing, and a debounce of reports after it.
        tokio::time::sleep(Duration::from_secs(7)).await;
    }
    assert_eq!(total_appends(&env, fx.store_id).await, before, "equal state: no device appended anything because of a report");
}

/// Bob's copy of `inside` gets a delete his link never hears of: with his
/// server stopped, the text is deleted in the node's file on disk, by an
/// update that carries deletes and no structs. His state vector is Alice's,
/// so nothing but the delete digest can say the two differ; and his link
/// has nothing marked to send. Answers Bob's restarted device.
async fn delete_while_away(shared: &mut Shared, reports: bool) -> (PimbleServer, PimbleClient) {
    let (store_id, inside) = (shared.fx.store_id, shared.fx.inside);
    // Level first: each side's `modified_at` stamp is still on its way for a
    // moment after the fixture's edits, longer under load.
    let level = wait_until(Duration::from_secs(30), || async { same_vector(&state_vector_of(&shared.bob, store_id, inside).await, &state_vector_of(&shared.alice, store_id, inside).await) }).await;
    assert!(
        level,
        "Bob and Alice hold the same structs of the note: Bob {}, Alice {}",
        pimble_crdt::describe_state_vector(&state_vector_of(&shared.bob, store_id, inside).await),
        pimble_crdt::describe_state_vector(&state_vector_of(&shared.alice, store_id, inside).await)
    );
    shared.bob_server.stop().await.unwrap();

    let file = bob_replica(shared).join("nodes").join(format!("{inside}.yrs"));
    let mut doc = NodeDoc::load(&std::fs::read(&file).expect("the note's document")).unwrap();
    let before = doc.state_vector();
    let mut scratch = NodeDoc::load(&doc.save()).unwrap();
    scratch.replace_plain_text("").unwrap();
    doc.apply_update(&scratch.diff_since(&scratch.state_vector()).unwrap()).unwrap();
    assert_eq!(doc.state_vector(), before, "a delete moves no clock");
    std::fs::write(&file, doc.save()).unwrap();

    let (server, bob) = start_device(shared.bob_dir.path(), reports).await;
    bob.open_store(bob_replica(shared)).await.expect("Bob's replica opens again");
    assert!(!node_text(&bob, store_id, inside).await.contains("socks"), "Bob's copy has the delete");
    assert!(wait_until(Duration::from_secs(15), || is_synced(&bob, store_id)).await, "Bob's link is up again");
    (server, bob)
}

/// Two devices that differ only by a delete: one state vector, two delete
/// sets. The digest in the reports tells them apart, each sends the other
/// what it holds from the other's vector, and both end with the delete.
/// Also: the device id is this computer's, in `devices.json` beside the
/// replicas, and nothing of it is in the store's folder.
#[tokio::test]
async fn a_delete_one_device_lacks_reaches_it_through_the_delete_digest() {
    let env = spawn_env().await;
    let mut shared = shared_with_bob(&env, true, true).await;
    let (store_id, inside) = (shared.fx.store_id, shared.fx.inside);
    assert!(
        wait_until(Duration::from_secs(10), || async { env.h_admin.vault_reports(store_id, vec![VaultDocId::Node(inside)]).await.map(|r| r.len()).unwrap_or(0) >= 2 }).await,
        "both devices report the note"
    );

    let devices: serde_json::Value = serde_json::from_slice(&std::fs::read(shared.bob_dir.path().join("devices.json")).expect("devices.json beside the replicas")).unwrap();
    let device = devices["stores"][store_id.to_string()]["device"].as_str().expect("Bob's device id in the store").to_string();
    let reporters: Vec<String> = env.h_admin.vault_reports(store_id, vec![VaultDocId::Node(inside)]).await.unwrap().into_iter().map(|r| r.device).collect();
    assert!(reporters.contains(&device), "Bob reports under that id");
    assert!(!contains_bytes_recursive(&bob_replica(&shared), device.as_bytes()), "nothing of the device id is in the store's folder");

    let (_bob_server, bob) = delete_while_away(&mut shared, true).await;
    let healed = wait_until(Duration::from_secs(10), || async { !node_text(&shared.alice, store_id, inside).await.contains("socks") }).await;
    assert!(healed, "Alice must receive the delete her copy missed, got {:?}", node_text(&shared.alice, store_id, inside).await);
    assert!(!node_text(&bob, store_id, inside).await.contains("socks"));

    // And then quiet: one exchange each way, then nothing.
    tokio::time::sleep(Duration::from_secs(7)).await;
    let settled = total_appends(&env, store_id).await;
    tokio::time::sleep(Duration::from_secs(7)).await;
    assert_eq!(total_appends(&env, store_id).await, settled, "equal delete sets: no device appends again");
}

/// The same with reports off: the delete never reaches Alice.
#[tokio::test]
async fn without_reports_a_delete_one_device_lacks_never_arrives() {
    let env = spawn_env().await;
    let mut shared = shared_with_bob(&env, false, false).await;
    let (store_id, inside) = (shared.fx.store_id, shared.fx.inside);
    let (_bob_server, _bob) = delete_while_away(&mut shared, false).await;
    let arrived = wait_until(Duration::from_secs(12), || async { !node_text(&shared.alice, store_id, inside).await.contains("socks") }).await;
    assert!(!arrived, "without reports nothing sends a delete that moved no clock");
}

// ── What is waited for, and out of step, said (wave 2) ───────────────────

/// Apply `update` to `node` through `client`, as an editor's change.
async fn apply(client: &PimbleClient, store_id: StoreId, node: NodeId, who: &str, update: &[u8]) {
    let changes = STANDARD.encode(update);
    client.apply_edit(store_id, node, who, EditOperation::IncrementalChanges { changes }).await.expect("the edit applies");
}

/// Two appends to `node`'s text as `client` holds it, the second built on the
/// first (its paragraph comes after the first's): applied without the first,
/// the second waits, parked.
async fn two_appends(client: &PimbleClient, store_id: StoreId, node: NodeId, first: &str, second: &str) -> (Vec<u8>, Vec<u8>) {
    let (answers, _) = client.sync_nodes(store_id, &[(node, pimble_crdt::empty_state_vector())], false).await.unwrap();
    let (_, whole, _) = answers.into_iter().find(|(id, _, _)| *id == node).unwrap();
    let mut doc = NodeDoc::load(&whole).unwrap();
    let one = doc.append_markdown(first).unwrap();
    let two = doc.append_markdown(second).unwrap();
    (one, two)
}

/// Section 3, met by wave 1's comparing: Bob's update waits on every other
/// device behind structs only Bob holds (the gap of 2026-11-19, then an edit
/// built on it that went out live). A waiting document's vector stops
/// before what it waits for, so Bob's is beyond any report of it, and he
/// sends the diff the moment a report asks. Alice's device does not report,
/// so the gap stands until her second device, which does, reports.
#[tokio::test]
async fn an_update_parked_behind_a_gap_is_filled_by_the_device_that_holds_it() {
    let env = spawn_env().await;
    let mut shared = shared_with_bob(&env, false, true).await;
    let (store_id, inside) = (shared.fx.store_id, shared.fx.inside);
    let (gap, on_top) = two_appends(&shared.bob, store_id, inside, "THE GAP", "BUILT ON THE GAP").await;
    let (_bob_server, bob) = make_the_gap_with(&env, &mut shared, true, "THE GAP", Some(&gap)).await;
    // Built on the gap, and sent live: everyone else parks it.
    apply(&bob, store_id, inside, "bob-editor", &on_top).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let alice_text = node_text(&shared.alice, store_id, inside).await;
    assert!(!alice_text.contains("THE GAP") && !alice_text.contains("BUILT ON THE GAP"), "Alice holds neither: the second waits behind the first, got {alice_text:?}");

    let c_dir = tempfile::tempdir().unwrap();
    let (_c_server, second_device) = start_device(c_dir.path(), true).await;
    env.sign_in(&second_device, ALICE).await;
    second_device.cloud_add_hosted_store(store_id).await.expect("Alice's second device adds the store");
    let filled = wait_until(Duration::from_secs(20), || async {
        let text = node_text(&second_device, store_id, inside).await;
        text.contains("THE GAP") && text.contains("BUILT ON THE GAP")
    })
    .await;
    assert!(filled, "the second device's report asks for what it waits on, got {:?}", node_text(&second_device, store_id, inside).await);
    let first = wait_until(Duration::from_secs(10), || async {
        let text = node_text(&shared.alice, store_id, inside).await;
        text.contains("THE GAP") && text.contains("BUILT ON THE GAP")
    })
    .await;
    assert!(first, "and the parked update is released on Alice's first device too");
}

async fn out_of_step_of(client: &PimbleClient, store_id: StoreId) -> Vec<pimble_core::OutOfStep> {
    client.get_store_sync_response(store_id).await.map(|r| r.out_of_step).unwrap_or_default()
}

/// Section 4: a gap no device can fill. Both devices get an update built on a
/// change nobody holds (made on a scratch copy and never sent: what a log
/// that carries an edit and not the one before it hands everyone); it waits,
/// parked, on both. A parked update moves no clock and is not sent on until
/// it is released, so no report can ask anyone for the missing change.
/// After the window both say the note is out of step, with the reason, and a
/// notification tells the app; once the missing change arrives on one
/// device, both are in step again.
#[tokio::test]
async fn a_gap_no_device_can_fill_makes_the_note_out_of_step_on_both_until_it_is_filled() {
    let env = spawn_env().await;
    let shared = shared_with_bob(&env, true, true).await;
    let (store_id, inside) = (shared.fx.store_id, shared.fx.inside);
    let mut alice_changes = shared.alice.subscribe_store_changes(store_id).await.unwrap();
    assert!(out_of_step_of(&shared.alice, store_id).await.is_empty() && out_of_step_of(&shared.bob, store_id).await.is_empty(), "in step to begin with");

    let (lost, waits) = two_appends(&shared.alice, store_id, inside, "NOBODY HOLDS THIS", "WAITS FOR IT").await;
    apply(&shared.alice, store_id, inside, "alice-editor", &waits).await;
    apply(&shared.bob, store_id, inside, "bob-editor", &waits).await;
    assert!(!node_text(&shared.alice, store_id, inside).await.contains("WAITS FOR IT"), "parked on Alice");
    assert!(!node_text(&shared.bob, store_id, inside).await.contains("WAITS FOR IT"), "and on Bob");

    for (who, client) in [("Alice", &shared.alice), ("Bob", &shared.bob)] {
        let marked = wait_until(Duration::from_secs(20), || async { out_of_step_of(client, store_id).await.iter().any(|o| o.node_id == inside) }).await;
        assert!(marked, "{who}'s device says the note is out of step");
        let entry = out_of_step_of(client, store_id).await.into_iter().find(|o| o.node_id == inside).unwrap();
        assert_eq!((entry.reason.as_str(), entry.kind), (pimble_rpc::vault::WAITING_REASON, pimble_core::OutOfStepKind::Waiting), "{who}");
        assert!(!entry.is_shown(), "{who}: nobody can act on it, so only its Properties say it (Joe, 2026-11-20)");
        assert!(chrono::Utc::now() - entry.since >= chrono::Duration::from_std(OUT_OF_STEP_AFTER).unwrap(), "since when the difference began, not when it was said");
        assert!(out_of_step_of(client, store_id).await.iter().all(|o| o.node_id == inside), "only that note");
    }
    let mut told = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !told && tokio::time::Instant::now() < deadline {
        if let Ok(Some(Ok(n))) = tokio::time::timeout(Duration::from_millis(200), alice_changes.next()).await {
            told = matches!(n.change_kind, StoreChangeKind::SyncStateChanged { .. });
        }
    }
    assert!(told, "the app hears the list changed (the link's state again) without asking");

    apply(&shared.alice, store_id, inside, "alice-editor", &lost).await;
    for (who, client) in [("Alice", &shared.alice), ("Bob", &shared.bob)] {
        let text = wait_until(Duration::from_secs(15), || async { node_text(client, store_id, inside).await.contains("WAITS FOR IT") }).await;
        assert!(text, "{who} has both once the missing change arrives");
        let cleared = wait_until(Duration::from_secs(15), || async { out_of_step_of(client, store_id).await.is_empty() }).await;
        assert!(cleared, "{who}'s device is in step again");
    }
}

// ── What cannot be had, set aside (wave 3, section 5) ─────────────────────

/// What a device's computer keeps of the store's convergence
/// (`convergence.json` in its state directory, never in the store's folder).
fn convergence_file(dir: &Path, store_id: StoreId) -> Option<serde_json::Value> {
    let bytes = std::fs::read(dir.join("stores").join(store_id.to_string()).join("convergence.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Section 5: an update parked behind a change nobody holds is set aside
/// once it has waited (a test's few seconds for the 24 hours) with every
/// device that reported lacking that change. Since when it waited survives
/// a restart (kept in the state directory, not the store's folder); each
/// device says it quietly in the note's Properties; the document is in step
/// from what everyone holds; and a snapshot took the entry that carried it
/// out of the log, so a device that reads the store afresh never parks it.
#[tokio::test]
async fn a_gap_nobody_can_fill_is_set_aside_and_leaves_the_log() {
    SET_ASIDE_AFTER.with(|a| a.set(Some(Duration::from_secs(10))));
    let env = spawn_env().await;
    let mut shared = shared_with_bob(&env, true, true).await;
    let (store_id, inside) = (shared.fx.store_id, shared.fx.inside);
    let (_lost, waits) = two_appends(&shared.alice, store_id, inside, "NOBODY HOLDS THIS", "WAITS FOR IT").await;
    apply(&shared.alice, store_id, inside, "alice-editor", &waits).await;
    apply(&shared.bob, store_id, inside, "bob-editor", &waits).await;
    let parked_at = chrono::Utc::now();

    // Bob's computer notes since when it waits, and keeps it across a restart.
    let noted = wait_until(Duration::from_secs(10), || async { convergence_file(shared.bob_dir.path(), store_id).is_some_and(|f| f["parked"][inside.to_string()]["since"].is_string()) }).await;
    assert!(noted, "Bob's computer keeps since when the note waits");
    assert!(!contains_bytes_recursive(&bob_replica(&shared), b"convergence"), "nothing of it is in the store's folder");
    let since_before = convergence_file(shared.bob_dir.path(), store_id).unwrap()["parked"][inside.to_string()]["since"].as_str().unwrap().to_string();
    shared.bob_server.stop().await.unwrap();
    let (_bob_server, bob) = start_device(shared.bob_dir.path(), true).await;
    bob.open_store(bob_replica(&shared)).await.expect("Bob's replica opens again");
    assert!(wait_until(Duration::from_secs(15), || is_synced(&bob, store_id)).await, "Bob's link is up again");
    assert!(wait_until(Duration::from_secs(5), || async { held_parked(&bob, store_id, inside).await }).await, "still parked on Bob's after the restart");
    let since_after = convergence_file(shared.bob_dir.path(), store_id).unwrap()["parked"][inside.to_string()]["since"].as_str().map(str::to_string);
    assert_eq!(since_after.as_deref(), Some(since_before.as_str()), "the 24 hours count from when it began, not from the restart");

    for (who, client) in [("Alice", &shared.alice), ("Bob", &bob)] {
        let aside = wait_until(Duration::from_secs(40), || async {
            out_of_step_of(client, store_id).await.iter().any(|o| o.node_id == inside && o.kind == pimble_core::OutOfStepKind::SetAside)
        })
        .await;
        assert!(aside, "{who}'s device set it aside and says so, got {:?}", out_of_step_of(client, store_id).await);
        let entry = out_of_step_of(client, store_id).await.into_iter().find(|o| o.node_id == inside).unwrap();
        assert_eq!(entry.reason, pimble_rpc::vault::SET_ASIDE_REASON);
        assert!(!entry.is_shown(), "{who}: quiet, in Properties only");
        assert!(entry.since >= parked_at, "{who}: when it was set aside");
        assert!(!held_parked(client, store_id, inside).await, "{who}: nothing waits parked any more");
        assert!(!node_text(client, store_id, inside).await.contains("WAITS FOR IT"), "{who}: what waited is not shown");
    }
    let record = convergence_file(shared.bob_dir.path(), store_id).unwrap();
    assert!(record["set_aside"][inside.to_string()][0]["waited_for"].as_str().is_some_and(|w| w.contains("client")), "the record says what it waited for: {record}");
    assert!(record["parked"].get(inside.to_string()).is_none(), "and it waits for nothing now");

    // In step from what everyone holds.
    let level = wait_until(Duration::from_secs(15), || async { same_vector(&state_vector_of(&bob, store_id, inside).await, &state_vector_of(&shared.alice, store_id, inside).await) }).await;
    assert!(level, "both hold the same");

    // A device that reads the store afresh: the log no longer carries it.
    let c_dir = tempfile::tempdir().unwrap();
    let (_c_server, second) = start_device(c_dir.path(), true).await;
    env.sign_in(&second, ALICE).await;
    second.cloud_add_hosted_store(store_id).await.expect("Alice's second device adds the store");
    assert!(wait_until(Duration::from_secs(20), || async { node_text(&second, store_id, inside).await.contains("socks and a map") }).await, "it pulls the note");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!held_parked(&second, store_id, inside).await, "a fresh reader never parks what was set aside: the snapshot took it out of the log");
    SET_ASIDE_AFTER.with(|a| a.set(None));
}

/// Whether `client`'s copy of `node` holds an update parked.
async fn held_parked(client: &PimbleClient, store_id: StoreId, node: NodeId) -> bool {
    match client.get_node(store_id, node).await {
        Ok(node) => NodeDoc::load(&node.content).map(|d| d.missing().is_some()).unwrap_or(false),
        Err(_) => false,
    }
}
