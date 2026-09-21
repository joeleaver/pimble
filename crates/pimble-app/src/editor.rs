//! Editor integration: the rinch `Editor {}` component + `EditorHandle`, with
//! M9 collaboration wired onto pimble's existing per-node change relay.
//!
//! Pimble uses ONE editor pane and swaps the active node's content into it, so we
//! keep a single thread-local [`EditorHandle`]. Collaboration maps cleanly onto
//! pimble's transport:
//!
//! * a local edit's delta → `outbound` → [`BackendCommand::BroadcastChanges`] → the
//!   server applies it (`apply_edit` → `ContentDoc::apply_update`) and relays it to peers;
//! * a peer's delta arrives as `BackendEvent::RemoteChanges` →
//!   [`apply_remote`] → [`EditorHandle::collab_receive`].
//!
//! The node's stored content IS the collab session snapshot (a yrs v1 update), so the
//! server's per-node `ContentDoc` shares the editor's CRDT lineage and incremental
//! deltas apply cleanly.

use std::cell::RefCell;

use pimble_core::{NodeId, StoreId};
use rinch::prelude::*;
use rinch_editor_core::Node as EditorNode;

use crate::protocol::BackendCommand;
use crate::rinch_editor::{create_editor, EditorHandle};
use crate::state::{label_from_title_and_content, ActiveEdit, AppStore};

thread_local! {
    /// The single content-pane editor handle, created lazily on first use.
    static EDITOR: RefCell<Option<EditorHandle>> = const { RefCell::new(None) };
    /// The pending debounced label refresh, if any (see `schedule_label_refresh`).
    /// A single slot, not a per-node map: only one node is ever under local
    /// edit at a time (one shared editor pane), so scheduling for a new node
    /// always supersedes whatever was pending.
    static LABEL_REFRESH: RefCell<Option<TimeoutHandle>> = const { RefCell::new(None) };
    /// The pending reopen after typing into a document this device may only
    /// read (see [`reject_local_edit`]).
    static READ_ONLY_REOPEN: RefCell<Option<TimeoutHandle>> = const { RefCell::new(None) };
}

/// The app's editor handle (created on first use). Cheap to clone (an `Rc`); the
/// toolbar, the `Editor {}` component, and the event loop all share this one handle.
pub(crate) fn editor() -> EditorHandle {
    EDITOR.with(|e| {
        e.borrow_mut()
            .get_or_insert_with(|| {
                let handle = create_editor();
                // Typing, commands and remote deltas all land here; the
                // toolbar's active states follow immediately (cursor-only
                // moves are covered by the toolbar's own watcher).
                handle.on_change(crate::toolbar::bump_toolbar);
                handle
            })
            .clone()
    })
}

