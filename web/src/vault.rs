//! The vault client: an encrypted store, driven from the browser.
//!
//! A vault store holds nothing the server can read. It keeps an append-only log
//! of opaque blobs per document, and everything that makes those blobs a tree of
//! notes happens here. Every node is one yrs document in this page's memory
//! (`pimble_crdt::NodeDoc`: its text, its place in the tree and its metadata in
//! one, docs/NODE_DOCUMENT_CONTRACT.md section 1), built by decrypting what
//! its log holds; the tree is `pimble_crdt::Tree` over those documents
//! (section 2), starting from the root the store's manifest names; and every
//! local change is encrypted before it is appended (docs/CRYPTO_CONTRACT.md,
//! "Web app"). There is no tree document any more. A hosted twin from before
//! may still list one, which nobody reads.
//!
//! It sits in front of `pimble_app::commands::process_command`. A command for a
//! vault store is answered from these documents and never reaches the plain
//! RPCs — which the server would refuse anyway (`-32005 encrypted_store`). A
//! command for a plain store is handed straight back, so today's path is
//! untouched.
//!
//! What the app never learns is that any of this happened: it sends the same
//! [`BackendCommand`]s and receives the same [`BackendEvent`]s, including the
//! collaboration pair (`BroadcastChanges` out, `RemoteChanges` in) that the
//! editor is wired to, and the same store-change kinds the server derives
//! from a merged update (see [`derive_kinds`]).
//!
//! One way to write node content, and one way to write a node's place: a
//! document changes only through `NodeDoc::apply_update` (a peer's update, or
//! the editor's own delta) and through the `Tree` operations, whose
//! `TreeEdit` says exactly what to append.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use base64::engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine as _;
use crossbeam_channel::{unbounded, Receiver, Sender};
use pimble_app::protocol::{BackendCommand, BackendEvent};
use pimble_client::PimbleClient;
use pimble_core::{custom_keys, node_types, Node, NodeId, NodeMetadata, Store, StoreId, StoreKind};
use pimble_crdt::{NodeDoc, NodeFields, NodeUpdateEffect, Tree, TreeEdit};
use pimble_crypto::{blob_aad, Blob};
use pimble_rpc::{
    SearchResultItem, StoreChangeKind, StoreChangedNotification, VaultCursor, VaultDocId,
    VaultFetchResponse,
};

use crate::keys::{self, Keyring};

/// How many appends to a document before its snapshot is refreshed. The
/// contract's number: a reader then replays at most this many updates.
const SNAPSHOT_EVERY: u32 = 200;

/// How long a merged structural update waits for the next one before the tree
/// is repaired, the server's number (`REPAIR_DEBOUNCE` in
/// `crates/pimble-server/src/handler.rs`). One peer's tree edit is several
/// documents' updates and they arrive one at a time; a repair run between two
/// of them judges a half-applied edit, and its answer to a half-applied move
/// is a new `parent_id` written into the node's document, which then
/// propagates. So no repair runs per update; it runs once they have stopped.
const REPAIR_DEBOUNCE_MS: f64 = 250.0;

/// How many documents' logs are fetched at once. One socket carries them all
/// and jsonrpsee caps the requests in flight on it, so a store of hundreds of
/// nodes is pulled in windows rather than one request per round trip.
const FETCH_WINDOW: usize = 32;

/// What [`VaultClient::handle`] decided about a command.
pub enum Handled {
    /// Not for a vault store. The command comes back untouched.
    No(BackendCommand),
    /// Answered here. The event, if there is one, is the command's reply.
    Yes(Option<BackendEvent>),
}

/// What this page knows about one node document's log. The document itself
/// lives in the store's `Tree`.
struct VaultDoc {
    /// How far into the log the document has been read without a gap: where
    /// the next fetch starts, and the only number a snapshot may be stamped
    /// with (see `pimble_rpc::VaultCursor`).
    cursor: VaultCursor,
    /// Appends since the last snapshot upload.
    appends: u32,
    /// A state vector everything up to which the server is known to hold
    /// (see `advance`). What a resend after a failed append is a diff against.
    known_sv: Vec<u8>,
    /// An append failed: the document holds work the server does not. Every
    /// later edit depends on it, so until it is resent no other device can
    /// show anything this one wrote.
    unsent: bool,
}

impl Default for VaultDoc {
    fn default() -> Self {
        Self {
            cursor: VaultCursor::default(),
            appends: 0,
            known_sv: pimble_crdt::empty_state_vector(),
            unsent: false,
        }
    }
}

/// One encrypted store.
struct VaultStore {
    /// The store as the server listed it: what the UI is told again when the
    /// documents turn out to name another root than the manifest does.
    listed: Store,
    keyring: Keyring,
    /// Every document this page holds, and the tree over them.
    tree: Tree,
    /// The log bookkeeping for each held document.
    docs: HashMap<NodeId, VaultDoc>,
    /// Each document's head as the server last reported it, so a document
    /// nothing was appended to is never fetched again.
    heads: HashMap<NodeId, u64>,
    /// When the debounced repair is due (the page's clock, ms), once a
    /// merged update has touched structure.
    repair_due: Option<f64>,
}

/// One document's log as fetched and decrypted, in log order.
struct Pulled {
    node_id: NodeId,
    entries: Vec<(Mark, Result<Vec<u8>, String>)>,
    head: u64,
}

/// One blob ready to append, and what to record once the server numbers it.
struct Outgoing {
    doc_id: VaultDocId,
    blob: String,
    /// The plaintext that went into the blob, to move `known_sv` by.
    payload: Vec<u8>,
    /// The document's state vector when the blob was made: what the server
    /// holds once the append succeeds, when this was a resend.
    sent_sv: Vec<u8>,
    /// Whether this carried everything since `known_sv` rather than one edit.
    resend: bool,
}

/// What merging a peer's updates into one document came to.
struct Merged {
    events: Vec<BackendEvent>,
    /// The `node` or `children` root changed: the tree may need a repair.
    structure: bool,
}

pub struct VaultClient {
    /// This client's id, as the server sees it, for recognising the
    /// notifications caused by this client's own appends.
    client_id: String,
    stores: HashMap<StoreId, VaultStore>,
    /// Store ids the accounts service calls vault stores, which is the
    /// authority when the RPC `Store` does not carry a kind yet.
    vault_ids: HashSet<StoreId>,
    /// The node the editor currently has open, learned from
    /// `SubscribeNodeChanges`. A decrypted content update is only turned into
    /// `RemoteChanges` for this node: the app has one editor pane and that
    /// event carries no node identity, so applying another node's update to it
    /// would corrupt what is on screen.
    active: Option<(StoreId, NodeId)>,
    /// The stores subscribed to on the current socket (see `subscribe`).
    subscribed: HashSet<StoreId>,
    notices_tx: Sender<StoreChangedNotification>,
    notices_rx: Receiver<StoreChangedNotification>,
}

impl VaultClient {
    pub fn new(client_id: String) -> Self {
        let (notices_tx, notices_rx) = unbounded();
        Self {
            client_id,
            stores: HashMap::new(),
            vault_ids: HashSet::new(),
            active: None,
            subscribed: HashSet::new(),
            notices_tx,
            notices_rx,
        }
    }

    /// Whether this store is one the vault client answers for.
    pub fn owns(&self, store_id: StoreId) -> bool {
        self.stores.contains_key(&store_id)
    }

    /// Whether a store's changes are this client's to subscribe to: an
    /// encrypted store, whether it is open here or only known to be encrypted
    /// from the account's store list.
    pub fn is_encrypted(&self, store_id: StoreId) -> bool {
        self.owns(store_id) || self.vault_ids.contains(&store_id)
    }

    /// Remember which stores the accounts service says are encrypted.
    ///
    /// Asked separately from the RPC store list because the two services are
    /// landing together: either source saying "vault" is enough.
    pub async fn learn_kinds(&mut self) {
        match crate::accounts::list_stores().await {
            Ok(list) => {
                for view in list {
                    if view.kind == "vault" {
                        if let Ok(id) = StoreId::parse(&view.store_id) {
                            self.vault_ids.insert(id);
                        }
                    }
                }
            }
            Err(e) => tracing::warn!("Could not read the account's store list: {}", e),
        }
    }

    /// Fill in what this page knows about a store before the UI sees it: the
    /// root its documents name.
    ///
    /// The root matters. A hosted store's manifest carries the root the
    /// server minted when the store was created, which is the real one only
    /// when this browser also made the tree. For a store hosted from a
    /// desktop the tree was made elsewhere and names a different root, and
    /// the hosted server, holding only ciphertext, never learns it. The
    /// documents are the authority and this is where they replace the
    /// placeholder. A store whose documents are not open here keeps the
    /// server's root: there is nothing better to say until they are.
    pub fn describe(&self, store: &mut Store) {
        if let Some(open) = self.stores.get(&store.id) {
            store.root_node_id = open.tree.root();
        }
    }

    /// [`describe`](Self::describe) over a whole list.
    pub fn describe_all(&self, stores: &mut [Store]) {
        for store in stores.iter_mut() {
            self.describe(store);
        }
    }

    /// Open every vault store in `stores` that is not open yet.
    ///
    /// Called before `StoresListed` reaches the UI, so the tree's first
    /// `GetChildren` already has documents to read.
    pub async fn open_listed(
        &mut self,
        client: &Arc<PimbleClient>,
        stores: &[Store],
        signal_ui: &Arc<dyn Fn() + Send + Sync>,
    ) -> Vec<String> {
        let mut problems = Vec::new();
        for store in stores {
            let is_vault = store.kind == StoreKind::Vault || self.vault_ids.contains(&store.id);
            if !is_vault || self.stores.contains_key(&store.id) {
                continue;
            }
            if let Err(message) = self.open_store(client, store, signal_ui).await {
                tracing::error!("Opening the encrypted store {}: {}", store.name, message);
                problems.push(format!("{}: {}", store.name, message));
            }
        }
        problems
    }

