# Remote mounts contract: a mount whose source store lives on another server

Status: in progress (2026-09-15). Written by the PM before dispatch; the interface below
is landed and committed before any agent starts.

Roadmap step 6, third item. Local mounts (`docs/history/MOUNTS_CONTRACT.md`) resolve a
`MountRef` to a store this server can open from disk. Replica sync
(`docs/history/SYNC_CONTRACT.md`) makes any store on another Pimble server a local
replica. Remote mounts are the composition: **the source becomes a replica on this server
and the mount resolves locally.** Nothing new travels over the wire for node data; the
new work is resolution, state, and notification.

## What exists

- `MountRef { source_store, source_node, source_path }`; `StoreManager::ensure_store_open`
  resolves open, registry, `source_path`, else `MountSourceUnavailable`.
- `MountState { Live, Cached { last_sync }, Unavailable, Connecting }` in `pimble-core`;
  only `Live` and `Unavailable` are produced.
- `addRemoteStore` creates an empty replica in `<data dir>/pimble/replicas/<id>.pimble`,
  writes `sync.json` (`auth: none`), starts a `SyncLink`, waits up to 10 s for `Synced`.
- `SyncLink::set_state` notifies `SyncStateChanged` on the store's subscribers on every
  category transition (`Offline`, `Syncing`, `Synced`).
- Credentials for a remote origin live in `credentials.json` and are resolved by
  `RpcHandler::connect_to_remote` whenever `RemoteEndpoint.auth` is `None`.
- The app discovers a store the server opened implicitly by seeing an unknown store id
  (`ChildrenLoaded.children_store_id`, or a `Live` mount state) and calling `listStores`.

## Goal

A mount whose source store is not on this machine resolves by itself: the server creates
a replica of the source in the background, links it, and the mount goes `Connecting`,
then `Live`. While the source's link is down the mount is `Cached { last_sync }` and its
replica's content still shows. When nothing can reach the source the mount is
`Unavailable` and says why. The app never polls: the server tells it when a mount's
state changes. A user can mount a store from a remote server in one action
("Mount Remote Store Here...").

Out of scope: replicating only the mounted subtree (the whole source store is
replicated), TLS, a retry loop for a failed replica creation (the next resolution
attempt retries).

## Ownership

| Who | Scope |
| --- | --- |
| PM (done before dispatch) | `MountRef.source_remote`; `MountState::Unavailable { reason }`; `SyncConfig.last_sync`; `StoreChangeKind::MountStateChanged`; compile fixes at every match site; this file |
| A | `pimble-server` (handler, `sync_link.rs`, `tests/remote_mounts.rs`), `pimble-store` (only if the handler needs a helper there), `pimble-cli`, `docs/ARCHITECTURE.md` |
| B | `pimble-app` only |

Agents never edit each other's crates. If A needs to change an RPC type it says so in
its report and the PM decides.

## Design decisions

1. **The source becomes a replica; the mount resolves locally.** Resolution of a
   `MountRef` on this server, in order: the source is open; the registry has a `Local`
   entry; `source_path` opens a store with the right id; `source_remote`; the mounting
   store's own remote (its `sync.json`), because a store and the stores it mounts usually
   live on the same server. The first three are `StoreManager::ensure_store_open` as it
   is today. The last two are the handler's: each is tried in turn by asking the remote
   for `listStores` and matching the id; the first remote that has the store wins and a
   replica is created from it exactly as `addRemoteStore` does with `path: None`
   (`<data dir>/pimble/replicas/<id>.pimble`, `is_replica`, removable). Creation runs in a
   background task; the RPC that triggered it answers `Connecting` at once.
2. **`MountRef.source_remote: Option<Url>`.** A URL only, never a credential: a mount ref
   is replicated with its store and lands on machines that must not hold the token.
   `createMount` fills it when the source store has a `sync.json` (it is a linked
   replica on this server); `None` otherwise. Serde: default, skipped when `None`.
