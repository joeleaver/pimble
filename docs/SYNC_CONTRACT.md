# Sync contract: replicas of a store on two Pimble servers (dispatch-ready, 2026-09-15)

Roadmap step 6, second item: remote sync. Vision (`docs/RESTART_PLAN.md` §1): stores can
sit "behind a remote Pimble server"; "offline is the normal state; sync is opportunistic";
"the server relays and persists; it never owns the data".

## What exists

Every primitive is already there and stateless: `syncStoreDocument` and `syncNodeContent`
(state vector in, diff plus server state vector out), `applyStoreUpdate` and `applyEdit`
(apply a yrs update, persist, relay it to the store's subscribers with the raw bytes and a
`source_client_id`), `subscribeStoreChanges` and `subscribeNodeChanges`. Both CRDT
documents start empty (`ContentDoc::new`, `StoreDocument::load(&[])` carry no state), so a
replica can begin with nothing and receive everything as diffs. `PimbleClient` wraps all
of it and `pimble-server` does not depend on `pimble-client` (no cycle).

## Goal

A local store can be a **replica** of the same store (same `StoreId`) on another Pimble
server. While the remote is reachable, edits made on either side appear on the other
within a second and the search index on each side follows. While it is unreachable, both
sides keep working; on reconnect they converge with no conflicts, because everything is a
yrs merge. A replica survives restarts: the link is stored in the store directory and comes
back when the store opens. Cloning a store from a remote creates a replica from nothing.

Out of scope: partial (subtree-only) replication, remote mounts, server-side auth
enforcement (the client sends the header; nothing checks it yet), conflict UI (there are
no conflicts to show), TLS.

## Ownership

| Who | Scope |
| --- | --- |
| PM (done before dispatch) | `RemoteEndpoint` in `pimble-core`; the three RPCs, their types and `StoreChangeKind::SyncStateChanged` in `pimble-rpc`; stub handlers; `PimbleClient` wrappers; a placeholder match arm in the app |
| A | `pimble-store` (replica creation, sync config file), `pimble-server` (the sync link, stubs made real, index follow-up, bound address), `pimble-client` (auth header), `pimble-cli`, `tests/sync.rs`, `docs/ARCHITECTURE.md` sync section |
| B | `pimble-app` only |

Agents never edit each other's crates. The interface is fixed by this file; if A needs to
change an RPC type it says so in its report and the PM decides.

## Design decisions

1. **The link lives in the local server, per store.** A `SyncLink` task belongs to the
   local `RpcHandler`, one per linked store. It holds a `PimbleClient` to the remote. It
   never touches `LocalStore` for writes: remote changes enter through the handler's own
   `apply_edit` and `apply_store_update` (so local subscribers are notified, the search
   index is fed, and persistence happens exactly as for any client) with
   `client_id = "sync-link:<uuid>"`.
2. **Two directions, two rules.**
   - Remote to local: the link subscribes to the remote store's `storeChanged`. A
     notification carrying `update` bytes (`TreeStructure` from `applyStoreUpdate`,
     `ContentUpdated` from `applyEdit`, see decision 4) is applied locally as is. One
     without bytes (`NodeCreated`, `NodeDeleted`, `NodeMoved`, `MetadataUpdated`,
     `ContentUpdated` from `updateNodeContent`) triggers a reconcile of the store document
     or of that node. Notifications whose `source_client_id` is this link's own id are
     skipped.
   - Local to remote: the link listens to an in-process broadcast of the local
     notifications (decision 3) and forwards changes whose `source_client_id` is not a
     `sync-link:` id: bytes are sent as `applyEdit` or `applyStoreUpdate` with the link's
     id; a structural notification without bytes triggers a store-document reconcile.
   Skipping every `sync-link:` source in the forward direction is what prevents echo
   storms when both ends link to each other or a chain of three servers exists: a change
   reaches a third server through that server's own subscription, never by re-forwarding.
3. **Local notifications are broadcast in-process.** `SubscriptionRegistry` gains a
   `tokio::sync::broadcast::Sender<LocalChange>` (`LocalChange::Store(StoreChangedNotification)`
   and `LocalChange::Node(NodeContentChangedNotification)`) published wherever WebSocket
   sinks are notified today. The link subscribes to it. No loopback socket.
4. **`ContentUpdated` carries the delta.** `notify_node_content_change` puts the
   `applyEdit` operation's bytes into `StoreChangedNotification.update` (the field exists
   and is `None` today). A subscriber to the store alone then gets content deltas without a
   subscription per node. The app ignores `update` on `ContentUpdated`, so nothing breaks.
5. **Reconcile is the same procedure everywhere.** For the store document:
   `(d_r, sv_r) = remote.syncStoreDocument(sv_local)`; apply `d_r` locally if not empty;
   `d_l = local diff since sv_r`; `remote.applyStoreUpdate(link_id, d_l)` if not empty.
   For a node: the same with `syncNodeContent`/`applyEdit`. A full reconcile is the store
   document first, then every node id in the local store document (after the first step
   both sides have the same node set). Reconciles are debounced (200 ms) per store and per
   node so a burst of structural notifications yields one round trip.
