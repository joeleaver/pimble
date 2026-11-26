//! Pictures through the relay (docs/IMAGES_CONTRACT.md, wave 3).

use super::*;

// ── Pictures through the relay (docs/IMAGES_CONTRACT.md, wave 3) ───────

/// A picture on `node_id`'s canvas, as the app puts one (`data.board`).
async fn put_on_canvas(client: &PimbleClient, store_id: StoreId, node_id: NodeId, url: &pimble_core::BlobUrl) {
    use pimble_board::{Board, Geom, Kind, NewElement, PictureGeom, Rect, Stamp};
    let node = client.get_node(store_id, node_id).await.unwrap();
    let doc = NodeDoc::load(&node.content).unwrap();
    let before = doc.state_vector();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let board = Board::of_node(&doc);
    if !board.exists() {
        board.create(Stamp::new(None, &now)).unwrap();
    }
    let geom = Geom::Picture(PictureGeom { rect: Rect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 }, src: url.to_string(), px_w: 10, px_h: 10, crop: None });
    board.add_element(Stamp::new(None, &now), NewElement::new(Kind::Picture, geom)).unwrap();
    let changes = base64::engine::general_purpose::STANDARD.encode(doc.diff_since(&before).unwrap());
    client.apply_edit(store_id, node_id, "canvas", EditOperation::IncrementalChanges { changes }).await.unwrap();
}

fn png(len: usize, seed: u8) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend((0..len.saturating_sub(8)).map(|i| ((i * 11 + i / 89) as u8).wrapping_add(seed)));
    bytes
}

async fn holds(client: &PimbleClient, store_id: StoreId, blob: pimble_core::BlobId, bytes: &[u8]) -> bool {
    client.get_blob(store_id, blob).await.is_ok_and(|(_, got)| got == bytes)
}

#[tokio::test]
async fn pictures_go_both_ways_through_the_relay_and_nothing_is_hosted() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = relayed_fixture(&env, &alice, a_dir.path()).await;
    let store_id = fx.store_id;
    let owners = png(3_500_000, 7);
    let url = alice.put_blob(store_id, fx.inside, "image/png", &owners).await.unwrap();
    put_on_canvas(&alice, store_id, fx.inside, &url).await;
    share_with(&alice, &fx, &[(BOB, MemberRole::Editor), (CAROL, MemberRole::Editor)]).await;

    let (mut b, bob, _b_dir) = member_device(&env, BOB, &fx).await;
    let (mut c, carol, _c_dir) = member_device(&env, CAROL, &fx).await;
    assert!(wait_until(Duration::from_secs(30), || holds(&bob, store_id, url.blob, &owners)).await, "the owner's picture reaches a member through the relay");

    let bobs = png(50_000, 8);
    let bobs_url = bob.put_blob(store_id, fx.leaf, "image/png", &bobs).await.unwrap();
    put_on_canvas(&bob, store_id, fx.leaf, &bobs_url).await;
    assert!(wait_until(Duration::from_secs(30), || holds(&carol, store_id, bobs_url.blob, &bobs)).await, "a member's picture reaches another member");
    assert!(wait_until(Duration::from_secs(30), || holds(&alice, store_id, bobs_url.blob, &bobs)).await, "and the owner");

    // The twin on the owner's machine holds ciphertext only; nothing is hosted.
    let needle = &owners[owners.len() / 2..owners.len() / 2 + 64];
    assert!(!contains_bytes_recursive(&twin_dir(a_dir.path(), store_id), needle));

    a.stop().await.unwrap();
    b.stop().await.unwrap();
    c.stop().await.unwrap();
}

/// What the owner's app depends on to show a member's canvas edit in its
/// note (Joe, 2026-11-14: a member's agent drew on a canvas in a shared
/// note; the owner saw it only on opening the canvas, never in the note).
/// The app keeps a copy of the open note and merges every update its store
/// subscription announces; opening the canvas reads the store instead. So the
/// announcements alone must bring the owner's copy to the member's drawing.
#[tokio::test]
async fn a_members_canvas_edit_reaches_the_owners_copy_through_the_announcements() {
    use pimble_board::{Board, Geom, Kind, NewElement, Rect, Stamp};
    let count = |doc: &NodeDoc, board: &str| -> usize { Board::in_text(doc, board).snapshot().layers.iter().map(|l| l.count).sum() };
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = relayed_fixture(&env, &alice, a_dir.path()).await;
    let store_id = fx.store_id;
    let now = || chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

    // The owner makes a canvas in the note's text, as Insert Canvas does.
    let node = alice.get_node(store_id, fx.inside).await.unwrap();
    let doc = NodeDoc::load(&node.content).unwrap();
    let before = doc.state_vector();
    Board::in_text(&doc, "c1").create(Stamp::new(None, &now())).unwrap();
    let changes = base64::engine::general_purpose::STANDARD.encode(doc.diff_since(&before).unwrap());
    alice.apply_edit(store_id, fx.inside, "alice-app", EditOperation::IncrementalChanges { changes }).await.unwrap();
    share_with(&alice, &fx, &[(BOB, MemberRole::Editor)]).await;
    let (mut b, bob, _b_dir) = member_device(&env, BOB, &fx).await;
    let bob_has_it = || async { bob.get_node(store_id, fx.inside).await.ok().is_some_and(|n| NodeDoc::load(&n.content).is_ok_and(|d| Board::in_text(&d, "c1").exists())) };
    assert!(wait_until(Duration::from_secs(30), bob_has_it).await, "the member holds the canvas");

    // The owner's app: a copy of the note, and the store's announcements.
    let mut copy = NodeDoc::load(&alice.get_node(store_id, fx.inside).await.unwrap().content).unwrap();
    let mut heard = alice.subscribe_store_changes(store_id).await.unwrap();

    // The member (their agent) draws on it.
    let theirs = NodeDoc::load(&bob.get_node(store_id, fx.inside).await.unwrap().content).unwrap();
    let before = theirs.state_vector();
    Board::in_text(&theirs, "c1").add_element(Stamp::new(None, &now()), NewElement::new(Kind::Rect, Geom::Rect(Rect { x: 10.0, y: 10.0, w: 50.0, h: 30.0 }))).unwrap();
    let changes = base64::engine::general_purpose::STANDARD.encode(theirs.diff_since(&before).unwrap());
    bob.apply_edit(store_id, fx.inside, "bob-agent", EditOperation::IncrementalChanges { changes }).await.unwrap();

    // The store has it (what opening the canvas reads)...
    let stored = || async { count(&NodeDoc::load(&alice.get_node(store_id, fx.inside).await.unwrap().content).unwrap(), "c1") == 1 };
    assert!(wait_until(Duration::from_secs(30), stored).await, "the owner's store holds the member's drawing");
    // ...and the announcements bring the copy there too.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut kinds = Vec::new();
    while count(&copy, "c1") == 0 {
        let Ok(Some(Ok(n))) = tokio::time::timeout_at(deadline, heard.next()).await else { break };
        kinds.push(format!("{:?} from {:?}, update {}", n.change_kind, n.source_client_id, n.update.as_ref().map_or(0, |u| u.len())));
        if let Some(update) = n.update {
            let bytes = base64::engine::general_purpose::STANDARD.decode(update).unwrap();
            let effect = copy.apply_update(&bytes).unwrap();
            kinds.push(format!("  merged: {effect:?}"));
        }
    }
    assert_eq!(count(&copy, "c1"), 1, "the owner's copy shows the member's drawing from what it was told; heard: {kinds:#?}");

    a.stop().await.unwrap();
    b.stop().await.unwrap();
}
