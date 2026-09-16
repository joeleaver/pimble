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
| `RESEND_API_KEY` | Resend API key; unset means `LogMailer` (verification links are logged at `info`, not emailed) | unset |
| `PIMBLE_MAIL_FROM` | the `from` address on every mail this service sends | `Pimble <no-reply@m.pimble.app>` |
| `PIMBLE_CLOUD_KDF_DECOY_SECRET` | the HMAC key `GET /kdf` derives an unknown email's decoy salt from | random (logs a warning; still deterministic within one process's lifetime) |

`PIMBLE_CLOUD_PUBLIC_URL`'s scheme decides two things: whether the session
cookie is marked `Secure`, and whether `/token`'s `rpc_url` is `ws://` or
`wss://`. `rpc_url` is `<PIMBLE_CLOUD_PUBLIC_URL scheme-swapped>/rpc` — the
same origin the accounts API is served from, not `PIMBLE_SERVER_URL` (in
production, jkbase's edge proxies both to their respective internal
services on the same VM; see `docs/CLOUD_CONTRACT.md` "Layout on jkbase").
Locally there is no such proxy, so `rpc_url` from a local `pimble-cloud`
won't resolve on its own — connect a `PimbleClient` straight to
`PIMBLE_SERVER_URL` with the minted `token` for manual testing instead.

## End-to-end encryption (Phase 2a)

docs/CRYPTO_CONTRACT.md: the server never sees a password or a plaintext
key. A real client (the web app, or the desktop app signing in) uses
`pimble-crypto` to do all of this:

1. **`GET /api/v1/kdf?email=`** returns `{ salt, m_cost, t_cost, p_cost }` —
   a real user's stored Argon2id parameters, or (same shape, so a passive
   observer can't tell) a deterministic decoy derived by HMAC-SHA256 of the
   lowercased email under `PIMBLE_CLOUD_KDF_DECOY_SECRET` for an unknown one.
2. The client derives `auth_key`/`kek` from the password and those params
   (`pimble_crypto::derive_password_keys`), generates an account X25519/Ed25519
   keypair, wraps it under `kek`, generates a recovery code and wraps the
   same keypair under a KEK derived from it too.
3. **`POST /api/v1/signup`** carries `auth_key` (base64url; hashed again with
   Argon2id at rest, exactly like a password before this phase), `kdf`,
   `public_keys: { encryption, signing }`, `account_key_blob`,
   `recovery_salt`, `recovery_key_blob` (the last two blobs are
   `pimble_crypto::AccountKeyBlob`, stored as opaque JSON strings). Everything
   else about signup (202, no session, verification email, duplicate/resend
   rules) is unchanged from Phase 1b, below.
4. **`POST /api/v1/login { email, auth_key }`** — same otherwise.
5. **`GET /api/v1/me/keys`** (session): `{ public_keys, kdf, account_key_blob }` —
   never `recovery_salt`/`recovery_key_blob`. The client unwraps
   `account_key_blob` locally with the password-derived `kek`.
6. **`GET /api/v1/users/lookup?email=`** (session, rate limited — see the
   design note below): `{ id, public_keys }` for a verified user, `404`
   otherwise (unknown address or real-but-unverified, same shape either
   way) — what a sharer uses to wrap a store key to someone.
7. **`POST /api/v1/stores { name, kind?, store_id? }`**: `kind` is `"plain"`
   (default) or `"vault"`; `store_id`, when given, asks the Pimble server to
   create the hosted store under that exact id (refused if already open
   there) — how a desktop app hosts an existing local store's twin. The
   response (and every `GET /stores` row) now also carries `kind`.
8. **`GET`/`PUT /api/v1/stores/{id}/keys`**: `GET` (any grant) returns the
   caller's own `{ envelopes: [{ key_id, envelope }] }` for that store, never
   another member's. `PUT { envelopes: [{ user_id, key_id, envelope }] }`
   upserts one or more; an owner or editor may always set their own, only an
   owner may set someone else's; every envelope's Ed25519 signature is
   verified against the *caller's* `public_signing_key` (whoever is doing the
   `PUT` must be the one who signed it) before it's stored.