/// Begin editing `node_id`: load its content into the shared editor and start a
/// collaboration session wired to the server relay. `content_bytes` is the node's
/// stored content from the server — a yrs collab snapshot, or empty for a new node.
pub(crate) fn start_editing(
    store: AppStore,
    store_id: StoreId,
    node_id: NodeId,
    content_bytes: &[u8],
) {
    let handle = editor();
    // The editor's built-in stylesheet is light unless the container carries
    // `data-pm-theme="dark"`; `set_dark_mode` is a no-op before the view
    // mounts, so apply the app's scheme here, once a document is opened.
    handle.set_dark_mode(untracked(|| store.dark_mode.get()));
    handle.stop_collaboration(); // end any prior node's session
    cancel_pending_label_refresh();
    cancel_pending_read_only_reopen();

    // Every local edit's delta is base64-broadcast to the server, which persists it
    // and relays it to the other clients. The closure captures only `Copy` values
    // (AppStore/StoreId/NodeId), so it is itself `Copy` and can be reused below.
    let outbound = move |delta: Vec<u8>| {
        use base64::Engine;
        // A store shared read-only takes remote changes but sends none back.
        // `store_id` is the node's CANONICAL store (`open_node` parses the
        // tree value, which strips any mount path), so a node reached through
        // a mount is judged by the store the edit would be written to.
        if !store.store_access(store_id).allows_write() {
            reject_local_edit(store, store_id, node_id);
            return;
        }
        let changes = base64::engine::general_purpose::STANDARD.encode(&delta);
        store.send(BackendCommand::BroadcastChanges {
            store_id,
            node_id,
            changes,
        });
        // The tree's label Effect only reads `node_data`/`live_label`, neither
        // of which reflects this edit on its own — refresh our own label so
        // this window doesn't wait on a GetNode round trip to stop showing a
        // stale one (the other clients already re-fetch on `NodeContentUpdated`).
        schedule_label_refresh(store, store_id, node_id);
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
                // Seeding the node's document is a content write; a store
                // this device may only read is left exactly as it is.
                if store.store_access(store_id).allows_write() {
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

    store.active_edit.set(Some(ActiveEdit { store_id, node_id }));
    store.editor_dirty.set(false);
    // See edits from other clients.
    store.send(BackendCommand::SubscribeNodeChanges { store_id, node_id });
    // Offline-first open: the session started from the cached bytes above
    // (instant, no flash); now reconcile with the server's copy, which may
    // hold edits this cache never saw (this window's own earlier session,
    // another window, a replica). The answer lands in `apply_reconcile`.
    request_reconcile(store, store_id, node_id);

    // A different document is under the toolbar now.
    crate::toolbar::bump_toolbar();
}

/// Stop editing the current node (detach the collab session).
pub(crate) fn stop_editing(store: AppStore) {
    let handle = editor();
    // The node cache holds the content bytes from the last fetch; the
    // session's edits never touched them. Write the session's final state
    // back so labels and anything else reading the cache see what was
    // typed. (Opening a document still fetches from the server, which may
    // hold remote edits this session never saw.)
    if let Some(active) = untracked(|| store.active_edit.get()) {
        if let Some(snapshot) = handle.collab_snapshot() {
            if let Some(sig) = store.get_node_signal(active.store_id, active.node_id) {
                sig.update(|node| node.content = snapshot);
            }
        }
    }
    handle.stop_collaboration();
    store.active_edit.set(None);
    cancel_pending_label_refresh();
    cancel_pending_read_only_reopen();
    crate::toolbar::bump_toolbar();
}

/// Cancel a pending read-only reopen, if one is scheduled.
fn cancel_pending_read_only_reopen() {
    READ_ONLY_REOPEN.with(|slot| {
        if let Some(handle) = slot.borrow_mut().take() {
            clear_timeout(handle);
        }
    });
}

/// What happens when someone types into a document their device may only read
/// (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles"): the delta is not sent,
/// and the node is reopened from the server's copy so the typed text does not
/// linger. Debounced, so a burst of keystrokes costs one reopen rather than
/// one per character.
///
/// This is the interim. rinch's editor has no read-only switch yet; when it
/// grows one, this function and the two lines that call it are the whole of
/// what goes away — nothing else in the collaboration path knows about it.
fn reject_local_edit(store: AppStore, store_id: StoreId, node_id: NodeId) {
    cancel_pending_read_only_reopen();
    let timeout = set_timeout(400, move || {
        READ_ONLY_REOPEN.with(|slot| {
            slot.borrow_mut().take();
        });
        // Drop the session without `stop_editing`'s write-back: that snapshot
        // is exactly the typed text this is undoing. Then open the node the
        // ordinary way — `GetNode` answers with the server's copy and the
        // `NodeLoaded` handler starts a fresh session from it, which is the
        // one path a document is ever opened through.
        editor().stop_collaboration();
        store.active_edit.set(None);
        store.live_label.update(|m| {
            m.remove(&(store_id, node_id));
        });
        store.send(BackendCommand::GetNode { store_id, node_id });
    });
    READ_ONLY_REOPEN.with(|slot| {
        *slot.borrow_mut() = Some(timeout);
    });
}

/// Ask the server for what it has beyond the active session's state vector
/// (`syncNodeContent`). Used when a document opens and when the connection
/// comes back after a failover.
pub(crate) fn request_reconcile(store: AppStore, store_id: StoreId, node_id: NodeId) {
    if let Some(state_vector) = editor().collab_state_vector() {
        store.send(BackendCommand::ReconcileNodeContent { store_id, node_id, state_vector });
    }
}

/// Merge the server's answer to `request_reconcile` into the active session,
/// then send the server whatever the session has that it lacks. Ignored if
/// the session has moved to another node meanwhile.
pub(crate) fn apply_reconcile(
    store: AppStore,
    store_id: StoreId,
    node_id: NodeId,
    diff: &[u8],
    server_state_vector: &[u8],
) {
    let active = untracked(|| store.active_edit.get());
    if !active.map_or(false, |e| e.store_id == store_id && e.node_id == node_id) {
        return;
    }
    let handle = editor();
    if !diff.is_empty() {
        // `collab_receive` re-projects the view and does not re-broadcast.
        handle.collab_receive(diff);
        schedule_label_refresh(store, store_id, node_id);
        crate::toolbar::bump_toolbar();
    }
    // A store shared read-only sends nothing back, here as in `outbound`: the
    // session's own state is not the server's to take
    // (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles"). Without this the
    // reconcile that follows every open pushes the freshly hosted empty
    // document, and the server refuses it out loud.
    if !store.store_access(store_id).allows_write() {
        return;
    }
    if let Some(ours) = handle.collab_sync_diff(server_state_vector) {
        if !ours.is_empty() {
            use base64::Engine;
            let changes = base64::engine::general_purpose::STANDARD.encode(&ours);
            store.send(BackendCommand::BroadcastChanges { store_id, node_id, changes });
        }
    }
}

/// Cancel any debounced label refresh left over from a prior editing session.
fn cancel_pending_label_refresh() {
    LABEL_REFRESH.with(|slot| {
        if let Some(handle) = slot.borrow_mut().take() {
            clear_timeout(handle);
        }
    });
}

/// Schedule a debounced refresh (~300ms after the last call) of `node_id`'s tree
/// label, computed straight from the editor's in-memory document — no CRDT
/// decode/projection through `ContentDoc`, since the editor already has the
/// model. Called from the collaboration `outbound` closure after each local edit.
fn schedule_label_refresh(store: AppStore, store_id: StoreId, node_id: NodeId) {
    cancel_pending_label_refresh();
    let timeout = set_timeout(300, move || {
        LABEL_REFRESH.with(|slot| { slot.borrow_mut().take(); });
        if let Some(label) = compute_live_label(store, store_id, node_id, &editor()) {
            store.live_label.update(|m| { m.insert((store_id, node_id), label); });
        }
    });
    LABEL_REFRESH.with(|slot| { *slot.borrow_mut() = Some(timeout); });
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

/// Apply a peer's remote delta (base64-decoded `bytes`) to the editor — the
/// `BackendEvent::RemoteChanges` path. Integrates and re-projects without
/// re-broadcasting.
pub(crate) fn apply_remote(bytes: &[u8]) {
    editor().collab_receive(bytes);
}
