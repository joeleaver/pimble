//! The randomized fault test of docs/CONVERGENCE_CONTRACT.md ("Proving it",
//! wave 3): three devices over a real hosted server and real vault links,
//! random edits, and between steps the faults real devices meet:
//!
//! - an append the server stored whose answer never came back
//!   (`LinkFaults::lose_answers`), and a device stopped right after one,
//!   started again from its files;
//! - the link's record of what the remote holds (the `vault` section of
//!   `sync-state.json`, `vault-link.json` before) deleted, or put back from an
//!   older copy, while the device is stopped;
//! - a device crashed: its disk as of the moment (copied while it runs, the
//!   state directory before the store's folder, so a read position is never
//!   copied newer than a document written before it), stopped without its
//!   final writes counting, and started again from that copy. Right after,
//!   with every other device answering no reports, it must get back from the
//!   log alone every change it held before the crash
//!   (docs/ONE_CODEBASE_CONTRACT.md wave 0: before it, a read position was
//!   written at once and its document 750 ms later, and a crash in between
//!   lost those changes from the device for good while the others healed it
//!   only while they were online);
//! - snapshots stored every few appends while the others read the logs in
//!   pages a few hundred bytes long, so a snapshot lands between two pages;
//! - notifications lost, one device's network gone, every connection cut
//!   (the relay all devices reach the hosted server through), and the
//!   owner's computer away.
//!
//! After every run, with every device online and quiet: every document on
//! every device that holds it has the same state vector and the same delete
//! digest, nothing waits parked, every edit anyone made is in it, nothing is
//! said out of step, and no device appends anything more, within a bound.
//!
//! The devices: Alice's computer (the owner, which hosts the store), Alice's
//! second device (the whole store), and Bob's (an editor of the shared
//! folder, a partial replica). Alice's second device is configured as a
//! browser on odd seeds (docs/ONE_CODEBASE_CONTRACT.md section 8, wave 4): the
//! server as the browser's worker runs it, started in process and reached over
//! an in-process connection, its files on a memory disk that outlives each
//! server (as OPFS outlives a leader tab) sealed with a disk key, its keys
//! resting apart from the disk, and the device id a browser hands over from
//! before it ran its own server. Its crash copies that disk.
//! `PIMBLE_LINK_BROWSER=all` makes it a browser on every seed, `none` on none.
//!
//! Seeds: `PIMBLE_LINK_SEEDS=1,5` replays those, `PIMBLE_LINK_SEEDS=200` runs
//! 0..200; two by default, so the normal suite stays quick.
//! `PIMBLE_LINK_PARALLEL=4` runs four seeds at a time (each its own servers).
//! A run's timing is not replayed, only its choices.

mod common;
use pimble_host::disk::testfs as tfs;
use common::*;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use pimble_client::PimbleClient;
use pimble_core::{NodeId, StoreId, SyncState};
use pimble_crdt::NodeDoc;
use pimble_rpc::{EditOperation, MemberRole};
use pimble_host::{Disk, DiskBackend, MemoryDisk, MemoryKeyRest, Paths, SealedDisk};
use pimble_server::faults::LinkFaults;
use pimble_server::{service_extensions, InheritedDevice, PimbleServer, ServerConfig};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// How long everything has, once every device is online, to agree.
const SETTLE_WITHIN: Duration = Duration::from_secs(90);
const STEPS: usize = 20;

fn config(dir: &Path, faults: &Arc<LinkFaults>) -> ServerConfig {
    ServerConfig { vault_reports: true, out_of_step_after: Some(Duration::from_secs(2)), link_faults: Some(Arc::clone(faults)), ..device_config(dir) }
}

/// A device configured as a browser: its disk outlives each server it runs.
struct Browser {
    /// The disk below the seal (OPFS), what a crash copies.
    raw: Arc<dyn DiskBackend>,
    key: pimble_crypto::SymmetricKey,
    /// Where its keystore rests (the device key's IndexedDB), apart from the
    /// disk.
    keys: Arc<MemoryKeyRest>,
    device: String,
}

impl Browser {
    fn new(device: String) -> Self {
        Self { raw: Arc::new(MemoryDisk::new()), key: pimble_crypto::SymmetricKey::generate(), keys: Arc::new(MemoryKeyRest::default()), device }
    }

    fn disk(&self) -> Disk {
        Disk::new(SealedDisk::new(Arc::clone(&self.raw), self.key.clone()))
    }