3. **Mount state is derived from the source's link.** For an open source store:
   no link → `Live`; link `Synced` → `Live`; link `Syncing` or `Offline` with a known
   `last_sync` → `Cached { last_sync }`; link `Syncing` or `Offline` that has never synced
   → `Connecting`. For a source that is not open: replica creation in flight →
   `Connecting`; nothing to try (no path, no remote) or the last creation attempt failed →
   `Unavailable { reason }`. The reason is the human-readable error ("remote
   http://host:7463 refused the credentials", "no remote has store <id>", "source store
   <id> is not on this server and no remote is known for it").
4. **`last_sync` is persisted.** `SyncConfig` gains `last_sync: Option<DateTime<Utc>>`
   (serde default). The link reads it at start and keeps it in memory
   (`SyncLinkHandle::last_sync()`), updates the in-memory value every time it sets
   `Synced { last_sync }`, and rewrites `sync.json` only on a category transition into
   `Synced` (with now) or out of it (with the last in-memory value), preserving `remote`
   with `auth: none`. `setStoreSync` and `addRemoteStore` write `last_sync: None`.
   `getStoreSync` and `Store.sync_state` are unchanged.
5. **The server remembers which mounts it resolved per source store.**
   `RpcHandler.resolved_mounts: HashMap<source StoreId, HashSet<(mounting StoreId,
   mount NodeId)>>`, added to by every resolution (`getMountState`, `getChildren` on a
   mount, `createMount`), cleared of a mounting store's entries when that store closes.
   When a source's link changes category (`notify_sync_state_changed`), when a background
   replica creation finishes (success or failure), the handler recomputes the mount state
   for that source (decision 3) and sends
   `StoreChangeKind::MountStateChanged { node_id, state }` on each mounting store, with
   `source_client_id: None`. Closing or removing a source store sends nothing (the next
   resolution reopens or recreates it; that is today's behaviour for local mounts).
   Stale entries for a deleted mount node are harmless and tolerated.
6. **Sync links ignore `MountStateChanged`** in both directions, like
   `SyncStateChanged`: it is derived state and every server computes its own.
7. **`getChildren` on a mount whose source is not open is an error** whose message
   carries the state: "mount source is connecting" or "mount source unavailable:
   <reason>". The app already puts that error in the status line and refetches when a
   `MountStateChanged` says `Live` or `Cached`.
8. **Duplicate creation is impossible.** A per-source in-flight set, checked and set under
   the same lock as `resolved_mounts`, means two resolutions of the same source start one
   task. The background task uses the handler's existing `add_remote_store` code path
   (factor it so the RPC and the task share one function) so the "already open locally"
   guard and the `StoreDocument::new` prohibition both apply. `opened_since` follow-ups
   (search index) happen inside that shared function.
9. **The app's one-action flow is two RPCs.** "Mount Remote Store Here..." reuses the Add
   Remote Store modal with a target node. The backend calls `listStores`; if the chosen
   store id is already open (an existing replica) it skips `addRemoteStore`, else calls
   it (`path: None`); then `createMount(target, source root, title = store name)`. Events:
   `StoreOpened` (when added) then `MountCreated`. No new RPC.

## Interface (landed by the PM)

`pimble-core`:

```rust
pub struct MountRef {
    pub source_store: StoreId,
    pub source_node: NodeId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_remote: Option<Url>,
}

pub enum MountState {
    Live,
    Cached { last_sync: DateTime<Utc> },
    Unavailable { #[serde(default)] reason: Option<String> },
    Connecting,
}
```

`pimble-store`:

```rust
pub struct SyncConfig {
    pub remote: RemoteEndpoint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync: Option<DateTime<Utc>>,
}
```

`pimble-rpc`:

```rust
StoreChangeKind::MountStateChanged { node_id: NodeId, state: MountState }
```

## A: server side

### `pimble-server/src/handler.rs`

- `resolved_mounts` and the in-flight set (decisions 5, 8), inside one
  `Arc<Mutex<..>>` or `RwLock`.
- `async fn resolve_mount(&self, mounting_store: StoreId, mount_node: NodeId, mount_ref:
  &MountRef) -> MountState`: the single resolution function (decision 1) used by
  `get_mount_state`, `get_children` on a mount, and `create_mount`. It records the mount,
  runs `ensure_store_open` under the manager lock, derives the state (decision 3), and
  spawns the background creation when needed. It must not hold the manager lock across a
  remote round trip.
- `async fn mount_state_of_open_source(&self, source: StoreId) -> MountState`
  (decision 3), used by `resolve_mount` and by the fan-out.
- `async fn notify_mount_states_for_source(&self, source: StoreId)` (decision 5).
  Called from `notify_sync_state_changed` and at the end of the background task.
- `create_mount` fills `source_remote` (decision 2) from `read_sync_config` of the source.
- `close_store` removes the closing store's entries as a mounting store.
- `get_children`: on a mount node, call `resolve_mount` first; if the source is open,
  answer from it; else the decision 7 error. Keep the canonical `store_id` in the
  response.
- `add_remote_store` is split into the RPC wrapper and `async fn create_replica_from(&self,
  remote: RemoteEndpoint, store_id: StoreId, path: Option<PathBuf>, wait: bool) ->
  Result<Store, ErrorObjectOwned>`; the background task calls it with `wait: false`
  (the link's own transitions produce the `Connecting` → `Live` notifications).
- `set_store_sync`/`add_remote_store` write `last_sync: None`; the link owns it after that.

### `pimble-server/src/sync_link.rs`

- `SyncLinkHandle::last_sync() -> Option<DateTime<Utc>>` (decision 4), backed by a
  `watch` or `Mutex` shared with the task. Initialised from `sync.json` at `start`.
- `set_state` updates it on every `Synced { last_sync }` and rewrites `sync.json` on
  category transitions into and out of `Synced`. A failed write is a `warn!`, never a
  link failure.
- `handle_remote_notification` and `forward_local_change` ignore `MountStateChanged`
  (decision 6).

### `pimble-cli`

- `mount-state` prints the four states (`Cached` with its timestamp, `Unavailable` with
  its reason) and `source_remote` when set.
- `create-mount` output prints `source_remote` when set.
- `mount-remote-store <store-id> <parent-id> <url> <remote-store-id> [title]` with the
  same `--token` handling as `add-remote-store`: decision 9's two steps, headless.
- `print_help` updated.

### `tests/remote_mounts.rs`

Two or three real `PimbleServer`s on `127.0.0.1:0`, driven through `PimbleClient`, with
`wait_until` as in `tests/sync.rs` (copy the helpers; a shared `tests/common` module is
fine too). Data dirs: `create_replica_from` with `path: None` uses `dirs::data_local_dir`;
tests must not write there. Add a `ServerConfig.replicas_dir: Option<PathBuf>` (default:
today's `<data dir>/pimble/replicas`) so a test points it at a temp directory; `mark_replica`
and `removeReplica` use the same configured value. Check `sync_default_replica_path.rs`
still holds.

1. **`createMount` records `source_remote`.** R has B. L adds B as a replica, creates a
   local A, mounts B's root into A: `mount_ref.source_remote == R's url`,
   `mount_ref.source_path` is the replica's path, `getMountState` is `Live`.
2. **Resolution through `source_remote`.** After 1, M adds A from L as a replica.
   `getMountState` on the mount (in M's copy of A) is `Connecting` on the first call;
   within 10 s it is `Live`; `listStores` on M includes B with `is_replica`; `getChildren`
   on the mount returns B's children with `store_id == B`; a `MountStateChanged { state:
   Live }` for the mount node arrived on a `subscribeStoreChanges(A)` opened before the
   first `getMountState`.
3. **Resolution through the mounting store's own remote.** R has A and B, both local,
   A mounts B (`source_remote == None`). L adds A from R. Same assertions as 2 on L.
4. **Cached.** After 3, stop R. Within 10 s L's `getMountState` is `Cached { last_sync }`
   and `getChildren` on the mount still returns B's children. A `MountStateChanged {
   Cached }` notification arrived. Start a new R on the same store directories on a new
   port and `setStoreSync(B, new R)` on L: within 10 s `Live` again.
5. **Unavailable with a reason.** A mount whose `source_remote` is a closed port and whose
   `source_path` does not exist: first `getMountState` is `Connecting`; within 10 s it is
   `Unavailable { reason: Some(..) }` mentioning the URL, and a `MountStateChanged {
   Unavailable }` notification arrived. A second `getMountState` tries again (it goes
   `Connecting` once more).
6. **`last_sync` survives a restart.** After a link has been `Synced`, `sync.json` holds
   `last_sync` and `auth: none`. Close the store, stop the remote, reopen the store: the
   link is `Offline` and a mount sourced from it reports `Cached { last_sync }` equal to
   the persisted value.
7. **Credentials.** R runs with a token. L has the token saved (an `addRemoteStore` with
   it succeeded earlier). A mount replicated to L that names R resolves with no token in
   the request. A mount naming a token-protected server L has no credential for goes
   `Unavailable` with "refused the credentials" in the reason.
8. **No duplicate replica.** Two concurrent `getMountState` calls for two different mounts
   of the same not-yet-replicated source produce one replica (one `listStores` entry, one
   directory).

Existing tests: `mounts.rs` and anything matching `MountState::Unavailable` compile
against the new shape (the PM did this; keep them green).

### Docs

`docs/ARCHITECTURE.md` "Mount State & Degradation" and "Mount Resolver": replace the
prose with decisions 1 to 8 in the same shape as the "Replica sync" section. Remove the
"only the mounted subtree's nodes need to be replicated" claim (whole store, first cut).

## B: app side

- `RemoteStoreChange` with `MountStateChanged { node_id, state }` for store S: update the
  mount signal at `(S, node_id)` keeping its `mount_ref` (a new `AppStore::update_mount_state`
  that does not take a `MountRef`; `set_mount_state` stays for the `getMountState` answer).
  If the state is `Live` or `Cached` and the source store is unknown, send `ListStores`.
  If the state is `Live` or `Cached` and the mount is expanded or its children are not
  loaded, send `GetChildren` for the mount.
- The label suffix (three places in `app.rs` today) becomes one function
  `mount_label_suffix(&MountState) -> &'static str`: `Live` none, `Connecting`
  " (connecting...)", `Cached` " (offline copy)", `Unavailable` " (unavailable)". The
  icon dims for `Unavailable` and `Connecting` as it dims for `Unavailable` today.
- The `getMountState` answer (`BackendEvent::MountStateChanged`) handling stays and also
  applies the `Live`/`Cached` refetch rule above.
- "Mount Remote Store Here..." on store roots and ordinary nodes (never on mount nodes),
  rendered always (rinch #714). It opens the Add Remote Store modal with
  `connect_modal_target: Signal<Option<(StoreId, NodeId)>>` set to the canonical target
  (a store root's target is its root node). With a target the modal's title is "Mount
  Remote Store" and the action button reads "Mount". The action sends
  `BackendCommand::MountRemoteStore { url, remote_store_id, token, target_store_id,
  target_parent_id }`; the backend does decision 9's steps and emits `StoreOpened` (if it
  added) then `MountCreated`; an error goes to the modal's error line through the existing
  `connect_modal_pending_add` route. The modal closes on `MountCreated` when a target is
  set, and clears the target on close.
- "Paste Mount Here" and "Copy as Mount Source" are unchanged; a replica's nodes work as
  sources already.
- The rinch skill (`rinch:rinch`) is mandatory reading before editing `rsx!`. Never touch
  `/home/joe/dev/rinch`.

## Verification (PM, after both reports)

1. `cargo check --workspace --all-targets`: zero warnings. `cargo test --workspace
   --release --no-fail-fast`: all pass including `remote_mounts.rs`.
2. Headless, three servers: R (7463, token) with A and B, A mounts B; L (7464) adds A
   from R; `mount-state` on L goes Connecting then Live; `list-stores` on L shows B as a
   replica; `list-children` through the mount shows B's node; kill R; `mount-state` shows
   Cached with a time; `show-node` through the mount still answers; restart R; Live.
3. GUI against R: "Mount Remote Store Here..." on a local store's root lists R's stores,
   mounts B; the mount expands and its document opens and edits live with `set-node-text`
   on R. Kill R: the mount label shows "(offline copy)", the document still opens.
   Restart R: label back to normal within the backoff. Restart the app: the mount
   resolves from the replica with no dialogs.

## Working agreements (unchanged)

- Build and run `pimble-app` in `--release`. At most three concurrent builds.
- Never touch `/home/joe/dev/rinch`; read rinch source from the cargo checkout.
- Report what you verified and how; report anything in this contract that turned out to
  be wrong rather than working around it.
