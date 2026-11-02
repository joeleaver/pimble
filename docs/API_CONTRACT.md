# API contract: one Pimble API for the app, the MCP and plugins

Status: drafted by the PM on 2026-11-02 from Joe's direction ("the plugin system should use
the same internal API as the main app does, and if that isn't cleanly abstracted yet, it
might be good to pause and get it to there"; "let's go crazy with internal refactoring").
Written from a read-only audit of `master` at `898a548` (three passes: the app's command
layer, the MCP, the RPC surface and the web vault client). Joe, 2026-11-02: "Do it. Go
ahead and do all six waves before the next release." He did not answer the open decisions
one by one, so the work goes ahead on the recommended answers (marked "taken" at the end);
any of them can still be overruled. Branch `api`.

Why now: view nodes (kanban first) and WASM plugins are next
(memory `project_views_and_plugins_2026_11_02`), and a plugin must be able to do what the
app does, the way the app does it. Today there is no such "way": there are three.

## 1. Where it stands (the audit)

### Three layers, none of them "the API"

1. **RPC** (`pimble-rpc`, 75 methods, `PimbleClient` a thin typed wrapper). Authorization,
   notifications and `applyEdit` live here. Used by the app (through `commands.rs`), the
   MCP, the CLI and the sync and vault links.
2. **Document libraries** (`NodeDoc`, `Tree`, `markdown`, `pimble-board`, `pimble-sheet`,
   `pimble-discussion`). Pure and shared. In good shape.
3. **The app's `BackendCommand`/`BackendEvent`** (66 commands, 52 events, `protocol.rs`)
   plus `AppStore` signals. This is what the UI talks to, on both targets: desktop and
   plain web stores go through `process_command` to RPC, encrypted web stores go to
   `web/src/vault.rs`, which answers the same commands itself. It is fire-and-forget with
   events, carries no request ids (errors are routed to modals by guessing,
   `events.rs:584`), and its rules read UI state.

### What is right already

- **Almost every write is `applyEdit`**: text, fields, boards, sheets, discussions build a
  yrs update with the libraries and send it (app `BroadcastChanges`, MCP `edit_doc`
  `tools.rs:176`, vault `merge`).
- `tree_drop::judge` (`pimble-app/src/tree_drop.rs:98`) is already pure; only `facts_of`
  reads `AppStore`.
- The `BackendCommand` seam already has two implementations (RPC, vault) behind one
  interface. It is at the wrong level (UI-shaped, no answers), not missing.

### What is wrong

**Second write paths (CLAUDE.md: "If a second path appears, one of them is a bug"):**
- `RenameNode` and `SetNodeAppearance` read the whole metadata, change it and write it all
  back with `updateNodeMetadata` (`commands.rs:215-260`); `LocalStore::update_node_metadata`
  drops custom keys missing from it. A concurrent edit of another key or field on another
  device can be lost. The vault does targeted `Tree::set_title`/`set_custom`
  (`vault.rs:3723-3755`), which is right.
- `updateNodeContent` (`handler.rs:4158`) seeds a document wholesale, skips `derive_kinds`
  and repair, and is still used by the editor seed (`editor.rs:281`) and the importer
  (`import.rs:375`).

**Security:** `loadWorkspace`/`saveWorkspace`/`createWorkspace` take no `Extensions`, check
nothing, and read or write any path the caller names (`handler.rs:5199-5247`). A share member
connected to the hosted server can reach them. Nobody calls them.

**Rules written two or three times, already disagreeing:**

