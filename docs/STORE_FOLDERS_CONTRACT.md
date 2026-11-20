# One store folder on several computers (draft, 2026-11-19)

Status: **decisions answered by Joe 2026-11-19** (below). Rules 1-5 are built (2026-11-19,
not merged or released; "Built" at the end).

Joe, 2026-11-19, on a store folder kept in Dropbox and opened on two computers: "we have to
plan for the possibility."

## What happens today

A store is a folder (`<name>.pimble`): `manifest.json`, `nodes/<id>.yrs` (one yrs document
per node), `blobs/`, `people.json`, and, written there too, per-computer state:
`sync.json` (the link's remote and its read positions, `last_seq`), `vault-link.json` (what
the link believes the remote holds), `index/` (the search index, rhypedb). Joe's stores
live in Dropbox.

When two computers open the same folder, each holds the documents in memory and writes them
back (a 750 ms debounce, whole files, atomically). Nothing watches the folder, so:

1. **Edits are lost.** Each computer writes its copy over the other's; an edit that only
   one had in memory and that the other overwrote is gone from the file. Neither app shows
   the other's edits until it restarts, and then only what survived.
2. **Sync tools make conflict files** (`… (conflicted copy 2026-11-19).yrs`, OneDrive's
   `…-COMPUTER.yrs`) that nothing reads.
3. **Per-computer state is shared.** One computer's read positions and beliefs about the
   remote are read as the other's: entries skipped or read twice, edits not sent (the
   convergence reports heal the belief, not the positions).
4. **The search index is written by two processes** through a file sync tool: lock
   contention at best (seen 2026-11-18), a corrupt index at worst.

A yrs document is built for this: any two copies merge, in any order, with nothing lost.
What is missing is merging what is on disk instead of overwriting it.

## The rule

**What a computer finds in the folder is merged in, never overwritten; and what belongs to
one computer is not kept in the folder.**

1. **Per-computer state leaves the folder.** `sync.json`'s read positions and link state,
   `vault-link.json`, the convergence `devices.json` record (already outside) and `index/`
   move to `<data dir>/pimble/stores/<store id>/`. The folder keeps only what every
   computer shares: the manifest, the documents, the blobs, `people.json`. Moved once, at
   open, the way `store.yrs` was migrated; a folder that another computer already migrated
   is left alone. (Whether the link's remote, the `sync.json` `remote`, is shared or per
   computer: decision 2.)
2. **Merge before write.** Before writing `nodes/<id>.yrs`, the store reads the file; if it
   is not what this process last wrote or read (a content hash), it is merged into the
   document in memory first. A write never loses what another computer put there.
