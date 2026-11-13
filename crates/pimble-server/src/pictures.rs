//! Pictures between servers (docs/IMAGES_CONTRACT.md, waves 2 and 3).
//!
//! A picture's bytes are an immutable blob beside the store; a document
//! names it (an image atom in the text, a picture on a canvas). Nothing about
//! a blob is ever merged, so replicating one is only making sure it is
//! present wherever a document that names it is:
//!
//! - [`PictureRefs`] remembers which pictures each document names, keyed by
//!   the document's state vector, so asking again about an unchanged
//!   document costs a state vector and not a projection of its text. It is
//!   what a link copies by, and what judges a share's member reading a blob
//!   (`RpcHandler::readable_pictures`).
//! - [`spawn_copier`] runs beside a link for as long as its connection is
//!   up: after the reconcile, and a moment after any document of the store
//!   changes here (an edit made here, or one the link merged in), it asks
//!   which pictures the changed documents name, which of them this store and
//!   the remote lack, and copies each one across, either way. A picture
//!   neither side holds yet (its uploader is still sending it) is looked for
//!   again on a backoff. A missing picture never blocks a document: the text
//!   is merged first, the picture when its bytes arrive.
//! - [`PictureRemote`] is the other end: [`PlainRemote`] for a plain sync
//!   link (`haveBlobs`, `getBlob`, `putBlob` with `copy_of`), and the vault
//!   link's own for an encrypted twin (`crate::vault_link`, a picture as a
//!   vault document of its own).
//!
//! Nothing here changes where data goes: a copier runs only beside a link
//! the store already has, to the remote its documents already go to.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jsonrpsee::core::async_trait;
use pimble_client::PimbleClient;
use pimble_core::{BlobId, NodeId, StoreId};
use pimble_crdt::NodeDoc;
use pimble_rpc::vault as protocol;
use pimble_rpc::{StoreChangeKind, VaultDocId, BLOB_NOT_HERE};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::handler::{LocalChange, RpcHandler};

/// How long after the first change of a run the copier looks (one person's
/// edit is several updates).
const DEBOUNCE: Duration = Duration::from_millis(400);
/// How soon a picture neither side holds is looked for again, doubling up to
/// [`MAX_RETRY`].
const RETRY: Duration = Duration::from_secs(2);
const MAX_RETRY: Duration = Duration::from_secs(60);
/// How many ids one `haveBlobs` asks about.
const ASK_AT_ONCE: usize = 256;

/// Which pictures each document names, as of a state vector.
#[derive(Default)]
pub struct PictureRefs {
    by_doc: Mutex<HashMap<(StoreId, NodeId), (Vec<u8>, Vec<BlobId>)>>,
}

impl PictureRefs {
    /// Forget a store's documents (it closed).
    pub(crate) fn forget_store(&self, store_id: StoreId) {
        self.by_doc.lock().unwrap().retain(|(store, _), _| *store != store_id);
    }
}

