//! One store folder opened on several computers through a sync tool
//! (docs/STORE_FOLDERS_CONTRACT.md): merge before write, merging what the
//! folder holds, conflict copies, per-computer state outside the folder, and
//! the randomized proof that two computers with a careless sync tool between
//! them end up holding the same store.
//!
//! The randomized test sweeps seeds like the tree's `three_replicas_converge`:
//! `PIMBLE_FOLDER_SEEDS=1,5` replays chosen seeds, `PIMBLE_FOLDER_SEEDS=300`
//! (a bare count) runs seeds 0..300; the default is a quick set.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use pimble_core::{Node, NodeId, Place, StoreAccess, StoreManifest};
use pimble_crdt::NodeDoc;
use pimble_store::{classify_node_file, conflict_copy_name, ConflictStyle, LocalStore, NodeFile, SyncConfig, SyncMode};
use pimble_host::disk::testfs as tfs;
use tempfile::TempDir;

// ── Helpers ──────────────────────────────────────────────────────────────

fn node_file(store: &Path, id: NodeId) -> PathBuf {
    store.join("nodes").join(format!("{id}.yrs"))
}

fn text_in_file(store: &Path, id: NodeId) -> String {
    NodeDoc::load(&tfs::read(node_file(store, id)).unwrap()).unwrap().text()
}

fn text_held(store: &LocalStore, id: NodeId) -> String {
    store.tree().doc(id).unwrap().text()
}

/// A store created at `<dir>/s.pimble`, with one note, written and closed.
async fn made_store(dir: &Path) -> (PathBuf, NodeId) {
    let path = dir.join("s.pimble");
    let mut store = LocalStore::create(&path, "Folder").await.unwrap();
    let root = store.root_node_id();
    let (note, _) = store.create_node(Node::document("Note"), Some(root)).unwrap();
    store.node_doc(note).unwrap().append_markdown("start").unwrap();
    store.flush().await.unwrap();
    (path, note)
}

fn copy_dir(from: &Path, to: &Path) {
    tfs::copy_dir(from, to).unwrap();
}

fn link(last_seq: &[(&str, u64)]) -> SyncConfig {
    SyncConfig {
        remote: pimble_core::RemoteEndpoint { url: "https://pimble.example/rpc".parse().unwrap(), auth: pimble_core::AuthMethod::None },
        last_sync: None,
        mode: SyncMode::Vault,
        via_relay: false,
        last_seq: last_seq.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        vault_key_id: None,
        access: StoreAccess::Full,
        shared_by: None,
        read_only_roots: Vec::new(),
        comment_roots: Vec::new(),
    }
}

// ── Rule 2: merge before write ───────────────────────────────────────────

/// Two processes on one folder (two computers whose sync tool has already
/// delivered): each writes the note after an edit of its own, and the file
/// ends up with both, because the second write merges the first.
#[tokio::test]
async fn a_write_never_loses_what_another_computer_wrote() {
    let dir = TempDir::new().unwrap();
    let (path, note) = made_store(dir.path()).await;
    let mut a = LocalStore::open_in(&path, Some(&dir.path().join("state-a"))).await.unwrap();
    let mut b = LocalStore::open_in(&path, Some(&dir.path().join("state-b"))).await.unwrap();

    a.node_doc(note).unwrap().append_markdown("from a").unwrap();
    a.flush().await.unwrap();
    b.node_doc(note).unwrap().append_markdown("from b").unwrap();
    b.flush().await.unwrap();

    let on_disk = text_in_file(&path, note);
    assert!(on_disk.contains("from a") && on_disk.contains("from b"), "both edits are in the file: {on_disk:?}");
    assert!(text_held(&b, note).contains("from a"), "the writer took the other's edit in before writing");
    let merges = b.take_folder_merges();
    assert_eq!(merges.len(), 1, "what the merge brought in waits to be announced");
    assert!(merges[0].node_id == note && merges[0].effect.content);
}

