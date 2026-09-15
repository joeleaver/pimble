# Hardening contract: auth, replica lifecycle, exact notifications, tree repair

Status: done 2026-09-14. Interface landed at 2e3821e; implemented as written, with these
changes decided during the work or in review:

- `TreeIssue` was redesigned around repair's model (orphan, detached, missing child,
  duplicate in a list, wrong list, missing from a list, root in a list, cycle);
  `DuplicateChild` is gone because a child in two lists is one correct placement plus
  `WrongList` entries.
- Repair edits lists surgically (removes exactly the wrong indices, appends exactly the
  missing children). A clear-and-rebuild of changed lists never converged across two
  replicas: each fabricated its own copy of every kept entry.
- The subtree delete walks with a visited set, skips the root and ids with no entry, and
  `StoreDocument::remove_node` tolerates a parent that is already gone, so deleting inside
  an unrepaired merge neither loops (the walk has no await point and would hang the
  runtime) nor half-fails nor takes the root with it.
- The flake's cause (not reproduced on demand, fixed by construction): `closeStore`
  returned while `StoreIndexer::run` and untracked debounce tasks still held the
  `Arc<SearchIndex>`, so a reopen could meet a second live rhypedb handle on the same
  directory. Index handles now own their tasks (debounce tasks in a `JoinSet`, reaped on
  every spawn) and `shutdown_index` drains them.
- Review also added: an empty token (file or config) is refused; the token and
  credentials temp files are created `0600`; an unreadable credentials file is logged;
  `removeReplica` reports a directory it could not delete; `[::1]` counts as loopback in
  the CLI; masked token fields; one refetch per parent for `TreeStructure`; RPC errors
  carry the server's message instead of jsonrpsee's `ErrorObject { .. }` rendering.
