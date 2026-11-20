//! Starting up (docs/STARTUP_CONTRACT.md): three saved stores of 500 notes
//! each open side by side and answer, with the account status, within a
//! second, whether their indexes have to be built (the first start) or were
//! held by a server that just handed the port over (Joe's 19 s start,
//! 2026-11-20); and after the hand-over no index is rebuilt.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pimble_client::PimbleClient;
use pimble_core::{IndexState, Node, StoreId};
use pimble_crdt::NodeDoc;
use pimble_server::{local, PimbleServer, ServerConfig};
use pimble_store::StoreManager;
use tempfile::TempDir;

const NOTES: usize = 500;
/// The bound on every open and the account status (the contract's "say 1 s").
const BOUND: Duration = Duration::from_secs(1);

/// A store of `NOTES` notes, each with a paragraph of its own, written
/// straight to disk.
async fn make_store(path: &Path, name: &str) -> StoreId {
    let mut manager = StoreManager::new();
    let store_id = manager.create_local_store(path, name).await.unwrap();
    let root = manager.root_node_id(store_id).unwrap();
    let mut folder = root;
    for i in 0..NOTES {
        if i % 50 == 0 {
            folder = manager.create_node(store_id, Node::folder(format!("Folder {}", i / 50)), Some(root)).unwrap().0;
        }
        let mut node = Node::document(format!("{name} note {i}"));
        let text = format!("Note {i} of {name}: the lighthouse keeper wrote about tides, weather, ships number {i}, and the lamp. {}", "More words follow here. ".repeat(10));
        node.content = NodeDoc::from_plain_text(&text).unwrap().save();
        manager.create_node(store_id, node, Some(folder)).unwrap();
    }
    manager.flush_all().await.unwrap();
    manager.close_store(store_id).await.unwrap();
    store_id
}

/// Open every store side by side and ask the account status beside them;
/// how long each took, the slowest last.
async fn start(client: &PimbleClient, paths: &[PathBuf]) -> (Vec<Duration>, Vec<pimble_core::Store>) {
    let started = Instant::now();
    let opens = paths.iter().map(|path| {
        let path = path.clone();
        async move {
            let store = client.open_store(&path).await.expect("the store opens");
            (started.elapsed(), store)
        }
    });
    let account = async {
        client.cloud_status().await.expect("the account status answers");
        started.elapsed()
    };
    let (opened, account) = tokio::join!(futures::future::join_all(opens), account);
    let mut times: Vec<Duration> = opened.iter().map(|(t, _)| *t).collect();
    times.push(account);
    (times, opened.into_iter().map(|(_, s)| s).collect())
}

/// Wait until every store's index is ready; how long that took.
async fn wait_ready(client: &PimbleClient, stores: &[StoreId], within: Duration) -> Duration {
    let started = Instant::now();
    for store in stores {
        loop {
            let state = client.get_store_sync_response(*store).await.unwrap().index;
            if state == IndexState::Ready {
                break;
            }
            assert!(!matches!(state, IndexState::Unavailable { .. }), "{state:?}");
            assert!(started.elapsed() < within, "store {store}'s index is ready within {within:?}: {state:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    started.elapsed()
}

/// The index directory of each store, and its LOCK file's inode: a rebuild
/// deletes the directory, so a new inode is a rebuilt index.
fn index_inodes(data: &Path, stores: &[StoreId]) -> Vec<u64> {
    stores
        .iter()
        .map(|id| std::fs::metadata(data.join("pimble").join("stores").join(id.to_string()).join("index").join("rhypedb").join("LOCK")).expect("the store's index").ino())
        .collect()
}

/// The only test in this binary: it sets the environment the servers
/// `ensure_connected` starts read (the port, the config and data
/// directories).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_stores_open_within_a_second_with_and_without_a_hand_over() {
    let dir = TempDir::new().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let data = dir.path().join("data");
    // SAFETY: this binary's only test, set before anything reads them.
    unsafe {
        std::env::set_var("PIMBLE_APP_ADDR", &addr);
        std::env::set_var("XDG_CONFIG_HOME", dir.path().join("config"));
        std::env::set_var("XDG_DATA_HOME", &data);
    }

    let mut paths = Vec::new();
    let mut stores = Vec::new();
    for name in ["Personal", "Family", "Work"] {
        let path = dir.path().join(format!("{name}.pimble"));
        stores.push(make_store(&path, name).await);
        paths.push(path);
    }

    // A first start: no index exists, so each is built, and none of that
    // holds an open. This server stands for the one pimble-mcp ran: an
    // older version, so the next client asks it to hand over.
    let mut first = PimbleServer::with_config(ServerConfig {
        addr: addr.parse().unwrap(),
        reported_version: Some("0.0.1".into()),
        ..ServerConfig::in_dir(&data.join("pimble"))
    });
    first.start().await.unwrap();
    let client = PimbleClient::connect(format!("http://{addr}")).await.unwrap();
    let (times, opened) = start(&client, &paths).await;
    eprintln!("first start (indexes built): opens and account answered after {times:?}");
    for t in &times {
        assert!(*t < BOUND, "answered within {BOUND:?} while the indexes are built: {times:?}");
    }
    assert!(opened.iter().all(|s| !s.index.is_ready()), "each answered before its index was ready: {:?}", opened.iter().map(|s| &s.index).collect::<Vec<_>>());
    let searched = client.search_response("lighthouse", Vec::new(), false, 10).await.unwrap();
    assert!(!searched.indexing.is_empty(), "search says which stores are still being indexed");
    let built = wait_ready(&client, &stores, Duration::from_secs(120)).await;
    eprintln!("first start: every index built {built:?} after the opens");
    let inodes = index_inodes(&data, &stores);
    let found = client.search_response("lighthouse", Vec::new(), false, 10).await.unwrap();
    assert!(found.indexing.is_empty() && !found.results.is_empty(), "{found:?}");

    // The app starts: the older server hands over, closing its stores and
    // letting go of their indexes before the port is free.
    let handed = Instant::now();
    let (client, owned) = local::ensure_connected().await.expect("the newer client gets a server");
    eprintln!("hand-over: connected to the new server after {:?}", handed.elapsed());
    assert!(owned.is_some(), "it started the server it now uses");
    assert!(tokio::time::timeout(Duration::from_secs(5), first.wait()).await.is_ok(), "the older server stopped");
    let (times, _) = start(&client, &paths).await;
    eprintln!("after the hand-over: opens and account answered after {times:?}");
    for t in &times {
        assert!(*t < BOUND, "answered within {BOUND:?} after a hand-over: {times:?}");
    }
    let ready = wait_ready(&client, &stores, Duration::from_secs(10)).await;
    eprintln!("after the hand-over: every index open {ready:?} after the opens");
    assert!(ready < Duration::from_secs(3), "no index waited on a lock or was rebuilt: {ready:?}");
    assert_eq!(index_inodes(&data, &stores), inodes, "no index was rebuilt after the hand-over");
    let found = client.search_response("lighthouse", Vec::new(), false, 10).await.unwrap();
    assert!(found.indexing.is_empty() && !found.results.is_empty(), "what was indexed is found at once: {found:?}");

    let mut owned = owned;
    local::shut_down(&mut owned).await;
}