/// The manifest too, field by field: two partial replicas on one folder each
/// add a scope root, and the file keeps both.
#[tokio::test]
async fn the_manifest_is_merged_field_by_field() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("p.pimble");
    let first = NodeId::new();
    let store = LocalStore::create_replica_with_scope(&path, pimble_core::StoreId::new(), "Shared", first, vec![first]).await.unwrap();
    drop(store);
    let mut a = LocalStore::open_in(&path, Some(&dir.path().join("state-a"))).await.unwrap();
    let mut b = LocalStore::open_in(&path, Some(&dir.path().join("state-b"))).await.unwrap();
    let (r1, r2) = (NodeId::new(), NodeId::new());
    a.add_scope_root(r1).await.unwrap();
    b.add_scope_root(r2).await.unwrap();

    // The tree's root first, the rest in the one order both arrive at.
    let mut rest = vec![r1, r2];
    rest.sort_by_key(|r| r.to_string());
    let expected = vec![first, rest[0], rest[1]];
    let on_disk: StoreManifest = serde_json::from_slice(&tfs::read(path.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(on_disk.scope_roots, expected);
    a.absorb_folder().await.unwrap();
    assert_eq!(a.scope_roots(), expected.as_slice());
    assert!(!a.has_unwritten(), "a holds what the file holds");
}

// ── Rule 3: what the folder holds is merged ──────────────────────────────

/// Another computer's write is merged once; this computer's own writes, seen
/// again, are not merged back.
#[tokio::test]
async fn an_external_change_is_merged_once_and_own_writes_are_not() {
    let dir = TempDir::new().unwrap();
    let (path, note) = made_store(dir.path()).await;
    let mut a = LocalStore::open_in(&path, Some(&dir.path().join("state-a"))).await.unwrap();
    let mut b = LocalStore::open_in(&path, Some(&dir.path().join("state-b"))).await.unwrap();

    a.node_doc(note).unwrap().append_markdown("news").unwrap();
    a.flush().await.unwrap();

    b.absorb_folder().await.unwrap();
    assert!(text_held(&b, note).contains("news"));
    let merges = b.take_folder_merges();
    assert_eq!(merges.len(), 1);
    assert!(merges[0].effect.content && !merges[0].effect.structure);
    assert!(!b.has_unwritten(), "the file holds everything b holds: nothing to write back");

    b.absorb_folder().await.unwrap();
    assert!(b.take_folder_merges().is_empty(), "the same file again is nothing new");

    b.node_doc(note).unwrap().append_markdown("b's own").unwrap();
    b.flush().await.unwrap();
    b.absorb_folder().await.unwrap();
    assert!(b.take_folder_merges().is_empty(), "b's own write is not merged back");

    // A new node another computer made joins the tree, and a document older
    // than what is held (a sync tool's stale copy) is written back.
    let root = a.root_node_id();
    let (made, _) = a.create_node(Node::document("Made on a"), Some(root)).unwrap();
    a.flush().await.unwrap();
    b.absorb_folder().await.unwrap();
    assert!(b.get_node(made).is_ok(), "the new node is in b's tree");
    assert!(b.take_folder_merges().iter().any(|m| m.node_id == made && m.effect.structure));

    let stale = tfs::read(node_file(&path, note)).unwrap();
    b.node_doc(note).unwrap().append_markdown("newer").unwrap();
    b.flush().await.unwrap();
    tfs::write(node_file(&path, note), &stale).unwrap();
    b.absorb_folder().await.unwrap();
    assert!(b.has_unwritten(), "b holds more than the stale file");
    b.flush().await.unwrap();
    assert!(text_in_file(&path, note).contains("newer"), "written back");
}

// ── Rule 4: conflict copies ──────────────────────────────────────────────

#[test]
fn conflict_copies_are_named_by_every_tool_style() {
    let id = NodeId::new();
    assert_eq!(classify_node_file(&format!("{id}.yrs")), NodeFile::Document(id));
    for style in ConflictStyle::ALL {
        for n in 0..3 {
            let name = conflict_copy_name(id, style, n);
            assert_eq!(classify_node_file(&name), NodeFile::Conflict(id), "{name}");
        }
    }
    for other in [format!("{id}.tmp"), format!("{id}.0123456789ab.tmp"), format!(".syncthing.{id}.yrs.tmp"), "notes.yrs".to_string(), format!("{id}.yrs.part")] {
        assert_eq!(classify_node_file(&other), NodeFile::Other, "{other}");
    }
}

/// A conflict copy in each tool's naming is merged into its document, the
/// merged document written, the copy removed: while the store is open, and
/// at open. A file named like one that is not a document is left alone.
#[tokio::test]
async fn conflict_copies_are_merged_and_removed() {
    for style in ConflictStyle::ALL {
        let dir = TempDir::new().unwrap();
        let (path, note) = made_store(dir.path()).await;
        let mut store = LocalStore::open_in(&path, Some(&dir.path().join("state"))).await.unwrap();

        // The other computer's copy, with an edit of its own.
        let mut theirs = NodeDoc::load(&tfs::read(node_file(&path, note)).unwrap()).unwrap();
        theirs.append_markdown("kept as a conflict copy").unwrap();
        let copy = path.join("nodes").join(conflict_copy_name(note, style, 0));
        tfs::write(&copy, theirs.save()).unwrap();
        let junk = path.join("nodes").join(conflict_copy_name(note, style, 1));
        tfs::write(&junk, b"not a document").unwrap();

        store.absorb_folder().await.unwrap();
        assert!(!tfs::exists(&copy), "{style:?}: the copy is removed");
        assert!(tfs::exists(&junk), "{style:?}: a file that is not a document is left alone");
        assert!(text_held(&store, note).contains("kept as a conflict copy"), "{style:?}");
        assert!(text_in_file(&path, note).contains("kept as a conflict copy"), "{style:?}: the merged document is written");
        drop(store);

        // At open.
        let mut later = NodeDoc::load(&tfs::read(node_file(&path, note)).unwrap()).unwrap();
        later.append_markdown("found at open").unwrap();
        let copy = path.join("nodes").join(conflict_copy_name(note, style, 2));
        tfs::write(&copy, later.save()).unwrap();
        let store = LocalStore::open_in(&path, Some(&dir.path().join("state"))).await.unwrap();
        assert!(!tfs::exists(&copy), "{style:?}: removed at open");
        assert!(text_held(&store, note).contains("found at open"));
        assert!(text_in_file(&path, note).contains("found at open"));
    }
}

// ── Rule 1: per-computer state leaves the folder ─────────────────────────

/// The first computer to open the folder with this version takes the state
/// the folder holds as its own and moves it out; a computer opening it after
/// that never reads what an older Pimble elsewhere wrote there again.
#[tokio::test]
async fn per_computer_state_moves_out_once() {
    let dir = TempDir::new().unwrap();
    let (path, _) = made_store(dir.path()).await;
    // What an older Pimble kept in the folder.
    let mut old = serde_json::to_value(link(&[("node:abc", 7)])).unwrap();
    old["last_sync"] = serde_json::json!("2026-11-18T10:00:00Z");
    tfs::write(path.join("sync.json"), serde_json::to_vec(&old).unwrap()).unwrap();
    tfs::write(path.join("vault-link.json"), b"{\"pushed_sv\":{}}").unwrap();
    tfs::write(path.join("people.json"), b"{\"scopes\":[]}").unwrap();
    tfs::create_dir_all(path.join("index").join("rhypedb")).unwrap();
    tfs::write(path.join("index").join("rhypedb").join("data"), b"x").unwrap();

    let state_root = dir.path().join("state-a");
    let a = LocalStore::open_in(&path, Some(&state_root)).await.unwrap();
    let state = state_root.join(a.id.to_string());
    assert_eq!(a.state_dir(), Some(state.as_path()));
    let config = a.read_sync_config().await.unwrap().unwrap();
    assert_eq!(config.last_seq.get("node:abc"), Some(&7), "the read positions are this computer's now");
    assert!(config.last_sync.is_some());
    let folder: serde_json::Value = serde_json::from_slice(&tfs::read(path.join("sync.json")).unwrap()).unwrap();
    assert!(folder.get("last_seq").is_none() && folder.get("last_sync").is_none(), "{folder}");
    assert_eq!(folder["mode"], "vault", "the shared part stays");
    for moved in ["people.json", "index"] {
        assert!(!tfs::exists(&path.join(moved)) && tfs::exists(&state.join(moved)), "{moved} moved out");
    }
    // The link's state, then, into the one record (docs/ONE_CODEBASE_CONTRACT.md
    // decision 7), the old file kept beside it.
    assert!(!tfs::exists(&path.join("vault-link.json")) && tfs::exists(&state.join("vault-link.json.migrated")), "vault-link.json moved out and folded in");
    let record: serde_json::Value = serde_json::from_slice(&tfs::read(state.join("sync-state.json")).unwrap()).unwrap();
    assert_eq!(record["vault"], serde_json::json!({"pushed_sv": {}}), "{record}");
    assert_eq!(record["link"]["last_seq"]["node:abc"], 7, "{record}");
    assert!(tfs::is_dir(path.join("computers")));

    // Writing a read position touches this computer's file only.
    let before = tfs::read(path.join("sync.json")).unwrap();
    a.write_sync_config(&link(&[("node:abc", 9)])).await.unwrap();
    assert_eq!(tfs::read(path.join("sync.json")).unwrap(), before, "the folder's sync.json is not rewritten for a read position");
    assert_eq!(a.read_sync_config().await.unwrap().unwrap().last_seq.get("node:abc"), Some(&9));
    drop(a);

    // An older Pimble on another computer writes its positions into the
    // folder again; a second computer opening it now takes none of them.
    let mut old = serde_json::to_value(link(&[("node:abc", 3)])).unwrap();
    old["last_sync"] = serde_json::json!("2026-11-18T11:00:00Z");
    tfs::write(path.join("sync.json"), serde_json::to_vec(&old).unwrap()).unwrap();
    tfs::write(path.join("vault-link.json"), b"{\"pushed_sv\":{\"x\":\"y\"}}").unwrap();
    let b_root = dir.path().join("state-b");
    let b = LocalStore::open_in(&path, Some(&b_root)).await.unwrap();
    let config = b.read_sync_config().await.unwrap().unwrap();
    assert!(config.last_seq.is_empty() && config.last_sync.is_none(), "another computer's positions are not this one's: {config:?}");
    assert_eq!(config.mode, SyncMode::Vault);
    assert!(tfs::exists(&path.join("vault-link.json")), "the older Pimble's own file is left where it uses it");
    assert!(!tfs::exists(&b_root.join(b.id.to_string()).join("vault-link.json")), "and never taken");
    // The first computer, opening again, keeps its own.
    let a = LocalStore::open_in(&path, Some(&state_root)).await.unwrap();
    assert_eq!(a.read_sync_config().await.unwrap().unwrap().last_seq.get("node:abc"), Some(&9));
}

// ── The randomized proof ─────────────────────────────────────────────────

/// A small deterministic generator (xorshift64*), so a seed replays.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    fn pick<T: Copy>(&mut self, items: &[T]) -> Option<T> {
        (!items.is_empty()).then(|| items[self.below(items.len())])
    }
}