| Rule | Copies | Disagreement |
|---|---|---|
| Create under a mount → its source | `tree_keys.rs:222`, `boards.rs:2374`, `app.rs:1724`; MCP `create_node`/`create_sheet`/`create_canvas` | `app.rs:1493`, `paste_mount_here`, MCP `start_discussion` and `move_node` do not redirect |
| Move judgement | app `tree_drop` (mount, into-itself, read-only, stays-put, leaves-a-share, cross-store); server prechecks; vault `vault.rs:1919` | MCP checks none before sending; different share sentences; vault does not dedupe `moveNodes` ids |
| Naming nodes by path | MCP `nodes::resolve_path`, `McpBook::find_nodes`, app `AppBook::find_nodes` | MCP tool paths must start at the store, formula paths may start anywhere |
| Following links | app `follow_resolution` (hops on `StoreNotHere`), `text_boards.rs:758` copy, MCP `resolve_link` (no hop), MCP `nodes::resolve` (no `resolveLink` at all: a moved node "names no node") | yes |
| Canvas locks | app `boards/tools.rs` (three places), MCP `Scene`/`change_element`; `BoardTxn` enforces nothing | app delete skips locked elements, MCP refuses the call; "notes and fields may change on a locked element" is MCP-only |
| New sheet / canvas / discussion recipes | app `sheets.rs:738`, `boards.rs:1820`, `discussions.rs:846`; MCP `ops.rs:1410`, `canvas_tools.rs:1505`, `discussion_tools.rs:328` | an attached discussion made by the MCP has no icon; MCP creates in two writes |
| Sheet type-change carry-over, `write_formulas`, tied-table check | app `sheets.rs:2080`, `in_text.rs:467,553`; MCP `ops.rs:1196`, `sheet_tools.rs:1052,1121` | copied verbatim |
| Embed atoms in Markdown | MCP hand-built strings and line scanners (`sheet_tools.rs:506,511,522`, `discussion_tools.rs:141,343`) | attribute order must match rinch by hand |
| Access | `node_access`/`_tracked`/`_in` (`state.rs:1355`), MCP `edit_doc` (checks `Read` only), vault `access_of`, server `judge_access`/`Reach` | MCP lets a commenter through to a server refusal; "unknown access = Full" is unstated policy |
| `derive_kinds`, `DocShape` | `handler.rs:730`, `vault.rs:3827` | verbatim copy |
| Partial-replica repair, `document_root`, `list_deleted`, link hops, node assembly | `pimble-store`/`handler.rs` and `vault.rs` | copied because `web` cannot depend on `pimble-store` |
| Vault protocol (encrypt, keys, snapshot policy) | `vault_link.rs`, `vault.rs` | `SNAPSHOT_EVERY` and its due rule already differ |
| Presence frames | app `presence::frame_for`, MCP `presence.rs` | different session naming, sequence, TTL |
| Author identity | app `shares::author_in` (per-share name), MCP `whoAmI` per write (two copies) | different names |
| Node-document session plumbing (reconcile, apply remote, send, hand across panes) | `editor`, `boards`, `sheets`, `discussions`, `text_boards` | five copies |
| Reconnect and resubscribe | app `backend.rs`, web `backend.rs`/`endpoints.rs`, MCP `connection.rs`, sync link, vault link | five copies |

**Hidden API:** the app's cache policy (`events.rs:302-442`, `1218-1351`: which lists to
refetch for each notification, access cascades, mounts sourced from a parent) is the
largest unwritten part of the API. The MCP has no cache at all and walks whole stores on
every `find_node`, `list_tables`, `rows_pointing_here` and formula load.

**Errors are strings:** `pimble-client`'s `rpc_error` (`client.rs:114`) drops JSON-RPC
codes; the web tells refusals apart by matching text (`vault.rs:4042`), the MCP guesses
absence from error text (`sheets/load.rs:40`).

**Dead RPCs:** `getNodes`, `haveBlobs`, and the three workspace RPCs.

## 2. The decisions (proposed)

1. **One crate, `pimble-api`, wasm-safe, is what every client uses**: the app (desktop and
   web), the MCP, the CLI's node commands, and plugins. Request and response: typed
   `async` methods returning `Result<T, ApiError>`. Notifications are a separate stream.
   `BackendCommand` becomes the app's thin adapter onto it, then disappears where it
   only forwards.

2. **Two traits, not one.** `Pimble` is the data API (stores' trees, nodes, edits,
   moves, blobs, search, presence, discussions, subscriptions). `Host` is the service
   side (open/close/create stores, replicas, sync links, cloud account, hosting, sharing,
   agents). The vault implements `Pimble` and refuses or omits `Host`; plugins get
   `Pimble` only.

