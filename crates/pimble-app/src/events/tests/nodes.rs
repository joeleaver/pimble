//! Nodes across panes: transplants, deletes, remembered layouts, links, the deleted list.

use super::*;

fn left_share(name: &str) -> pimble_core::LeftShare {
    pimble_core::LeftShare { root: NodeId::new(), name: name.to_string() }
}

fn transplanted(
    from_store_id: StoreId,
    old_node_id: NodeId,
    old_parent_id: NodeId,
    to_store_id: StoreId,
    node_id: NodeId,
    new_parent_id: NodeId,
    title: &str,
    left_shares: Vec<pimble_core::LeftShare>,
) -> BackendEvent {
    BackendEvent::NodeTransplanted {
        from_store_id, old_node_id, old_parent_id, to_store_id, node_id, new_parent_id,
        title: title.to_string(), left_shares,
    }
}

/// A move between two shares of one store, the way a member holding both
/// drags a note from one into the other (docs/MOVE_CONTRACT.md
/// "Verification (the bar)"): what it did to both lists is shown before
/// the answer, the new parent's list (never loaded) is loaded so the
/// person sees where it landed, and the notice names the nearest share
/// left.
#[test]
fn a_transplant_leaving_a_share_drops_the_old_row_loads_where_it_landed_and_shows_the_notice() {
    let (store, events, commands) = store_with_events();
    let store_id = StoreId::new();
    let (old_parent, new_parent) = (NodeId::new(), NodeId::new());
    let (old_id, new_id) = (NodeId::new(), NodeId::new());
    store.set_children(store_id, old_parent, vec![(store_id, old_id)]);

    events.send(shown(store_id, Vec::new(), vec![(old_parent, Vec::new())], vec![old_id])).unwrap();
    events
        .send(transplanted(
            store_id, old_id, old_parent, store_id, new_id, new_parent,
            "Grocery list", vec![left_share("Trips"), left_share("Recipes")],
        ))
        .unwrap();
    pump(store);

    assert_eq!(
        store.get_children_signal(store_id, old_parent).unwrap().with(|c| c.clone()),
        Vec::new(),
        "the old row leaves the list it was in"
    );
    assert!(
        commands.try_iter().any(|c| matches!(c, BackendCommand::GetChildren { store_id: s, node_id: n, .. } if s == store_id && n == new_parent)),
        "the new parent's list is asked for, so the person sees where it landed"
    );
    assert_eq!(
        store.notice.get(),
        "\"Grocery list\" was moved out of \"Trips\". The people it is shared with see it as deleted and can put it back.",
        "the nearest share left, first in the list"
    );
}

/// A plain move between stores, with no share left, is quiet: the lists
/// are shown as they are now, but there is nothing to tell anyone.
#[test]
fn a_transplant_between_stores_with_no_share_left_shows_no_notice() {
    let (store, events, commands) = store_with_events();
    let (from_store, to_store) = (StoreId::new(), StoreId::new());
    let (old_parent, new_parent) = (NodeId::new(), NodeId::new());
    let (old_id, new_id) = (NodeId::new(), NodeId::new());
    store.set_children(from_store, old_parent, vec![(from_store, old_id)]);

    events.send(shown(from_store, Vec::new(), Vec::new(), vec![old_id])).unwrap();
    events
        .send(transplanted(from_store, old_id, old_parent, to_store, new_id, new_parent, "Notes", Vec::new()))
        .unwrap();
    pump(store);

    assert_eq!(store.get_children_signal(from_store, old_parent).unwrap().with(|c| c.clone()), Vec::new());
    let asked: Vec<BackendCommand> = commands.try_iter().collect();
    assert!(asked.iter().any(|c| matches!(c, BackendCommand::GetChildren { store_id: s, node_id: n, .. } if *s == to_store && *n == new_parent)));
    assert_eq!(store.notice.get(), "");
}