    fn config(&self, dir: &Path, faults: &Arc<LinkFaults>) -> ServerConfig {
        let paths = Paths::new("/pimble", "/pimble-config");
        ServerConfig {
            disk: self.disk(),
            replicas_dir: Some(paths.replicas()),
            credentials_path: Some(paths.credentials()),
            keystore_path: Some(paths.keystore()),
            relay_dir: Some(paths.relay()),
            agents_path: None,
            paths,
            key_rest: Some(self.keys.clone()),
            inherited_device: Some(InheritedDevice { device: self.device.clone(), answered: Default::default() }),
            ..config(dir, faults)
        }
    }

    fn store_path(store_id: StoreId) -> PathBuf {
        Paths::new("/pimble", "/pimble-config").replicas().join(format!("{store_id}.pimble"))
    }
}

/// Copy a disk's files as they are now, the state directories first and the
/// rest after (see [`Device::crash`]), leaving out temporaries.
async fn copy_disk(from: &Disk, to: &Disk) {
    async fn walk(from: &Disk, to: &Disk, dir: PathBuf) {
        let Ok(entries) = from.list(&dir).await else { return };
        let _ = to.create_dir_all(&dir).await;
        for entry in entries {
            if entry.is_dir {
                Box::pin(walk(from, to, entry.path)).await;
            } else if !entry.path.extension().is_some_and(|e| e == "tmp") {
                if let Ok(bytes) = from.read(&entry.path).await {
                    let _ = to.write(&entry.path, bytes).await;
                }
            }
        }
    }
    walk(from, to, PathBuf::from("/pimble/stores")).await;
    let Ok(roots) = from.list("/").await else { return };
    for root in roots {
        if root.path == Path::new("/pimble") {
            for entry in from.list("/pimble").await.unwrap_or_default() {
                if entry.path != Path::new("/pimble/stores") {
                    if entry.is_dir {
                        walk(from, to, entry.path).await;
                    } else if let Ok(bytes) = from.read(&entry.path).await {
                        let _ = to.write(&entry.path, bytes).await;
                    }
                }
            }
        } else if root.is_dir {
            walk(from, to, root.path).await;
        }
    }
}

/// Whether `seed` makes Alice's second device a browser.
fn browser_on(seed: u64) -> bool {
    match std::env::var("PIMBLE_LINK_BROWSER").as_deref() {
        Ok("all") => true,
        Ok("none") => false,
        _ => seed % 2 == 1,
    }
}

struct Device {
    name: &'static str,
    /// Configured as a browser (see the module documentation).
    browser: Option<Browser>,
    dir: tempfile::TempDir,
    /// The store's folder on this device, opened again after a restart.
    store_path: PathBuf,
    faults: Arc<LinkFaults>,
    server: Option<PimbleServer>,
    client: Option<PimbleClient>,
    /// The documents this device holds and edits.
    docs: Vec<NodeId>,
    /// Steps left with its network gone; 0 when it has one.
    offline_for: u32,
    /// Steps left stopped (the owner away); 0 when running.
    away_for: u32,
    /// An older copy of its record of what the remote holds, to put back.
    older_progress: Option<serde_json::Value>,
}

impl Device {
    fn running(&self) -> bool {
        self.server.is_some()
    }

    fn client(&self) -> &PimbleClient {
        self.client.as_ref().expect("a running device")
    }

    fn state_dir(&self, store_id: StoreId) -> PathBuf {
        match &self.browser {
            Some(_) => Path::new("/pimble/stores").join(store_id.to_string()),
            None => self.dir.path().join("stores").join(store_id.to_string()),
        }
    }

    async fn read_file(&self, path: &Path) -> Option<Vec<u8>> {
        match &self.browser {
            Some(browser) => browser.disk().read(path).await.ok(),
            None => tfs::read(path).ok(),
        }
    }

    /// The link's record of what the remote holds: the `vault` section of
    /// `sync-state.json`.
    async fn progress(&self, store_id: StoreId) -> Option<serde_json::Value> {
        let bytes = self.read_file(&self.state_dir(store_id).join("sync-state.json")).await?;
        let mut record: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        record.get_mut("vault").map(serde_json::Value::take)
    }