9. **`POST /api/v1/recover { email, recovery_code_auth }`** is `501` — Phase
   2a only stores the recovery blob at signup; using it to actually recover
   is a later phase.

**Migration**: `User` gained nine required fields (`kdf_*`,
`public_*_key`, `account_key_blob`, `recovery_salt`, `recovery_key_blob`) with
no default — a pre-Phase-2a row (the two keyless smoke accounts from
initial testing) cannot satisfy the new schema and is orphaned by this
change. They were never real accounts; deleting them (or leaving them
orphaned — nothing reads a `User` row that doesn't have this crate's full
current field set) is the intended cleanup, not a bug.

## Email verification

An account can't log in until its email address is verified (docs/
CLOUD_CONTRACT.md, "Phase 1b: email verification"; the redirects below now
target the web app per docs/CRYPTO_CONTRACT.md — "the web app owns the
account pages now"). The flow:

1. `POST /signup` creates the user **unverified**, sends a mail with a
   `<PIMBLE_CLOUD_PUBLIC_URL>/api/v1/verify?token=...` link (24h expiry), and
   answers `202 { "status": "verification_sent", "email": "..." }` — no
   session, no token. Signing up again for the same still-unverified address
   just re-sends the link (also 202); a verified duplicate is `409`.
2. `GET /api/v1/verify?token=...` (what the mail links to; a browser follows
   it) marks the account verified and redirects (`303`) to
   `/app/login?verified=1`, or to `/app/login?verify_error=invalid` /
   `...=expired` for a bad token.
3. `POST /api/v1/resend-verification { "email" }` always answers `202`; it
   re-sends only for a real, still-unverified address, and only once per
   minute per address (silently ignored otherwise — still `202`) — a send
   from `POST /signup` counts too, so a resend moments after signing up is
   also silent.
4. `POST /login` for an unverified account is `403 { "error":
   "email_unverified", "message": "..." }`, checked after the password (so a
   wrong password is still a plain `401`, never a hint that the email
   exists).

**Sending mail**: `RESEND_API_KEY` unset (the default, and every local run
and test in this repo) means `LogMailer` — it logs the verification link at
`info` instead of emailing it. The service defaults its own log level to
`info` when `RUST_LOG` isn't set (so the link is visible out of the box, not
just when you remember to ask for it), and `RUST_LOG` still overrides that
default as usual:

```bash
cargo run --release -p pimble-cloud   # ... and copy the logged link out of the terminal
# or, to be explicit (or to change the level):
RUST_LOG=pimble_cloud=info cargo run --release -p pimble-cloud
```

**Testing Resend for real**: set `RESEND_API_KEY` to a real key (Joe's
`m.pimble.app` sending domain is already configured on Resend) and restart
`pimble-cloud`; `POST /signup` then sends a real mail via
`https://api.resend.com/emails` from `PIMBLE_MAIL_FROM` (default `Pimble
<no-reply@m.pimble.app>`), and every call above behaves identically — the
only difference is where the link goes. There's nothing else to flip: the
mailer is chosen once at startup from whether the key is set.

## Example requests

Signup's real body needs real `pimble-crypto` output (a KDF salt, a wrapped
key blob, …) that isn't something to type by hand — `tests/integration.rs`'s
`build_signup_body` builds one exactly as a real client would and is the
easiest way to see one on the wire (run a test with `--nocapture` and a
`reqwest` tracing filter, or read the helper itself). The shape:

