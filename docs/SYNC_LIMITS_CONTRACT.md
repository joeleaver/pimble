# Sync limits contract: no size ever stops a store from syncing

Status: **decided by the PM on Joe's direction, built** (2026-11-18; see "Built" at the
end; not deployed or released). Joe, after
Personal stopped syncing: "We can't just stop syncing because we hit limits. Those limits are
arbitrary, anyway. We need a better plan and an emergency fix." The emergency fix shipped in
0.26.1 (docs/NEXT_SESSION.md); this is the plan. Builds on docs/CRYPTO_CONTRACT.md (vault
stores: per-document logs and snapshots) and docs/RELAY_CONTRACT.md (the relay twin).

## What went wrong

A canvas grew to 4.9 MB with a long history. Its encrypted log on the relay twin reached the
64 MiB log limit; the server asked for a snapshot; the snapshot, one blob holding the whole
document, was over the 4 MiB blob limit; and the vault link took the refusal for a lost
connection and dropped every document of the store, retrying every 32 s for hours. Separately,
a reconnect skipped any document whose catch-up was over 3 MiB ("left for live updates"), and
jsonrpsee's 10 MB message default made one large document unloadable for new clients.

## Rules

1. **A limit is a reason to do something, never a reason to stop.** Every limit a sync path
   can meet has an automatic remedy (compact, split, ask again in parts) or, if it has none,
   holds back that one document and says so. No limit drops a link, a store or a client.
2. **One document never holds back another.** A link that cannot send or take one document
   keeps syncing every other one.
3. **Nothing is held back silently.** A document waiting for any reason is listed by the link
   with its reason, shown in the app on the store's row and on the node, and reported by the
   MCP.
4. **Limits are runaway guards only**, sized far beyond what a person makes (0.26.1: 256 MiB
   a blob, 1 GiB a log, 512 MiB an RPC message).

## Design

### Compaction by size, not only by count

Today a link snapshots a document every 200 appends. A few hundred big appends (paint
batches) filled 64 MiB. A link now also snapshots when the log has grown, since its last
snapshot, past **twice the size of the document's last snapshot, and at least 1 MiB**. The
remote says how large the log is: `vaultAppend`'s answer gains `log_bytes` and
`snapshot_bytes` (optional; an older server omits them and the count rule applies alone).
The same rule as today on what a snapshot may be stamped with (`applied_through`).

### Snapshots and appends in parts

A snapshot or an append larger than a **part** (8 MiB of plaintext) is sealed in parts:
each part its own blob under the document's key, with the associated data naming the part
(`"{store}/{doc}#{index}/{count}"`), sent as `vaultSnapshot`/`vaultAppend` calls that carry
`part: { index, count, group }`. The server stores a snapshot's parts together and replaces
the old snapshot only when the last part arrives (a half-uploaded snapshot never replaces a
whole one); it stores an append's parts as consecutive log entries marked as one group, and a
reader applies a group only when it holds every part. `vaultFetch` returns parts as they are
stored. An older reader that meets a part it cannot read leaves that document waiting with
the reason "made by a newer Pimble" (rule 3), never misapplies it.

### Saying what waits

`GetStoreSyncResponse` gains `waiting: [{ node_id, reason }]` (a sentence each), kept by the
link as it holds documents back and cleared when they go. The app shows a store row's badge as
"synced · 1 note waiting" and the reason in the Properties panel of that node; the MCP's
`get_node` and `list_stores` say it.

### Everywhere it applies

