# Plugins contract: WASM guests that run plain rinch over the Pimble API

Status: drafted by the PM on 2026-11-02 from Joe's direction (memory
`project_views_and_plugins_2026_11_02`) and a read-only study of rinch at the pinned
revision (`2d1f11a0`, cargo checkout) and of `crates/pimble-plugins`. Nothing is built.
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
    exit); no interruption is possible, so a plugin that runs too long is unloaded at its
    next turn (open decision 5).
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
   `metanode:viewer = <id>` mounts the plugin that provides that viewer, with the source's
   forest and the viewer's settings as props; the plugin acts through `Pimble`, and core
   judges its edits. A viewer declares the source shapes it can show.
2. **A node type** (later): a pane opening a node whose type a plugin provides.
3. **Embedded in text** (later): a node view matched on `data-plugin=<id>`; the attribute
   verbs (`set_attrs`, `delete`, `select`) become host calls.

### 3.4 The API a plugin gets

- `Pimble` (the data API), never `Host` (stores, replicas, accounts, sharing, agents).
- **Authorized as the person running it**: the guest's calls go through the person's own
  backend, so the server judges them with the person's role; nothing extra is granted.
- **Narrowed by what it was given at install** (open decision 2): which stores, read or
  write, network.
- **Attributed**: edits carry `client_id = "plugin:<id>"`; presence shows the plugin like an
  agent.
- Changes: the guest's `Pimble` gets the same change stream (`next_change`) through the
  host, and the same `DocSession` plumbing for documents it shows.

### 3.5 Packaging

- A plugin is a `.wasm` file with a manifest it returns from `describe`: id, name, version,
  ABI version, rinch revision it was built against, what it provides (viewers and the source shapes they take, node
  types, embeds, recipes such as "New Kanban"), and what it asks for.
- **ABI and rinch versions are checked at load**: a plugin built against another ABI or an
  incompatible rinch is refused with a sentence, and the view falls back to an outline.
- Where plugins live and how they reach other devices: open decision 3.

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
3. **The browser host**: the same plugin running in the web app.
4. **Host slots** (the Editor first), overlay roots for portals, the forwarded global
   machinery.
5. **The kanban plugin** (`docs/KANBAN_CONTRACT.md`).

## 5. How it is checked

The SDK's `RemoteDocument` against rinch's `MockDomDocument` (the same component renders the
same tree through both); the host's op application and sandbox (a guest naming a node not
its own is refused); a toy plugin in the release desktop app and in the browser on the
local stack; a stuck plugin interrupted on the desktop; a plugin built against another ABI
refused with the outline fallback.

## 6. Open decisions for Joe

Taken by Joe on 2026-11-02 ("Sounds OK for now"): 1, a core module with a byte codec on
`wasm32-wasip1`; 3, a per-device plugins folder installed by hand. The rest are open.


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
   stop, but synchronous layout queries then cost a message each).
6. **Network access:** none in the first waves (recommended), or a host-mediated `fetch`
   gated by the install permission.