6. **Link lifecycle.** Connect, full reconcile (`SyncState::Syncing`), subscribe, process
   (`Synced { last_sync }` updated after each applied change), on any error drop the
   connection and retry with backoff 1 s doubling to 30 s (`Offline` meanwhile). A
   `SyncStateChanged { state }` `storeChanged` notification goes to the store's local
   subscribers on every transition; `Store.sync_state` in `listStores`, `openStore` and
   `cloneStore` responses reflects the link (`Offline` for an unlinked store too;
   `getStoreSync` tells them apart).
7. **The link is persisted in the store.** `<store>/sync.json`:
   `{ "remote": { "url": "http://host:7462", "auth": { "method": "none" } } }`. `openStore`
   starts the link when the file exists; `setStoreSync` writes or deletes it and starts or
   stops the link; `closeStore` stops it.
8. **A clone is an empty replica plus a link.** `cloneStore` asks the remote for the
   store (`listStores` there, matched by id) and creates the local directory with the
   remote's id, name and root node id and an **empty** store document
   (`LocalStore::create_replica`), writes `sync.json`, opens the store, starts the link,
   and waits up to 10 s for the first full reconcile before answering (so the store comes
   back populated); on timeout it answers anyway with the current state. It refuses an
   existing path. `StoreDocument::new` must never run for a replica: two roots for the same
   id would merge into duplicated children.
