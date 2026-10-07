//! Editor integration: the rinch `Editor {}` component + `EditorHandle`, with
//! M9 collaboration wired onto pimble's existing per-node change relay.
//!
//! Pimble has four pane slots (docs/SPLIT_VIEW_CONTRACT.md), each with its own
//! editor, mounted once, into which the pane's document is swapped. So there
//! is a thread-local table of [`EditorHandle`]s keyed by [`PaneId`], and each
//! pane that holds a document has its own collaboration session.
//! Collaboration maps cleanly onto pimble's transport:
//!
//! * a local edit's delta → `outbound` → [`BackendCommand::BroadcastChanges`] → the
//!   server applies it (`apply_edit` → `ContentDoc::apply_update`) and relays it to
//!   peers; the same delta goes straight to every other pane in this window that
//!   holds the node, because the server relays to other clients and not back;
//! * a peer's delta arrives as `BackendEvent::RemoteChanges` →
//!   [`apply_remote`] → [`EditorHandle::collab_receive`] of every pane holding
//!   the node.
//!
//! The node's stored content IS the collab session snapshot (a yrs v1 update), so the
//! server's per-node `ContentDoc` shares the editor's CRDT lineage and incremental
//! deltas apply cleanly.

use std::cell::RefCell;
use std::collections::HashMap;

use pimble_core::{NodeId, StoreId};
use rinch::prelude::*;
use rinch_editor_core::Node as EditorNode;

use crate::panes::{PaneId, MAX_PANES};
use crate::protocol::BackendCommand;
use crate::rinch_editor::{create_editor, EditorHandle};
use crate::state::{label_from_title_and_content, ActiveEdit, AppStore};

