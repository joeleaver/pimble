# pimble-web

Pimble in the browser. The same UI as the desktop app, mounted through
`rinch-web` instead of a window, over the same Pimble server the desktop
embeds, run here in a worker of the browser (docs/ONE_CODEBASE_CONTRACT.md).

This is its own cargo workspace on purpose: a wasm build wants its own lock, its
own profiles, and above all its own dependency graph. `cargo check --workspace`
in the repo root does not see it. Check it with
`cargo check --workspace --target wasm32-unknown-unknown` here (CI does): a
plain check skips the worker crate, which builds for wasm32 only.

## How it fits together

| Piece | Where |
| --- | --- |
| The UI, the state, the editor wiring | `pimble-app` built with `--no-default-features --features web` |
| What a `BackendCommand` means | `pimble_app::commands`, shared with the desktop |
| The server, in the leader tab's worker | `crates/pimble-worker` (built through `worker/`), `hub.js` |
| The page's side of it: the hub, the backend, sign-in and the account page | `src/worker_mode/` |
| Which page a URL means | `src/route.rs` |
| Sign up, "I forgot my password", the recovery link | `src/pages/`, over `src/accounts.rs` and `src/http.rs` |
| The page's own diagnostics recorder | `src/diagnostics.rs` |

## The browser's own server

The page runs the same Pimble server the desktop runs, in a dedicated worker of one leader
tab (docs/ONE_CODEBASE_CONTRACT.md sections 4 to 7, waves 4 and 5). Every read and write the
UI makes goes through the same `pimble_api::Pimble` and `pimble_app::commands` the desktop
uses, over `PimbleClient::over_message_port`; the server keeps the account's keys and this
browser's stores as replicas, and syncs them with Pimble Cloud through the same links the
desktop's server uses (the vault link for an encrypted store, the plain link for a plain
hosted one). Until wave 5 the page had a second path of its own (keys in the page, a vault
client, the hosted server read directly); it is gone, and `scripts/check-one-path.sh` (run by
CI) keeps a second one from coming back.

- `hub.js`: the leader tab (a Web Lock), the other tabs over a `BroadcastChannel`, the worker it
  starts, and the service worker it registers.
- The worker is built by trunk from `crates/pimble-worker` (its `entry` feature, the `worker`
  profile); `build/post_build.sh` (a trunk `post_build` hook) renames its files by their
  contents, names the loader in index.html's `pimble-worker-loader` meta tag, and writes
  `sw.js` from `build/sw.template.js`: the service worker that keeps this build's own files so
  the page opens with no network (never `/api`, `/rpc` or any store data; a new build replaces
  the cache whole).
- `src/worker_mode/`: the hub's bindings, the backend (the desktop's shape over the
  `MessagePort`), and the sign-in and account pages that talk to that server.
- The stores live in OPFS, sealed with the remembered device's disk key, or in memory with
  "Don't remember this browser" (docs/ONE_CODEBASE_CONTRACT.md section 7). A reload asks for
  no password: the device key unwraps the account's keys.

## The account pages

`/app/signup`, `/app/login`, `/app/forgot`, `/app/recover` and `/app/account` are rinch views
in this same wasm binary, not static HTML. Moving between any of them and the app is
`crate::route`, which pushes a history entry and swaps what is mounted, never a link that
reloads: the app is mounted once and afterwards only hidden and shown, because behind it sit
the connection to this browser's server, its subscriptions and the editors' sessions.

**Signup** (`src/pages/signup.rs`) takes an email and a password twice (a mismatch is refused
here, before anything is derived or sent). It then generates a fresh `KdfParams`, derives the
password into an `auth_key` and a KEK, generates an X25519/Ed25519 account keypair, and wraps
that keypair twice: once under the password's KEK and once under a KEK derived from a recovery
code. `POST /api/v1/signup` carries `auth_key`, `kdf`, `public_keys`, `account_key_blob`,
`recovery_salt` and `recovery_key_blob`, never the password, never an unwrapped key. The
recovery code is shown once, behind an "I have written this down" confirmation.

