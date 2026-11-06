# Plugins contract: WASM guests that run plain rinch over the Pimble API

Status: drafted by the PM on 2026-11-02 from Joe's direction (memory
`project_views_and_plugins_2026_11_02`) and a read-only study of rinch at the pinned
revision (`2d1f11a0`, cargo checkout) and of `crates/pimble-plugins`. Waves 0 to 3 are built (section 4).
Open decisions at the end. Metanodes (`docs/METANODES_CONTRACT.md`) are a separate, generic
concept whose viewers plugins provide; the kanban (`docs/KANBAN_CONTRACT.md`) is the
first plugin.

## 1. What Joe decided (2026-11-02)

- A plugin is a WASM module that renders with **plain rinch**, the same components and
  theme as the app, on the desktop and in the browser alike.
- A plugin uses **the same internal API as the app** (`pimble-api`'s `Pimble`), after that
  API was made clean (waves 0-5 of `docs/API_CONTRACT.md`).
- **Plugins may write.** The rule is the existing one: one way to write (through the API),
  authorized with the role of the person running it, attributed to the plugin like an
  agent's edits. A plugin keeps no copy of anyone's data.
- **No Pimble concepts in rinch**: anything rinch needs is a general mechanism, upstream.

## 2. Where it stands

- **rinch:** components render into any `dyn DomDocument` (`rinch-core/src/dom/traits.rs`,
  through `RenderScope::new(doc, parent)`); there are desktop, web and mock documents, but
  **no remote or guest document** and no stream of structural DOM operations. Event
  handlers are integer ids in a closed list of 25 `data-*` attributes, dispatched through
  thread-local registries; event details are plain data in thread-locals. The reactive
  runtime is single-threaded with pluggable timer, task and poll hooks (`web_runtime`
  installs them for the browser), so it can run in a guest with host imports in their
  place. The `rinch` facade pulls wasm-bindgen on any `wasm32` target; on `wasm32-wasip1`
  those are panicking stubs, so a guest builds but must never reach a browser-only API.
- **Editor:** `EditorHandle::register_node_view` + the `embed` atom already host live
  components inside text (sheets, discussions use it), so a plugin can be embedded in a
  note. A plugin cannot hold the host's `EditorHandle`, so a plugin that wants text editing
  asks the host to mount the real collaborating Editor into a slot of its DOM.
- **Pimble:** `pimble-plugins` is a skeleton: a sync `NodePlugin` trait, built-ins whose
  `render` JSON nobody reads, `load_wasm` a TODO; only `index_units` is used (by the
  server's search index). wasmtime 27 is a dependency nobody calls, and it costs every
  hosted build about 12 minutes (`docs/NEXT_SESSION.md` 0a).
- **pimble-api:** `Backend` is async, transport-only, `?Send` on wasm, no tokio; a guest
  can run `Pimble` over a backend whose methods are host calls.

## 3. The design (proposed)

### 3.1 The guest

- Target **`wasm32-wasip1`**, a **core module** (not the component model: browsers have no
  native one, and one ABI for both hosts matters more), with a small byte codec (postcard)
  for every message.
- A **guest SDK crate**, `pimble-plugin-sdk`, that a plugin depends on with `rinch`
  (components and theme only) and nothing browser-specific. It provides:
  - **`RemoteDocument: DomDocument`**: guest-side node ids, a shadow tree (tags,
    attributes, children, text) so structural reads are answered locally, and an ordered
    **op log** of mutations, flushed to the host at the end of every guest turn and before
    any read only the host can answer (layout, bounds, caret, focus, scroll, live values).
    `parse_html`/`set_inner_html` are refused.
  - **The runtime hooks**: timers, tasks and the poll waker as host imports;
    `fire_timeout`, `poll_task`, `drain_polls` as exports (the `web_runtime` pattern).
  - **`GuestBackend: pimble_api::Backend`**: each call is a host call that returns a call
    id; the host later calls the guest's `resolve(call_id, bytes)`, which wakes the
    future. So a plugin uses `Pimble` exactly as the app does.
  - **Exports**: `alloc`/`free`, `describe` (the manifest), `mount(mount_point, root,
    props)`, `dispatch(event) -> consumed`, `props_changed`, `unmount`, `resolve`.

### 3.2 The host

- One `pimble-plugin-host` module in `pimble-app` (both targets), plus a thin engine per
  platform:
  - **desktop:** wasmtime core module, `wasmtime-wasi` preview1, one `Store` per instance
    on the UI thread, **epoch interruption** so a stuck plugin is stopped with a notice;
  - **browser:** `js_sys::WebAssembly` with a tiny WASI shim (console, clock, random,
    exit); no interruption is possible, so a turn is timed and a plugin whose turn ran
    past its budget is stopped when it returns (decision 5, taken; wave 3).
- **Applying the op log**: the host keeps, per instance, a map from guest node ids to host
  node ids under the instance's root and applies ops through the host's own
  `dyn DomDocument`, so the same code works on the desktop and the web documents. **The
  map is the sandbox**: a guest can name only its own nodes.
- **Events**: when the op log sets one of the 25 handler attributes, the host registers a
  proxy handler of its own that captures the event context (node ids translated) and calls
  the guest's `dispatch`; key and wheel handlers get the guest's synchronous "consumed"
  answer. Handler ids are translated, never shared.
- **Global machinery the guest cannot see** (Escape/outside-press dismiss stack, app keys,
  paste, clipboard, drags, focus, portals that append to `body()`): forwarded as explicit
  host calls; a guest's `body()` is its instance's overlay root, so menus and modals stay
  inside the plugin's area of the window.
- **Host slots**: the guest renders a placeholder (`data-pimble-slot="editor:<link>"`,
  later `node-picker`, ...) and the host mounts the real component there: the collaborating
  `Editor` for a node, with every invariant of CLAUDE.md "Collaboration shape". A plugin
  never runs its own Editor over a node's text.

### 3.3 Mount points

1. **A metanode's viewer** (`docs/METANODES_CONTRACT.md`): a pane opening a node with
   `metanode:viewer = <id>` mounts the plugin that provides that viewer, with the metanode's
   address and the viewer's settings as props (wave 2: it reads the forest itself with
   `Pimble::metanode_source`); the plugin acts through `Pimble`, and core judges its
   edits. A viewer declares the source shapes it can show.
2. **A node type** (later): a pane opening a node whose type a plugin provides.
3. **Embedded in text** (later): a node view matched on `data-plugin=<id>`; the attribute
   verbs (`set_attrs`, `delete`, `select`) become host calls.

### 3.4 The API a plugin gets

- `Pimble` (the data API), never `Host` (stores, replicas, accounts, sharing, agents).
- **Authorized as the person running it**: the guest's calls go through the person's own
  backend, so the server judges them with the person's role; nothing extra is granted.
- **Narrowed by what the person approved** (decision 2, taken): which stores, read or
  write; no network (decision 6).
- **Attributed**: edits carry `client_id = "plugin:<id>"`. Presence does not name the
  plugin: the person is present through the pane exactly as in a native view (wave 2b;
  docs/PRESENCE_CONTRACT.md "Plugins").
- Changes: the guest's `Pimble` gets the same change stream (`next_change`) through the
  host, and the same `DocSession` plumbing for documents it shows.

### 3.5 Packaging

- A plugin is a `.wasm` file with a manifest it returns from `describe`: id, name, version,
  ABI version, rinch revision it was built against, what it provides (viewers and the source shapes they take, node
  types, embeds, recipes such as "New Kanban"), and what it asks for.
- **ABI and rinch versions are checked at load**: a plugin built against another ABI or an
  incompatible rinch is refused with a sentence, and the view falls back to an outline.
- Where plugins live and how they reach other devices: decision 3 (a per-device
  folder on the desktop; in the browser, which has no folder, the page's own IndexedDB,
  filled by "Add plugin...": wave 3).

### 3.6 What happens to `pimble-plugins` today

- `NodePlugin`, `RenderOutput` and the placeholder built-ins are retired; `index_units`
  (search units per node type) stays as a small server-side registry, since the server
  runs no UI plugins (open decision 4 for plugin node types' search).
- wasmtime moves behind a feature that only the desktop app turns on, so the hosted server
  and the CLI stop compiling it (`docs/NEXT_SESSION.md` 0a; about 12 minutes per deploy).

### 3.7 rinch changes (general, upstream)

- Gate the facade's browser-only parts on a `web` feature or `target_os = "unknown"`
  rather than any `wasm32`, so a `wasm32-wasip1` guest gets no panicking stubs.
- Whatever the kanban needs for dragging across several lists (a drag source and drop
  targets with a judge callback), if rinch lacks it.
- Nothing Pimble-specific; anything that seems to need it is asked of Joe first.

## 4. Waves

0. **wasmtime behind a feature** (hosted builds stop paying for it) and `NodePlugin`
   retired to the `index_units` registry.

   **Built 2026-11-02 (branch `api`).** `pimble-plugins` is now `index_units_for(node_type,
   content)` and `INDEXED_NODE_TYPES` (document, folder, canvas, sheet, discussion; the
   units each gave before, unchanged), which the server's search index calls directly.
   `NodePlugin`, `PluginHost`, `create_default_host`, `PluginInfo`, `NodeSchema`/
   `SchemaField`/`FieldType`, `RenderOutput`, `PluginAction`, `ValidationResult`,
   `PluginError` and the five placeholder built-ins are gone, with the crate's unused
   serde, thiserror, anyhow and tracing dependencies; `pimble-search` no longer declares a
   dependency on it. wasmtime is an optional dependency behind the `wasm-host` feature,
   off by default and turned on by nothing until wave 1 (the desktop app):
   `cargo tree -i wasmtime` finds it for none of `pimble-cli`, `pimble-server`,
   `pimble-app`, `pimble-mcp` or the workspace as a whole.
1. **The guest SDK and the desktop host with a toy plugin**: `RemoteDocument`, the op log,
   events, runtime hooks, epoch interruption, ABI/version checks; a "hello" viewer
   that renders rinch components and handles clicks and keys. The rinch gating PR.

   **Built 2026-11-02 (branch `api`).** rinch: joeleaver/rinch#2088 (branch
   `wasi-guest-gating`) gates the facade's browser halves on `target_os = "unknown"`, so a
   `wasm32-wasip1` build with components and theme links no wasm-bindgen; only the SDK
   points at it, the app stays on `main`. **`pimble-plugin-abi`** (main workspace, no
   rinch): postcard messages (`Manifest`, `MountArgs`/`Props` with the metanode's address
   and its forest, `Op`, `Event`/`EventData`, `Query`/`Answer`), `ABI_VERSION` 1, export and
   import names all `pimble_`-prefixed (wasi-libc's `free` and libm's `log` would collide),
   `HANDLER_ATTRIBUTES` (rinch's closed list of 25, with the handler kind each holds).
   **`pimble-plugin-sdk`** (`crates/pimble-plugin-sdk`, a member of the new `plugins/`
   workspace with its own lock, because it is on the rinch branch): `RemoteDocument`
   (shadow tree answering structural reads and a small selector matcher; the op log flushed
   at the end of every turn and before live value, layout, scroll, focus and selection
   reads; `parse_html`/`set_inner_html` refused with a log line; `insert_child` sent as a
   place by sibling, not an index), `Guest` (mount/dispatch/props/unmount, target
   independent), the runtime hooks installed on first use as imports, a reactor
   `_initialize` (without it the linker wraps every export in constructors and destructors),
   and `plugin!(describe: .., view: ..)`. A plugin author writes a `describe()` with
   `manifest(..).with(viewer(id, name, &[SourceShape::Any]))` and a view
   `fn view(__scope: &mut RenderScope, mount: Mount) -> NodeHandle` (props are a
   `Signal<Props>`), and glob-imports the prelude (`rsx!` reaches rinch through it). The
   build script records the rinch revision from the plugins lock. **`pimble-plugin-host`**
   (main workspace; `wasm-host` turns on wasmtime 47 and wasmtime-wasi, which
   `pimble-plugins` no longer carries): `Applier` (ops through rinch `NodeHandle`s over the
   app's `dyn DomDocument`; the guest-to-host map is the sandbox: an unknown id, an id made
   twice, `<style>`/`<script>`/`<link>`/... or touching its own area are refused and stop
   the plugin; handler attributes become unowned host proxies calling the guest's
   `dispatch`, keys and wheels answering synchronously, file drops not wired; the
   desktop's text-field shim, which `rsx!` omits on any wasm32), `Instance` (engine
   agnostic: imports reach it through a key, since wasmtime store data must be `Send`;
   timers, tasks and poll drains as host `set_timeout`s; events arriving mid-call queued;
   a failure stops it, clears what it built and calls `on_stop` with a sentence), and the
   wasmtime engine (preview1 with inherited stderr only, one `Store` per instance, 256 MiB,
   epoch interruption at 10 ms ticks with a 2 s turn budget, `Registry::load` of
   `<data dir>/pimble/plugins/*.wasm` with the ABI check). **The rinch version check is
   not what section 3.5 says:** rinch is 0.1.0 at every revision and revisions have no
   order outside its repository, so the host checks the ABI exactly and only logs a
   plugin built against another rinch revision; an exact match would refuse every plugin
   at each pin move (open question for Joe). **App** (`crates/pimble-app/src/plugins.rs`,
   desktop only): the folder loads off the UI thread at start; a pane opening a metanode
   whose viewer a loaded plugin provides mounts it in a slot in `render_metanode` (props
   updated in place as the forest changes); a refused plugin's viewer, or a plugin that
   stopped, shows the outline under its sentence, and the stopped one is not restarted
   while the pane shows that metanode. **Plugins:** `plugins/hello-viewer` (`hello`: the
   metanode's title, the forest indented, a counting `Button`, a `TextInput` echoing what
   is typed and the last key), `plugins/stuck-viewer` (`stuck`: a button that never
   returns). **Tests:** abi 3; sdk 3 (`remote_vs_mock`: the same component straight into
   `MockDomDocument` and through a `RemoteDocument` applied to another mock make the same
   tree, and stay the same after clicks, new props and typing; a host-only read flushes
   first; selectors); host 9 (ABI refusal sentence; applier: tree, translated handlers both
   ways, discard releasing proxies, the sandbox refusals, a refused op stopping the plugin
   and clearing it, unmount; wasm with hand-written WAT guests: a manifest for ABI 2
   refused at load and in a folder, a looping guest interrupted at a 200 ms budget with
   the notice, a guest naming node 0 stopped; and the built hello plugin over a mock
   document taking a click, typing, a key and new props, skipped with a line when it is
   not built). **Checked in the release desktop app** (port 7491, scratch dirs, a copy of
   the family store, both `.wasm` files in the scratch plugins folder): MEDICAL set to
   `hello` two levels deep through the MCP's `set_metanode` opened as the plugin's view
   with the 62-node forest; three clicks read "Clicked 3 times"; typing "hi plug" echoed
   with "Last key: g"; setting `stuck` from the MCP switched the open pane in place;
   "Hang" was stopped after 2 s with "The Stuck viewer plugin was stopped: it ran too long
   without answering." over the outline. Not checked: the browser (no host there yet; it
   compiles), a refused plugin in the app (tested in the host), menus or modals portaled
   to the plugin's body. Known limits: a plugin's markup is styled by the app's theme;
   fixed-position content is not clipped to the pane; queued (non-key) events wait for the
   running call; no `GuestBackend` yet, so a plugin cannot read or write anything.
2. **The API bridge**: `GuestBackend`, call/resolve, the change stream, attribution,
   install-time permissions.

   **Built 2026-11-02 (branch `api`), on decision 2 as recommended.** **The bridge's
   shape:** a plugin's `pimble_api::Pimble` runs over the SDK's `GuestBackend`, whose
   every `Backend` method is one `ApiCall` sent through the import
   `pimble_api_call(call_id, json)` and answered later through the export
   `pimble_resolve(json)` (`Resolution { call, reply }`), which wakes the awaiting
   future (`Bridge`, a call table; the host never answers inside the guest's own
   call). A subscription is one call answered `Subscribed`, then `Change` per change,
   then `Ended`; dropping the stream sends `pimble_api_cancel`. These messages
   (`pimble_plugin_abi::api`) are JSON, not postcard: they carry the server's own
   types (tagged enums, skipped fields, JSON values in `custom`), which postcard cannot
   read back; bytes travel as base64. `ABI_VERSION` is 2. `pimble-api` has an `rpc`
   feature (default) holding `RpcBackend`, so the guest builds it for `wasm32-wasip1`
   without pimble-client; a built plugin imports only `pimble` and
   `wasi_snapshot_preview1` (hello is 3.2 MB). **The host side**
   (`pimble_plugin_host::bridge`, engine-agnostic): `Approval { stores: All |
   Only(ids) | Nothing, write }`; `Approval::check` judges each call by the stores it
   names and whether it writes (`ApiCall::stores`), on the UI thread before it leaves
   (`Instance`'s `ApiSide`) and again where it is made (`execute`), which also narrows
   answers: `listStores` to approved stores, children read through a mount into an
   unapproved store refused, a search of every store to the approved ones; `apply`
   and presence carry `client_id = "plugin:<id>"` whatever the guest said; nothing
   else is granted, since every call goes through the person's own backend and the
   server judges it with their role. A refusal is a sentence: "The Hello viewer plugin
   was not allowed to use that store.", "The Hello viewer plugin was allowed to read
   this store, not to change it." A manifest asking for the network is refused at
   load (decision 6). **Desktop:** `BackendCommand::PluginCall { instance, plugin_id,
   plugin_name, approval, call_id, call }` runs beside the command loop on the backend
   thread (a task per call, so a subscription or a slow call holds nothing up) over
   the window's own `Pimble::backend()`, answered as `BackendEvent::PluginReply`;
   `PluginCancel` and `PluginForget` (sent when an instance stops) abort what is in
   flight. The UI thread has no second API instance. **Changes:** the plugin's
   `Pimble` subscribes like any client, through the bridge, to stores its approval
   covers only, so its held documents stay live and `next_change` works as in the
   app. **Props** are the metanode's address and the viewer's settings (its `data`
   under the viewer's id; a viewer writes its settings with `api.edit` and
   `NodeDoc::edit_data`, which sets, inserts or removes one place inside `data` by a path
   of keys and indexes and leaves the rest alone, so two people changing different
   settings both keep theirs: docs/TIMELINE_CONTRACT.md section 8); the forest is no
   longer sent: the plugin reads it with
   `Pimble::metanode_source` and keeps it live itself. **What an author writes:** in
   `describe`, `.writes(StoreAsk::Chosen)` (or `.reads(..)`, `StoreAsk::All`); in the
   view, `mount.api` (an `Arc<Pimble>`) and `mount.metanode()`, with calls run in
   `spawn_local(async move { ... })`, e.g. `api.metanode_source(addr).await`,
   `api.edit(addr, |doc, now| doc.set_title(..))`, `api.move_node(node, parent,
   Place::First)`, `while let Some(c) = api.next_change().await { if
   forest.touched_by(&c) { .. } }`. **Approval flow:** a plugin whose manifest asks for
   anything and whose approval does not cover the metanode's store mounts nothing;
   the pane shows the outline under "The Hello viewer plugin wants to read and change
   the store Family Management." with "Allow" and "Not now". Allow grows the approval
   (`Approval::grown`: the store for `Chosen`, everything for `All`, write when asked),
   writes `<data dir>/pimble/plugins/approvals.json` and mounts it; Not now shows the
   outline under "Shown as an outline: the Hello viewer plugin was not allowed here."
   until the pane shows something else. Account > "Plugins..." lists each plugin,
   what it asks for and what it was allowed here, with "Revoke", which stops it in
   every pane (they ask again). A plugin asking for nothing mounts at once.
   **Toy:** `hello` asks `.writes(StoreAsk::Chosen)`, reads its forest through the
   API and keeps it live, and with a chosen row renames it (`Pimble::edit`), moves it
   to the top (`Pimble::move_node`) or tries to move it into itself, which `plan`
   refuses in the plugin with the explorer's "A node can't go inside itself."; the
   line under the buttons says what came of each. **Tests:** abi 5 (calls and replies
   round trip with bytes, nodes, changes and every error); sdk 5 (a call waits for
   its resolution and reads errors as the API's; a subscription takes changes until it
   ends and cancels when dropped; `remote_vs_mock` unchanged in substance); host 4
   unit (a call outside the approval refused with a sentence; a read-only approval
   refuses writes; allowing grows the approval; the network refusal) and 2 end to end
   in `tests/bridge.rs` (the built hello plugin over a mock document against a real
   in-process server: the forest read, a key, typing, a rename whose change the
   server attributes to `plugin:hello-viewer`, a move whose order the server and the
   plugin agree on, `plan`'s refusal with nothing sent, a read-only approval refusing
   a rename; and a plugin with no approval shown the refusal and reading nothing),
   plus the 7 earlier ones; `pimble-api` 17+13+2 and `pimble-app` 209 unchanged.
   **Checked in the release desktop app** (port 7491, scratch dirs, a copy of the
   family store, both plugins in the scratch plugins folder): MEDICAL set to `hello`
   one level deep through the MCP opened as the outline under the request with Allow
   and Not now; Not now gave the outline with its line; reopened, Allow mounted the
   plugin showing the five children read through the API and wrote
   `approvals.json`; a row chosen, "Boo Kitty (vet)" typed and Rename renamed it in
   the explorer too; Move to top moved it in both; Move into itself showed "A node
   can't go inside itself."; a rename made through the MCP showed in the plugin
   live; Account > Plugins... listed both plugins with what each asks and is allowed,
   and Revoke emptied `approvals.json` and put the pane back to the request; Allow,
   then a restart, mounted the plugin without asking; `stuck` (asks for nothing)
   mounted without a request. **Not done:** presence does not show a plugin like an
   agent (a presence frame's `who` is stamped by the server from the connection's
   principal, which for a plugin is the person's own; showing the plugin needs the
   server to read `plugin:` client ids, or the app to publish frames under a plugin
   session, so it is left for Joe); `Backend::capabilities` is the default
   (`StoreCapabilities::SERVER`), since a capability is a synchronous answer the bridge
   cannot give yet, which is right for the desktop's stores and wrong for the
   browser's vault stores (wave 3); the presence stream (`Backend::presence`) is not
   bridged; `Chosen` adds the store of each metanode the plugin is shown for, with no
   store picker; the browser host is wave 3.
2b. **Presence like native, host state and requests, recipes (ABI 3)**.

   **Built 2026-11-02 (branch `api`).** Joe: plugin presence looks and feels just like
   native presence (docs/PRESENCE_CONTRACT.md "Plugins" has the design and what was
   checked). **rinch:** joeleaver/rinch#2088 merged to `main` on 2026-11-02; the SDK and
   the plugins point at `main` again (no branch reference anywhere) and the plugins lock
   pins its own revision (`0740192d` now; the app's stays `2d1f11a0` until
   `cargo update rinch` there). **ABI 3** (`ABI_VERSION`): an export
   `pimble_host_state(ptr, len)` taking a postcard `HostState` (people per node in the
   stores the plugin may read, resolved as the app resolves them; the key of the person its
   pane follows; dark or light), sent after mount and whenever any of it changes; an import
   `pimble_host(ptr, len)` taking a postcard `HostRequest` (`Open { node }`,
   `Focus { node }`, `Follow { person }`), handed to the app a turn later (never inside the
   guest's call) and dropped for a node of a store the plugin may not read;
   `ApiCall::PublishPresence` removed (the person is present through the pane, never as a
   plugin session); `Provision::Recipe(RecipeDecl { id, label, title, icon, viewer, depth,
   children })` and `Manifest::recipes()`. **`pimble-ui`** (new crate, both workspaces):
   the presence chips and dots, icons and colors as rinch components with no app state,
   drawn by the app and by plugins alike. **SDK:** `Mount::host` (`Host`: `open`,
   `focus`, `follow`, `people_in`, `following`, `dark`, `seen()` a `Copy` handle),
   `HostLink::request`, `recipe(..)`, `display_label(node)` (the explorer's label),
   `person(..)`, re-exports `pimble_ui`. **Host:** `Instance::on_request`,
   `Instance::set_host_state`, `Call::HostState`, the engine links the import and needs
   the export. **App:** `plugins.rs` pushes host state from one effect over presence, the
   theme and every pane's following; answers `Open` with the pane's link-open, `Focus`
   with `presence::say_within`, `Follow` with `follow::start`; lists every loaded
   plugin's recipes in `AppStore::recipes`, offered in the explorer's row and store menus,
   made by `BackendCommand::MakeFromRecipe` through the window's `Pimble`
   (`pimble_api::recipes::metanode`: the metanode keys go in with the create, through
   `NodeDoc::metanode_seed`; then the children, last each). A recipe runs no plugin code,
   so it needs nothing allowed; the node is the person's own edit. **The handler list:**
   rinch #2091 (sortable lists, being built) was asked for plain `data-*` handlers; any
   attribute it adds goes in `HANDLER_ATTRIBUTES`, and a synchronous drop judge needs a
   kind of its own answered as keys are (the note sits above `handler_kind`).
   **Tests:** abi 5 (host state, requests and recipes round trip), sdk 3+3
   (`a_plugin_draws_presence_with_the_apps_chips_and_asks_the_host_to_follow`), ui 5,
   host 4 unit + applier 4 + bridge 2 + kanban 2 + wasm 3 (the harness is
   `tests/common/mod.rs` now, shared by the bridge and kanban tests), api 18 (a recipe's
   metanode is one from its first edit), app 206 (a pane and what its plugin view is on
   are one person; the presence and appearance tests moved to `pimble-ui`).
   **Install:** `scripts/plugins/install.sh [plugin ...]` builds the plugins workspace
   for wasm32-wasip1 (guarded) and copies each `.wasm` into `<data dir>/pimble/plugins/`
   (`PIMBLE_DATA_DIR` names a scratch one); the app loads the folder at start.
3. **The browser host**: the same plugin running in the web app.

   **Built 2026-11-03 (branch `api`), decision 5 as recommended.** **The engine**
   (`pimble_plugin_host::js_engine`, feature `js-host`, which pimble-app's `web` turns
   on): the same `wasm32-wasip1` core modules, compiled and instantiated by the page's
   own `WebAssembly` (asynchronously: a browser may refuse either synchronously for a
   module this size on the page's thread). The imports are an object of wasm-bindgen
   closures that mirror the wasmtime linker's one for one, reading and writing guest
   memory through a fresh `Uint8Array` view of its buffer each time (a view goes stale
   when the memory grows) and reaching the instance's `HostCalls` through its key, as
   the desktop's do; `i64`s cross as BigInts. WASI is a shim of what a plugin imports:
   `fd_write` into the log, `clock_time_get` from the wall clock, `random_get` from
   `crypto.getRandomValues` (a plugin's new ids come from it), empty environment and
   arguments, `sched_yield`, `proc_exit` (stops it with "it exited with status N"), and
   `ENOSYS` for any other preview1 function a module imports. An import that refuses
   what the guest sent records why and throws; the throw unwinds the guest to the
   export call, which reads the reason back, so the sentences are the desktop's.
   **Stopping (decision 5):** nothing can interrupt running code in a page, so each turn
   is timed against the desktop's 2 s budget and a plugin whose turn ran past it is
   stopped when the turn returns, with the desktop's notice ("... was stopped: it ran
   too long without answering."); one that never returns holds the page until the
   browser offers to stop the script. Memory is not capped in the browser (the desktop
   caps it at 256 MiB). **Ready ahead:** a pane mounts a plugin synchronously, so each
   plugin keeps one instance ready (the one that described it, then one made behind
   each that is taken); a pane that finds none shows the outline under "The Kanban
   plugin is starting." and looks again once it is ready, and one that could not be
   made says why. **Shared code:** the applier, bridge, instance and approvals are
   unchanged; the registry moved out of the wasmtime engine (`registry.rs`, generic
   over an engine's loaded plugin; `wasmtime_engine::load_dir` is the folder loader).
   `crates/pimble-app/src/plugins.rs` is one module for both targets, with only a
   `place` per target (engine, where plugins and approvals are kept, the window's
   add and remove); `plugin_calls.rs` runs plugins' calls on either backend (tokio on
   the desktop, `spawn_local` in the page). **Where plugins live in the browser:** there
   is no folder, so the Plugins window (File > "Plugins...") has "Add plugin...", which
   takes a `.wasm` file the person picks, compiles and describes it in the page, and
   keeps one this app runs in the page's own IndexedDB (`pimble-plugins`, its bytes
   under the plugin's id, so a new version replaces the old), per browser and never
   uploaded (nothing is hosted unless asked); one it does not run is refused with its
   sentence and not kept. Each row has "Remove" (the plugin and what it was allowed
   leave this browser). Approvals are `localStorage` `pimble.plugins.approvals`, the
   same JSON as the desktop's `approvals.json`. **The API in the browser:** a plugin's
   calls go through a router of their own over the page's connections and vault
   (`Routes::plugin_router`, taught each store as the app's router is), so plain stores
   go to their server and encrypted ones to the vault, and the router's write gate
   refuses a reader's write the same way. **Capabilities:** `ApiReply::Stores` carries
   each listed store's capabilities as the host's backend says them (optional JSON, so
   no ABI bump), and the SDK's `GuestBackend::capabilities` answers from the last
   listing (a server's before one), so an encrypted store reads as one; never
   `local_documents`, which is about holding documents in one's own process: the guest
   is another, so its `Pimble` keeps its own copies, which `plan` and a sortable list's
   synchronous drop judge read. **An encrypted store's change stream:** the app's own
   `Pimble` holds nothing of an encrypted store and so never subscribed; a plugin's does,
   so `VaultBackend::subscribe` is now `VaultClient::watch`: every update the vault
   merges (a peer's, a catch-up) or writes (the app's commands, a plugin's, repair) is
   heard with the document it is about (`TreeStructure`, `ContentUpdated` or
   `MetadataUpdated` naming that document, carrying its update), handed over when an
   API call lets go of the vault and on every pass of the backend loop. What a plugin
   writes is attributed to it (`VaultBackend::for_plugins`; `apply` carries
   `plugin:<id>`) and shown to the app's views as a peer's change is (the kinds through
   `owe`/`show_owed`, a content update as `RemoteChanges`), since no command of the
   app's own told them. "New Kanban" (`MakeFromRecipe`) is routed in the browser too.
   **Tests:** host `tests/js.rs` 4 under wasm-bindgen-test in node (a manifest for
   another ABI and junk refused at load; a guest's ops reach a mock document and the
   next instance is made ready; a guest whose turn ran past a 1 ms budget is stopped
   with the notice when it returns; a guest naming node 0 is stopped from inside the
   import); run with `CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner
   cargo test -p pimble-plugin-host --features js-host --target wasm32-unknown-unknown
   --test js` and a runner of the lock's wasm-bindgen (0.2.128); web 69 (new:
   `a_subscription_hears_every_write_and_the_app_hears_the_plugins`); host with
   `wasm-host` unchanged (applier 6, bridge 2, kanban 5, wasm 3, unit 4); pimble-app
   206.
   **Checked in a real browser** (the built-in browser pane) on the local stack (a
   scratch `PIMBLE_STACK_DIR`, alice with the stack's made-up password): File >
   "Plugins..." said no plugins were installed; "Add plugin..." took the built
   `kanban_viewer.wasm` (the file handed to the page's own file input, as the dialog
   would) and listed it with what it asks for; a plain hosted store's row menu offered
   "New Kanban", which made Kanban with To do, Doing, Done; opening it showed the
   outline under "The Kanban plugin wants to read and change the store Plain Board."
   with Allow and Not now; Allow mounted the board; "+ Add card" named in place;
   Alt+Shift+Right moved a card to Doing; a pointer drag moved another into Done; the
   explorer followed each. In an encrypted store (File > "New Store..."): "New
   Kanban", the request again (Chosen is per store), Allow, a card added and named, a
   key move and a drag, each shown in the explorer (the vault's writes made for the
   plugin reaching the app's views); a rename made in the explorer showed live in the
   board in the other split pane (the vault's change stream). After a reload the plugin
   and both approvals were still there (IndexedDB and `localStorage`), and a second tab
   of the same account mounted the board at once. Presence between the two tabs: the
   card one tab held natively showed the other's chip on it in the board, and the
   kanban and the card the board chose showed the other tab's dots and chips. "Add
   plugin..." took the stuck plugin ("Added the Stuck viewer plugin 0.1.0."), and
   "Remove" took it out ("Removed the Stuck viewer plugin from this browser.").
   **Not checked:** a stuck plugin in a real page (no menu sets a viewer, and the MCP
   reaches no browser; the engine's path is the node test's); the OS file dialog
   itself (the file was handed to the input the button made); presence between the
   browser and a desktop app (the desktop's "Add Hosted Store..." lists encrypted
   stores and the browser's plain hosted store is not reachable from a desktop without
   a replica set up by hand; the frames are the app's own, so the existing paths carry
   them); the desktop app run by hand after the refactor (its plugin tests and
   pimble-app's 206 pass). **Known limits:** a never-returning plugin holds the page;
   no memory cap; "Add plugin..." is browser-only (the desktop's folder stays by hand,
   decision 3); one ready instance per plugin is kept in memory.
4. **Host slots** (the Editor first), overlay roots for portals, the forwarded global
   machinery.
5. **The kanban plugin** (`docs/KANBAN_CONTRACT.md`). Wave 1 built 2026-11-02 (its
   contract's "Built" note), before waves 3 and 4: it needed no host slot.
   **ABI 4** (2026-11-02, for its drags on rinch #2093, merged 2026-11-03): `Query::BoundingRect`
   answers `DomDocument::bounding_rect` (the painted box in the click context's frame;
   the SDK's `bounding_rect` had answered the parent-relative `Layout`); `Key::target`, the
   guest node a key was sent to (the focused element), passed as rinch's
   `KeyEventData::target`; an export `pimble_drag_ended` (`Instance::drag_ended`), which
   the guest turns into rinch's `notify_element_drag_ended` (the app calls it from one
   `on_element_drag_end` listener); the host hides the window's drag ghost for a proxied
   drag start marked `data-drag-ghost="false"`; and `dispatch` answers `dispatched` flags
   (`CONSUMED`). Sortable lists added no handler attribute. Every workspace is on rinch main at
   `8421ff53` (the `[patch]` onto #2093's branch is gone).
   **ABI 5** (2026-11-04, for the kanban's folded lanes and columns): a viewer's look on
   this device. `Props::remembered` hands a viewer, at mount only, what this device kept
   for it on that metanode; `HostRequest::Remember { value }` (`Host::remember`, at most
   `MAX_REMEMBERED`, 16 KiB) keeps it. The app stores it in `views.rs` beside the
   explorer's expanded rows and the text places (`views.json` on the desktop,
   `localStorage` in the browser), keyed by plugin, viewer and metanode, under Joe's
   2026-10-23 rule that how things look is each device's own: never in a store, never
   synced, never in the personal document. The props kept for comparing are the
   settings alone, so remembering never sends new props.
   **Mounts into stores not allowed** (2026-11-04, Joe's mounted kanban showed as a
   column): while a plugin runs, the app lists the stores its metanode's mounts reach
   that the plugin was not allowed (`mounted_stores`, `Shown::wants`) and asks for them
   above the view with the same sentence and buttons as the first approval
   (`Outcome::ShownAsking`): "The Kanban plugin wants to read and change the store
   Apex." Allow grows the approval by those stores and starts the plugin again, so it
   reads what they let it (the mount becomes a lane); Not now hides the question in
   that pane and the plugin runs without them. Approvals stay per store.
   **ABI 6** (2026-11-05, for opening kanban cards beside the board):
   `HostRequest::OpenBeside { node }` (`Host::open_beside`) opens a node beside the
   plugin's pane, which keeps the focus: the app keeps one pane beside each pane for it
   (`PaneState::beside`, `pane_view::open_beside`, remembered in `state.json`'s `panes`),
   splits the plugin's pane to the right when there is none, uses another pane shown when
   four are open, and the plugin's own pane when it is the only one. A guest that wants a
   double-click times its clicks itself: WASI's clock answers on both hosts.

## 5. How it is checked

The SDK's `RemoteDocument` against rinch's `MockDomDocument` (the same component renders the
same tree through both); the host's op application and sandbox (a guest naming a node not
its own is refused); a toy plugin in the release desktop app and in the browser on the
local stack; a stuck plugin interrupted on the desktop; a plugin built against another ABI
refused with the outline fallback.

## 6. Open decisions for Joe

Taken by Joe on 2026-11-02 ("Sounds OK for now"): 1, a core module with a byte codec on
`wasm32-wasip1`; 3, a per-device plugins folder installed by hand. Taken the same day
for wave 2: 2, as recommended (a manifest asks for stores, all or chosen at approval,
and read or write; the person approves once per device on first use; remembered in
`<data dir>/pimble/plugins/approvals.json`; a plugin without approval mounts nothing and
the pane shows the outline with the request and "Allow"); 6, no network in these waves
(a plugin asking for it is refused at load). The rest are open.


1. **ABI:** a core module with a byte codec on `wasm32-wasip1` (recommended), or the
   component model/WIT (cleaner interfaces, but no native browser support).
2. **Permissions:** a plugin asks at install for stores (all or chosen), read or write,
   and network, and Joe approves once per device (recommended); or per store; or plugins
   get everything the person has.
3. **Where plugins live:** in a per-device plugins folder installed by hand (recommended
   for now), or as blobs in a store so they travel with it (convenient, but a shared
   store would hand code to every member).
4. **Search for plugin node types:** the server indexes them through the node document's
   generic units (text and fields) only (recommended), or plugins also ship a headless
   indexing export the server runs.
5. **A stuck plugin in the browser:** unloaded at its next turn with a notice
   (recommended; a browser cannot interrupt it), or plugins run in a Web Worker (safe to
   stop, but synchronous layout queries then cost a message each). Taken as recommended
   for wave 3 (2026-11-03): stopped when a turn that ran past its budget returns.
6. **Network access:** none in the first waves (recommended), or a host-mediated `fetch`
   gated by the install permission.

## What a plugin tells agents (2026-11-06)

A plugin may carry a guide for agents: what each of its viewers shows, the node fields it
reads, and its settings with a schema. The plugin's crate holds it as `guide.json`, and it
is embedded with `plugin!(describe: ..., view: ..., guide: include_bytes!("../guide.json"))`
as the module's `pimble-guide` custom section. The MCP reads it from the file; nothing
runs. No ABI change. docs/AGENT_GUIDES_CONTRACT.md.