thread_local! {
    /// Each pane slot's editor handle, created lazily on first use.
    static EDITORS: RefCell<[Option<EditorHandle>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
    /// The pending debounced label refresh of each node under local edit
    /// (see `schedule_label_refresh`). Per node, not per pane: two panes
    /// typing into the same note owe its row one refresh.
    static LABEL_REFRESH: RefCell<HashMap<(StoreId, NodeId), TimeoutHandle>> = RefCell::new(HashMap::new());
}

/// Pane `pane`'s editor handle (created on first use). Cheap to clone (an
/// `Rc`); the pane's toolbar, its `Editor {}` component, and the event loop
/// all share this one handle.
pub(crate) fn editor(pane: PaneId) -> EditorHandle {
    EDITORS.with(|e| {
        e.borrow_mut()[pane.index()]
            .get_or_insert_with(|| {
                let handle = create_editor();
                // Before any content: adding a plugin resets history.
                handle.add_plugin(std::rc::Rc::new(crate::links::LinksPlugin { pane }));
                // After the links plugin: a pasted link is claimed there.
                handle.add_plugin(std::rc::Rc::new(crate::pictures::PicturesPlugin));
                // Typing, commands and remote deltas all land here; the
                // toolbar's active states follow immediately (cursor-only
                // moves are covered by the toolbar's own watcher).
                handle.on_change(move || crate::toolbar::bump_toolbar(pane));
                handle
            })
            .clone()
    })
}

/// The focused pane's editor: what the link picker and everything else that
/// acts on "the editor" acts on.
pub(crate) fn focused_editor(store: AppStore) -> EditorHandle {
    editor(store.focused())
}

/// Switch every pane's editor stylesheet between the dark and light schemes.
pub(crate) fn set_dark_mode(dark: bool) {
    for pane in PaneId::ALL {
        editor(pane).set_dark_mode(dark);
    }
}

/// A key was pressed in pane `pane`'s editor, so that is where the keyboard
/// is and the pane has the focus (a press of the pointer reaches the pane
/// itself: `pane_view::render_pane`). The link picker belongs to the pane it
/// was opened in and closes when the focus leaves it.
fn editor_took_focus(store: AppStore, pane: PaneId) {
    crate::pane_view::focus_pane(store, pane);
}

/// Give `delta`, which pane `from` just made, to every other pane of this
/// window that holds the node. A turn later, not from inside the edit: this
/// is called while `from`'s editor is mid-dispatch, and an editor receiving
/// a change runs whatever effects are waiting, some of which reach back
/// into the editor that is still busy (in the browser that was a `RefCell`
/// panic on the second key typed). Deltas are handed over in the order they
/// were made; a pane that has moved to another note by then gets nothing.
fn hand_across(store: AppStore, from: PaneId, store_id: StoreId, node_id: NodeId, delta: Vec<u8>) {
    let deliver = move || {
        for other in store.panes_editing(store_id, node_id) {
            if other != from {
                editor(other).collab_receive(&delta);
            }
        }
    };
    // No event loop runs a timer under `cargo test`.
    #[cfg(test)]
    deliver();
    #[cfg(not(test))]
    let _ = set_timeout(0, deliver);
}

/// Begin editing `node_id` in pane `pane`: load its content into the pane's
/// editor and start a collaboration session wired to the server relay.
/// `content_bytes` is the node's stored content from the server — a yrs collab
/// snapshot, or empty for a new node.
pub(crate) fn start_editing(
    store: AppStore,
    pane: PaneId,
    store_id: StoreId,
    node_id: NodeId,
    content_bytes: &[u8],
) {
    // Whatever the pane held is let go of first, as it always was; done
    // through `stop_editing` so the node's subscription is counted.
    stop_editing(store, pane);
    let handle = editor(pane);
    // Another pane of this window may hold the node already: its session is
    // the freshest copy there is (the cache is as old as the last fetch), and
    // for a note nothing was ever written to, joining it is what keeps two
    // panes from each hosting an empty document of their own, which would
    // merge into two paragraphs.
    let already_open = store.panes_editing(store_id, node_id);
    let from_sibling = already_open.iter().find_map(|other| editor(*other).collab_snapshot());
    let content_bytes: &[u8] = from_sibling.as_deref().unwrap_or(content_bytes);
    // The editor's built-in stylesheet is light unless the container carries
    // `data-pm-theme="dark"`; `set_dark_mode` is a no-op before the view
    // mounts, so apply the app's scheme here, once a document is opened.
    handle.set_dark_mode(untracked(|| store.dark_mode.get()));
    crate::links::set_app_store(store);
    // Pictures (docs/IMAGES_CONTRACT.md): shown from the store's blobs, and
    // a pasted or dropped one stored there before it enters the document.
    crate::pictures::opening(store, pane);
    // An unmounted or switched editor says nothing more about its hovered
    // link (rinch #892): the tooltip goes with the note it was over.
    crate::links::hover_link(store, None);
    // Links (docs/LINKS_CONTRACT.md): Ctrl/Cmd+click follows one, a plain
    // click places the caret as ever; resting on one shows where it leads.
    // A link followed from this pane opens in this pane.
    handle.on_link_click(move |click| {
        if !click.primary {
            return false;
        }
        store.link_from.set(Some(pane));
        crate::links::follow_href(store, &click.link.href);
        true
    });
    handle.on_link_hover(move |hover| crate::links::hover_link(store, hover));
    // The link picker (`[[`, Ctrl+L): its keys come first, it follows the
    // caret and the typing after the brackets.
    // The picker is the focused pane's: a selection that moves in another
    // pane (a peer's edit above its caret) is not the picker's business.
    handle.on_key(move |key| {
        editor_took_focus(store, pane);
        let modified = key.primary || key.ctrl || key.meta || key.alt;
        crate::link_picker::key(store, &editor(pane), key.key, modified)
    });
    handle.on_selection_change(move |_| {
        if store.focused() == pane {
            crate::link_picker::selection_changed(store, &editor(pane));
        }
    });
    handle.on_caret_moved(move || {
        if store.focused() == pane {
            crate::link_picker::caret_moved(store, &editor(pane));
        }
    });

    // Every local edit's delta is base64-broadcast to the server, which persists it
    // and relays it to the other clients. The closure captures only `Copy` values
    // (AppStore/StoreId/NodeId), so it is itself `Copy` and can be reused below.
    let outbound = move |delta: Vec<u8>| {
        use base64::Engine;
        // A document shared read-only takes remote changes but sends none
        // back. The editor refuses the edit before it gets here
        // ([`set_read_only`], which the pane switches as the access changes),
        // so nothing reaches this from a read-only document; this is the
        // invariant kept where a delta leaves, judged per node by the
        // server's own word on it (`AppStore::node_access`): a store can
        // hold a root this account reads beside one it edits, and a delta
        // the server would refuse is text on this screen that exists
        // nowhere. `store_id` is the node's CANONICAL store (`open_node`
        // parses the tree value, which strips any mount path), so a node
        // reached through a mount is judged as the node the edit would be
        // written to.
        if !store.node_access(store_id, node_id).allows_write() {
            tracing::warn!("an edit to read-only node {node_id} reached the outbound path and was dropped");
            return;
        }
        let changes = base64::engine::general_purpose::STANDARD.encode(&delta);
        store.send(BackendCommand::BroadcastChanges {
            store_id,
            node_id,
            changes,
        });
        // The same note open in another pane of this window: the server
        // relays to other clients, not back to the one that sent, so the
        // window hands the delta across itself. Never to this pane.
        hand_across(store, pane, store_id, node_id, delta);
        // The tree's label Effect only reads `node_data`/`live_label`, neither
        // of which reflects this edit on its own — refresh our own label so
        // this window doesn't wait on a GetNode round trip to stop showing a
        // stale one (the other clients already re-fetch on `NodeContentUpdated`).
        schedule_label_refresh(store, pane, store_id, node_id);
    };

    // Join the shared CRDT if the stored content is a non-empty, readable yrs
    // snapshot. Otherwise (empty content, or a guest join that failed) host a
    // fresh session from an empty document and persist its snapshot so the
    // server's node doc adopts a format future deltas apply cleanly against.
    let joined = if content_bytes.is_empty() {
        false
    } else {
        match handle.start_collaboration_guest(content_bytes, outbound) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("start_collaboration_guest failed: {e}");
                false
            }
        }
    };

    if !joined {
        // Empty content → an empty editable paragraph. (rinch's load_html now
        // repairs an empty/whitespace parse to a single paragraph itself.)
        handle.load_html("");
        match handle.start_collaboration_host(outbound) {
            Ok(snapshot) => {
                // Seeding the node's document is a content write; a node
                // this device may only read is left exactly as it is.
                if store.node_access(store_id, node_id).allows_write() {
                    store.send(BackendCommand::SetNodeContent {
                        store_id,
                        node_id,
                        content: snapshot,
                    });
                }
            }
            Err(e) => tracing::warn!("start_collaboration_host failed: {e}"),
        }
    }

    let state = store.pane(pane);
    state.active_edit.set(Some(ActiveEdit { store_id, node_id }));
    store.editor_dirty.set(false);
    // A followed deep link into this note: its spot, now that the session
    // holds the content.
    let arrived = untracked(|| state.pending_anchor.get()).filter(|(s, n, _)| (*s, *n) == (store_id, node_id));
    if let Some((_, _, anchor)) = arrived {
        state.pending_anchor.set(None);
        crate::links::place_anchor(&handle, &anchor);
    }
    // See edits from other clients: once per node, however many panes hold
    // it.
    if already_open.is_empty() {
        store.send(BackendCommand::SubscribeNodeChanges { store_id, node_id });
    }
    // Offline-first open: the session started from the cached bytes above
    // (instant, no flash); now reconcile with the server's copy, which may
    // hold edits this cache never saw (this window's own earlier session,
    // another window, a replica). The answer lands in `apply_reconcile`.
    request_reconcile(store, pane, store_id, node_id);

    // A different document is under the toolbar now.
    crate::toolbar::bump_toolbar(pane);
}

