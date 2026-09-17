# pimble-cloud

The Pimble Cloud accounts service: users, sessions, hosted stores, grants,
invitations, and the JWTs a Pimble server verifies. See
`docs/CLOUD_CONTRACT.md` at the repo root for the full design (and
`docs/CRYPTO_CONTRACT.md` / `docs/SHARING_CONTRACT.md` for the encryption
and sharing phases); this file is how to run it.

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
Account recovery (`POST /recover`, `501` in the first cut of this phase) is
now real — see "Account recovery, password change, new recovery code"
below.

**Migration and legacy rows — read every new field as optional.** This
service has now shipped three schema generations (Phase 1's four fields;
Phase 1b's `verified`/`verify_token_hash`/`verify_expires_at`; Phase 2a's
nine key-material fields; Phase 2a-2's `recovery_token_hash`/
`recovery_token_expires_at`), and RhypeDB never backfills a field onto a row
that predates it — the field is simply absent from that row, forever, until
something writes it. A production database accumulates rows from every
generation. `db.rs`'s `user_from_object` reads **every** field added after
the original four (`get_string_opt`/`get_bool_opt`/`get_u32_opt`/
`get_datetime_ms_opt`) with a safe default on absence — `false`/`""`/`0`,
never an error — precisely because a live crash-loop
(`Error: 500 ... User.verified: expected a Bool, got None`, hit in
production after the Phase 2a-2 deploy) came from treating one of these as
required. The rule going forward: **a new `User` field is always read
optional**, and the startup cleanup (below) never deserializes a row through
`user_from_object` at all, so a future field doesn't risk a repeat.
`kdf_salt`'s absence is what marks a row keyless (`has_key_material`); the
two pre-Phase-2a smoke accounts and any phase-1-shape row are deleted by
this at every startup.

The rule is not about `User` alone. `HostedStore` has now gained two fields
after rows already existed — Phase 2a's `kind` and Phase 2b's `share` — and
`hosted_store_from_object` reads **both** optional, as the kind a row from
before `kind` really is (`"plain"`) and the share a row from before `share`
really is (`false`). `kind` was required until Phase 2b, which would have
failed `GET /stores` outright for anyone holding a store created in Phase 1;
that is the same shape of bug as the `User.verified` crash, found while
adding `share` beside it.

## Sharing (Phase 2b)

docs/SHARING_CONTRACT.md: a share is an ordinary vault store with
`share: true` and ordinary `(user, store, role)` grants — this service
learns nothing new about what is in one. What it does gain is a way to
name somebody who has no Pimble account yet:

