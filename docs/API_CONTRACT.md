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

**Wave 2, `pimble-api` with the RPC implementation, and the MCP moves onto it.** The trait,
`ApiError`, held documents, the one write, naming and links, recipes, presence. The MCP is
the first consumer because it has no UI state: its 62 tools become thin, its tree walks
become lookups. The MCP's tests are the check.

**Wave 3, planning.** `TreeView`, `plan_move`, `create_parent`, `can_delete`,
`capabilities`, labels. The app's `tree_drop`, `tree_keys`, menus and toolbar call them
through an adapter over `AppStore`; the MCP's `move_node` gets the app's refusals and
`moveNodes`.

**Wave 4, the vault behind the trait.** `web/src/vault.rs` becomes the `Pimble` vault
implementation; the `Router` composes it with RPC; `dispatch_transplant`,
`refuse_write`, `handle_unreachable` move into the router.

**Wave 5, the app onto the API.** `AppStore`'s server mirror becomes a reactive view of the
API's held documents and change stream; `DocSession` replaces the five session copies;
`BackendCommand` shrinks to what is genuinely UI plumbing; every command that forwards is
deleted. Error routing uses answers, not modal flags.

**After this contract:** views (source, grouping, order over `TreeView`, the kanban as a
built-in written in plain rinch against `Pimble`) and the WASM plugin host, which hands a
plugin the same `Pimble` trait.

## 4. How it is checked

- Every wave: `cargo test --workspace` (under `scripts/guarded.sh`), `web` compiles, the
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
