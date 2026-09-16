# Crypto contract, phase 2a: end-to-end encryption for hosted stores

Status: code done 2026-09-16 (branch `cloud/phase-1`), verified end to end by the PM in a
browser through trunk's proxies against the real four-process stack; deployed to
pimble.app 2026-09-16 (rinch PR #791 merged the same day).
Decided with Joe 2026-09-15: encryption is important and comes first, before sharing.
Assumptions Joe has not overruled: local store files stay unencrypted on disk; signup
issues a recovery code. Decided 2026-09-16: the store display name stays plaintext (listed under visible
metadata). Decisions made during the work: the mode
field is `sync_mode` (`Store.kind` already means the store's own kind); `cloudAddHostedStore`
starts from an empty replica; the accounts service's `rpc_url` only resolves behind an
edge that routes `/rpc`, so the interop test runs a tiny proxy; `run_on_main_thread` from
the main thread runs synchronously, so "defer past the menu close" uses a zero timer.

## Goal

A person signs up, signs in on the desktop, and hosts a store on Pimble Cloud. Every byte of
node content and tree structure that leaves their machine is encrypted with keys only their
devices hold. Pimble Cloud stores ciphertext (hosted tier) or, later, relays it without
storing it (relay tier); it never holds a key. The web app decrypts in the browser after
the person unlocks their account with their password. A second device signs in, receives
the wrapped keys, and replicates the store. Sharing (phase 2b) reuses the same key
envelopes with per-share keys.

## Threat model, stated plainly

- Protects against: anyone reading the hosted server's disk or database, a passive or
  breached Pimble Cloud, and Pimble Cloud's own operator reading content.
- Does not protect against: a malicious Pimble Cloud serving a tampered web app to a
  browser (the desktop app is not subject to this), key substitution by a malicious server
  on first contact (member public keys are trusted on first use through the accounts
  service in this phase; envelopes are signed so later substitution is detectable), or a
  compromised device.
- Metadata visible to the server: account emails, store ids, **store display names**
  (Joe, 2026-09-16: not worth encrypting), node ids, document ids, blob sizes and timing,
  membership. Node titles are inside the encrypted tree document.

## Primitives (crate `pimble-crypto`, pure Rust, native and wasm32, no I/O, no async)

The PM has landed the API as a skeleton in `crates/pimble-crypto/src/lib.rs`; agent K fills
it in. Everyone else codes against the skeleton's signatures, which are the contract.

| Purpose | Choice |
| --- | --- |
| Password to keys | Argon2id, 32 MiB, 3 passes, 1 lane (web-tolerable), 16-byte salt per user; 64 bytes out, split by HKDF-SHA256 into `auth_key` (sent to the server as the password) and `kek` (never leaves the client) |
| Symmetric encryption | XChaCha20-Poly1305, 24-byte random nonce, associated data as stated per use |
| Account keypairs | X25519 for key wrapping, Ed25519 for signing envelopes |
| Key wrapping | X25519 ECDH with an ephemeral sender key, HKDF-SHA256 to a wrapping key, XChaCha20-Poly1305; the envelope is signed by the sender's Ed25519 key |
| Recovery | 20 random bytes as a 32-character base32 code shown once at signup (`XXXX-XXXX-…`), Argon2id with its own salt to a second KEK that wraps the same account keys |
| Randomness | `rand` with the `getrandom` js feature on wasm |
| Encoding | binary for blobs (the layout below); JSON with base64url for envelopes and key blobs, all versioned with `v: 1` |

**Blob layout** (what the server stores and relays; `pimble_crypto::Blob`):
`b"PB"` · version `u8 = 1` · key id 16 bytes (UUID) · nonce 24 bytes · ciphertext. The
associated data is `"{store_id}/{doc_id}"` so a blob cannot be replayed into another
document. The plaintext of a content blob is a yrs update (`encode_update_v1`) or, for a
snapshot, a full state update (`encode_state_as_update_v1` against an empty state vector).
The tree document's blobs are the same for the store document.

**Client-derived login.** The server never sees the password. Signup sends
`auth_key` (base64url) which the server hashes again with argon2id at rest, exactly as it
hashes passwords today; login sends `auth_key`. `GET /api/v1/kdf?email=…` returns the
user's salt and parameters, and for an unknown email a deterministic decoy salt
(HMAC-SHA256 of the lowercased email under a server secret) so the endpoint reveals
nothing. The recovery code derives its own KEK and is the only way back in without the
password.

## Data model additions

**Accounts service (RhypeDB):**

- `User`: `kdf_salt`, `kdf_m_cost`, `kdf_t_cost`, `kdf_p_cost`, `public_encryption_key`,
  `public_signing_key`, `account_key_blob` (JSON string: the account private keys wrapped
  under the password KEK), `recovery_salt`, `recovery_key_blob` (the same keys wrapped
  under the recovery KEK). Existing keyless users (the two smoke accounts) are deleted.
- `HostedStore`: `kind` (`plain` | `vault`).
- `KeyGrant`: `user`, `store`, `key_id`, `envelope` (JSON `KeyEnvelope`), plus the
  denormalized scalar ids the crate already uses for lookups. One per (user, store, key
  id). Rotation adds a new key id and new envelopes; old ones stay so old blobs decrypt.

**Pimble server, a store of kind `vault`** (`Store.kind`, new in `pimble-core`, default
`plain` for every existing serialized store): no `StoreDocument`, no `ContentDoc`, no
search index. Only the vault RPCs below apply; every other store-scoped RPC answers a new
error `-32005 encrypted_store` ("this store is encrypted; use the vault API"). On disk:
`<store>/vault/{doc_id}/log` (append-only, length-prefixed `seq u64 | len u32 | blob`) and
`<store>/vault/{doc_id}/snapshot` (`seq u64 | blob`), with an in-memory index of offsets
built at open. `doc_id` is a node id, or the literal `tree`.

**Pimble server, the desktop side** (`sync.json` gains `mode: "vault"` and a per-document
`last_seq` map; the local store itself stays `plain` on disk): the local keystore is
`<config dir>/pimble/keys.json`, mode 0600, holding the unwrapped account keys and the
unwrapped store keys by (store id, key id), written after sign-in. Moving the unwrapped
material to the OS keychain is a follow-up, not this phase.

## RPCs (landed by the PM in `pimble-rpc`; B implements them in `pimble-server`)

| Method | Auth | Behaviour |
| --- | --- | --- |
| `createStore { path, name, kind?, store_id? }` | Service | `kind` defaults to `plain`; `store_id` lets the accounts service create the hosted twin of a local store under the same id (refused if that id is already open) |
| `vaultAppend { store_id, doc_id, blob }` | editor | appends, answers `{ seq }`; notifies subscribers with `StoreChangeKind::VaultAppended { doc_id, seq }` and the blob in `update` |
| `vaultFetch { store_id, doc_id, after_seq }` | reader | `{ snapshot: Option<{ seq, blob }>, updates: [{ seq, blob }], head }`: the snapshot if its seq is greater than `after_seq`, then every update after `max(after_seq, snapshot.seq)` |
| `vaultSnapshot { store_id, doc_id, upto_seq, blob }` | editor | stores the snapshot; the server may drop updates with seq ≤ `upto_seq`; refused if `upto_seq` > head |
| `vaultListDocs { store_id }` | reader | `[{ doc_id, head, snapshot_seq }]` |

`subscribeStoreChanges` on a vault store delivers `VaultAppended` notifications with the
blob so a live client never re-fetches. Blobs travel base64url in JSON. Limits: one blob
at most 4 MiB; a document's log at most 64 MiB before a snapshot is required (append
answers `-32006 snapshot_required`).

## Accounts service endpoints (C)

| Method and path | Auth | Behaviour |
| --- | --- | --- |
| `GET /api/v1/kdf?email=` | none | `{ salt, m_cost, t_cost, p_cost }`, decoy for unknown emails |
| `POST /api/v1/signup` | none | body adds `auth_key`, `kdf: { salt, m_cost, t_cost, p_cost }`, `public_keys: { encryption, signing }`, `account_key_blob`, `recovery_salt`, `recovery_key_blob`; `password` is gone |
| `POST /api/v1/login { email, auth_key }` | none | as today otherwise |
| `GET /api/v1/me/keys` | session | `{ public_keys, kdf, account_key_blob }` (never the recovery blob) |
| `POST /api/v1/recover { email, recovery_code_auth }` | none | out of scope for this phase beyond storing the blob; returns 501 |
| `GET /api/v1/users/lookup?email=` | session | `{ id, public_keys }` for a verified user, 404 otherwise; rate limited |
| `POST /api/v1/stores { name, kind, store_id? }` | session | creates the hosted store with that kind and, when given, that id (the Pimble server's new `store_id` parameter) |
| `GET /api/v1/stores/{id}/keys` | any grant | the caller's envelopes for that store |
| `PUT /api/v1/stores/{id}/keys { envelopes: [{ user_id, key_id, envelope }] }` | owner or editor of the store for their own user; owner for others | upserts envelopes; each envelope's signature must verify against the caller's public signing key |

Signup's verification flow is unchanged. The web app owns the account pages now (below),
so the accounts service redirects verification outcomes to `/app/login?verified=1` and the
error variants likewise.

## Desktop (E, after B): sign-in and the encrypting link

Local-server RPCs, Service-only, called by the app: `cloudSignIn { url, email, password }`
(derive keys, log in, fetch and unwrap the account keys, persist session and keys),
`cloudSignOut`, `cloudStatus` → `{ signed_in, email, url }`, `cloudHostStore { store_id }`
(create the hosted vault twin under the same id, generate the store key, wrap it to self,
`PUT …/keys`, link with `AuthMethod::CloudSession`), `cloudListHostedStores`,
`cloudAddHostedStore { store_id }` (fetch envelopes, unwrap, create the local store empty,
link). `AuthMethod::CloudSession { url, session }` is a new variant: the link mints a JWT
through `POST /api/v1/token` before every connect.

`VaultLink` (`pimble-server/src/vault_link.rs`) replaces `SyncLink` for `mode: "vault"`:
at start `vaultListDocs`, then `vaultFetch` per document from its `last_seq`, decrypt, and
apply through `apply_edit`/`apply_store_update` with `client_id = "vault-link:<uuid>"`;
subscribe and apply `VaultAppended` blobs live; encrypt and `vaultAppend` every local update
whose source is not this link, remembering its own seqs to drop echoes; upload a snapshot
per document every 200 appended updates. Keys come from the keystore; a blob with an
unknown key id is logged and skipped, never fatal.

The app: "Account" in the menu with Sign in (URL, email, password), Sign out, status in the
status bar; store row menu "Host on Pimble Cloud…" and "Add hosted store…"; the sync badge
shows "encrypted" for vault links.

## Web app (A)

- Account pages inside the rinch web app: `/app/signup` (email, password twice, shows the
  recovery code once with a "I have saved it" confirmation), `/app/login` (with the
  verified/expired/invalid banners moved here), `/app/account` (stores, members, keys are
  not shown). The static site's signup, login and account pages become redirects to
  these. The web app derives keys with `pimble-crypto` compiled to wasm.
- After login the app holds the unwrapped account keys and store keys in memory only
  (never storage); a reload asks for the password again.
- Vault client: for each hosted vault store, fetch the `tree` document, decrypt, and hold a
  `pimble_crdt::StoreDocument` in the browser (the crate builds for wasm); the tree UI
  reads it instead of calling `getChildren`; tree edits apply locally and are appended
  encrypted. Node content: fetch, decrypt, feed the collab session; outbound updates are
  encrypted and appended. `VaultAppended` notifications are decrypted and applied. Plain
  stores keep the existing RPC path.
- Search in the web app: client-side over decrypted titles and any loaded content. No
  server search for vault stores.
- **Endpoint-agnostic (Joe, 2026-09-15: the browser must also be a client for relayed
  shares).** The vault client never assumes one server: every store it opens carries its
  own RPC endpoint and token (`POST /api/v1/token` answers per-store `rpc_url`s in the
  relay phase; today all are the hosted server), and it holds one `PimbleClient` per
  endpoint. A relayed store is served by the owner's local server through the relay,
  which presents the plain local store as a vault: it encrypts the update stream on the
  way out with the share key and keeps a per-document sequence log so a late-joining
  client can `vaultFetch` from a point. Keys reach the browser through the same
  envelope path. Nothing in the web app's code may special-case "the" server.

## Ownership

| Who | Scope (no one edits another's files) |
| --- | --- |
| PM | this file, skeletons (`pimble-crypto` API, `Store.kind`, vault RPC types and stub handlers), root `Cargo.toml`, `CLAUDE.md`, `docs/NEXT_SESSION.md`, `site/` link changes, commits |
| K | `crates/pimble-crypto` implementation and tests (native and wasm) |
| B | `crates/pimble-server` vault storage, handler methods, authorization, `createStore` kind and id; `crates/pimble-cli` (`create-store --kind vault --id`, `vault-*` commands); `crates/pimble-server/tests/vault.rs` |
| C | `crates/pimble-cloud`: everything under "Accounts service endpoints", the schema, tests |
| A | `web/` and `crates/pimble-app` (web only): account pages, crypto in the browser, the vault client |
| E (after B lands) | `crates/pimble-server/src/{vault_link,keystore,cloud}.rs`, the desktop sign-in commands and UI in `crates/pimble-app` (native), `pimble-core::AuthMethod::CloudSession`, `pimble-client` wrappers for the vault RPCs if A has not added them |

`pimble-client` wrappers for the five vault RPCs are added by whichever of A or B needs
them first, in `crates/pimble-client/src/client.rs`, kept to thin pass-throughs; the other
agent waits for them rather than duplicating.

## Verification

- K: round trips for every primitive, known-answer tests for HKDF and XChaCha20-Poly1305
  from the RFC vectors, tampering detection, wrong-key failure, envelope signature
  verification, `cargo test -p pimble-crypto` natively and `cargo check --target
  wasm32-unknown-unknown`.
- B: log and snapshot round trip through the RPCs, fetch semantics around a snapshot,
  reader versus editor, `encrypted_store` on plain RPCs, `snapshot_required`, subscription
  delivery of `VaultAppended`, `createStore` with a chosen id, survival across close and
  reopen, all against a real server as `tests/sync.rs` does.
- C: signup with keys, kdf decoy, login with `auth_key`, `me/keys`, lookup, key envelopes
  put and get with signature verification, store creation with a chosen id and kind, all
  the existing verification tests still passing, against a real rhypedb-server.
- A: the web app signing up, showing the recovery code, logging in, opening a vault store
  seeded by B's tests or a script, editing, and a second tab seeing the edit; a wrong
  password fails locally without a request.
- PM: the four-process stack in the browser end to end, as for phase 1.