    /// Replace (or with `None`, remove) that section, the device stopped.
    async fn put_progress(&self, store_id: StoreId, section: Option<serde_json::Value>) {
        let path = self.state_dir(store_id).join("sync-state.json");
        let Some(mut record) = self.read_file(&path).await.and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok()) else { return };
        let Some(map) = record.as_object_mut() else { return };
        match section {
            Some(section) => map.insert("vault".to_string(), section),
            None => map.remove("vault"),
        };
        let bytes = serde_json::to_vec_pretty(&record).unwrap();
        match &self.browser {
            Some(browser) => browser.disk().write(&path, bytes).await.unwrap(),
            None => tfs::write(&path, bytes).unwrap(),
        }
    }

    async fn start(&mut self) {
        let (server, client) = match &self.browser {
            Some(browser) => start_browser(browser, self.dir.path(), &self.faults).await,
            None => {
                let mut server = PimbleServer::with_config(config(self.dir.path(), &self.faults));
                server.start().await.expect("a device starts");
                let client = PimbleClient::connect(format!("http://{}", server.addr())).await.expect("a client connects");
                (server, client)
            }
        };
        client.open_store(&self.store_path).await.expect("the store opens again");
        self.server = Some(server);
        self.client = Some(client);
    }

    async fn stop(&mut self) {
        self.client = None;
        if let Some(mut server) = self.server.take() {
            server.stop().await.expect("a device stops");
        }
    }

    /// Crash: the disk as of now, and nothing written after it. The device's
    /// directory is copied while it runs, its state directory first and the
    /// rest (the store's folder) after: every file is written whole (a
    /// rename), and a read position written after its document is then never
    /// copied newer than the document. The derived search index is left out
    /// and built again. The server is stopped, and what its stop wrote is
    /// thrown away for the copy.
    async fn crash(&mut self) {
        if let Some(browser) = &self.browser {
            let below = Disk::from_arc(Arc::clone(&browser.raw));
            let copy: Arc<dyn DiskBackend> = Arc::new(MemoryDisk::new());
            copy_disk(&below, &Disk::from_arc(Arc::clone(&copy))).await;
            self.stop().await;
            tokio::time::sleep(Duration::from_millis(300)).await;
            // A browser's index lives in memory: gone with the crash.
            self.browser.as_mut().expect("a browser").raw = copy;
            return;
        }
        let copy = tempfile::tempdir().unwrap();
        copy_tree(&self.dir.path().join("stores"), &copy.path().join("stores"));
        for path in tfs::read_dir(self.dir.path()).unwrap() {
            let name = path.file_name().unwrap().to_owned();
            if name != "stores" {
                copy_tree(&path, &copy.path().join(name));
            }
        }
        self.stop().await;
        // Anything the stopped server still had in flight lands before the
        // copy goes back.
        tokio::time::sleep(Duration::from_millis(300)).await;
        for path in tfs::read_dir(self.dir.path()).unwrap() {
            if tfs::is_dir(&path) {
                tfs::remove_dir_all(&path).unwrap();
            } else {
                tfs::remove_file(&path).unwrap();
            }
        }
        if tfs::is_memory() {
            // The search indexes are on the machine's own disk whatever the
            // server's disk is (seam F): gone with the crash, as above.
            let _ = std::fs::remove_dir_all(self.dir.path().join("stores"));
        }
        for path in tfs::read_dir(copy.path()).unwrap() {
            tfs::rename(&path, self.dir.path().join(path.file_name().unwrap())).unwrap();
        }
    }
}

/// A browser-configured device's server: in process, as the worker starts
/// it, reached over an in-process connection with the `Service` principal, as
/// the page reaches the worker.
async fn start_browser(browser: &Browser, dir: &Path, faults: &Arc<LinkFaults>) -> (PimbleServer, PimbleClient) {
    let mut server = PimbleServer::with_config(browser.config(dir, faults));
    server.start_in_process().await.expect("a browser's server starts");
    let client = server.connect_in_process(service_extensions()).expect("a started server connects");
    (server, client)
}

/// Copy `from` to `to` as it is now, a file at a time, leaving out search
/// indexes and whatever vanishes while it is copied (a temporary renamed
/// into place).
fn copy_tree(from: &Path, to: &Path) {
    if !tfs::exists(from) {
        return;
    }
    if tfs::is_dir(from) {
        if from.file_name().is_some_and(|n| n == "index") {
            return;
        }
        let _ = tfs::create_dir_all(to);
        let Ok(entries) = tfs::read_dir(from) else { return };
        for path in entries {
            copy_tree(&path, &to.join(path.file_name().unwrap()));
        }
    } else if !from.extension().is_some_and(|e| e == "tmp") {
        let _ = tfs::copy(from, to);
    }
}