/// A careless sync tool between two folders: it notices files that changed
/// on a side (when asked to look), queues a copy of their bytes as they were,
/// and delivers copies late, out of order across files and in bursts. A file
/// changed on the far side since the tool last saw it there becomes a
/// conflict copy (in any tool's naming), or is simply written over (the
/// other computer's write lost from the file); each side's last change wins
/// whichever arrives last.
///
/// One thing it never does: put an older copy of a file from one side over a
/// newer copy from the same side. Versions of one file from one computer land
/// in the order they were made, as every real tool keeps them. A tool that
/// did not would lose the newer version on the receiving computer whenever
/// that computer had not read it yet, until the sender writes the document
/// again: nothing on either computer knows the other lacks it (the
/// contract's known limits).
struct SyncTool {
    folders: [PathBuf; 2],
    /// What the tool last saw (or wrote) at each relative path, per side.
    seen: [HashMap<String, Vec<u8>>; 2],
    queue: Vec<(usize, String, Vec<u8>)>,
    conflicts_made: u32,
    log: Vec<String>,
}

impl SyncTool {
    fn new(a: &Path, b: &Path) -> Self {
        let mut tool = SyncTool { folders: [a.to_path_buf(), b.to_path_buf()], seen: [HashMap::new(), HashMap::new()], queue: Vec::new(), conflicts_made: 0, log: Vec::new() };
        // It starts with both folders in step (one was copied from the other).
        for side in 0..2 {
            for (rel, bytes) in tool.files(side) {
                tool.seen[side].insert(rel, bytes);
            }
        }
        tool
    }