/// Stop editing pane `pane`'s node (detach the collab session). Nothing to do
/// for a pane that holds none.
pub(crate) fn stop_editing(store: AppStore, pane: PaneId) {
    let state = store.pane(pane);
    let Some(active) = untracked(|| state.active_edit.get()) else { return };
    let handle = editor(pane);
    // The node cache holds the content bytes from the last fetch; the
    // session's edits never touched them. Write the session's final state
    // back so labels and anything else reading the cache see what was
    // typed. (Opening a document still fetches from the server, which may
    // hold remote edits this session never saw.)
    if let Some(snapshot) = handle.collab_snapshot() {
        if let Some(sig) = store.get_node_signal(active.store_id, active.node_id) {
            sig.update(|node| node.content = snapshot);
        }
    }
    handle.stop_collaboration();
    crate::links::hover_link(store, None);
    if store.focused() == pane {
        crate::link_picker::close(store);
    }
    state.active_edit.set(None);
    // The last pane holding the node lets go of its subscription and of the
    // label refresh it may be owed.
    if !store.is_editing(active.store_id, active.node_id) {
        store.send(BackendCommand::UnsubscribeNodeChanges { store_id: active.store_id, node_id: active.node_id });
        cancel_pending_label_refresh(active.store_id, active.node_id);
    }
    crate::toolbar::bump_toolbar(pane);
}