```bash
# GET /kdf first (a real client always does, for both signup and login)
curl -s 'http://127.0.0.1:8080/api/v1/kdf?email=alice@example.com'
# -> {"salt":"<base64url>","m_cost":32768,"t_cost":3,"p_cost":1} (real or decoy)

# Sign up: 202, no session — check the LogMailer log (or your inbox with a
# real RESEND_API_KEY) for the verification link, then follow it in a
# browser (or curl -i, to read the Location header) before logging in.
# auth_key/kdf/public_keys/account_key_blob/recovery_salt/recovery_key_blob
# all come from pimble_crypto — the values below are illustrative shapes.
curl -si -X POST http://127.0.0.1:8080/api/v1/signup \
  -H 'Content-Type: application/json' \
  -d '{
    "email": "alice@example.com",
    "auth_key": "<base64url, pimble_crypto::encode_auth_key>",
    "kdf": {"salt": "<base64url>", "m_cost": 32768, "t_cost": 3, "p_cost": 1},
    "public_keys": {"encryption": "<base64url>", "signing": "<base64url>"},
    "account_key_blob": {"v": 1, "nonce": "<base64url>", "ciphertext": "<base64url>"},
    "recovery_salt": "<base64url>",
    "recovery_key_blob": {"v": 1, "nonce": "<base64url>", "ciphertext": "<base64url>"}
  }'

# Resend the verification link (202 either way; a no-op for an unknown or
# already-verified address, and rate-limited to once per minute)
curl -s -X POST http://127.0.0.1:8080/api/v1/resend-verification \
  -H 'Content-Type: application/json' -d '{"email": "alice@example.com"}'

# Log in (also 403 email_unverified until the link above has been followed)
curl -sc cookies.txt -X POST http://127.0.0.1:8080/api/v1/login \
  -H 'Content-Type: application/json' \
  -d '{"email": "alice@example.com", "auth_key": "<base64url, from the real kdf params above>"}'

# Who am I / my keys (never the recovery blob)
curl -sb cookies.txt http://127.0.0.1:8080/api/v1/me
curl -sb cookies.txt http://127.0.0.1:8080/api/v1/me/keys

# Look someone up by email (session required, rate limited) — what a
# sharer uses to get a recipient's public keys before wrapping a key to them
curl -sb cookies.txt 'http://127.0.0.1:8080/api/v1/users/lookup?email=bob@example.com'

# Create a hosted store (creates it on the Pimble server + an owner grant);
# kind defaults to "plain", store_id is optional
curl -sb cookies.txt -X POST http://127.0.0.1:8080/api/v1/stores \
  -H 'Content-Type: application/json' -d '{"name": "My Notes", "kind": "vault"}'

# List my stores
curl -sb cookies.txt http://127.0.0.1:8080/api/v1/stores

# Add a member
curl -sb cookies.txt -X PUT http://127.0.0.1:8080/api/v1/stores/<store-id>/members \
  -H 'Content-Type: application/json' -d '{"email": "bob@example.com", "role": "editor"}'

# My own key envelopes for a store, and setting one (envelope from
# pimble_crypto::wrap_key, signed by the caller's own signing key)
curl -sb cookies.txt http://127.0.0.1:8080/api/v1/stores/<store-id>/keys
curl -sb cookies.txt -X PUT http://127.0.0.1:8080/api/v1/stores/<store-id>/keys \
  -H 'Content-Type: application/json' \
  -d '{"envelopes": [{"user_id": "<my user id>", "key_id": "<uuid>", "envelope": { "...": "a pimble_crypto::KeyEnvelope" }}]}'

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
- **`build_router` is now `build_state` + `router_from_state`** (`build_router`
  still exists and just composes the two). Tests need the `AppState` itself,
  not just the `Router` built from it, to reach the `LogMailer` through
  `AppState::mailer` (`Mailer::as_log_mailer`) — e.g. to pull the verify link
  a test's signup triggered back out of memory. `main.rs` is unchanged; it
  still just calls `build_router`.
- **`verify_token_hash` uses an empty string, not `Option`, as its "no
  current token" sentinel** (set once at verification, matching the schema's
  other required-`String` fields, none of which are nullable). Nothing ever
  queries that sentinel: an empty `token` query parameter on `GET /verify` is
  rejected before it would be hashed and looked up, so a stale or verified
  user's `""` can never be matched by an incoming request.
- **The resend rate limiter (`src/ratelimit.rs`) is in-memory, per-process**,
  keyed by lowercased email, same tradeoff as the releases/JWKS caches
  already in this crate — a restart only ever makes it more permissive.
- **`ResendMailer`/`LogMailer` share one `Mailer` trait** rather than an `if
  let Some(key) = ...` scattered through the handlers; `routes/accounts.rs`
  calls `state.mailer.send(...)` without knowing which one is behind it.
  `LogMailer::last_message` plus `mail::first_url` are the "test-only
  accessor" the contract asks for — `tests/integration.rs`'s
  `extract_verify_token` is the one place that calls them.
- **`PimbleService` (`src/pimble.rs`) talks to the Pimble server over a raw
  jsonrpsee connection** (`pimble_rpc::PimbleApiClient`, generated in
  `pimble-rpc`), not through `pimble_client::PimbleClient::create_store`.
  That wrapper still hardcodes `kind: Default::default(), store_id: None`
  and `pimble-client/src/client.rs` had a concurrent editor (agent A/B's
  vault-RPC wrappers) while this crate's Phase 2a work landed — extending it
  risked clobbering in-flight, out-of-scope work. `pimble-client` is still a
  dependency (used by `tests/integration.rs` and `src/error.rs`'s `From`
  impl); only the one `createStore` call bypasses it. Once
  `PimbleClient::create_store` grows `kind`/`store_id` parameters, `pimble.rs`
  should switch back to it and this crate can drop its own `pimble-rpc`/
  `jsonrpsee` dependencies.
- **Envelope signature verification (`src/envelope.rs`) reimplements
  `pimble-crypto`'s private `envelope_signing_bytes`** rather than calling
  `pimble_crypto::unwrap_key`: that function needs the *recipient's* private
  `AccountKeys` to unwrap the key afterwards, which the server never has,
  and the signing-bytes helper itself isn't `pub`. The exact layout (`v ||
  key_id || recipient || ephemeral || nonce || ciphertext || context`, with
  `recipient`/`ephemeral`/`nonce`/`ciphertext` as their **decoded raw
  bytes**, not the base64url string) was confirmed by reading
  `wrap_key`/`unwrap_key` in `pimble-crypto/src/lib.rs` directly, not
  guessed from the `KeyEnvelope` doc comment alone — the doc comment doesn't
  say whether those fields are raw or encoded, and getting it wrong would
  make every real envelope fail verification here. If `pimble-crypto` ever
  exports that helper, `src/envelope.rs` should call it instead.
- **`users_lookup_rate_limit`'s interval (200ms, in `src/state.rs`) is a
  judgment call**, not a number the contract gives: it only says "rate
  limited". Chosen to survive ordinary UI use (typing an email into a share
  dialog) while still blocking a scripted enumeration loop.
- **`KeyGrant` mirrors `Grant`'s denormalization** (`user_rid`/`store_rid`/
  `store_uuid` scalar copies alongside the `user`/`store` relations) for the
  same reason: no relation-equality filter in the query language.
  `(user, store, key_id)` uniqueness is enforced in the service
  (find-then-create/update in `routes/stores.rs::put_store_keys`), not the
  schema — key rotation adds a new `key_id` and new envelopes per member
  rather than mutating an old row, so old blobs keep decrypting.
- **`NewUserKeyMaterial::placeholder_for_tests`** exists because every
  `User` field is now schema-required; a test that needs a user row to
  exist without exercising any crypto endpoint (mail/rate-limit tests that
  call `db.create_user` directly) uses it rather than inventing its own
  dummy strings inline.