    /// The files the tool syncs: the manifest and everything in `nodes/`
    /// but temporaries.
    fn files(&self, side: usize) -> BTreeMap<String, Vec<u8>> {
        let mut files = BTreeMap::new();
        let root = &self.folders[side];
        if let Ok(bytes) = tfs::read(root.join("manifest.json")) {
            files.insert("manifest.json".to_string(), bytes);
        }
        for path in tfs::read_dir(root.join("nodes")).unwrap() {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name.ends_with(".tmp") {
                continue;
            }
            if let Ok(bytes) = tfs::read(&path) {
                files.insert(format!("nodes/{name}"), bytes);
            }
        }
        files
    }

    fn scan(&mut self, side: usize) {
        for (rel, bytes) in self.files(side) {
            if self.seen[side].get(&rel) != Some(&bytes) {
                self.log.push(format!("scan {side} {rel} {}", sv_of(&bytes)));
                self.seen[side].insert(rel.clone(), bytes.clone());
                self.queue.push((side, rel, bytes));
            }
        }
    }

    fn deliver(&mut self, at: usize, rng: &mut Rng, careless: bool) {
        // The oldest queued version of the same file from the same side.
        let at = {
            let (from, rel) = (self.queue[at].0, self.queue[at].1.clone());
            self.queue.iter().position(|(f, r, _)| *f == from && *r == rel).unwrap_or(at)
        };
        let (from, rel, bytes) = self.queue.remove(at);
        let to = 1 - from;
        let target = self.folders[to].join(&rel);
        let current = tfs::read(&target).ok();
        let changed_there = current.as_ref().is_some_and(|c| self.seen[to].get(&rel) != Some(c));
        self.log.push(format!("deliver {from}->{to} {rel} {} over {} changed_there {changed_there}", sv_of(&bytes), current.as_deref().map(sv_of).unwrap_or_default()));
        if current.as_ref() == Some(&bytes) {
            self.seen[to].insert(rel, bytes);
            return;
        }
        let node = rel.strip_prefix("nodes/").and_then(|name| match classify_node_file(name) {
            NodeFile::Document(id) => Some(id),
            _ => None,
        });
        if changed_there && node.is_some() && !(careless && rng.chance(40)) {
            let style = ConflictStyle::ALL[rng.below(ConflictStyle::ALL.len())];
            self.conflicts_made += 1;
            let name = conflict_copy_name(node.unwrap(), style, self.conflicts_made);
            self.log.push(format!("  as copy {name}"));
            tfs::write(self.folders[to].join("nodes").join(&name), &bytes).unwrap();
            self.seen[to].insert(format!("nodes/{name}"), bytes);
            return;
        }
        // Written in place, the way a sync tool lands a download.
        let tmp = target.with_extension("synctool.tmp");
        tfs::write(&tmp, &bytes).unwrap();
        tfs::rename(&tmp, &target).unwrap();
        self.seen[to].insert(rel, bytes);
    }
}

