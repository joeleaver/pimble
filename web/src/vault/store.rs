//! One encrypted store's documents: building them from fetches and merging what arrives.

use std::collections::HashMap;

use pimble_app::protocol::BackendEvent;
use pimble_core::{NodeId, Store, StoreId};
use pimble_crdt::{derive_changes, NodeDoc, NodeUpdateEffect, Tree};
use pimble_crypto::KeyId;
use pimble_rpc::vault::{Mark, PartBuffer};
use pimble_rpc::{StoreChangeKind, VaultFetchResponse, VaultPart};

use super::{heard_kind, KeyLook, Merged, Pulled, Side, StoreKeys, VaultDoc, VaultStore};
use super::helpers::{advance, load_answered, now_ms};
use super::shares::share_key_id;

// ── The store's documents, without the network ───────────────────────────────
//
// Everything below is decided from what this page holds and what it is handed,
// so the rules the vault client was debugged for (cursors, what is known to be
// on the server, what to resend, when a snapshot may be stamped) are testable
// on their own.

impl VaultStore {
    /// A store from what `vaultFetch` answered for each of its documents: take
    /// each document's wraps, decrypt its log, [`assemble`](Self::assemble).
    ///
    /// Also answers the documents with a blob nothing held opened, as they
    /// were fetched: a key that arrives a moment later (the key of a share in
    /// a whole store, see [`fetch_share_keys`]) opens them through
    /// [`VaultStore::take_fetched`] with no second round trip.
    pub(super) fn from_fetched(
        listed: Store,
        mut keys: StoreKeys,
        scope_roots: Vec<NodeId>,
        fetched: Vec<(NodeId, VaultFetchResponse)>,
    ) -> (Self, Vec<(NodeId, VaultFetchResponse)>) {
        let store_id = listed.id;
        let mut pulled = Vec::with_capacity(fetched.len());
        let mut shut = Vec::new();
        for (node_id, fetched) in fetched {
            keys.note_wraps(node_id, fetched.keys.clone());
            let entries = keys.decrypt_entries(store_id, node_id, &fetched);
            let head = fetched.head;
            if entries.iter().any(|(_, opened)| opened.is_err()) {
                shut.push((node_id, fetched));
            }
            pulled.push(Pulled { node_id, entries, head });
        }
        (Self::assemble(listed, keys, scope_roots, pulled), shut)
    }

    /// A store from what its logs held: every decrypted entry, per document
    /// in log order. Picks the root, builds the tree, records each
    /// document's cursor. Nothing is repaired here; the caller runs
    /// [`VaultStore::repair_now`] once the whole pull is in.
    pub(super) fn assemble(listed: Store, keys: StoreKeys, scope_roots: Vec<NodeId>, pulled: Vec<Pulled>) -> Self {
        let listed_id = listed.id;
        let mut node_docs: HashMap<NodeId, NodeDoc> = HashMap::new();
        let mut docs: HashMap<NodeId, VaultDoc> = HashMap::new();
        let mut heads = HashMap::new();
        let mut held_back = HashMap::new();
        for Pulled { node_id, entries, head } in pulled {
            let node_doc = node_docs.entry(node_id).or_default();
            let doc = docs.entry(node_id).or_default();
            for (mark, update) in entries {
                match update {
                    Ok(bytes) => match node_doc.apply_update(&bytes) {
                        Ok(_) => {
                            advance(&mut doc.known_sv, &bytes);
                            doc.applied(mark);
                        }
                        Err(e) => {
                            tracing::warn!("Skipping an unreadable update of {}: {}", node_id, e);
                            held_back.entry((node_id, Side::Read)).or_insert_with(|| format!("A change to it would not apply here ({e})."));
                        }
                    },
                    Err(reason) => {
                        tracing::warn!("Skipping a blob of {}: {}", node_id, reason);
                        held_back.entry((node_id, Side::Read)).or_insert(reason);
                    }
                }
            }
            heads.insert(node_id, head);
        }
        // A scope's tree starts at its first shared root, whose own
        // `parent_id` names a document this page will never hold. Nothing may
        // look for another root: there is none to find, and the documents'
        // rule below would answer with whichever node happens to be missing
        // its parent.
        let start = scope_roots.first().copied().unwrap_or(listed.root_node_id);
        let mut tree = Tree::from_docs(start, node_docs);
        if scope_roots.is_empty() {
            let root = tree.document_root(listed.root_node_id).unwrap_or(listed.root_node_id);
            if root != tree.root() {
                tracing::info!("Store {}: manifest root {} replaced by the documents' root {}", listed.id, listed.root_node_id, root);
                tree.reroot(root);
            }
        }
        Self {
            listed,
            keys,
            scope_roots,
            tree,
            docs,
            heads,
            repair_due: None,
            look: KeyLook::default(),
            epoch: None,
            heard: Vec::new(),
            parts_ok: false,
            parts: PartBuffer::default(),
            held_changed: !held_back.is_empty(),
            held_back,
            reports_ok: false,
            report_due: HashMap::new(),
            reported: HashMap::new(),
            answered: load_answered(listed_id),
            reports_in: Vec::new(),
            reports_heard: HashMap::new(),
            people: HashMap::new(),
            people_at: None,
            steps: HashMap::new(),
            out_of_step: HashMap::new(),
            steps_judged_at: None,
            parked_since: HashMap::new(),
            set_aside: HashMap::new(),
        }
    }

