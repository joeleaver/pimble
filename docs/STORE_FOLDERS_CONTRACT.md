# One store folder on several computers (draft, 2026-11-19)

Status: **decisions answered by Joe 2026-11-19** (below). Nothing here is built yet; it follows convergence wave 2, which works in the same files.

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