fn sv_of(bytes: &[u8]) -> String {
    NodeDoc::load(bytes).map(|d| pimble_crdt::describe_state_vector(&d.state_vector())).unwrap_or_else(|_| "?".into())
}

fn live_ids(store: &LocalStore) -> Vec<NodeId> {
    let mut ids = store.list_node_ids();
    ids.sort_by_key(|id| id.to_string());
    ids
}

fn fresh_id(rng: &mut Rng) -> NodeId {
    NodeId(uuid::Uuid::from_u128(((rng.next() as u128) << 64) | rng.next() as u128))
}

/// One random edit through the store's own API, as the server makes them.
fn random_edit(store: &mut LocalStore, rng: &mut Rng, step: usize) {
    let live = live_ids(store);
    let root = store.root_node_id();
    let movable: Vec<NodeId> = live.iter().copied().filter(|id| *id != root).collect();
    match rng.below(6) {
        0 => {
            let Some(parent) = rng.pick(&live) else { return };
            let mut node = Node::document(format!("n{step}"));
            node.id = fresh_id(rng);
            let _ = store.create_node(node, Some(parent));
        }
        1 => {
            let Some(id) = rng.pick(&live) else { return };
            let Ok(node) = store.get_node(id) else { return };
            let mut metadata = node.metadata.clone();
            metadata.title = format!("t{step}");
            let _ = store.update_node_metadata(id, &metadata);
        }
        2 | 3 => {
            let Some(id) = rng.pick(&live) else { return };
            let _ = store.node_doc(id).and_then(|doc| doc.append_markdown(&format!("w{step}")).map_err(Into::into));
        }
        4 => {
            let (Some(id), Some(parent)) = (rng.pick(&movable), rng.pick(&live)) else { return };
            let place = if rng.chance(50) { Place::First } else { Place::Last };
            let _ = store.move_node(id, parent, place);
        }
        _ => {
            if rng.chance(50) {
                if let Some(id) = rng.pick(&movable) {
                    let _ = store.delete_node(id);
                }
            }
        }
    }
    store.repair_tree().unwrap();
}

