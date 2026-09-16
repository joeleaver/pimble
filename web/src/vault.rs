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
}

/// One encrypted store.
struct VaultStore {
    root_node_id: NodeId,
    keyring: Keyring,
    tree: StoreDocument,
    tree_seq: u64,
    tree_appends: u32,
    docs: HashMap<NodeId, VaultDoc>,
    /// Each document's head as the server last reported it, so a node with an
    /// empty log is never fetched.
    heads: HashMap<NodeId, u64>,
    /// Whether `vaultListDocs` has been asked yet.
    heads_known: bool,
    /// Sequence numbers this client produced, per document name, so its own
    /// `VaultAppended` notifications are dropped instead of applied twice.
    own_seqs: HashMap<String, HashSet<u64>>,
}

pub struct VaultClient {
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
    pub fn new() -> Self {
        let (notices_tx, notices_rx) = unbounded();
        Self {
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
        let keyring = keys::fetch_keyring(&store_id.to_string()).await?;

        let fetched = client
            .vault_fetch(store_id, VaultDocId::Tree, 0)
            .await
            .map_err(|e| format!("fetching the tree failed: {e}"))?;

        let mut tree = StoreDocument::load(&[]).map_err(|e| e.to_string())?;
        for update in decrypt_entries(&keyring, store_id, &VaultDocId::Tree, &fetched) {
            match update {
                Ok(bytes) => {
                    if let Err(e) = tree.apply_update(&bytes) {
                        tracing::warn!("Skipping an unreadable tree update: {}", e);
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
                .vault_append(store_id, VaultDocId::Tree, blob)
                .await
                .map_err(|e| format!("seeding the tree failed: {e}"))?;
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
                docs: HashMap::new(),
                heads: HashMap::new(),
                heads_known: false,
                own_seqs: HashMap::new(),
            },
        );

        // The seeding append above is this client's own; it must not come back
        // as news.
        if let Some(store) = self.stores.get_mut(&store_id) {
            remember_own(store, &VaultDocId::Tree, tree_seq);
        }
        Ok(())
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
            if let Some(event) = self.apply_notification(store_id, doc_id, seq, &blob) {
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
    ) -> Option<BackendEvent> {
        let active = self.active;
        let store = self.stores.get_mut(&store_id)?;

        // This client's own append, echoed back. Dropping it by sequence number
        // is what stops a local edit being applied twice.
        if forget_own(store, &doc_id, seq) {
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
                store.heads.insert(node_id, seq);

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
        let update = store.tree.diff_since(&before).map_err(|e| e.to_string())?;
        let blob = encrypt_blob(&store.keyring, store_id, &VaultDocId::Tree, &update)?;

        let seq = client
            .vault_append(store_id, VaultDocId::Tree, blob)
            .await
            .map_err(|e| e.to_string())?;

        let Some(store) = self.stores.get_mut(&store_id) else { return Ok(()) };
        store.tree_seq = store.tree_seq.max(seq);
        store.tree_appends += 1;
        remember_own(store, &VaultDocId::Tree, seq);

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

        let blob = encrypt_blob(&store.keyring, store_id, &doc_id, update)?;
        let seq = client
            .vault_append(store_id, doc_id.clone(), blob)
            .await
            .map_err(|e| e.to_string())?;

        let Some(store) = self.stores.get_mut(&store_id) else { return Ok(()) };
        remember_own(store, &doc_id, seq);
        store.heads.insert(node_id, seq);
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
        });

        for update in decrypt_entries(&store.keyring, store_id, &doc_id, &fetched) {
            match update {
                Ok(bytes) => {
                    if let Err(e) = entry.content.apply_update(&bytes) {
                        tracing::warn!("Skipping an unreadable content update: {}", e);
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

/// Note that this client produced `seq` for `doc_id`, so the notification it
/// causes is recognised as an echo.
fn remember_own(store: &mut VaultStore, doc_id: &VaultDocId, seq: u64) {
    store.own_seqs.entry(doc_id.as_str()).or_default().insert(seq);
}

/// Whether `seq` was this client's own append, consuming the record.
fn forget_own(store: &mut VaultStore, doc_id: &VaultDocId, seq: u64) -> bool {
    store
        .own_seqs
        .get_mut(&doc_id.as_str())
        .map(|seqs| seqs.remove(&seq))
        .unwrap_or(false)
}