    /// Hold `node_id` back one way, with the sentence that says why.
    pub(super) fn hold(&mut self, node_id: NodeId, side: Side, reason: impl Into<String>) {
        let reason = reason.into();
        if self.held_back.get(&(node_id, side)) != Some(&reason) {
            self.held_back.insert((node_id, side), reason);
            self.held_changed = true;
        }
    }

    pub(super) fn release(&mut self, node_id: NodeId, side: Side) {
        if self.held_back.remove(&(node_id, side)).is_some() {
            self.held_changed = true;
        }
    }

    /// One entry per document held back, the reason it cannot be sent first.
    pub(super) fn waiting_list(&self) -> Vec<pimble_core::WaitingDoc> {
        let mut out: Vec<pimble_core::WaitingDoc> = Vec::new();
        let mut held: Vec<(&(NodeId, Side), &String)> = self.held_back.iter().collect();
        held.sort_by_key(|((node_id, side), _)| (node_id.to_string(), *side));
        for ((node_id, _), reason) in held {
            if !out.iter().any(|w| w.node_id == *node_id) {
                out.push(pimble_core::WaitingDoc { node_id: *node_id, reason: reason.clone() });
            }
        }
        out
    }

    /// Whether this page holds only a scope of the store: a share's recipient.
    pub(super) fn is_partial(&self) -> bool {
        !self.scope_roots.is_empty()
    }

    pub(super) fn doc_entry(&mut self, node_id: NodeId) -> &mut VaultDoc {
        self.docs.entry(node_id).or_default()
    }

    /// Merge a peer's decrypted updates of one document, in log order, and
    /// say what the UI should hear: the kinds the server would derive from
    /// what the merge changed (`pimble_crdt::derive_changes`), a content
    /// change as `NodeContentUpdated`; and, `live` (an update that arrived as
    /// it was made, not a catch-up the views reconcile across), every update
    /// that changed something as `RemoteChanges`, which the app's views
    /// holding the node merge, as a plain store's subscription hands them
    /// over (`pimble_app::commands::change_events`). `source` is who wrote
    /// it, when the notification said.
    ///
    /// An entry that would not decrypt or merge is never marked, so the
    /// cursor stops in front of it and a later snapshot cannot vouch for it.
    pub(super) fn merge(
        &mut self,
        store_id: StoreId,
        node_id: NodeId,
        entries: Vec<(Mark, Result<Vec<u8>, String>)>,
        live: bool,
        source: Option<String>,
    ) -> Merged {
        let before = self.tree.shape(node_id);
        let marker_before = before.as_ref().and_then(share_key_id);
        let mut effect = NodeUpdateEffect::default();
        let mut content_updates = Vec::new();
        // Why the document waits after this, if it does (rule 3: nothing is
        // held back silently).
        let mut unread: Option<String> = None;
        let any = !entries.is_empty();
        for (mark, update) in entries {
            let bytes = match update {
                Ok(bytes) => bytes,
                Err(reason) => {
                    tracing::warn!("Skipping a blob of {}: {}", node_id, reason);
                    unread.get_or_insert(reason);
                    continue;
                }
            };
            match self.tree.apply_update(node_id, &bytes) {
                Ok(one) => {
                    let doc = self.doc_entry(node_id);
                    advance(&mut doc.known_sv, &bytes);
                    doc.applied(mark);
                    effect.changed |= one.changed;
                    effect.structure |= one.structure;
                    effect.content |= one.content;
                    effect.data |= one.data;
                    if one.changed {
                        content_updates.push(bytes);
                    }
                }
                Err(e) => {
                    tracing::warn!("An update of {} would not apply: {}", node_id, e);
                    unread.get_or_insert_with(|| format!("A change to it would not apply here ({e})."));
                }
            }
        }
        match unread {
            Some(reason) => self.hold(node_id, Side::Read, reason),
            None if any => self.release(node_id, Side::Read),
            None => {}
        }
        if !effect.changed {
            return Merged { events: Vec::new(), structure: false, wants_key: false };
        }
        self.note_report_due(node_id, now_ms());

        let after = self.tree.shape(node_id);
        // A share made, or made again under another key, while this page was
        // looking: a whole store's page is owed its key (see [`KeyLook`]).
        let marker_after = after.as_ref().and_then(share_key_id);
        let wants_key = !self.is_partial()
            && marker_after != marker_before
            && marker_after.is_some_and(|key_id| self.keys.scope.get(&key_id).is_none());
        // Whoever listens through the API hears each update with the document
        // it is about, and keeps its own copy current from it.
        let kind = heard_kind(node_id, effect);
        for update in &content_updates {
            self.heard.push(pimble_api::StoreChange { store: store_id, kind: kind.clone(), source: source.clone(), update: Some(update.clone()) });
        }
        let mut events = Vec::new();
        if live {
            for update in content_updates {
                events.push(BackendEvent::RemoteChanges { store_id, node_id, update });
            }
        }
        for change in derive_changes(node_id, self.tree.root(), before.as_ref(), after.as_ref(), effect) {
            match StoreChangeKind::from(change) {
                StoreChangeKind::ContentUpdated { .. } => {
                    events.push(BackendEvent::NodeContentUpdated { store_id, node_id });
                }
                change_kind => events.push(BackendEvent::RemoteStoreChange {
                    store_id,
                    change_kind,
                    source_client_id: source.clone(),
                }),
            }
        }
        Merged { events, structure: effect.structure, wants_key }
    }

