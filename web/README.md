# pimble-web

Pimble in the browser. The same UI as the desktop app, mounted through
`rinch-web` instead of a window, talking to a hosted Pimble server instead of an
embedded one.

This is its own cargo workspace on purpose: a wasm build wants its own lock, its
own profiles, and above all its own dependency graph. `cargo check --workspace`
in the repo root does not see it.

## How it fits together

| Piece | Where |
| --- | --- |
| The UI, the state, the editor wiring | `pimble-app` built with `--no-default-features --features web` |
| What a `BackendCommand` means | `pimble_app::commands::process_command`, shared with the desktop |
| Credential, connection, reconnect | `src/backend.rs` |
| Minting a token from the session cookie | `src/api.rs` |
| Which page a URL means | `src/route.rs` |
| Sign up, sign in, the account | `src/pages/` |
| The account keys, in memory | `src/session.rs` |
| Store keys and envelopes | `src/keys.rs` |
| Encrypted stores | `src/vault.rs` |
| Everything the accounts service answers | `src/accounts.rs`, over `src/http.rs` |
| Starting the app | `src/app_page.rs` |

Startup is three steps. `POST /api/v1/token` exchanges the `pimble_session`
cookie for a one-hour JWT and the server's WebSocket URL; a 401 sends the
visitor to `/app/login`. The backend connects `PimbleClient` to that URL with
the token in the query string (a browser `WebSocket` cannot set headers, so the
server reads `access_token`). `ListStores` then fills the tree with whatever the
token's grants allow. There is no "open a store by path" here: the accounts
service creates stores, and the token says which ones this account may see.

The token is refreshed five minutes before it expires. The loop reconnects
whenever the socket drops, using the freshest token it holds.

A word on what counts as connected. jsonrpsee's wasm client answers `connect`
before the browser has opened the socket, so a refused endpoint looks like a
success for an instant. The backend therefore proves every connection with one
RPC (`listStores`, which the tree wants anyway) before telling the UI it is
connected, and it only forgets the current backoff once a connection has also
stayed up for two seconds. Failed attempts back off from one second to thirty
and say so once per outage, not once per attempt.

## One binary, several servers

Nothing here talks to "the" Pimble server. `POST /api/v1/token` names the server
the store list comes from, and may name a different one per store; `src/endpoints.rs`
holds one credential and one `PimbleClient` per URL, and every command is
answered through the endpoint that serves the store it names
(docs/CRYPTO_CONTRACT.md, "Endpoint-agnostic"). Today the token response names
one URL and every store resolves to it, so there is exactly one connection. When
a relayed share arrives — served by its owner's machine rather than the hosted
server — it becomes a second entry with its own token, and nothing else in the
app changes.

The backend loop supervises the endpoint the store list comes from, because that
is the one whose absence means there is nothing to show. Any other endpoint
connects the first time a store on it is touched.

## The account pages

`/app/signup`, `/app/login` and `/app/account` are rinch views in this same wasm
binary, not static HTML, and they are here rather than in `site/` for one
reason: signing in derives keys that have to stay in the page's memory, and a
page that reloads cannot hold them.

**Signup** takes an email and a password twice (a mismatch is refused here,
before anything is derived or sent). It then generates a fresh `KdfParams`,
derives the password into an `auth_key` and a KEK, generates an X25519/Ed25519
account keypair, and wraps that keypair twice: once under the password's KEK and
once under a KEK derived from a recovery code. `POST /api/v1/signup` carries
`auth_key`, `kdf`, `public_keys`, `account_key_blob`, `recovery_salt` and
`recovery_key_blob` — never the password, never an unwrapped key. The recovery
code is then shown once, behind an "I have written this down" confirmation,
after which it is dropped from the page and the inbox state takes over.

**Login** fetches `GET /api/v1/kdf?email=` for the salt, derives, and sends
`POST /api/v1/login { email, auth_key }`. `?verified=1` and
`?verify_error=expired|invalid` become banners (the accounts service redirects
verification outcomes to this path), and a `403 email_unverified` offers to send
the link again. After login it fetches `GET /api/v1/me/keys` and unwraps the
account keys with the KEK.

The same page also **unlocks**. If the session cookie is still good but the keys
are gone — which is every reload — it asks only for the password, fetches the
wrapped blob once, and either opens it or does not. A wrong password fails right
there, in the browser, with nothing further sent.