9. **`applyStoreUpdate` feeds the index.** `StoreDocument::apply_update` returns the ids of
   the node entries the update touched (a `observe_deep` on the `nodes` map, collecting
   the top-level key of each event's path, for the duration of the transaction), and the
   handler enqueues `IndexEvent::Upsert` for each (and `Remove` for ids no longer present).
   Today a title change arriving as a store update is never reindexed.
10. **Auth is plumbed, not enforced.** `PimbleClient::connect_with_auth(url, &AuthMethod)`
    sends `Authorization: Bearer <token>` or `X-Api-Key: <key>` on the WebSocket
    handshake (`WsClientBuilder::set_headers`); `None` sends nothing; `OAuth2` is an
    error. The server checks nothing. `sync.json` stores the `AuthMethod` as is.

## Interface (landed by the PM)

`pimble-core`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteEndpoint { pub url: Url, pub auth: AuthMethod }
```

`pimble-rpc` (namespace `pimble`):

```rust
CloneStoreRequest   { remote: RemoteEndpoint, remote_store_id: StoreId, path: PathBuf }
// -> OpenStoreResponse { store }
SetStoreSyncRequest { store_id: StoreId, remote: Option<RemoteEndpoint> }   // None unlinks
// -> GetStoreSyncResponse
GetStoreSyncRequest { store_id: StoreId }
GetStoreSyncResponse { remote: Option<RemoteEndpoint>, state: SyncState }
StoreChangeKind::SyncStateChanged { state: SyncState }
```

`PimbleClient`: `clone_store(remote, remote_store_id, path) -> Store`,
`set_store_sync(store_id, Option<RemoteEndpoint>) -> (Option<RemoteEndpoint>, SyncState)`,
`get_store_sync(store_id) -> (Option<RemoteEndpoint>, SyncState)`.

## A: server side

### `pimble-store`

- `LocalStore::create_replica(path, id: StoreId, name, root_node_id) -> Result<Self>`:
  directory layout as `create`, manifest with the given id and root, `store.yrs` from
  `StoreDocument::load(&[])`. `StoreManager::create_replica(...) -> Result<StoreId>`.
- `SyncConfig { remote: RemoteEndpoint }` with `LocalStore::read_sync_config()`,
  `write_sync_config(&SyncConfig)`, `clear_sync_config()` on `<store>/sync.json`.
- `StoreDocument::apply_update` returns `Vec<NodeId>` (decision 9). `StoreManager::
  apply_store_doc_update` passes it through. A node id whose entry was removed is included.

### `pimble-server`

- `sync_link.rs`: `SyncLink::start(handler: RpcHandler, store_id, remote) -> SyncLinkHandle`
  (`RpcHandler` is made `Clone`; it is a bag of `Arc`s). The handle exposes a
  `tokio::sync::watch::Receiver<SyncState>` and `stop()`. `RpcHandler` keeps
  `links: Arc<RwLock<HashMap<StoreId, SyncLinkHandle>>>`.
- The three stub RPCs become real (decisions 6 to 8). `openStore` starts a link from
  `sync.json`; `closeStore` stops it; `listStores`/`openStore`/`cloneStore` fill
  `Store.sync_state` from the link.
- `SubscriptionRegistry` broadcast (decision 3); `ContentUpdated` carries bytes
  (decision 4); index follow-up on `applyStoreUpdate` (decision 9).
- `PimbleServer::addr()` returns the bound address (`Server::local_addr()`), so a config
  with port 0 works in tests.
- Logging: one `info!` per link transition, `debug!` per applied change. A reconcile that
  applies nothing logs nothing above `debug`.

### `pimble-client`

- `connect_with_auth` (decision 10). `connect` stays and equals `connect_with_auth(url,
  &AuthMethod::None)`.

### `pimble-cli`

- `server [--addr HOST:PORT] [--open PATH]...`: bind address and stores to open at start,
  so a headless replica host can run as `pimble-cli server --addr 0.0.0.0:7462 --open
  /srv/family.pimble`.
- `PIMBLE_SERVER` env var (default `http://127.0.0.1:7462`) for every client command.
- `clone-store <url> <remote-store-id> <path>`, `link-store <store-id> <url>`,
  `unlink-store <store-id>`, `sync-state <store-id>`, `remote-stores <url>` (lists the
  stores a remote has open). Update `print_help`.

### `tests/sync.rs`

Two real `PimbleServer`s on `127.0.0.1:0` in one process, driven through `PimbleClient`.
Helper: `wait_until(timeout, || async { cond })` polling every 50 ms; never `sleep` a fixed
long time and hope.

1. Clone: B has a store with one document ("hello"); A clones it; A's store has the same
   id, root id, and a node with text "hello"; A's `getStoreSync` is `Synced`.
2. Remote to local, live: `applyEdit` on B (a `ContentDoc` delta) appears in A's node text
   within 2 s; `createNode` on B appears in A's `getChildren`.
3. Local to remote, live: the reverse of 2 through A.
4. Offline and reconverge: unlink A (`setStoreSync None`), edit the same node on both
   sides and create one node on each, relink; both sides' text and child lists are equal
   within 5 s and contain both edits.
5. Remote down and back: stop B's server, edit on A, start a new B server on the same
   store directory (any port; update A's link to it), converge.
6. No echo storm: after test 3 settles, one more edit on A causes exactly one `applyEdit`
   to arrive at B (count via a `subscribeStoreChanges` on B for 1 s).
7. Index: a node created on B with text "quokka" is found by `search` on A after sync.
8. Restart: reopen A's store directory on a fresh server; the link comes back from
   `sync.json` and reaches `Synced`.

### Docs

`docs/ARCHITECTURE.md`: replace "Phase 5: Remote Sync" and the `SyncState` paragraph with
the link design (decisions 1 to 9) in a "Replica sync" section. Keep it short.

## B: app side

- Backend commands and events: `ListRemoteStores { url }` -> `RemoteStoresListed { url,
  result: Result<Vec<Store>, String> }` (a temporary `PimbleClient::connect(url)`;
  errors are reported, not logged away); `CloneStore { url, remote_store_id, path }` ->
  `StoreOpened` (or `Error`); `SetStoreSync { store_id, remote: Option<RemoteEndpoint> }`
  and `GetStoreSync { store_id }` -> `StoreSyncChanged { store_id, remote, state }`;
  `RemoteStoreChange` with `SyncStateChanged { state }` -> the same `StoreSyncChanged`
  update (keep the known `remote`).
- State: `sync_data: Signal<HashMap<StoreId, Signal<(Option<RemoteEndpoint>, SyncState)>>>`.
  `register_opened_store` sends `GetStoreSync`. On a transition to `Synced` for a store
  whose root children failed to load or are empty, refetch the root's children (a fresh
  clone has no root until the first reconcile).
- Store row: a small status badge after the name, only for linked stores: "offline",
  "syncing", "synced" (text is fine; an icon with those three states is better if
  cheap). Reactive per store, no tree rebuild.
- File menu: "Connect to Remote Store..." opens a `Modal` with a URL `TextInput`
  (default `http://`), a "List stores" `Button`, a `Select` of the remote's stores (name),
  a "Clone..." `Button` that opens `pick_folder` for the parent directory and clones into
  `<parent>/<store name>.pimble`, and an error line. The modal closes on success.
- Store root context menu: "Link to Remote..." (a `Modal` with a URL field; sends
  `SetStoreSync` with that endpoint; the server refuses if the remote has no store with
  this id and the error shows in the modal) and, when linked, "Unlink from Remote"
  (`SetStoreSync None`).
- Menu items must not sit inside reactive blocks (rinch #714): render both link items
  always and set `disabled` from the sync state at render time; bump the tree on
  `StoreSyncChanged` transitions between linked and unlinked so menus re-render.
- The rinch skill (`rinch:rinch`) is mandatory reading before editing `rsx!`.

## Verification (PM, after both reports)

1. `cargo check --workspace --all-targets`: zero warnings. `cargo test --workspace
   --release --no-fail-fast`: all pass except the known search flake.
2. Headless: `pimble-cli server --addr 127.0.0.1:7463 --open /tmp/b.pimble` as the
   remote; the app on 7462 clones from it; `set-node-text` against 7463 shows in the app;
   typing in the app shows in `show-node` against 7463; kill 7463, type, restart 7463,
   converge; restart the app, the badge comes back "synced".
3. GUI: the connect modal lists the remote's stores and clones; the store badge shows the
   three states; "Unlink from Remote" and "Link to Remote..." round-trip.

## Working agreements (unchanged)

- Build and run `pimble-app` in `--release`. At most three concurrent builds.
- Never touch `/home/joe/dev/rinch`; read rinch source from the cargo checkout.
- Report what you verified and how; report anything in this contract that turned out to
  be wrong rather than working around it.
