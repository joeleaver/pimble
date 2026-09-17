# Pimble - Claude Context

This file provides context for Claude Code sessions working on this project.
Read `docs/NEXT_SESSION.md` first (where things stand, the open decision, how to verify), then `docs/RESTART_PLAN.md`: it holds the vision, the diagnosis of what went wrong,
and the ordered plan.

## Project Overview

Pimble is an **offline-first personal information manager**:
- Nodes in a tree inside a store; stores are `.pimble` directories
- CRDT node content so devices and people can edit concurrently and merge
- Rust backend, **rinch** UI (the Slint and Makepad UIs are gone)
- Embedded JSON-RPC server (jsonrpsee over WebSocket) between UI and store
- Mounts: any subtree of any store can appear in any other store's tree
- Search: keyword (rhypedb full-text) and semantic (chunked embeddings, `semantic` feature)
- WASM plugin system for node types (skeleton)

## Rules

- **rinch comes from GitHub `main`** (`joeleaver/rinch`), never a `path` dependency on
  `/home/joe/dev/rinch`. That directory is off limits. To read rinch source use the cargo
  checkout under `~/.cargo/git/checkouts/` or a fresh clone in the scratchpad. Move the
  pinned revision with `cargo update rinch rinch-tabler-icons rinch-editor-core`.
- rinch fixes go upstream as pull requests; point `Cargo.toml` at the branch until merged.
- Always build and run `pimble-app` with `--release`; debug builds are unusably slow.
- One way to write node content. If a second path appears, one of them is a bug.

## Current design (2026-09-13)

Both CRDT documents in Pimble are yrs. There is no Automerge anywhere in the stack and no
legacy format to read: a store predating this design does not open, and the answer is to
re-import from Scrivener.

| Crate | Purpose | Status |
| --- | --- | --- |
| `pimble-core` | Node, Store, Workspace, MountRef types | Complete |
| `pimble-crdt` | `ContentDoc` (per-node content) and `StoreDocument` (tree + metadata), both yrs | Complete |
| `pimble-store` | `LocalStore` (`store.yrs` + `nodes/{id}.yrs`), `StoreManager` | Complete |
| `pimble-rpc` / `pimble-server` / `pimble-client` | RPC protocol, embedded server, WebSocket client | Complete |
| `pimble-search` | rhypedb index per store: keyword (`@fulltext`, BM25), chunked embeddings behind `semantic`, backlinks | Complete |
| `pimble-plugins` | `NodePlugin` trait, built-ins | Skeleton |
| `pimble-app` | UI library (desktop and web) plus the `pimble` desktop binary | Works: two-window live editing, persistence, local and remote mounts, replica sync UI |
| `pimble-cli` | server, stores, nodes, mounts, replica sync, search | Complete |
| `pimble-import` | Scrivener + RTF import | Complete |
| `pimble-cloud` | Pimble Cloud accounts service: users, sessions, verification mail, hosted stores, grants, keys, JWTs | Complete (phases 1 and 2a) |
| `pimble-crypto` | client-side cryptography: password-derived keys, account keys, envelopes, blobs | Complete |
| `web/` (`pimble-web`) | the same UI built for the browser with trunk; its own cargo workspace; account pages and the vault client | Complete (phases 1 and 2a) |

Node content is a yrs document (`pimble_crdt::ContentDoc`), stored as `nodes/{id}.yrs`.
The server holds one `ContentDoc` per open node, merges every `applyEdit` update, relays
the raw bytes to other subscribers, and flushes dirty content on a 750ms debounce and on
stop. The store document (tree structure and node metadata) is a second yrs document per
store, stored as `store.yrs`; `applyStoreUpdate` merges and relays it the same way.
`syncNodeContent` and `syncStoreDocument` are both stateless: state vector in, diff plus
server state vector out. `setNodeText` and `ServerSyncManager` do not exist.

### Search index

