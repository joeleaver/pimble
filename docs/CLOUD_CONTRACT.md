# Cloud contract, phase 1: accounts, hosted server, web app, website

Status: code done 2026-09-15 (branch `cloud/phase-1`), not yet deployed. Implemented as
written, with these decisions made during the work: `listRemoteStores` is `Service`-only
like its siblings; `createMount` and `getMountState` authorize against the mount's source
store as `getChildren` does; the accounts service refuses to demote the last owner as well
as to remove them; `/.well-known/jwks.json`, `/releases` and `/health` live under `/api/v1`
so the edge routes reach them; the accounts schema carries denormalized scalar ids because
RhypeDB's query language has no relation-equality filter; the jkbase project id is
`pimble` and the public origin is `https://pimble.app` (custom domain, verified, with
certificates). Decided with Joe the same day: everything server-side runs on jkbase
(`~/dev/jkbase`, Joe's own platform, live at jkbase.app).

## Goal

A person can go to the Pimble website, download the Linux or Windows build, or sign up for
an account and open the web app in the browser. A signed-in user has hosted stores on a
Pimble server that runs on jkbase; the web app edits them live through the existing
JSON-RPC protocol; the desktop app can add them as replicas (phase 2 makes that a one-click
sign-in). Accounts, store ownership and sharing live in one small service in front of a
managed RhypeDB. Later phases (relay of a local store, the "teams" product where the server
is the source of truth with organisations, permissions and binary files) build on the same
identity and grant model, so that model is designed here and nothing in phase 1 may assume a
user has exactly one role or that stores have exactly one owner forever.

## What jkbase provides (verified in its README and source on 2026-09-15)

- A project is one microVM. Static sites, a trunk-built Rust/WASM site, and source-built
  Rust servers all live in it; servers listen on loopback ports and the edge proxy routes
  paths to them, **including WebSocket upgrades** (`jkbase-wsproxy`). TLS is the edge's job.
- A managed RhypeDB on loopback `127.0.0.1:4201` (native wire, `rhypedb-client`) and
  `:4200` (HTTP), no credentials inside the VM. Schema is a `.rhype` SDL file named in
  `jkbase.toml`.
- jkbase-Auth: `POST https://auth.jkbase.app/v1/projects/<project-id>/token` with
  `Authorization: Bearer jkbk_…` and body `{ "sub": "...", "aud": "...", "ttl": 3600,
  "claims": { ... } }` returns `{ "token": "<EdDSA JWT>", "exp": ..., "kid": "..." }`.
  Registered claims are `iss` (`https://auth.jkbase.app/v1/projects/<project-id>`), `sub`,
  `aud`, `iat`, `exp`, `jti`; custom claims are nested under a top-level `claims` object.
  Public keys: `GET …/v1/projects/<project-id>/.well-known/jwks.json`. jkbase stores no
  end-user accounts; Pimble authenticates its own users.
- S3-compatible object storage (not used in phase 1).
- Secrets via `jkbase secret set NAME=value`, delivered as environment variables.

## Layout on jkbase (one project, `pimble`, one origin)

```
https://pimble.jkbase.app/          site/                static marketing site, downloads, login
https://pimble.jkbase.app/app/      web/                 trunk-built rinch web app (SPA)
https://pimble.jkbase.app/api/*     crates/pimble-cloud  accounts service (axum, port 8080)
https://pimble.jkbase.app/rpc       pimble-cli server    hosted Pimble server (port 7462)
```

One origin means no CORS anywhere. `jkbase.toml` at the repo root declares all four plus
`[database] schema = "crates/pimble-cloud/schema.rhype"`. The two servers share the VM, so
the accounts service reaches the Pimble server on `http://127.0.0.1:7462` with the static
service token, and RhypeDB on `127.0.0.1:4201`.

## Ownership

| Who | Scope (nobody edits another's files) |
| --- | --- |
| PM | this file, root `Cargo.toml`, `CLAUDE.md`, `docs/NEXT_SESSION.md`, merge and commits |
| A | `crates/pimble-client` (wasm transport), `crates/pimble-app` (library + desktop binary split), `web/` (new, its own workspace) |
| B | `crates/pimble-rpc` (error codes, `--stores-dir` types if any), `crates/pimble-server` (JWT auth, authorization, origin allowlist, query token, stores dir), `crates/pimble-cli` (flags and env), `tests/` |
| C | `crates/pimble-cloud` (new; the PM adds the workspace member and an empty stub first) |
| D | `site/`, `jkbase.toml`, `.github/workflows/`, `docs/DEPLOY.md` |

If an agent needs something outside its scope it says so in its report; the PM decides.
Agents write tests for their own scope and run `cargo check --workspace --all-targets`
before reporting; zero warnings is the standard. `cargo test --workspace --release` must
keep passing (195 tests today).

## Identity and grants (the model every phase uses)

- A **user** is `sub` = a UUID minted at signup, with an email and an argon2id password
  hash. Email is unique and case-insensitive.
- A **hosted store** is a Pimble store the hosted server owns on disk. Its `StoreId` is
  whatever the Pimble server returned from `createStore`; the accounts DB records it.
- A **grant** is `(user, store, role)` with role `owner | editor | reader`. A store has at
  least one owner. Owners manage grants; editors and readers do not. Phase 1 has no
  organisations; the grant table is where an organisation's membership will expand to.
- A **token** is a jkbase-Auth JWT (or, in development, one signed by the accounts service's
  own Ed25519 key). `aud` is `pimble`. Custom claims:

  ```json
  { "claims": { "email": "joe@example.com", "stores": { "<store-uuid>": "owner", "<store-uuid>": "reader" } } }
  ```

  Tokens live one hour. Grant changes take effect at the next token, which is fine for
  phase 1 (a revoked user keeps a live connection for at most an hour; owners see that in
  the members list). Pimble servers therefore hold **no account state**: they verify a
  signature against a JWKS and read the grants from the token.
- A **service principal** is the hosted server's static token (existing `--token-file`
  mechanism). It may do everything, and only the accounts service holds it.

## B: pimble-server

1. **Two credential verifiers, both optional, both may be on.** `AuthLayer` keeps the static
   token. A new JWT mode is configured by `--jwks URL --issuer ISS` (env
   `PIMBLE_JWKS_URL`, `PIMBLE_JWT_ISSUER`; audience is always `pimble`). The JWKS is fetched
   at start and refreshed in the background (every 10 minutes and on an unknown `kid`, at
   most once a minute). EdDSA (Ed25519) only. Verify `iss`, `aud`, `exp` with 60 s skew.
   A server with neither verifier refuses to bind beyond loopback, as today.
2. **Credential carriers.** `Authorization: Bearer`, `X-Api-Key` (both as today) and, new,
   an `access_token` query parameter on the request URL. The third exists because a browser
   `WebSocket` cannot set headers. Any of the three may carry either the static token or a
   JWT; the static token is checked first (constant time), then the JWT.
3. **Origin allowlist.** `--allow-origin ORIGIN` (repeatable; env `PIMBLE_ALLOW_ORIGINS`,
   comma separated). A request whose `Origin` is in the list passes the origin check; any
   other `Origin` is 403 as today; no `Origin` passes as today. The app's embedded server
   sets nothing and keeps refusing every origin.
4. **A principal per connection.** The verified identity (`Principal::Service` or
   `Principal::User { sub, email, grants: HashMap<StoreId, Role> }`) is attached to the HTTP
   request's extensions in the auth layer and reaches every RPC through jsonrpsee 0.24's
   request `Extensions` (RPC middleware or the `#[method(with_extensions)]` attribute; B
   picks and documents the mechanism). A connection that arrived with no verifier configured
   (loopback, tokenless) is `Principal::Service`.
5. **Authorization in the handler**, one function `authorize(principal, store_id, needed)`
   called at the top of every RPC that names a store, where `needed` is `Read` or `Write`:
   `reader` may read (get*, list*, search, subscribe*, sync* in the pull direction);
   `editor` and `owner` may also write (applyEdit, applyStoreUpdate, createNode, rename,
   move, delete, updateNodeMetadata, updateNodeContent, createMount, rebuildIndex);
   `Service` may do everything including `createStore`, `openStore`, `closeStore`,
   `addRemoteStore`, `setStoreSync`, `removeReplica`, which a user principal may never call.
   `listStores` returns only the stores the principal may read. A mount's `getChildren`
   authorizes against the **source** store as well. Denied calls return JSON-RPC error
   code `-32004` (`forbidden`); B adds it beside the existing codes in `pimble-rpc`.
   A user principal reaching `syncNodeContent`/`syncStoreDocument` is a reader operation;
   the push direction is `applyEdit`/`applyStoreUpdate`, already write.
6. **Stores directory.** `pimble-cli server --stores-dir DIR` (env `PIMBLE_STORES_DIR`):
   at start the server opens every `*.pimble` directly inside `DIR`; `createStore` with a
   path inside `DIR` is what the accounts service uses. Nothing else changes about paths.
7. **Env for every server flag** so jkbase secrets configure it: `PIMBLE_ADDR`,
   `PIMBLE_TOKEN` (the static token itself; `--token-file` still works), `PIMBLE_JWKS_URL`,
   `PIMBLE_JWT_ISSUER`, `PIMBLE_ALLOW_ORIGINS`, `PIMBLE_STORES_DIR`. Flags win over env.
   Note `PIMBLE_SERVER`/`PIMBLE_TOKEN` are already the CLI's *client* env; B keeps the
   client meaning of `PIMBLE_TOKEN` and names the server one `PIMBLE_SERVER_TOKEN`.
8. **Tests** (`tests/auth.rs` or in-crate): a server in JWT mode with a JWKS served from a
   local axum or hyper stub and tokens signed in the test with `ed25519-dalek`; reader can
   read and not write; editor can write; unknown store forbidden; expired token 401;
   `access_token` query on the WebSocket URL works; origin allowlist admits one origin and
   refuses another; static token still works alongside; `listStores` filtering;
   `--stores-dir` opens what is there.

## C: pimble-cloud (accounts service)

A single axum binary, `pimble-cloud`, port from `PORT` (default 8080), serving under
`/api/v1`. Dependencies go in its own `Cargo.toml` (axum 0.7, tokio, `rhypedb-client` from
`git = "https://github.com/joeleaver/rhypedb.git", branch = "master"` with its `async`
feature, argon2, ed25519-dalek, jsonwebtoken or hand-rolled EdDSA, reqwest with rustls,
`pimble-client` for the Pimble server). Environment:

| Variable | Meaning |
| --- | --- |
| `RHYPEDB_ADDR` | default `127.0.0.1:4201` |
| `PIMBLE_SERVER_URL` | default `http://127.0.0.1:7462` |
| `PIMBLE_SERVER_TOKEN` | the hosted server's static token (service principal) |
| `PIMBLE_STORES_DIR` | where to ask the server to create stores (default `/app/data/stores`) |
| `JKBASE_AUTH_ISSUER_URL` | `https://auth.jkbase.app/v1/projects/<project-id>`; unset means development signing |
| `JKBASE_AUTH_KEY` | the `jkbk_…` issuer key |
| `PIMBLE_CLOUD_DEV_SIGNING_SEED` | 32 bytes hex; with no issuer URL, tokens are signed locally with this key and `iss` is `PIMBLE_CLOUD_PUBLIC_URL/api/v1` |
| `PIMBLE_CLOUD_PUBLIC_URL` | `https://pimble.jkbase.app` (cookie `Secure` when https) |
| `GITHUB_REPO` | `joeleaver/pimble`, for `/releases` |

Schema `crates/pimble-cloud/schema.rhype` (RhypeDB SDL; C reads `~/dev/rhypedb/examples/*.rhype`
and the schema docs for syntax): `User { email, email_lower @unique, password_hash,
created_at }`, `Session { token_hash @unique, user -> User, expires_at, created_at }`,
`HostedStore { store_id @unique, name, dir_name, created_at, deleted: bool }`,
`Grant { user -> User, store -> HostedStore, role }` unique per (user, store).

Endpoints (JSON in and out; errors as `{ "error": "<code>", "message": "..." }` with
sensible statuses):

| Method and path | Auth | Behaviour |
| --- | --- | --- |
| `POST /signup` `{email, password}` | none | 8+ char password; creates user; starts a session; same response as login |
| `POST /login` `{email, password}` | none | sets `pimble_session` cookie (HttpOnly, SameSite=Lax, Secure in prod, 30 days) and returns `{ user: {id, email}, session: "<opaque>", token, exp }`. Constant-time on unknown email (hash anyway) |
| `POST /logout` | session | deletes the session |
| `GET /me` | session | `{ id, email }` |
| `POST /token` | session | mints a fresh JWT with the current grants: `{ token, exp, rpc_url }` where `rpc_url` is `wss://<host>/rpc` |
| `GET /stores` | session | `[ { store_id, name, role, created_at } ]` |
| `POST /stores` `{name}` | session | creates the store on the Pimble server (`createStore` with path `<PIMBLE_STORES_DIR>/<new uuid>.pimble`), records it and an owner grant |
| `DELETE /stores/{id}` | owner | marks `deleted`, removes grants; the server-side directory stays (deletion on disk is a later phase) |
| `GET /stores/{id}/members` | any grant | `[ { user_id, email, role } ]` |
| `PUT /stores/{id}/members` `{email, role}` | owner | adds or changes a grant; unknown email is 404 (no invitations in phase 1) |
| `DELETE /stores/{id}/members/{user_id}` | owner | removes; refuses to remove the last owner |
| `GET /releases` | none | latest GitHub release: `{ version, published_at, assets: [ { name, os, url, size } ] }`, cached 10 min; `os` inferred from the asset name (`linux`, `windows`) |
| `GET /.well-known/jwks.json` | none | production: jkbase's JWKS fetched and cached 10 min; development: the local key |
| `GET /health` | none | `ok` |

Session auth accepts the cookie or `Authorization: Bearer <session>` (the desktop app in
phase 2 stores the opaque session and uses the header). Sessions and tokens are never
logged. Tests run against a real RhypeDB: C checks whether `rhypedb-server` exposes a
library entry point to start one on an ephemeral port inside the test; if not, tests spawn
the `rhypedb` binary when present and skip cleanly when absent, and C says which in the
report. The Pimble server in tests is `pimble_server::PimbleServer` on loopback with a
token. A `README.md` in the crate documents running it locally against a local RhypeDB.

## A: client for wasm, app split, web app

1. **`pimble-client` on wasm32.** Under `cfg(target_arch = "wasm32")` the transport is
   jsonrpsee's `wasm-client`; native keeps `ws-client`. Same `PimbleClient` API. The
   connect call takes the URL with the credential already in it as `access_token` (the
   browser cannot set headers); native callers keep the header. Subscriptions and
   `on_disconnect` work on both.
2. **`pimble-app` becomes a library plus the desktop binary.** `lib.rs` exports the UI
   (`app`, state, events, editor, toolbar, appearance, styles) and the backend command and
   event types. The native backend thread (`backend.rs`, the embedded `PimbleServer`,
   tokio runtime, `dirs`, `persistence.rs`) sits behind a `native` cargo feature, on by
   default; the `pimble` binary requires it. The UI talks to the backend only through
   `BackendCommand` in and `BackendEvent` out, so the web build supplies a different
   implementation of the same two channels. Whatever seam A introduces, the collaboration
   invariants in `CLAUDE.md` stand: one editor pane, one thread-local `EditorHandle`, edits
   travel as yrs bytes through `BroadcastChanges` and `RemoteChanges`, never a document
   model in the sync path.
3. **`web/` is its own cargo workspace** (rinch's guide, `docs/src/guide/wasm.md` in the
   rinch checkout; the working example is `examples/collab-editor-web`): crate
   `pimble-web`, `index.html` with the trunk link, `Trunk.toml` with `public_url = "/app/"`,
   `rinch` with `default-features = false`, `rinch-web` with `collaboration`, all rinch
   crates from GitHub `main` (never a path). It depends on `pimble-app` with
   `default-features = false`. Its backend: on start it `fetch`es `POST /api/v1/token`
   with the session cookie; a 401 redirects to `/login.html`; otherwise it connects
   `PimbleClient` to `rpc_url` with the token, then runs the same command loop as native
   on `wasm_bindgen_futures::spawn_local`, and refreshes the token (and reconnects if
   needed) five minutes before `exp`. `ListStores` on connect gives it the hosted stores it
   may see; there is no create/open store by path in the web app (the accounts service and
   the site do that). Editor: `rinch_web`'s `Editor`/`create_editor` in place of the
   desktop editor component, behind `cfg`, keeping `editor.rs`'s collab wiring.
4. **Out of scope for A:** any server change (B), the account pages (D), the desktop sign-in
   (phase 2). Where the web app needs a rinch fix, A reports it; rinch fixes go upstream.
5. **Verification:** `cargo build -p pimble-app --release` and the desktop app unchanged in
   behaviour; `cd web && trunk build --release` produces `dist/`; served locally (any
   static server for `dist/` plus a local `pimble-cloud` and `pimble-cli server` in JWT
   mode is the full stack; A documents the local recipe in `web/README.md`) the web app
   lists stores, opens a document, and two browser tabs see each other's edits.

## D: site, jkbase, CI

1. **`site/`** is static HTML, CSS and a little JavaScript, no framework, no build step.
   Pages: `index.html` (what Pimble is: offline-first, CRDT, mounts, search; a screenshot
   placeholder; "Download" and "Sign up"), `download.html` (fetches `/api/v1/releases`
   and shows the Linux and Windows assets; explains the Linux build is a plain binary),
   `signup.html`, `login.html` (post to the API, then go to `/app/`), `account.html`
   (stores list, create store, members per store with add and remove, log out).
   Follow `docs/STYLE_GUIDE.md`'s quiet tone; dark and light via `prefers-color-scheme`.
2. **`jkbase.toml`** at the repo root, project `pimble`: `[hosting] public = "site"`;
   `[sites.app] source = "web" context = "." build = "trunk" prefix = "/app" spa = true`;
   `[servers.cloud] source = "crates/pimble-cloud" context = "." port = 8080
   health_check = { path = "/api/v1/health" }`; `[servers.pimble] source = "crates/pimble-cli"
   context = "." port = 7462 command = [...] volumes = [{ name = "data", mount = "/app/data" }]`
   with the command running `server` with `--addr 127.0.0.1:7462 --stores-dir /app/data/stores`
   and the rest from env; `[database] schema = "crates/pimble-cloud/schema.rhype"`;
   `[routes] "/api/*" = cloud, "/rpc" = pimble`. D verifies every key against the
   `jkbase.toml` reference in `~/dev/jkbase/README.md` (read only; that repo is not edited)
   and checks how a Rust workspace crate's binary is selected by the rust buildpack
   (`~/dev/jkbase/crates/jkbuild/src/buildpacks/rust.rs`). The hosted server's ONNX
   download at build time may not be possible in the sealed build VM: D checks the
   buildpack's fetch phase and, if downloads are not allowed, the server is built with
   `--no-default-features` (keyword-only search) and D says so.
3. **`docs/DEPLOY.md`**: the secrets to set (`PIMBLE_SERVER_TOKEN`, `JKBASE_AUTH_ISSUER_URL`,
   `JKBASE_AUTH_KEY`, `PIMBLE_JWKS_URL`, `PIMBLE_JWT_ISSUER`, `PIMBLE_ALLOW_ORIGINS`,
   `PIMBLE_CLOUD_PUBLIC_URL`), the `jkbase auth key create` step, `jkbase deploy`, how to
   tail logs, and how to run the whole stack locally.
4. **`.github/workflows/release.yml`**: on a `v*` tag, build `pimble-app` in release for
   `ubuntu-22.04` and `windows-latest` (ONNX download feature on; the Windows job installs
   nothing exotic), package (`pimble-<version>-linux-x86_64.tar.gz`,
   `pimble-<version>-windows-x86_64.zip`), and attach to a GitHub release. Also
   `ci.yml`: `cargo check --workspace --all-targets` and `cargo test --workspace --release`
   on push. D cannot run the Windows job locally; D makes the Linux packaging step run
   locally as a script (`tools/package.sh`) and reports what is unverified.

## Phase 1b: email verification (added 2026-09-15 after the first deploy)

Joe's requirement: an account is not complete until its email address is verified by
clicking a link in a message. Sending goes through Resend from the domain `m.pimble.app`.

**Accounts service (C):**

- `User` gains `verified: Bool`, `verify_token_hash: String`, `verify_expires_at: DateTime`.
  Signup creates the user unverified, generates a 32-byte random token (stored hashed, 24 h
  expiry), sends the verification mail, and answers `202 { "status": "verification_sent",
  "email": "<as given>" }`. It no longer starts a session or returns a token. Signing up
  again with an unverified email re-sends the mail and answers 202 (no enumeration);
  a verified duplicate is 409 as before.
- `GET /api/v1/verify?token=<token>`: on success marks the user verified, clears the token,
  and answers `303 Location: /login.html?verified=1`; an unknown or expired token answers
  `303 Location: /login.html?verify_error=invalid` or `...=expired`. A browser follows it.
- `POST /api/v1/resend-verification { "email" }`: 202 always. Re-sends for an unverified
  account (new token), does nothing otherwise. Rate limit: one send per address per minute.
- `POST /api/v1/login` for an unverified account: `403 { "error": "email_unverified",
  "message": "Check your inbox for the verification link." }`, checked after the password.
- Mail: a `Mailer` trait with two implementations. `ResendMailer` posts to
  `https://api.resend.com/emails` with `Authorization: Bearer <RESEND_API_KEY>` and
  `{ "from", "to", "subject", "html", "text" }`; `LogMailer` (no key configured) logs the
  link at `info` and keeps the last message per address in memory so tests can read the
  link back through a test-only accessor. Env: `RESEND_API_KEY` (unset means `LogMailer`),
  `PIMBLE_MAIL_FROM` (default `Pimble <no-reply@m.pimble.app>`). The link is
  `<PIMBLE_CLOUD_PUBLIC_URL>/api/v1/verify?token=...`. Subject "Verify your Pimble
  account"; plain, short body, the link, and "if you did not sign up, ignore this".
- Tests: signup answers 202 and no session; login before verification is 403
  `email_unverified`; the link from `LogMailer` verifies and redirects; login then works;
  an expired token redirects with `expired`; resend issues a new token and invalidates the
  old; a second signup for an unverified address re-sends.

**Site (D):**

- `signup.html`: a "Confirm password" field; mismatch is a form error before any request;
  on 202 replace the form with "Check your inbox: we sent a verification link to
  <email>." plus a "Send it again" button calling `resend-verification`.
- `login.html`: banners for `?verified=1` ("Your email is verified. Log in."),
  `?verify_error=expired` ("That link has expired." with a resend form) and `invalid`;
  on `403 email_unverified` show the message and the resend button.
- The site's script is `site.js` (never a name starting with `app`: jkbase routes every
  path with the `/app` prefix, including `/app.js`, to the web app).