impl RpcHandler {
    /// The pictures of `store_id` its documents name (`nodes` only, or every
    /// document it holds, tombstones included, when `None`), each with the
    /// documents that name it. A picture kept in another store is that
    /// store's to replicate and is left out.
    pub(crate) async fn pictures_named(&self, store_id: StoreId, nodes: Option<&[NodeId]>) -> HashMap<BlobId, Vec<NodeId>> {
        let mut named: HashMap<BlobId, Vec<NodeId>> = HashMap::new();
        let mut stale: Vec<(NodeId, Vec<u8>, Vec<u8>)> = Vec::new();
        {
            let manager = self.store_manager_handle();
            let manager = manager.read().await;
            let Ok(tree) = manager.tree(store_id) else { return named };
            let ids = match nodes {
                Some(nodes) => nodes.to_vec(),
                None => tree.ids(),
            };
            let cache = self.picture_refs().by_doc.lock().unwrap();
            for id in ids {
                let Some(doc) = tree.doc(id) else { continue };
                let sv = doc.state_vector();
                match cache.get(&(store_id, id)) {
                    Some((held, blobs)) if *held == sv => {
                        for blob in blobs {
                            named.entry(*blob).or_default().push(id);
                        }
                    }
                    _ => stale.push((id, sv, doc.save())),
                }
            }
        }
        if stale.is_empty() {
            return named;
        }
        // Reading a document's pictures projects its text: off the async
        // threads, and outside the store's lock.
        let read = tokio::task::spawn_blocking(move || {
            stale
                .into_iter()
                .map(|(id, sv, bytes)| {
                    let blobs: Vec<BlobId> =
                        NodeDoc::pictures_of(&bytes).into_iter().filter(|url| url.store == store_id).map(|url| url.blob).collect();
                    (id, sv, blobs)
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        let mut cache = self.picture_refs().by_doc.lock().unwrap();
        for (id, sv, blobs) in read {
            for blob in &blobs {
                named.entry(*blob).or_default().push(id);
            }
            cache.insert((store_id, id), (sv, blobs));
        }
        named
    }

    /// Which of `ids` a principal that reaches only `readable` (a share's
    /// member) may read: the ones a document it may read names
    /// (docs/IMAGES_CONTRACT.md "Who may read a blob"; a tombstone counts,
    /// so "Put Back" brings its pictures back).
    pub(crate) async fn readable_pictures(&self, store_id: StoreId, readable: &HashSet<NodeId>, ids: &[BlobId]) -> HashSet<BlobId> {
        let docs: Vec<NodeId> = readable.iter().copied().collect();
        let named = self.pictures_named(store_id, Some(&docs)).await;
        ids.iter().copied().filter(|id| named.contains_key(id)).collect()
    }
}

/// The other end of a link, as far as pictures go.
#[async_trait]
pub(crate) trait PictureRemote: Send + Sync + 'static {
    /// Which of `ids` the remote lacks, or holds only part of. `held` gives
    /// the length of each one this store holds whole: what a remote that
    /// keeps a picture in chunks judges a part by.
    async fn missing(&self, ids: &[BlobId], held: &HashMap<BlobId, u64>) -> anyhow::Result<HashSet<BlobId>>;
    /// A picture the remote holds: its type and bytes. `None` when it does
    /// not hold it whole (yet).
    async fn fetch(&self, id: BlobId) -> anyhow::Result<Option<(String, Vec<u8>)>>;
    /// Send a picture this store holds and the remote lacks. `named_by` are
    /// the documents here that name it (what a scoped member's upload is
    /// judged by). `Ok(false)`: refused for this picture (a read-only
    /// share), which is not retried.
    async fn send(&self, id: BlobId, named_by: &[NodeId], mime: &str, bytes: &[u8]) -> anyhow::Result<bool>;
}

/// A plain sync link's remote: the same store on another Pimble server.
pub(crate) struct PlainRemote {
    pub client: Arc<PimbleClient>,
    pub store_id: StoreId,
}

#[async_trait]
impl PictureRemote for PlainRemote {
    // A plain server never names a blob until it is whole (an upload cut
    // short is a part file, sent again from the start).
    async fn missing(&self, ids: &[BlobId], _held: &HashMap<BlobId, u64>) -> anyhow::Result<HashSet<BlobId>> {
        let mut missing = HashSet::new();
        for chunk in ids.chunks(ASK_AT_ONCE) {
            let answer = self.client.have_blobs(self.store_id, chunk).await.map_err(|e| anyhow::anyhow!("haveBlobs: {e}"))?;
            missing.extend(answer);
        }
        Ok(missing)
    }

    async fn fetch(&self, id: BlobId) -> anyhow::Result<Option<(String, Vec<u8>)>> {
        match self.client.get_blob(self.store_id, id).await {
            Ok(picture) => Ok(Some(picture)),
            Err(e) if e.to_string().contains(BLOB_NOT_HERE) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("getBlob {id}: {e}")),
        }
    }

    async fn send(&self, id: BlobId, named_by: &[NodeId], mime: &str, bytes: &[u8]) -> anyhow::Result<bool> {
        let mut last = None;
        // Judged as a write of a document naming it: the first one this
        // principal may write will do.
        for node in named_by {
            match self.client.put_blob_copy(self.store_id, *node, id, mime, bytes).await {
                Ok(_) => return Ok(true),
                Err(e) if e.is_forbidden() => last = Some(e),
                Err(e) => return Err(anyhow::anyhow!("putBlob {id}: {e}")),
            }
        }
        if let Some(e) = last {
            debug!("Store {}: the remote takes picture {} from no document naming it here ({})", self.store_id, id, e);
        }
        Ok(false)
    }
}

/// A vault link's remote: the store's encrypted twin (hosted, or a relay's),
/// where a picture is a document of its own, `blob-<id>`, under its own data
/// key wrapped per scope key (docs/IMAGES_CONTRACT.md "Replication").
pub(crate) struct VaultRemote {
    pub handler: RpcHandler,
    pub client: Arc<PimbleClient>,
    pub store_id: StoreId,
    /// The scope key the link writes under: the store key on a whole
    /// replica, the share's on a member's.
    pub link_key_id: uuid::Uuid,
    pub link_id: String,
}

#[async_trait]
impl PictureRemote for VaultRemote {
    async fn missing(&self, ids: &[BlobId], held: &HashMap<BlobId, u64>) -> anyhow::Result<HashSet<BlobId>> {
        let listing = self.client.vault_list_docs_with_blobs(self.store_id).await.map_err(|e| anyhow::anyhow!("vaultListDocs: {e}"))?;
        let heads: HashMap<BlobId, u64> = listing
            .docs
            .into_iter()
            .filter_map(|doc| match doc.doc_id {
                VaultDocId::Blob(id) => Some((id, doc.head)),
                _ => None,
            })
            .collect();
        // Missing: not there at all, or (for one this store holds) with fewer
        // entries than the picture takes, which is an upload that stopped
        // half way. Its sender, or anyone holding it whole, finishes it.
        Ok(ids
            .iter()
            .copied()
            .filter(|id| match (heads.get(id), held.get(id)) {
                (None | Some(0), _) => true,
                (Some(head), Some(len)) => *head < protocol::picture_chunk_count(*len as usize),
                (Some(_), None) => false,
            })
            .collect())
    }

    async fn fetch(&self, id: BlobId) -> anyhow::Result<Option<(String, Vec<u8>)>> {
        let fetched = match self.client.vault_fetch(self.store_id, VaultDocId::Blob(id), 0).await {
            Ok(fetched) => fetched,
            // Not in this account's reach yet (the owner's upkeep has not
            // added it to the share): looked for again later.
            Err(e) if crate::vault_link::is_refusal(&e) => return Ok(None),
            Err(e) => return Err(anyhow::anyhow!("vaultFetch picture {id}: {e}")),
        };
        if fetched.head == 0 {
            return Ok(None);
        }
        let scope = self.handler.keystore().store_keys(self.store_id).await;
        match protocol::open_picture(self.store_id, id, &fetched, &scope) {
            Ok(picture) => Ok(picture),
            // No wrap this device can open yet (its share's key is on its
            // way, or the upkeep has not wrapped it): looked for again.
            Err(protocol::Unopened::NoKey(_)) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("picture {id}: {e}")),
        }
    }

    async fn send(&self, id: BlobId, named_by: &[NodeId], mime: &str, bytes: &[u8]) -> anyhow::Result<bool> {
        let Some(&node) = named_by.first() else { return Ok(false) };
        // A document there already got only part of the picture: finished
        // under the key it was begun with, from where it stopped.
        match self.client.vault_fetch(self.store_id, VaultDocId::Blob(id), 0).await {
            Ok(fetched) if fetched.head > 0 => return self.finish(id, &fetched, mime, bytes).await,
            Ok(_) => {}
            Err(e) if crate::vault_link::is_refusal(&e) => {}
            Err(e) => return Err(anyhow::anyhow!("vaultFetch picture {id}: {e}")),
        }
        let scope_key_ids = {
            let manager = self.handler.store_manager_handle();
            let manager = manager.read().await;
            let partial = !manager.scope_roots(self.store_id).is_empty();
            let tree = manager.tree(self.store_id).map_err(|e| anyhow::anyhow!("store unavailable: {e}"))?;
            let mut ids = Vec::new();
            for named in named_by {
                for key in protocol::scope_key_ids(tree, *named, (!partial).then_some(self.link_key_id), [self.link_key_id]) {
                    if !ids.contains(&key) {
                        ids.push(key);
                    }
                }
            }
            ids
        };
        let scope = self.handler.keystore().store_keys(self.store_id).await;
        let Some((dek_id, dek, keys)) = protocol::new_picture_key(self.store_id, id, &scope_key_ids, &scope) else {
            return Err(anyhow::anyhow!("no scope key held for store {} to seal picture {id} under", self.store_id));
        };
        let mut keys = Some(keys);
        for chunk in protocol::picture_chunks(mime, bytes) {
            let sealed = protocol::seal_picture(self.store_id, id, dek_id, &dek, &chunk);
            // The first entry creates the document with its wraps, and names
            // the node it is for, which is what admits a member's upload
            // (the picture joins every scope of theirs holding that node).
            let first = keys.is_some();
            let appended = self
                .client
                .vault_append_with_keys(self.store_id, VaultDocId::Blob(id), sealed, Some(self.link_id.clone()), first.then_some(node), keys.take())
                .await;
            match appended {
                Ok(_) => {}
                Err(e) if crate::vault_link::is_refusal(&e) => {
                    debug!("Store {}: picture {} refused by the remote ({})", self.store_id, id, e);
                    return Ok(false);
                }
                Err(e) => return Err(anyhow::anyhow!("vaultAppend picture {id}: {e}")),
            }
        }
        Ok(true)
    }
}

impl VaultRemote {
    /// Append what a picture's document lacks, under the data key its wraps
    /// hold (`pimble_rpc::vault::continue_picture`). `Ok(false)` when no wrap
    /// opens here or the remote refuses: someone else's to finish.
    async fn finish(&self, id: BlobId, fetched: &pimble_rpc::VaultFetchResponse, mime: &str, bytes: &[u8]) -> anyhow::Result<bool> {
        let scope = self.handler.keystore().store_keys(self.store_id).await;
        let progress = match protocol::picture_progress(self.store_id, id, fetched, &scope) {
            Ok(progress) => progress,
            Err(_) => return Ok(false),
        };
        if matches!(progress, protocol::PictureProgress::Whole(..)) {
            return Ok(true);
        }
        let (Some(keys), Some(dek)) = (fetched.keys.as_ref(), fetched.keys.as_ref().and_then(|keys| protocol::picture_key(self.store_id, id, keys, &scope))) else {
            return Ok(false);
        };
        let chunks = protocol::continue_picture(&progress, mime, bytes);
        info!("Store {}: finishing picture {} ({} of its entries were missing)", self.store_id, id, chunks.len());
        for chunk in chunks {
            let sealed = protocol::seal_picture(self.store_id, id, keys.dek_id, &dek, &chunk);
            match self.client.vault_append_with_keys(self.store_id, VaultDocId::Blob(id), sealed, Some(self.link_id.clone()), None, None).await {
                Ok(_) => {}
                Err(e) if crate::vault_link::is_refusal(&e) => return Ok(false),
                Err(e) => return Err(anyhow::anyhow!("vaultAppend picture {id}: {e}")),
            }
        }
        Ok(true)
    }
}

/// What starts a look: everything (after a reconcile), or some documents.
enum Kick {
    All,
    Nodes(Vec<NodeId>),
}

/// A running copier. Dropping it stops it.
pub(crate) struct Copier {
    kick: mpsc::UnboundedSender<Kick>,
    join: JoinHandle<()>,
}

impl Copier {
    /// Look at every document's pictures (the link has just reconciled).
    pub(crate) fn all(&self) {
        let _ = self.kick.send(Kick::All);
    }
}

impl Drop for Copier {
    fn drop(&mut self) {
        self.join.abort();
    }
}

/// Start copying `store_id`'s pictures to and from `remote`, for as long as
/// the returned [`Copier`] lives. It does nothing until it is kicked
/// ([`Copier::all`]) or a document of the store changes here.
pub(crate) async fn spawn_copier(handler: &RpcHandler, store_id: StoreId, remote: impl PictureRemote) -> Copier {
    let (kick, kicks) = mpsc::unbounded_channel();
    let local = handler.subscribe_local_changes().await;
    let join = tokio::spawn(run(handler.clone(), store_id, remote, kicks, local));
    Copier { kick, join }
}

/// Documents whose pictures are due a look: all, or these.
#[derive(Default)]
struct Due {
    all: bool,
    nodes: HashSet<NodeId>,
}

impl Due {
    fn add(&mut self, kick: Kick) {
        match kick {
            Kick::All => self.all = true,
            Kick::Nodes(nodes) => self.nodes.extend(nodes),
        }
    }

