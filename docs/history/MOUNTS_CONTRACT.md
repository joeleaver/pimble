# Mounts contract: local mounts end to end (dispatch-ready, 2026-09-14)

Roadmap step 6, first item: "mounts (local first)". The scaffolding from before the
restart is still in the tree (`MountRef`, `Node::mount`, `StoreManager::resolve_mount`,
`createMount`/`getMountState` RPCs, the app's "Mount Store..." menu item), but it has one
structural bug and several gaps, so a mount does not work end to end today:

- The app files every child returned by `getChildren` under the store it asked, so a
  document reached through a mount is addressed `(mounting store, node)`. Opening it,
  editing it, subscribing to it, renaming it all go to the wrong store.
- A mount only resolves if its source store happens to be open or was opened earlier in
  the same process. After a restart the source is `Unavailable` unless it is in the app's
  open-store list.
- A store the resolver opens implicitly gets no search index and the client never learns
  it is open.
- Tree changes inside the source subtree never refresh the mounted view.
- `createMount` sends no `storeChanged` notification.
- No server-boundary tests exist for any mount RPC.

## Goal

A mount of any node of any local store under any node of any store: the mounted subtree
renders and expands, its documents open and edit live through the ordinary relay, it
survives an app restart even when the source store was not in the open-store list, tree
changes in the source appear under the mount, cycles are rejected, nested mounts resolve.
Remote sources stay out of scope; `MountState::Cached` and `Connecting` remain unused.

## Ownership

| Who | Scope |
| --- | --- |
| PM (done before dispatch) | `GetChildrenResponse.store_id`; `PimbleClient::get_children -> (StoreId, Vec<Node>)`; `BackendEvent::ChildrenLoaded.children_store_id` |
| A | `pimble-core` (`MountRef.source_path`), `pimble-store`, `pimble-server` (+ `tests/mounts.rs`), `pimble-client` (mount helpers only), `pimble-cli`, `docs/ARCHITECTURE.md` mount section |
| B | `pimble-app` only |

Agents never edit each other's crates. The interface between them is fixed by this file:
if A finds it must change an RPC type, it says so in its report and the PM decides.

## Design decisions

1. **Canonical addressing.** Every node a client holds is addressed by its canonical
   `(StoreId, NodeId)`: the store whose `store.yrs` holds it. `getChildren` returns
   `store_id`, the canonical store of the returned children: the request's store for an
   ordinary node, `mount_ref.source_store` for a mount point. (A nested mount node inside
   the source subtree is itself a source-store node; it resolves when expanded.)
2. **The mount node is a placeholder.** `getNode` on it returns the mount node itself
   (`node_type = "mount"`, empty `children`, empty `content`). `getChildren` on it returns
   the source node's children. Rename, delete and move addressed at the mount node act on
   the mount node in the mounting store only, never on the source. `createNode` with a
   mount node as parent is an error ("create under the mount's source instead"); the
   client creates under `mount_ref` when the user asks for a child of a mount.
3. **Source path hint.** `MountRef` gains `source_path: Option<PathBuf>`
   (`#[serde(default, skip_serializing_if = "Option::is_none")]`). `createMount` fills it
   from the registry's `Local` endpoint for the source store. Resolution order: store
   already open; registry entry; `source_path` hint (registering it on success);
   otherwise `Unavailable`. This is what makes a mount survive a restart.
4. **An implicitly opened source is an ordinary open store.** When the server opens a
   source store to resolve a mount, that store gets its search index (same path as
   `openStore`), appears in `listStores`, and accepts subscriptions. Clients learn about
   it by seeing a store id they do not know (in `children_store_id` or a `Live` mount
   state) and calling `listStores`. The app then treats it exactly like a store the user
   opened, including persisting it in the open-store list. Mounting a store opens it;
   that is the intended behaviour.
5. **`getMountState` tells the truth.** It attempts resolution (opening the source if it
   can) and reports `Live` or `Unavailable`, never `Live` merely because a registry entry
   exists.
6. **Notifications fan out on the client.** The source store's own `storeChanged`
   subscription already carries every change inside a mounted subtree. A client that
   shows store B's subtree through a mount in store A refreshes that mount's children when
   B reports a structural change. No new server-side fan-out.
7. **Tree values are path addresses.** The same canonical node can appear in several
   places (directly under its own store and under one or more mounts), and rinch's Tree
   keys expansion and selection by value string, so each place needs a distinct value.
   Format: `node_{store}_{node}` for a node reached directly; for a node reached through
   mounts append `/{mount_store}_{mount_node}` once per mount level, innermost last.
   `parse_tree_value` keeps returning the canonical pair (the first 73 characters after
   `node_`), so every existing consumer of it keeps working. `children_of`, `expanded`,
   `node_data`, `mount_data` stay keyed by canonical pairs.
8. **Cycles and depth.** `validate_mount_creation` (cycle and 16-level depth checks) stays
   as is and gets tests. Mounting into a mount node is rejected (a mount node has no
   children of its own).
9. **Search is unchanged.** Mount nodes index with no text; results address canonical
   nodes, and the source store is open, so a result inside a mounted subtree opens.

## A: server side

### `pimble-core`

- `MountRef { source_store, source_node, source_path: Option<PathBuf> }` with the serde
  attributes above. `Node::mount(store, node)` sets `None`; add
  `Node::mount_with_ref(MountRef)` (or `MountRef::with_source_path`) so the handler can
  store the hint. `mount_node_roundtrip` and the serialization test keep passing; add a
  test that a `MountRef` JSON without `source_path` still deserializes.

### `pimble-store`

- `StoreManager::ensure_store_open(&mut self, mount_ref: &MountRef) -> Result<StoreId>`
  is public and implements the resolution order in decision 3. `StoreEndpoint::Remote`
  still maps to `MountSourceUnavailable`.
- `StoreManager::get_children` on a mount node returns the source node's children; the
  handler needs the canonical store too, so either return `(StoreId, Vec<Node>)` from a
  new `get_children_canonical` or expose the mount ref and let the handler call
  `get_children` on the source. Choose one; do not leave two ways.
- `mount_state` becomes `async fn mount_state(&mut self, &MountRef) -> MountState`:
  `Live` if `ensure_store_open` succeeds, `Unavailable` otherwise.
- `StoreManager::opened_since(&mut self) -> Vec<StoreId>` or an equivalent so the handler
  can find stores the resolver opened implicitly (a drained "newly opened" list is
  simplest). Whatever the shape, the handler must not have to diff `list_stores()` on
  every call.
- `create_node` with a mount-node parent returns a new `StoreError::MountHasNoChildren`
  (name it as you like; the message must say to create under the source).

### `pimble-server`

- `get_children`: mount-aware; fills `GetChildrenResponse.store_id` with the canonical
  store; after any manager call that may have opened a store, open the search index for
  each newly opened store via the same helper `open_store` uses.
- `get_mount_state`: calls the async `mount_state`; same index follow-up.
- `create_mount`: fills `source_path` from the registry; rejects a mount-node parent;
  flushes; sends `notify_store_change(store_id, NodeCreated { node_id }, None)` (the gap
  noted in the current code) and the index upsert.
- `create_node`: surfaces the mount-parent error as an RPC error.
- `delete_node` on a mount node removes only the mount node (verify; the store document
  has no children for it, so this should already hold).
- `crates/pimble-server/tests/mounts.rs`, calling `RpcHandler` directly like
  `store_sync.rs`:
  1. `create_mount` then `get_children` on the mount returns the source node's children
     and `store_id == source store`.
  2. `get_mount_state` returns `Live` and a `mount_ref` whose `source_path` is the source
     store's directory.
  3. Restart: a fresh `StoreManager`/`RpcHandler` over the same directories opens only the
     mounting store; `get_children` on the mount resolves through `source_path`, returns
     the children, and `list_stores` now includes the source store.
  4. Source gone: rename the source directory away; `get_mount_state` is `Unavailable`
     and `get_children` on the mount is an error.
  5. Cycle: B's root mounted in A, then mounting A's root into B is rejected.
  6. Nested: A mounts B's root, B mounts C's root; `get_children` through A's mount lists
     B's mount node; `get_children` on that node (addressed in B) returns C's children with
     `store_id == C`.
  7. `create_mount` delivers a `NodeCreated` notification to a `subscribeStoreChanges`
     subscriber of the mounting store (follow whatever pattern `content_sync.rs` or
     `store_sync.rs` uses for subscriptions; if none exists, test via the handler's
     internal notification path and say so).
  8. `create_node` with the mount node as parent is an error.

### `pimble-client` and `pimble-cli`

- `create_mount` returns the server's `MountRef` (with `source_path`), not a locally
  rebuilt one: change `CreateMountResponse` to carry `mount_ref` and drop the client's
  reconstruction.
- CLI: `create-mount <store-id> <parent-id> <source-store-id> <source-node-id> [title]`,
  `mount-state <store-id> <node-id>`, and `list-children <store-id> <node-id>` printing
  one line per child: canonical store id, node id, type, title. Update `print_help`.

### Docs

- `docs/ARCHITECTURE.md` mount section: `MountRef.source_path`, `getChildren.store_id`,
  the "implicitly opened source is an ordinary open store" rule, and the placeholder rule
  for mount nodes. Keep it short; the section already has the shape.

## B: app side

- `children_of: Signal<HashMap<(StoreId, NodeId), Signal<Vec<(StoreId, NodeId)>>>>`.
  `ChildrenLoaded` keys the parent by `(store_id, parent_id)` and each child by
  `(children_store_id, child.id)`; `upsert_node`, `track_mount_info` and `GetMountState`
  for mount children use `children_store_id`.
- Unknown `children_store_id` (not in `store_ids`): send a new
  `BackendCommand::ListStores`; the backend answers `BackendEvent::StoresListed { stores }`
  (via `PimbleClient::list_stores`); the handler runs the `StoreOpened` code path (factor
  it into one function) for each store not yet known, minus the pending-mount match.
- `MountInfo` gains `mount_ref: Option<MountRef>`; `BackendEvent::MountStateChanged`
  carries the `mount_ref` the RPC already returns (the backend currently discards it).
- `RemoteStoreChange` with a structural kind (`NodeCreated`, `NodeDeleted`, `NodeMoved`,
  `TreeStructure`) for store S: in addition to the existing root refetch, send
  `GetChildren` for every mount node in `mount_data` whose `mount_ref.source_store == S`
  and whose children are loaded.
- Tree builder: recursion follows each child's canonical store; values carry the mount
  path (decision 7); a mount node whose children are not loaded shows the existing
  "Loading..." placeholder so it has a chevron; a mount in `Unavailable` state renders
  dimmed as today and expanding it is allowed (the `GetChildren` error lands in the status
  line, which is enough for now).
- `open_node`, rename, delete, drag-and-drop, `start_editing`, `SubscribeNodeChanges` all
  use the canonical pair from `parse_tree_value`; check each one still does after the
  value format change, especially any code that rebuilds a value string from a pair
  (`format!("node_{}_{}", ...)`) for `controller.select` or `expand`: it must use the
  path-qualified value of the place the user is looking at.
- Drag-and-drop: a drop whose source and target canonical stores differ is ignored with a
  `tracing::warn!` (no cross-store move yet). A drop onto a mount node is ignored the same
  way.
- Mount node context menu: "New Node" creates under `mount_ref` (source store and node);
  "Delete" deletes the mount node; no rename. Store roots and ordinary nodes gain:
  - "Copy as Mount Source": sets `mount_source: Signal<Option<(StoreId, NodeId, String)>>`
    (canonical pair and display title; for a store root, its root node and store name).
  - "Paste Mount Here": shown only while `mount_source` is set; sends `CreateMount` with
    `title: Some(that title)`; clears `mount_source`.
  "Mount Store..." (folder picker) stays for stores that are not open.
- `MountCreated` handling stays, using the returned `mount_ref`.

## Verification (PM, after both reports)

1. `cargo check --workspace --all-targets`: zero warnings. `cargo test --workspace
   --release`: all pass including the new `mounts.rs`.
2. Headless: `pimble-cli server`; create stores A and B; a node in B; `create-mount` into
   A; `list-children` on the mount shows B's node with B's store id; kill the server;
   start it; `open-store` A only; `list-children` on the mount still works and
   `list-stores` shows B.
3. GUI, two windows: mount B's root into A via "Copy as Mount Source" / "Paste Mount
   Here"; expand the mount; open a document through it in window 1 and directly in B in
   window 2; type in both; both update live. Add a node in B directly; it appears under
   the mount. Restart the app with B removed from `state.json`; the mount resolves and B
   reappears in the tree.

## Working agreements (unchanged)

- Build and run `pimble-app` in `--release`. At most three concurrent builds.
- Never touch `/home/joe/dev/rinch`; read rinch source from the cargo checkout.
- Report what you verified and how; report anything in this contract that turned out to
  be wrong rather than working around it.