/// One edit of `node` on `client`: a paragraph naming `marker`, appended to
/// the document as that device holds it.
async fn edit(client: &PimbleClient, store_id: StoreId, node: NodeId, who: &str, marker: &str) -> bool {
    let Ok((answers, _)) = client.sync_nodes(store_id, &[(node, pimble_crdt::empty_state_vector())], false).await else { return false };
    let Some((_, whole, _)) = answers.into_iter().find(|(id, _, _)| *id == node) else { return false };
    let Ok(mut doc) = NodeDoc::load(&whole) else { return false };
    let Ok(update) = doc.append_markdown(marker) else { return false };
    client.apply_edit(store_id, node, who, EditOperation::IncrementalChanges { changes: STANDARD.encode(update) }).await.is_ok()
}

/// What one device holds of one document.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Held {
    state_vector: Vec<u8>,
    deletes: [u8; 32],
    parked: bool,
    text: String,
}

async fn held(client: &PimbleClient, store_id: StoreId, node: NodeId) -> Option<Held> {
    let node = client.get_node(store_id, node).await.ok()?;
    let doc = NodeDoc::load(&node.content).ok()?;
    Some(Held { state_vector: doc.state_vector(), deletes: doc.delete_set_digest(), parked: doc.missing().is_some(), text: doc.text() })
}

fn same_vector(a: &[u8], b: &[u8]) -> bool {
    !pimble_crdt::state_vector_exceeds(a, b).unwrap_or(true) && !pimble_crdt::state_vector_exceeds(b, a).unwrap_or(true)
}

/// Why the devices do not agree yet, or `None` when they do.
async fn disagreement(devices: &[Device], store_id: StoreId, all_docs: &[NodeId], markers: &std::collections::HashMap<NodeId, Vec<String>>) -> Option<String> {
    for &doc in all_docs {
        let mut first: Option<(&str, Held)> = None;
        for device in devices.iter().filter(|d| d.docs.contains(&doc)) {
            let Some(h) = held(device.client(), store_id, doc).await else { return Some(format!("{} does not hold {doc}", device.name)) };
            if h.parked {
                return Some(format!("{} holds an update of {doc} parked", device.name));
            }
            for marker in markers.get(&doc).into_iter().flatten() {
                if !h.text.contains(marker.as_str()) {
                    return Some(format!("{} lacks {marker:?} in {doc}", device.name));
                }
            }
            match &first {
                None => first = Some((device.name, h)),
                Some((name, f)) => {
                    if !same_vector(&f.state_vector, &h.state_vector) {
                        return Some(format!(
                            "{doc}: {name} holds {}, {} holds {}",
                            pimble_crdt::describe_state_vector(&f.state_vector),
                            device.name,
                            pimble_crdt::describe_state_vector(&h.state_vector)
                        ));
                    }
                    if f.deletes != h.deletes {
                        return Some(format!("{doc}: {name} and {} hold different deletes", device.name));
                    }
                }
            }
        }
    }
    None
}