1. **`POST /stores { name, kind, store_id?, share? }`** — `share: true`
   needs `kind: "vault"` (400 otherwise). Every `GET /stores` row now
   carries `share` and `shared_by` (an owner's email, filled in only when
   the caller is not an owner — `null` for one's own store).
2. **`GET /stores/{id}/members`** (any grant) returns
   `[{ user_id, email, role, status, has_key, public_keys }]`:
   - `status` is `"active"` (a `Grant`) or `"invited"` (an `Invitation`);
   - `has_key` is whether a `KeyGrant` exists for that member on that store
     — what the owner's key sweep looks for (docs/SHARING_CONTRACT.md,
     "Key sweep"). The desktop's third state, "waiting for the key", is an
     active member with `has_key: false`, not a status here;
   - `user_id` is `null` for an invitation, and `public_keys` is `null`
     unless the caller is an **owner** (a reader of a shared folder learns
     nothing about the other members' keys). Invitation rows are only
     listed for an owner at all.
3. **`PUT /stores/{id}/members { email, role }`** (owner) now does one of
   two things, and says which in `status`:
   - the address has a **verified account with key material** → a grant, as
     before, plus a "shared with you" mail; the answer is `status:
     "active"` with that account's `public_keys`, so the caller can wrap the
     store key to them without a second round trip;
   - otherwise (no account, unverified, or a legacy keyless row) → an
     `Invitation` is upserted, an invitation mail goes out, and the answer
     is `status: "invited"`. Role `owner` this way is refused (400): an
     owner can delete the store, which is not something to hand to an
     unproven address.
4. **`DELETE /stores/{id}/members/{user_id}`** — an **owner, or the member
   themself** (leaving a share needs nobody's permission). Deletes that
   user's `KeyGrant`s for the store along with the grant; the last-owner
   rule stands.
5. **`DELETE /stores/{id}/invitations/{email}`** (owner) withdraws an
   invitation — the address is URL-encoded in the path, and it is `200`
   whether or not there was one to withdraw.
6. **`GET /stores/{id}/keys`** additionally returns `signers: [{ user_id,
   email, public_signing_key }]`, the store's owners: a recipient is handed
   the key by the sharer, so "signed by me" is no longer the only signature
   worth accepting.
7. **`DELETE /stores/{id}`** also deletes the store's key grants and
   invitations, and for a `vault` store calls `deleteVaultStore` on the
   hosted Pimble server so the ciphertext goes too. That call failing is
   logged, never returned: the row is marked deleted either way and nothing
   can reach the store afterwards.

**Claiming.** An invitation becomes a grant the moment the address really
has an account: at `GET /verify` (the address becoming verified) and at
every `POST /login`. An existing grant wins — an owner who invited an
address and then added or re-roled the account directly is not overruled by
the older invitation — and the invitation is deleted either way, as it is
for a store that has since been deleted. Both call sites treat it as best
effort: a claim that fails is logged and retried at the next login rather
than failing the verification or the login itself.

**Mail and limits** (all in-memory and per-process, like the other
limiters here):

| Limit | Key | Beyond it |
| --- | --- | --- |
| one sharing mail per minute | `<store id>:<lowercased address>` | the grant or invitation still happens; no mail is sent |
| thirty new invitations per hour | the inviter's account | `429 rate_limited` |
| fifty members plus invitations | the store | `409 conflict` |

**Names and addresses in a mail are attacker-controlled.** A share's name is
typed by its owner and the inviter's address by whoever signed up, and both
go out in a message Pimble's own sending domain puts its name to. So
`mail::one_line` runs over each first — every `char::is_control` dropped (CR
and LF among them, which is what makes a subject header injectable),
whitespace runs collapsed, trimmed — the name is capped at 80 characters
with an ellipsis, and every interpolated value in an HTML body goes through
`escape_html` (`& < > " '`), the link's `href` and its visible text
included. `POST /stores` applies the same one-line rule to a name before
storing it and refuses one over **200 characters** with a 400 (there was no
limit at all before Phase 2b). `mail.rs`'s unit tests cover a name like
`<a href="…">click</a>\r\nBcc: x@y`.

Both mails (`mail::invitation_email`, `mail::shared_with_you_email`) say
plainly that the notes are end-to-end encrypted and that they open once the
sender's Pimble has been online to hand over the key. The invitation links
to `<PIMBLE_CLOUD_PUBLIC_URL>/app/signup?email=<urlencoded address>`; the
"shared with you" mail links to `<PIMBLE_CLOUD_PUBLIC_URL>/app/`. Without
`RESEND_API_KEY` both go to `LogMailer` like every other mail here.

## Account recovery, password change, new recovery code (Phase 2a-2)

The server holds neither the password KEK nor the recovery KEK, so recovery
can only ever replace what the recovery code unwraps client-side — the
account keys themselves never change, only what they're wrapped under.
Every one of these is 202-or-404-always, never a different code for a real
versus an unfamiliar account:

1. **`POST /api/v1/recover/start { email }`** — 202 always. For a real,
   verified account with key material, mails
   `<PIMBLE_CLOUD_PUBLIC_URL>/app/recover?token=...` (32 random bytes,
   stored hashed, 1 hour, one use) through the same `Mailer` `start_verification`
   uses, rate-limited to once a minute per address by its own limiter
   (`recovery_rate_limit` — a separate instance from `resend_rate_limit`,
   not shared: sharing it would leave a brand-new signup's address
   rate-limited here for a minute purely because signup itself just sent a
   verification email).
2. **`GET /api/v1/recover/{token}`** — auth is holding the token, nothing
   else. `{ email, recovery_salt, recovery_kdf: { m_cost, t_cost, p_cost },
   recovery_key_blob, public_keys }`; unknown, expired, or already-consumed
   is `404 { "error": "recovery_invalid" }` (one code for all three).
   `recovery_kdf` is always the fixed `pimble_crypto` constants — there's no
   per-user stored cost for recovery, only a salt.
3. **`POST /api/v1/recover/{token}/complete { auth_key, kdf, account_key_blob,
   recovery_salt, recovery_key_blob }`** — auth is the token. The client
   rotates *both* the password material and the recovery code in the same
   call (a fresh recovery code every time one gets used, so an old emailed
   link is never a standing risk); replaces both, consumes the token,
   deletes every session, answers `{ user }`, starts no new session. Public
   keys never change, so every store key envelope already wrapped to this
   account stays valid without re-sharing anything.
4. **`POST /api/v1/me/password { current_auth_key, auth_key, kdf,
   account_key_blob }`** (session) — verifies `current_auth_key` like login,
   replaces the password material, keeps every other session.
5. **`POST /api/v1/me/recovery-code { recovery_salt, recovery_key_blob }`**
   (session) — replaces the recovery material with a new code the client
   generated while already holding the keys.
6. **`POST /api/v1/recover/{token}/delete-account`** — auth is the token,
   for someone who no longer has the code. Deletes the user, its sessions,
   grants and key grants; a store it solely owned is marked `deleted` (the
   same soft-delete `DELETE /stores/{id}` uses) since nobody else could ever
   manage it once the account is gone — wiping the underlying vault data is
   a later phase.

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
# kind defaults to "plain", store_id is optional, share needs kind "vault"
curl -sb cookies.txt -X POST http://127.0.0.1:8080/api/v1/stores \
  -H 'Content-Type: application/json' -d '{"name": "My Notes", "kind": "vault"}'
curl -sb cookies.txt -X POST http://127.0.0.1:8080/api/v1/stores \
  -H 'Content-Type: application/json' -d '{"name": "Recipes", "kind": "vault", "share": true}'

# List my stores
# -> [{"store_id":"...","name":"Recipes","role":"editor","kind":"vault",
#      "created_at":"...","share":true,"shared_by":"ann@example.com"}]
curl -sb cookies.txt http://127.0.0.1:8080/api/v1/stores

# Add a member, or invite an address with no account yet — same call
# -> {"user_id":"...","email":"bob@example.com","role":"editor",
#     "status":"active","has_key":false,"public_keys":{"encryption":"...","signing":"..."}}
# -> {"user_id":null,"email":"new@example.com","role":"editor",
#     "status":"invited","has_key":false,"public_keys":null}
curl -sb cookies.txt -X PUT http://127.0.0.1:8080/api/v1/stores/<store-id>/members \
  -H 'Content-Type: application/json' -d '{"email": "bob@example.com", "role": "editor"}'

# The members list (invitations and public_keys only for an owner)
curl -sb cookies.txt http://127.0.0.1:8080/api/v1/stores/<store-id>/members

# Remove a member (an owner, or the member themself), or withdraw an
# invitation (an owner; the address is url-encoded, 200 even if there is none)
curl -sb cookies.txt -X DELETE http://127.0.0.1:8080/api/v1/stores/<store-id>/members/<user-id>
curl -sb cookies.txt -X DELETE http://127.0.0.1:8080/api/v1/stores/<store-id>/invitations/new%40example.com

# My own key envelopes for a store, plus the owners whose signatures on them
# are legitimate, and setting one (envelope from pimble_crypto::wrap_key,
# signed by the caller's own signing key)
# -> {"envelopes":[{"key_id":"...","envelope":{...}}],
#     "signers":[{"user_id":"...","email":"ann@example.com","public_signing_key":"..."}]}
curl -sb cookies.txt http://127.0.0.1:8080/api/v1/stores/<store-id>/keys
curl -sb cookies.txt -X PUT http://127.0.0.1:8080/api/v1/stores/<store-id>/keys \
  -H 'Content-Type: application/json' \
  -d '{"envelopes": [{"user_id": "<my user id>", "key_id": "<uuid>", "envelope": { "...": "a pimble_crypto::KeyEnvelope" }}]}'

# Recovery: start (202 always), fetch what the token names, then complete
# with rotated material (both blobs come from pimble_crypto, same as signup)
curl -s -X POST http://127.0.0.1:8080/api/v1/recover/start -H 'Content-Type: application/json' -d '{"email": "alice@example.com"}'
curl -s http://127.0.0.1:8080/api/v1/recover/<token>
curl -si -X POST http://127.0.0.1:8080/api/v1/recover/<token>/complete \
  -H 'Content-Type: application/json' \
  -d '{"auth_key": "<new>", "kdf": {"...": "new KdfParams"}, "account_key_blob": {"...": "rewrapped"}, "recovery_salt": "<new>", "recovery_key_blob": {"...": "rewrapped under a new code"}}'
# ...or, without the code at all:
curl -s -X POST http://127.0.0.1:8080/api/v1/recover/<token>/delete-account

# Change password (session) / generate a new recovery code (session)
curl -sb cookies.txt -X POST http://127.0.0.1:8080/api/v1/me/password \
  -H 'Content-Type: application/json' -d '{"current_auth_key": "<old>", "auth_key": "<new>", "kdf": {"...": "new KdfParams"}, "account_key_blob": {"...": "rewrapped"}}'
curl -sb cookies.txt -X POST http://127.0.0.1:8080/api/v1/me/recovery-code \
  -H 'Content-Type: application/json' -d '{"recovery_salt": "<new>", "recovery_key_blob": {"...": "rewrapped under the new code"}}'

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
- **Envelope signature verification calls `pimble_crypto::verify_envelope`**
  (a tiny wrapper in `routes/stores.rs`, mapping `CryptoError::BadSignature`
  to 401 and anything else to 400) — it needs no private key, unlike
  `unwrap_key`, so the server (which only ever holds public keys) can
  authenticate an envelope it relays without unwrapping it. An earlier,
  short-lived version of this crate reimplemented the signing-bytes layout
  itself in a since-deleted `src/envelope.rs`, before `pimble-crypto`
  exported this helper; nothing calls that layout by hand any more.
- **`recovery_rate_limit` is a separate `RateLimiter` instance from
  `resend_rate_limit`**, not the same bucket, even though both are "one per
  minute per address": a fresh signup already records a use of
  `resend_rate_limit` for its verification email, so sharing it would leave
  `POST /recover/start` rate-limited for that same address for a minute
  before anyone ever called it — caught by every recovery-flow test
  failing with a stale (verification, not recovery) link in `LogMailer`.
- **`RhypeDb::complete_recovery` is one `.update()` call**, not three,
  covering the password material, the recovery material, and clearing
  `recovery_token_hash` together — so there's no window where a crash
  between separate updates leaves a token half-consumed or a row with new
  recovery material still holding the old password (or vice versa).
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
- **`Invitation` carries `store`/`store_rid`/`store_uuid` like `Grant`**, for
  the same reason (no relation-equality filter), and `(store, email_lower)`
  uniqueness is likewise enforced in the service (find-then-create/update in
  `routes/stores.rs::invite_member`). `invited_by_rid` is a scalar with *no*
  matching relation on purpose: the inviter is only ever read back to name
  them in the mail, and an invitation whose inviter has since deleted their
  account is still a perfectly good invitation.
- **`QuotaLimiter` (`src/ratelimit.rs`) is a second limiter**, not a
  parameterisation of `RateLimiter`: the existing one allows exactly one use
  per interval, and thirty invitations in a row is the ordinary way somebody
  shares a folder with their family. It keeps the instants still inside the
  window per key, which is bounded by the limit (a key at its limit records
  nothing further).
- **`Config::max_members_per_store` is not an environment variable.** The
  contract fixes the number at fifty and an operator raising it would quietly
  change what the service promises; it is a field rather than a constant only
  so a test can lower it (`spawn_stack_with_members_cap`) instead of making
  fifty accounts.
- **A sharing mail that fails to send is logged, not returned**, unlike
  signup's (which is a 502 — an account nobody can verify is unusable). By
  the time the mail goes out the grant or invitation is already committed, so
  a 502 would show the owner an error for a share that really was created,
  and the recipient finds it in their own store list regardless. The mail is
  a courtesy, not the mechanism.
- **`PUT members` treats "verified with key material" as "has an account"**.
  An unverified or legacy row is invited instead of granted — the same test
  `users_lookup` applies — so the claim path picks it up when that address
  really becomes usable, rather than leaving a grant nobody can ever hold the
  key for.
- **`LogMailer::message_count`** joins `last_message` as a test-only
  accessor: two sharing mails a minute apart have identical bodies, so
  "was a second one sent?" — what the per-(store, address) rate-limit test
  asks — cannot be read off the last body alone.
- **`NewUserKeyMaterial::placeholder_for_tests`** exists because every
  `User` field is now schema-required; a test that needs a user row to
  exist without exercising any crypto endpoint (mail/rate-limit tests that
  call `db.create_user` directly) uses it rather than inventing its own
  dummy strings inline.