/// The document open in the editor when it is transplanted follows to
/// its new id: a document opens on the new pair as any newly-selected
/// one does, and the old one is gone from the tree.
#[test]
fn a_transplant_of_the_open_document_moves_the_editor_to_the_new_id() {
    let (store, events, commands) = store_with_events();
    let store_id = StoreId::new();
    let (old_parent, new_parent) = (NodeId::new(), NodeId::new());
    let (old_id, new_id) = (NodeId::new(), NodeId::new());
    store.set_children(store_id, old_parent, vec![(store_id, old_id)]);
    store.select_in_pane(crate::panes::PaneId::FIRST, Some(format!("node_{store_id}_{old_id}")));
    store.panes[0].active_edit.set(Some(crate::state::ActiveEdit { store_id, node_id: old_id }));
    store.node_title.set("Grocery list".to_string());
    store.panes[0].show_editor.set(true);

    events
        .send(transplanted(store_id, old_id, old_parent, store_id, new_id, new_parent, "Grocery list", Vec::new()))
        .unwrap();
    pump(store);

    assert_eq!(store.selected_id.get(), Some(format!("node_{store_id}_{new_id}")));
    assert!(store.panes[0].show_editor.get());
    assert_eq!(store.node_title.get(), "Grocery list");
    assert!(
        commands.try_iter().any(|c| matches!(c, BackendCommand::GetNode { store_id: s, node_id: n } if s == store_id && n == new_id)),
        "the new document is fetched, which is what starts its session"
    );
}

/// A transplant of a node that was not open in the editor leaves the
/// selection and the editor alone — only the tree changes.
#[test]
fn a_transplant_of_a_node_not_open_leaves_the_editor_alone() {
    let (store, events, commands) = store_with_events();
    let store_id = StoreId::new();
    let (old_parent, new_parent) = (NodeId::new(), NodeId::new());
    let (old_id, new_id) = (NodeId::new(), NodeId::new());
    let open_id = NodeId::new();
    store.set_children(store_id, old_parent, vec![(store_id, old_id)]);
    store.select_in_pane(crate::panes::PaneId::FIRST, Some(format!("node_{store_id}_{open_id}")));
    store.panes[0].active_edit.set(Some(crate::state::ActiveEdit { store_id, node_id: open_id }));

    events
        .send(transplanted(store_id, old_id, old_parent, store_id, new_id, new_parent, "Untitled", Vec::new()))
        .unwrap();
    pump(store);

    assert_eq!(store.selected_id.get(), Some(format!("node_{store_id}_{open_id}")), "a document open elsewhere stays open");
    assert!(!commands.try_iter().any(|c| matches!(c, BackendCommand::GetNode { node_id: n, .. } if n == new_id)));
}

/// Three panes: the first two on one document, the third on another
/// (docs/SPLIT_VIEW_CONTRACT.md). The second has the focus.
fn two_panes_on_one_document(store: AppStore) -> (StoreId, NodeId, NodeId, NodeId) {
    use crate::panes::PaneId;
    let store_id = StoreId::new();
    let (parent, shared, other) = (NodeId::new(), NodeId::new(), NodeId::new());
    store.set_children(store_id, parent, vec![(store_id, shared), (store_id, other)]);
    for (pane, node_id) in [(0, shared), (1, shared), (2, other)] {
        store.select_in_pane(PaneId::ALL[pane], Some(format!("node_{store_id}_{node_id}")));
        store.panes[pane].show_editor.set(true);
        store.panes[pane].active_edit.set(Some(crate::state::ActiveEdit { store_id, node_id }));
    }
    store.focus_pane(PaneId::ALL[1]);
    assert_eq!(store.selected_id.get(), Some(format!("node_{store_id}_{shared}")), "the tree's row is the focused pane's");
    (store_id, parent, shared, other)
}

/// A document deleted elsewhere closes in every pane holding it, and in
/// no other (docs/SPLIT_VIEW_CONTRACT.md "What the person sees").
#[test]
fn a_document_deleted_elsewhere_closes_in_every_pane_holding_it() {
    let (store, events, _commands) = store_with_events();
    let (store_id, parent, shared, other) = two_panes_on_one_document(store);

    let other_node = store.get_node_signal(store_id, other).map(|n| n.with(|n| n.clone()));
    events.send(shown(store_id, Vec::new(), vec![(parent, other_node.into_iter().collect())], vec![shared])).unwrap();
    pump(store);

    for pane in [0, 1] {
        assert_eq!(store.panes[pane].selected.get(), None, "pane {pane}");
        assert!(!store.panes[pane].show_editor.get(), "pane {pane}");
    }
    assert_eq!(store.selected_id.get(), None, "the focused pane held it");
    assert_eq!(store.panes[2].selected.get(), Some(format!("node_{store_id}_{other}")));
    assert!(store.panes[2].show_editor.get(), "a pane on another document stays as it is");
}