/// Lock or unlock pane `pane`'s editor (docs/NODE_DOCUMENT_CONTRACT.md
/// section 5, "Roles"). rinch's switch lives on the handle, so it works
/// before the view mounts and survives a re-mount; locked, the editor refuses
/// every local change (typing, IME, paste, commands, undo) while remote
/// changes keep applying, and the caret, selection and copy still work. Each
/// pane calls this from an effect over its own node's access
/// (`AppStore::node_access`), so a role that changes while a document is
/// open flips it, in every pane that holds the document.
pub(crate) fn set_read_only(pane: PaneId, read_only: bool) {
    editor(pane).set_read_only(read_only);
}

/// Ask the server for what it has beyond the state vector of pane `pane`'s
/// session (`syncNodeContent`). Used when a document opens and when the
/// connection comes back after a failover.
pub(crate) fn request_reconcile(store: AppStore, pane: PaneId, store_id: StoreId, node_id: NodeId) {
    if let Some(state_vector) = editor(pane).collab_state_vector() {
        store.send(BackendCommand::ReconcileNodeContent { store_id, node_id, state_vector });
    }
}

/// Merge the server's answer to `request_reconcile` into the session of
/// every pane holding the node, then send the server whatever a session has
/// that it lacks. Ignored by a pane that has moved to another node
/// meanwhile. (Each pane asks with its own state vector; an answer meant for
/// one is a harmless merge in another, since they hold the same document.)
pub(crate) fn apply_reconcile(
    store: AppStore,
    store_id: StoreId,
    node_id: NodeId,
    diff: &[u8],
    server_state_vector: &[u8],
) {
    for pane in store.panes_editing(store_id, node_id) {
        let handle = editor(pane);
        if !diff.is_empty() {
            // `collab_receive` re-projects the view and does not re-broadcast.
            handle.collab_receive(diff);
            schedule_label_refresh(store, pane, store_id, node_id);
            crate::toolbar::bump_toolbar(pane);
        }
        // A document shared read-only sends nothing back, here as in `outbound`:
        // the session's own state is not the server's to take
        // (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles"). Without this the
        // reconcile that follows every open pushes the freshly hosted empty
        // document, and the server refuses it out loud.
        if !store.node_access(store_id, node_id).allows_write() {
            continue;
        }
        if let Some(ours) = handle.collab_sync_diff(server_state_vector) {
            if !ours.is_empty() {
                use base64::Engine;
                let changes = base64::engine::general_purpose::STANDARD.encode(&ours);
                store.send(BackendCommand::BroadcastChanges { store_id, node_id, changes });
            }
        }
    }
}

/// Cancel the debounced label refresh a node may still be owed.
fn cancel_pending_label_refresh(store_id: StoreId, node_id: NodeId) {
    LABEL_REFRESH.with(|pending| {
        if let Some(handle) = pending.borrow_mut().remove(&(store_id, node_id)) {
            clear_timeout(handle);
        }
    });
}

