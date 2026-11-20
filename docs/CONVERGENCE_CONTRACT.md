# Convergence: devices compare what they hold (draft, 2026-11-19)

Status: **decisions answered by Joe 2026-11-19** (below). Wave 1 is built and released
(0.28.8, "Built: wave 1" below); wave 2 is built (2026-11-19, not merged or released; "Built:
wave 2" below); wave 3 is not.

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
   See "Built: wave 1" below.
2. Requests for what is waited for, "out of step" said in the log and the app (3-4).
3. The randomized fault test (Proving it), and the giving up (5) once Joe decides it.

## Decisions (Joe, 2026-11-19)

1. **Giving up on what no device holds** (section 5): dropped after 24 hours in which every
   reporting device lacks it, and said so on the note.
2. **"Out of step" is shown, on the note** (its row, its pane, its Properties, the MCP), not
   on the store row.

## Built: wave 1 (2026-11-19, not merged or released)

**RPCs** (`crates/pimble-rpc/src/methods.rs`, `types.rs`; `crates/pimble-server/src/handler.rs`;
`crates/pimble-client`):

- `vaultReport { store_id, doc_id, device, blob }` -> `{}`. `blob` is base64url like every
  vault blob. Reader or more (a reader's device needs what it lacks too); a share's member
  only within their scope (`require_vault_doc_in_scope`, judged through `Reach`), refused
  with "no grant for this document" outside it. Only a node's document, only one that has a
  log (a report never brings a document into being), at most 64 KiB of blob, a device id of
  1 to 64 bytes. Refusals are `-32602` with a sentence.
- `vaultReports { store_id, doc_ids }` -> `{ reports: [{ doc_id, device, blob, at }] }`.
  Reader; a member is answered for the documents of their scope and the rest are left out
  without saying whether they exist.
- `StoreChangeKind::VaultReported { doc_id, device, blob }`, sent on the store's
  subscription when a report is stored, scoped like `VaultAppended` (`named_by`,
  `delivery_for`). **Sent only to a subscriber that asks**:
  `subscribeStoreChanges(store_id, { reports: true })` (`StoreSubscribeOptions`, an optional
  second parameter an older server ignores; `PimbleClient::subscribe_store_changes_with_reports`).
  A `StoreChangeKind` a client does not know fails to deserialize, which a vault link of an
  older desktop reads as a broken subscription and the web page's subscription loop ends on,
  so an unasked kind would have broken every older client at every report.
- A server without the RPCs answers `-32601`; links and pages then carry on as before
  (`ClientError::is_method_not_found`).

**Server storage** (`crates/pimble-store/src/vault.rs`): `<store>/vault/{doc}/reports`, a JSON
map device -> `{ blob (base64), at }`, written atomically, overwritten per device, at most 32
devices per document (the oldest `at` goes), loaded at open (`VaultStore::set_report`,
`reports`; `StoreManager::vault_set_report`, `vault_reports`). It lives in the document's
directory, so it goes with the document. The hosted server and the relay face are both
`pimble-server`, so both serve reports.

**The report** (`crates/pimble-rpc/src/vault.rs`, shared by the desktop and the browser):
`Report { state_vector, deletes, missing }` (`NodeDoc::state_vector`, `NodeDoc::delete_set_digest`,
`NodeDoc::missing`), plaintext JSON `{ "v": 1, "sv": <base64>, "ds": <base64>, "missing": <base64>? }`.
`ds` is a SHA-256 of the document's delete set made canonical (clients in order, each one's
deleted clock ranges sorted and joined where they touch, `client u64 | count u32 | (start u32 |
end u32)*` little endian), so the same delete set gives the same digest on every device however
the blocks were split, merged or garbage collected. The report is sealed by `seal_report` under the key the
document's next blob would go out under and opened by `DocKeys::open_report`. Its associated
data is the document's blob AAD with `/report` after it, so a report never opens as a log entry
nor an entry as a report (a deliberate difference from "sealed exactly like an append"). A
document this device has no key to seal under is not reported. `missing` is said in wave 1 and
acted on in wave 2.

**Comparing** (`pimble_rpc::vault::Comparing`): where this device's vector exceeds another
device's report, it appends the diff from that report (`doc_diff`/`append_blob` on the desktop,
`append_prepared` in the browser); never for its own report, never on a reader's replica. Where
neither vector exceeds the other and the delete digests differ, both devices append the diff from
the other's vector: it carries no structs and the whole delete set, so each merge leaves the union,
one exchange each way (a report without a digest is compared by vector alone). A (document,
device) pair answered once is not answered again until that device reports a different vector or
digest, which is what keeps the delete exchange from ping-ponging. The record of answers is
**kept** (`devices.json`, below, and the browser's `localStorage` `pimble.vault-answered.<store>`)
and forgotten only with the logs (a new epoch, or the store unlinked): an answer is an acknowledged append, in the log for good, and without the record a
removed replica's last report would be answered again by every device at every connect, for
every document changed since.

**The desktop's link** (`crates/pimble-server/src/vault_link.rs`): a device id per store and per
computer, never the account id, kept with the record of answers **outside the store's folder**, in
`<data dir>/pimble/devices.json` (beside the replicas directory; `crate::devices`), a map of store
id to `{ device, answered }`, made on first use. Not in the store's folder because stores often
live in Dropbox: two computers syncing one `.pimble` folder would share the id, be one device to
the reports, and ignore each other (the coordinator, 2026-11-19).

`vault-link.json` itself (`pushed_sv`, `dirty`, the epoch) and `sync.json`'s `last_seq` cursors
are still in the store's folder, and that is a hazard of its own for two computers syncing one
folder: each would read the other's belief about the remote and its read positions. Reports
make a wrong `pushed_sv` heal, but the cursors are not covered. Not moved in this change.

After the connect's reconcile (`compare_at_connect`): `vaultReports` for the documents held here
and on the remote, in batches of 128, each answered; then a report of every such document
whose report the remote does not already hold as it is, 32 in flight at a time. Live: every
local change notification for a document (made here or applied from the remote) makes its
report due 5 s later, debounced per document; a `VaultReported` from another device is answered
at once. `pushed_sv` stays as the hint it was. `ServerConfig::vault_reports` (default `true`)
turns reporting and comparing off, for the test that shows the gap without them.

**The browser** (`web/src/vault.rs`, `web/src/backend.rs` step 3e): the same, per store: device id
in `localStorage` `pimble.vault-device` (one slot per browser, not per load), reports and
comparing at every open and connect (`compare_reports`), reports due 5 s after a merge or a
local append, `VaultReported` queued by `pump` and answered by `exchange_reports` in the backend
loop.

**Tests**:

- `crates/pimble-store/src/vault.rs` `state_reports_are_one_slot_per_device_beside_the_log`.
- `crates/pimble-rpc/src/vault.rs` `a_report_opens_as_a_report_and_never_as_a_blob`,
  `comparing_answers_what_a_report_lacks_once_per_report`,
  `a_delete_one_device_lacks_is_compared_by_its_digest`,
  `the_delete_digest_does_not_depend_on_how_deletes_were_made`.
- `crates/pimble-server/tests/vault.rs` `a_report_is_one_slot_per_device_and_reaches_only_who_asked_for_reports`
  (round trip, overwrite, notification only to who asked, size limit, refusals),
  `reports_stay_inside_a_members_scope_and_a_reader_reports_too`.
- `crates/pimble-server/tests/convergence.rs` (the sharing harness):
  `an_edit_a_device_wrongly_believes_it_sent_reaches_the_others_through_reports` (the
  2026-11-19 state made on purpose: a member's edit made with the link down, `vault-link.json`
  rewritten to claim it was sent, the device restarted; the owner has it within 10 s, and the
  store is quiet after), `without_reports_an_edit_a_device_wrongly_believes_it_sent_never_arrives`
  (the same with reports off: it never arrives), `a_report_heard_live_is_answered_and_a_device_without_reports_is_left_alone`
  (an older device beside new ones; a new device's report answered live),
  `two_devices_with_equal_state_append_nothing_because_of_reports` (heads on the hosted logs
  unchanged across reconnects), `a_delete_one_device_lacks_reaches_it_through_the_delete_digest`
  (a member's copy deletes the note's text on disk with an update of deletes only, equal state
  vectors; the owner has the delete within 10 s, the store is quiet after; the device id is in
  `devices.json` and nowhere in the store's folder), `without_reports_a_delete_one_device_lacks_never_arrives`.
- `web/src/vault.rs` `a_report_that_lacks_what_this_page_holds_is_answered_once`,
  `a_readers_page_reports_and_answers_nothing`, `a_report_is_due_a_debounce_after_the_last_change`,
  `a_delete_one_page_missed_is_answered_by_its_digest`.

**Known limits of wave 1**: a report that arrives just before an append the reporting
device is about to receive can earn a duplicate append (it merges to nothing); reports of a
device that is gone stay on the server until 32 newer devices push them out (answered once,
then never again); a delete difference between devices whose vectors also differ the other way
(each holds structs the other lacks) is settled by the struct exchange, whose diffs carry the
delete sets too.

## Built: wave 2 (2026-11-19, not merged or released)

### Section 3, what is waited for: met by wave 1's comparing

No request is needed. A document holding a parked update reports its state vector, and a
state vector counts only integrated structs: it stops before the first missing clock of every
client the parked update waits on (a client the reporter has never seen at all reads as clock 0,
`state_vector_exceeds`). So any device that holds the missing structs has a vector beyond that
report, and comparing makes it append the diff from it, which carries the missing structs and
everything after them; the parked update is released by their arrival. `missing` stays in the
report (wave 1) for wave 3's giving up. What no device holds integrated (the parked update's
predecessors exist nowhere) cannot be asked for by anyone, and that is section 4's and 5's case:
it is said, and in wave 3 given up. A parked update is not sent on by the device that parked it
until it is released, so a gap no device can fill stays on the devices that received it.

Test: `crates/pimble-server/tests/convergence.rs`
`an_update_parked_behind_a_gap_is_filled_by_the_device_that_holds_it` (Bob's change never left
his device while his link believed it had; a change built on it went out live and waits, parked,
on Alice's device; her second device's report asks, and Bob, hearing it live, sends; both of
Alice's devices release it).

### Section 4, out of step, said

**Definition** (the coordinator's, 2026-11-19, refined where noted). A document is out of step on
a device when, for at least five minutes during which that device's link (or page) was connected,
one of these has held without a break, judged in this order
(`pimble_rpc::vault::out_of_step_reason`, the same function on the desktop and in the browser):

1. it holds an update parked behind a gap (`NodeDoc::missing`): "Waits for earlier changes
   another device has not sent.";
2. another device's latest report from the last 30 days holds structs this device lacks: "Will's
   device holds changes this one lacks." (Will's device, Your other device, or Another device);
3. this device holds something another device's report lacks (structs, or deletes by the
   digest), has not answered that report (no acknowledged append on record), and the remote is not
   known to hold it (`pushed_sv`, or the document is marked after a refused send): "This device
   holds changes Will's device lacks and could not send them."

Refined from "the reports differ": what this device has sent, or knows the remote holds, and
another device has not taken yet is that device's to take when it is back (it may be off for a
week), and does not count; otherwise every note edited while one device is away would be out of
step. A reader's surplus never counts: a reader sends nothing. A difference first seen at a
judgment has lasted no time; the time since the previous judgment, while connected, is what adds
up. It clears at the first judgment where none holds. Judged every fifth of the window (30 s for
five minutes) on the desktop and every 30 s in the browser, only while connected.

**Naming devices.** The server stamps each stored report with the sending account as it knew the
caller (`VaultReport::account`, `VaultReported { account }`, the `reports` file's `account`; `None`
for the server's operator). The desktop names it with the store's people list
(`crate::people::cached`, `StorePeople::name_of`): "<name>'s device", "Your other device" for the
signed-in account, "Another device" when no name is known. The browser holds no people list: it
names this person's other devices and calls everyone else's another device.

**Log.** A `warn` line when a document becomes out of step ("out of step since <time>: <reason>")
and an `info` line when it is in step again; diagnostics carry both.

**Server and RPC.** `pimble_core::OutOfStep { node_id, since, reason }`; `Store::out_of_step`
and `GetStoreSyncResponse::out_of_step` (serde default, skipped when empty), filled from the
vault link (`VaultLinkHandle::out_of_step`, kept in the link's `Waiting` beside what waits). A
change to the list is announced exactly as a change to `waiting` is: the link's state again
(`SyncStateChanged`), on which the app asks `getStoreSync`. `ServerConfig::out_of_step_after`
(default five minutes) is the window; tests shorten it.

**App** (`crates/pimble-app`, desktop and browser alike: both feed `StoreSyncChanged.out_of_step`
into `AppStore::out_of_step`; `src/out_of_step.rs` holds the words):
- the note's row in the explorer: a small warning mark after its label, the sentence as its
  tooltip; the row's `TreeNodeData` label carries it (`out_of_step::row_snapshot`) and a change to
  the list rebuilds the rows;
- the pane showing the note (text, canvas, sheet, discussion): one line at its top, "Out of step
  since 19 Nov 2026, 14:05: <reason>";
- Properties: the same line under the access note;
- never the store row.

**MCP**: `get_node` adds `sync: Out of step since 2026-11-19 14:05 UTC: <reason>`.

**Browser** (`web/src/vault.rs`): the same judgement over the reports it hears (`reports_heard`)
and its documents' parked state, `VaultStore::judge_steps`, driven by the backend loop's step 3e
(`steps_due`, `judge_steps`, `pause_steps` while the endpoint is down); the list rides the store's
sync answer (`sync_event`) like what waits.

**Tests**:
- `crates/pimble-rpc/src/vault.rs` `out_of_step_reasons_name_whose_device_and_why`.
- `crates/pimble-store/src/vault.rs` (a report keeps its sender's account),
  `crates/pimble-server/tests/vault.rs` (a report names the caller's account; `None` for the
  operator).
- `crates/pimble-server/tests/convergence.rs`
  `a_gap_no_device_can_fill_makes_the_note_out_of_step_on_both_until_it_is_filled`: both devices
  get an update built on a change nobody holds; after the (test) window of 2 s both list the note
  out of step with the parked reason and a `since` from when it began, the app's subscription hears
  the change, and once the missing change arrives on one device both are in step again.
- `crates/pimble-app/src/out_of_step.rs` (the pane's and the row's words, the row label).
- `crates/pimble-mcp/src/tools.rs` `get_node_says_since_when_and_why`.
- `web/src/vault.rs` `a_page_says_a_note_out_of_step_after_the_window_and_clears_it`,
  `a_report_beyond_this_page_names_whose_device_holds_more`.

**Known limits of wave 2**: a device whose own report and another's differ only in deletes that
this device sent (answered) waits for that device to take them, without saying so; the browser
cannot name other people's devices; the window counts this device's connected time, not the
other device's.

## Next: one sync engine for both apps (Joe, 2026-11-19: after the store-folder work)

The desktop's encrypted link (`crates/pimble-server/src/vault_link.rs` and `share.rs`, about
4,500 lines) and the browser's vault client (`web/src/vault.rs`, about 6,500) do the same
job twice, and every change to how syncing works has been built twice and has drifted
(wave 2 shipped with the browser unable to name devices it could have named). The shared
rules in `pimble_rpc::vault` (comparing, out of step, sealing) are about 1,700 lines.

The plan: one sync core, free of I/O (what to read, send, report, compare, retry, and when),
in a crate both apps use, with each app supplying only its transport (the RPC client) and
its storage (store folders on the desktop, the page's memory and `localStorage` in the
browser). Wave 3's randomized fault test then runs the one engine both apps ship, which is
what makes the guarantee hold in the browser as much as on the desktop. Sequenced after
docs/STORE_FOLDERS_CONTRACT.md, which changes where the desktop keeps the link's state.

## When "out of step" is shown (Joe, 2026-11-20)

A mark on a note means something there needs someone, and it says what. Of the three
reasons (wave 2):

1. **Another device holds changes this one lacks.** Shown, with the step: "Will's device
   holds changes this one doesn't have yet. They'll arrive when Will opens Pimble."
2. **This device could not send its changes.** Shown, with the step (sign in again, get
   online, whatever stopped the send).
3. **It waits for earlier changes no device has sent.** Not shown on the note: nobody can
   do anything about it. Logged (diagnostics see it), and a quiet line in Properties ("Some
   changes from Will's device on Oct 26 never arrived"). If any device holds them, wave 1's
   comparing sends them by itself; if none does, after 24 hours the waiting part is set
   aside (section 5) and that line says what was set aside.

Found on "Shared Pimble Notes": a big warning for Will's arrows of 2026-10-26, on a canvas
the note no longer shows, that no device holds. Built in wave 3.
