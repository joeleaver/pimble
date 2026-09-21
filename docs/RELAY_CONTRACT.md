# Relay contract: sharing from an unhosted store, with nothing uploaded

Status: written 2026-09-21 by the PM from `docs/NODE_DOCUMENT_CONTRACT.md` section 5b,
which Joe approved on 2026-09-18 (decision 3: "Not supposed to be hosted, that's what the
relay is for"; and "we really shouldn't host anything unless it's specifically been asked
to be hosted"). This document fills in how. Where it departs from 5b it says so.

## What it is

A store that is not hosted on Pimble Cloud can still be shared. The owner's own machine
serves the shared documents; members reach it through a relay that Pimble Cloud runs and
that **stores nothing**: no documents, no ciphertext, no queue. It pipes bytes between two
live connections and forgets them. What it pipes is already end-to-end encrypted: the
same `PB` blobs a hosted store holds, so the relay can read none of it.

The tier's promise and its price, both said in the Share dialog: nothing of yours is on
Pimble Cloud; people you invite reach the share only while this computer is on and
online. While it is off they read what they have, their edits wait on their own devices,
and two members do not see each other until it is back.

## The shape: host the encrypted twin yourself

Everything a hosted share does already works against "a Pimble server in JWT mode that
holds the store's vault twin": scope sets, per-document keys, `vaultListDocs/Fetch/Append/
Snapshot/SetDocKeys`, `setScope`, scoped principals, the owner's upkeep, members' vault
links, the web vault client. So the relay tier does not grow a second implementation of
any of it. The owner's server runs **a second Pimble server inside its own process, the
relay face**: JWT mode, loopback only, its own `StoreManager` over
`<data dir>/pimble/relay/`, holding the vault twin `<store id>.pimble` of each relayed
store. The owner's plain store is kept in step with its twin by **an ordinary vault link**
whose remote is the face's loopback URL, authenticated as it is against the hosted server
(a JWT minted from the signed-in account, which carries the owner's grant). Share upkeep
rides that link as it does for a hosted store. Members' connections arrive at the face
through the tunnel below and are ordinary authenticated connections with scoped
principals. The twin is derived and disposable, like the search index: deleted, it is
pushed again by the link's reconcile.

Departure from 5b: the twin holds every document of the store as ciphertext, not only the
shared ones. It is the owner's own disk, no member can reach a document outside their
scope (the same enforcement as hosted), and it keeps the link unchanged. Restricting the
twin to documents under a share is a later optimisation.

## Accounts service (`pimble-cloud`)

- **`Store.tier`**: `"hosted"` (the default, and what a row without the field reads as:
  the production-incident rule, old rows read with later fields optional) or `"relay"`.
  `POST /api/v1/stores { name, kind: "vault", store_id, tier: "relay" }` records the store
  and the owner's grant and does **not** call the hosted server. For a relayed store the
  name sent is the empty string: nothing on Pimble Cloud needs the owner's name for it
  (members see share names). `GET /stores` rows carry `tier`.
  Everything else about grants, invitations, share names, key envelopes and the token's
  claims is the same for both tiers.
- **`POST /api/v1/token`** answers, beside `token`, `exp` and `rpc_url`, a list
  `stores: [{ store_id, rpc_url }]` naming every relay-tier store the account holds a
  grant on, with `rpc_url = wss://<public host>/api/v1/relay/<store id>` (`ws://` for a
  plain-http public URL). The web client already reads this shape (`web/src/api.rs`,
  `Session.stores`).
