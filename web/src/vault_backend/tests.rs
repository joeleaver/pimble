//! The vault as a backend: the conformance suite every backend runs
//! (`pimble_api::conformance`, the same checks the RPC backend passes over a
//! real server), alone and behind a router, and the router's own word about
//! an encrypted store (its gate, and a store whose owner's computer is off).
//!
//! The server's side is a log kept in memory ([`Logs`]): it numbers what is
//! appended, as `vaultAppend` does, and never reads it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use futures::executor::block_on;
use pimble_api::conformance::{self, Fixture};
use pimble_api::router::{OWNER_OFFLINE_REFUSAL, Write};
use pimble_api::{ApiError, Backend, NodeInit, Router};
use pimble_client::ClientError;
use pimble_core::{node_types, NodeId, Place, StoreAccess, StoreId};
use pimble_rpc::{VaultDocId, VaultDocKeys, VaultFetchResponse};

use super::*;

/// A hosted twin's logs: each append numbered per document, nothing read.
#[derive(Default)]
struct Logs {
    logs: Mutex<HashMap<(StoreId, VaultDocId), u64>>,
    appends: AtomicUsize,
}

#[async_trait]
impl VaultWire for Logs {
    async fn append(
        &self,
        store_id: StoreId,
        doc_id: VaultDocId,
        _blob: String,
        _client_id: Option<String>,
        _parent_id: Option<NodeId>,
        _discussion_host: Option<NodeId>,
        _keys: Option<VaultDocKeys>,
    ) -> Result<u64, ClientError> {
        self.appends.fetch_add(1, Ordering::SeqCst);
        let mut logs = self.logs.lock().unwrap();
        let head = logs.entry((store_id, doc_id)).or_default();
        *head += 1;
        Ok(*head)
    }

    async fn snapshot(&self, _store_id: StoreId, _doc_id: VaultDocId, _upto_seq: u64, _blob: String) -> Result<(), ClientError> {
        Ok(())
    }

    async fn fetch(&self, _store_id: StoreId, _doc_id: VaultDocId, _after_seq: u64) -> Result<VaultFetchResponse, ClientError> {
        Err(ClientError::Rpc("these logs are written, never read".into()))
    }
}

/// The endpoint every store is served at, and whether it is up.
struct Table {
    logs: Arc<Logs>,
    up: AtomicBool,
}

impl WireTable for Table {
    fn wire(&self, _store: StoreId) -> Option<Arc<dyn VaultWire>> {
        self.up.load(Ordering::SeqCst).then(|| self.logs.clone() as Arc<dyn VaultWire>)
    }
}

struct Page {
    backend: VaultBackend,
    table: Arc<Table>,
    store: StoreId,
    root: NodeId,
}

fn page() -> Page {
    let mut vault = VaultClient::new("page".to_string());
    let store = StoreId::new();
    let root = vault.hold_new_store(store, "Vault");
    let table = Arc::new(Table { logs: Arc::default(), up: AtomicBool::new(true) });
    let backend = VaultBackend::new(vault, table.clone());
    Page { backend, table, store, root }
}

fn appends(page: &Page) -> usize {
    page.table.logs.appends.load(Ordering::SeqCst)
}

fn document(title: &str) -> NodeInit {
    NodeInit { node_type: node_types::DOCUMENT.into(), title: title.into(), content: None, custom: Vec::new() }
}

// ── Conformance ─────────────────────────────────────────────────────────────

/// The vault gives the RPC backend's answers and errors to the same calls;
/// what it cannot do is stated, not faked.
#[test]
fn the_vault_conforms() {
    let page = page();
    let fx = Fixture { store: page.store, root: page.root };
    let passed = block_on(conformance::run(&page.backend, fx, CAPABILITIES));
    assert_eq!(passed.len(), 12, "every check ran: {passed:?}");
    assert!(appends(&page) > 0, "every write went out encrypted through the wire");
}

#[test]
fn the_vault_behind_a_router_conforms() {
    let page = page();
    let fx = Fixture { store: page.store, root: page.root };
    let router = Router::new(page.backend);
    let passed = block_on(conformance::run(&router, fx, CAPABILITIES));
    assert_eq!(passed.len(), 12, "every check ran: {passed:?}");
}

/// The differences from a server's store, said explicitly.
#[test]
fn an_encrypted_store_says_what_it_cannot_do() {
    let page = page();
    let caps = page.backend.capabilities(page.store);
    assert!(caps.encrypted);
    assert!(!caps.mounts && !caps.pictures && !caps.sync_links && !caps.semantic_search && !caps.index_rebuild);
    let refused = block_on(page.backend.put_blob(page.store, page.root, "image/png", b"png")).unwrap_err();
    assert_eq!(refused, ApiError::not_available(pimble_app::pictures::ENCRYPTED_IN_BROWSER));
}

// ── The router's word ───────────────────────────────────────────────────────