/// A transplanted document is followed to its new id in every pane that
/// had it open.
#[test]
fn a_transplant_is_followed_in_every_pane_holding_the_document() {
    let (store, events, commands) = store_with_events();
    let (store_id, parent, shared, other) = two_panes_on_one_document(store);
    let (new_id, new_parent) = (NodeId::new(), NodeId::new());

    events.send(transplanted(store_id, shared, parent, store_id, new_id, new_parent, "Moved", Vec::new())).unwrap();
    pump(store);

    let value = Some(format!("node_{store_id}_{new_id}"));
    assert_eq!(store.panes[0].selected.get(), value);
    assert_eq!(store.panes[1].selected.get(), value);
    assert_eq!(store.selected_id.get(), value);
    assert_eq!(store.panes[2].selected.get(), Some(format!("node_{store_id}_{other}")));
    let fetched = commands.try_iter().filter(|c| matches!(c, BackendCommand::GetNode { node_id: n, .. } if *n == new_id)).count();
    assert_eq!(fetched, 1, "the new document is fetched once for both panes");
}

/// A remembered layout's documents (docs/SPLIT_VIEW_CONTRACT.md
/// "Persistence"): asked after when their store opens, opened in every
/// pane that held one when it is there, and left empty, with nothing
/// said, when it is gone.
#[test]
fn a_remembered_layout_opens_its_documents_when_their_store_does() {
    use crate::panes::{Direction, PaneId, SavedDocument, SavedPanes, Tiling};
    use pimble_core::{LinkResolution, PimbleUrl};
    let (store, events, commands) = store_with_events();
    let opened = pimble_core::Store::new_local("Notes", "/tmp/notes.pimble".into());
    let store_id = opened.id;
    let (kept, gone) = (NodeId::new(), NodeId::new());
    let mut tiling = Tiling::default();
    tiling.split(PaneId::ALL[0], Direction::Right);
    tiling.split(PaneId::ALL[1], Direction::Down);
    store.restore_panes(SavedPanes {
        tiling: tiling.clone(),
        focused: PaneId::ALL[1],
        documents: vec![
            SavedDocument { pane: PaneId::ALL[0], store_id, node_id: kept },
            SavedDocument { pane: PaneId::ALL[1], store_id, node_id: kept },
            SavedDocument { pane: PaneId::ALL[2], store_id, node_id: gone },
        ],
        beside: Vec::new(),
    });
    assert_eq!(store.tiling.get(), tiling, "the tiling is back at once");
    assert_eq!(store.focused(), PaneId::ALL[1]);
    assert_eq!(store.saved_panes().documents.len(), 3, "a pane still waiting keeps its document in what is saved");

    events.send(BackendEvent::StoreOpened { store: opened }).unwrap();
    pump(store);
    let asked: Vec<NodeId> = commands
        .try_iter()
        .filter_map(|c| match c {
            BackendCommand::ResolveLink { url, hops: 0 } if url.store == store_id => Some(url.node),
            _ => None,
        })
        .collect();
    assert_eq!(asked, vec![kept, gone], "each document is asked after once");

    events.send(BackendEvent::LinkResolved { url: PimbleUrl::node(store_id, kept), hops: 0, resolution: LinkResolution::Live { store_id, node_id: kept } }).unwrap();
    events.send(BackendEvent::LinkResolved { url: PimbleUrl::node(store_id, gone), hops: 0, resolution: LinkResolution::Deleted { store_id, node_id: gone } }).unwrap();
    pump(store);
    let value = Some(format!("node_{store_id}_{kept}"));
    assert_eq!(store.panes[0].selected.get(), value);
    assert_eq!(store.panes[1].selected.get(), value);
    assert_eq!(store.selected_id.get(), value, "the focused pane's document is the tree's row");
    assert_eq!(store.panes[2].selected.get(), None, "a note that is gone leaves its pane empty");
    assert_eq!(store.link_open.get(), None, "nothing is followed");
    assert!(store.notice.get().is_empty(), "and nothing is said");
    let sent: Vec<BackendCommand> = commands.try_iter().collect();
    assert_eq!(sent.iter().filter(|c| matches!(c, BackendCommand::GetNode { node_id: n, .. } if *n == kept)).count(), 1);
    assert!(!sent.iter().any(|c| matches!(c, BackendCommand::GetNode { node_id: n, .. } if *n == gone)));
    assert_eq!(store.saved_panes().documents, Vec::new(), "nothing is open yet, and nothing is waiting");
}

