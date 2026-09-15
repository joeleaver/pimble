# pimble-cloud

The Pimble Cloud accounts service: users, sessions, hosted stores, grants,
and the JWTs a Pimble server verifies. See `docs/CLOUD_CONTRACT.md` at the
repo root for the full design; this file is how to run it.

## What it needs

- A **RhypeDB** server, holding this crate's schema (`schema.rhype`).
- A **Pimble server** (`pimble-cli server`), reachable with a static token —
  this service is that token's one holder (the "service principal").

## Running it locally

1. **Start RhypeDB** against this crate's schema:

   ```bash
   # from a checkout of https://github.com/joeleaver/rhypedb (branch master)
   cargo run --release -p rhypedb-server -- \
     --schema /path/to/pimble/crates/pimble-cloud/schema.rhype \
     --data-dir /tmp/pimble-cloud-rhypedb \
     --listen 127.0.0.1:4200 \
     --tcp-listen 127.0.0.1:4201
   ```

2. **Start a Pimble server** with a static token, and a directory for
   accounts-created stores:

   ```bash
   mkdir -p /tmp/pimble-cloud-stores
   cargo run --release -p pimble-cli -- server \
     --addr 127.0.0.1:7462 \
     --token-file /tmp/pimble-server-token   # any file; create one with e.g. `openssl rand -hex 32 > /tmp/pimble-server-token`
   ```

3. **Run `pimble-cloud`**:

   ```bash
   RHYPEDB_ADDR=127.0.0.1:4201 \
   PIMBLE_SERVER_URL=http://127.0.0.1:7462 \
   PIMBLE_SERVER_TOKEN="$(cat /tmp/pimble-server-token)" \
   PIMBLE_STORES_DIR=/tmp/pimble-cloud-stores \
   PIMBLE_CLOUD_DEV_SIGNING_SEED="$(openssl rand -hex 32)" \
   PIMBLE_CLOUD_PUBLIC_URL=http://127.0.0.1:8080 \
   PORT=8080 \
   cargo run --release -p pimble-cloud
   ```

   With `JKBASE_AUTH_ISSUER_URL` unset (as above), tokens are signed locally
   with an Ed25519 key derived from `PIMBLE_CLOUD_DEV_SIGNING_SEED`; leaving
   that unset too still starts the server, but logs a warning and uses a
   random key for this process only (fine for a single manual test, useless
   across a restart or with more than one instance).

## Environment variables

| Variable | Meaning | Default |
| --- | --- | --- |
| `PORT` | axum bind port | `8080` |
| `RHYPEDB_ADDR` | RhypeDB's binary-protocol address | `127.0.0.1:4201` |
| `PIMBLE_SERVER_URL` | the hosted Pimble server | `http://127.0.0.1:7462` |
| `PIMBLE_SERVER_TOKEN` | the service principal's static token | none (tokenless; only valid against a Pimble server also started without a token, e.g. on loopback for local testing) |
| `PIMBLE_STORES_DIR` | where `POST /stores` creates new store directories | `/app/data/stores` |
| `JKBASE_AUTH_ISSUER_URL` | jkbase-Auth's per-project token endpoint; unset means local dev signing | unset |
| `JKBASE_AUTH_KEY` | the `jkbk_…` bearer key for the above | unset |
| `PIMBLE_CLOUD_DEV_SIGNING_SEED` | 32 bytes hex, the local signing key (dev mode only) | random (logs a warning) |
| `PIMBLE_CLOUD_PUBLIC_URL` | this service's own public origin | `http://127.0.0.1:8080` |
| `GITHUB_REPO` | `owner/repo` for `/releases` | `joeleaver/pimble` |
| `PIMBLE_CLOUD_RELEASES_BASE_URL` | overrides the GitHub API base URL for `/releases` | `https://api.github.com` (not part of the contract; exists so tests can point this at a local stub instead of the network) |

`PIMBLE_CLOUD_PUBLIC_URL`'s scheme decides two things: whether the session
cookie is marked `Secure`, and whether `/token`'s `rpc_url` is `ws://` or
`wss://`. `rpc_url` is `<PIMBLE_CLOUD_PUBLIC_URL scheme-swapped>/rpc` — the
same origin the accounts API is served from, not `PIMBLE_SERVER_URL` (in
production, jkbase's edge proxies both to their respective internal
services on the same VM; see `docs/CLOUD_CONTRACT.md` "Layout on jkbase").
Locally there is no such proxy, so `rpc_url` from a local `pimble-cloud`
won't resolve on its own — connect a `PimbleClient` straight to
`PIMBLE_SERVER_URL` with the minted `token` for manual testing instead.

## Example requests

