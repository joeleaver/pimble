# Convergence: devices compare what they hold (draft, 2026-11-19)

Status: **decisions answered by Joe 2026-11-19** (below). Nothing here is built yet.

Joe, 2026-11-19: "this kind of de-sync can't be allowed to happen again, it ruins the whole
point of the app."

## What went wrong, all of it one thing

The faults found on 2026-11-19 ("Shared with Will"):

1. An update parked by yrs was relayed only as the bytes that released it (fixed 0.28.2).
2. A live append left a gap the link noticed only at its next connect (0.28.2).
3. A member's device lacked a tombstone; two repairs undid each other (0.28.2, 0.28.5).
4. A paged read straddled a snapshot and held part of a large change (0.28.6).
5. Twelve structs from 2026-10-26 never reached the shared copy, and every edit built on
   them has waited since, on every device.

Each was fixed where it was found, but they share a cause. **An encrypted link decides
what to send from its own belief about what the remote holds** (`pushed_sv`, the
cursors): the remote is ciphertext and cannot answer with a state vector. When that
belief is wrong, for any reason (a bug, a crash between an append and its bookkeeping,
a file restored from a backup, a replica rebuilt), the link sends nothing and nobody
notices. The plain sync link has no such belief: every reconcile compares the state
vectors both ends actually hold (`syncNodes`), which is why it heals.

Fixing each way the belief can go wrong does not make the belief right. The rule has to
be that devices compare what they hold.

## The rule

**Every device that holds a document says, sealed, what it holds of it, and every device
that holds more than another says it sends what the other lacks.** A state vector says
exactly what a yrs document holds (and a parked update's `missing` says what it waits
for), so the comparison is exact and cheap.

1. **State reports.** For each document it holds, a device writes its state vector (and,
   when it has a parked update, what that waits for) to a slot of its own on the remote:
   `vaultReport { store, doc, device, blob }`, sealed under the document's data key like
   any blob, so the server stores ciphertext and overwrites the slot. A device reports
   after it applies or makes changes (debounced, 5 s), and all of them at connect. A
   report is about 20 bytes per client in the document.
2. **Comparing.** At connect and whenever a report changes (a `VaultReported`
   notification), a device reads the others' reports of the documents it holds. Where its
   own vector has a clock beyond another device's report, the other lacks something it
   holds: it appends the diff from that report (one append whichever device needs it; a
   duplicate merges to nothing). Where another's is beyond its own, it should receive it:
   the log normally brings it, and if it does not within a minute, the other device sees
   the same comparison from its side and sends.
3. **What is waited for.** A report that names a parked update's `missing` vector is a
   request: any device whose document has those structs appends the diff from that
   vector. The twelve structs of 2026-10-26 would have been asked for the moment either
   device saw the gap, and sent by whichever held them.
4. **Out of step, said.** A document whose reports still differ (or that still waits on
   structs) after five minutes during which both devices were connected is out of step.
   The log says so (`warn`, with the document and the clients: diagnostics carry it), and
   the app says so **on the note itself** (Joe: "it should go on the specific note, not the
   store"): a mark on its row in the explorer and a line at the top of its pane or canvas
   saying what is out of step and since when, in its Properties, and in the MCP's answer
   for it. Never silent.
5. **What cannot be had.** When no device that ever reported holds the missing structs
   (the device that made them is gone, or its copy was rebuilt from the remote), the
   comparison cannot finish. Then the waiting part is released: the parked update is
   dropped from the document after it has waited 24 hours with every reporting device
   lacking the same structs, the log and the note's Properties say what was given up and
   whose it was, and the document is in step again from what everyone holds. Joe's call,
   below.

The owner's own devices, the relay's twin and the hosted twin are all just devices here.
`pushed_sv` stays as a hint (it saves sending what is known to be there) but no longer
decides alone what is sent.

## Proving it

The repair test proves the tree converges; nothing proves the links do. Wave 3 is a
randomized test of three devices over a real relay and real vault links, with faults
injected between steps:

- appends dropped after the server stored them but before the device recorded them;
- a device killed mid-push and started again from its files;
- a link's `vault-link.json` deleted or restored from an older copy;
- snapshots stored while another device pages through the log;
- notifications dropped, connections cut, the owner's computer away.

After every run with all devices online and quiet, every document on every device must be
byte-for-byte the same state (equal state vectors, nothing parked), within a bound. Seeds
are swept the way `three_replicas_converge`'s are. A failing seed replays.

## Waves

1. State reports and comparing (sections 1-2): the RPC and notification, the
   server's slots (vault stores and the relay face), the link's reports and its sends from
   the comparison. The web vault client the same.
2. Requests for what is waited for, "out of step" said in the log and the app (3-4).
3. The randomized fault test (Proving it), and the giving up (5) once Joe decides it.

## Decisions (Joe, 2026-11-19)

1. **Giving up on what no device holds** (section 5): dropped after 24 hours in which every
   reporting device lacks it, and said so on the note.
2. **"Out of step" is shown, on the note** (its row, its pane, its Properties, the MCP), not
   on the store row.