/// Schedule a debounced refresh (~300ms after the last call) of `node_id`'s tree
/// label, computed straight from the document of the pane that was typed in —
/// no CRDT decode/projection through `ContentDoc`, since the editor already
/// has the model. Called from the collaboration `outbound` closure after each
/// local edit. One pending refresh per node, whichever pane asked last.
fn schedule_label_refresh(store: AppStore, pane: PaneId, store_id: StoreId, node_id: NodeId) {
    cancel_pending_label_refresh(store_id, node_id);
    let timeout = set_timeout(300, move || {
        LABEL_REFRESH.with(|pending| { pending.borrow_mut().remove(&(store_id, node_id)); });
        // The pane may have moved on in the meantime; any pane still
        // holding the node has the same words.
        let holders = store.panes_editing(store_id, node_id);
        let Some(holder) = holders.iter().find(|p| **p == pane).or(holders.first()).copied() else { return };
        if let Some(label) = compute_live_label(store, store_id, node_id, &editor(holder)) {
            store.live_label_signal((store_id, node_id)).set_if_changed(Some(label));
        }
    });
    LABEL_REFRESH.with(|pending| { pending.borrow_mut().insert((store_id, node_id), timeout); });
}

/// Compute `node_id`'s tree label from the editor's live document, mirroring
/// `label_from_title_and_content`'s priority (explicit title, then first
/// non-empty line, then the node's title, then "Untitled") — the only
/// difference from `display_label_from_node` is that the content text comes
/// from the in-memory document rather than a decoded `ContentDoc` snapshot.
/// `None` if the node isn't tracked locally (e.g. it was deleted mid-debounce).
fn compute_live_label(store: AppStore, store_id: StoreId, node_id: NodeId, handle: &EditorHandle) -> Option<String> {
    let node_sig = store.get_node_signal(store_id, node_id)?;
    let (has_explicit_title, title) = untracked(|| {
        node_sig.with(|n| {
            let explicit = n.metadata.custom
                .get("explicit_title")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            (explicit, n.metadata.title.clone())
        })
    });
    if has_explicit_title && !title.is_empty() {
        return Some(title);
    }
    let content = doc_text(handle);
    Some(label_from_title_and_content(has_explicit_title, &title, &content))
}

/// The editor's current document as plain text, blocks joined by `'\n'` — the
/// same walk `main.rs`'s collaboration test uses (`doc_text`), kept private
/// here so the label refresh can read the live model with no CRDT involved.
fn doc_text(handle: &EditorHandle) -> String {
    fn collect(n: &EditorNode, out: &mut String) {
        if let Some(t) = n.text() {
            out.push_str(t);
            return;
        }
        for i in 0..n.child_count() {
            collect(n.child(i), out);
        }
    }
    let doc = handle.doc();
    let mut s = String::new();
    for i in 0..doc.child_count() {
        if i > 0 {
            s.push('\n');
        }
        collect(doc.child(i), &mut s);
    }
    s
}