Each open store has a rhypedb database at `<store>/index/rhypedb/`, derived and
disposable (`rebuildIndex` RPC, "Rebuild search index" in the View menu). The server feeds
it in-process from the same places it broadcasts change notifications, with a 2s per-node
debounce on content. Nodes index `title` and `text` with `@fulltext`. Semantic search is
on by default in `pimble-app` (`onnx-download`: the ONNX runtime is linked statically,
the int8 `all-MiniLM-L6-v2` model downloads once into the user's data dir under
`pimble/models`, and the server warms it before any store opens; if that fails the app
runs keyword-only). Measured on the 674-node family store after rhypedb #18: model ready
in about 4 s, peak 767 MB resident, UI responsive during the backfill. Content is chunked (about 200 words, block-aligned, heading context, per-chunk
hash so an edit re-embeds one chunk) into `Chunk` objects with `all-MiniLM-L6-v2`
embeddings. Chunking works over `IndexUnit`s (prose, heading, code, table row, field,
other) produced by `ContentDoc::units()` or a plugin's `index_units`, so tables and
structured node types can index later without redesign. Both fields use rhypedb's
`english` analyzer (stemming), and the last typed word is searched as a prefix term so
results update while typing (rhypedb #17).

### Mounts (local, done 2026-09-14)

A mount node (`node_type = "mount"`, `MountRef { source_store, source_node, source_path,
source_remote }`
in `metadata.custom`) is a placeholder in the mounting store: `getNode` returns it with no
children or content, `getChildren` returns the source node's children, and
`GetChildrenResponse.store_id` names the store they live in. Every node the app holds is
addressed by that canonical `(StoreId, NodeId)`, so opening, editing, subscribing and
renaming through a mount hit the source store. Tree values are path addresses
(`node_{store}_{node}/{mount_store}_{mount_node}` per mount level) so the same node can
appear in several places; `parse_tree_value` returns the canonical pair. The server opens
a source store it needs (registry, then the `source_path` hint) and treats it as an
ordinary open store; the app discovers it via `listStores` when it sees an unknown store
id and adds it to the open-store list. Cycles are rejected at `createMount`. Creating a
child of a mount node is an error; the app creates under the source instead. Contract:
`docs/history/MOUNTS_CONTRACT.md`.

### Remote mounts (done 2026-09-15)

A mount whose source store is not on this machine resolves by making the source a replica
here and then resolving locally. `RpcHandler::resolve_mount` is the one resolution path
(`getMountState`, `getChildren` on a mount, `createMount`): the local chain first (open,
registry, `source_path`), then `MountRef.source_remote` (a URL only, filled by `createMount`
when the source is a linked replica), then the mounting store's own `sync.json` remote
(a store and the stores it mounts usually live on the same server). A remote candidate
means creating a replica exactly as `addRemoteStore` does (`create_replica_from`), in a
detached task; the RPC answers `Connecting` at once and a per-source in-flight set makes
two resolutions start one task. Mount state is derived from the source's link, never
stored: no link or `Synced` is `Live`; `Syncing`/`Offline` is `Cached { last_sync }` once
the link has ever synced (`SyncConfig.last_sync`, written on transitions into and out of
`Synced`) and `Connecting` until then; nothing to try or a failed attempt is
`Unavailable { reason }`. The server remembers which mounts it resolved per source
(`resolved_mounts`, pruned when the mounting store closes or the mount node is deleted)
and sends `StoreChangeKind::MountStateChanged { node_id, state }` on the mounting store
when the source's link changes category or a background creation ends; links never
forward it. A store opened implicitly (mount resolution, replica creation) gets everything
`openStore` gives one: index, sync link from `sync.json`, tree repair (`adopt_newly_opened`).
`getChildren` on a mount whose source is not open is an error naming the state. App:
"Mount Remote Store Here..." (the Add Remote Store modal with a target node; two RPCs,
`addRemoteStore` skipped when the store is already open here), label suffixes
"(connecting...)", "(offline copy)", "(unavailable)", and a same-kind dedup on
`MountStateChanged` (a down link flaps `Syncing`/`Offline` on every backoff tick). CLI:
`mount-remote-store`, `mount-state` prints all four states. Contract:
`docs/history/REMOTE_MOUNTS_CONTRACT.md`.

### Replica sync (local ↔ remote server, done 2026-09-15)

A local store can be a replica of the same store (same `StoreId`) on another Pimble
server. One `SyncLink` per linked store lives in the local server
(`pimble-server/src/sync_link.rs`), persisted as `<store>/sync.json` and restarted by
`openStore`. It reconciles with the stateless primitives (`syncStoreDocument` /
`syncNodeContents`: state vector in, diff out; then push back only what the remote lacks) and then
shuttles live updates both ways: remote `storeChanged` notifications carry the yrs bytes
(`TreeStructure` from `applyStoreUpdate`, `ContentUpdated` from `applyEdit`) and are
applied through the handler's own `apply_edit`/`apply_store_update` with
`client_id = "sync-link:<uuid>"`; local notifications reach the link through an in-process
broadcast and are forwarded unless their source is this link's own id, so an edit travels
a whole chain of servers; a bounce back to a server that already has the change is a no-op
merge there and sends nothing, which is what stops echo storms. `addRemoteStore` creates an empty replica
(never `StoreDocument::new`: two roots for one id would merge into duplicated children) in
`<data dir>/pimble/replicas/<store id>.pimble` and links it; `setStoreSync` links or
unlinks an existing store. A store already open on this server is refused as a replica, and
a store cannot be linked to the server it lives on. `removeReplica` stops, closes and
deletes a replica (only inside the replicas directory; not while unsynced unless `force`).
Partial replication and TLS are not done. Contract:
`docs/history/SYNC_CONTRACT.md`; CLI: `server --addr --open --token-file`, `token`,
`add-remote-store`, `link-store`, `unlink-store`, `sync-state`, `remote-stores`,
`remove-replica`, `mount-remote-store`, `move-node`, `delete-node`, `PIMBLE_SERVER`,
`PIMBLE_TOKEN`. `set-node-text` is an `applyEdit` of `ContentDoc::replace_plain_text`
(an edit of the node's existing document); `updateNodeContent` replaces the document
wholesale and is only right for a node whose content was never written (the importer),
since a replacement shares no history with the copies replicas hold and merges in beside
the old paragraphs instead of replacing them.

### Hardening (done 2026-09-14)

Contract: `docs/history/HARDENING_CONTRACT.md`.

- **Auth at the HTTP edge** (`pimble-server/src/auth.rs`): any request with an `Origin`
  header is refused (403; browsers can open WebSockets to loopback), and a server with a
  token requires `Authorization: Bearer` or `X-Api-Key` (401). A server without a token
  refuses to bind beyond loopback; the app's embedded server is tokenless on
  `127.0.0.1:7462`. Server token file: `<config dir>/pimble/server-token`.
- **Credentials** a server uses for remotes live in `<config dir>/pimble/credentials.json`
  (0600, keyed by origin), saved after a connection with them succeeds. Never in
  `sync.json` (always `auth: none`), never in an RPC response. Clients never connect to a
  remote: `listRemoteStores` goes through the local server.
- **No-op updates change nothing.** A yrs diff is never empty (`[0, 0]` plus the whole delete
  set), so `is_empty()` checks on diffs are always wrong. `ContentDoc`/`StoreDocument`
  report whether a merge changed anything, and `diff_if_peer_lacks_it` decides whether a
  reconcile pushes. A reconnect between synced servers sends no edits.
- **Tree repair** (`StoreDocument::repair`, deterministic, surgical list edits) runs at open
  and after every changing `applyStoreUpdate`; the result is broadcast as `TreeStructure`
  with `source_client_id: None`. `validate_tree` reports exactly what repair fixes.
- **Notifications name parents** (`NodeCreated`/`NodeDeleted { parent_id }`, `NodeMoved {
  old_parent_id, new_parent_id }`, `TreeStructure { node_ids }`); the app refetches only
  those lists. Deleting a node deletes its subtree; the root cannot be deleted.
- **`closeStore` releases the search index** before answering (the old flaky reopen test).
- rinch's Tree re-renders a row only when its `TreeNodeData` changes: anything a row
  snapshots at render time (context-menu `disabled` states) must be reflected in that data
  (the store row's unrendered label carries the link state).

### Collaboration shape (keep these invariants)

- The app has ONE editor pane and one thread-local `EditorHandle` (`pimble-app/src/editor.rs`).
- Local edits: `EditorHandle` outbound closure -> `BackendCommand::BroadcastChanges` ->
  server persists and relays -> peers receive `BackendEvent::RemoteChanges` ->
  `EditorHandle::collab_receive`.
- Never wrap the editor in a document-model layer in the sync path. Never call
  `load_html`/`load_doc` on a collaborating editor.
- Rinch's collab scope is flat text blocks (paragraph, heading, code block), nested bullet
  and ordered lists, and the starter-kit marks (bold, italic, underline, strike, code, link,
  highlight, text colour, sub/superscript). Block quotes, tables, images and hard breaks in a
  collaborating document fail loudly by design. `pimble_crdt::Block` is that scope as data;
  `ContentDoc::from_blocks` builds a document from it (the importer's way in).

### Tree appearance (done 2026-09-15)

A node's custom icon and colour live in `metadata.custom` under
`pimble_core::custom_keys::{ICON, COLOR}` (`NodeMetadata::icon/color/set_icon/set_color`):
a Tabler icon name and a `#rrggbb`, replicated with the store like any metadata and
written through `updateNodeMetadata` (`BackendCommand::SetNodeAppearance`). The app's
`appearance.rs` holds the picker's curated icon table and palette; `icon_by_name` resolves
any Tabler icon (`ALL_ICONS`), and the picker's search box (`icons_matching`) offers all
of them, rendered through the `IconGlyph` component so a reactive `for` can draw icons.
A store row takes its root node's icon and colour (`register_opened_store` fetches the
root; the store row's menu has "Appearance..." too). Rows snapshot icon and colour at
render time, so the row's `TreeNodeData` label carries them (with the paste flag) and
`NodeLoaded` bumps the tree when they change. `display_color` lifts a dark stored colour
on the dark theme and caps a light one on the light theme, at render time and reactively
on `AppStore::dark_mode`; the stored value is never altered. "Appearance..." opens the
picker (every click applies at once; the tags field applies on Enter or Done). Tags are
not shown in the tree. The Scrivener importer maps a binder item's label to colour plus a
tag with the label's name, and its `IconFileName` to an icon where one matches. View >
"Toggle Dark Mode" switches the theme at runtime (`rinch::update_theme`, the editor's
`set_dark_mode`, `state.json` `dark_mode`); the app stylesheet uses only rinch's semantic
colour variables, never the dark palette directly, so both schemes work.

### Cloud, phase 1 (done and deployed 2026-09-15)

Contract: `docs/CLOUD_CONTRACT.md`; operations: `docs/DEPLOY.md`. Everything server-side
runs on jkbase (`~/dev/jkbase`, Joe's own platform; read it, never edit it) as one project
`pimble` on one origin, `https://pimble.app` (the platform subdomain `pimble.jkbase.app`
works too): `site/` at `/` (a named `[sites.site]`; jkbase ignores `[hosting]` once any
`[sites.*]` exists), `web/` at `/app/`, `crates/pimble-cloud` at `/api/*`, and `pimble-cli
server` at `/rpc`, all declared in `jkbase.toml`. Deploy with `git push jkbase` (the
remote's refspec targets the platform's `main`); the root and `web/` lock files must pin
the same `wasm-bindgen` (the trunk buildpack reads the root lock); the hosted server
target takes about 12 minutes to build.

- **Identity.** A user is a UUID `sub` with an argon2id password in the accounts service's
  managed RhypeDB (`crates/pimble-cloud/schema.rhype`). A grant is `(user, store, role)`,
  role `owner | editor | reader`; a store always keeps at least one owner. A token is an
  EdDSA JWT minted by jkbase-Auth (`https://auth.jkbase.app/v1/projects/pimble`) or, in
  development, by the service's own Ed25519 key; `aud` is `pimble` and the custom claims
  carry `email` and `stores: { <store id>: <role> }`. Tokens live an hour, so Pimble servers
  hold no account state: they verify against a JWKS and read grants from the token.
- **pimble-server.** `AuthMiddleware` resolves a `Principal` per connection (`Service` for
  the static token or a tokenless loopback server; `User { sub, email, grants }` for a JWT)
  and attaches it to the request extensions; every store-scoped RPC declares
  `with_extensions` in `pimble-rpc` and calls `authorize(principal, store_id, Read|Write)`
  first. Readers read, editors and owners write, anything touching a mount authorizes
  against the source store too, `listStores` and `search` filter to readable stores, and
  store lifecycle RPCs (`createStore`, `openStore`, `closeStore`, `addRemoteStore`,
  `listRemoteStores`, `setStoreSync`, `removeReplica`) are `Service`-only. Denied is
  JSON-RPC `-32004`. Credentials arrive as `Authorization: Bearer`, `X-Api-Key`, or the
  `access_token` query parameter (a browser WebSocket cannot set headers). An origin
  allowlist (`--allow-origin` / `PIMBLE_ALLOW_ORIGINS`) admits browsers; the embedded app
  server sets none and refuses every `Origin` as before. `pimble-cli server` takes
  `--jwks`, `--issuer`, `--allow-origin`, `--stores-dir` (opens every `*.pimble` in it) and
  env fallbacks for every flag (`PIMBLE_ADDR`, `PIMBLE_SERVER_TOKEN`, `PIMBLE_JWKS_URL`,
  `PIMBLE_JWT_ISSUER`, `PIMBLE_ALLOW_ORIGINS`, `PIMBLE_STORES_DIR`).
- **pimble-cloud.** axum under `/api/v1`: signup, login, logout, me, token, stores (create
  goes through `createStore` on the hosted server with the static token), members,
  releases (GitHub, cached), `.well-known/jwks.json`, health. Sessions are opaque 30-day
  tokens hashed at rest, sent as the `pimble_session` cookie or a bearer header. Tests run
  against a real `rhypedb-server` process (`~/dev/rhypedb/target/debug/rhypedb-server`) and
  skip when it is absent.
- **App split.** `pimble-app` is a library: UI, state, events, editor wiring,
  `protocol.rs` (`BackendCommand`/`BackendEvent`), `commands.rs` (`process_command`, shared
  by both backends), `rinch_editor.rs` (the editor types from whichever rinch backend is
  in play). The embedded server, tokio thread and persistence are behind the default
  `native` feature; the `web` feature takes `rinch-web`'s collaboration adapter. rinch is
  declared in `crates/pimble-app/Cargo.toml` with default features off, not in the root
  workspace. `--no-default-features` alone means "the UI library"; the keyword-only desktop
  build is `--no-default-features --features native`.
- **web/** is its own cargo workspace (`pimble-web`, trunk, `public_url = "/app/"`). On
  start it `POST`s `/api/v1/token` with the session cookie (401 sends the visitor to
  `/login.html`), connects `PimbleClient` to the returned `rpc_url` with the token in the
  query string, fills the tree from `listStores`, refreshes the token five minutes before
  expiry and reconnects with backoff. Both `Cargo.lock`s must pin the same rinch revision
  (`cd web && cargo update -p rinch --precise <sha>` alongside the root update).
- **Site and CI.** `site/` is static HTML/CSS/JS. `.github/workflows/ci.yml` checks and
  tests; `release.yml` builds Linux and Windows packages on a `v*` tag and attaches them
  to a GitHub release, which the download page reads through `/api/v1/releases`.
- **Email verification (done 2026-09-15):** signup creates an unverified user and mails a
  24-hour link through Resend (`m.pimble.app`; `LogMailer` logs the link when no key is
  set); login refuses unverified accounts with `email_unverified`; resend is limited to one
  per address per minute, counting signup's own send.

### Cloud, phase 2a: end-to-end encryption (code done 2026-09-16, deploy pending)

Contract: `docs/CRYPTO_CONTRACT.md` (status header lists the decisions and the open
store-name question). Pimble Cloud holds ciphertext only.

- **`pimble-crypto`** (native and wasm): Argon2id (32 MiB, 3 passes) split by HKDF into
  `auth_key` (sent as the password; the server hashes it again) and `kek`; X25519 and
  Ed25519 account keys wrapped under the password KEK and a recovery-code KEK; signed
  sealed-box `KeyEnvelope`s; the `PB` blob layout over XChaCha20-Poly1305 with
  `"{store_id}/{doc_id}"` as associated data; `verify_envelope` for servers.
- **Vault stores** (`Store.kind = vault`): no StoreDocument, ContentDoc or index on the
  server; per document an append-only log plus a snapshot under `<store>/vault/{doc}/`;
  RPCs `vaultAppend/Fetch/Snapshot/ListDocs` (reader/editor), `VaultAppended`
  notifications carry the blob; a snapshot deletes every log entry at or below its
  number, so it may only be stamped with `VaultCursor::applied_through` (every entry up
  to it applied locally, never "my latest append"), its request says so
  (`covers_prefix`; one without it is acknowledged and ignored), and a snapshot not newer
  than the held one changes nothing; every other store-scoped RPC answers `-32005`; `createStore`
  takes `kind` and a chosen `store_id`. Limits: 4 MiB per blob, 64 MiB per log before
  `-32006 snapshot_required`.
- **Accounts service**: users carry kdf parameters, public keys and the wrapped key blobs;
  `GET /kdf` (HMAC decoy for unknown emails), signup and login with `auth_key`,
  `GET /me/keys`, `GET /users/lookup`, `POST /stores { name, kind, store_id? }`,
  `GET`/`PUT /stores/{id}/keys` with ownership rules and signature checks. The account
  pages live in the web app (`/app/signup`, `/app/login`, `/app/account`); the site's
  pages redirect there.
- **Desktop**: `AuthMethod::CloudSession` mints a JWT before every connect; `keys.json`
  (0600) holds the unwrapped account and store keys; Service-only RPCs `cloudSignIn/
  SignOut/Status/HostStore/ListHostedStores/AddHostedStore`; `VaultLink` (mode `vault` in
  `sync.json`, per-document `last_seq`) pulls, decrypts and applies blobs, encrypts and
  appends local updates, drops echoes by seq, snapshots every 200 appends; it keeps its
  local-change subscription across disconnects and records in `<store>/vault-link.json`
  what the remote is known to hold (a state vector per document, advanced only by
  updates that continue from it) plus the documents that changed while it was down, and
  every reconnect pushes what the remote lacks (the hosted twin is ciphertext and cannot
  answer with a state vector; before 2026-09-17 an edit made while the link was down was
  never sent, and every later edit from that device sat pending on every other one);
  `Store.sync_mode`/`GetStoreSyncResponse.sync_mode` tell the app it is encrypted. CLI
  `cloud-*` commands. The desktop sign-in UI is not built yet.
- **Web app**: keys in memory only (a reload asks for the password to unlock); one
  `PimbleClient` per endpoint with per-store `rpc_url` and token ready for relayed shares;
  the vault client holds a decrypted `StoreDocument` and per-node `ContentDoc`s in the
  browser, answers tree and editor commands from them, encrypts outbound edits and
  searches client-side; it outlives its socket (replaced on every token refresh), so on
  every connect `catch_up` fetches what each held document missed and resends what a
  failed append left unsent (`known_sv`/`unsent` per document); the explorer's `+` creates an encrypted store through the accounts
  service and re-mints the token; menus are data in `crates/pimble-app/src/menus.rs` and the
  browser mounts them through `rinch_web::mount_with_menu_bar` (rinch PR #791, merged to `main` 2026-09-16).
- **Verified by the PM** in a browser through trunk's proxies against the real stack:
  signup with the recovery code, verification, unlock, an encrypted store, a node typed in
  one tab, only `PB` ciphertext on the hosted disk (no typed words, title or store name in
  the vault), a second tab decrypting and receiving edits live.
- **Phase 2b** (designed for, not built): sharing in place with subtree grants and per-share
  keys, invitations, the relay tier (store nothing), then teams.

### Desktop account UI (done 2026-09-16)

Contract: `docs/DESKTOP_ACCOUNT_CONTRACT.md`. `native` only; the browser keeps its own
pages. An "Account" menu (between File and Edit) holds "Account..." and "Add Hosted
Store...". The Account modal has two faces: the sign-in form (service URL defaulting to
`https://pimble.app`, email, password) and, once signed in, the email, the URL and
"Sign Out"; a sign-in survives an app restart because the embedded server's keystore
(`<config dir>/pimble/keys.json`) holds the session, and the app sends `CloudStatus` on
every `Connected`. The status bar shows "Signed in as <email>" (click opens the modal).
A store row's menu has "Host on Pimble Cloud..." (disabled for a linked store, a replica
or a vault-kind store; opens the Account modal with a hint when nothing is signed in) with
a confirmation that names the store and the account. "Add Hosted Store..." lists the
account's vault stores not open here and adds one as a replica (`StoreOpened`, like
`addRemoteStore`). Every cloud command answers with an event naming its operation
(`CloudStatusChanged`, `CloudError { op }`, `CloudHostedStoresListed`, `CloudStoreHosted`),
never the generic `Error`. `StoreSyncChanged` carries the server's `sync_mode`, written
into the store's `Store` signal; the badge reads it: `encrypted · synced` for a vault
link, `synced` for a plain one. Two server fixes came out of verifying it: adopting an
implicitly opened store (`adopt_newly_opened`, drained by the next `getChildren`) now
starts the link `sync.json` describes instead of always a plain one, `ensure_link_started`
refuses a store that has a vault link, and a vault replica's manifest root is rewritten
from the store document once the tree is pulled (`adopt_document_root`, also at
vault-link start), so a reopened replica no longer reports a placeholder root.

## Key Files

- `docs/RESTART_PLAN.md` - vision, diagnosis, decisions, ordered plan
- `docs/ARCHITECTURE.md` - the architecture, including mounts
- `crates/pimble-core/src/node.rs` - Node, NodeId, NodeLink, MountRef
- `crates/pimble-crdt/src/store_document.rs` - StoreDocument (tree + metadata CRDT)
- `crates/pimble-store/src/local.rs` - LocalStore
- `crates/pimble-rpc/src/methods.rs` - RPC API trait
- `crates/pimble-server/src/handler.rs` - RPC method implementations
- `crates/pimble-app/src/app.rs` - UI tree, menus, rename, drag-and-drop
- `crates/pimble-app/src/editor.rs` - editor pane + collaboration wiring
- `crates/pimble-app/src/protocol.rs` - `BackendCommand`/`BackendEvent`; `commands.rs` - `process_command`, shared by desktop and web
- `crates/pimble-app/src/backend.rs` - desktop background thread and embedded server (`native` only)
- `crates/pimble-server/src/auth.rs`, `jwt.rs`, `principal.rs` - HTTP-edge auth, JWT verification, `Principal` and `authorize`
- `crates/pimble-cloud/src/routes/` - the accounts API; `schema.rhype` - its RhypeDB schema
- `web/src/backend.rs` - the browser backend (token, connection, reconnect)
- `jkbase.toml`, `docs/DEPLOY.md` - deployment
- `crates/pimble-app/src/events.rs` - `BackendEvent` -> UI state
- `crates/pimble-app/src/appearance.rs` - tree icon and colour choices, name lookup, dark-theme legibility

## Build & Run

```bash
cargo check --workspace
cargo build -p pimble-app --release
cargo run -p pimble-app --release          # starts the embedded server itself
cd web && trunk build --release            # the browser build, dist/ (cargo install trunk once)
```

The full local stack (accounts service, a `pimble-cli server` in JWT mode, the site and
the web app) is described in `web/README.md` and `docs/DEPLOY.md`.

The app has the rinch `debug` feature on, so the rinch MCP tools (`list_apps`, `connect`,
`screenshot`, `dom_tree`, `click`, `type_text`) can drive a running instance.

## Dependencies

- `rinch` (git main), declared in `crates/pimble-app/Cargo.toml`: default features off, `native` adds `desktop, file-dialogs, clipboard, debug, collaboration`, `web` adds `rinch-web` with `collaboration`; software rendering (no `gpu`, which would need rinch's wgpu fork patch)
- `rinch-editor-core` (git main)
- `yrs` 0.27 for both CRDT documents; `rinch-editor-collab` (git main) wraps it for node
  content's rich-text schema, the store document uses `yrs` directly (Maps and Arrays)
- `jsonrpsee` 0.24
- `rhypedb-engine`/`-schema`/`-query`/`-embed` (git master) for `pimble-search`; `semantic` turns on the code paths, `onnx-download` or `onnx-dynamic` picks the ONNX link mode