**Forgot and recover** (`src/pages/forgot.rs`, `recover.rs`) mail a recovery link and, on the
link's page, unwrap the account keys with the recovery code and wrap them under a new password.

**Sign-in and the account page** (`src/worker_mode/pages.rs`, `account_sections.rs`) go to the
server: `cloudSignIn` derives and unwraps there, and the account page's sections (the
password, a new recovery code, whole-store members, keeping what you have read in step) are
the server's RPCs (docs/ONE_CODEBASE_CONTRACT.md, wave 5 parts 3 and 4). The sidebar's
account button is registered by this crate through `pimble_app::app::set_account_action`, so
the desktop, which registers nothing, does not draw one.

Every path under `/app/` has to serve the same `index.html`. `trunk serve` does that already,
and jkbase's edge routes the whole prefix.

## Controls, and the ones that would do nothing

A browser build has no menu bar, so anything the desktop reaches only through a
menu has to be reachable another way or not be offered. What that came to:

- **The explorer's "+"** makes a node under the selected store, as on the
  desktop. With no store open the desktop offers "New Store..." from the File
  menu; here "+" opens that modal instead of doing nothing. It asks for a name,
  creates the store on this browser's server (encrypted) and hosts it with
  `cloudHostStore`, and the store arrives through `listStores` like any other.
- **Ctrl+K** focuses the search box, whose placeholder has always advertised it.
  Both shells carry that accelerator on their View menu, and rinch-web's menu
  bar arms it against the document, so neither binds it by hand.
- **"Mount Store..."** is offered here too: `createMount` is a write an editor
  may make, and where the desktop picks a directory the browser picks one of the
  stores the account already grants, leaving out the one being mounted into.
- **Mounts** are hidden from an encrypted store's context menu. The server holds
  only blobs there and has nothing to point a mount at, so "Copy as Mount
  Source" and "Paste Mount Here" would fail wherever they were pressed.
  Hiding beats disabling: there is nothing the reader could do to enable them.
- **The empty state** says to use "+" rather than naming a shortcut that only
  the desktop has.
- **Right-click** shows Pimble's menu and not the browser's (see below).
- **Rebuild Search Index** and **Sign Out** are in the menu bar, which the
  browser now has.

The desktop's items that do nothing (Undo/Redo/Cut/Copy/Paste, disabled; Toggle
Sidebar, the zoom items, Documentation, About, which log and return) were left
alone: they are the desktop menu's, not this build's, and changing them is not
this crate's business.

### Menus

`crates/pimble-app/src/menus.rs` is the menus as data — a title, and per item a
label, an accelerator and an action — so the desktop's native bar and the
browser's DOM one cannot drift. Items that need a file dialog or a service
principal are `native`; the ones that need an account are not. `build_menus`
turns the spec into `rinch::menu::Menu`s, and both shells call it: the desktop
hands the result to `App::menu`, and `src/route.rs` hands the very same values
to `rinch_web::mount_into_with_menu_bar`. So the browser shows File (New
Store..., Account, Sign Out), Edit (Delete) and View (Toggle Dark Mode, Focus
Search, Rebuild Search Index), and the desktop shows its own File with the
dialogs and the remote-store items.

That call also arms every item's accelerator against the document, which is why
nothing in this crate binds Ctrl+K any more.

### The browser's own context menu

A right-click used to show Pimble's menu with the browser's on top of it.
rinch-web already calls `prevent_default()` on `contextmenu`, but only when
`closest("[data-oncontextmenu]")` finds a handler
(`crates/rinch-web/src/event_delegation.rs`, the listener installed at the end
of `setup_event_delegation`), so every right-click outside a menu target — the
tree's padding, the editor, and `ContextMenu`'s own portalled overlay while a
menu is open — left the native one to appear. `src/shortcuts.rs` cancels it for
the whole page: a bubble-phase listener on `document` that never stops
propagation, so rinch's own listener still runs and its menu still opens.

