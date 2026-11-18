# Sync limits contract: no size ever stops a store from syncing

Status: **decided by the PM on Joe's direction, being built** (2026-11-18). Joe, after
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