/// A reader's write is refused by the router before the vault is asked,
/// with the sentence a plain store's reader gets.
#[test]
fn a_reader_is_refused_by_the_router_with_nothing_sent() {
    let page = page();
    let store = page.store;
    let root = page.root;
    let router = Router::new(page.backend);
    let wire = router.backend(0);
    let a = block_on(wire.create_node(store, root, document("A"), Place::Last)).unwrap();

    router.set_access(store, Some(StoreAccess::Read));
    let before = page.table.logs.appends.load(Ordering::SeqCst);
    assert_eq!(block_on(router.create_node(store, root, document("B"), Place::Last)).unwrap_err(), ApiError::ReadOnly);
    assert_eq!(block_on(router.delete(store, a)).unwrap_err(), ApiError::ReadOnly);
    assert_eq!(block_on(router.apply(store, a, Vec::new(), "page")).unwrap_err(), ApiError::ReadOnly);
    assert_eq!(page.table.logs.appends.load(Ordering::SeqCst), before, "nothing was appended");
    // Reading is not refused.
    assert_eq!(block_on(router.fetch_one(store, a)).unwrap().metadata.title, "A");

    // A commenter makes discussions and nothing else.
    router.set_access(store, Some(StoreAccess::Comment));
    assert_eq!(router.gate(store, Write::Structure).unwrap_err(), ApiError::CommentOnly);
    assert!(router.gate(store, Write::Discussion).is_ok());
}

/// A commenter's edit of a document that is not a discussion is refused by
/// the vault, judging the document, as the server judges an `applyEdit`.
#[test]
fn a_commenter_edits_discussions_and_nothing_else() {
    let mut vault = VaultClient::new("page".to_string());
    let store = StoreId::new();
    let root = vault.hold_new_store(store, "Vault");
    let table = Arc::new(Table { logs: Arc::default(), up: AtomicBool::new(true) });
    let backend = VaultBackend::new(vault, table.clone());
    let note = block_on(backend.create_node(store, root, document("Note"), Place::Last)).unwrap();
    let talk = block_on(backend.create_attachment(
        store,
        note,
        NodeInit { node_type: node_types::DISCUSSION.into(), title: "Talk".into(), content: None, custom: Vec::new() },
    ))
    .unwrap();
    block_on(backend.shared().lock()).grant_whole(store, "commenter");

    let edit = |id: NodeId| {
        let node = block_on(backend.fetch_one(store, id)).unwrap();
        let mut doc = pimble_crdt::NodeDoc::load(&node.content).unwrap();
        let sv = doc.state_vector();
        doc.replace_plain_text("words").unwrap();
        doc.diff_since(&sv).unwrap()
    };
    assert_eq!(block_on(backend.apply(store, note, edit(note), "page")).unwrap_err(), ApiError::CommentOnly);
    block_on(backend.apply(store, talk, edit(talk), "page")).expect("a discussion is a commenter's to write");
}

/// While a store's owner's computer is off: no change but what the editor
/// already has on screen, which is kept for later; everything handed out
/// is to be read.
#[test]
fn a_held_store_whose_owner_is_away_is_read_and_keeps_its_edits() {
    let page = page();
    let (store, root) = (page.store, page.root);
    let a = block_on(page.backend.create_node(store, root, document("A"), Place::Last)).unwrap();
    let shared = page.backend.shared();
    let table = page.table.clone();
    let router = Router::new(page.backend);

    table.up.store(false, Ordering::SeqCst);
    block_on(shared.lock()).owner_away(store);
    let row = block_on(shared.lock()).away_row(store).expect("away");
    router.set_away(store, Some(row));

    let refused = block_on(router.create_node(store, root, document("B"), Place::Last)).unwrap_err();
    assert_eq!(refused, ApiError::OwnerOffline { sentence: OWNER_OFFLINE_REFUSAL.into() });
    assert_eq!(block_on(router.fetch_one(store, a)).unwrap().access, StoreAccess::Read, "handed out to be read");

    // An edit already on screen is merged and kept, not refused.
    let node = block_on(router.fetch_one(store, a)).unwrap();
    let mut doc = pimble_crdt::NodeDoc::load(&node.content).unwrap();
    let sv = doc.state_vector();
    doc.replace_plain_text("typed before the news arrived").unwrap();
    block_on(router.apply(store, a, doc.diff_since(&sv).unwrap(), "page")).expect("kept");
    assert!(block_on(shared.lock()).has_unsent(store, a), "kept for the next connection");
    let kept = block_on(router.fetch_one(store, a)).unwrap();
    assert_eq!(pimble_crdt::NodeDoc::load(&kept.content).unwrap().text(), "typed before the news arrived");
}