3. **Watch the folder.** A change to `nodes/` (a file another computer, through the sync
   tool, wrote or added) is merged into the open document and goes out exactly as a
   remote update does (the window's views, the search index, every link), attributed to
   the folder. Debounced (sync tools write in bursts) and checked by hash, so this
   computer's own writes are not merged back. `manifest.json` likewise, field by field.
4. **Conflict files are merged and removed.** A file in `nodes/` whose name starts with a
   node id and is not exactly `<id>.yrs` (Dropbox's "conflicted copy", OneDrive's
   `-<COMPUTER>`, Syncthing's `.sync-conflict-…`, Google Drive's `(1)`) is merged into that
   node's document and deleted once the merge is written.
5. **Each computer says it is there.** A computer that opens the folder writes
   `computers/<device>.json` in it (name, last seen, version), and reads the others'. The
   store's Properties say "Also open on joe-desktop (seen 2 minutes ago); changes arrive
   through the folder's sync." An older Pimble on the other computer (one that overwrites)
   is named there with a sentence asking for it to be updated.

## Proving it

Two `LocalStore`s (two "computers") open on one folder, with a fake sync tool between them
that copies files late, out of order, in bursts, and sometimes as conflict copies; random
edits on both; at the end both hold identical documents and the folder holds exactly one
file per node. Swept over seeds like the tree's convergence test. And the app on two
scratch data dirs against one folder, by hand, once.

## Decisions (Joe, 2026-11-19: both recommendations)

1. **Support it, or refuse it.** Support one folder on several computers through a sync
   tool, as above (recommended: people will do it, Joe already does, and yrs makes it safe),
   or detect a second computer and refuse to open the folder, pointing to a replica through
   Pimble Cloud instead.
2. **A linked store's remote** (the hosted or relayed twin `sync.json` names): shared by
   every computer opening the folder (recommended: they are one store; each computer links
   it with its own read positions, moved out per rule 1), or per computer.

## Built (2026-11-19)

All five rules, in `pimble-store` (what a store does with its folder) and `pimble-server`
(watching it, announcing what it merges, saying who is there).

### Rule 1: per-computer state outside the folder

- A `LocalStore` opened with a state root keeps this computer's state in
  `<state root>/<store id>/`: `link.json` (`sync.json`'s `last_sync` and `last_seq`),
  `vault-link.json`, `people.json`, `index/`. A server's state root is `stores` beside its
  replicas directory: `<data dir>/pimble/stores/` by default, inside a test's temp
  directory whenever its replicas are (`handler::state_root_beside`,
  `StoreManager::with_state_root`, `StoreManager::state_dir`). A bare `LocalStore` (the
  importer, unit tests) has no state root and keeps the old single-file layout.
- `SyncConfig` stays one struct for its callers (sync link, vault link, share upkeep, relay
  face, handler: none of them changed); `LocalStore::read_sync_config` lays `link.json`
  over the folder's `sync.json`, and `write_sync_config` splits it, rewriting the folder's
  file only when the shared part changed (a vault link records a read position on every
  append; each of those rewriting a synced file would be a change for every other computer
  to merge). The shared part (remote, mode, `via_relay`, `vault_key_id`, access, roots)
  stays in the folder (decision 2); `clear_sync_config` removes both.
- Migration at open, once (`local/folder.rs` `migrate_per_computer_state`). Whose is the
  state the folder holds? **Decided here:** it is this computer's when no newer Pimble has
  opened the folder yet (no `computers/` directory, which every newer Pimble makes at open
  and an older one never touches) and this computer has none of its own; then it is moved
  out (an index on another filesystem is deleted and rebuilt, being derived). Otherwise it
  is an older Pimble's on another computer: left where it is and never read (an older
  Pimble writing positions into `sync.json` again is ignored at every read too).
- `people.json` moved out as well: **decided here** after reading `people.rs`: it is the
  signed-in account's derived view (its own name in each share), one computer's, not the
  store's.
- `removeReplica` deletes the replica's state directory with it.

### Rule 2: merge before write

- `LocalStore` remembers a hash of the bytes it last read from or wrote to each
  `nodes/<id>.yrs` (`disk_hashes`) and to `manifest.json`. `write_node` (every document
  write: `flush`, conflict merges) reads the file first and merges it when the hash
  differs; `write_manifest` (every manifest write: `flush`, root, scope and ended roots,
  partial replica names) merges field by field (`merge_manifests`: a field one side
  changed takes that side's value, both changed keeps ours; scope roots are the union, put
  in one order both computers reach when one side lacked a root; ended roots a three-way set
  merge; id and kind never change).
- What a merge changed in a held document queues as a `FolderMerge` (its diff, effect and
  the shapes before and after) for the server to announce.
- **Decided here:** `flush` no longer rewrites the manifest for its `modified_at` alone; it
  writes it when something else changed, or when documents were written and the file's
  `modified_at` is over ten minutes old. Each rewrite of a shared manifest is a change for
  every other computer, and two at once is how a sync tool makes conflict copies.
- Temporaries are `<stem>.<random>.tmp` (two processes on one folder shared `<id>.tmp`);
  ones over an hour old are swept at open.

### Rule 3: the folder is watched

- `pimble_store::watch_folder`: one `notify` watcher for the whole process (each is an
  inotify instance, 128 per user by default, and a test binary starts dozens of servers),
  routing paths to whoever watches `nodes/`, the folder (`manifest.json`) and `computers/`.
- `pimble-server/src/folder_watch.rs`: one task per open plain store (started wherever a
  plain store opens: `openStore`, `createStore`, a replica's creation, an implicit open;
  stopped by `closeStore` and `PimbleServer::stop`). Changes rest 500 ms (at most 2 s) and
  are merged by `LocalStore::absorb_folder_paths`; a file whose hash is this process's last
  read or write is skipped (its own write coming back). A held document with more than its
  file is marked dirty and written back (a sync tool can put an older copy over a newer
  one); one equal to its file is unmarked, so receiving a document never echoes a write.
  The task announces queued merges every second (flush merges land there too) exactly as
  `apply_node_update_from` announces a peer's update: `derive_changes`, the index fed,
  `broadcast_document_change` with the diff, `source_client_id: "folder:<store id>"` (no
  link's own id, so every link forwards it), root adoption and a debounced repair after a
  structural change. A new node file joins the tree that way.
- A manifest changed in the folder is merged in (the tree re-rooted if the root changed);
  nothing is announced for it (see "Not built").

### Rule 4: conflict copies

`classify_node_file`: a name that starts with a node id and ends in `.yrs` but is not
exactly `<id>.yrs` is a conflict copy (Dropbox, OneDrive, Syncthing, Google Drive styles,
`conflict_copy_name` makes each for tests). Merged into the document, the merged document
written, the copy deleted: when the watch sees it and at open. A copy that does not load
as a yrs document is left alone with a warning.

### Rule 5: computers say they are there

`computers/<presence id>.json` (`name`, `last_seen`, `version`): written when the store
opens and every three minutes; the others' read at open, every minute and whenever
`computers/` changes. **Decided here:** the file is named by a hash of the device id in
`devices.json`, not the id itself, which the convergence reports use and which stays out of
the folder (`tests/convergence.rs` checks that). The name is the host name.
`Store::also_open_on` (`OtherComputer { name, last_seen, version, older }`, computers seen
in the last 14 days, newest first) is filled on every `Store` the server returns;
`OtherComputer::sentence` says "Also open on joe-desktop (seen 2 minutes ago); changes
arrive through the folder's sync." and `update_sentence`, for one whose version is older
(`pimble_core::version_older`), "joe-desktop runs Pimble 0.28.8, which can write over
changes made here in this folder. Please update Pimble on joe-desktop." The Properties
panel shows both under "Lives in" for any node of the store.

### Tests

- `crates/pimble-store/tests/store_folders.rs`:
  `a_write_never_loses_what_another_computer_wrote`, `the_manifest_is_merged_field_by_field`,
  `an_external_change_is_merged_once_and_own_writes_are_not` (a new node joins, a stale
  copy is written back), `conflict_copies_are_named_by_every_tool_style`,
  `conflict_copies_are_merged_and_removed` (each style, while open and at open, a
  non-document left alone), `per_computer_state_moves_out_once` (and a folder already
  migrated by another computer, with an older Pimble's positions written back into it),
  and the randomized `two_computers_through_a_sync_tool_converge`: two folders, a fake
  sync tool that notices changes when asked, delivers late, out of order across files, in
  bursts, as conflict copies in any style or by writing over the other computer's write;
  random creates, renames, text, moves and deletes on both through the store's API; after
  the tool settles, both hold identical documents (state vectors, delete digests, fields,
  text) and each folder exactly one file per document, no copies. Seeds:
  `PIMBLE_FOLDER_SEEDS=1,5` replays, `PIMBLE_FOLDER_SEEDS=300` runs 0..300, default 12.
  300 seeds pass in about 3 s in release (run five times), 2000 once.
- `crates/pimble-server/tests/store_folders.rs`: two `PimbleServer`s with their own data
  directories on one folder: an edit through one's RPC reaches the other within 10 s,
  announced there once (`folder:<id>`, with the bytes), a node made on the other joins the
  first's tree, each lists the other in `also_open_on`; and an older Pimble's
  `computers/` file is named with the update sentence, the index in the state directory.
- `pimble-core` `other_computers_are_named_in_sentences`, `versions_compare_by_number`.
- Existing tests moved to the state directory: `remote_mounts.rs` (`last_sync` in
  `link.json`), `convergence.rs`, `vault_link.rs`, `relay.rs`; `remote_mounts.rs` and
  `pimble-cloud`'s integration test now give a replicas directory inside their temp
  directory, so what lives beside it stays inside too.

### Known limits and not built

- **A sync tool that lands an older copy of a file over a newer one from the same
  computer**, before the receiving computer read the newer, loses the newer there until the
  sender writes that document again: neither computer knows the other lacks it. Real tools
  keep one computer's versions of a file in order; the randomized test's tool does too
  (found by the sweep: with reordering of one file's versions allowed, about one seed in
  300 diverged this way). A periodic exchange of state vectors through `computers/` would
  close it; not built.
- A conflict copy of `manifest.json` is not merged (only `nodes/`).
- A manifest change from another computer (a name, a root) is merged but not announced;
  the app sees it at its next `listStores`. Likewise `also_open_on` reaches the app with
  the next `Store` it is sent; no notification says it changed.
- Another computer linking or unlinking the store (the shared `sync.json`) takes effect
  here at the next open, not live.
- No periodic rescan: a change the watch misses (a filesystem that sends no events) is
  merged at this computer's next write of that document (rule 2) or the next open.
- The two-computer walk-through of the app by hand is still to do.
