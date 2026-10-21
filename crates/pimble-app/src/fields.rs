//! A node's own fields (docs/SHEETS_CONTRACT.md section 8, decision 1): named
//! values on the node (a project's rate, a person's phone), which formulas
//! read as `'Note'!rate` and `[@project].rate`. Shown as a band under the
//! pane's title strip while the node has any, or while one is being added
//! (Edit > "Add Field"), on the desktop and in the browser from this module.
//!
//! Each field is one key of the node's `node` root (`pimble_crdt::FIELD_PREFIX`),
//! so a write is a tiny update made on a blank document
//! (`NodeDoc::field_update`), sent through `applyEdit` like any edit and
//! handed to this window's own copies of the node: its editors, its sheet
//! sessions and its node cache.

use pimble_core::{NodeId, StoreId};
use pimble_crdt::NodeDoc;
use rinch::prelude::*;

use crate::panes::PaneId;
use crate::protocol::BackendCommand;
use crate::state::AppStore;

/// The node pane `pane` holds (a note in its editor, or a sheet node).
fn held(store: AppStore, pane: PaneId) -> Option<(StoreId, NodeId)> {
    let state = store.pane(pane);
    state.active_edit.get().or_else(|| state.sheet.get()).map(|e| (e.store_id, e.node_id))
}

/// The fields of node `node`, as this window has them, tracked.
fn fields_of(store: AppStore, node: Option<(StoreId, NodeId)>) -> Vec<(String, String)> {
    let Some((s, n)) = node else { return Vec::new() };
    store.get_node_signal(s, n).map(|sig| sig.with(|node| node.metadata.fields.clone())).unwrap_or_default()
}

/// Set field `name` of node `(store_id, node_id)` to `value` (`None` removes
/// it): refused with the sentence on a node this device may only read.
pub(crate) fn write(store: AppStore, store_id: StoreId, node_id: NodeId, name: &str, value: Option<&str>) {
    if !store.node_access(store_id, node_id).allows_write() {
        crate::events::show_notice(store, store.node_access(store_id, node_id).refusal().to_string());
        return;
    }
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    // The same field in another case is that field.
    let existing = untracked(|| fields_of(store, Some((store_id, node_id))));
    let name = existing.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(n, _)| n.clone()).unwrap_or_else(|| name.to_string());
    // Made on the node's document as this window has it, so it replaces the
    // value it was read from: one made on a blank document would merge as
    // concurrent with it, and either could win.
    let bytes = match current_doc(store, store_id, node_id) {
        Some(mut doc) => {
            let before = doc.state_vector();
            let now = chrono::Utc::now().to_rfc3339();
            if doc.set_field(&name, value, &now).is_err() {
                return;
            }
            match doc.diff_since(&before) {
                Ok(bytes) => bytes,
                Err(_) => return,
            }
        }
        // A node this window has no copy of: a new field is all it can be.
        None => NodeDoc::field_update(&name, value),
    };
    use base64::Engine;
    store.send(BackendCommand::BroadcastChanges { store_id, node_id, changes: base64::engine::general_purpose::STANDARD.encode(&bytes) });
    // This window's copies: the cache (what the band and formulas read),
    // the editors and the sheets holding the node.
    if let Some(sig) = store.get_node_signal(store_id, node_id) {
        sig.update(|node| {
            // The cached document takes the update too: the next write is
            // made on it.
            if !node.content.is_empty() {
                if let Ok(mut doc) = NodeDoc::load(&node.content) {
                    if doc.apply_update(&bytes).is_ok() {
                        node.content = doc.save();
                    }
                }
            }
            node.metadata.fields.retain(|(n, _)| *n != name);
            if let Some(v) = value {
                node.metadata.fields.push((name.clone(), v.to_string()));
                node.metadata.fields.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
            }
        });
    }
    crate::editor::apply_remote(store, store_id, node_id, &bytes);
    crate::sheets::apply_remote(store, store_id, node_id, &bytes);
    crate::sheets::node_loaded(store);
}

/// Node `(store_id, node_id)`'s document as this window has it: a sheet's
/// copy, an editor's, or the node cache's, each with what the others know.
fn current_doc(store: AppStore, store_id: StoreId, node_id: NodeId) -> Option<NodeDoc> {
    let mut copies: Vec<Vec<u8>> = Vec::new();
    copies.extend(crate::sheets::held_copy(store_id, node_id));
    for pane in store.panes_editing(store_id, node_id) {
        copies.extend(crate::editor::editor(pane).collab_snapshot());
    }
    if let Some(sig) = store.get_node_signal(store_id, node_id) {
        let cached = untracked(|| sig.with(|n| n.content.clone()));
        if !cached.is_empty() {
            copies.push(cached);
        }
    }
    let mut doc: Option<NodeDoc> = None;
    for bytes in copies {
        match &mut doc {
            None => doc = NodeDoc::load(&bytes).ok(),
            Some(d) => {
                let _ = d.apply_update(&bytes);
            }
        }
    }
    // Only a document that holds the node's metadata can be written on.
    doc.filter(|d| d.fields().is_ok())
}