- Found in GUI verification: store rows never re-rendered when a store became linked
  (rinch's Tree diffs rows on `TreeNodeData`, which did not change), so "Link to
  Remote..." / "Unlink from Remote" stayed stale. The store row's (unrendered) label now
  carries the link state.
- `pimble-cli` gained `move-node` and `delete-node` for headless testing.

This phase takes the top follow-ups from `docs/NEXT_SESSION.md` (items 1, 2, 4, 6, 7, 8,
12, 13, 15) before the roadmap resumes with remote mounts. Remote mounts need two things
from it: credentials that never live in a store (a mount ref is replicated with its store),
and a tree that repairs itself after concurrent moves on two replicas.

## What exists

Replica sync (`docs/history/SYNC_CONTRACT.md`) works, but:

- Any client that reaches a server's port can do anything. `connect_with_auth` sends
  `Authorization: Bearer` or `X-Api-Key`; nothing checks it. A web page in a browser can
  open `ws://127.0.0.1:7462` (WebSockets are not bound by same-origin) and read every note.
- A full reconcile is one `syncNodeContent` round trip per node, and **every** one of them
  also pushes an `applyEdit`: yrs `encode_diff_v1` never returns empty bytes (an up-to-date
  diff is `[0, 0]`, plus the document's whole delete set), so `if !diff.is_empty()` is
  always true. Each no-op push touches the node's `modified_at`, flushes, notifies and
  re-indexes on the far side. 674 of them per reconnect on the family store.
- `deleteNode` removes the node's entry only; its descendants stay in `store.yrs` with a
  parent that no longer exists, and their `nodes/*.yrs` files stay on disk. The root can be
  deleted.
- Concurrent moves on two replicas can leave a child in two parents' lists, or a cycle;
  `validate_tree` reports some of it and nothing repairs it. `get_children` fails outright
  when a list names a node that does not exist.
- Structural notifications name the node but not its parents, so the app refetches every
  loaded children list of the store.
- `closeStore` drops its index handle while background tasks still hold the rhypedb
  database, so an immediate reopen runs against a second live handle on the same directory
  (the flaky `reopening_a_store_preserves_its_search_index`). The open-failure fallback
  wipes the directory but never reindexes (the schema hash it compares was read before
  the wipe).
- Unlinking a replica leaves its directory forever.

## Goal

A server refuses browsers always and, when given a token, every client without it; a
server without a token will not bind beyond loopback. Credentials a server uses to reach
remotes live in one per-user file, never in a store directory or an RPC response. The app
never connects to a remote itself. A replica can be removed. A reconnect costs about one
round trip per 100 nodes plus one per node that actually differs. Tree notifications name
every parent they change. Deleting a node deletes its subtree. After any merge the tree is
well formed on every replica, and replicas converge on the same tree.

Out of scope: TLS (`wss://`), OAuth2, remote mounts (next phase), the tree-label text cache
(item 9), importer formatting, cross-store drag and drop, rinch #714.

## Ownership

| Who | Scope |
| --- | --- |
| PM (done before dispatch) | `pimble-core` `Store.is_replica`; `pimble-rpc` (notification shapes, `syncNodeContents` replacing `syncNodeContent`, `listRemoteStores`, `removeReplica`); `TreeRepair` + `StoreDocument::repair` stub; `StoreManager::{delete_node -> NodeRemoval, move_node -> old parent, repair_tree}` (interim bodies); handler: parent ids in notifications, `syncNodeContents`, `is_replica` fill, stubs for the two new RPCs; sync link on `syncNodeContents`; client wrappers; compile fixes in tests and app |
| A: edge | `pimble-server/src/server.rs`, new `auth.rs` and `credentials.rs`; in `handler.rs` only `add_remote_store`, `set_store_sync`, `get_store_sync`, `list_remote_stores`, `remove_replica` (and new private helpers they need); in `sync_link.rs` only the connect line of `connect_and_sync`; `pimble-client` (error text); `pimble-cli`; new server tests `tests/auth.rs`, `tests/credentials.rs`, `tests/replica_removal.rs`; `docs/ARCHITECTURE.md` auth and replica-removal paragraphs |
| C: data | `pimble-crdt`, `pimble-store`, `pimble-search` (only if the index close needs it); in `handler.rs` only `apply_edit`, `apply_store_update`, `open_store`, `close_store`, `rebuild_store_index`, `open_index_for_store`, `reindex_all_nodes`, `StoreIndexer`, `IndexHandle`, `install_index_handle`; in `sync_link.rs` only `full_reconcile`, `reconcile_store`, `reconcile_node`, `push_node_diff`; new server tests `tests/tree_repair.rs`, `tests/noop_updates.rs`; the flaky test in `tests/search.rs` |
| B: app | `pimble-app` only |

A and C edit different functions of the same two server files in the same working tree.
Never edit a function the other owns. If the build breaks in code you do not own, wait a
minute and build again; if it stays broken, say so in your report rather than fixing it.
Nobody edits `pimble-rpc` or `pimble-core`; if an interface is wrong, report it and the PM
decides.

## Design decisions

1. **Two checks at the HTTP edge, before JSON-RPC.** A tower layer on the jsonrpsee server
   (`Server::builder().set_http_middleware(...)`; `jsonrpsee-server`'s own
   `middleware/http/host_filter.rs` is the pattern) runs on every HTTP request, which
   includes the WebSocket upgrade:
   - Any request carrying an `Origin` header is refused with `403`. Browsers always send
     `Origin` on a WebSocket handshake; native clients (jsonrpsee, the app, the CLI, a sync
     link) never do. This holds whether or not a token is configured.
   - When the server has a token, a request is accepted only with `Authorization: Bearer
     <token>` or `X-Api-Key: <token>`, compared in constant time; otherwise `401`.
2. **No token, loopback only.** `ServerConfig` gains `auth_token: Option<String>`.
   `PimbleServer::start` returns an error, before binding, when the address is not a
   loopback address and `auth_token` is `None`. The app's embedded server stays on
   `127.0.0.1:7462` with no token.
3. **The server token file.** `<dirs::config_dir()>/pimble/server-token`: 32 random bytes,
   base64url without padding, one line, file mode `0600`, created on first need.
   `pimble-cli server` uses it when `--addr` is not loopback (or with `--token-file PATH`
   for another file; loopback plus `--token-file` also enables the check). `pimble-cli
   token` prints it (creating it if missing); `pimble-cli token --new` replaces it. A
   server never logs the token.
4. **Credentials for remotes live in one file, keyed by origin.**
   `<dirs::config_dir()>/pimble/credentials.json`, mode `0600`: a JSON object from URL
   origin (`scheme://host:port`, default port filled in, `ws`/`wss` written as
   `http`/`https`) to `AuthMethod`. When the server
   connects to a remote (sync link, `addRemoteStore`, `setStoreSync`, `listRemoteStores`)
   the auth used is the request's `remote.auth` if it is not `None`, else the saved
   credential for that origin, else `None`. After a call that used a non-`None` request
   auth succeeds (the remote accepted it), the server saves it for that origin. `sync.json`
   is written with `auth: none` always; a server never returns a credential in any RPC
   response (`GetStoreSyncResponse.remote.auth` is `None`). The file path is configurable
   (`ServerConfig`, and whatever `RpcHandler` constructor the tests need) so tests never
   touch the real config directory.
5. **Clients never connect to remotes.** `listRemoteStores { remote }` on the local server
   replaces the app's and CLI's direct connection. When a remote refuses, the error message
   says so plainly: it contains `refused the credentials` for a `401` and `refused the
   connection` for a `403`, plus the remote's URL.
6. **Removing a replica.** `removeReplica { store_id, force }` is refused unless the store
   is open on this server and its directory is inside `<data dir>/pimble/replicas/`
   (`Store.is_replica`), and refused when its link is not `Synced` unless `force` (the
   message says the replica has changes the remote may not have). It stops the link,
   closes the store exactly as `closeStore` does (call it), then deletes the directory.
   Saved credentials stay (other stores may use that remote).
7. **Batch content reconcile, and no no-op pushes.** `syncNodeContents` takes at most 100
   nodes (`MAX_SYNC_NODE_CONTENTS`), answers one entry per node the server has in request
   order, and leaves unknown nodes out. A full reconcile is the store document, then node
   content in batches of 100. After pulling a node's diff, the link pushes a diff back only
   when the local document has something the remote lacks: a struct beyond the remote's
   state vector, or a deletion missing from the delete set the remote just sent (a yrs diff
   always carries its sender's full delete set). The same rule for the store document.
8. **A no-op update changes nothing on the server.** `applyEdit` and `applyStoreUpdate`
   with an update the document already contains return success without touching
   `modified_at`, flushing, notifying or re-indexing. `pimble-crdt` reports whether an
   update changed its document (transaction before/after state and delete set).
9. **Tree repair.** `StoreDocument::repair()` rewrites the merged tree into a well formed
   one, deterministically in the merged state, in one transaction, never touching
   timestamps. With `R` the root and `N` the node entries:
   1. Effective parent `e(n)` for `n != R`: its `parent_id` if that names an entry in `N`
      other than itself, else `R`.
   2. While some node's `e` chain does not reach `R` (a cycle), set `e` of the cycle member
      with the smallest id (string order) to `R`.
   3. Write `parent_id = e(n)` wherever it differs (absent included). The root has none.
   4. In every children list keep an entry only if it names `c` in `N` with `e(c)` equal to
      the list's owner, and only its first occurrence. Remove every other entry, including
      any naming `R`.
   5. Append each `c != R` with no remaining entry to `e(c)`'s list, in id order.
   It returns `Some(TreeRepair { update, touched })` when it changed anything, else `None`,
   and `validate_tree` reports exactly the conditions it fixes (so after `repair`,
   `validate_tree` is empty). Two replicas that repair the same merged state make the same
   deletions and the same `parent_id` writes; concurrent appends of the same child can
   leave a duplicate in one list, which the next repair removes (first occurrence is the
   same everywhere). The server repairs a store when it opens and after every
   `applyStoreUpdate` that changed the document; a repair is flushed, broadcast as
   `TreeStructure { node_ids: touched }` with the update bytes and `source_client_id: None`
   (so a sync link forwards it), and re-indexes the touched nodes.
10. **Deletes remove the subtree.** `StoreManager::delete_node` removes the node, every
    descendant's entry and every descendant's content file, refuses the root, and returns
    `NodeRemoval { parent_id, removed }`. An orphan can then only come from concurrency
    (one replica deletes a folder while another adds to it), and repair keeps the orphan by
    moving it under the root rather than losing it.
11. **Structural notifications name their parents** (landed): `NodeCreated { node_id,
    parent_id }`, `NodeDeleted { node_id, parent_id }` (the whole subtree went),
    `NodeMoved { node_id, old_parent_id, new_parent_id }`, `TreeStructure { node_ids }`
    (entries the update created, removed or changed, parents whose lists changed
    included). `createNode` with `parent_id: None` creates under the root.
12. **`closeStore` closes the index before it returns.** When it answers, no task holds the
    store's `SearchIndex` any more (the indexer task has ended and pending debounced
    upserts no longer keep it alive), so a reopen never meets a second live database on the
    directory. `rebuildIndex` closes the old index the same way before deleting the
    directory. When opening fails and the fallback wipes the directory, the fresh index is
    always rebuilt from the store.
13. **Signals.** `pimble-cli server` and `pimble_server::run_server` stop and flush on
    SIGTERM as well as SIGINT.

## Interface (landed by the PM)

`pimble-core`: `Store.is_replica: bool` (`#[serde(default)]`), filled by the server on
every `Store` it returns.

`pimble-rpc`:

```rust
StoreChangeKind::NodeCreated { node_id, parent_id: NodeId }
StoreChangeKind::NodeDeleted { node_id, parent_id: NodeId }
StoreChangeKind::NodeMoved { node_id, old_parent_id: NodeId, new_parent_id: NodeId }
StoreChangeKind::TreeStructure { node_ids: Vec<NodeId> }

pub const MAX_SYNC_NODE_CONTENTS: usize = 100;
syncNodeContents(SyncNodeContentsRequest { store_id, nodes: Vec<NodeStateVector { node_id, state_vector }> })
  -> SyncNodeContentsResponse { nodes: Vec<NodeContentDiff { node_id, diff, state_vector }> }   // replaces syncNodeContent
listRemoteStores(ListRemoteStoresRequest { remote: RemoteEndpoint }) -> ListStoresResponse
removeReplica(RemoveReplicaRequest { store_id, force: bool }) -> EmptyResponse
```

`pimble-crdt`: `TreeRepair { update: Vec<u8>, touched: Vec<NodeId> }`,
`StoreDocument::repair(&mut self) -> Result<Option<TreeRepair>>` (stub).

`pimble-store`: `NodeRemoval { parent_id, removed: Vec<NodeId> }`;
`StoreManager::delete_node(..) -> Result<NodeRemoval>` (interim: node only),
`move_node(..) -> Result<NodeId>` (the old parent), `repair_tree(store_id) ->
Result<Option<TreeRepair>>` (calls the stub).

`PimbleClient`: `sync_node_contents(store_id, &[(NodeId, Vec<u8>)]) -> Vec<(NodeId, diff,
server_sv)>` (splits into requests of 100), `sync_node_content` (one-node convenience,
error if the node is left out), `list_remote_stores(remote) -> Vec<Store>`,
`remove_replica(store_id, force)`.

## A: edge

- `auth.rs`: the tower layer (decision 1). Unit-test the header rules without a network.
- `server.rs`: `ServerConfig { addr, auth_token, credentials_path }` (keep `Default`: the
  app's address, no token, default credentials path); the refusal (decision 2); SIGTERM in
  `run_server`.
- `credentials.rs`: load, look up by origin, save (atomic write, `0600`); origin
  normalization (`http://host` and `http://host:80` are the same origin).
- Handler: `listRemoteStores` (decision 5); `addRemoteStore`, `setStoreSync` resolve and
  save credentials (decision 4) and write `sync.json` with `auth: none`; `getStoreSync` and
  `setStoreSync` never return auth; `removeReplica` (decision 6). The sync link resolves
  auth through the credentials store when it connects. Map a remote's `401`/`403` into the
  decision 5 wording wherever the server connects to a remote.
- `pimble-cli`: `server [--addr HOST:PORT] [--open PATH]... [--token-file PATH]`;
  `token [--new]`; client commands send `PIMBLE_TOKEN` as a bearer token when set, else the
  default server token file's token when `PIMBLE_SERVER`'s host is loopback and the file
  exists; `add-remote-store <url> <store-id> [path] [--token T]`, `link-store <store-id>
  <url> [--token T]`, `remote-stores <url> [--token T]` (through `listRemoteStores`),
  `remove-replica <store-id> [--force]`. Update `print_help`.
- Tests. `tests/auth.rs`: a token server on `127.0.0.1:0` refuses no header, a wrong
  bearer, and accepts the right bearer and the right `X-Api-Key`; a request with an
  `Origin` header is refused with and without a token configured; `start` on `0.0.0.0:0`
  without a token fails and with one succeeds. `tests/credentials.rs` (temp credentials
  path): server B with a token, A adds B's store with that bearer token and reaches
  `Synced`; A's `sync.json` holds `"method": "none"`; the credentials file holds the token
  under B's origin; `getStoreSync` returns no auth; a fresh A on the same replica
  directory and credentials path reaches `Synced` with nothing passed; `listRemoteStores`
  works with an explicit token and with only the saved one, and a wrong token gives an
  error containing `refused the credentials`. `tests/replica_removal.rs` (its own binary
  with `XDG_DATA_HOME` set to a temp dir, like `sync_default_replica_path.rs`): an ordinary
  store is refused; a replica whose remote is down is refused without `force` and removed
  with it; a synced replica is removed, its directory is gone, it is no longer in
  `listStores`, and `is_replica` was `true` before.
- `docs/ARCHITECTURE.md`: a short "Auth" paragraph (decisions 1 to 5, and that the app's
  loopback server trusts local processes) and replica removal in "Replica sync".

## C: data

- `pimble-crdt`: `repair` and a complete `validate_tree` (decision 9); no-op detection for
  both documents (decision 8); a way to compute "what the peer lacks, given its state vector
  and the diff it sent" for both documents (decision 7), `None` when nothing. Tests:
  scripted concurrent scenarios on two documents exchanging updates (moves of one node to
  two parents; A under B and B under A; delete a folder while the other side adds a child
  to it; delete a node while the other side moves it; both sides repair concurrently and
  exchange); after at most two rounds of exchange-and-repair both documents have identical
  trees (compare parent and ordered children for every node), `validate_tree` is empty and
  `repair` returns `None` on both.
- `pimble-store`: subtree delete and root refusal (decision 10); `repair_tree` marks the
  document dirty; `apply_content_update` and the store-document merge skip `touch_modified`
  and dirtying for a no-op.
- Handler: `apply_edit`/`apply_store_update` no-op early return (decision 8); repair after a
  changing `applyStoreUpdate` and at `openStore` (decision 9); index close and fallback
  rebuild (decision 12). Sync link reconcile functions push only what the remote lacks
  (decision 7).
- Tests. `tests/noop_updates.rs`: re-sending an update the server already merged produces
  no notification and leaves `modified_at` unchanged; relinking two already synced servers
  (unlink, relink, no edits, content with deletions in it) produces no `ContentUpdated` or
  `TreeStructure` notification on either side and changes no node's `modified_at` (watch a
  `subscribeStoreChanges` on each for 1 s after the link is `Synced` again). Unit tests in
  `pimble-crdt` show "what the peer lacks" is `None` for identical documents that contain
  deletions, and `Some` when either a struct or a deletion is missing. `tests/tree_repair.rs`: two linked servers, unlink, move one node
  to different parents on each, relink, within 5 s both stores' `validate_tree` is empty and
  the node sits under the same single parent on both; the same for a cycle. A test that a
  deleted folder's children are gone from `store.yrs` and their content files from disk.
  The search flake: find the actual cause and report it; the test passes 20 runs in a row
  while the rest of the server tests run in parallel (script it; say how).

## B: app

- Add Remote Store modal: URL, a token field (empty means "use what the server saved"),
  "List stores" sends `ListRemoteStores { remote }` through the local server (no direct
  connection from the app), "Add" sends the same endpoint. The Link to Remote modal gets
  the same token field. Show the server's error text as is.
- Store root context menu: "Remove Replica..." (enabled when `Store.is_replica`) opens a
  confirmation modal naming the store and its remote; when the link is not synced it warns
  that changes made here since the last sync will be lost and removes with `force`. On
  success the store leaves the tree and the saved open-store list, as on close.
- Exact refetch from the notification shapes: `NodeCreated`/`NodeDeleted` refetch that
  parent's loaded children, `NodeMoved` both parents, `TreeStructure { node_ids }` the
  loaded children of every listed id and of every listed id's cached parent; plus every
  mount (any store) whose `mount_ref` source is one of those `(store, parent)` pairs and
  whose children are loaded. A `NodeDeleted` removes the whole subtree from the app's
  caches and clears the selection if it was inside.
- `MountStateChanged` for a source store the app does not know yet sends `ListStores`
  (item 8).
- Hide the bullet list, ordered list and blockquote toolbar buttons: the collaboration
  scope rejects those blocks (item 13).
- Menu items stay outside reactive blocks (rinch #714); the rinch skill (`rinch:rinch`) is
  mandatory reading before editing `rsx!`. Verify in the running app with the rinch MCP
  tools against a second server, not the app's own.

## Verification (PM, after all reports)

1. `cargo check --workspace --all-targets`: zero warnings. `cargo test --workspace
   --release --no-fail-fast`: all pass, three full runs in a row, flake included.
2. Headless: `pimble-cli server --addr 127.0.0.1:7463 --token-file /tmp/t` with a store;
   `curl -i -H 'Origin: http://evil' http://127.0.0.1:7463` is `403`; the app's server adds
   it with the token; `sync.json` has no token; restart the app, still `Synced`.
3. GUI: token field round trip, Remove Replica, a move on the remote shows in the app's tree
   without refetching unrelated lists (log lines).

## Working agreements (unchanged)

- Build and run `pimble-app` in `--release`. At most three concurrent builds.
- Never touch `/home/joe/dev/rinch`; read rinch source from the cargo checkout.
- Report what you verified and how; report anything in this contract that turned out to
  be wrong rather than working around it.