```bash
# Sign up (also logs in: sets the session cookie and returns a token)
curl -sc cookies.txt -X POST http://127.0.0.1:8080/api/v1/signup \
  -H 'Content-Type: application/json' \
  -d '{"email": "alice@example.com", "password": "correct horse battery staple"}'

# Who am I
curl -sb cookies.txt http://127.0.0.1:8080/api/v1/me

# Create a hosted store (creates it on the Pimble server + an owner grant)
curl -sb cookies.txt -X POST http://127.0.0.1:8080/api/v1/stores \
  -H 'Content-Type: application/json' -d '{"name": "My Notes"}'

# List my stores
curl -sb cookies.txt http://127.0.0.1:8080/api/v1/stores

# Add a member
curl -sb cookies.txt -X PUT http://127.0.0.1:8080/api/v1/stores/<store-id>/members \
  -H 'Content-Type: application/json' -d '{"email": "bob@example.com", "role": "editor"}'

# Mint a fresh JWT for the Pimble server (Authorization: Bearer <session> works in
# place of the cookie, too — this is what a non-browser caller uses)
curl -sb cookies.txt -X POST http://127.0.0.1:8080/api/v1/token

# JWKS, the latest release, and the health check need no auth
curl -s http://127.0.0.1:8080/api/v1/.well-known/jwks.json
curl -s http://127.0.0.1:8080/api/v1/releases
curl -s http://127.0.0.1:8080/api/v1/health
```

## Tests

```bash
cargo test -p pimble-cloud --release
```

Every test drives the real axum app over real HTTP, backed by a **real**
`rhypedb-server` subprocess and a real in-process
[`pimble_server::PimbleServer`] — nothing about RhypeDB or the Pimble server
is stubbed. `rhypedb-server` isn't a workspace member (`rhypedb` is a sibling
repo checked out at `~/dev/rhypedb`; see the repo's `CLAUDE.md`), and its
only public library entry point (`rhypedb_server::run()`) parses the calling
process's own `argv` via `clap` and calls `std::process::exit` on any
problem — unusable from inside a test. So each test spawns the `rhypedb-server`
binary as a subprocess, found (in order) via `$RHYPEDB_SERVER_BIN`,
`~/dev/rhypedb/target/release/rhypedb-server`,
`~/dev/rhypedb/target/debug/rhypedb-server`, or `$PATH`, and **skips itself
cleanly with a message on stderr** if none of those exist. `AppState`'s
`ReleasesCache` can point at a local stub instead of the real GitHub API
(`PIMBLE_CLOUD_RELEASES_BASE_URL`, wired through in tests, not documented
elsewhere) so the `/releases` tests never touch the network.

## Design notes not obvious from the contract

- **`Grant`'s `user_rid`/`store_rid`/`store_uuid` and `Session`'s `user_rid`**
  are redundant scalar copies of the `user`/`store` relations the contract
  specifies. RhypeDB's query language has no relation-equality filter —
  confirmed by reading `rhypedb-query`'s parser and executor:
  `Predicate::Compare`'s right-hand side is always a scalar `Literal`, and
  `evaluate_predicate` reads the comparison field straight out of the
  object's own field map, which never holds a relation. Without the scalar
  copies, checking "does a grant already exist for (user, store)", listing a
  store's members, and building a token's `stores` claim would each need a
  relation traversal per candidate row instead of one scalar-filtered query.
  `@unique` has no cross-relation form either, so `(user, store)` uniqueness
  on `Grant` is enforced in the service (check-then-create), not the schema.
- **`User.user_uuid` / `HostedStore.store_id`** are this service's or the
  Pimble server's own minted UUIDs — the JWT `sub` and the path/`store_id`
  clients see are always one of these, never RhypeDB's internal integer
  `id`, which never leaves this crate.
- **`/health`, `/.well-known/jwks.json`, and `/releases`** are mounted under
  `/api/v1` (`/api/v1/health`, etc.), matching the contract's own framing
  sentence ("serving under `/api/v1`") and `jkbase.toml`'s `"/api/*" = cloud`
  route (a bare `/.well-known/jwks.json` at the site root would not reach
  this service at all in production). This also makes `/.well-known/jwks.json`
  sit exactly where OIDC-style discovery would look for it relative to a
  locally-signed token's `iss` (`PIMBLE_CLOUD_PUBLIC_URL/api/v1`).
- **JWTs are hand-rolled**, not built with a JWT crate, in both modes:
  jkbase-Auth mode is a plain HTTP call with jkbase's own request/response
  shape, and local dev-mode signing is header+payload base64url JSON, signed
  with `ed25519-dalek` directly — there is no third shape a generic JWT
  library's claim struct would need to accommodate. The dev-mode
  `/.well-known/jwks.json` is a hand-rolled RFC 8037 OKP JWK for the same
  reason (`jsonwebtoken` has no JWK-serialization surface either).
- **Login is constant-time across "wrong password" and "unknown email"**:
  an unknown email still runs one argon2id verification, against a fixed
  decoy hash computed once, so both cases take the same time and return the
  same generic message.
- **The last-owner guard covers both `DELETE .../members/{user_id}` and
  `PUT .../members`**: removing a store's sole owner, or changing their role
  away from `owner`, is refused with the same 409 either way. The contract's
  endpoint table only spells this out for `DELETE`; `PUT` is guarded too so a
  store can never end up with zero owners by either path.
- **`POST /stores`'s response shape** isn't specified by the contract; it
  returns the same shape as a `GET /stores` row: `{ store_id, name, role,
  created_at }` (`role` is always `"owner"`).