The desktop vault link (hosted stores and relay members and owners), the browser's vault
client (`web/src/vault.rs`, which appends and snapshots the same way), the hosted server and
the relay face (both `pimble-server` over `pimble-store`'s vault). Old and new mix: a new
client against an old server sends no parts larger than the old blob limit's worth it can
avoid and otherwise waits with a sentence; an old client against a new server behaves as
today.

## Not in this contract

Splitting a large canvas into several documents, and a lower-detail paint level for far
zoom-outs (docs/DRAWING_CONTRACT.md "Large canvases"). Both reduce sizes; neither is needed
for sync to keep working.

## Built (2026-11-18)

Every path: the desktop vault link (hosted stores, a relayed store's owner and its
members), the browser's vault client, the hosted server and the relay face (both
`pimble-server` over `pimble-store`'s vault).

### The protocol, as built

- `vaultAppend` answers `{ seq, first_seq?, log_bytes?, snapshot_bytes? }`. `log_bytes`
  and `snapshot_bytes` are the document's log and snapshot (every part) on the remote
  after the append.
- `vaultListDocs` answers `parts: true` from a server that takes parts.
- `VaultPart { index, count, group }` (at most 4096 parts) rides `vaultAppend.part`,
  `vaultSnapshot.part`, each fetched `VaultEntry.part` and
  `StoreChangeKind::VaultAppended.part`.
- `vaultFetch` answers a snapshot in parts as its first part in `snapshot` and the rest in
  the new `snapshot_parts`. It takes `max_bytes` and `skip` and answers `more`, so a
  document of several parts is fetched a page at a time (see the deviations below).
- `GetStoreSyncResponse.waiting` and `Store.waiting` are `[{ node_id, reason }]`
  (`pimble_core::WaitingDoc`).
- A part is sealed with the associated data `"{store}/{doc}#{index}/{count}/{group}"`
  (`pimble_crypto::blob_part_aad`). The blob layout and its version byte are unchanged.
- `pimble_rpc::vault` has `PART_BYTES` (8 MiB), `seal_parts`, `fetched_units` / `Unit`,
  `PartBuffer`, `DocKeys::open_part`/`open_parts`/`open_unit`, `LogSizes`,
  `snapshot_due_by`, `COMPACT_FLOOR` (1 MiB), `Mark::Span`, and the waiting sentences
  (`WAITING_FOR_KEY`, `MADE_BY_NEWER`, `TOO_LARGE_FOR_REMOTE`, `Unopened::reason`).

### Old and new together

- **An old reader meeting parts** (any client before this): a part does not decrypt
  under the whole-blob associated data an old reader uses, so its `Blob::decrypt` fails.
  The desktop link logs and skips the entry (`Applying::Skipped`), the browser logs it.
  Neither marks it, so the cursor stops in front of it, and `snapshot_due`'s
  `applied_through` rule keeps that reader from ever snapshotting past it. A snapshot in
  parts reaches an old reader as one undecryptable `snapshot` entry, so nothing of it is
  applied. An old reader never misapplies a fragment. It cannot say "made by a newer
  Pimble" either, because that sentence is new code; it keeps the document as it last
  read it. A new blob version byte was not needed, since the associated data already
  makes old readers refuse.
- **A new client against an old server** (no `parts` in its listing): it sends whole
  blobs only. A catch-up larger than 200 MiB, or a blob the server refuses for size,
  holds that document back with "Too large for the server it syncs with, which needs a
  newer Pimble to take it in parts." The old server ignores `max_bytes` and answers a
  fetch whole. An old server's append answer has no sizes, so the count rule applies
  alone.
- **An old client against a new server**: it sends no parts, no `max_bytes` and no
  sizes, so it gets what it got before, apart from documents a new client sent in parts
  (above).
- **A new reader meeting a blob of a newer version byte**: the document waits with
  "Made by a newer Pimble: update Pimble on this device to read it."

### Where

- `crates/pimble-store/src/vault.rs`: parts are held aside in memory per document and
  group (`append_part`, `snapshot_part`, `completes_group`). Part 0 starts or restarts a
  group, and later parts must come in order. A group nobody has sent a part of for 15
  minutes is dropped.
  - An append group lands as consecutive records in one write. A record that is a part
    sets the high bit of `len` and carries `index | count | group` before its blob.
  - A snapshot in parts is one `snapshot` file (`seq | "PP" | count | group | (len |
    blob)*`) that replaces the old one atomically, and only when its last part arrives.
  - A snapshot part 0 not newer than the held snapshot is taken and dropped. Part 0 of a
    snapshot beyond the head is refused.
  - The blob limit applies per part, and the log limit to the parts held so far, so a
    group that would overflow is refused (`-32006`) at the part that would.
  - A fetch that would start inside a group starts at its first part. At open, a log
    whose tail is a group cut short is truncated.
  - `sizes` gives the log and snapshot bytes.
- `crates/pimble-server/src/handler.rs`: the vault RPCs above. A new document's keys and
  scopes are stored when its group lands. A group's parts are announced one notification
  each. `vaultFetch` pages, and `waiting_of` fills `GetStoreSyncResponse.waiting` and
  `Store.waiting` (`listStores`, `openStore`, `describe_link`).
- `crates/pimble-server/src/vault_link.rs`:
  - Sending: the link seals in parts when the remote takes them, sends a group again
    whole once if the remote lost it, records the sizes, and snapshots by
    `snapshot_due_by`. A snapshot also goes in parts. The 200 MiB catch-up cap applies
    only to a remote without parts.
  - Reading: it reads fetches as units, holds live parts in a `PartBuffer`, and applies
    a group only when it is whole.
  - `Waiting` (per document and side, send or read) is kept wherever the link holds a
    document back: a refused or too-large append, a snapshot that could not empty a full
    log, no key to seal or open, a newer format, an incomplete group, an update that
    would not merge. Each entry is cleared when its side goes through.
  - A change to what waits is announced as the link's state again, and the app asks
    `getStoreSync`. An update that will not merge now holds back its document instead
    of dropping the link.
- `crates/pimble-client`: `vault_fetch` pages (`VAULT_FETCH_PAGE_BYTES`, 12 MiB);
  `vault_append_request` and `vault_snapshot_request` take whole requests.
- `web/src/vault.rs`: the same rules as the desktop link.
  - `VaultWire::append`/`snapshot` take `part` and append answers in full.
  - `prepare` seals in parts, `record_span` marks the group and snapshots by size, and
    `snapshot_blob` seals in parts.
  - Fetches are read as units, and `pump` buffers live parts (`take_parts`).
  - `held_back` per store is reported through `GetStoreSync`'s `waiting`, and
    `StoreSyncChanged` is pushed again when it changes.
- The app: `BackendEvent::StoreSyncChanged.waiting` and `AppStore::waiting`.
  - The store row's badge reads "encrypted · synced · 1 note waiting"
    (`badge_with_waiting`). It also shows "N notes waiting" on a row with no badge of
    its own: the browser's encrypted stores, and a store shared from here.
  - The Properties panel of a waiting node says "Waiting to sync." and its reason.
- The MCP: `list_stores` lists the waiting notes under their store with their links and
  reasons, and `get_node`'s header has a `sync: waiting. <reason>` line.

### Deviations

- **The associated data also names the group**, `#{index}/{count}/{group}`, not
  `#{index}/{count}` alone. Without the group, two groups of one document with the same
  count could be spliced into each other.
- **`vaultFetch` pages.** The contract has fetches return parts "as they are stored". A
  20 MiB document's three parts are about 27 MiB of base64, and a message through Pimble
  Cloud's relay may be 16 MiB (`relay_tunnel::MAX_MESSAGE_BYTES`, `pimble-cloud`'s
  `RelayLimits`), so one answer would close the member's connection: a limit stopping a
  store. `max_bytes`/`skip`/`more` let `PimbleClient::vault_fetch` ask for about 12 MiB at
  a time, at least one entry per page. A page boundary may fall inside a group: pages are
  joined before anything is read, and a group spliced by a snapshot landing between two
  pages reads as incomplete (the associated data binds every part to its group) and is
  read again later. Appends, snapshots and announcements go a part at a time, each about
  11 MiB as base64.