**The account page** lists the account's stores, creates one (encrypted by
default, which also mints its key and seals it to the creator), adds and removes
members with roles, and logs out. Adding a member to an encrypted store also
seals that store's key to them; without that they would have a grant and
unreadable blobs.

Moving between any of these and the app is `crate::route`, which pushes a
history entry and swaps what is mounted — never a link that reloads. The app is
mounted once and afterwards only hidden and shown, because behind it sit a
WebSocket, subscriptions, an editor session and the vault client. Coming back to
it re-mints the token and reconnects, since a store created on the account page
is invisible to a token minted before that grant existed. The sidebar's account
button is registered by this crate through `pimble_app::app::set_account_action`,
so the desktop, which registers nothing, does not draw one.

Every path under `/app/` has to serve the same `index.html`. `trunk serve` does
that already, and jkbase's edge routes the whole prefix.

## Keys, and where they are not

The unwrapped account keys live in one thread-local in `src/session.rs` for as
long as the page does. They are never written to `localStorage`,
`sessionStorage`, a cookie or the console, and never sent anywhere. The session
cookie survives a reload; the keys deliberately do not, so a reload asks for the
password again and `/app/` with no keys sends the visitor to `/app/login`.

A store key reaches an account as a `KeyEnvelope`: sealed to their X25519 public
key, signed by whoever sent it, held by the accounts service, which cannot open
it. `src/keys.rs` fetches the envelopes for a store and opens the ones this
account can, keeping every key id so a rotated store still reads its own
history. An envelope is verified against the signing key it carries rather than
one fetched independently — trust on first use, which the contract's threat
model states out loud and phase 2a accepts.

Argon2id at the contract's cost (32 MiB, 3 passes) takes between about 60 and
260 ms in Chrome on a desktop machine, measured through the line
`src/session.rs` logs on every derivation. It blocks the only thread there is,
so every page that derives yields to the browser first — otherwise the button
never repaints as busy.

## Encrypted stores

A store of kind `vault` holds nothing the server can read: an append-only log of
opaque blobs per document, and nothing else. `src/vault.rs` is what makes those
blobs a tree of notes.

It sits in front of `pimble_app::commands::process_command`. A command naming a
vault store is answered from documents this page holds; anything else is handed
straight back, so the plain path is untouched. On connect it fetches each vault
store's keys and its `tree` document, decrypts every blob, and builds a
`pimble_crdt::StoreDocument` in the browser — the same store document the
desktop's server holds, just built here. The tree UI then reads that instead of
calling `getChildren`. A store whose log is empty (one the accounts service has
just created) is seeded from here, under the id and root the server already
minted.

Node content works the same way: `vaultFetch`, decrypt, merge into a
`ContentDoc`, and hand the editor its bytes through the ordinary `NodeLoaded`
path. Outbound edits arrive as `BroadcastChanges` exactly as they do for a plain
store; they are merged, encrypted with `Blob::encrypt` (associated data
`blob_aad(store_id, doc_id)`, so a blob cannot be replayed into another
document) and appended with `vaultAppend`. Tree edits — create, rename, move,
delete, appearance — mutate the store document and append the difference that
mutation made, never the whole document.

`VaultAppended` notifications come back through the store subscription with the
blob attached. The subscription task only forwards them; decrypting and merging
needs the vault client itself, which lives in the backend loop, so they travel
down a channel it drains once per turn. This client's own appends are dropped by
sequence number. A blob whose key id this device does not hold is logged and
skipped, never fatal. A snapshot goes up every 200 appends per document.

Two things are worth knowing. `RemoteChanges` carries no node identity and the
app has one editor pane, so a decrypted content update only becomes one for the
node the editor has open — learned from `SubscribeNodeChanges`, which the vault
client answers itself rather than subscribing again. And `getChildren` loads the
content of the children it returns, because the app derives a document's tree
label from its first line and opens it from the bytes it was handed, with no
second fetch in between.

Search over an encrypted store is client-side: a case-insensitive substring over
decrypted titles and whatever content is loaded. There is no server index for
such a store and there cannot be one. Plain stores named in the same query still
go to the server, and the two sets of hits are merged.

## Controls, and the ones that would do nothing