- **The relay**, in memory only, under `/api/v1/relay` (already routed to this service by
  `"/api/*"` in `jkbase.toml`):
  - `GET /api/v1/relay` (WebSocket), **the owner's tunnel**. Authenticated with the
    account's session (`Authorization: Bearer <session>`), never a browser (`Origin`
    refused). First message from the owner: `{"serve": ["<store id>", ...]}`; the relay
    keeps only the ids whose tier is `relay` and on which the account is an owner, answers
    `{"serving": [...]}`, and records "store S is behind this connection". A later tunnel
    for the same store replaces the earlier one (the owner restarted). When the tunnel
    closes the record is gone and every member connection through it is closed.
  - `GET /api/v1/relay/<store id>` (WebSocket), **a member's connection**. The credential
    is the account's JWT, as for `/rpc`: `Authorization: Bearer`, or the `access_token`
    query parameter (a browser cannot set headers). The relay verifies the signature
    against its own JWKS, `aud`, `exp`, and that the `stores` claim names the store;
    a browser's `Origin` must be this service's own public origin. No tunnel for the
    store: close with code 4404 and reason `owner offline`. Otherwise the relay opens a
    virtual connection over the tunnel and pipes WebSocket messages both ways, closing
    the member's socket when the token expires. It buffers nothing beyond what is in
    flight; a slow side applies backpressure to the other; a message over 8 MiB closes
    the virtual connection.
  - **Tunnel framing** (binary WebSocket messages, owner tunnel only):
    `[conn: u32 BE][kind: u8][payload]`, kinds `1 open` (relay to owner; payload is the
    member's JWT, UTF-8), `2 text` (either way; one JSON-RPC text message), `3 close`
    (either way; payload empty). The `serve`/`serving` exchange is the only text traffic
    on the tunnel itself.
  - Limits: 64 member connections per tunnel, 16 tunnels per account; over a limit is a
    refusal, not a queue.
  - Keepalive: the platform edge reaps an upgraded connection after 600 s without a byte
    in either direction, so the relay pings every tunnel and every member connection
    every 30 s and closes one that has not answered in 90 s; the tunnel client pings too.
- The relay never writes anything about a connection to the database, and logs store ids
  and counts only.

## The owner's server (`pimble-server`)

- **`cloudRelayStore { store_id }`** (Service-only, like `cloudHostStore`): the person
  asked for this store to be shared from this computer. Refused for a store that is
  linked, a replica, or vault-kind. It makes the store key (keystore), registers the store
  with the accounts service as `tier: "relay"` with the owner's own key envelope (the same
  calls `cloudHostStore` makes, minus anything that touches the hosted server), creates
  the twin on the relay face, writes `sync.json` with `mode: "relay"` (a new
  `SyncMode::Relay`: a vault link whose remote is this process's own relay face; the URL
  is not stored, the face's port is new every run), starts the link and the tunnel.
  `cloudStopRelaying { store_id }` is the way back: shares stopped first or refused,
  tunnel entry withdrawn, link stopped, twin deleted, accounts-service record removed.
- **The relay face** starts when the first relayed store opens (at `openStore` a store
  whose `sync.json` says `relay` gets its link through the same `ensure_link` paths as
  any other, which start the face first). JWT mode with the JWKS and issuer of the
  signed-in account's service; `127.0.0.1:0`; every `Origin` refused; a random static
  token held in memory for the in-process lifecycle calls (`createStore`/`openStore` of a
  twin). No account signed in, or the accounts service unreachable: the face waits, the
  link is `Offline`, nothing else in the app is affected.
- **The tunnel client**: one task per signed-in account while any relayed store is open.
  Connects to `wss://<account url>/api/v1/relay` with the session, announces the stores,
  and for every `open` frame opens a loopback WebSocket to the face with the member's JWT
  as `Authorization: Bearer` (the face verifies it itself: the relay is never trusted for
  who someone is), piping text both ways. Reconnects with the vault link's backoff.
- `GetStoreSyncResponse.sync_mode`/`Store.sync_mode` report a relayed store so the app
  can say so (a third value beside plain and vault, or a separate `tier` field: the
  implementer's choice, stated in the report).
- **Members' side**: `cloudAddHostedStore` and the vault link take a store's endpoint
  from `/token`'s `stores` entry when there is one, else `rpc_url`. A link whose endpoint
  answers close code 4404 (`owner offline`) is `Offline` and retries with backoff; it is
  not an error to report. `cloudShareNode` on a relayed store works exactly as on a
  hosted one. On a store that is neither, the refusal becomes: "Sharing needs this store
  hosted on Pimble Cloud or shared from this computer." (the app offers both).
- CLI: `cloud-relay-store <store>`, `cloud-stop-relaying <store>`; `cloud-list-hosted`
  prints `tier`.

## The apps

- **Share dialog on a store that is neither hosted nor relayed** offers the two ways,
  each with its sentence: "Share from this computer: nothing is uploaded. People you
  invite reach it while this computer is on and online." and "Host on Pimble Cloud...:
  an encrypted copy is kept there, so it works while this computer is off." Choosing the
  first calls `cloudRelayStore`, then continues into the share as today. Nothing is ever
  chosen for the person.
- The store row of a relayed store says `shared from here` with the link's state, not
  `encrypted · synced`.
- A member sees no difference except when the owner is offline: the badge reads
  `owner offline` and the store is read and edited from the local replica as any offline
  store is.
- **Web**: a relay-tier store is listed from its own endpoint (`Endpoints` already
  connects one client per URL); when that endpoint is down the store row is shown from
  the accounts service's row with `owner offline`.

## Verification (the bar)

On the local stack (`scripts/local-stack/`): an unhosted store; "share from this
computer"; `$STACK/stores` stays empty and the accounts database holds no name and no
blob for it; two members add it and co-edit the shared folder's tree through the relay
while the owner's server is on; the relay process's memory and disk hold nothing after
they disconnect; with the owner's server off, members read and edit locally, reconnect
attempts get `owner offline`, and everything converges when it returns; a reader cannot
write; a node outside the share is unreachable; killing the accounts service drops the
tunnel and nothing is lost. The hosted tier's walk-through passes unchanged.