/// A link followed from a pane opens in that pane: the resolution names
/// it, whichever pane has the focus by the time the answer arrives.
#[test]
fn a_link_followed_from_a_pane_opens_in_that_pane() {
    use crate::panes::PaneId;
    use pimble_core::{LinkResolution, PimbleUrl};
    let (store, events, _commands) = store_with_events();
    let (store_id, node_id) = (StoreId::new(), NodeId::new());
    let url = PimbleUrl::node(store_id, node_id);

    store.link_from.set(Some(PaneId::ALL[2]));
    events.send(BackendEvent::LinkResolved { url: url.clone(), hops: 0, resolution: LinkResolution::Live { store_id, node_id } }).unwrap();
    pump(store);
    assert_eq!(store.link_open.get().expect("it opens").pane, Some(PaneId::ALL[2]));
    assert_eq!(store.link_from.get(), None);
    store.link_open.set(None);

    // A link that leads nowhere forgets the pane it was followed from.
    store.link_from.set(Some(PaneId::ALL[1]));
    events.send(BackendEvent::LinkResolved { url, hops: 0, resolution: LinkResolution::Missing }).unwrap();
    pump(store);
    assert_eq!(store.link_open.get(), None);
    assert_eq!(store.link_from.get(), None);
}

fn deleted_node(
    node: pimble_core::Node,
    parent_title: Option<&str>,
    deleted_at: Option<&str>,
    put_back_under: Option<NodeId>,
) -> pimble_core::DeletedNode {
    pimble_core::DeletedNode {
        node,
        parent_title: parent_title.map(str::to_string),
        deleted_at: deleted_at.map(str::to_string),
        put_back_under,
    }
}

/// What following a link does with each answer (docs/LINKS_CONTRACT.md
/// "Following a link"): a live node is asked to open with the link's
/// anchor; a chain that stopped in another store this app holds is asked
/// again there, up to the hop limit; everything else is a sentence, and a
/// note that moved where this app cannot follow reads as no access.
#[test]
fn a_resolved_link_opens_asks_again_or_says_why_not() {
    use pimble_core::{Anchor, LinkResolution, PimbleUrl, MAX_LINK_HOPS};
    let (store, events, commands) = store_with_events();
    let held = pimble_core::Store::new_local("Other", "/tmp/other.pimble".into());
    let held_id = held.id;
    store.upsert_store(held);
    let (asked_store, asked_node, moved_to) = (StoreId::new(), NodeId::new(), NodeId::new());
    let anchor = Anchor::new(None, "the spot");
    let url = PimbleUrl::deep(asked_store, asked_node, anchor.clone());

    events.send(BackendEvent::LinkResolved { url: url.clone(), hops: 0, resolution: LinkResolution::Live { store_id: held_id, node_id: moved_to } }).unwrap();
    pump(store);
    let open = store.link_open.get().expect("a live node is asked to open");
    assert_eq!((open.store_id, open.node_id, open.anchor), (held_id, moved_to, Some(anchor.clone())));
    assert_eq!(open.value, format!("node_{held_id}_{moved_to}"));
    store.link_open.set(None);

    // Stopped in a store this app holds: asked again there, one hop on.
    events.send(BackendEvent::LinkResolved { url: url.clone(), hops: 2, resolution: LinkResolution::StoreNotHere { store_id: held_id, node_id: moved_to } }).unwrap();
    pump(store);
    match commands.try_recv().expect("asked again") {
        BackendCommand::ResolveLink { url: next, hops } => {
            assert_eq!((next.store, next.node, next.anchor, hops), (held_id, moved_to, Some(anchor.clone()), 3));
        }
        other => panic!("unexpected {other:?}"),
    }

    // The link's own store is not here: that sentence. The note moved on
    // into a store this app does not hold: no access, whichever hop it
    // was. Out of hops: it leads nowhere. The rest: their own sentence.
    let not_here = LinkResolution::StoreNotHere { store_id: asked_store, node_id: asked_node };
    for (resolution, hops, said) in [
        (not_here.clone(), 0, not_here.sentence()),
        (LinkResolution::StoreNotHere { store_id: StoreId::new(), node_id: moved_to }, 0, LinkResolution::NoAccess.sentence()),
        (LinkResolution::StoreNotHere { store_id: asked_store, node_id: moved_to }, 1, LinkResolution::NoAccess.sentence()),
        (LinkResolution::StoreNotHere { store_id: held_id, node_id: moved_to }, MAX_LINK_HOPS, LinkResolution::Missing.sentence()),
        (LinkResolution::NoAccess, 0, LinkResolution::NoAccess.sentence()),
        (LinkResolution::Missing, 0, LinkResolution::Missing.sentence()),
        (LinkResolution::Deleted { store_id: asked_store, node_id: asked_node }, 0, LinkResolution::Deleted { store_id: asked_store, node_id: asked_node }.sentence()),
    ] {
        let sentence = said.unwrap();
        events.send(BackendEvent::LinkResolved { url: url.clone(), hops, resolution }).unwrap();
        pump(store);
        assert_eq!(store.notice.get(), sentence);
        assert!(commands.try_recv().is_err(), "nothing asked again for {sentence}");
        assert_eq!(store.link_open.get(), None);
    }
}