A browser build has no menu bar, so anything the desktop reaches only through a
menu has to be reachable another way or not be offered. What that came to:

- **The explorer's "+"** makes a node under the selected store, as on the
  desktop. With no store open the desktop offers "New Store..." from the File
  menu; here "+" opens that modal instead of doing nothing. It asks for a name,
  creates the store through `POST /api/v1/stores` as a vault, mints its key and
  seals it to the account, then asks for a fresh token and reconnects — a store
  is invisible to a token minted before the grant existed. The store then
  arrives through `listStores` like any other.
- **Ctrl+K** focuses the search box, whose placeholder has always advertised it.
  The desktop carries that accelerator on its View menu; `src/shortcuts.rs`
  binds it here.
- **Mounts** are hidden from an encrypted store's context menu. The server holds
  only blobs there and has nothing to point a mount at, so "Copy as Mount
  Source" and "Paste Mount Here" would fail wherever they were pressed.
  Hiding beats disabling: there is nothing the reader could do to enable them.
- **The empty state** says to use "+" rather than naming a shortcut that only
  the desktop has.
- **Right-click** shows Pimble's menu and not the browser's (see below).
- **Rebuild Search Index** and **Sign Out** have no route yet. They are in the
  shared menu spec and arrive with the menu bar; nothing in the UI offers them
  in the meantime, so nothing is offered that fails.

The desktop's items that do nothing (Undo/Redo/Cut/Copy/Paste, disabled; Toggle
Sidebar, the zoom items, Documentation, About, which log and return) were left
alone: they are the desktop menu's, not this build's, and changing them is not
this crate's business.

### Menus

