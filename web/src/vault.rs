//! The vault client: an encrypted store, driven from the browser.
//!
//! A vault store holds nothing the server can read. It keeps an append-only log
//! of opaque blobs per document, and everything that makes those blobs a tree of
//! notes happens here: the store document and every open node's content
//! document are yrs documents in this page's memory, built by decrypting what
//! the log holds, and every local change is encrypted before it is appended
//! (docs/CRYPTO_CONTRACT.md, "Web app").
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
//! editor is wired to.

use std::collections::{HashMap, HashSet};

use base64::engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine as _;
use crossbeam_channel::{unbounded, Receiver, Sender};
use pimble_app::protocol::{BackendCommand, BackendEvent};
use pimble_client::PimbleClient;
use pimble_core::{custom_keys, node_types, Node, NodeId, NodeMetadata, Store, StoreId, StoreKind};
use pimble_crdt::{ContentDoc, StoreDocument};
use pimble_crypto::{blob_aad, Blob};
use pimble_rpc::{
    SearchResultItem, StoreChangeKind, StoreChangedNotification, VaultDocId, VaultEntry,
};

use crate::keys::{self, Keyring};

/// How many appends to a document before its snapshot is refreshed. The
/// contract's number: a reader then replays at most this many updates.
const SNAPSHOT_EVERY: u32 = 200;

/// What [`VaultClient::handle`] decided about a command.
pub enum Handled {
    /// Not for a vault store. The command comes back untouched.
    No(BackendCommand),
    /// Answered here. The event, if there is one, is the command's reply.
    Yes(Option<BackendEvent>),
}

/// One node's content, as this page holds it.
struct VaultDoc {
    content: ContentDoc,
    /// The highest sequence number merged into `content`.
    seq: u64,
    /// Appends since the last snapshot upload.
    appends: u32,
    /// A state vector everything up to which the server is known to hold
    /// (see `advance`). What a resend after a failed append is a diff against.
    known_sv: Vec<u8>,
    /// An append failed: `content` holds work the server does not. Every later
    /// edit depends on it, so until it is resent no other device can show
    /// anything this one types.
    unsent: bool,
}