/// What a store holds, document by document: its state vector, delete
/// digest, fields and text. Two stores that compare equal hold the same.
fn holdings(store: &LocalStore) -> BTreeMap<String, (Vec<u8>, [u8; 32], String, String)> {
    store
        .doc_ids()
        .into_iter()
        .map(|id| {
            let doc = store.tree().doc(id).unwrap();
            (id.to_string(), (doc.state_vector(), doc.delete_set_digest(), format!("{:?}", doc.fields().ok()), doc.text()))
        })
        .collect()
}

async fn absorb_and_repair(store: &mut LocalStore) -> usize {
    store.absorb_folder().await.unwrap();
    let merged = store.take_folder_merges().len();
    store.repair_tree().unwrap();
    merged
}

/// How much of what the test means to provoke a seed provoked: conflict
/// copies made, documents at the end, merges taken from the folder.
#[derive(Default, Debug)]
struct Provoked {
    conflicts: u32,
    documents: usize,
    merges: usize,
}

async fn run_seed(seed: u64) -> Provoked {
    let mut provoked = Provoked::default();
    let mut rng = Rng::new(seed);
    let dir = TempDir::new().unwrap();
    let (path_a, _) = made_store(dir.path()).await;
    let path_b = dir.path().join("b").join("s.pimble");
    copy_dir(&path_a, &path_b);
    let mut stores = [
        LocalStore::open_in(&path_a, Some(&dir.path().join("state-a"))).await.unwrap(),
        LocalStore::open_in(&path_b, Some(&dir.path().join("state-b"))).await.unwrap(),
    ];
    let mut tool = SyncTool::new(&path_a, &path_b);

    for step in 0..80 {
        let side = rng.below(2);
        tool.log.push(format!("step {step} side {side}"));
        match rng.below(100) {
            0..=39 => random_edit(&mut stores[side], &mut rng, step),
            40..=54 => {
                stores[side].flush().await.unwrap();
                tool.log.push(format!("flush {side}"));
            }
            55..=64 => tool.scan(side),
            65..=84 => {
                for _ in 0..=rng.below(4) {
                    if tool.queue.is_empty() {
                        break;
                    }
                    let at = rng.below(tool.queue.len());
                    tool.deliver(at, &mut rng, true);
                }
            }
            _ => provoked.merges += absorb_and_repair(&mut stores[side]).await,
        }
    }

    // The tool settles: every change is noticed and delivered, in order, and
    // both computers merge what arrives and write what they hold.
    let mut settled = false;
    for _ in 0..40 {
        for store in stores.iter_mut() {
            store.flush().await.unwrap();
        }
        tool.scan(0);
        tool.scan(1);
        let quiet = tool.queue.is_empty();
        while !tool.queue.is_empty() {
            tool.deliver(0, &mut rng, false);
        }
        for store in stores.iter_mut() {
            provoked.merges += absorb_and_repair(store).await;
        }
        if quiet && stores.iter().all(|s| !s.has_unwritten()) {
            settled = true;
            break;
        }
    }
    assert!(settled, "seed {seed}: the folders never settled");

    let (a, b) = (holdings(&stores[0]), holdings(&stores[1]));
    let only_a: HashSet<_> = a.keys().filter(|k| !b.contains_key(*k)).collect();
    let only_b: HashSet<_> = b.keys().filter(|k| !a.contains_key(*k)).collect();
    assert!(only_a.is_empty() && only_b.is_empty(), "seed {seed}: documents held on one side only: {only_a:?} {only_b:?}");
    for (id, held) in &a {
        if held != &b[id] {
            // What the tool did with the document, to read the failure by.
            for line in tool.log.iter().filter(|l| l.contains(id.as_str()) || l.starts_with("step") || l.starts_with("flush")) {
                eprintln!("seed {seed}: {line}");
            }
        }
        assert_eq!(held, &b[id], "seed {seed}: document {id} differs between the computers");
    }
    for (side, store) in stores.iter().enumerate() {
        let mut names: Vec<String> = tfs::names(store.path.join("nodes")).unwrap();
        names.sort();
        let mut expected: Vec<String> = store.doc_ids().into_iter().map(|id| format!("{id}.yrs")).collect();
        expected.sort();
        assert_eq!(names, expected, "seed {seed}: side {side}'s folder holds exactly one file per document and no conflict copies");
        for id in store.doc_ids() {
            let file = NodeDoc::load(&tfs::read(node_file(&store.path, id)).unwrap()).unwrap();
            let doc = store.tree().doc(id).unwrap();
            assert_eq!((file.state_vector(), file.delete_set_digest()), (doc.state_vector(), doc.delete_set_digest()), "seed {seed}: side {side}'s file of {id} holds what it holds");
        }
    }
    let manifests: Vec<StoreManifest> = stores.iter().map(|s| serde_json::from_slice(&tfs::read(s.path.join("manifest.json")).unwrap()).unwrap()).collect();
    assert_eq!(manifests[0].root_node_id, manifests[1].root_node_id, "seed {seed}");
    provoked.conflicts = tool.conflicts_made;
    provoked.documents = a.len();
    provoked
}

fn seeds() -> Vec<u64> {
    match std::env::var("PIMBLE_FOLDER_SEEDS") {
        Ok(list) if list.contains(',') => list.split(',').filter_map(|s| s.trim().parse().ok()).collect(),
        Ok(count) => match count.trim().parse::<u64>() {
            Ok(n) => (0..n).collect(),
            Err(_) => (0..12).collect(),
        },
        Err(_) => (0..12).collect(),
    }
}

/// Two computers, one store folder each, a careless sync tool between them,
/// random edits on both: once the tool settles, both hold the same documents
/// and each folder holds exactly one file per document.
#[tokio::test(flavor = "multi_thread")]
async fn two_computers_through_a_sync_tool_converge() {
    let seeds = seeds();
    let mut total = Provoked::default();
    for seed in &seeds {
        let provoked = run_seed(*seed).await;
        total.conflicts += provoked.conflicts;
        total.documents += provoked.documents;
        total.merges += provoked.merges;
    }
    eprintln!("{} seed(s) converged: {:?}", seeds.len(), total);
    // Not a vacuous proof: the seeds made conflict copies, documents and merges.
    assert!(total.conflicts > 0 && total.merges > seeds.len() && total.documents > 2 * seeds.len(), "{total:?}");
}