`crates/pimble-app/src/menus.rs` is the menus as data — a title, and per item a
label, an accelerator and an action — so the desktop's native bar and the
browser's DOM one cannot drift. Items that need a file dialog or a service
principal are `native`; the ones that need an account are not. `build_menus`
turns the spec into `rinch::menu::Menu`s and is `native` only because
`rinch::menu` is behind rinch's `desktop` feature. When rinch-web gains a menu
bar, that `cfg` comes off and mounting the app becomes one call with the spec
passed in. `src/menu.rs` registers the two actions no shared crate could
perform on its own (reaching the accounts service, dropping this page's keys)
and holds the test that every item the web configuration offers has an action
behind it.

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

That fix has landed upstream, on rinch PR #791's branch rather than on `main`:
`rinch_web::set_suppress_native_context_menu(bool)`, a flag rather than a
widening, because unconditional suppression would take right-click-copy away
from a host page hydrating rinch islands. The same commit closes a gap this
crate's listener cannot reach, where a stale `data-oncontextmenu` suppressed the
default and dispatched nothing. When this workspace is on a rinch that has it,
call it with `true` where the app mounts and delete the `contextmenu` half of
`src/shortcuts.rs`; the Ctrl+K half stays until the menu bar carries that
accelerator.

## What the browser build leaves out

A browser connects as a signed-in user, whose token carries per-store grants and
nothing else. The service principal's RPCs — open a store from a path, add a
remote as a replica, link, unlink, remove a replica, close a store, create one —
are refused for that principal, so the web build does not offer them: no "Mount
Store...", "Mount Remote Store Here...", "Link to Remote...", "Unlink from
Remote", "Remove Replica..." or "Close Store" in the tree's context menus, and
no "press Ctrl+N to create a new store" in the empty editor. Stores are created
and shared on the account pages. "New Node", "Copy as Mount Source", "Paste
Mount Here", "Rename", "Appearance..." and "Delete" are ordinary writes and stay.

One flag decides this, `CAN_ADMINISTER_STORES` in `crates/pimble-app/src/app.rs`,
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

That redirects to `/app/login?verified=1`. Sign in there, make a store on
`/app/account` (encrypted by default), and open it with "Open Pimble".

Because the session cookie is set by the accounts service, the browser must
reach the app and the API on one origin. Everything above assumes `trunk
serve`'s port is that origin, which is why `PIMBLE_CLOUD_PUBLIC_URL`,
`--issuer` and `--allow-origin` all name it.

### Without pimble-cloud

There are two smaller arrangements, depending on what is being looked at.

For the **account pages alone**, anything that stores what signup sends and
hands it back at login will do: the crypto under test is the browser's, and a
stub service never needs to understand a blob. That is enough to walk signup,
the recovery code, login, unlock and a wrong password.

For the **app alone**, all it needs is something that answers
`POST /api/v1/token` with `{ "token": ..., "exp": ..., "rpc_url": ... }`, with
an empty token against a tokenless loopback Pimble server:

```bash
cargo run -p pimble-cli --release -- server \
  --addr 127.0.0.1:7462 --open ./demo.pimble \
  --allow-origin http://127.0.0.1:8099
```

That is how the collaboration path below was first verified.

## What has been verified

### The account pages and the encrypted store, against the real stack

In headless Chrome against all four processes (RhypeDB, `pimble-cloud`,
`pimble-cli server`, and the release bundle behind a one-origin reverse proxy
standing in for `trunk serve`):

- signing up derives keys in the browser, shows a 32-character recovery code
  once behind an acknowledgement, and lands on the check-your-inbox state;
- mismatched passwords are refused by the page, with nothing sent;
- the verification link redirects to `/app/login?verified=1`, and signing in
  there reaches the app;
- a wrong password on the unlock path fails locally: `GET /me` and
  `GET /me/keys` and nothing after them, no `POST /login`;
- `/app/account` creates an encrypted store, mints its key and seals it to the
  creator, and lists it as encrypted;
- the app opens that store: the tree is built from a decrypted store document,
  a new node is created through it, and typing into that node works;
- the server's copy is `vault/tree/log` and `vault/<node id>/log`, each blob
  `PB` + version + key id + nonce + ciphertext, with neither the store's name
  nor a word of the note anywhere on its disk;
- a second tab, signed in separately, decrypts the same store and shows the
  text — in the tree label and in the editor.

Argon2id at the contract's cost took 56–254 ms across runs in that browser.

### The controls, against the same stack

From a fresh account each run, so the tree starts empty:

- the explorer's "+" opens the New Store modal, refuses an empty name in the
  page, and otherwise creates an encrypted store that appears in the tree once
  the backend has a token carrying the new grant;
- a right-click on a tree row opens Pimble's menu with the event's
  `defaultPrevented` true, and a right-click anywhere else is cancelled too;
- that menu, on an encrypted store, offers "New Node" and "Appearance..." and
  no mount items;
- Ctrl+K puts the caret in the search box;
- the theme choice written to `localStorage` survives a reload.

The encrypted round trip was then re-run against a store made by "+" rather than
by the account page, after the endpoint registry replaced the single connection:
a node created, typed into, the server holding only ciphertext, and a second tab
decrypting the same text.

### The plain path

In Chromium, against a real `pimble-cli server` started with `--allow-origin`,
with the release build served by a plain static server and a stub answering
`POST /api/v1/token`. That arrangement gives the app one origin without trunk in
the way, so it exercises the app and the server but **not** the `trunk serve`
proxies:

- the app boots, mints a token, connects, and `listStores` fills the tree;
- clicking a document opens it in the browser editor with its content;
- two tabs on the same document see each other's typing live, in both
  directions, and the result is on disk afterwards (`pimble-cli show-node`);
- pointed at a port with nothing listening, the backend makes 7 attempts in 65
  seconds (1s, 2s, 4s, 8s, 16s, 30s, 30s), never claims to be connected, and
  writes one error to the status bar rather than one per attempt;
- pointed at the real server, it connects once and stays connected, with no
  retries and no errors.

Not yet verified here: the flow through `trunk serve`'s own proxies (the runs
above used an equivalent reverse proxy), a store shared with a second account,
key rotation, and the snapshot path (200 appends to one document).

## Two dev-only traps seen on 2026-09-16

- `trunk serve`'s `/api` proxy follows redirects against the backend, so opening the
  verification link through port 8081 verifies the account but shows a 404; open the link
  against the accounts service's own port, or accept the 404 and sign in. jkbase's edge does
  not follow redirects.
- The desktop app's embedded server binds `127.0.0.1:7462`; a hosted server for the local
  stack must use another port (`--addr 127.0.0.1:17463`, `PIMBLE_SERVER_URL` to match, and
  a copy of `Trunk.toml` with an absolute `target` and the `/rpc` backend on that port,
  passed with `trunk serve --config`).