/// One encrypted store.
struct VaultStore {
    root_node_id: NodeId,
    keyring: Keyring,
    tree: StoreDocument,
    tree_seq: u64,
    tree_appends: u32,
    /// As `VaultDoc::known_sv` and `VaultDoc::unsent`, for the tree.
    tree_known_sv: Vec<u8>,
    tree_unsent: bool,
    docs: HashMap<NodeId, VaultDoc>,
    /// Each document's head as the server last reported it, so a node with an
    /// empty log is never fetched.
    heads: HashMap<NodeId, u64>,
    /// Whether `vaultListDocs` has been asked yet.
    heads_known: bool,
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
    /// `SubscribeNodeChanges`. A decrypted update is only turned into
    /// `RemoteChanges` for this node: the app has one editor pane and that
    /// event carries no node identity, so applying another node's update to it
    /// would corrupt what is on screen.
    active: Option<(StoreId, NodeId)>,
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
            notices_tx,
            notices_rx,
        }
    }

    /// Whether this store is one the vault client answers for.
    pub fn owns(&self, store_id: StoreId) -> bool {
        self.stores.contains_key(&store_id)
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

    /// Open every vault store in `stores` that is not open yet.
    ///
    /// Called before `StoresListed` reaches the UI, so the tree's first
    /// `GetChildren` already has a store document to read.
    pub async fn open_listed(&mut self, client: &PimbleClient, stores: &[Store]) -> Vec<String> {
        let mut problems = Vec::new();
        for store in stores {
            let is_vault = store.kind == StoreKind::Vault || self.vault_ids.contains(&store.id);
            if !is_vault || self.stores.contains_key(&store.id) {
                continue;
            }
            if let Err(message) = self.open_store(client, store).await {
                tracing::error!("Opening the encrypted store {}: {}", store.name, message);
                problems.push(format!("{}: {}", store.name, message));
            }
        }
        problems
    }

    /// Fetch this store's keys and its whole tree document, decrypt, and hold it.
    async fn open_store(&mut self, client: &PimbleClient, store: &Store) -> Result<(), String> {
        let store_id = store.id;
        let me = self.client_id.clone();
        let keyring = keys::fetch_keyring(&store_id.to_string()).await?;

        let fetched = client
            .vault_fetch(store_id, VaultDocId::Tree, 0)
            .await
            .map_err(|e| format!("fetching the tree failed: {e}"))?;

        let mut tree = StoreDocument::load(&[]).map_err(|e| e.to_string())?;
        let mut tree_known_sv = pimble_crdt::empty_state_vector();
        for update in decrypt_entries(&keyring, store_id, &VaultDocId::Tree, &fetched) {
            match update {
                Ok(bytes) => {
                    if let Err(e) = tree.apply_update(&bytes) {
                        tracing::warn!("Skipping an unreadable tree update: {}", e);
                    } else {
                        advance(&mut tree_known_sv, &bytes);
                    }
                }
                Err(message) => tracing::warn!("Skipping a tree blob: {}", message),
            }
        }
        let mut tree_seq = fetched.head;

        // An empty log is a store the accounts service has just created. Seed
        // it here, under the id and root the server already minted, so both
        // sides agree about what the root is.
        if tree.root_node_id().is_err() {
            tree = StoreDocument::new(&store.name, store.root_node_id)
                .map_err(|e| format!("building the store document failed: {e}"))?;
            let blob = encrypt_blob(&keyring, store_id, &VaultDocId::Tree, &tree.save())?;
            tree_seq = client
                .vault_append_from(store_id, VaultDocId::Tree, blob, Some(me))
                .await
                .map_err(|e| format!("seeding the tree failed: {e}"))?;
            tree_known_sv = tree.state_vector();
        }

        let root_node_id = tree.root_node_id().unwrap_or(store.root_node_id);

        self.stores.insert(
            store_id,
            VaultStore {
                root_node_id,
                keyring,
                tree,
                tree_seq,
                tree_appends: 0,
                tree_known_sv,
                tree_unsent: false,
                docs: HashMap::new(),
                heads: HashMap::new(),
                heads_known: false,
            },
        );
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
    /// the one before). Called on every connect, after `open_listed`.
    pub async fn catch_up(&mut self, client: &PimbleClient) -> Vec<BackendEvent> {
        let mut events = Vec::new();
        let store_ids: Vec<StoreId> = self.stores.keys().copied().collect();
        for store_id in store_ids {
            // Down: the tree, then every document held.
            let tree_seq = self.stores.get(&store_id).map(|s| s.tree_seq).unwrap_or(0);
            if let Ok(fetched) = client.vault_fetch(store_id, VaultDocId::Tree, tree_seq).await {
                if let Some(store) = self.stores.get_mut(&store_id) {
                    let mut touched = Vec::new();
                    for update in decrypt_entries(&store.keyring, store_id, &VaultDocId::Tree, &fetched) {
                        let Ok(bytes) = update else { continue };
                        if let Ok(effect) = store.tree.apply_update(&bytes) {
                            advance(&mut store.tree_known_sv, &bytes);
                            if effect.changed {
                                touched.extend(effect.touched);
                            }
                        }
                    }
                    store.tree_seq = store.tree_seq.max(fetched.head);
                    if !touched.is_empty() {
                        events.push(BackendEvent::RemoteStoreChange {
                            store_id,
                            change_kind: StoreChangeKind::TreeStructure { node_ids: touched },
                            source_client_id: None,
                        });
                    }
                }
            }

            let held: Vec<(NodeId, u64)> = self
                .stores
                .get(&store_id)
                .map(|s| s.docs.iter().map(|(id, doc)| (*id, doc.seq)).collect())
                .unwrap_or_default();
            for (node_id, seq) in held {
                let doc_id = VaultDocId::Node(node_id);
                let Ok(fetched) = client.vault_fetch(store_id, doc_id.clone(), seq).await else { continue };
                let active = self.active == Some((store_id, node_id));
                let Some(store) = self.stores.get_mut(&store_id) else { continue };
                let updates = decrypt_entries(&store.keyring, store_id, &doc_id, &fetched);
                let Some(doc) = store.docs.get_mut(&node_id) else { continue };
                let mut changed = false;
                for update in updates {
                    let Ok(bytes) = update else { continue };
                    if doc.content.apply_update(&bytes).is_ok() {
                        advance(&mut doc.known_sv, &bytes);
                        changed = true;
                        if active {
                            events.push(BackendEvent::RemoteChanges { changes: STANDARD.encode(&bytes) });
                        }
                    }
                }
                doc.seq = doc.seq.max(fetched.head);
                store.heads.insert(node_id, fetched.head);
                if changed && !active {
                    events.push(BackendEvent::NodeContentUpdated { store_id, node_id });
                }
            }

            // Up: whatever an append failed to deliver.
            if self.stores.get(&store_id).map_or(false, |s| s.tree_unsent) {
                if let Err(e) = self.commit_tree(client, store_id, |_| Ok(())).await {
                    tracing::warn!("Resending the tree of {} failed: {}", store_id, e);
                }
            }
            let unsent: Vec<NodeId> = self
                .stores
                .get(&store_id)
                .map(|s| s.docs.iter().filter(|(_, doc)| doc.unsent).map(|(id, _)| *id).collect())
                .unwrap_or_default();
            for node_id in unsent {
                if let Err(e) = self.resend_content(client, store_id, node_id).await {
                    tracing::warn!("Resending the content of {} failed: {}", node_id, e);
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
            let doc_id = doc_id.clone();
            let seq = *seq;
            let Some(blob) = notification.update.clone() else {
                tracing::warn!("A VaultAppended notification carried no blob; ignoring it");
                continue;
            };
            let source = notification.source_client_id.clone();
            if let Some(event) =
                self.apply_notification(store_id, doc_id, seq, &blob, source.as_deref())
            {
                events.push(event);
            }
        }
        events
    }

    fn apply_notification(
        &mut self,
        store_id: StoreId,
        doc_id: VaultDocId,
        seq: u64,
        blob: &str,
        source_client_id: Option<&str>,
    ) -> Option<BackendEvent> {
        let active = self.active;
        let apply = should_apply(source_client_id, &self.client_id);
        let store = self.stores.get_mut(&store_id)?;

        // Sequence numbers say how far this client has read, never who wrote
        // what; `should_apply` is the whole of that decision.
        if !apply {
            return None;
        }

        let update = match decrypt_blob(&store.keyring, store_id, &doc_id, blob) {
            Ok(bytes) => bytes,
            Err(message) => {
                // An unknown key id is not fatal: a rotation this device has
                // not been granted yet looks exactly like this.
                tracing::warn!("Skipping a blob for {}: {}", doc_id.as_str(), message);
                return None;
            }
        };

        match doc_id {
            VaultDocId::Tree => {
                let effect = match store.tree.apply_update(&update) {
                    Ok(effect) => effect,
                    Err(e) => {
                        tracing::warn!("A tree update would not apply: {}", e);
                        return None;
                    }
                };
                store.tree_seq = store.tree_seq.max(seq);
                advance(&mut store.tree_known_sv, &update);
                if !effect.changed {
                    return None;
                }
                Some(BackendEvent::RemoteStoreChange {
                    store_id,
                    change_kind: StoreChangeKind::TreeStructure { node_ids: effect.touched },
                    source_client_id: None,
                })
            }
            VaultDocId::Node(node_id) => {
                let doc = store.docs.get_mut(&node_id)?;
                if let Err(e) = doc.content.apply_update(&update) {
                    tracing::warn!("A content update would not apply: {}", e);
                    return None;
                }
                doc.seq = doc.seq.max(seq);
                advance(&mut doc.known_sv, &update);
                let head = store.heads.entry(node_id).or_insert(0);
                *head = (*head).max(seq);

                if active == Some((store_id, node_id)) {
                    // The editor has this node open, so hand it the delta the
                    // same way a plain store's subscription would.
                    Some(BackendEvent::RemoteChanges {
                        changes: STANDARD.encode(&update),
                    })
                } else {
                    Some(BackendEvent::NodeContentUpdated { store_id, node_id })
                }
            }
        }
    }

    // ── Commands ────────────────────────────────────────────────────────────

    /// Answer `cmd` if it names a vault store; otherwise hand it back.
    pub async fn handle(
        &mut self,
        client: &PimbleClient,
        cmd: BackendCommand,
        signal_ui: &std::sync::Arc<dyn Fn() + Send + Sync>,
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
            BackendCommand::GetChildren { store_id, node_id } => {
                Some(self.get_children(client, store_id, node_id).await)
            }

            BackendCommand::GetNode { store_id, node_id } => {
                Some(self.get_node(client, store_id, node_id).await)
            }

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
                match self.set_content(client, store_id, node_id, &content).await {
                    Ok(()) => Some(BackendEvent::NodeContentUpdated { store_id, node_id }),
                    Err(message) => Some(BackendEvent::Error { message }),
                }
            }

            BackendCommand::ReconcileNodeContent { store_id, node_id, state_vector } => {
                Some(self.reconcile(client, store_id, node_id, &state_vector).await)
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

    async fn get_children(
        &mut self,
        client: &PimbleClient,
        store_id: StoreId,
        node_id: NodeId,
    ) -> BackendEvent {
        let child_ids = match self.stores.get(&store_id).map(|s| s.tree.get_children(node_id)) {
            Some(Ok(ids)) => ids,
            Some(Err(e)) => return BackendEvent::Error { message: e.to_string() },
            None => return BackendEvent::Error { message: "no such encrypted store".into() },
        };

        // A document's tree label is derived from its first line, and the app
        // reads that straight off the node it is handed here (there is no
        // second fetch on the way to opening one). So a child that has content
        // must arrive with it, which means loading what this subtree holds.
        for child in &child_ids {
            let _ = self.ensure_doc(client, store_id, *child).await;
        }

        let Some(store) = self.stores.get(&store_id) else {
            return BackendEvent::Error { message: "no such encrypted store".into() };
        };
        let children: Vec<Node> = child_ids
            .iter()
            .filter_map(|id| node_of(store, *id).ok())
            .collect();

        BackendEvent::ChildrenLoaded {
            store_id,
            parent_id: node_id,
            children_store_id: store_id,
            children,
        }
    }

    async fn get_node(
        &mut self,
        client: &PimbleClient,
        store_id: StoreId,
        node_id: NodeId,
    ) -> BackendEvent {
        let _ = self.ensure_doc(client, store_id, node_id).await;
        let Some(store) = self.stores.get(&store_id) else {
            return BackendEvent::Error { message: "no such encrypted store".into() };
        };
        match node_of(store, node_id) {
            Ok(node) => BackendEvent::NodeLoaded { store_id, node },
            Err(message) => BackendEvent::Error { message },
        }
    }

    // ── Tree writes ─────────────────────────────────────────────────────────

    async fn create_node(
        &mut self,
        client: &PimbleClient,
        store_id: StoreId,
        parent_id: Option<NodeId>,
        title: String,
    ) -> BackendEvent {
        let node_id = NodeId::new();
        let parent = match parent_id.or_else(|| self.stores.get(&store_id).map(|s| s.root_node_id)) {
            Some(parent) => parent,
            None => return BackendEvent::Error { message: "no such encrypted store".into() },
        };

        let outcome = self
            .commit_tree(client, store_id, |tree| {
                tree.add_node(node_id, Some(parent), node_types::DOCUMENT, &title)
                    .map_err(|e| e.to_string())
            })
            .await;

        match outcome {
            Ok(()) => BackendEvent::NodeCreated { store_id, parent_id: Some(parent), node_id },
            Err(message) => BackendEvent::Error { message },
        }
    }

    async fn rename_node(
        &mut self,
        client: &PimbleClient,
        store_id: StoreId,
        node_id: NodeId,
        title: String,
    ) -> BackendEvent {
        let outcome = self
            .commit_tree(client, store_id, |tree| {
                tree.set_title(node_id, &title).map_err(|e| e.to_string())?;
                tree.set_custom(
                    node_id,
                    custom_keys::EXPLICIT_TITLE,
                    &serde_json::Value::Bool(true),
                )
                .map_err(|e| e.to_string())
            })
            .await;

        match outcome {
            Ok(()) => BackendEvent::NodeRenamed { store_id, node_id },
            Err(message) => BackendEvent::Error { message },
        }
    }

    async fn set_appearance(
        &mut self,
        client: &PimbleClient,
        store_id: StoreId,
        node_id: NodeId,
        icon: Option<Option<String>>,
        color: Option<Option<String>>,
        tags: Option<Vec<String>>,
    ) -> BackendEvent {
        let outcome = self
            .commit_tree(client, store_id, |tree| {
                // `None` for a field leaves it alone; `Some(None)` clears it.
                // The store document has no "remove a custom key", so a cleared
                // field becomes the empty string, which `NodeMetadata::icon`
                // and `color` already read as "not set".
                if let Some(icon) = icon {
                    let value = serde_json::Value::String(icon.unwrap_or_default());
                    tree.set_custom(node_id, custom_keys::ICON, &value).map_err(|e| e.to_string())?;
                }
                if let Some(color) = color {
                    let value = serde_json::Value::String(color.unwrap_or_default());
                    tree.set_custom(node_id, custom_keys::COLOR, &value).map_err(|e| e.to_string())?;
                }
                if let Some(tags) = tags {
                    tree.set_tags(node_id, &tags).map_err(|e| e.to_string())?;
                }
                Ok(())
            })
            .await;

        match outcome {
            Ok(()) => BackendEvent::NodeRenamed { store_id, node_id },
            Err(message) => BackendEvent::Error { message },
        }
    }

    async fn delete_node(
        &mut self,
        client: &PimbleClient,
        store_id: StoreId,
        node_id: NodeId,
    ) -> BackendEvent {
        let Some(store) = self.stores.get(&store_id) else {
            return BackendEvent::Error { message: "no such encrypted store".into() };
        };
        let parent_id = match store.tree.get_node_info(node_id) {
            Ok(info) => info.parent_id,
            Err(e) => return BackendEvent::Error { message: e.to_string() },
        };
        let Some(parent_id) = parent_id else {
            return BackendEvent::Error { message: "the root node cannot be deleted".into() };
        };

        // The whole subtree goes, as it does server-side, and deepest first so
        // no entry is orphaned mid-way through the transaction.
        let mut doomed = Vec::new();
        collect_subtree(&store.tree, node_id, &mut doomed);
        doomed.reverse();

        let outcome = self
            .commit_tree(client, store_id, |tree| {
                for id in &doomed {
                    tree.remove_node(*id).map_err(|e| e.to_string())?;
                }
                Ok(())
            })
            .await;

        if let Some(store) = self.stores.get_mut(&store_id) {
            for id in &doomed {
                store.docs.remove(id);
                store.heads.remove(id);
            }
        }

        match outcome {
            Ok(()) => BackendEvent::NodeDeleted { store_id, node_id, parent_id },
            Err(message) => BackendEvent::Error { message },
        }
    }

    async fn move_node(
        &mut self,
        client: &PimbleClient,
        store_id: StoreId,
        node_id: NodeId,
        new_parent_id: NodeId,
        position: Option<usize>,
    ) -> BackendEvent {
        let old_parent_id = match self
            .stores
            .get(&store_id)
            .map(|s| s.tree.get_node_info(node_id))
        {
            Some(Ok(info)) => info.parent_id.unwrap_or(new_parent_id),
            Some(Err(e)) => return BackendEvent::Error { message: e.to_string() },
            None => return BackendEvent::Error { message: "no such encrypted store".into() },
        };

        let outcome = self
            .commit_tree(client, store_id, |tree| {
                tree.move_node(node_id, new_parent_id, position).map_err(|e| e.to_string())
            })
            .await;

        match outcome {
            Ok(()) => BackendEvent::NodeMoved { store_id, node_id, old_parent_id, new_parent_id },
            Err(message) => BackendEvent::Error { message },
        }
    }

    /// Mutate the store document, then append exactly what that mutation
    /// produced.
    ///
    /// The update is the difference the transaction made, taken against the
    /// state vector from just before it — never the whole document, which would
    /// grow every append and merge badly with a peer's own history.
    async fn commit_tree<F>(
        &mut self,
        client: &PimbleClient,
        store_id: StoreId,
        mutate: F,
    ) -> Result<(), String>
    where
        F: FnOnce(&mut StoreDocument) -> Result<(), String>,
    {
        let Some(store) = self.stores.get_mut(&store_id) else {
            return Err("no such encrypted store".to_string());
        };

        let before = store.tree.state_vector();
        mutate(&mut store.tree)?;
        // After a failed append the server is behind by more than this one
        // change, so what goes out is everything it lacks.
        let resend = store.tree_unsent;
        let since = if resend { store.tree_known_sv.clone() } else { before };
        let update = store.tree.diff_since(&since).map_err(|e| e.to_string())?;
        let sent_sv = store.tree.state_vector();
        let blob = encrypt_blob(&store.keyring, store_id, &VaultDocId::Tree, &update)?;

        let appended = client
            .vault_append_from(store_id, VaultDocId::Tree, blob, Some(self.client_id.clone()))
            .await;

        let Some(store) = self.stores.get_mut(&store_id) else { return Ok(()) };
        let seq = match appended {
            Ok(seq) => seq,
            Err(e) => {
                store.tree_unsent = true;
                return Err(e.to_string());
            }
        };
        if resend {
            store.tree_known_sv = sent_sv;
            store.tree_unsent = false;
        } else {
            advance(&mut store.tree_known_sv, &update);
        }
        store.tree_seq = store.tree_seq.max(seq);
        store.tree_appends += 1;

        if store.tree_appends >= SNAPSHOT_EVERY {
            let full = store.tree.save();
            let blob = encrypt_blob(&store.keyring, store_id, &VaultDocId::Tree, &full)?;
            match client.vault_snapshot(store_id, VaultDocId::Tree, seq, blob).await {
                Ok(()) => {
                    if let Some(store) = self.stores.get_mut(&store_id) {
                        store.tree_appends = 0;
                    }
                }
                Err(e) => tracing::warn!("Uploading the tree snapshot failed: {}", e),
            }
        }
        Ok(())
    }

    // ── Content ─────────────────────────────────────────────────────────────

    /// A local edit from the editor: merge it, encrypt it, append it.
    async fn broadcast(
        &mut self,
        client: &PimbleClient,
        store_id: StoreId,
        node_id: NodeId,
        changes: &str,
    ) -> Result<(), String> {
        // The editor's outbound closure encodes with the standard alphabet,
        // exactly as the plain path's `applyEdit` carries it.
        let update = STANDARD
            .decode(changes)
            .map_err(|e| format!("the editor's delta would not decode: {e}"))?;
        self.append_content(client, store_id, node_id, &update).await
    }

    /// A whole content document, from a session that started from nothing.
    async fn set_content(
        &mut self,
        client: &PimbleClient,
        store_id: StoreId,
        node_id: NodeId,
        content: &[u8],
    ) -> Result<(), String> {
        self.append_content(client, store_id, node_id, content).await
    }

    /// Send everything `node_id`'s document holds that the server does not,
    /// for a document an earlier append failed to deliver.
    async fn resend_content(&mut self, client: &PimbleClient, store_id: StoreId, node_id: NodeId) -> Result<(), String> {
        let doc_id = VaultDocId::Node(node_id);
        let (blob, sent_sv) = {
            let Some(store) = self.stores.get(&store_id) else { return Ok(()) };
            let Some(doc) = store.docs.get(&node_id) else { return Ok(()) };
            let payload = doc.content.diff_since(&doc.known_sv).map_err(|e| e.to_string())?;
            (encrypt_blob(&store.keyring, store_id, &doc_id, &payload)?, doc.content.state_vector())
        };
        let seq = client
            .vault_append_from(store_id, doc_id, blob, Some(self.client_id.clone()))
            .await
            .map_err(|e| e.to_string())?;
        let Some(store) = self.stores.get_mut(&store_id) else { return Ok(()) };
        let head = store.heads.entry(node_id).or_insert(0);
        *head = (*head).max(seq);
        if let Some(doc) = store.docs.get_mut(&node_id) {
            doc.seq = doc.seq.max(seq);
            doc.known_sv = sent_sv;
            doc.unsent = false;
        }
        Ok(())
    }

    async fn append_content(
        &mut self,
        client: &PimbleClient,
        store_id: StoreId,
        node_id: NodeId,
        update: &[u8],
    ) -> Result<(), String> {
        self.ensure_doc(client, store_id, node_id).await?;

        let doc_id = VaultDocId::Node(node_id);
        let Some(store) = self.stores.get_mut(&store_id) else {
            return Err("no such encrypted store".to_string());
        };
        let Some(doc) = store.docs.get_mut(&node_id) else {
            return Err("that node's content is not open".to_string());
        };
        doc.content.apply_update(update).map_err(|e| e.to_string())?;

        // After a failed append the server is behind by more than this one
        // edit, so what goes out is everything it lacks.
        let resend = doc.unsent;
        let payload = if resend {
            doc.content.diff_since(&doc.known_sv).map_err(|e| e.to_string())?
        } else {
            update.to_vec()
        };
        let sent_sv = doc.content.state_vector();
        let blob = encrypt_blob(&store.keyring, store_id, &doc_id, &payload)?;
        let appended = client
            .vault_append_from(store_id, doc_id.clone(), blob, Some(self.client_id.clone()))
            .await;

        let Some(store) = self.stores.get_mut(&store_id) else { return Ok(()) };
        let seq = match appended {
            Ok(seq) => seq,
            Err(e) => {
                if let Some(doc) = store.docs.get_mut(&node_id) {
                    doc.unsent = true;
                }
                return Err(e.to_string());
            }
        };
        if let Some(doc) = store.docs.get_mut(&node_id) {
            if resend {
                doc.known_sv = sent_sv;
                doc.unsent = false;
            } else {
                advance(&mut doc.known_sv, &payload);
            }
        }
        let head = store.heads.entry(node_id).or_insert(0);
        *head = (*head).max(seq);
        let needs_snapshot = match store.docs.get_mut(&node_id) {
            Some(doc) => {
                doc.seq = doc.seq.max(seq);
                doc.appends += 1;
                doc.appends >= SNAPSHOT_EVERY
            }
            None => false,
        };

        if needs_snapshot {
            let full = store
                .docs
                .get(&node_id)
                .map(|doc| doc.content.save())
                .unwrap_or_default();
            let blob = encrypt_blob(&store.keyring, store_id, &doc_id, &full)?;
            match client.vault_snapshot(store_id, doc_id, seq, blob).await {
                Ok(()) => {
                    if let Some(doc) = self
                        .stores
                        .get_mut(&store_id)
                        .and_then(|s| s.docs.get_mut(&node_id))
                    {
                        doc.appends = 0;
                    }
                }
                Err(e) => tracing::warn!("Uploading a content snapshot failed: {}", e),
            }
        }
        Ok(())
    }

    /// The same stateless reconcile the plain path does, answered from the
    /// content document this page holds.
    async fn reconcile(
        &mut self,
        client: &PimbleClient,
        store_id: StoreId,
        node_id: NodeId,
        state_vector: &[u8],
    ) -> BackendEvent {
        if let Err(message) = self.ensure_doc(client, store_id, node_id).await {
            return BackendEvent::Error { message };
        }
        let Some(doc) = self.stores.get(&store_id).and_then(|s| s.docs.get(&node_id)) else {
            return BackendEvent::Error { message: "that node's content is not open".into() };
        };
        match doc.content.diff_since(state_vector) {
            Ok(diff) => BackendEvent::NodeContentReconciled {
                store_id,
                node_id,
                diff,
                server_state_vector: doc.content.state_vector(),
            },
            Err(e) => BackendEvent::Error { message: e.to_string() },
        }
    }

    /// Make sure `node_id`'s content document is in memory and current.
    ///
    /// A document with an empty log is created empty and never fetched again
    /// until something is appended to it, which is what `heads` is for.
    async fn ensure_doc(
        &mut self,
        client: &PimbleClient,
        store_id: StoreId,
        node_id: NodeId,
    ) -> Result<(), String> {
        self.ensure_heads(client, store_id).await;

        let (have, from_seq, head) = {
            let Some(store) = self.stores.get(&store_id) else {
                return Err("no such encrypted store".to_string());
            };
            let doc = store.docs.get(&node_id);
            (
                doc.is_some(),
                doc.map(|d| d.seq).unwrap_or(0),
                store.heads.get(&node_id).copied().unwrap_or(0),
            )
        };

        if have && from_seq >= head {
            return Ok(());
        }

        let doc_id = VaultDocId::Node(node_id);
        let fetched = client
            .vault_fetch(store_id, doc_id.clone(), from_seq)
            .await
            .map_err(|e| format!("fetching that node's content failed: {e}"))?;

        let Some(store) = self.stores.get_mut(&store_id) else {
            return Err("no such encrypted store".to_string());
        };
        let entry = store.docs.entry(node_id).or_insert_with(|| VaultDoc {
            content: ContentDoc::new(),
            seq: 0,
            appends: 0,
            known_sv: pimble_crdt::empty_state_vector(),
            unsent: false,
        });

        for update in decrypt_entries(&store.keyring, store_id, &doc_id, &fetched) {
            match update {
                Ok(bytes) => {
                    if let Err(e) = entry.content.apply_update(&bytes) {
                        tracing::warn!("Skipping an unreadable content update: {}", e);
                    } else {
                        advance(&mut entry.known_sv, &bytes);
                    }
                }
                Err(message) => tracing::warn!("Skipping a content blob: {}", message),
            }
        }
        entry.seq = entry.seq.max(fetched.head);
        store.heads.insert(node_id, fetched.head);
        Ok(())
    }

    /// Ask once which documents exist and how long their logs are.
    async fn ensure_heads(&mut self, client: &PimbleClient, store_id: StoreId) {
        if self.stores.get(&store_id).map(|s| s.heads_known).unwrap_or(true) {
            return;
        }
        match client.vault_list_docs(store_id).await {
            Ok(docs) => {
                if let Some(store) = self.stores.get_mut(&store_id) {
                    for info in docs {
                        if let VaultDocId::Node(id) = info.doc_id {
                            store.heads.insert(id, info.head);
                        }
                    }
                    store.heads_known = true;
                }
            }
            Err(e) => tracing::warn!("Listing the vault's documents failed: {}", e),
        }
    }

    // ── Subscription ────────────────────────────────────────────────────────

    /// Subscribe to the store's changes and forward every notification into the
    /// channel `pump` drains.
    ///
    /// The task deliberately holds nothing but the sender: decryption needs
    /// `&mut self`, which a detached task cannot have and the backend loop
    /// already does.
    async fn subscribe(
        &mut self,
        client: &PimbleClient,
        store_id: StoreId,
        signal_ui: &std::sync::Arc<dyn Fn() + Send + Sync>,
    ) -> Option<BackendEvent> {
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
            Err(e) => Some(BackendEvent::Error {
                message: format!("Subscribe failed: {e}"),
            }),
        }
    }

    // ── Search ──────────────────────────────────────────────────────────────

    /// Client-side search: a case-insensitive substring over decrypted titles
    /// and whatever content this page has loaded.
    ///
    /// There is no server index for a vault store and there cannot be one — the
    /// server has never seen a word of it. Plain stores in the same query still
    /// go to the server, and the two sets of hits are merged.
    async fn search(
        &self,
        client: &PimbleClient,
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
            let Ok(ids) = store.tree.list_node_ids() else { continue };
            for node_id in ids {
                let Ok(info) = store.tree.get_node_info(node_id) else { continue };
                let text = store
                    .docs
                    .get(&node_id)
                    .map(|doc| doc.content.text())
                    .unwrap_or_default();

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

/// Assemble the `Node` the app expects from the store document and whatever
/// content is loaded.
fn node_of(store: &VaultStore, node_id: NodeId) -> Result<Node, String> {
    let info = store.tree.get_node_info(node_id).map_err(|e| e.to_string())?;
    let children = store.tree.get_children(node_id).map_err(|e| e.to_string())?;

    let created_at = parse_time(&info.created_at);
    let modified_at = parse_time(&info.modified_at);
    let content = store.docs.get(&node_id).map(|doc| doc.content.save()).unwrap_or_default();

    Ok(Node {
        id: node_id,
        parent_id: info.parent_id,
        node_type: info.node_type,
        metadata: NodeMetadata {
            title: info.title,
            created_at,
            modified_at,
            tags: info.tags,
            custom: info.custom,
        },
        content,
        children,
        links: Vec::new(),
    })
}

fn parse_time(raw: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now())
}

/// `node_id` and every descendant, parents before children.
fn collect_subtree(tree: &StoreDocument, node_id: NodeId, out: &mut Vec<NodeId>) {
    out.push(node_id);
    if let Ok(children) = tree.get_children(node_id) {
        for child in children {
            collect_subtree(tree, child, out);
        }
    }
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

/// Every blob in a fetch, snapshot first, decrypted in order.
fn decrypt_entries(
    keyring: &Keyring,
    store_id: StoreId,
    doc_id: &VaultDocId,
    fetched: &pimble_rpc::VaultFetchResponse,
) -> Vec<Result<Vec<u8>, String>> {
    let mut out = Vec::with_capacity(fetched.updates.len() + 1);
    let entries = fetched
        .snapshot
        .iter()
        .chain(fetched.updates.iter())
        .collect::<Vec<&VaultEntry>>();
    for entry in entries {
        out.push(decrypt_blob(keyring, store_id, doc_id, &entry.blob));
    }
    out
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
    use super::should_apply;

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