/// Edit > "Add Field": the focused pane's node shows its fields with an empty
/// one to fill in.
pub fn add_field(store: AppStore) {
    let pane = store.focused();
    let Some((s, n)) = untracked(|| held(store, pane)) else {
        crate::events::show_notice(store, "Open a note to give it a field.".to_string());
        return;
    };
    if !store.node_access(s, n).allows_write() {
        crate::events::show_notice(store, store.node_access(s, n).refusal().to_string());
        return;
    }
    store.pane(pane).adding_field.set(true);
}

/// The band of pane `pane`'s node's fields.
pub(crate) fn render_fields(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let state = store.pane(pane);
    let new_name = Signal::new(String::new());
    let new_value = Signal::new(String::new());
    let shown = move || {
        (state.show_editor.get() || state.show_sheet.get()) && !state.show_board.get() && (state.adding_field.get() || !fields_of(store, held(store, pane)).is_empty())
    };
    let add = move || {
        let Some((s, n)) = untracked(|| held(store, pane)) else { return };
        let (name, value) = untracked(|| (new_name.get(), new_value.get()));
        if name.trim().is_empty() {
            return;
        }
        write(store, s, n, &name, Some(value.trim()));
        new_name.set(String::new());
        new_value.set(String::new());
        state.adding_field.set(false);
    };
    rsx! {
        div {
            class: "pimble-fields",
            style: {move || if shown() { "" } else { "display: none;" }},
            // Its inputs keep the keyboard a press gives them (the pane
            // would hand it to the editor).
            onmousedown: move || crate::pane_view::press_in_view(),
            for (name, value) in fields_of(store, held(store, pane)) {
                div {
                    key: format!("{name}={value}"),
                    class: "pimble-fields__item",
                    span { class: "pimble-fields__name", {name.clone()} }
                    {field_value(__scope, store, pane, name.clone(), value.clone())}
                    button {
                        class: "pimble-fields__remove",
                        title: "Remove this field",
                        onclick: {
                            let name = name.clone();
                            move || {
                                if let Some((s, n)) = untracked(|| held(store, pane)) {
                                    write(store, s, n, &name, None);
                                }
                            }
                        },
                        "×"
                    }
                }
            }
            if state.adding_field.get() {
                div {
                    class: "pimble-fields__item pimble-fields__item--new",
                    TextInput {
                        size: "xs",
                        placeholder: "name",
                        value_fn: move || new_name.get(),
                        oninput: move |v: String| new_name.set(v),
                        onsubmit: add,
                    }
                    TextInput {
                        size: "xs",
                        placeholder: "value",
                        value_fn: move || new_value.get(),
                        oninput: move |v: String| new_value.set(v),
                        onsubmit: add,
                    }
                    button { class: "pimble-sheet__adder", onclick: add, "Add" }
                    button { class: "pimble-sheet__adder", onclick: move || state.adding_field.set(false), "Cancel" }
                }
            } else {
                button {
                    class: "pimble-fields__add",
                    title: "Add a field to this node",
                    onclick: move || state.adding_field.set(true),
                    "+ Field"
                }
            }
        }
    }
}

/// One field's value, edited in place: Enter writes it.
fn field_value(__scope: &mut RenderScope, store: AppStore, pane: PaneId, name: String, value: String) -> NodeHandle {
    let text = Signal::new(value);
    let commit = move || {
        if let Some((s, n)) = untracked(|| held(store, pane)) {
            write(store, s, n, &name, Some(untracked(|| text.get()).trim()));
        }
    };
    rsx! {
        TextInput {
            size: "xs",
            value_fn: move || text.get(),
            oninput: move |v: String| text.set(v),
            onsubmit: commit,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{BackendEvent, BackendHandle};
    use crossbeam_channel::bounded;

    /// A field written twice from this window, over a value a peer wrote: each
    /// write replaces what it was read from (made on the node's document as
    /// this window has it, never on a blank one, which would merge as
    /// concurrent and could lose).
    #[test]
    fn a_field_write_replaces_the_value_it_read() {
        let store = AppStore::new();
        let (cmd_tx, commands) = bounded::<BackendCommand>(64);
        let (_event_tx, event_rx) = bounded::<BackendEvent>(1);
        store.backend.set(Some(BackendHandle { cmd_tx, event_rx }));
        let store_id = StoreId::new();
        // The server's document: a peer set rate to 25.
        let mut server = NodeDoc::new();
        server.init("document", "Packing", None, "2026-10-20T00:00:00Z").unwrap();
        server.set_field("rate", Some("25"), "2026-10-20T00:00:01Z").unwrap();
        let mut node = server.fields().unwrap().into_node(NodeId::new());
        node.content = server.save();
        let node_id = node.id;
        store.upsert_node(store_id, node);

        for value in ["30", "35"] {
            write(store, store_id, node_id, "rate", Some(value));
            for c in commands.try_iter() {
                if let BackendCommand::BroadcastChanges { node_id: n, changes, .. } = c {
                    assert_eq!(n, node_id);
                    use base64::Engine;
                    server.apply_update(&base64::engine::general_purpose::STANDARD.decode(changes).unwrap()).unwrap();
                }
            }
            assert_eq!(server.node_fields(), vec![("rate".to_string(), value.to_string())], "the server has {value}");
        }
    }
}