    /// Fetch this store's keys and every document's log, decrypt, build the
    /// tree, and hold it.
    ///
    /// Every document is pulled at open, not lazily: titles and children
    /// lists live in the documents, so nothing of the tree can be drawn until
    /// they are here. That costs one `vaultFetch` per node (in windows of
    /// [`FETCH_WINDOW`]); a store of thousands of nodes wants a fetch that
    /// answers for many documents at once, which the vault RPCs do not offer
    /// yet.
    ///
    /// The subscription is made first, so an append that lands during the
    /// pull is queued for `pump` rather than missed until the next connect;
    /// one the pull already applied merges again to nothing.
    async fn open_store(
        &mut self,
        client: &Arc<PimbleClient>,
        store: &Store,
        signal_ui: &Arc<dyn Fn() + Send + Sync>,
    ) -> Result<(), String> {
        let store_id = store.id;
        let keyring = keys::fetch_keyring(&store_id.to_string()).await?;

        if let Some(BackendEvent::Error { message }) = self.subscribe(client, store_id, signal_ui).await {
            return Err(message);
        }

        let listed = client
            .vault_list_docs(store_id)
            .await
            .map_err(|e| format!("listing the vault's documents failed: {e}"))?;
        let wanted: Vec<(NodeId, u64)> = listed
            .into_iter()
            .filter_map(|info| match info.doc_id {
                VaultDocId::Node(id) => (info.head > 0).then_some((id, 0)),
                VaultDocId::Tree => {
                    // A hosted twin from before this layout keeps its tree
                    // document, superseded by the node documents.
                    tracing::debug!("Store {} lists a tree document; skipping it", store_id);
                    None
                }
            })
            .collect();

        let mut pulled = Vec::with_capacity(wanted.len());
        for (node_id, fetched) in fetch_many(client, store_id, wanted).await {
            match fetched {
                Ok(fetched) => pulled.push(Pulled {
                    node_id,
                    entries: decrypt_entries(&keyring, store_id, &VaultDocId::Node(node_id), &fetched),
                    head: fetched.head,
                }),
                // Left unheld: the next catch-up sees it listed and fetches
                // it from the start.
                Err(message) => tracing::warn!("Fetching the document {} failed: {}", node_id, message),
            }
        }

        let mut vault_store = VaultStore::assemble(store.clone(), keyring, pulled);
        // Once, after the whole pull, never between the updates of one edit.
        let repair = vault_store.repair_now(&now_rfc3339());
        self.stores.insert(store_id, vault_store);
        if let Some(edit) = repair {
            if let Err(message) = self.append_edit(client, store_id, &edit).await {
                tracing::warn!("Appending the repair of {} failed: {}", store_id, message);
            }
        }
        Ok(())
    }

    // ── After a reconnect ───────────────────────────────────────────────────

    /// Bring every open store level with the server again, both ways.
    ///
    /// This client outlives its connection: the socket is replaced on every
    /// token refresh and after every drop, while the decrypted documents stay.
    /// Nothing else closes the gap in between. Appends the server took while
    /// the socket was down were never notified here, and an append of this
    /// client's that failed was, until 2026-09-17, simply lost, along with
    /// every later edit's chance of being shown anywhere else (each depends on
    /// the one before). Called on every connect, after the subscriptions are
    /// made again and before `open_listed`.
    pub async fn catch_up(&mut self, client: &Arc<PimbleClient>) -> Vec<BackendEvent> {
        let mut events = Vec::new();
        let store_ids: Vec<StoreId> = self.stores.keys().copied().collect();
        for store_id in store_ids {
            // Down: every document the server lists, from where this page
            // stopped reading it, or from the start for one it never held (a
            // node created elsewhere while the socket was down).
            let listed = match client.vault_list_docs(store_id).await {
                Ok(listed) => listed,
                Err(e) => {
                    tracing::warn!("Listing the documents of {} failed: {}", store_id, e);
                    continue;
                }
            };
            let wanted: Vec<(NodeId, u64)> = {
                let Some(store) = self.stores.get(&store_id) else { continue };
                listed
                    .into_iter()
                    .filter_map(|info| match info.doc_id {
                        VaultDocId::Node(id) => {
                            let from = store.docs.get(&id).map(|d| d.cursor.applied_through()).unwrap_or(0);
                            (info.head > from).then_some((id, from))
                        }
                        VaultDocId::Tree => None,
                    })
                    .collect()
            };
            let fetched = fetch_many(client, store_id, wanted).await;

            let mut structure = false;
            for (node_id, fetched) in fetched {
                let Ok(fetched) = fetched else { continue };
                let active = self.active == Some((store_id, node_id));
                let Some(store) = self.stores.get_mut(&store_id) else { continue };
                let entries = decrypt_entries(&store.keyring, store_id, &VaultDocId::Node(node_id), &fetched);
                let merged = store.merge(store_id, node_id, entries, active, None);
                store.heads.insert(node_id, fetched.head);
                structure |= merged.structure;
                events.extend(merged.events);
            }

            // A pull is applied whole, then judged once: as at open, and as
            // the desktop's vault link does after its reconnect pull.
            if structure {
                if let Some(store) = self.stores.get_mut(&store_id) {
                    store.repair_due = None;
                    events.extend(store.adopt_root().map(|store| BackendEvent::StoreOpened { store }));
                }
                events.extend(self.repair(client, store_id).await);
            }

            // Up: whatever an append failed to deliver.
            let unsent: Vec<NodeId> = self
                .stores
                .get(&store_id)
                .map(|s| s.docs.iter().filter(|(_, doc)| doc.unsent).map(|(id, _)| *id).collect())
                .unwrap_or_default();
            for node_id in unsent {
                if let Err(e) = self.resend(client, store_id, node_id).await {
                    tracing::warn!("Resending the document {} failed: {}", node_id, e);
                }
            }
        }
        events
    }

    // ── Live updates ────────────────────────────────────────────────────────

    /// Apply whatever the subscriptions have delivered since the last pass.
    ///
    /// Returns the events the UI should see. Called once per turn of the
    /// backend loop, which is what keeps `&mut self` out of the subscription
    /// task (it only forwards raw notifications down a channel).
    pub fn pump(&mut self) -> Vec<BackendEvent> {
        let mut events = Vec::new();
        while let Ok(notification) = self.notices_rx.try_recv() {
            let StoreChangeKind::VaultAppended { doc_id, seq } = &notification.change_kind else {
                // A vault store emits nothing else, but forward anything that
                // does arrive rather than swallowing it.
                events.push(BackendEvent::RemoteStoreChange {
                    store_id: notification.store_id,
                    change_kind: notification.change_kind.clone(),
                    source_client_id: notification.source_client_id.clone(),
                });
                continue;
            };
            let store_id = notification.store_id;
            let node_id = match doc_id {
                VaultDocId::Node(id) => *id,
                VaultDocId::Tree => {
                    tracing::debug!("An append to the tree document of {}; nothing reads it", store_id);
                    continue;
                }
            };
            let seq = *seq;
            let Some(blob) = notification.update.clone() else {
                tracing::warn!("A VaultAppended notification carried no blob; ignoring it");
                continue;
            };
            let source = notification.source_client_id.clone();
            events.extend(self.apply_notification(store_id, node_id, seq, &blob, source));
        }
        events
    }

    fn apply_notification(
        &mut self,
        store_id: StoreId,
        node_id: NodeId,
        seq: u64,
        blob: &str,
        source_client_id: Option<String>,
    ) -> Vec<BackendEvent> {
        let active = self.active == Some((store_id, node_id));
        let apply = should_apply(source_client_id.as_deref(), &self.client_id);
        let Some(store) = self.stores.get_mut(&store_id) else { return Vec::new() };

        // Sequence numbers say how far this client has read, never who wrote
        // what; `should_apply` is the whole of that decision. This client's own
        // append is already in its document, so its number counts as read.
        if !apply {
            if let Some(doc) = store.docs.get_mut(&node_id) {
                doc.cursor.mark(seq);
            }
            return Vec::new();
        }

        let doc_id = VaultDocId::Node(node_id);
        let update = decrypt_blob(&store.keyring, store_id, &doc_id, blob);
        // An unknown key id is not fatal: a rotation this device has not been
        // granted yet looks exactly like this. The cursor stops in front of
        // the entry, so a later snapshot cannot vouch for it.
        let merged = store.merge(store_id, node_id, vec![(Mark::One(seq), update)], active, source_client_id);
        let head = store.heads.entry(node_id).or_insert(0);
        *head = (*head).max(seq);

        let mut events = merged.events;
        if merged.structure {
            store.repair_due = Some(now_ms() + REPAIR_DEBOUNCE_MS);
            events.extend(store.adopt_root().map(|store| BackendEvent::StoreOpened { store }));
        }
        events
    }

