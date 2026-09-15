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
whenever the socket drops, with backoff, using the freshest token it holds.

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

Against a real `pimble-cli server` with `--allow-origin`, a stub `/api/v1/token`
and the release `dist/`, in Chromium:

- the app boots, mints a token, connects, and `listStores` fills the tree;
- clicking a document opens it in the browser editor with its content;
- two tabs on the same document see each other's typing live, in both
  directions, and the result is on disk afterwards (`pimble-cli show-node`).

Not yet verified end to end: the same flow against `pimble-cloud` with real
signup, login and JWT verification, which is being built alongside this.