    fn is_empty(&self) -> bool {
        !self.all && self.nodes.is_empty()
    }
}

async fn run(handler: RpcHandler, store_id: StoreId, remote: impl PictureRemote, mut kicks: mpsc::UnboundedReceiver<Kick>, mut local: broadcast::Receiver<LocalChange>) {
    // What is due a look, and when; what a look left owed, and when to look
    // at it again.
    let mut due = Due::default();
    let mut at: Option<Instant> = None;
    let mut owed = Due::default();
    let mut retry_at: Option<Instant> = None;
    let mut retry_wait = RETRY;
    loop {
        let wake = [at, retry_at].into_iter().flatten().min();
        tokio::select! {
            kick = kicks.recv() => {
                let Some(kick) = kick else { return };
                due.add(kick);
                at.get_or_insert(Instant::now() + DEBOUNCE);
                retry_wait = RETRY;
            }
            change = local.recv() => {
                match change {
                    Ok(LocalChange::Store(notif)) if notif.store_id == store_id => {
                        if let Some(node) = document_of(&notif.change_kind) {
                            due.add(Kick::Nodes(vec![node]));
                            at.get_or_insert(Instant::now() + DEBOUNCE);
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        due.add(Kick::All);
                        at.get_or_insert(Instant::now() + DEBOUNCE);
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
            _ = crate::share::until(wake) => {
                let now = Instant::now();
                if retry_at.is_some_and(|r| r <= now) {
                    retry_at = None;
                    let again = std::mem::take(&mut owed);
                    due.all |= again.all;
                    due.nodes.extend(again.nodes);
                    at = Some(now);
                }
                if at.is_none_or(|a| a > now) {
                    continue;
                }
                at = None;
                if due.is_empty() {
                    continue;
                }
                let look = std::mem::take(&mut due);
                let nodes: Option<Vec<NodeId>> = (!look.all).then(|| look.nodes.into_iter().collect());
                let left = match look_once(&handler, store_id, &remote, nodes.as_deref()).await {
                    Ok(left) => Due { all: false, nodes: left },
                    Err(e) => {
                        warn!("Pictures of store {}: {}; looking again in {:?}", store_id, e, retry_wait);
                        Due { all: nodes.is_none(), nodes: nodes.unwrap_or_default().into_iter().collect() }
                    }
                };
                if left.is_empty() {
                    retry_wait = RETRY;
                } else {
                    owed.all |= left.all;
                    owed.nodes.extend(left.nodes);
                    retry_at = Some(Instant::now() + retry_wait);
                    retry_wait = (retry_wait * 2).min(MAX_RETRY);
                }
            }
        }
    }
}

/// The document a local notification is about.
fn document_of(kind: &StoreChangeKind) -> Option<NodeId> {
    match kind {
        StoreChangeKind::NodeCreated { node_id, .. }
        | StoreChangeKind::NodeDeleted { node_id, .. }
        | StoreChangeKind::NodeMoved { node_id, .. }
        | StoreChangeKind::MetadataUpdated { node_id }
        | StoreChangeKind::AttachmentChanged { node_id, .. }
        | StoreChangeKind::ContentUpdated { node_id } => Some(*node_id),
        StoreChangeKind::TreeStructure { node_ids } => node_ids.first().copied(),
        _ => None,
    }
}

/// One look: the pictures `nodes` name (every document's when `None`), each
/// copied to whichever side lacks it. Answers the documents whose pictures
/// are still owed (neither side holds one yet, or a copy failed).
pub(crate) async fn look_once(handler: &RpcHandler, store_id: StoreId, remote: &impl PictureRemote, nodes: Option<&[NodeId]>) -> anyhow::Result<HashSet<NodeId>> {
    let named = handler.pictures_named(store_id, nodes).await;
    let mut owed = HashSet::new();
    if named.is_empty() {
        return Ok(owed);
    }
    let ids: Vec<BlobId> = named.keys().copied().collect::<BTreeSet<_>>().into_iter().collect();
    let (blobs, may_send) = {
        let manager = handler.store_manager_handle();
        let manager = manager.read().await;
        (manager.blobs(store_id)?, manager.store_access(store_id).allows_write())
    };
    let held = blobs.sizes(&ids).await;
    let here_missing: HashSet<BlobId> = ids.iter().copied().filter(|id| !held.contains_key(id)).collect();
    let there_missing = remote.missing(&ids, &held).await?;
    let (mut fetched, mut sent) = (0usize, 0usize);
    for id in ids {
        let named_by = &named[&id];
        match (here_missing.contains(&id), there_missing.contains(&id)) {
            (true, true) => owed.extend(named_by.iter().copied()),
            (true, false) => match remote.fetch(id).await {
                Ok(Some((mime, bytes))) => match blobs.put_as(id, &mime, bytes).await {
                    Ok(_) => fetched += 1,
                    Err(e) => warn!("Store {}: picture {} from the remote was not kept: {}", store_id, id, e),
                },
                Ok(None) => owed.extend(named_by.iter().copied()),
                Err(e) => {
                    warn!("Store {}: picture {} could not be fetched: {}", store_id, id, e);
                    owed.extend(named_by.iter().copied());
                }
            },
            (false, true) if may_send => {
                let Some(blob) = blobs.get(id).await? else {
                    owed.extend(named_by.iter().copied());
                    continue;
                };
                match remote.send(id, named_by, blob.mime.as_str(), &blob.bytes).await {
                    Ok(true) => sent += 1,
                    Ok(false) => {}
                    Err(e) => {
                        warn!("Store {}: picture {} could not be sent: {}", store_id, id, e);
                        owed.extend(named_by.iter().copied());
                    }
                }
            }
            _ => {}
        }
    }
    if fetched + sent > 0 {
        info!("Pictures of store {}: {} fetched, {} sent", store_id, fetched, sent);
    }
    // A picture just sent may be under a share: the owner's upkeep wraps its
    // key for the share and adds it to the share's scope, now rather than at
    // its next pass (a no-op on a store with no vault link or no shares).
    if sent > 0 {
        handler.share_command(store_id, crate::share::ShareCommand::PassNow(None)).await;
    }
    Ok(owed)
}