/// The crash fault (see the module doc): `devices[i]`, once its own edits are
/// on the remote, hears another device's change and crashes within the
/// content flush's debounce; started again from its disk as it was, with
/// every other device answering no reports, it must hold again within a bound
/// every marker it held just before the crash, from the log alone.
#[allow(clippy::too_many_arguments)]
async fn crash_step(
    devices: &mut [Device],
    i: usize,
    store_id: StoreId,
    seed: u64,
    step: usize,
    rng: &mut StdRng,
    markers: &mut std::collections::HashMap<NodeId, Vec<String>>,
    log: &mut Vec<String>,
    relay_cut_for: u32,
) -> Result<(), String> {
    // Connected, so that what it made itself is on the remote: an edit a
    // crash takes before it left the device is lost by the crash, which is not
    // what this checks.
    if !devices[i].running() || devices[i].away_for > 0 || devices[i].offline_for > 0 || relay_cut_for > 0 {
        return Ok(());
    }
    if !wait_until(Duration::from_secs(15), || async { synced(devices[i].client(), store_id).await }).await {
        log.push(format!("{step}: {} not connected; no crash", devices[i].name));
        return Ok(());
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Another device's change, applied here right before the crash.
    let others: Vec<usize> = (0..devices.len()).filter(|j| *j != i && devices[*j].running()).collect();
    if !others.is_empty() {
        let j = others[rng.random_range(0..others.len())];
        let shared: Vec<NodeId> = devices[j].docs.iter().copied().filter(|d| devices[i].docs.contains(d)).collect();
        let doc = shared[rng.random_range(0..shared.len())];
        let marker = format!("seed{seed}-step{step}-{}-before-a-crash", devices[j].name);
        if edit(devices[j].client(), store_id, doc, devices[j].name, &marker).await {
            markers.entry(doc).or_default().push(marker);
            log.push(format!("{step}: {} edits {doc}", devices[j].name));
        }
    }
    tokio::time::sleep(Duration::from_millis(rng.random_range(50..500))).await;
    // What it holds now, of every marker made so far.
    let mut held_before: Vec<(NodeId, String)> = Vec::new();
    for &doc in &devices[i].docs {
        let text = node_text(devices[i].client(), store_id, doc).await;
        for marker in markers.get(&doc).into_iter().flatten() {
            if text.contains(marker.as_str()) {
                held_before.push((doc, marker.clone()));
            }
        }
    }
    devices[i].crash().await;
    log.push(format!("{step}: {} crashed holding {} markers", devices[i].name, held_before.len()));
    for (j, device) in devices.iter().enumerate() {
        if j != i {
            device.faults.set_deaf_to_reports(true);
        }
    }
    devices[i].start().await;
    let client = devices[i].client();
    let back = wait_until(Duration::from_secs(30), || async {
        if !synced(client, store_id).await {
            return false;
        }
        for (doc, marker) in &held_before {
            if !node_text(client, store_id, *doc).await.contains(marker.as_str()) {
                return false;
            }
        }
        true
    })
    .await;
    for device in devices.iter() {
        device.faults.set_deaf_to_reports(false);
    }
    if !back {
        let mut lacking = Vec::new();
        for (doc, marker) in &held_before {
            if !node_text(devices[i].client(), store_id, *doc).await.contains(marker.as_str()) {
                lacking.push(format!("{marker:?} in {doc}"));
            }
        }
        return Err(format!(
            "after its crash {} never got back from the log what it held before it (synced: {}): {}",
            devices[i].name,
            synced(devices[i].client(), store_id).await,
            lacking.join(", ")
        ));
    }
    log.push(format!("{step}: {} back after its crash, everything it held there again", devices[i].name));
    Ok(())
}

async fn total_appends(env: &Env, store_id: StoreId) -> u64 {
    env.h_admin.vault_list_docs(store_id).await.map(|docs| docs.iter().map(|d| d.head).sum()).unwrap_or(0)
}

async fn synced(client: &PimbleClient, store_id: StoreId) -> bool {
    matches!(client.get_store_sync(store_id).await, Ok((_, SyncState::Synced { .. })))
}

/// One seed's run. `Err` says what went wrong, with the run's steps.
async fn run(seed: u64) -> Result<String, String> {
    let mut rng = StdRng::seed_from_u64(seed);
    let env = spawn_env().await;
    let mut log: Vec<String> = Vec::new();

    let make = |name: &'static str, who: (&'static str, &'static str)| {
        let faults = Arc::new(LinkFaults::new());
        // Snapshots every few appends, logs read a few hundred bytes a page,
        // reports a moment after a change: snapshots land while others read.
        faults.set_snapshot_every(3);
        faults.set_page_bytes(400);
        faults.set_report_debounce(Duration::from_millis(300));
        (name, who, faults)
    };

    // Alice's computer: the store, hosted, its folder shared with Bob.
    let (name, who, faults) = make("alice", ALICE);
    let a_dir = tempfile::tempdir().unwrap();
    let mut a_server = PimbleServer::with_config(config(a_dir.path(), &faults));
    a_server.start().await.expect("Alice's computer starts");
    let a_client = PimbleClient::connect(format!("http://{}", a_server.addr())).await.unwrap();
    let fx = hosted_fixture(&env, &a_client, a_dir.path()).await;
    let store_id = fx.store_id;
    a_client.cloud_share_node(store_id, fx.shared, "Holiday Plans").await.map_err(|e| format!("sharing: {e}"))?;
    a_client.cloud_share_invite(store_id, fx.shared, BOB.0, MemberRole::Editor).await.map_err(|e| format!("inviting: {e}"))?;
    let shared_docs = vec![fx.shared, fx.inside, fx.deeper, fx.leaf];
    let all_docs = vec![fx.root_id, fx.shared, fx.inside, fx.deeper, fx.leaf, fx.private];
    let _ = who;
    let mut devices = vec![Device {
        name,
        browser: None,
        store_path: a_dir.path().join("a.pimble"),
        dir: a_dir,
        faults,
        server: Some(a_server),
        client: Some(a_client),
        docs: vec![fx.inside, fx.leaf, fx.private, fx.root_id, fx.shared, fx.deeper],
        offline_for: 0,
        away_for: 0,
        older_progress: None,
    }];

    // Alice's second computer and Bob's.
    for (name, who, docs) in [("alice-2", ALICE, all_docs.clone()), ("bob", BOB, shared_docs.clone())] {
        let (name, who, faults) = make(name, who);
        let dir = tempfile::tempdir().unwrap();
        let browser = (name == "alice-2" && browser_on(seed)).then(|| Browser::new(format!("browser-of-seed-{seed}")));
        let (server, client) = match &browser {
            Some(browser) => start_browser(browser, dir.path(), &faults).await,
            None => {
                let mut server = PimbleServer::with_config(config(dir.path(), &faults));
                server.start().await.expect("a device starts");
                let client = PimbleClient::connect(format!("http://{}", server.addr())).await.unwrap();
                (server, client)
            }
        };
        if browser.is_some() {
            log.push(format!("{name} is configured as a browser"));
        }
        env.sign_in(&client, who).await;
        client.cloud_add_hosted_store(store_id).await.map_err(|e| format!("{name} adds the store: {e}"))?;
        let pulled = wait_until(Duration::from_secs(30), || async { node_text(&client, store_id, fx.inside).await.contains("socks and a map") }).await;
        if !pulled {
            return Err(format!("{name} never pulled the store"));
        }
        let store_path = match &browser {
            Some(_) => Browser::store_path(store_id),
            None => dir.path().join("replicas").join(format!("{store_id}.pimble")),
        };
        if let Some(browser) = &browser {
            // The device it was handed is the one it reports as.
            let record = Path::new("/pimble/stores").join(store_id.to_string()).join("sync-state.json");
            let reports_as = || async {
                let bytes = browser.disk().read(&record).await.ok()?;
                serde_json::from_slice::<serde_json::Value>(&bytes).ok()?["device"]["device"].as_str().map(str::to_string)
            };
            let handed = wait_until(Duration::from_secs(15), || async { reports_as().await.as_deref() == Some(browser.device.as_str()) }).await;
            if !handed {
                return Err(format!("{name} does not report as the device it was handed: {:?}", reports_as().await));
            }
        }
        devices.push(Device { name, browser, dir, store_path, faults, server: Some(server), client: Some(client), docs, offline_for: 0, away_for: 0, older_progress: None });
    }

    let mut markers: std::collections::HashMap<NodeId, Vec<String>> = std::collections::HashMap::new();
    let mut relay_cut_for = 0u32;
    for step in 0..STEPS {
        // What ends this step.
        for device in devices.iter_mut() {
            if device.offline_for > 0 {
                device.offline_for -= 1;
                if device.offline_for == 0 {
                    device.faults.set_offline(false);
                    log.push(format!("{step}: {} online again", device.name));
                }
            }
            if device.away_for > 0 {
                device.away_for -= 1;
                if device.away_for == 0 {
                    device.start().await;
                    log.push(format!("{step}: {} back", device.name));
                }
            }
        }
        if relay_cut_for > 0 {
            relay_cut_for -= 1;
            if relay_cut_for == 0 {
                env.relay.restore();
                log.push(format!("{step}: connections back"));
            }
        }

        let roll: u32 = rng.random_range(0..100);
        let i = rng.random_range(0..devices.len());
        match roll {
            // An edit, sometimes with its append's answer lost.
            0..=44 => {
                let candidates: Vec<usize> = (0..devices.len()).filter(|i| devices[*i].running()).collect();
                if candidates.is_empty() {
                    continue;
                }
                let d = candidates[rng.random_range(0..candidates.len())];
                let doc = devices[d].docs[rng.random_range(0..devices[d].docs.len())];
                let lose = rng.random_range(0..4) == 0;
                if lose {
                    devices[d].faults.lose_answers(1);
                }
                let marker = format!("seed{seed}-step{step}-{}", devices[d].name);
                if edit(devices[d].client(), store_id, doc, devices[d].name, &marker).await {
                    markers.entry(doc).or_default().push(marker.clone());
                    log.push(format!("{step}: {} edits {doc}{}", devices[d].name, if lose { ", its answer lost" } else { "" }));
                }
                // Killed right after an append whose answer was lost, and
                // started again from its files.
                if lose && rng.random_range(0..2) == 0 && devices[d].away_for == 0 {
                    tokio::time::sleep(Duration::from_millis(rng.random_range(0..200))).await;
                    devices[d].stop().await;
                    devices[d].start().await;
                    log.push(format!("{step}: {} stopped mid-push and started again", devices[d].name));
                }
            }
            // Two devices edit one document at once.
            95..=99 => {
                let running: Vec<usize> = (0..devices.len()).filter(|i| devices[*i].running()).collect();
                if running.len() >= 2 {
                    let (x, y) = (running[0], running[running.len() - 1]);
                    let doc = *devices[x].docs.iter().find(|d| devices[y].docs.contains(d)).expect("a document both hold");
                    let (mx, my) = (format!("seed{seed}-step{step}-{}-at-once", devices[x].name), format!("seed{seed}-step{step}-{}-at-once", devices[y].name));
                    let (ex, ey) = futures::join!(edit(devices[x].client(), store_id, doc, devices[x].name, &mx), edit(devices[y].client(), store_id, doc, devices[y].name, &my));
                    if ex {
                        markers.entry(doc).or_default().push(mx);
                    }
                    if ey {
                        markers.entry(doc).or_default().push(my);
                    }
                    log.push(format!("{step}: {} and {} edit {doc} at once", devices[x].name, devices[y].name));
                }
            }
            45..=54 => {
                let n = rng.random_range(1..4);
                devices[i].faults.drop_notifications(n);
                log.push(format!("{step}: {} loses its next {n} notifications", devices[i].name));
            }
            55..=64 => {
                if devices[i].offline_for == 0 {
                    devices[i].offline_for = rng.random_range(1..4);
                    devices[i].faults.set_offline(true);
                    log.push(format!("{step}: {} offline for {} steps", devices[i].name, devices[i].offline_for));
                }
            }
            65..=69 => {
                if relay_cut_for == 0 {
                    relay_cut_for = rng.random_range(1..3);
                    env.relay.cut();
                    log.push(format!("{step}: every connection cut for {relay_cut_for} steps"));
                }
            }
            // Stopped and started again from its files, its record of what
            // the remote holds deleted, put back from an older copy, or kept.
            70..=75 => {
                if devices[i].running() && devices[i].away_for == 0 {
                    devices[i].stop().await;
                    let what = match rng.random_range(0..3) {
                        0 => {
                            devices[i].put_progress(store_id, None).await;
                            "its record of what the remote holds deleted"
                        }
                        1 => match devices[i].older_progress.clone() {
                            Some(older) => {
                                devices[i].put_progress(store_id, Some(older)).await;
                                "its record of what the remote holds put back from an older copy"
                            }
                            None => "its files kept",
                        },
                        _ => "its files kept",
                    };
                    devices[i].start().await;
                    log.push(format!("{step}: {} restarted, {what}", devices[i].name));
                }
            }
            // Crashed right after another device's change reached it, and
            // started again from its disk as it was.
            76..=81 => {
                if let Err(why) = crash_step(&mut devices, i, store_id, seed, step, &mut rng, &mut markers, &mut log, relay_cut_for).await {
                    let summary = format!("seed {seed}: {} steps", log.len());
                    return Err(format!("{summary}\n{why}\nsteps:\n  {}", log.join("\n  ")));
                }
            }
            // The owner's computer away for a few steps.
            82..=86 => {
                if devices[0].running() && devices[0].away_for == 0 {
                    devices[0].away_for = rng.random_range(2..5);
                    devices[0].stop().await;
                    log.push(format!("{step}: the owner's computer away for {} steps", devices[0].away_for));
                }
            }
            // A copy of a device's record to put back later.
            87..=94 => {
                if let Some(section) = devices[i].progress(store_id).await {
                    devices[i].older_progress = Some(section);
                }
            }
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(rng.random_range(100..400))).await;
    }

    // Everyone online, and quiet.
    env.relay.restore();
    for device in devices.iter_mut() {
        device.faults.set_offline(false);
        device.offline_for = 0;
        if !device.running() {
            device.away_for = 0;
            device.start().await;
        }
    }
    let deadline = tokio::time::Instant::now() + SETTLE_WITHIN;
    let last = loop {
        let all_synced = futures::future::join_all(devices.iter().map(|d| synced(d.client(), store_id))).await.into_iter().all(|s| s);
        let why = if all_synced { disagreement(&devices, store_id, &all_docs, &markers).await } else { Some("not every link is up".to_string()) };
        if why.is_none() || tokio::time::Instant::now() >= deadline {
            break why;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    let (lost, dropped) = devices.iter().map(|d| d.faults.happened()).fold((0, 0), |(a, b), (c, d)| (a + c, b + d));
    let crashes = log.iter().filter(|line| line.contains(" crashed ")).count();
    let browser_crashes = log.iter().filter(|line| line.contains(" alice-2 crashed ")).count();
    let browser = if log.iter().any(|line| line.contains("configured as a browser")) { format!(", alice-2 a browser ({browser_crashes} of its crashes)") } else { String::new() };
    let summary = format!("seed {seed}: {} steps, {lost} answers lost, {dropped} notifications dropped, {crashes} crashes{browser}", log.len());
    if let Some(why) = last {
        return Err(format!("{summary}\nnot converged within {:?}: {why}\nsteps:\n  {}", SETTLE_WITHIN, log.join("\n  ")));
    }

    // Quiet: nobody appends anything more, and nothing is said out of step.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let settled = total_appends(&env, store_id).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let after = total_appends(&env, store_id).await;
    if after != settled {
        return Err(format!("{summary}\nnot quiet: {} appends after everyone agreed\nsteps:\n  {}", after - settled, log.join("\n  ")));
    }
    for device in &devices {
        let said = device.client().get_store_sync_response(store_id).await.map(|r| r.out_of_step).unwrap_or_default();
        if !said.is_empty() {
            let mut held_by = String::new();
            for entry in &said {
                for other in &devices {
                    if let Some(h) = held(other.client(), store_id, entry.node_id).await {
                        held_by.push_str(&format!("\n  {} holds {}: {}", other.name, entry.node_id, pimble_crdt::describe_state_vector(&h.state_vector)));
                    }
                }
            }
            return Err(format!("{summary}\n{} still says {said:?}{held_by}\nsteps:\n  {}", device.name, log.join("\n  ")));
        }
    }
    for device in devices.iter_mut() {
        device.stop().await;
    }
    Ok(summary)
}

fn seeds() -> Vec<u64> {
    match std::env::var("PIMBLE_LINK_SEEDS") {
        Ok(spec) if spec.contains(',') => spec.split(',').filter_map(|s| s.trim().parse().ok()).collect(),
        Ok(spec) => match spec.trim().parse::<u64>() {
            Ok(n) => (0..n).collect(),
            Err(_) => vec![0, 1],
        },
        Err(_) => vec![0, 1],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_devices_converge_through_faults() {
    // `PIMBLE_LINK_LOG=pimble_server=info` says what the links do, for a replay.
    if let Ok(filter) = std::env::var("PIMBLE_LINK_LOG") {
        let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::new(filter)).with_writer(std::io::stderr).try_init();
    }
    let seeds = seeds();
    let parallel: usize = std::env::var("PIMBLE_LINK_PARALLEL").ok().and_then(|n| n.parse().ok()).unwrap_or(1).max(1);
    let mut failures = Vec::new();
    for chunk in seeds.chunks(parallel) {
        let runs = chunk.iter().map(|&seed| tokio::spawn(async move { (seed, run(seed).await) }));
        for joined in futures::future::join_all(runs).await {
            match joined {
                Ok((_, Ok(summary))) => eprintln!("{summary}: converged"),
                Ok((seed, Err(why))) => {
                    eprintln!("FAILED {why}");
                    failures.push(seed);
                }
                Err(e) => {
                    eprintln!("a run panicked: {e}");
                    failures.push(u64::MAX);
                }
            }
        }
    }
    assert!(failures.is_empty(), "seeds that did not converge: {failures:?} (replay with PIMBLE_LINK_SEEDS={})", failures.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(","));
}
