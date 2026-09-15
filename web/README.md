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

Two things must agree or every WebSocket is closed the moment it opens, with
nothing in either log to say why. `--issuer` has to match the accounts service's
`PIMBLE_CLOUD_PUBLIC_URL` plus `/api/v1` (the JWKS URL is a server-to-server
fetch and can stay on the service's own port), and the Pimble server has to be
started **after** the accounts service, because it fetches the JWKS once at
startup and keeps an empty key set if that fetch fails.

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