## Phase 2: sharing (decided with Joe 2026-09-15, not built)

Share a node **in place**: nothing in the owner's store moves or is restructured (Joe
rejected extracting a shared subtree into its own store as taking liberties with the
user's data). Roles for now: `reader` and `editor`; finer permissions later.

- **Grant** becomes `(user, store, root node, role)`; a whole-store grant is the root.
- **Content** replicates per node (each node is its own yrs document), so a recipient holds
  copies of exactly the nodes under the shared root: a **partial replica** with a root, a
  structure cache fed by the source's change notifications, content sync limited to nodes
  under the root, and tree edits forwarded as node-level RPCs. Recipients never sync the
  store document (a diff of it carries every title in the store).
- **Authorization**: every node-scoped RPC checks that the node is under a granted root
  (ancestor walk); `syncStoreDocument`/`applyStoreUpdate` are refused for subtree grants.
  Moving a node out of a shared subtree revokes access to it; moving one in shares it.
- **Recipient side** is a remote mount of the owner's node (exists today) backed by a
  partial replica instead of a whole-store one. The web app needs no replica.
- **Source, a per-store choice the owner makes (Joe, 2026-09-15: both are products):**
  *Hosted*: an encrypted copy on Pimble Cloud (whole-store replica upward); works while
  the owner is offline. *Relay*: Pimble Cloud stores **nothing**, not even ciphertext at
  rest; the owner's local server serves the store live through a reverse tunnel and
  recipients keep their cached partial replica; when the owner is offline recipients read
  their cache and their edits wait. The promise of the relay tier is "we never store your
  data", and the relay must stay stateless to keep it (no persistent queue). The same
  code encrypts on the way out and enforces access in whichever server serves the store.