    /// Take one document's `vaultFetch` answer: its wraps, then its log
    /// decrypted and merged, then its head. A marker it brought whose key
    /// this page lacks wants a look.
    pub(super) fn take_fetched(
        &mut self,
        store_id: StoreId,
        node_id: NodeId,
        fetched: &VaultFetchResponse,
        now: f64,
    ) -> Merged {
        self.keys.note_wraps(node_id, fetched.keys.clone());
        let entries = self.keys.decrypt_entries(store_id, node_id, fetched);
        let merged = self.merge(store_id, node_id, entries, false, None);
        self.heads.insert(node_id, fetched.head);
        if merged.wants_key {
            self.look.want(now);
        }
        merged
    }

    /// Take one blob from the live path: decrypt, merge, note the head.
    ///
    /// An unknown key id is not fatal: a rotation this device has not been
    /// granted yet, a document whose wraps have not been fetched, or a
    /// member's document under a share whose key is not here yet looks
    /// exactly like this. The cursor stops in front of the entry, so a later
    /// snapshot cannot vouch for it, and a look is wanted for what would open
    /// it (see [`KeyLook`]).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn take_blob(
        &mut self,
        store_id: StoreId,
        node_id: NodeId,
        seq: u64,
        blob: &str,
        source: Option<String>,
        now: f64,
    ) -> Merged {
        let update = self.keys.decrypt_blob(store_id, node_id, blob);
        let unopened = match &update {
            Ok(_) => None,
            Err(_) => self.keys.key_wanted(store_id, node_id, blob),
        };
        let merged = self.merge(store_id, node_id, vec![(Mark::One(seq), update)], true, source);
        let head = self.heads.entry(node_id).or_insert(0);
        *head = (*head).max(seq);
        if merged.wants_key {
            self.look.want(now);
        }
        if let Some(key_id) = unopened {
            self.note_unopened(node_id, key_id, now);
        }
        merged
    }

    /// Take a group of parts from the live path, once every part of it is
    /// here: decrypt them as one, merge, note the head.
    pub(super) fn take_parts(&mut self, store_id: StoreId, node_id: NodeId, mark: Mark, parts: &[(VaultPart, String)], source: Option<String>, now: f64) -> Merged {
        let update = self.keys.decrypt_parts(store_id, node_id, parts);
        let unopened = match &update {
            Ok(_) => None,
            Err(_) => parts.first().and_then(|(_, blob)| self.keys.key_wanted(store_id, node_id, blob)),
        };
        let merged = self.merge(store_id, node_id, vec![(mark, update)], true, source);
        if let Mark::Span(_, last) = mark {
            let head = self.heads.entry(node_id).or_insert(0);
            *head = (*head).max(last);
        }
        if merged.wants_key {
            self.look.want(now);
        }
        if let Some(key_id) = unopened {
            self.note_unopened(node_id, key_id, now);
        }
        merged
    }

    /// A blob of `node_id` named `key_id` and nothing here opened it.
    ///
    /// The first time, the document is worth reading again: a fetch answers
    /// with its wraps, which a notification never carries. After that only a
    /// scope key can help, which is worth a look while a share in this store
    /// still has none here; the floor on those asks is the look's.
    fn note_unopened(&mut self, node_id: NodeId, key_id: KeyId, now: f64) {
        if !self.look.asked.contains(&(node_id, key_id)) {
            self.look.unread.insert(node_id, key_id);
            self.look.want(now);
        } else if self.look.due.is_none() && !self.missing_share_roots().is_empty() {
            self.look.want(now);
        }
    }
}