`src/shortcuts.rs` now asks rinch to do it: `set_suppress_native_context_menu(true)`.
It is a flag rather than a widening because unconditional suppression would take
right-click-copy away from a host page hydrating rinch islands, and it also
closes a gap a page-level listener cannot reach, where a stale
`data-oncontextmenu` suppressed the default and dispatched nothing.

## What the browser build leaves out

The browser's stores are the account's, kept as replicas by this browser's
server; there are no folders to open or remotes to link by hand. So the web
build does not offer what administers stores on a disk: no "Mount
Store...", "Mount Remote Store Here...", "Link to Remote...", "Unlink from
Remote", "Remove Replica..." or "Close Store" in the tree's context menus, and
no "press Ctrl+N to create a new store" in the empty editor. Stores are created
with the explorer's "+". "New Node", "Copy as Mount Source", "Paste
Mount Here", "Rename", "Appearance..." and "Delete" are ordinary writes and stay.

One flag decides this, `CAN_ADMINISTER_STORES` in `crates/pimble-app/src/app/mod.rs`,
so the desktop app is unchanged.

## Build

```bash
cargo install trunk          # once
cd web
trunk build --release        # writes dist/
```

`Trunk.toml` sets `public_url = "/app/"`, because jkbase serves this under
`https://pimble.jkbase.app/app/`. The release build is about 4.3 MB of wasm
(the account pages and `pimble-crypto` added roughly 400 KB).

**This workspace and the root are on rinch's `feat/web-menu-bar` (PR #791)**,
which is what makes `rinch::menu` build without the `desktop` feature and adds
rinch-web's menu-bar entry points. Both must name the same branch: a workspace
that ends up with two rinch sources gets two of every rinch type, and cargo
will not patch a git source with another branch of the same repository. Move
them together, back to `main` once the PR merges.

To move the pinned rinch revision, do it here as well as in the root workspace:

```bash
cd web && cargo update -p rinch --precise <sha>
```

Both locks should name the same revision. `pimble-app` is a path dependency, so
this workspace compiles it from source and its rinch must be the same one.

## Running the whole stack locally

Four processes, one origin. In production jkbase's edge is the origin and routes
`/`, `/app/`, `/api/*` and `/rpc` to the four pieces; locally `trunk serve`
plays that part through the proxies already in `Trunk.toml`.

**0. RhypeDB**, the accounts database, on `127.0.0.1:4201`:

```bash
rhypedb-server --schema crates/pimble-cloud/schema.rhype --data-dir /tmp/pimble-accounts
```

**1. The accounts service** (`crates/pimble-cloud`) on port 8080:

```bash
export RUST_LOG=info                       # the verification link is logged
export PIMBLE_SERVER_URL=http://127.0.0.1:7462
export PIMBLE_SERVER_TOKEN="$(cat ~/.config/pimble/server-token)"
export PIMBLE_STORES_DIR=/tmp/pimble-stores
export PIMBLE_CLOUD_DEV_SIGNING_SEED=$(head -c32 /dev/urandom | xxd -p -c64)
export PIMBLE_CLOUD_KDF_DECOY_SECRET=$(head -c32 /dev/urandom | xxd -p -c64)
export PIMBLE_CLOUD_PUBLIC_URL=http://127.0.0.1:8081
cargo run -p pimble-cloud
```

With no `JKBASE_AUTH_ISSUER_URL` it signs tokens itself with that seed and
serves its own JWKS at `/api/v1/.well-known/jwks.json`. `PIMBLE_CLOUD_PUBLIC_URL`
is **the origin the browser uses**, not this service's own port: it is what the
tokens' `iss` is built from and what `POST /api/v1/token` reports as `rpc_url`,
so it has to be the port everything is behind. With no `RESEND_API_KEY` the
verification link is written to this service's log instead of being emailed.