/// `DeletedListed` fills the modal's rows exactly as the contract lists
/// them: an empty title reads "Untitled", and each `DeletedNode` becomes
/// one row. A late answer for a store the modal has moved on from names
/// nothing.
#[test]
fn deleted_listed_fills_the_modals_rows() {
    let (store, events, _commands) = store_with_events();
    let store_id = StoreId::new();
    store.deleted_modal_store.set(Some(store_id));

    let named = pimble_core::Node::document("Pasta");
    let named_id = named.id;
    let mut untitled = pimble_core::Node::document("");
    untitled.access = pimble_core::StoreAccess::Read;
    let untitled_id = untitled.id;
    let put_back_under = NodeId::new();

    events
        .send(BackendEvent::DeletedListed {
            store_id,
            nodes: vec![
                deleted_node(named, Some("Recipes"), Some("2026-09-21T14:32:10Z"), None),
                deleted_node(untitled, None, None, Some(put_back_under)),
            ],
        })
        .unwrap();
    pump(store);

    let rows = store.deleted_modal_nodes.with(|rows| rows.clone());
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].node_id, named_id);
    assert_eq!(rows[0].title, "Pasta");
    assert_eq!(rows[0].parent_title.as_deref(), Some("Recipes"));
    assert_eq!(rows[0].deleted_at.as_deref(), Some("2026-09-21T14:32:10Z"));
    assert_eq!(rows[0].access, pimble_core::StoreAccess::Full);
    assert_eq!(rows[1].node_id, untitled_id);
    assert_eq!(rows[1].title, "Untitled", "an empty title reads Untitled");
    assert_eq!(rows[1].put_back_under, Some(put_back_under));
    assert_eq!(rows[1].access, pimble_core::StoreAccess::Read);

    // The modal moved to another store meanwhile: a late answer for the
    // one it left is not shown.
    store.deleted_modal_store.set(Some(StoreId::new()));
    events.send(BackendEvent::DeletedListed { store_id, nodes: Vec::new() }).unwrap();
    pump(store);
    assert_eq!(store.deleted_modal_nodes.with(|rows| rows.len()), 2, "a late answer for a store the modal left changes nothing");
}

/// The modal being open is not enough to claim an `Error`: something else
/// in flight (a sync refusal, a mount fetch) while the person happens to
/// have "Recently Deleted..." open must still reach the notice or the
/// status bar, or a "Put Back" would look like it failed when it never
/// even ran.
#[test]
fn an_error_with_nothing_pending_skips_the_deleted_modal() {
    let (store, events, _commands) = store_with_events();
    store.deleted_modal_store.set(Some(StoreId::new()));

    events
        .send(BackendEvent::Error {
            message: format!("Forbidden: {}", pimble_core::StoreAccess::READ_ONLY_REFUSAL),
        })
        .unwrap();
    pump(store);

    assert!(store.deleted_modal_error.get().is_empty(), "not this modal's request");
    assert_eq!(store.notice.get(), pimble_core::StoreAccess::READ_ONLY_REFUSAL);
}