    /// The stores whose debounced repair has come due.
    pub fn repairs_due(&self) -> Vec<StoreId> {
        let now = now_ms();
        self.stores
            .iter()
            .filter(|(_, store)| store.repair_due.is_some_and(|due| due <= now))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Repair a store's tree if it needs it and append what the repair wrote,
    /// as any edit is appended. The UI hears `TreeStructure` for the
    /// documents it touched, as it would from the server.
    pub async fn repair(&mut self, client: &Arc<PimbleClient>, store_id: StoreId) -> Vec<BackendEvent> {
        let edit = {
            let Some(store) = self.stores.get_mut(&store_id) else { return Vec::new() };
            store.repair_due = None;
            store.repair_now(&now_rfc3339())
        };
        let Some(edit) = edit else { return Vec::new() };
        tracing::info!("Store {}: repaired the tree ({} documents)", store_id, edit.touched.len());
        if let Err(message) = self.append_edit(client, store_id, &edit).await {
            tracing::warn!("Appending the repair of {} failed: {}", store_id, message);
        }
        vec![BackendEvent::RemoteStoreChange {
            store_id,
            change_kind: StoreChangeKind::TreeStructure { node_ids: edit.node_ids() },
            source_client_id: None,
        }]
    }

    // ── Commands ────────────────────────────────────────────────────────────

    /// Answer `cmd` if it names a vault store; otherwise hand it back.
    pub async fn handle(
        &mut self,
        client: &Arc<PimbleClient>,
        cmd: BackendCommand,
        signal_ui: &Arc<dyn Fn() + Send + Sync>,
    ) -> Handled {
        // A search spans every store, so it is decided before the store id is.
        if let BackendCommand::Search { query, stores, limit } = &cmd {
            if stores.iter().any(|id| self.owns(*id)) {
                let event = self.search(client, query, stores, *limit).await;
                return Handled::Yes(Some(event));
            }
            return Handled::No(cmd);
        }

        let Some(store_id) = store_id_of(&cmd) else { return Handled::No(cmd) };
        if !self.owns(store_id) {
            return Handled::No(cmd);
        }

        Handled::Yes(match cmd {
            BackendCommand::GetChildren { store_id, node_id } => Some(self.get_children(store_id, node_id)),

            BackendCommand::GetNode { store_id, node_id } => Some(self.get_node(store_id, node_id)),

            BackendCommand::CreateNode { store_id, parent_id, title } => {
                Some(self.create_node(client, store_id, parent_id, title).await)
            }

            BackendCommand::RenameNode { store_id, node_id, title } => {
                Some(self.rename_node(client, store_id, node_id, title).await)
            }

            BackendCommand::SetNodeAppearance { store_id, node_id, icon, color, tags } => {
                Some(self.set_appearance(client, store_id, node_id, icon, color, tags).await)
            }

            BackendCommand::DeleteNode { store_id, node_id } => {
                Some(self.delete_node(client, store_id, node_id).await)
            }

            BackendCommand::MoveNode { store_id, node_id, new_parent_id, position } => {
                Some(self.move_node(client, store_id, node_id, new_parent_id, position).await)
            }

            BackendCommand::BroadcastChanges { store_id, node_id, changes } => {
                if let Err(message) = self.broadcast(client, store_id, node_id, &changes).await {
                    tracing::warn!("Appending an encrypted edit failed: {}", message);
                }
                None
            }

            BackendCommand::SetNodeContent { store_id, node_id, content } => {
                match self.append_update(client, store_id, node_id, &content).await {
                    Ok(()) => Some(BackendEvent::NodeContentUpdated { store_id, node_id }),
                    Err(message) => Some(BackendEvent::Error { message }),
                }
            }

            BackendCommand::ReconcileNodeContent { store_id, node_id, state_vector } => {
                Some(self.reconcile(store_id, node_id, &state_vector))
            }

            BackendCommand::SubscribeStoreChanges { store_id } => {
                self.subscribe(client, store_id, signal_ui).await
            }

            // One editor pane, one open node: this is how the vault client
            // learns which node a decrypted update belongs on screen. No
            // server subscription is needed — the store's already delivers
            // every blob.
            BackendCommand::SubscribeNodeChanges { store_id, node_id } => {
                self.active = Some((store_id, node_id));
                None
            }

            // A vault store has no sync link and no server-side index. Both are
            // answered here rather than refused, so the app's ordinary
            // registration path does not light up the status bar with errors.
            BackendCommand::GetStoreSync { store_id } => Some(BackendEvent::StoreSyncChanged {
                store_id,
                remote: None,
                state: pimble_core::SyncState::Offline,
                sync_mode: pimble_core::StoreKind::Plain,
            }),
            BackendCommand::RebuildIndex { store_id } => {
                Some(BackendEvent::IndexRebuilt { store_id, indexed: 0 })
            }

            other => Some(BackendEvent::Error {
                message: format!("{} is not available on an encrypted store", name_of(&other)),
            }),
        })
    }

    // ── Reads ───────────────────────────────────────────────────────────────

    /// Answered from the documents held: every child arrives with its
    /// document's bytes, because the app derives a document's tree label from
    /// its first line and opens it from the bytes it is handed here, with no
    /// second fetch in between.
    fn get_children(&self, store_id: StoreId, node_id: NodeId) -> BackendEvent {
        let Some(store) = self.stores.get(&store_id) else {
            return BackendEvent::Error { message: "no such encrypted store".into() };
        };
        let children = match store.children_of(node_id) {
            Ok(ids) => ids.iter().filter_map(|id| store.node_of(*id).ok()).collect(),
            Err(message) => return BackendEvent::Error { message },
        };
        BackendEvent::ChildrenLoaded { store_id, parent_id: node_id, children_store_id: store_id, children }
    }

    fn get_node(&self, store_id: StoreId, node_id: NodeId) -> BackendEvent {
        let Some(store) = self.stores.get(&store_id) else {
            return BackendEvent::Error { message: "no such encrypted store".into() };
        };
        match store.node_of(node_id) {
            Ok(node) => BackendEvent::NodeLoaded { store_id, node },
            Err(message) => BackendEvent::Error { message },
        }
    }

    // ── Tree writes ─────────────────────────────────────────────────────────

    async fn create_node(
        &mut self,
        client: &Arc<PimbleClient>,
        store_id: StoreId,
        parent_id: Option<NodeId>,
        title: String,
    ) -> BackendEvent {
        let node_id = NodeId::new();
        let now = now_rfc3339();
        let (parent, edit) = {
            let Some(store) = self.stores.get_mut(&store_id) else {
                return BackendEvent::Error { message: "no such encrypted store".into() };
            };
            let parent = parent_id.unwrap_or(store.tree.root());
            let mut edit = store.seed_root(&now);
            match store.tree.add_node(node_id, Some(parent), None, node_types::DOCUMENT, &title, &now) {
                Ok(more) => edit.touched.extend(more.touched),
                Err(e) => return BackendEvent::Error { message: e.to_string() },
            }
            (parent, edit)
        };
        match self.append_edit(client, store_id, &edit).await {
            Ok(()) => BackendEvent::NodeCreated { store_id, parent_id: Some(parent), node_id },
            Err(message) => BackendEvent::Error { message },
        }
    }

    async fn rename_node(
        &mut self,
        client: &Arc<PimbleClient>,
        store_id: StoreId,
        node_id: NodeId,
        title: String,
    ) -> BackendEvent {
        let now = now_rfc3339();
        let edit = {
            let Some(store) = self.stores.get_mut(&store_id) else {
                return BackendEvent::Error { message: "no such encrypted store".into() };
            };
            let mut edit = store.seed_root(&now);
            match rename_edit(&mut store.tree, node_id, &title, &now) {
                Ok(more) => edit.touched.extend(more.touched),
                Err(e) => return BackendEvent::Error { message: e.to_string() },
            }
            edit
        };
        match self.append_edit(client, store_id, &edit).await {
            Ok(()) => BackendEvent::NodeRenamed { store_id, node_id },
            Err(message) => BackendEvent::Error { message },
        }
    }

    async fn set_appearance(
        &mut self,
        client: &Arc<PimbleClient>,
        store_id: StoreId,
        node_id: NodeId,
        icon: Option<Option<String>>,
        color: Option<Option<String>>,
        tags: Option<Vec<String>>,
    ) -> BackendEvent {
        let now = now_rfc3339();
        let edit = {
            let Some(store) = self.stores.get_mut(&store_id) else {
                return BackendEvent::Error { message: "no such encrypted store".into() };
            };
            let mut edit = store.seed_root(&now);
            match appearance_edit(&mut store.tree, node_id, icon, color, tags, &now) {
                Ok(more) => edit.touched.extend(more.touched),
                Err(e) => return BackendEvent::Error { message: e.to_string() },
            }
            edit
        };
        match self.append_edit(client, store_id, &edit).await {
            Ok(()) => BackendEvent::NodeRenamed { store_id, node_id },
            Err(message) => BackendEvent::Error { message },
        }
    }

    /// The whole subtree becomes tombstones, as it does server-side; the
    /// documents stay, so an undelete elsewhere finds them.
    async fn delete_node(&mut self, client: &Arc<PimbleClient>, store_id: StoreId, node_id: NodeId) -> BackendEvent {
        let now = now_rfc3339();
        let (parent_id, edit) = {
            let Some(store) = self.stores.get_mut(&store_id) else {
                return BackendEvent::Error { message: "no such encrypted store".into() };
            };
            let parent_id = match store.tree.get_node_info(node_id) {
                Ok(info) => info.parent_id,
                Err(e) => return BackendEvent::Error { message: e.to_string() },
            };
            let Some(parent_id) = parent_id else {
                return BackendEvent::Error { message: "the root node cannot be deleted".into() };
            };
            match store.tree.remove_node(node_id, &now) {
                Ok(edit) => (parent_id, edit),
                Err(e) => return BackendEvent::Error { message: e.to_string() },
            }
        };
        match self.append_edit(client, store_id, &edit).await {
            Ok(()) => BackendEvent::NodeDeleted { store_id, node_id, parent_id },
            Err(message) => BackendEvent::Error { message },
        }
    }

    async fn move_node(
        &mut self,
        client: &Arc<PimbleClient>,
        store_id: StoreId,
        node_id: NodeId,
        new_parent_id: NodeId,
        position: Option<usize>,
    ) -> BackendEvent {
        let now = now_rfc3339();
        let (old_parent_id, edit) = {
            let Some(store) = self.stores.get_mut(&store_id) else {
                return BackendEvent::Error { message: "no such encrypted store".into() };
            };
            let old_parent_id = match store.tree.get_node_info(node_id) {
                Ok(info) => info.parent_id.unwrap_or(new_parent_id),
                Err(e) => return BackendEvent::Error { message: e.to_string() },
            };
            match store.tree.move_node(node_id, new_parent_id, position, &now) {
                Ok(edit) => (old_parent_id, edit),
                Err(e) => return BackendEvent::Error { message: e.to_string() },
            }
        };
        match self.append_edit(client, store_id, &edit).await {
            Ok(()) => BackendEvent::NodeMoved { store_id, node_id, old_parent_id, new_parent_id },
            Err(message) => BackendEvent::Error { message },
        }
    }

    /// Append what a tree operation wrote: one `vaultAppend` per touched
    /// document, in the order the edit names them (a new document before its
    /// parent's list, so a peer never lists a node it cannot read yet).
    ///
    /// Every document is attempted even after one fails, so each failure is
    /// recorded against its own document and resent from there; stopping at
    /// the first would leave the rest holding work nothing knows is unsent.
    async fn append_edit(&mut self, client: &Arc<PimbleClient>, store_id: StoreId, edit: &TreeEdit) -> Result<(), String> {
        let mut first_error = None;
        for (node_id, update) in &edit.touched {
            if let Err(message) = self.append_prepared(client, store_id, *node_id, update).await {
                tracing::warn!("Appending to the document {} failed: {}", node_id, message);
                first_error.get_or_insert(message);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    // ── Content ─────────────────────────────────────────────────────────────

    /// A local edit from the editor: merge it, encrypt it, append it.
    async fn broadcast(
        &mut self,
        client: &Arc<PimbleClient>,
        store_id: StoreId,
        node_id: NodeId,
        changes: &str,
    ) -> Result<(), String> {
        // The editor's outbound closure encodes with the standard alphabet,
        // exactly as the plain path's `applyEdit` carries it.
        let update = STANDARD
            .decode(changes)
            .map_err(|e| format!("the editor's delta would not decode: {e}"))?;
        self.append_update(client, store_id, node_id, &update).await
    }

    /// Merge `update` into the node's document (the editor's delta, or a whole
    /// document from a session that started from nothing), then append it.
    async fn append_update(
        &mut self,
        client: &Arc<PimbleClient>,
        store_id: StoreId,
        node_id: NodeId,
        update: &[u8],
    ) -> Result<(), String> {
        {
            let Some(store) = self.stores.get_mut(&store_id) else {
                return Err("no such encrypted store".to_string());
            };
            store.tree.apply_update(node_id, update).map_err(|e| e.to_string())?;
        }
        self.append_prepared(client, store_id, node_id, update).await
    }

    /// Send everything `node_id`'s document holds that the server does not,
    /// for a document an earlier append failed to deliver.
    async fn resend(&mut self, client: &Arc<PimbleClient>, store_id: StoreId, node_id: NodeId) -> Result<(), String> {
        // An empty update: `prepare` sends the diff since `known_sv` for an
        // unsent document whatever it is handed.
        self.append_prepared(client, store_id, node_id, &[]).await
    }

    /// Append one document's update, already merged into the document here:
    /// encrypt, `vaultAppend`, record the outcome, snapshot when due.
    async fn append_prepared(
        &mut self,
        client: &Arc<PimbleClient>,
        store_id: StoreId,
        node_id: NodeId,
        update: &[u8],
    ) -> Result<(), String> {
        let outgoing = {
            let Some(store) = self.stores.get_mut(&store_id) else {
                return Err("no such encrypted store".to_string());
            };
            store.prepare(store_id, node_id, update)?
        };
        let appended = client
            .vault_append_from(store_id, outgoing.doc_id.clone(), outgoing.blob.clone(), Some(self.client_id.clone()))
            .await
            .map_err(|e| e.to_string());

        let Some(store) = self.stores.get_mut(&store_id) else { return Ok(()) };
        let snapshot = store.record(node_id, &outgoing, appended)?;

        if let Some(upto_seq) = snapshot {
            let blob = store.snapshot_blob(store_id, node_id)?;
            match client.vault_snapshot(store_id, outgoing.doc_id, upto_seq, blob).await {
                Ok(()) => {
                    if let Some(doc) = self.stores.get_mut(&store_id).and_then(|s| s.docs.get_mut(&node_id)) {
                        doc.appends = 0;
                    }
                }
                Err(e) => tracing::warn!("Uploading a snapshot of {} failed: {}", node_id, e),
            }
        }
        Ok(())
    }

    /// The same stateless reconcile the plain path does, answered from the
    /// node document this page holds.
    fn reconcile(&self, store_id: StoreId, node_id: NodeId, state_vector: &[u8]) -> BackendEvent {
        let Some(doc) = self.stores.get(&store_id).and_then(|s| s.tree.doc(node_id)) else {
            return BackendEvent::Error { message: "that node's document is not held here".into() };
        };
        match doc.diff_since(state_vector) {
            Ok(diff) => BackendEvent::NodeContentReconciled {
                store_id,
                node_id,
                diff,
                server_state_vector: doc.state_vector(),
            },
            Err(e) => BackendEvent::Error { message: e.to_string() },
        }
    }

    // ── Subscription ────────────────────────────────────────────────────────

    /// Subscribe to the store's changes and forward every notification into the
    /// channel `pump` drains.
    ///
    /// The task deliberately holds nothing but the sender: decryption needs
    /// `&mut self`, which a detached task cannot have and the backend loop
    /// already does.
    ///
    /// Three things ask for this — the client itself when it opens a store,
    /// the UI when it registers one, and the backend loop when it restores
    /// what a dead socket carried — and one per store per connection is what
    /// is wanted. `forget_subscriptions` is what makes a new socket start
    /// again.
    pub async fn subscribe(
        &mut self,
        client: &Arc<PimbleClient>,
        store_id: StoreId,
        signal_ui: &Arc<dyn Fn() + Send + Sync>,
    ) -> Option<BackendEvent> {
        if !self.subscribed.insert(store_id) {
            return None;
        }
        match client.subscribe_store_changes(store_id).await {
            Ok(mut sub) => {
                let tx = self.notices_tx.clone();
                let signal = signal_ui.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    while let Some(Ok(notification)) = sub.next().await {
                        if tx.send(notification).is_err() {
                            break;
                        }
                        signal();
                    }
                });
                None
            }
            Err(e) => {
                self.subscribed.remove(&store_id);
                tracing::warn!("Subscribing to the changes of {} failed: {}", store_id, e);
                Some(BackendEvent::Error { message: format!("Subscribe failed: {e}") })
            }
        }
    }

    /// Forget which stores are subscribed, because the socket that carried
    /// those subscriptions is gone. Called once per new connection, before
    /// anything subscribes again.
    pub fn forget_subscriptions(&mut self) {
        self.subscribed.clear();
    }

    // ── Search ──────────────────────────────────────────────────────────────

    /// Client-side search: a case-insensitive substring over decrypted titles
    /// and the text of every document held, which is every node's.
    ///
    /// There is no server index for a vault store and there cannot be one — the
    /// server has never seen a word of it. Plain stores in the same query still
    /// go to the server, and the two sets of hits are merged.
    async fn search(
        &self,
        client: &Arc<PimbleClient>,
        query: &str,
        stores: &[StoreId],
        limit: usize,
    ) -> BackendEvent {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return BackendEvent::SearchResults { results: Ok(Vec::new()) };
        }

        let mut results = Vec::new();
        for store_id in stores.iter().filter(|id| self.owns(**id)) {
            let Some(store) = self.stores.get(store_id) else { continue };
            for node_id in store.tree.list_node_ids() {
                let Ok(info) = store.tree.get_node_info(node_id) else { continue };
                let text = store.tree.doc(node_id).map(NodeDoc::text).unwrap_or_default();

                let in_title = info.title.to_lowercase().contains(&needle);
                let at = text.to_lowercase().find(&needle);
                if !in_title && at.is_none() {
                    continue;
                }

                results.push(SearchResultItem {
                    node_id,
                    store_id: *store_id,
                    // A title match ranks above a body match; there is no
                    // scoring model here and pretending otherwise would be
                    // worse than saying so.
                    score: if in_title { 1.0 } else { 0.5 },
                    title: if info.title.is_empty() { "Untitled".to_string() } else { info.title },
                    snippet: snippet_around(&text, at),
                    kind: "prose".to_string(),
                    node_type: info.node_type,
                    path: String::new(),
                });
            }
        }

        // Plain stores in the same query keep their server-side index.
        let plain: Vec<StoreId> = stores.iter().copied().filter(|id| !self.owns(*id)).collect();
        if !plain.is_empty() {
            match client.search(query.to_string(), plain, true, limit).await {
                Ok(mut items) => results.append(&mut items),
                Err(e) => tracing::warn!("Searching the plain stores failed: {}", e),
            }
        }

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);
        BackendEvent::SearchResults { results: Ok(results) }
    }
}