**2. The Pimble server** on port 7462, verifying those tokens and admitting the
dev origin:

```bash
mkdir -p /tmp/pimble-stores
cargo run -p pimble-cli --release -- server \
  --addr 127.0.0.1:7462 \
  --stores-dir /tmp/pimble-stores \
  --token-file ~/.config/pimble/server-token \
  --jwks http://127.0.0.1:8080/api/v1/.well-known/jwks.json \
  --issuer http://127.0.0.1:8081/api/v1 \
  --allow-origin http://127.0.0.1:8081
```

The static token is what the accounts service uses as the service principal; the
JWKS is what makes a user's JWT acceptable. Both verifiers may be on at once.
`--allow-origin` is what lets a browser connect at all: without it every request
carrying an `Origin` header is refused with 403, which is the right default for
the desktop app's embedded server.

`--issuer` has to match the accounts service's `PIMBLE_CLOUD_PUBLIC_URL` plus
`/api/v1`, or every WebSocket is closed the moment it opens with nothing in
either log to say why. The JWKS URL is a server-to-server fetch and can stay on
the service's own port.

**Start this one before the accounts service.** The two depend on each other and
only one of them tolerates it: the accounts service exits if the Pimble server
is not there when it starts, while the Pimble server only warns that its first
JWKS fetch failed and retries on the first token it cannot verify (a minute at
worst). So: Pimble server, then accounts service.

**3. The app**:

```bash
cd web && trunk serve --release --port 8081
```

The `/rpc` proxy in `Trunk.toml` has a `ws://` backend, not `http://`: trunk's
WebSocket proxy rejects an `http` scheme with `Url(UnsupportedUrlScheme)` and
then every upgrade fails. A broken proxy shows up in the app as a connection
that is refused immediately and retried, so check trunk's own log first. The
proxy must also strip the `/rpc` prefix — the Pimble server serves at the root —
which `rewrite = "/rpc"` does.

**4. Sign up** at `http://127.0.0.1:8081/app/signup`. Write the recovery code
down (it is shown once), then follow the verification link out of the accounts
service's log:

```bash
# from the pimble-cloud log
curl -s -o /dev/null -w '%{redirect_url}\n' "http://127.0.0.1:8081/api/v1/verify?token=..."
```

That redirects to `/app/login?verified=1`. Sign in there and make a store with
the explorer's "+".

Because the session cookie is set by the accounts service, the browser must
reach the app and the API on one origin. Everything above assumes `trunk
serve`'s port is that origin, which is why `PIMBLE_CLOUD_PUBLIC_URL`,
`--issuer` and `--allow-origin` all name it.

### Without pimble-cloud

For the **signup and recovery pages alone**, anything that stores what signup sends and
hands it back will do: the crypto under test is the browser's, and a stub service never
needs to understand a blob.

## What has been verified

What each wave proved, in Chromium and Firefox against the local stack, is in the "Proof"
sections of docs/ONE_CODEBASE_CONTRACT.md (waves 4 and 5). The verification of the page's
earlier path (the account pages, the vault client and the controls, 2026-09-16 to 2026-11-22)
is in this file's git history.

## Two dev-only traps seen on 2026-09-16

- `trunk serve`'s `/api` proxy follows redirects against the backend, so opening the
  verification link through port 8081 verifies the account but shows a 404; open the link
  against the accounts service's own port, or accept the 404 and sign in. jkbase's edge does
  not follow redirects.
- The desktop app's embedded server binds `127.0.0.1:7462`; a hosted server for the local
  stack must use another port (`--addr 127.0.0.1:17463`, `PIMBLE_SERVER_URL` to match, and
  a copy of `Trunk.toml` with an absolute `target` and the `/rpc` backend on that port,
  passed with `trunk serve --config`).