/// A store never opened here whose owner is away is listed from the
/// account's row and opens as an empty folder, with no error.
#[test]
fn a_store_never_opened_whose_owner_is_away_opens_empty() {
    let page = page();
    let shared = page.backend.shared();
    let router = Router::new(page.backend);
    let elsewhere = StoreId::new();
    {
        let mut vault = block_on(shared.lock());
        vault.grant_whole(elsewhere, "editor");
        vault.owner_away(elsewhere);
    }
    let row = block_on(shared.lock()).away_row(elsewhere).expect("listed from the account's row");
    let placeholder = row.root_node_id;
    router.set_away(elsewhere, Some(row));

    let (at, children) = block_on(router.children(elsewhere, placeholder)).expect("an empty folder, not an error");
    assert_eq!(at, elsewhere);
    assert!(children.is_empty());
    let folder = block_on(router.fetch_one(elsewhere, placeholder)).expect("the placeholder root");
    assert_eq!(folder.access, StoreAccess::Read);
    assert!(block_on(router.list_deleted(elsewhere)).unwrap().is_empty());
    assert!(block_on(router.list_stores()).unwrap().iter().any(|s| s.id == elsewhere), "listed");
    // An edit has nowhere to wait: the store is not held here.
    let refused = block_on(router.apply(elsewhere, placeholder, Vec::new(), "page")).unwrap_err();
    assert!(matches!(refused, ApiError::OwnerOffline { .. }));
}

/// Closing an encrypted store lets go of it in the page (it answered "not
/// available" until 2026-11-02).
#[test]
fn an_encrypted_store_closes() {
    let page = page();
    let shared = page.backend.shared();
    let event = block_on(shared.lock()).close(page.store);
    assert!(matches!(event, pimble_app::protocol::BackendEvent::StoreClosed { store_id } if store_id == page.store));
    let vault = block_on(shared.lock());
    assert!(vault.is_closed(page.store));
    assert!(!vault.owns(page.store));
    assert!(vault.held_stores().is_empty());
}

// ── Listening through the API (docs/PLUGINS_CONTRACT.md wave 3) ─────────────

/// A plugin's `Pimble` keeps copies of its own, so it subscribes: it hears
/// every write of the store, the app's and its own, each with the update of
/// the document it is about; the app's views hear only what a plugin wrote
/// (their own commands show what they wrote themselves).
#[test]
fn a_subscription_hears_every_write_and_the_app_hears_the_plugins() {
    use futures::{FutureExt, StreamExt};
    let page = page();
    let plugins = page.backend.for_plugins();
    let mut changes = block_on(plugins.subscribe(page.store)).unwrap();

    // The app's own write: heard by the plugin, not shown again to the app.
    let mine = block_on(page.backend.create_node(page.store, page.root, document("Mine"), Place::Last)).unwrap();
    let mut heard = Vec::new();
    while let Some(Some(change)) = changes.next().now_or_never() {
        heard.push(change);
    }
    assert!(heard.iter().any(|c| pimble_api::backend::document_of(&c.kind) == Some(mine) && c.update.is_some()), "{heard:?}");
    assert!(heard.iter().all(|c| c.source.as_deref() == Some("page")), "{heard:?}");
    let shown = block_on(page.backend.shared().lock()).pump();
    assert!(shown.is_empty(), "the app's own write is its commands' to show: {shown:?}");

    // The plugin's write: heard by the plugin, and shown to the app's views.
    let theirs = block_on(plugins.create_node(page.store, page.root, document("Theirs"), Place::Last)).unwrap();
    let mut heard = Vec::new();
    while let Some(Some(change)) = changes.next().now_or_never() {
        heard.push(change);
    }
    assert!(heard.iter().any(|c| pimble_api::backend::document_of(&c.kind) == Some(theirs) && c.source.as_deref() == Some("plugin")), "{heard:?}");
    let shown = block_on(page.backend.shared().lock()).pump();
    assert!(
        shown.iter().any(|e| matches!(e, pimble_app::protocol::BackendEvent::RemoteStoreChange { change_kind: pimble_rpc::StoreChangeKind::TreeStructure { node_ids }, .. } if node_ids.contains(&theirs))),
        "{shown:?}"
    );
    // A plugin's content write reaches the editors holding the document.
    let doc = pimble_crdt::NodeDoc::load(&block_on(plugins.fetch_one(page.store, theirs)).unwrap().content).unwrap();
    let mut edited = pimble_crdt::NodeDoc::load(&doc.save()).unwrap();
    let update = edited.replace_plain_text("words").unwrap();
    block_on(plugins.apply(page.store, theirs, update, "plugin:kanban-viewer")).unwrap();
    let shown = block_on(page.backend.shared().lock()).pump();
    assert!(shown.iter().any(|e| matches!(e, pimble_app::protocol::BackendEvent::RemoteChanges { node_id, .. } if *node_id == theirs)), "{shown:?}");
    let mut heard = Vec::new();
    while let Some(Some(change)) = changes.next().now_or_never() {
        heard.push(change);
    }
    assert!(heard.iter().any(|c| c.source.as_deref() == Some("plugin:kanban-viewer")), "{heard:?}");
}