/// A "Put Back" that fails lands in the modal, the way `open_deleted_modal`
/// and `put_back_now` set `deleted_modal_pending` before sending.
#[test]
fn a_failed_put_back_lands_in_the_deleted_modal() {
    let (store, events, _commands) = store_with_events();
    store.deleted_modal_store.set(Some(StoreId::new()));
    store.deleted_modal_pending.set(true);

    events
        .send(BackendEvent::Error { message: "Put Back failed".to_string() })
        .unwrap();
    pump(store);

    assert_eq!(store.deleted_modal_error.get(), "Put Back failed");
    assert!(!store.deleted_modal_pending.get(), "the flag clears once the error lands");
    assert!(store.notice.get().is_empty(), "not the status bar's error");
}

/// What the explorer's New makes, as `NodeCreated` answers it: the
/// command sent and the node made under `parent`.
fn made(store: AppStore, events: &crossbeam_channel::Sender<BackendEvent>, commands: &crossbeam_channel::Receiver<BackendCommand>, kind: crate::creating::Kind, store_id: StoreId, parent: NodeId, suffix: &str) -> NodeId {
    crate::creating::make(store, &kind, store_id, parent, pimble_core::Place::Last, suffix.to_string());
    assert!(
        commands.try_iter().any(|c| matches!(c, BackendCommand::CreateNode { store_id: s, parent_id: Some(p), .. } if s == store_id && p == parent)),
        "{kind:?} is made under its parent"
    );
    let node_id = NodeId::new();
    events.send(BackendEvent::NodeCreated { store_id, parent_id: Some(parent), node_id, request: None }).unwrap();
    pump(store);
    node_id
}

/// A new text note opens in the focused pane as soon as it is made (Joe,
/// 2026-11-26), at the tree value of its level, and its row is the one
/// renamed when drawn; the other panes keep what they show.
#[test]
fn a_new_text_opens_in_the_focused_pane_and_is_named_in_place() {
    use crate::panes::PaneId;
    let (store, events, commands) = store_with_events();
    let (store_id, parent, shared, _other) = two_panes_on_one_document(store);
    store.focus_pane(PaneId::ALL[2]);
    let _ = commands.try_iter().count();

    let suffix = format!("/{store_id}_{}", NodeId::new());
    let node_id = made(store, &events, &commands, crate::creating::Kind::Text, store_id, parent, &suffix);

    let value = format!("node_{store_id}_{node_id}{suffix}");
    assert_eq!(store.panes[2].selected.get(), Some(value.clone()), "the focused pane opens it");
    assert_eq!(store.selected_id.get(), Some(value.clone()), "and the tree's row is it");
    assert!(
        commands.try_iter().any(|c| matches!(c, BackendCommand::GetNode { store_id: s, node_id: n } if s == store_id && n == node_id)),
        "a node the cache has not seen is fetched, which starts its session"
    );
    for pane in [0, 1] {
        assert_eq!(store.panes[pane].selected.get(), Some(format!("node_{store_id}_{shared}")), "pane {pane} keeps its note");
    }
    assert!(crate::state::take_pending_rename(&value), "its row starts renaming once drawn");
}

/// A new query metanode opens too (its setup), as it did before texts did.
#[test]
fn a_new_metanode_opens_in_the_focused_pane() {
    let (store, events, commands) = store_with_events();
    let (store_id, parent) = (StoreId::new(), NodeId::new());
    let node_id = made(store, &events, &commands, crate::creating::Kind::Metanode, store_id, parent, "");
    let value = format!("node_{store_id}_{node_id}");
    assert_eq!(store.selected_id.get(), Some(value.clone()));
    assert!(crate::state::take_pending_rename(&value));
}

/// A kind named by what it is (a sheet) is made and left where it is: it
/// neither opens nor renames, and a node made by anyone else opens nothing.
#[test]
fn a_new_sheet_or_a_node_made_elsewhere_opens_nothing() {
    let (store, events, commands) = store_with_events();
    let (store_id, parent) = (StoreId::new(), NodeId::new());
    let node_id = made(store, &events, &commands, crate::creating::Kind::Sheet, store_id, parent, "");
    assert_eq!(store.selected_id.get(), None);
    assert!(!crate::state::take_pending_rename(&format!("node_{store_id}_{node_id}")));

    let elsewhere = NodeId::new();
    events.send(BackendEvent::NodeCreated { store_id, parent_id: Some(parent), node_id: elsewhere, request: None }).unwrap();
    pump(store);
    assert_eq!(store.selected_id.get(), None, "only the node the explorer asked for opens");
}