3. **The API holds node documents, not `Node` copies.** Both implementations become "a
   local `Tree` of the documents this client holds, plus a transport": the vault already
   is exactly that (a decrypted `Tree` fed by `vaultFetch` and the store subscription);
   the RPC implementation holds the documents it has fetched and applies the update bytes
   `storeChanged` already carries. Reads (`children`, `parent`, `ancestors`, `title`,
   `fields`, access) are answered from held documents through `NodeFields::into_node`. That
   replaces the app's refetch policy (R19), the MCP's tree walks, and the vault's separate
   assembly code, and it is how "everything is co-editable" already works on the server.
   What is held is bounded by what is asked for (and a store's tree when a view needs it),
   not every document of every store. *(Open decision 3.)*

4. **One write.** `api.edit(addr, |doc: &mut NodeDoc| ...) -> Result<Edited, ApiError>`:
   access pre-check (advisory), run the closure on the held document, the "changed
   anything?" check, `applyEdit` (or `vaultAppend`), the caret for presence. Rename,
   tags, icon, color and fields become targeted `NodeDoc` edits through it.
   `updateNodeMetadata` read-modify-write is gone; `updateNodeContent` is gone (the
   importer creates with `createNode{init}`, the editor seeds a held empty document with
   an ordinary edit). Structural operations stay their own RPCs (`createNode`, `moveNode`,
   `moveNodes`, `transplantNode`, `deleteNode`, `undeleteNode`, `createMount`) because the
   server merges and repairs those.

5. **Planning is pure and shared.** `pimble-api` holds the judgements as functions over a
   `TreeView` (the held documents): `plan_move(view, nodes, target, place) ->
   Result<Plan, Refusal>` (today's `judge` plus `facts_of` without `AppStore`), one
   `create_parent(view, row)` (the single mount redirect), `can_delete`, `capabilities(store,
   node)` (today's `state.rs:343-620` menu rules), labels (`display_label`), author names.
   The explorer, a kanban view, the MCP and plugins get the same refusals and the same
   sentences, before anything is sent. The server still judges, and is authoritative.

6. **One naming and link service.** Link, bare id, path from a store, path from anywhere;
   follows `became` and `StoreNotHere` hops with one `stop_sentence` rule; mount-to-source
   as an explicit option; shortest unique display path. Replaces `nodes.rs`, both
   `find_nodes`, `follow_resolution` and its `text_boards` copy.

7. **Recipes by kind.** `create_document`, `create_sheet`, `create_canvas` (optionally from
   a picture), `create_discussion` (attached or standalone, with its first message), each
   one `createNode{init}`: type, icon, `EXPLICIT_TITLE`, first content. Plugin node types
   register recipes the same way.

8. **Rules go into the library that owns the data.** Canvas locks and their sentences into
   `pimble-board` (`BoardTxn` refuses); sheet operations by name, value parsing, carry-over,
   formula checks, `move_table` and an RPC-free `Book` over a `TreeView` into `pimble-sheet`;
   the embed atom format (read and write) into `pimble_crdt::markdown`; presence frame
   building into `pimble-api`. `derive_kinds`, `DocShape`, scope repair, `document_root`,
   `list_deleted`, link hops, move prechecks and the access judgement move into `pimble-crdt`
   (or a small wasm-safe crate beside it) so the server and the vault call the same code.

9. **Errors are data.** `ApiError` names what happened (ReadOnly, CommentOnly, NoGrant,
   NotFound, Ambiguous { candidates }, Locked, Tied, EncryptedStore, OwnerOffline,
   NotAvailable, Offline, ...) and carries the sentence a person reads. `pimble-client`
   keeps JSON-RPC codes; the server maps its refusals to codes. No caller matches text.

10. **Capabilities are visible.** A store answers what it can do (mounts, pictures, sync
    links, semantic search, index rebuild). The vault's faked `IndexRebuilt { 0 }` and
    synthesized `GetStoreSync` become a capability saying no. Search results say how
    they were found.

11. **Connection is the implementation's job.** The RPC implementation owns reconnect,
    resubscribe, token refresh hooks and per-node subscription counting; callers see
    `Offline` and a resumed stream. A `Router` above the trait picks the implementation per
    store (the web's `Endpoints`, relayed stores, plain versus vault) and owns operations
    that span two stores (transplant).

12. **Sessions for open documents live in the API.** One `DocSession` (subscribe once
    however many views hold it, reconcile on reconnect, apply remote, send local, hand the
    same delta to the other views of this client) replaces the five copies. The app keeps
    only presentation (panes, rinch's editor wiring); a plugin view uses the same session.

13. **Behavior differences are settled to one answer** (open decision 5).

## 3. Waves

Each wave leaves the app, the web and the MCP working and is releasable on its own.

**Wave 0, hygiene (small, ships first).**
- Remove the workspace RPCs (security). `getNodes` and `haveBlobs` stay: they are
  authorized and tested, `getNodes` is what held documents fetch with (wave 2) and
  `haveBlobs` is images wave 2's.
- `pimble-client` keeps error codes (`ClientError::Server { code, message }`, `code()`,
  `is_forbidden()`); the server already gives each refusal a code.
- Rename and appearance become targeted `applyEdit`s (the lost-update bug), through
  `PimbleClient::edit_node`, the first piece of decision 4's one write.
  `updateNodeMetadata` is removed (the server's own share-marker writes keep their
  under-one-lock `edit_node_metadata`).
- The importer and the editor seed stop using `updateNodeContent` (the app's
  `SetNodeContent` becomes `ApplyUpdate`, an answered `applyEdit`); the RPC is removed.
- Update `docs/MCP_CONTRACT.md` where it is stale (`write_new`, `mcp:<uuid>`, `Service`).

Built 2026-11-02 on branch `api`.

**Wave 1, rules into libraries (no behavior change except decision 5's answers).**
`derive_kinds` and friends into `pimble-crdt`; scope repair, `document_root`,
`list_deleted`, link hops, access judgement made wasm-safe and shared by server and
vault; canvas locks into `pimble-board`; sheet operations, carry-over, `write_formulas`
into `pimble-sheet`; embed atoms into `markdown`; one vault protocol module shared by
`vault_link.rs` and `vault.rs`. Each move deletes its copies in the same commit.

Built 2026-11-02 except the shared vault protocol (it runs beside wave 2): tree and
server rules in `pimble_crdt::rules` (`derive_changes`, `Tree::repair_held`,
`document_root`, `deleted`, `link_hop`, `node`, `check_move`, `move_order`) and
`StoreAccess::of_role`/`on_document`; canvas locks, the layer rule and the new-canvas
recipe in `pimble-board` (`locks.rs`, `recipe.rs`); sheet operations, path matching and
moves in `pimble-sheet` (`ops.rs`, `book.rs`); embed atoms as `markdown::Embed`.
The shared vault protocol, built 2026-11-02: `pimble_rpc::vault` beside `VaultCursor`
(`DocKeys`, `seal`, `scope_key_ids`, `NewDocument`, `snapshot_due` with the desktop's
log-length rule, `should_apply`, `fetched_entries`), used by `vault_link.rs` and `vault.rs`.

**Wave 2, `pimble-api` with the RPC implementation, and the MCP moves onto it.** The trait,
`ApiError`, held documents, the one write, naming and links, recipes, presence. The MCP is
the first consumer because it has no UI state: its 62 tools become thin, its tree walks
become lookups. The MCP's tests are the check.

Built 2026-11-02: `crates/pimble-api` (native and `wasm32-unknown-unknown`, no tokio).
`Backend` (transport, `async-trait`, `?Send` on wasm) and `RpcBackend` over
`PimbleClient`, which asks a `Connector` for a live client on every call (the MCP's
connection joins or starts the local server; `Connected` holds one made elsewhere).
`Pimble` holds a `Tree` per store with each node's server judgement (access, attachment
count), subscribes to a store the first time it reads one, and drains the subscriptions
before every read, so nothing is spawned; a subscription that ends is made again and
what is held reconciled with `syncNodes` (`Change::Resynced`); `next_change` is the
stream for a caller that listens. `hold_store` holds a store whole in two `syncNodes`,
after which walks and names are lookups. The one write is `Pimble::edit` (the document
brought up to date first, the access pre-check including comment-only, the "changed
anything?" merge, `applyEdit`, the caret). Naming and links in `names.rs` (paths from a
store's name or anywhere through `pimble_sheet::book::find_by_path`, `follow` with the
app's `stop_sentence`, `path`, `short_name`, `ancestors`); `create_parent` is the one
mount redirect, used by every create and by `move_node`. Recipes (`document`, `sheet`,
`canvas`, `discussion`) build one `NodeInit`. `PresenceFrames` (session, sequence, TTL)
and `Whereabouts` (an agent's per-node sessions and carets). `Me`, asked once.
`ApiError` from codes and library error types. The sheet book (`SheetBook`, was the
MCP's `McpBook`) is filled from held documents by `Pimble::sheet_book`. The MCP's tools
and the CLI's `set-node-text` and `append-link` call it. Not in this wave: the vault as
a `Backend` (wave 4), the app (wave 5), the conformance suite against both backends
(needs wave 4), `move_table`'s and an inline discussion's two writes (two documents,
which no single write can name), `Host`.

**Wave 3, planning.** `TreeView`, `plan_move`, `create_parent`, `can_delete`,
`capabilities`, labels. The app's `tree_drop`, `tree_keys`, menus and toolbar call them
through an adapter over `AppStore`; the MCP's `move_node` gets the app's refusals and
`moveNodes`.

Built 2026-11-02 in `pimble_core::plan` rather than `pimble-api` (decision 5 placed it
there): the app uses it now, before it moves onto the API in wave 5, and `pimble-api`
was being built beside it; the API implements `TreeView` over its held documents and
re-exports the module. `TreeView` (store facts, parent, ordered children, mount and its
source, share marker, held access, label), the one bounded walk (`ancestors`, `nearest`,
`is_at_or_under`), `node_access` with "unknown access is `Full`" stated as policy,
`plan_move` (answers `MovePlan { steps: MoveStep::{Move, MoveMany, Transplant}, line,
warning }` or a `Refusal` with its sentence), `create_parent`, `can_delete`,
`capabilities` with the menu rules, and `display_label`. The app reads them through
`tree_view::AppView` over `AppStore`; `tree_drop` only translates rinch's drop into
rows and a `Place`. Decision 5's answer for mounts is in: every creation on a row goes
through `create_parent` (the toolbar's "+" too, on the selected store's row), so "New
Node" on a mount row and "Paste Mount Here" on one now go to the mount's source, and
"Paste Mount Here" checks access.
Deleting from a row's menu is all or none, like Delete in the tree. `pimble-api`
implements `TreeView` over its held documents (`tree_view.rs`: it holds the nodes, the
target, a mount's source and their ancestors, then judges), and `Pimble::create_parent`,
`plan_move`, `move_nodes`, `move_node` and `can_delete` go through it, so the MCP's
`move_node` (several nodes with `nodes`) and `delete_node` refuse what the explorer
refuses, with its sentences, before anything is sent.

**Wave 4, the vault behind the trait.** `web/src/vault.rs` becomes the `Pimble` vault
implementation; the `Router` composes it with RPC; `dispatch_transplant`,
`refuse_write`, `handle_unreachable` move into the router.

Built 2026-11-02. `Backend::capabilities(store) -> StoreCapabilities` (encrypted,
mounts, pictures, sync links, semantic search, index rebuild; a provided method whose
answer is `StoreCapabilities::SERVER`), `ApiError::OwnerOffline`, and
`ApiError::store_error`/`invalid_operation`/`node_not_found`/`of_move`, the words a
server answers a store operation with (code `-32000` until the server gives them codes
of their own), so a backend that answers the same operations itself answers the same
errors. `pimble_api::Router` (`router.rs`) is a `Backend` over several: `serve(store,
backend)`, the write gate (`set_access`, `gate(store, Write::{Structure, Discussion,
Content})`: a reader's writes, a commenter's that are not a discussion's, anything but
an on-screen edit while the owner is away), a store whose owner's computer is off
(`set_away(store, row)`: every write but content refused with `OWNER_OFFLINE_REFUSAL`,
nodes handed out to be read, a store held nowhere answered as an empty folder), the
transplant (both sides gated; an encrypted and a plain store, or two backends, refused
with `MIXED_`/`SPLIT_TRANSPLANT_REFUSAL`), and search (each store to its own backend,
merged by score: a plain store is no longer searched through an encrypted store's
endpoint). The browser's vault is `web/src/vault_backend.rs` `VaultBackend` (the vault
behind an async lock the backend loop shares, a `WireTable` naming each store's
connection; the vault's I/O is the `VaultWire` trait, `PimbleClient` in the page, logs
in memory in the tests); its capabilities are all "no" but `encrypted`, and pictures are
`NotAvailable` rather than faked. The web routes through the router (`web/src/routed.rs`,
the `BackendCommand` adapter): for an encrypted store `GetChildren`, `GetNode`,
`CreateNode`, `CreateAttachment`, `ListAttachments`, `DeleteNode`, `MoveNode`,
`MoveNodes`, `UndeleteNode`, `ListDeleted`, `ResolveLink`, `BroadcastChanges`,
`ApplyUpdate`, `PutBlob`, `GetBlob`, `RebuildIndex`; every `TransplantNode`; and a
`Search` naming an encrypted store. Every store's writes pass the router's gate first,
plain ones included. Still the vault's commands (wave 5, with the app): rename and
appearance (to become `Pimble::edit`), discussions listed, ancestors, reconcile,
subscriptions and presence, `GetStoreSync` (the store's status: access, relay side,
owner away; its `sync_links` capability says no link), `CloseStore`; a plain store's
other commands still go through `process_command`. `VaultBackend` answers
`subscribe`, `who_am_i` and `publish_presence` with `NotAvailable` until then (the vault
pumps its changes as `BackendEvent`s). Behavior changes: `CloseStore` on an encrypted
store closes it in the page (it answered "not available"); `RebuildIndex` on one answers
a sentence instead of "rebuilt, 0" (the app never sends it); errors read as `ApiError`
sentences; a commenter's picture is refused with the comment-only sentence; an edit made
while the hosted server's connection is down is kept and sent on reconnect; a delete of
the root and a move refused in an encrypted store use the server's words. The
conformance suite is `pimble_api::conformance` (feature `conformance`): create at each
place, fetch, the one write, sync, move, `moveNodes` order and dedupe, move refusals,
delete, the deleted list, undelete, the root refused, links, capabilities stated; it
passes against `RpcBackend` over a real server and behind a `Router`
(`crates/pimble-api/tests/conformance.rs`) and against `VaultBackend` alone and behind a
`Router` (`web/src/vault_backend/tests.rs`).

**Wave 5, the app onto the API.** `AppStore`'s server mirror becomes a reactive view of the
API's held documents and change stream; `DocSession` replaces the five session copies;
`BackendCommand` shrinks to what is genuinely UI plumbing; every command that forwards is
deleted. Error routing uses answers, not modal flags.

Built 2026-11-02 (part; what is left is listed at the end). **The backend loop is over
`Pimble`.** `crates/pimble-app/src/commands.rs` is the adapter: `api_command` answers every
command that reads or writes a store with `Pimble` calls (`node`, the new `listing`,
`attachments`, the new `discussions`, `apply`, `edit`, `delete`, `undelete`,
`list_deleted`, the new `reconcile`, `watch`, `search`, `put_blob`/`get_blob`, and
`backend()` for create, attach, move, transplant and links, which the explorer has already
planned), and `host_command` keeps the server operator's RPCs (stores, replicas, sync
links, the cloud account, sharing, agents, mounts, presence subscription) on the host
connection. The desktop (`backend.rs`) owns a `Pimble` over `RpcBackend` whose
`Connector` is `Local`: it joins or starts the embedded server (`pimble_server::local`),
watches the socket, connects again the moment it closes and says `Connected`/`Disconnected`
itself, so the watchdog, `ConnectionLost` and the connection generations are gone; a
listener task turns `Pimble::next_change` into events (`commands::change_events`). The
browser (`web/src/backend.rs`) owns a `Pimble` over `Arc<Router>` (a blanket
`Backend for Arc<B>`), with the same listener, and calls `Pimble::rewatch` on every new
connection; `routed.rs` keeps only the write gate (its `answer` is gone: the API through
the router answers what it answered).
**API additions:** `StoreCapabilities::local_documents`, `Backend::list_discussions`
(default `NotAvailable`; RPC, router and vault implement it), `Pimble::listing`,
`watch`/`rewatch`/`forget` (a watched store's subscription is made again when it ends, and
`next_change` waits instead of answering `None` while nothing is subscribed),
`reconcile`, `apply`, `discussions`, `Edited::delta`.
**The vault holder decision:** the browser's vault is the one holder of an encrypted
store's documents. `VaultBackend` says `local_documents: true`, and `Pimble` then holds
nothing of that store: every read, `edit`, `reconcile` and listing goes through to the
vault (in memory). The alternative, feeding `Held` from the vault's decrypted updates,
keeps two copies that disagree between an update and its delivery (a tombstone the vault
holds and `Held` does not yet was found to leak back into a children list). So
`VaultBackend::subscribe` stays `NotAvailable` on purpose: the vault pumps what it
decrypts as the app's events itself, and its subscription, sealed presence, status and
closing are answered by it before the API (`VaultClient::handle`); what an encrypted store
cannot do is refused by `VaultClient::refuse`. Rename, appearance, discussions listed,
ancestors and reconcile moved off the vault onto `Pimble` (`rename_node`, `set_appearance`,
`reconcile`, `ancestors`, `rename_edit`, `appearance_edit` and the vault's `active` set are
deleted). Planning over an encrypted store stays the app's (`AppView`): `Pimble`'s
`TreeView` holds no documents of a pass-through store.
**Subscriptions:** there is no subscription per node. `SubscribeNodeChanges`,
`UnsubscribeNodeChanges`, `ConnectionLost` and the app's `NODE_SUBSCRIPTIONS` are deleted;
a store's subscription carries every document's update, the listener hands each one that
this window's views did not make to them as `RemoteChanges { update }` (bytes, no base64;
`BroadcastChanges { update }` likewise), once per update even when the server announces it
under several kinds, and `Resynced { store }` after a gap (the tree reads the store again
from the held documents, and every view of it reconciles). The vault hands over every live
update the same way.
**One plumbing for open documents:** `crates/pimble-app/src/doc_session.rs` (`send`,
`ask`, `writable`, `send_back`, `received`, `reconciled`, `reconnected`, `resynced`); the
five views (`editor`, `boards`, `sheets`, `discussions`, `text_boards`) send, ask and send
back through it, and `events.rs` fans out through it once. Each view still keeps its own
copy and its own hand-across to same-kind views (the invariants in CLAUDE.md
"Collaboration shape" are unchanged and their tests pass).
**Answers:** `BackendEvent::Failed { asked: Asked, message }` (`protocol::Asked::of`,
`Asked::answer`) names the request a modal waits on (add or mount a remote store, link,
remove a replica, a new store, a mount, the deleted list, undelete); `events.rs` routes it
by that, and the pending-flag guess is left only for "Put Back" as a `MoveNode`.
**Presence and recipes:** frames from `Pimble::frames()` (`commands::presence_frame`; the
browser's vault seals the frame it is handed); the app's `presence::frame_for` is deleted.
New sheets, canvases and discussions are `pimble_api::recipes` (`protocol::NodeInit::of`).
**Behavior changes:** rename and appearance in an encrypted store go through `applyEdit`'s
vault path (one append of one update instead of the tree's two); a view now also receives a
peer's structural and metadata updates of its node (harmless merges); a vault store's
`ContentUpdated` still answers `NodeContentUpdated`, plus `RemoteChanges` for live updates;
the browser re-reads a plain store's shown lists after its subscription is made again; a
reconcile of a node the vault does not hold says "Node not found" in the server's words;
`Search` always goes through the router (the same answer); an error from a modal's request
no longer depends on that modal being the one waiting.
**Part 2, built 2026-11-02.** **The tree is a view of held documents.** A change names
what it touched; `pimble_api::view` (`Affected::of(kind)`: rows, lists, parents of,
gone) says which rows and lists that is, and `Pimble::shown(store, &affected) -> View`
reads them from what is held after the merge (a list only for a parent held and not a
mount; a child not held is fetched with the list, the one time anything is asked).
`Held::apply` now holds a node from the update that announces it when that update is its
whole document (fields readable, nothing pending), so a node made elsewhere or a store's
root reaching a new replica needs no fetch, and derives access again from the new parent
for a moved or new node and its held subtree (a share's root keeps its own judgement),
answering the nodes whose access changed as `Change::Rejudged`; `Change::Resynced` names
the documents the catch-up changed. The app's listener turns every change into
`BackendEvent::Shown { view }` (`commands::change_events`, `commands::shown`); every
structural command the window makes shows what it did before its answer
(`Pimble::shown_after`: the touched documents reconciled first, since the change that says
it may not have been heard yet); rename, appearance and `ApplyUpdate` show the edited node.
The browser's vault, the one holder of an encrypted store, feeds the same view: what it
pumps (`RemoteStoreChange`, `NodeContentUpdated`) is noted (`web/src/backend.rs` `owe`)
and shown through the API, which reads the vault, once the vault is not locked
(`show_owed`). `events::apply_view` is the one update: rows the window shows (or a pane
waits for), each loaded list and every loaded mount of it (a store's shown roots count as
loaded: they are its row's children), subtrees that left; the tree is rebuilt only when a
list or a row's snapshot (`row_data_changed`: icon, colors, share marker, access,
chevron) changed. **Deleted:** `refetch_parent_children`, `refetch_held_nodes`,
`refetch_below_changed_access`, `refetch_root_if_empty`, the structural arms of
`RemoteStoreChange`, `ChildrenLoaded`'s keep-newer-by-`modified_at`, the list surgery and
refetches in the `NodeMoved`/`NodeDeleted`/`NodeTransplanted`/`MountCreated` answers, the
`GetNode`s after a share update, `BackendEvent::NodeRenamed`, `AppStore::node_held_access`
and `loaded_lists_holding`. What stays asked is what no document says: a store whose
access changed sends one `BackendCommand::Rejudge { node_ids }` (`Pimble::rejudge`, one
`getNodes` for every node the window shows), and first loads (a folder opened, a list
never loaded that a node was created or moved into, a mount's state, a mount's children
when its source becomes live).
**Mounts, presence and people on `Backend`:** `create_mount`, `mount_state`,
`presence(store, client_id) -> (frames now, PresenceStream)` and `people(store)` (defaults
`NotAvailable`; `RpcBackend` and `Router` implement them, the router gating a mount as a
structural write); `CreateMount`, `GetMountState`, `SubscribePresence` and `GetPeople` are
`api_command`s now, and "Mount Remote Store Here..." makes its mount through the API.
**Behavior changes:** another device's (or the CLI's, the MCP's) create, rename, move and
delete show in the explorer with no `getChildren`/`getNode` round trip; a list arrives as
it is held, never replaced by an older copy; a moved folder's subtree takes its new
parent's access without a refetch; a list never loaded is not loaded by someone else's
change (as before) but a store's root list always is (it replaces the `Synced`
refetch); a row's node is shown again on a content change even while a pane holds it
(cheap: it comes from held documents); a role change asks once for all shown nodes instead
of every list and root; a failed "Mount Remote Store Here..." mount reads as that modal's
error, unchanged. Verified in the release desktop app on a store copy (port 7491): folders
expanded; a node made, moved into a never-opened folder (the chevron appeared), given text
and deleted from the CLI, a node renamed and reordered from the MCP, each shown with no
`ChildrenLoaded` in the log; two panes typing into one note; a sheet and a canvas made by
the MCP appeared, and rows and elements the MCP added showed live in their panes.
**Not done, and why.** *One `DocSession` per window* would put the editor's document
somewhere the four rinch editors and the board, sheet and in-text views all read: rinch's
editor owns its document inside its collaboration adapter, so a shared session would be a
document-model layer between the editor and that adapter, which CLAUDE.md forbids. The
views keep their own copies and hand-across (`doc_session.rs` stays the one plumbing).
*Author names through `pimble_api::identity`:* the desktop's API connection is the local
server's operator (no account, `Me` is `None`), and the name is per share (the person's
name in the share the node is in, `shares::author_in`), which needs the cloud account's
people and the node's scope root; `Me` says neither, so the app keeps it.
*The browser's reconnect loop as a `Connector`:* `supervise` does far more than connect
(proving the socket, the vault's rows, subscriptions, catch-up and opening, the store
list), all of it the vault's, and the plain stores already reach their connection through
`Wires`, a `Connector`; moving the loop is a rewrite of the vault's life cycle with no
browser walk-through to check it, left for when the vault becomes a `Backend` that
subscribes (`VaultBackend::subscribe` stays `NotAvailable` by the holder decision above).

**After this contract:** views (source, grouping, order over `TreeView`, the kanban as a
built-in written in plain rinch against `Pimble`) and the WASM plugin host, which hands a
plugin the same `Pimble` trait.

## 3b. The crate, concretely (wave 2's design)

`crates/pimble-api`, a workspace member that builds for `wasm32` (no tokio in the
library; `futures`, `async-trait` with `?Send` on wasm, timers behind a small trait).

```text
pimble-api
  backend.rs   trait Backend      what an implementation must do (transport only)
  rpc.rs       RpcBackend          over PimbleClient: reconnect, resubscribe (native and wasm)
  held.rs      Held                per store: a Tree of the node documents this client holds,
                                   the store's Store row, what is known to be complete
  api.rs       Pimble              the API every client uses (reads, edit, structure, ...)
  names.rs     resolve/display     links, ids, paths; became and StoreNotHere hops
  recipes.rs   create_*            document, sheet, canvas, discussion: one createNode{init}
  presence.rs  PresenceFrames      one frame builder (session, sequence, TTL, body)
  identity.rs  Author              who is writing, per share, cached (no whoAmI per write)
  error.rs     ApiError            data with a sentence (decision 9)
```

- **`Backend`** (async, object-safe through `async-trait`): `list_stores`, `fetch(store,
  ids)` (whole documents: `getNodes`), `children(store, node)` (`getChildren`, mount-aware,
  answers the store the children live in), `apply(store, node, update, client_id)`
  (`applyEdit`), the structural calls (`create_node`, `move_node(s)`, `transplant`,
  `delete`, `undelete`, `create_mount`), `resolve_link`, `list_deleted`, `search`, blobs,
  presence publish and subscribe, discussions' attachments, and `subscribe(store) ->
  Stream<StoreChange>` where a change carries its kind, its source and the update bytes.
  `RpcBackend` owns the reconnect loop and resubscribes; callers see `ApiError::Offline`
  and, on return, a `Change::Resynced { store }` after which held documents are brought up
  to date with `syncNodes` (state vectors in, diffs out).
- **`Held`** is the cache: documents come in whole the first time they are read and are
  kept current by the store subscription's update bytes (merged with
  `NodeDoc::apply_update`); `Tree` answers children, parents, ancestors, labels, access
  through `NodeFields::into_node` and the wave 1 rules. A store's documents are fetched as
  they are asked for (a node, a list, a walk); `hold_store(store)` fetches all of them
  (search-free name lookups, `rows_pointing_here`, a view that needs the tree).
- **`Pimble`**: `stores()`, `node(addr)`, `children(addr)`, `ancestors(addr)`,
  `label(addr)`, `doc(addr)` (a read-only clone), `edit(addr, |doc, now| ...)` (decision 4;
  answers `Edited { changed, caret }`), `create(recipe, parent, place)`, `move_nodes(nodes,
  parent, place)`, `delete`, `undelete`, `transplant`, `resolve(name)` and `display(addr)`,
  `changes()` (the stream, after `Held` has merged each change), plus pass-throughs for
  search, blobs, deleted lists, attachments, presence.
- **What does not move in wave 2:** the app keeps `BackendCommand` (wave 5); the vault is
  not a `Backend` yet (wave 4); planning functions arrive in wave 3. The MCP and the CLI's
  node commands move onto `Pimble` in this wave, and `pimble_client::PimbleClient::edit_node`
  becomes `Pimble::edit`'s RPC path.

## 4. How it is checked

- Every wave: the crates it touched are tested by whoever built it; then the workspace
  compiles with its tests (`cargo check --workspace --tests`) and `web` compiles, and the
  wave is committed. The whole suite (`cargo test --release --workspace`, and `web`'s
  `cargo test`) runs after wave 3 and before the release (Joe, 2026-11-02: a full run
  after every wave costs days). Before the release also: the
  release desktop app driven over the debug port for the paths the wave touched, the MCP
  walk-through (`scripts/mcp/`), the local stack sharing walk-through
  (`scripts/local-stack/README.md`) for waves 1 and 4.
- Waves 0 and 1 change behavior only where this contract says; anything else that changes
  is a regression.
- Wave 2 adds a conformance suite run against both implementations (RPC against a test
  server, vault against the stub accounts service in `pimble-server/tests/common`): the
  same calls, the same answers, the same `ApiError`s.
- Convergence sweeps (`PIMBLE_CONVERGENCE_SEEDS`) before and after anything touching repair.

## 5. Open decisions for Joe

1. **Workspace RPCs:** remove them (recommended; unauthenticated, unused), or make them
   Service-only.
2. **Where shared rules live:** in `pimble-crdt` (recommended where they are pure over
   documents and trees) or a new small crate (`pimble-rules`) for the ones that need
   grants and roles.
3. **The API holds documents** (decision 3) rather than caching `Node` values and
   refetching. Recommended: it is what the vault already does, it removes the refetch
   policy, and it makes views and plugins cheap. Cost: the RPC client holds more memory
   for big open trees, and wave 2 is bigger.
4. **Old clients during the move:** each wave that removes an RPC (0: workspace,
   `updateNodeContent`) needs desktop, web and the hosted server released together, as
   with `#1503`. Recommended: yes, per the release rule; no compatibility shims.
5. **One answer for each disagreement:**
   - Deleting on a locked layer: skip the locked elements and say how many were kept
     (recommended), or refuse the whole delete.
   - "Notes and fields may change on a locked element": keep (recommended), or drop.
   - Paths: a tool or formula path may start at the store name or anywhere, with
     ambiguity answered by candidates (recommended).
   - The MCP says "comment-only" as well as "read-only" (recommended).
   - Every creation path under a mount goes to its source, including discussions and
     moves (recommended).
6. **Order:** waves as listed (recommended), or wave 3 (planning) before wave 2 so the
   kanban can start sooner on the app side.