/// Apply a peer's remote delta (base64-decoded `bytes`) to the editor of every
/// pane holding the node — the `BackendEvent::RemoteChanges` path. Integrates
/// and re-projects without re-broadcasting.
pub(crate) fn apply_remote(store: AppStore, store_id: StoreId, node_id: NodeId, bytes: &[u8]) {
    for pane in store.panes_editing(store_id, node_id) {
        editor(pane).collab_receive(bytes);
    }
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;
    use crate::protocol::{BackendEvent, BackendHandle};
    use crossbeam_channel::bounded;
    use rinch_editor_core::{Pos, Selection};

    const P: [PaneId; MAX_PANES] = PaneId::ALL;

    fn store_with_commands() -> (AppStore, crossbeam_channel::Receiver<BackendCommand>) {
        let store = AppStore::new();
        let (cmd_tx, cmd_rx) = bounded::<BackendCommand>(256);
        let (_event_tx, event_rx) = bounded::<BackendEvent>(1);
        store.backend.set(Some(BackendHandle { cmd_tx, event_rx }));
        (store, cmd_rx)
    }

    fn type_at_end(pane: PaneId, text: &str) {
        let handle = editor(pane);
        let end = handle.doc().content_size().saturating_sub(1);
        handle.set_selection(Selection::cursor(Pos(end)));
        assert!(handle.insert_text(text));
    }

    fn subscribes(commands: &[BackendCommand], node_id: NodeId) -> usize {
        commands.iter().filter(|c| matches!(c, BackendCommand::SubscribeNodeChanges { node_id: n, .. } if *n == node_id)).count()
    }

    fn unsubscribes(commands: &[BackendCommand], node_id: NodeId) -> usize {
        commands.iter().filter(|c| matches!(c, BackendCommand::UnsubscribeNodeChanges { node_id: n, .. } if *n == node_id)).count()
    }

    /// The same note in two panes (docs/SPLIT_VIEW_CONTRACT.md "Editors and
    /// collaboration"): what is typed in one appears in the other and goes
    /// to the server once; a peer's delta reaches both and no other pane;
    /// the node is subscribed once and let go of when the last pane does.
    #[test]
    fn the_same_note_in_two_panes_is_one_document() {
        let (store, commands) = store_with_commands();
        let (store_id, node_id, other_id) = (StoreId::new(), NodeId::new(), NodeId::new());

        // A note nothing was ever written to: the first pane hosts it, the
        // second joins the first rather than hosting an empty one of its own.
        start_editing(store, P[0], store_id, node_id, &[]);
        start_editing(store, P[1], store_id, node_id, &[]);
        start_editing(store, P[2], store_id, other_id, &[]);
        assert_eq!(store.panes_editing(store_id, node_id), vec![P[0], P[1]]);
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert_eq!(subscribes(&sent, node_id), 1, "once per node, however many panes hold it");
        assert_eq!(subscribes(&sent, other_id), 1);

        type_at_end(P[0], "typed in the first");
        assert_eq!(doc_text(&editor(P[1])), "typed in the first", "the window hands the delta across");
        assert_eq!(doc_text(&editor(P[2])), "", "a pane on another note hears nothing");
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        let broadcasts = sent.iter().filter(|c| matches!(c, BackendCommand::BroadcastChanges { node_id: n, .. } if *n == node_id)).count();
        assert_eq!(broadcasts, 1, "the server is sent the delta once, and the second pane does not send it again");

        type_at_end(P[1], ", and in the second");
        assert_eq!(doc_text(&editor(P[0])), "typed in the first, and in the second");
        assert_eq!(doc_text(&editor(P[0])), doc_text(&editor(P[1])));

        // A peer: joins from the session's snapshot, types, and its delta
        // arrives as `RemoteChanges` would bring it.
        let peer = create_editor();
        let outbox = std::rc::Rc::new(RefCell::new(Vec::<Vec<u8>>::new()));
        let out = outbox.clone();
        let snapshot = editor(P[0]).collab_snapshot().unwrap();
        peer.start_collaboration_guest(&snapshot, move |delta| out.borrow_mut().push(delta)).unwrap();
        peer.set_selection(Selection::cursor(Pos(1)));
        assert!(peer.insert_text("Peer: "));
        for delta in outbox.borrow_mut().drain(..) {
            apply_remote(store, store_id, node_id, &delta);
        }
        assert_eq!(doc_text(&editor(P[0])), "Peer: typed in the first, and in the second");
        assert_eq!(doc_text(&editor(P[1])), doc_text(&editor(P[0])));
        assert_eq!(doc_text(&editor(P[2])), "", "and not the pane on another note");
        let _ = commands.try_iter().count();

        // Letting go: the subscription lasts as long as one pane holds it.
        stop_editing(store, P[0]);
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert_eq!(unsubscribes(&sent, node_id), 0, "the second pane still holds it");
        type_at_end(P[1], "!");
        assert!(doc_text(&editor(P[1])).ends_with("second!"), "the pane left keeps working");
        stop_editing(store, P[1]);
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert_eq!(unsubscribes(&sent, node_id), 1, "the last pane lets go of it");
        assert!(store.edited_nodes() == vec![ActiveEdit { store_id, node_id: other_id }]);
        stop_editing(store, P[2]);
    }

    /// A document this device may only read is locked in every pane that
    /// holds it, and a pane beside it on a document it may edit is not.
    #[test]
    fn a_read_only_switch_is_per_pane() {
        set_read_only(P[3], true);
        assert!(editor(P[3]).is_read_only());
        assert!(!editor(P[2]).is_read_only());
        set_read_only(P[3], false);
        assert!(!editor(P[3]).is_read_only());
    }
}