- Parts in flight are held **in memory** on the server, not on disk. A server that
  restarts in the middle of a group refuses the next part with "send the group again from
  its first part", and the link sends the group again, whole.

### Tests

- `pimble-rpc` (`vault.rs`): compaction by size, parts sealed and opened only whole
  (never by the whole-blob associated data, never as another index or group), a snapshot
  in parts as one unit, live parts waiting for their group, a newer format saying so.
- `pimble-store` (`vault.rs`):
  - An append in parts lands whole with its last part, consecutively, even with
    another append in between.
  - Parts out of order are refused.
  - A half-uploaded snapshot never replaces a whole one, and a snapshot in parts
    survives a reopen.
  - A group cut short by a crash is dropped at open.
  - Limits apply per part: 6 MiB in parts under a 4 MiB blob limit, and a 90 MiB group
    refused at the part that overflows a 64 MiB log.
- `pimble-server/tests/sync_limits.rs` (its own binary, its own limits: 9 MiB a blob,
  32 MiB a log):
  - Sixty 600 KiB appends that replace each other never fill the log: the largest log
    stays under 8 MiB, and the snapshot fires by size.
  - A 20 MiB document goes both ways in parts through a real hosted server, appended and
    snapshotted.
  - A half-uploaded snapshot replaces nothing, and an old-format reader opens no part.
  - Two documents (one under a key this device lacks, one in a newer format) are listed
    with their reasons while another document syncs both ways.
- `pimble-server/tests/relay.rs`, `a_twenty_megabyte_document_goes_both_ways_through_the_relay`:
  owner to member, member to owner, and a member who joins later reads both, all through
  the relay tunnel's 16 MiB messages.
- `web` (`vault.rs`): a 20 MiB edit goes in parts (whole to a server that takes none), a
  snapshot is due by size and goes in parts, a reader applies it whole from a fetch or
  live, a missing part waits with a reason, and a held-back document is listed and then
  released.

### Not built

- The local stack (`scripts/local-stack/`) end-to-end check was not run; the relay test
  above runs the real owner-side tunnel and face code with its 16 MiB messages.

- A whole blob over the relay's 16 MiB sent to an **older relay face** (an owner on a
  Pimble from before parts): the member's link sends it whole, the relay closes that
  connection, and the link tries again. Parts need both ends new.
- The browser does not say "1 note waiting" for a store whose page has not opened it (a
  store waiting for its key already says so in its name).
- The MCP and app tests do not drive a real waiting document end to end: the server test
  checks `getStoreSync` and `listStores`, and the badge text has unit tests.