- **Encryption (Joe, 2026-09-15: important, from day 1)**: node content and tree
  documents are encrypted client-side with per-store and per-share keys; the hosted server
  and the relay carry ciphertext only, as a blob log (append, fetch-since, snapshot) with
  a per-member node-id list, and clients do all merging. Details and costs in the
  discussion of 2026-09-15; a crypto contract (account keys at signup, wrapped keys,
  recovery code, blob-mode server) comes before any sharing work.
- **Identity**: both sides have accounts. The owner signs in on the desktop (needed to
  create hosted stores, mint the link's token, manage grants). An invitation is a pending
  grant keyed by email or an "anyone with the link" token at `https://pimble.app/s/<token>`;
  signup or login with that email claims it; a signed-in visitor lands in the web app on
  the shared node or is offered "Open in Pimble desktop".
- **Revocation** deletes the grant; effective at the next token (an hour) or at once via a
  Service-only RPC that drops a subject's live connections. Already-synced content stays
  with the recipient, as in any sharing system.
- **Consequences of encryption**: no server-side search of hosted stores (the desktop
  keeps its local index; the web app searches client-side over decrypted data); tree
  repair moves to clients; clients compact blob logs into snapshots; the web app's
  encryption protects against a breached or passive server, not a malicious one.
- **UI**: node context menu "Share..." (sign in first if needed); dialog with people by
  email plus role and an optional "anyone with the link" switch; a shared badge on the node;
  the same dialog manages members, roles, the link and "Stop sharing".

Build order: crypto foundation and account keys at signup; desktop sign-in
(`AuthMethod::CloudSession` so the sync link mints a fresh JWT before each connect);
hosted stores in blob mode with the encrypting sync link (one person, several devices, the
web app); subtree grants, invitation link and email; partial replicas, the share mirror
and the Share dialog; the relay tier; then teams (organisations above grants, server as
source of truth, binary files in object storage, store deletion on disk).