// ── The store's documents, without the network ───────────────────────────────
//
// Everything below is decided from what this page holds and what it is handed,
// so the rules the vault client was debugged for (cursors, what is known to be
// on the server, what to resend, when a snapshot may be stamped) are testable
// on their own.

impl VaultStore {
    /// A store from what its logs held: every decrypted entry, per document
    /// in log order. Picks the root, builds the tree, records each
    /// document's cursor. Nothing is repaired here; the caller runs
    /// [`VaultStore::repair_now`] once the whole pull is in.
    fn assemble(listed: Store, keyring: Keyring, pulled: Vec<Pulled>) -> Self {
        let mut node_docs: HashMap<NodeId, NodeDoc> = HashMap::new();
        let mut docs: HashMap<NodeId, VaultDoc> = HashMap::new();
        let mut heads = HashMap::new();
        for Pulled { node_id, entries, head } in pulled {
            let node_doc = node_docs.entry(node_id).or_default();
            let doc = docs.entry(node_id).or_default();
            for (mark, update) in entries {
                match update {
                    Ok(bytes) => match node_doc.apply_update(&bytes) {
                        Ok(_) => {
                            advance(&mut doc.known_sv, &bytes);
                            mark.apply(&mut doc.cursor);
                        }
                        Err(e) => tracing::warn!("Skipping an unreadable update of {}: {}", node_id, e),
                    },
                    Err(message) => tracing::warn!("Skipping a blob of {}: {}", node_id, message),
                }
            }
            heads.insert(node_id, head);
        }
        let mut tree = Tree::from_docs(listed.root_node_id, node_docs);
        let root = document_root(&tree, listed.root_node_id);
        if root != tree.root() {
            tracing::info!("Store {}: manifest root {} replaced by the documents' root {}", listed.id, listed.root_node_id, root);
            tree = rerooted(tree, root);
        }
        Self { listed, keyring, tree, docs, heads, repair_due: None }
    }

    fn doc_entry(&mut self, node_id: NodeId) -> &mut VaultDoc {
        self.docs.entry(node_id).or_default()
    }

