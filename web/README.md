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
| Mounting the app | `src/main.rs` |

Startup is three steps. `POST /api/v1/token` exchanges the `pimble_session`
cookie for a one-hour JWT and the server's WebSocket URL; a 401 sends the
visitor to `/login.html`. The backend connects `PimbleClient` to that URL with
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
`https://pimble.jkbase.app/app/`. The release build is about 3.9 MB of wasm.

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

**1. The accounts service** (`crates/pimble-cloud`) on port 8080:

```bash
export PIMBLE_SERVER_URL=http://127.0.0.1:7462
export PIMBLE_SERVER_TOKEN="$(cat ~/.config/pimble/server-token)"
export PIMBLE_STORES_DIR=/tmp/pimble-stores
export PIMBLE_CLOUD_DEV_SIGNING_SEED=$(head -c32 /dev/urandom | xxd -p -c64)
export PIMBLE_CLOUD_PUBLIC_URL=http://127.0.0.1:8080
cargo run -p pimble-cloud
```

With no `JKBASE_AUTH_ISSUER_URL` it signs tokens itself with that seed and
serves its own JWKS at `/api/v1/.well-known/jwks.json`. It needs a RhypeDB on
`127.0.0.1:4201`.

**2. The Pimble server** on port 7462, verifying those tokens and admitting the
dev origin:

```bash
mkdir -p /tmp/pimble-stores
cargo run -p pimble-cli --release -- server \
  --addr 127.0.0.1:7462 \
  --stores-dir /tmp/pimble-stores \
  --token-file ~/.config/pimble/server-token \
  --jwks http://127.0.0.1:8080/api/v1/.well-known/jwks.json \
  --issuer http://127.0.0.1:8080/api/v1 \
  --allow-origin http://127.0.0.1:8080
```

The static token is what the accounts service uses as the service principal; the
JWKS is what makes a user's JWT acceptable. Both verifiers may be on at once.
`--allow-origin` is what lets a browser connect at all: without it every request
carrying an `Origin` header is refused with 403, which is the right default for
the desktop app's embedded server.

**3. The app**:

```bash
cd web && trunk serve --release --port 8081
```

The `/rpc` proxy in `Trunk.toml` has a `ws://` backend, not `http://`: trunk's
WebSocket proxy rejects an `http` scheme with `Url(UnsupportedUrlScheme)` and
then every upgrade fails. A broken proxy shows up in the app as a connection
that is refused immediately and retried, so check trunk's own log first.

**4. Sign up** at the site's `signup.html` (or `curl -X POST
http://127.0.0.1:8080/api/v1/signup -d '{"email":"...","password":"..."}'`),
create a store, then open the app.

Because the session cookie is set by the accounts service, the browser must
reach the app and the API on one origin. The simplest local arrangement is to
put both behind `trunk serve`'s proxies (already configured) and use its port
for everything, matching `--allow-origin` and `PIMBLE_CLOUD_PUBLIC_URL` to it.

### Without pimble-cloud

The app only needs something that answers `POST /api/v1/token` with
`{ "token": ..., "exp": ..., "rpc_url": ... }`. A few lines of any static server
will do, with an empty token against a tokenless loopback Pimble server:

```bash
cargo run -p pimble-cli --release -- server \
  --addr 127.0.0.1:7462 --open ./demo.pimble \
  --allow-origin http://127.0.0.1:8099
```

That is how the collaboration path below was first verified.

## What has been verified

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

Not yet verified here: the same flow against `pimble-cloud` with real signup,
login and JWT verification, and the flow through `trunk serve`'s proxies.