    /// Merge a peer's decrypted updates of one document, in log order, and
    /// say what the UI should hear: the kinds the server would derive from
    /// what the merge changed (see [`derive_kinds`]), with a content change
    /// handed to the editor as `RemoteChanges` when it has the node open and
    /// as `NodeContentUpdated` otherwise. `source` is who wrote it, when the
    /// notification said.
    ///
    /// An entry that would not decrypt or merge is never marked, so the
    /// cursor stops in front of it and a later snapshot cannot vouch for it.
    fn merge(
        &mut self,
        store_id: StoreId,
        node_id: NodeId,
        entries: Vec<(Mark, Result<Vec<u8>, String>)>,
        active: bool,
        source: Option<String>,
    ) -> Merged {
        let before = shape_of(&self.tree, node_id);
        let mut effect = NodeUpdateEffect::default();
        let mut content_updates = Vec::new();
        for (mark, update) in entries {
            let bytes = match update {
                Ok(bytes) => bytes,
                Err(message) => {
                    tracing::warn!("Skipping a blob of {}: {}", node_id, message);
                    continue;
                }
            };
            match self.tree.apply_update(node_id, &bytes) {
                Ok(one) => {
                    let doc = self.doc_entry(node_id);
                    advance(&mut doc.known_sv, &bytes);
                    mark.apply(&mut doc.cursor);
                    effect.changed |= one.changed;
                    effect.structure |= one.structure;
                    effect.content |= one.content;
                    effect.data |= one.data;
                    if one.content || one.data {
                        content_updates.push(bytes);
                    }
                }
                Err(e) => tracing::warn!("An update of {} would not apply: {}", node_id, e),
            }
        }
        if !effect.changed {
            return Merged { events: Vec::new(), structure: false };
        }

        let after = shape_of(&self.tree, node_id);
        let mut events = Vec::new();
        for kind in derive_kinds(node_id, self.tree.root(), before.as_ref(), after.as_ref(), effect) {
            match kind {
                StoreChangeKind::ContentUpdated { .. } if active => {
                    // The editor has this node open, so hand it each delta
                    // the same way a plain store's subscription would.
                    for bytes in &content_updates {
                        events.push(BackendEvent::RemoteChanges { changes: STANDARD.encode(bytes) });
                    }
                }
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
        Merged { events, structure: effect.structure }
    }

    /// Repair the tree if it needs it (`Tree::repair`): what it wrote, to be
    /// appended like any edit.
    fn repair_now(&mut self, now: &str) -> Option<TreeEdit> {
        match self.tree.repair(now) {
            Ok(edit) => edit,
            Err(e) => {
                tracing::warn!("Repairing the tree of {} failed: {}", self.listed.id, e);
                None
            }
        }
    }

    /// Re-root the tree when the documents say the root is another node
    /// than the one it started from: the manifest's root is a placeholder
    /// nothing has written, and exactly one node has no parent. The store
    /// as the UI should now see it, to be announced again so the tree is
    /// fetched under the real root.
    ///
    /// A store opened before its documents arrived — hosted from a desktop
    /// whose link had not pushed yet — starts from the placeholder; as the
    /// desktop's root document lands this is what moves the tree under it.
    fn adopt_root(&mut self) -> Option<Store> {
        let current = self.tree.root();
        let root = document_root(&self.tree, current);
        if root == current {
            return None;
        }
        tracing::info!("Store {}: root {} replaced by the documents' root {}", self.listed.id, current, root);
        let tree = std::mem::replace(&mut self.tree, Tree::from_docs(root, HashMap::new()));
        self.tree = rerooted(tree, root);
        Some(self.view())
    }

    /// The store as the UI should see it: what the server listed, with the
    /// tree's root.
    fn view(&self) -> Store {
        let mut store = self.listed.clone();
        store.root_node_id = self.tree.root();
        store
    }

    /// Write the root's document when the store has none: a store the
    /// accounts service has just created has an empty log and a root id in
    /// its manifest, and nothing else will ever write that document.
    ///
    /// Done at the first write under the root rather than at open, so that
    /// a store whose documents are on their way from elsewhere (a desktop's
    /// link pushing them) is not given a second root by a page that merely
    /// looked at it; two roots each repair the other under themselves and
    /// never agree again. The update is the whole document, since nothing
    /// else holds any of it.
    fn seed_root(&mut self, now: &str) -> TreeEdit {
        let root = self.tree.root();
        let mut edit = TreeEdit::default();
        if self.tree.has_node(root) || self.tree.doc(root).is_some_and(NodeDoc::is_initialised) {
            return edit;
        }
        let mut doc = self.tree.take_doc(root).unwrap_or_default();
        match doc.init(node_types::FOLDER, &self.listed.name, None, now) {
            Ok(()) => edit.touched.push((root, doc.save())),
            Err(e) => tracing::warn!("Seeding the root of {} failed: {}", self.listed.id, e),
        }
        self.tree.insert_doc(root, doc);
        edit
    }

    /// The children of `node_id` from the tree. The root with no document
    /// yet (see [`VaultStore::seed_root`]) has none rather than being an
    /// error: the row is there, the store is simply empty.
    fn children_of(&self, node_id: NodeId) -> Result<Vec<NodeId>, String> {
        if node_id == self.tree.root() && !self.tree.has_node(node_id) {
            return Ok(Vec::new());
        }
        self.tree.get_children(node_id).map_err(|e| e.to_string())
    }

    /// Assemble the `Node` the app expects from the tree's view of a node and
    /// its document's bytes, as the server assembles one: the content is the
    /// whole node document, which the editor joins as it joined a content
    /// document (Pimble's roots ride along). The root with no document yet
    /// is a folder named after the store.
    fn node_of(&self, node_id: NodeId) -> Result<Node, String> {
        let root = self.tree.root();
        if node_id == root && !self.tree.has_node(root) {
            let now = chrono::Utc::now();
            return Ok(Node {
                id: root,
                parent_id: None,
                node_type: node_types::FOLDER.to_string(),
                metadata: NodeMetadata {
                    title: self.listed.name.clone(),
                    created_at: now,
                    modified_at: now,
                    tags: Vec::new(),
                    custom: HashMap::new(),
                },
                content: Vec::new(),
                children: Vec::new(),
                links: Vec::new(),
            });
        }
        let info = self.tree.get_node_info(node_id).map_err(|e| e.to_string())?;
        let children = self.tree.get_children(node_id).map_err(|e| e.to_string())?;
        let content = self.tree.doc(node_id).map(NodeDoc::save).unwrap_or_default();
        Ok(Node {
            id: node_id,
            parent_id: info.parent_id,
            node_type: info.node_type,
            metadata: NodeMetadata {
                title: info.title,
                created_at: parse_time(&info.created_at),
                modified_at: parse_time(&info.modified_at),
                tags: info.tags,
                custom: info.custom,
            },
            content,
            children,
            links: Vec::new(),
        })
    }

    /// Encrypt what goes to the server for one document: `update`, already
    /// merged into the document here, or — after a failed append, when the
    /// server is behind by more than this one change — everything it lacks.
    fn prepare(&mut self, store_id: StoreId, node_id: NodeId, update: &[u8]) -> Result<Outgoing, String> {
        let Some(node_doc) = self.tree.doc(node_id) else {
            return Err("that node's document is not held here".to_string());
        };
        let doc = self.docs.entry(node_id).or_default();
        let resend = doc.unsent;
        let payload = if resend {
            node_doc.diff_since(&doc.known_sv).map_err(|e| e.to_string())?
        } else {
            update.to_vec()
        };
        let doc_id = VaultDocId::Node(node_id);
        let blob = encrypt_blob(&self.keyring, store_id, &doc_id, &payload)?;
        Ok(Outgoing { doc_id, blob, payload, sent_sv: node_doc.state_vector(), resend })
    }

    /// Record how an append went. `Ok(Some(seq))` says a snapshot stamped
    /// `seq` is due; `Err` is the append's failure, recorded against the
    /// document so the next append (or the next connect) resends it.
    fn record(&mut self, node_id: NodeId, outgoing: &Outgoing, appended: Result<u64, String>) -> Result<Option<u64>, String> {
        let doc = self.docs.entry(node_id).or_default();
        let seq = match appended {
            Ok(seq) => seq,
            Err(message) => {
                doc.unsent = true;
                return Err(message);
            }
        };
        if outgoing.resend {
            doc.known_sv = outgoing.sent_sv.clone();
            doc.unsent = false;
        } else {
            advance(&mut doc.known_sv, &outgoing.payload);
        }
        doc.cursor.mark(seq);
        doc.appends += 1;
        let head = self.heads.entry(node_id).or_insert(0);
        *head = (*head).max(seq);

        // The server deletes every log entry at or below a snapshot's number,
        // so it may only be stamped with a number read *through*. Another
        // client's append just below ours may still be on its way; until it
        // lands the snapshot waits for a later append.
        let due = doc.appends >= SNAPSHOT_EVERY && doc.cursor.applied_through() == seq;
        Ok(due.then_some(seq))
    }

    /// The document's whole state, encrypted, for a snapshot.
    fn snapshot_blob(&self, store_id: StoreId, node_id: NodeId) -> Result<String, String> {
        let full = self.tree.doc(node_id).map(NodeDoc::save).unwrap_or_default();
        encrypt_blob(&self.keyring, store_id, &VaultDocId::Node(node_id), &full)
    }
}

/// A rename as the shared command path makes one: the title, and the flag
/// that stops the tree deriving a label from the content. Two transactions
/// on one document, appended as two blobs.
fn rename_edit(tree: &mut Tree, node_id: NodeId, title: &str, now: &str) -> pimble_crdt::Result<TreeEdit> {
    let mut edit = tree.set_title(node_id, title, now)?;
    let flag = tree.set_custom(node_id, custom_keys::EXPLICIT_TITLE, &serde_json::Value::Bool(true), now)?;
    edit.touched.extend(flag.touched);
    Ok(edit)
}

/// An appearance change: `None` for a field leaves it alone; `Some(None)`
/// (or an empty string, which `NodeMetadata::icon` and `color` already read
/// as "not set") removes the key, as `NodeMetadata::set_icon` does.
fn appearance_edit(
    tree: &mut Tree,
    node_id: NodeId,
    icon: Option<Option<String>>,
    color: Option<Option<String>>,
    tags: Option<Vec<String>>,
    now: &str,
) -> pimble_crdt::Result<TreeEdit> {
    let mut edit = TreeEdit::default();
    for (key, value) in [(custom_keys::ICON, icon), (custom_keys::COLOR, color)] {
        let Some(value) = value else { continue };
        let more = match value.filter(|s| !s.is_empty()) {
            Some(value) => tree.set_custom(node_id, key, &serde_json::Value::String(value), now)?,
            None => tree.remove_custom(node_id, key, now)?,
        };
        edit.touched.extend(more.touched);
    }
    if let Some(tags) = tags {
        edit.touched.extend(tree.set_tags(node_id, &tags, now)?.touched);
    }
    Ok(edit)
}

/// The root the documents say a store has: `current` (the manifest's root,
/// or what the tree starts from now) when it is a node with no `parent_id`;
/// otherwise the one node with no `parent_id`, when there is exactly one;
/// otherwise `current` (no documents yet, or several claim to be it, which
/// only a repair from a known root settles). The rule of
/// `pimble_store::LocalStore::document_root`, applied here.
fn document_root(tree: &Tree, current: NodeId) -> NodeId {
    let is_root_like = |id: NodeId| tree.get_node_info(id).is_ok_and(|info| info.parent_id.is_none());
    if is_root_like(current) {
        return current;
    }
    let mut candidates = tree.list_node_ids().into_iter().filter(|&id| is_root_like(id));
    match (candidates.next(), candidates.next()) {
        (Some(only), None) => only,
        _ => current,
    }
}

/// The same documents under another root. `Tree` starts from one root for
/// its life, so a tree that learns its real root is rebuilt around it.
fn rerooted(mut tree: Tree, root: NodeId) -> Tree {
    let ids = tree.ids();
    let mut docs: HashMap<NodeId, NodeDoc> = HashMap::with_capacity(ids.len());
    for id in ids {
        if let Some(doc) = tree.take_doc(id) {
            docs.insert(id, doc);
        }
    }
    Tree::from_docs(root, docs)
}

// ── Deriving notifications from a merged update ─────────────────────────────
//
// Copied from `crates/pimble-server/src/handler.rs` (`DocShape`, `shape_of`,
// `derive_kinds`), so a browser holding the documents tells the UI exactly
// what the server would. The two should become one implementation in
// `pimble-crdt`.

/// What a node document says about the node before or after a merge: the
/// `node` root as fields (`None` while the document is not initialised) and
/// the children list as stored.
struct DocShape {
    fields: Option<NodeFields>,
    children: Vec<NodeId>,
}

/// The shape of `id`'s document, `None` when the tree holds none.
fn shape_of(tree: &Tree, id: NodeId) -> Option<DocShape> {
    tree.doc(id).map(|doc| DocShape { fields: doc.fields().ok(), children: doc.children() })
}

/// The notifications a merged update earns, from what it changed in the
/// document (docs/NODE_DOCUMENT_CONTRACT.md section 2): the `node` root
/// newly written is `NodeCreated` (or `NodeDeleted` when it arrived as a
/// tombstone, or `TreeStructure` for a root, which has no parent to name);
/// a tombstone set is `NodeDeleted`, cleared is `NodeCreated` again (the
/// node is back under its parent); a changed `parent_id` is `NodeMoved`;
/// any other change to the `node` root is `MetadataUpdated`; a changed
/// children list is `TreeStructure { [node] }`; a changed `content` root,
/// or the plugin's `data` root, is `ContentUpdated`. One update can earn
/// several; a structural change nothing above names (a rewrite of the same
/// values) still earns `TreeStructure`. `root` stands in for a parent no
/// document names.
fn derive_kinds(
    node_id: NodeId,
    root: NodeId,
    before: Option<&DocShape>,
    after: Option<&DocShape>,
    effect: NodeUpdateEffect,
) -> Vec<StoreChangeKind> {
    let mut kinds = Vec::new();
    if effect.structure {
        let before_fields = before.and_then(|s| s.fields.as_ref());
        let after_fields = after.and_then(|s| s.fields.as_ref());
        let node_kind = match (before_fields, after_fields) {
            (None, Some(a)) => Some(match (a.deleted_at.is_some(), a.parent_id) {
                (true, parent) => StoreChangeKind::NodeDeleted { node_id, parent_id: parent.unwrap_or(root) },
                (false, Some(parent_id)) => StoreChangeKind::NodeCreated { node_id, parent_id },
                (false, None) => StoreChangeKind::TreeStructure { node_ids: vec![node_id] },
            }),
            (Some(b), Some(a)) => {
                if b.deleted_at.is_none() && a.deleted_at.is_some() {
                    Some(StoreChangeKind::NodeDeleted { node_id, parent_id: b.parent_id.or(a.parent_id).unwrap_or(root) })
                } else if b.deleted_at.is_some() && a.deleted_at.is_none() {
                    Some(StoreChangeKind::NodeCreated { node_id, parent_id: a.parent_id.unwrap_or(root) })
                } else if b.parent_id != a.parent_id {
                    Some(StoreChangeKind::NodeMoved {
                        node_id,
                        old_parent_id: b.parent_id.unwrap_or(root),
                        new_parent_id: a.parent_id.unwrap_or(root),
                    })
                } else if b != a {
                    Some(StoreChangeKind::MetadataUpdated { node_id })
                } else {
                    None
                }
            }
            // An initialised `node` root cannot become uninitialised, and
            // a change that touched neither side of it is a list change.
            (Some(_), None) | (None, None) => None,
        };
        let named_the_list = matches!(node_kind, Some(StoreChangeKind::TreeStructure { .. }));
        kinds.extend(node_kind);
        let before_children = before.map(|s| s.children.as_slice()).unwrap_or(&[]);
        let after_children = after.map(|s| s.children.as_slice()).unwrap_or(&[]);
        if (before_children != after_children || kinds.is_empty()) && !named_the_list {
            kinds.push(StoreChangeKind::TreeStructure { node_ids: vec![node_id] });
        }
    }
    if effect.content || effect.data {
        kinds.push(StoreChangeKind::ContentUpdated { node_id });
    }
    if kinds.is_empty() {
        kinds.push(StoreChangeKind::TreeStructure { node_ids: vec![node_id] });
    }
    kinds
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Which store a command is about, when it is about one.
///
/// Public because the backend loop needs it before the vault client does: a
/// command is answered through the endpoint that serves its store, whichever
/// kind of store that is.
pub fn store_id_of(cmd: &BackendCommand) -> Option<StoreId> {
    use BackendCommand::*;
    Some(match cmd {
        CloseStore { store_id }
        | CreateNode { store_id, .. }
        | GetNode { store_id, .. }
        | GetChildren { store_id, .. }
        | SetNodeContent { store_id, .. }
        | RenameNode { store_id, .. }
        | SetNodeAppearance { store_id, .. }
        | DeleteNode { store_id, .. }
        | MoveNode { store_id, .. }
        | CreateMount { store_id, .. }
        | GetMountState { store_id, .. }
        | BroadcastChanges { store_id, .. }
        | ReconcileNodeContent { store_id, .. }
        | SubscribeStoreChanges { store_id }
        | SubscribeNodeChanges { store_id, .. }
        | RebuildIndex { store_id }
        | SetStoreSync { store_id, .. }
        | GetStoreSync { store_id }
        | RemoveReplica { store_id, .. } => *store_id,
        MountRemoteStore { target_store_id, .. } => *target_store_id,
        _ => return None,
    })
}

/// A command's name, for the one error a vault store answers with.
fn name_of(cmd: &BackendCommand) -> &'static str {
    use BackendCommand::*;
    match cmd {
        CreateMount { .. } => "Mounting",
        GetMountState { .. } => "Mount state",
        SetStoreSync { .. } => "Linking to a remote",
        RemoveReplica { .. } => "Removing a replica",
        CloseStore { .. } => "Closing a store",
        _ => "That",
    }
}

fn parse_time(raw: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now())
}

/// The timestamp this page's edits carry (`created_at`, `modified_at`,
/// `deleted_at`), rfc3339 like the server's. The crdt crate never reads a
/// clock; what two replicas write is decided by their callers alone.
fn now_rfc3339() -> String {
    js_sys::Date::new_0().to_iso_string().as_string().unwrap_or_default()
}

/// The page's clock, in milliseconds.
fn now_ms() -> f64 {
    js_sys::Date::now()
}

/// About 120 characters around the match, or the start of the text.
fn snippet_around(text: &str, at: Option<usize>) -> String {
    if text.is_empty() {
        return String::new();
    }
    let start = at.unwrap_or(0).saturating_sub(40);
    let start = floor_char_boundary(text, start);
    let end = floor_char_boundary(text, (start + 160).min(text.len()));
    let mut snippet = text[start..end].replace('\n', " ");
    if start > 0 {
        snippet.insert(0, '…');
    }
    if end < text.len() {
        snippet.push('…');
    }
    snippet
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Encrypt one update for one document. The associated data ties the blob to
/// this store and this document, so it cannot be replayed into another.
fn encrypt_blob(
    keyring: &Keyring,
    store_id: StoreId,
    doc_id: &VaultDocId,
    plaintext: &[u8],
) -> Result<String, String> {
    let key = keyring
        .current_key()
        .ok_or("this store's key is not available on this device")?;
    let aad = blob_aad(&store_id.to_string(), &doc_id.as_str());
    Ok(URL_SAFE_NO_PAD.encode(Blob::encrypt(key, keyring.current, &aad, plaintext)))
}

/// Open one blob. The key id inside it picks the key, so a store that has been
/// rotated still reads its own history.
fn decrypt_blob(
    keyring: &Keyring,
    store_id: StoreId,
    doc_id: &VaultDocId,
    encoded: &str,
) -> Result<Vec<u8>, String> {
    let blob = decode_blob(encoded)?;
    let key_id = Blob::key_id(&blob).map_err(|e| e.to_string())?;
    let key = keyring
        .get(&key_id)
        .ok_or_else(|| format!("no key {key_id} on this device"))?;
    let aad = blob_aad(&store_id.to_string(), &doc_id.as_str());
    Blob::decrypt(key, &aad, &blob).map_err(|e| e.to_string())
}

/// base64url without padding is what the contract specifies; the padded and
/// standard alphabets are accepted too, so a server that encodes either way
/// still reads.
fn decode_blob(encoded: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD
        .decode(encoded)
        .or_else(|_| URL_SAFE.decode(encoded))
        .or_else(|_| STANDARD.decode(encoded))
        .map_err(|e| format!("the blob would not decode: {e}"))
}

/// What applying a fetched entry says about the log: a snapshot stands for
/// everything up to its number, an update for its own number only.
#[derive(Clone, Copy)]
enum Mark {
    Through(u64),
    One(u64),
}

impl Mark {
    /// Called only once the entry has been applied; an entry that would not
    /// decrypt or merge is never marked, so the cursor stops in front of it.
    fn apply(self, cursor: &mut VaultCursor) {
        match self {
            Mark::Through(seq) => cursor.mark_through(seq),
            Mark::One(seq) => cursor.mark(seq),
        }
    }
}

/// Every blob in a fetch, snapshot first, decrypted in order.
fn decrypt_entries(
    keyring: &Keyring,
    store_id: StoreId,
    doc_id: &VaultDocId,
    fetched: &VaultFetchResponse,
) -> Vec<(Mark, Result<Vec<u8>, String>)> {
    let mut out = Vec::with_capacity(fetched.updates.len() + 1);
    if let Some(entry) = &fetched.snapshot {
        out.push((Mark::Through(entry.seq), decrypt_blob(keyring, store_id, doc_id, &entry.blob)));
    }
    for entry in &fetched.updates {
        out.push((Mark::One(entry.seq), decrypt_blob(keyring, store_id, doc_id, &entry.blob)));
    }
    out
}

/// One document's `vaultFetch` answer, or why there is none.
type Fetched = (NodeId, Result<VaultFetchResponse, String>);

/// `vaultFetch` for several documents at once: `(node id, after seq)` in,
/// each answer (or failure) out, in windows of [`FETCH_WINDOW`].
///
/// The browser has one thread and the futures have to be driven together, so
/// each is handed to the page as a promise and the window is `Promise.all`;
/// every promise resolves whatever the fetch did, with the outcome kept
/// aside, so one refused document never fails the window.
async fn fetch_many(client: &Arc<PimbleClient>, store_id: StoreId, wanted: Vec<(NodeId, u64)>) -> Vec<Fetched> {
    let results: Rc<RefCell<Vec<Fetched>>> = Rc::new(RefCell::new(Vec::with_capacity(wanted.len())));
    for window in wanted.chunks(FETCH_WINDOW) {
        let promises = js_sys::Array::new();
        for &(node_id, after_seq) in window {
            let client = client.clone();
            let results = results.clone();
            promises.push(&wasm_bindgen_futures::future_to_promise(async move {
                let fetched = client
                    .vault_fetch(store_id, VaultDocId::Node(node_id), after_seq)
                    .await
                    .map_err(|e| e.to_string());
                results.borrow_mut().push((node_id, fetched));
                Ok(wasm_bindgen::JsValue::NULL)
            }));
        }
        let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::all(&promises)).await;
    }
    let collected = std::mem::take(&mut *results.borrow_mut());
    collected
}

/// `update` is on the server (this client appended it, or fetched it): move
/// the record of what the server holds. It only moves when the update continues
/// from what is already known, so it can lag, never overstate.
fn advance(known_sv: &mut Vec<u8>, update: &[u8]) {
    if let Ok(next) = pimble_crdt::advance_state_vector(known_sv, update) {
        *known_sv = next;
    }
}

/// Whether an incoming `VaultAppended` should be applied.
///
/// Authorship is the only thing that can decide this, and the notification
/// carries it: a blob this client sent comes back with its own id on it, and
/// everything else is somebody else's work. A sequence number cannot stand in
/// for authorship. Several clients append to one document and the server hands
/// out its numbers in arrival order, so one client's appends are interleaved
/// with another's, and "not newer than the last number I was given" throws away
/// exactly the updates that arrived while this client was typing.
///
/// When in doubt this applies. A yrs merge is idempotent, so a genuine echo
/// applied twice changes nothing, while a dropped update is gone for good.
pub fn should_apply(source_client_id: Option<&str>, my_client_id: &str) -> bool {
    match source_client_id {
        Some(source) => source != my_client_id,
        // Unattributed: an append made by something that did not name itself,
        // or by a server that does not stamp one. Applying is the safe
        // reading — a merge repeated is nothing, a merge missed is a lost
        // edit.
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_crypto::SymmetricKey;
    use std::path::PathBuf;

    const T0: &str = "2026-09-18T10:00:00Z";
    const T1: &str = "2026-09-18T10:00:01Z";
    const T2: &str = "2026-09-18T10:00:02Z";

    // ── Fixtures ────────────────────────────────────────────────────────────

    fn keyring() -> Keyring {
        let key_id = uuid::Uuid::new_v4();
        Keyring { keys: HashMap::from([(key_id, SymmetricKey::generate())]), current: key_id }
    }

    fn listed(root: NodeId) -> Store {
        let mut store = Store::new_local("Vault", PathBuf::new());
        store.root_node_id = root;
        store.kind = StoreKind::Vault;
        store
    }

    /// A peer's tree, whose edits play the part of what another device
    /// appended to the hosted logs.
    fn origin() -> (Tree, NodeId) {
        let root = NodeId::new();
        (Tree::new(root, "Vault", T0).unwrap(), root)
    }

    /// The whole state of every document `tree` holds, as one pull: what a
    /// fresh page fetches (each document's log collapsed to a snapshot).
    fn pull_of(tree: &Tree) -> Vec<Pulled> {
        tree.ids()
            .into_iter()
            .map(|id| Pulled {
                node_id: id,
                entries: vec![(Mark::Through(1), Ok(tree.doc(id).unwrap().save()))],
                head: 1,
            })
            .collect()
    }

    /// A store page holding a copy of `tree`, as it is after an open.
    fn opened(tree: &Tree) -> VaultStore {
        VaultStore::assemble(listed(tree.root()), keyring(), pull_of(tree))
    }

    /// Feed the updates of `edit` to `store` one at a time, as the
    /// notifications of one peer's edit arrive; `seq` numbers them.
    fn arrive(store: &mut VaultStore, store_id: StoreId, edit: &TreeEdit, seq: &mut u64, active: Option<NodeId>) -> (Vec<BackendEvent>, bool) {
        let mut events = Vec::new();
        let mut structure = false;
        for (id, update) in &edit.touched {
            *seq += 1;
            let merged = store.merge(store_id, *id, vec![(Mark::One(*seq), Ok(update.clone()))], active == Some(*id), None);
            events.extend(merged.events);
            structure |= merged.structure;
        }
        (events, structure)
    }

    fn kinds_of(events: &[BackendEvent]) -> Vec<StoreChangeKind> {
        events
            .iter()
            .filter_map(|e| match e {
                BackendEvent::RemoteStoreChange { change_kind, .. } => Some(change_kind.clone()),
                _ => None,
            })
            .collect()
    }

    // ── The tree from documents ─────────────────────────────────────────────

    #[test]
    fn the_tree_is_built_from_the_documents_and_follows_their_updates() {
        let (mut peer, root) = origin();
        let (folder, x, y) = (NodeId::new(), NodeId::new(), NodeId::new());
        peer.add_node(folder, Some(root), None, "folder", "F", T0).unwrap();
        peer.add_node(x, Some(root), None, "document", "x", T0).unwrap();
        peer.add_node(y, Some(folder), None, "document", "y", T0).unwrap();

        let store_id = StoreId::new();
        let mut store = opened(&peer);
        assert_eq!(store.tree.root(), root);
        assert_eq!(store.children_of(root).unwrap(), vec![folder, x]);
        assert_eq!(store.children_of(folder).unwrap(), vec![y]);
        assert!(store.tree.validate_tree().is_empty());
        assert!(store.repair_now(T1).is_none(), "a consistent pull needs no repair");
        for id in [root, folder, x, y] {
            assert_eq!(store.docs[&id].cursor.applied_through(), 1, "each document's cursor covers its snapshot");
        }

        let mut seq = 10;
        // A create arriving as two document updates.
        let z = NodeId::new();
        let edit = peer.add_node(z, Some(folder), Some(0), "document", "z", T1).unwrap();
        arrive(&mut store, store_id, &edit, &mut seq, None);
        assert_eq!(store.children_of(folder).unwrap(), vec![z, y]);
        assert_eq!(store.node_of(z).unwrap().metadata.title, "z");

        // A move: three documents' updates.
        let edit = peer.move_node(y, root, None, T1).unwrap();
        arrive(&mut store, store_id, &edit, &mut seq, None);
        assert_eq!(store.children_of(folder).unwrap(), vec![z]);
        assert_eq!(store.children_of(root).unwrap(), vec![folder, x, y]);
        assert_eq!(store.node_of(y).unwrap().parent_id, Some(root));

        // A delete: tombstones for the subtree, and the parent's list.
        let edit = peer.remove_node(folder, T2).unwrap();
        arrive(&mut store, store_id, &edit, &mut seq, None);
        assert_eq!(store.children_of(root).unwrap(), vec![x, y]);
        assert!(store.node_of(folder).is_err(), "a tombstone is not a node");
        assert!(store.node_of(z).is_err());
        assert!(store.tree.doc(z).is_some(), "the document stays");
        assert!(store.tree.validate_tree().is_empty());
        assert!(store.repair_now(T2).is_none());
    }

    #[test]
    fn the_root_is_the_manifest_s_unless_the_documents_name_another() {
        let (mut peer, real_root) = origin();
        let x = NodeId::new();
        peer.add_node(x, Some(real_root), None, "document", "x", T0).unwrap();

        // A store hosted from a desktop: the manifest carries a placeholder
        // the desktop's documents never mention.
        let placeholder = NodeId::new();
        let store = VaultStore::assemble(listed(placeholder), keyring(), pull_of(&peer));
        assert_eq!(store.tree.root(), real_root);
        assert_eq!(store.view().root_node_id, real_root);
        assert_eq!(store.children_of(real_root).unwrap(), vec![x]);

        // An empty log keeps the manifest's root, answers an empty tree for
        // it, and describes it as a folder named after the store.
        let mut empty = VaultStore::assemble(listed(placeholder), keyring(), Vec::new());
        assert_eq!(empty.tree.root(), placeholder);
        assert_eq!(empty.children_of(placeholder).unwrap(), Vec::<NodeId>::new());
        assert_eq!(empty.node_of(placeholder).unwrap().metadata.title, "Vault");
        assert!(empty.repair_now(T1).is_none());

        // The first write under it seeds the root document, and only once.
        let seed = empty.seed_root(T1);
        assert_eq!(seed.node_ids(), vec![placeholder]);
        assert!(empty.tree.has_node(placeholder));
        assert!(empty.seed_root(T1).is_empty());
        let y = NodeId::new();
        empty.tree.add_node(y, Some(placeholder), None, "document", "y", T1).unwrap();
        assert_eq!(empty.children_of(placeholder).unwrap(), vec![y]);
    }

    #[test]
    fn a_root_arriving_after_the_open_re_roots_the_tree_once() {
        // Opened empty, before a desktop's link pushed anything; the
        // person only looked, so nothing was seeded here.
        let placeholder = NodeId::new();
        let store_id = StoreId::new();
        let mut store = VaultStore::assemble(listed(placeholder), keyring(), Vec::new());

        let (mut peer, real_root) = origin();
        let x = NodeId::new();
        let edit = peer.add_node(x, Some(real_root), None, "document", "x", T0).unwrap();
        let mut seq = 0;
        // The child's document lands first: no root to adopt yet.
        arrive(&mut store, store_id, &TreeEdit { touched: vec![edit.touched[0].clone()] }, &mut seq, None);
        assert!(store.adopt_root().is_none());
        // The root's document: the tree moves under it and the UI is told.
        let root_update = peer.doc(real_root).unwrap().save();
        let merged = store.merge(store_id, real_root, vec![(Mark::One(1), Ok(root_update))], false, None);
        assert!(merged.structure);
        let announced = store.adopt_root().expect("the documents' root replaces the placeholder");
        assert_eq!(announced.root_node_id, real_root);
        assert_eq!(store.tree.root(), real_root);
        assert_eq!(store.children_of(real_root).unwrap(), vec![x]);
        assert!(store.adopt_root().is_none(), "settled");
    }

    // ── What a tree edit appends ────────────────────────────────────────────

    #[test]
    fn a_tree_edit_is_appended_per_document_in_order() {
        let (mut peer, root) = origin();
        let store_id = StoreId::new();
        let mut store = opened(&peer);
        let x = NodeId::new();
        let edit = store.tree.add_node(x, Some(root), None, "document", "x", T1).unwrap();
        assert_eq!(edit.node_ids(), vec![x, root], "the new document before its parent's list");

        let mut seq = 0;
        for (node_id, update) in &edit.touched {
            let outgoing = store.prepare(store_id, *node_id, update).unwrap();
            assert_eq!(outgoing.doc_id, VaultDocId::Node(*node_id));
            assert!(!outgoing.resend);
            // The blob is that document's update and nothing else, under
            // this store's and this document's associated data.
            let opened = decrypt_blob(&store.keyring, store_id, &outgoing.doc_id, &outgoing.blob).unwrap();
            assert_eq!(&opened, update);
            let other = VaultDocId::Node(NodeId::new());
            assert!(decrypt_blob(&store.keyring, store_id, &other, &outgoing.blob).is_err(), "bound to its document");
            seq += 1;
            assert_eq!(store.record(*node_id, &outgoing, Ok(seq)).unwrap(), None);
            let doc = &store.docs[node_id];
            assert_eq!(doc.cursor.applied_through(), seq, "our own append counts as read");
            assert!(!doc.unsent);
        }
        // A peer applying the blobs in that order ends with the same tree.
        for (node_id, update) in &edit.touched {
            peer.apply_update(*node_id, update).unwrap();
        }
        assert_eq!(peer.get_children(root).unwrap(), vec![x]);
        assert_eq!(peer.get_node_info(x).unwrap().title, "x");
        assert!(peer.validate_tree().is_empty());
    }

    #[test]
    fn a_failed_append_is_resent_as_everything_the_server_lacks() {
        let (peer, root) = origin();
        let store_id = StoreId::new();
        let mut store = opened(&peer);
        let known_before = store.docs[&root].known_sv.clone();

        // A rename whose append fails.
        let first = store.tree.set_title(root, "One", T1).unwrap();
        let (_, update) = &first.touched[0];
        let outgoing = store.prepare(store_id, root, update).unwrap();
        assert!(store.record(root, &outgoing, Err("socket closed".into())).is_err());
        assert!(store.docs[&root].unsent);
        assert_eq!(store.docs[&root].known_sv, known_before, "nothing moved");

        // The next edit carries both: a diff since what the server holds.
        let second = store.tree.set_title(root, "Two", T2).unwrap();
        let (_, update) = &second.touched[0];
        let outgoing = store.prepare(store_id, root, update).unwrap();
        assert!(outgoing.resend);
        let mut fresh = NodeDoc::load(&peer.doc(root).unwrap().save()).unwrap();
        fresh.apply_update(&outgoing.payload).unwrap();
        assert_eq!(fresh.fields().unwrap().title, "Two", "the resend brings the server level");
        assert_eq!(store.record(root, &outgoing, Ok(7)).unwrap(), None);
        assert!(!store.docs[&root].unsent);
        assert_eq!(store.docs[&root].known_sv, store.tree.doc(root).unwrap().state_vector());
    }

    #[test]
    fn a_snapshot_is_due_only_when_the_cursor_has_read_through_its_number() {
        let (peer, root) = origin();
        let store_id = StoreId::new();
        let mut store = opened(&peer);
        let mut seq = 1;
        for i in 0..SNAPSHOT_EVERY {
            let edit = store.tree.set_title(root, &format!("t{i}"), T1).unwrap();
            let (_, update) = &edit.touched[0];
            let outgoing = store.prepare(store_id, root, update).unwrap();
            seq += 1;
            // Somebody else's append at 100 never arrived: from then on the
            // cursor stops short of our own numbers.
            if seq == 100 {
                seq += 1;
            }
            let due = store.record(root, &outgoing, Ok(seq)).unwrap();
            assert_eq!(due, None, "append {i}: not until the gap closes");
        }
        // The missing entry lands; the next append may snapshot.
        store.docs.get_mut(&root).unwrap().cursor.mark(100);
        let edit = store.tree.set_title(root, "last", T2).unwrap();
        let outgoing = store.prepare(store_id, root, &edit.touched[0].1).unwrap();
        seq += 1;
        assert_eq!(store.record(root, &outgoing, Ok(seq)).unwrap(), Some(seq));
    }

    // ── What the UI hears ───────────────────────────────────────────────────

    #[test]
    fn merged_structural_updates_earn_the_kinds_the_server_derives() {
        let (mut peer, root) = origin();
        let (p, q) = (NodeId::new(), NodeId::new());
        peer.add_node(p, Some(root), None, "folder", "P", T0).unwrap();
        peer.add_node(q, Some(root), None, "folder", "Q", T0).unwrap();
        let store_id = StoreId::new();
        let mut store = opened(&peer);
        let mut seq = 0;

        // Create: the node's own document is `NodeCreated`, the parent's
        // list `TreeStructure`.
        let x = NodeId::new();
        let edit = peer.add_node(x, Some(p), None, "document", "x", T1).unwrap();
        let (events, structure) = arrive(&mut store, store_id, &edit, &mut seq, None);
        assert!(structure);
        assert!(matches!(kinds_of(&events)[..], [StoreChangeKind::NodeCreated { node_id, parent_id }, StoreChangeKind::TreeStructure { ref node_ids }] if node_id == x && parent_id == p && node_ids == &vec![p]), "{events:?}");

        // Rename: `MetadataUpdated`.
        let edit = peer.set_title(x, "Renamed", T1).unwrap();
        let (events, _) = arrive(&mut store, store_id, &edit, &mut seq, None);
        assert!(matches!(kinds_of(&events)[..], [StoreChangeKind::MetadataUpdated { node_id }] if node_id == x), "{events:?}");

        // Move: `NodeMoved` for the node, `TreeStructure` for both lists.
        let edit = peer.move_node(x, q, None, T1).unwrap();
        let (events, _) = arrive(&mut store, store_id, &edit, &mut seq, None);
        let kinds = kinds_of(&events);
        assert!(kinds.iter().any(|k| matches!(k, StoreChangeKind::NodeMoved { node_id, old_parent_id, new_parent_id } if *node_id == x && *old_parent_id == p && *new_parent_id == q)), "{kinds:?}");
        assert_eq!(kinds.iter().filter(|k| matches!(k, StoreChangeKind::TreeStructure { .. })).count(), 2, "{kinds:?}");

        // Delete: `NodeDeleted` naming the parent it left.
        let edit = peer.remove_node(x, T2).unwrap();
        let (events, _) = arrive(&mut store, store_id, &edit, &mut seq, None);
        let kinds = kinds_of(&events);
        assert!(kinds.iter().any(|k| matches!(k, StoreChangeKind::NodeDeleted { node_id, parent_id } if *node_id == x && *parent_id == q)), "{kinds:?}");

        // The same bytes again change nothing and say nothing.
        let (events, structure) = arrive(&mut store, store_id, &edit, &mut seq, None);
        assert!(events.is_empty() && !structure);
    }

    #[test]
    fn a_content_update_reaches_the_editor_only_for_the_node_it_has_open() {
        let (mut peer, root) = origin();
        let x = NodeId::new();
        peer.add_node(x, Some(root), None, "document", "x", T0).unwrap();
        let store_id = StoreId::new();
        let mut store = opened(&peer);

        let delta = peer.doc_mut(x).unwrap().replace_plain_text("typed elsewhere").unwrap();
        let edit = TreeEdit { touched: vec![(x, delta.clone())] };
        let mut seq = 0;

        // Not open here: the UI refetches the node.
        let (events, structure) = arrive(&mut store, store_id, &edit, &mut seq, None);
        assert!(!structure, "content is not structure");
        assert!(matches!(events[..], [BackendEvent::NodeContentUpdated { node_id, .. }] if node_id == x), "{events:?}");
        assert_eq!(store.tree.doc(x).unwrap().text(), "typed elsewhere");

        // Open in the editor: the delta itself, base64 as the plain path
        // carries it.
        let mut again = opened(&peer);
        let delta = peer.doc_mut(x).unwrap().replace_plain_text("and more").unwrap();
        let edit = TreeEdit { touched: vec![(x, delta.clone())] };
        let (events, _) = arrive(&mut again, store_id, &edit, &mut seq, Some(x));
        assert!(matches!(&events[..], [BackendEvent::RemoteChanges { changes }] if *changes == STANDARD.encode(&delta)), "{events:?}");
    }

    // ── Repair only after a whole pull ──────────────────────────────────────

    #[test]
    fn a_pull_is_repaired_once_after_the_whole_of_it() {
        let (mut peer, root) = origin();
        let (p, q, x) = (NodeId::new(), NodeId::new(), NodeId::new());
        peer.add_node(p, Some(root), None, "folder", "P", T0).unwrap();
        peer.add_node(q, Some(root), None, "folder", "Q", T0).unwrap();
        peer.add_node(x, Some(p), None, "document", "x", T0).unwrap();
        let before = pull_of(&peer);
        let before_the_move = opened(&peer);
        let edit = peer.move_node(x, q, None, T1).unwrap();
        assert_eq!(edit.touched.len(), 3);

        // The pull holds the snapshots and, after them, the move's three
        // updates in log order. Applied whole, the tree is consistent and
        // the one repair afterwards has nothing to do.
        let mut pulled = before;
        for (id, update) in &edit.touched {
            let doc = pulled.iter_mut().find(|d| d.node_id == *id).unwrap();
            doc.entries.push((Mark::One(2), Ok(update.clone())));
            doc.head = 2;
        }
        let mut store = VaultStore::assemble(listed(root), keyring(), pulled);
        assert!(store.tree.validate_tree().is_empty(), "{:?}", store.tree.validate_tree());
        assert!(store.repair_now(T2).is_none());
        assert_eq!(store.children_of(q).unwrap(), vec![x]);
        assert!(store.children_of(p).unwrap().is_empty());

        // Judged after each update it would have written something: after
        // P's removal alone, x is listed nowhere, and repair's answer is to
        // append it to P again, an edit that then travels. On the live path
        // the merge itself never repairs; it only says a repair is wanted,
        // and by the time the debounce fires the rest of the edit is in.
        let mut half = before_the_move;
        let store_id = StoreId::new();
        let (id, update) = &edit.touched[0];
        assert_eq!(*id, p, "the old parent's list goes first");
        let merged = half.merge(store_id, *id, vec![(Mark::One(2), Ok(update.clone()))], false, None);
        assert!(merged.structure, "flagged for the debounced repair");
        assert!(!half.tree.validate_tree().is_empty(), "half a move is inconsistent");
        assert!(half.repair_due.is_none(), "the merge itself schedules nothing");
        for (id, update) in &edit.touched[1..] {
            half.merge(store_id, *id, vec![(Mark::One(2), Ok(update.clone()))], false, None);
        }
        assert!(half.tree.validate_tree().is_empty());
        assert!(half.repair_now(T2).is_none(), "nothing left to write once the whole edit is in");
        assert_eq!(half.children_of(q).unwrap(), vec![x]);
    }

    #[test]
    fn a_repair_writes_what_the_tree_needs_and_names_it() {
        let (mut peer, root) = origin();
        let x = NodeId::new();
        peer.add_node(x, Some(root), None, "document", "x", T0).unwrap();
        let mut store = opened(&peer);
        // A duplicate a concurrent append left in the root's list.
        store.tree.doc_mut(root).unwrap().insert_child(99, x).unwrap();
        let edit = store.repair_now(T1).expect("a duplicate needs a repair");
        assert_eq!(edit.node_ids(), vec![root]);
        assert!(store.tree.validate_tree().is_empty());
        assert_eq!(store.children_of(root).unwrap(), vec![x]);
    }

    // ── Which notifications are applied ─────────────────────────────────────

    #[test]
    fn a_client_skips_only_its_own_work() {
        assert!(!should_apply(Some("me"), "me"));
        assert!(should_apply(Some("someone-else"), "me"));
    }

    #[test]
    fn an_unattributed_notification_is_applied() {
        // What the server sends today: `VaultAppendRequest` carries no client
        // id, so nothing is suppressed and every blob is merged.
        assert!(should_apply(None, "me"));
    }

    #[test]
    fn interleaved_appends_from_two_clients_all_arrive() {
        // Two tabs typing in turn. The server numbers appends in arrival
        // order, so each client's own ids are scattered through the other's —
        // which is why a sequence number cannot stand in for authorship. Every
        // update written by the other client must be applied, whether its
        // number is above or below anything this client was last given.
        let log: [(u64, &str); 6] = [
            (1, "a"),
            (2, "b"),
            (3, "a"),
            (4, "b"),
            (5, "b"),
            (6, "a"),
        ];

        let seen_by_a: Vec<u64> = log
            .iter()
            .filter(|(_, who)| should_apply(Some(who), "a"))
            .map(|(seq, _)| *seq)
            .collect();
        let seen_by_b: Vec<u64> = log
            .iter()
            .filter(|(_, who)| should_apply(Some(who), "b"))
            .map(|(seq, _)| *seq)
            .collect();

        assert_eq!(seen_by_a, vec![2, 4, 5], "a must see every append b made");
        assert_eq!(seen_by_b, vec![1, 3, 6], "b must see every append a made");
    }

    #[test]
    fn a_late_number_is_not_a_reason_to_drop() {
        // The shape of the bug this replaced: b's append lands between two of
        // a's, so its number is below the last one a was given. It is still b's
        // work and still has to be applied.
        assert!(should_apply(Some("b"), "a"));
    }
}
