# Diagnostics for alpha testers (draft, 2026-11-19)

Status: **decisions answered by Joe 2026-11-19** (section 7). All three waves built 2026-11-19 (section 8), the server deployed.

Why: the "Shared with Will" faults of 2026-11-19 took hours to diagnose from one side only.
Joe's journal showed the symptoms (a repair storm, edits held back), and the cause was on
Will's Windows machine, whose log nobody could read. A tester's Pimble should send what we
need to see what went wrong, without sending what they wrote. beardictate does this for its
alpha (`../beardictate/docs/telemetry.md`); this follows its shape and differs where Pimble
does: its testers' data is private and end-to-end encrypted, and "nothing is hosted unless
the person asked for it" (CLAUDE.md) applies to diagnostics as much as to stores.

## 1. A log file on every platform, always

Today the desktop log goes to standard error on Linux (the journal, when a desktop file
starts it) and to `%LOCALAPPDATA%\pimble\logs\pimble.log` on Windows only; a server run by
`pimble-mcp` writes `<data dir>/pimble/logs/server.log` (0.28.2). From this wave every
platform writes `<data dir>/pimble/logs/pimble.log` (plus standard error), the previous
run's kept as `pimble.previous.log`, capped at 20 MB a file. This is local and needs no
decision; everything below reads from the same records.

## 2. What is sent: records, scrubbed at the source

One record per log line at `warn` and `error` from every target, and the `info` lines of the
targets that tell how syncing and sharing go (`pimble_server::{vault_link, sync_link,
share, relay_face, relay_tunnel, handler}`, `pimble_server::local`, `pimble_app::events`'s
sync and connection lines, `pimble_app::agents`: what setting an agent up in another app wrote,
by path and agent id, docs/AGENTS_CONTRACT.md section 6). Plus a few structured events:

| `kind` | When | `data` |
|---|---|---|
| `system` | start | version, OS and version, arch, CPU, cores, RAM, renderer (GPU or CPU, adapter name), whether the semantic model loaded, host kind (`app`/`mcp`) |
| `log` | each line above | `level`, `target`, `message` (scrubbed, below); the same message more than 3 times is counted, as beardictate does (3, then the 10th, 100th, ...) |
| `stores` | start, and when one opens or closes | per open store: its id, name, path (`~`), kind (plain/vault), tier (local/hosted/relay/replica), role, sync state, how many documents, how many waiting, scope roots |
| `panic` | a panic on any thread | thread, location, backtrace (first 6000 characters), sent at once |
| `previous_run` | start, when the last run did not quit | that run's session id |
| `report` | Help > "Send Diagnostics..." (section 4) | the person's note, if they write one |

**Scrubbing** happens before a record is written to the queue, never on the server: a
record that holds something it should not is a leak whether or not anyone reads it.
- File paths stay, with the home folder written `~` (`~/Dropbox/Personal.pimble`), in
  every form a line can hold it: on Windows either slash, any case, a network (`\\server`)
  home too, and every backslash doubled as Rust's `{:?}` prints a path (until 2026-11-26 `C:\\Users\\<name>` got through): where a
  store lives (a Dropbox or OneDrive folder, a network drive) is often the cause.
- Store names stay (`'Personal'`): they make a record readable. Note titles do not: a log
  line that names a note writes its id, and one that quotes a title is a bug in that line,
  to fix there (the scrubber replaces a title it is told about with `'…'`).
- Email addresses become `<email>` (the account id says who); tokens, keys and bearer
  strings `<secret>` (by shape).
- Ids stay: store, node, client and account ids are random UUIDs and say nothing about what
  a person wrote, and they are the only way to line up two devices of one share (the
  `client 5596681811603945` that held back Will's edits).
- Never sent, whatever a line holds: note text, note titles, tags, field values, search
  queries, email addresses, passwords, keys.

A test feeds the scrubber every log line the code base can produce (a list kept beside
it) and fails on a home folder, a title or an address left in.

## 3. Transport and server

As beardictate's: records appended to `<data dir>/pimble/diagnostics/records-<n>.ndjson`,
sent gzip NDJSON every minute to `POST /v1/records`, each file deleted once acknowledged;
at most 20 MB waiting, the oldest dropped first; the folder deleted when diagnostics are
turned off. Envelope: `v`, `install` (a random id per install), `session` (per run),
`seq`, `ts`, `app` (version), `os`, `kind`, `data`, and `account` (the account's id, a
UUID, when signed in; see decision 2).

The server: a jkbase project of its own, `pimble-diagnostics`, reusing beardictate's
dependency-free Node server and RhypeDB schema (no audio, no object store), with a send-only
key compiled into builds and a separate read key that never leaves Joe's machine
(`~/.config/pimble-release/diagnostics-read-key`). `tools/diagnostics.sh stats | export
--install <id> | --account <id> | --since <date>` reads it back, and an export lines up two
installs' records by time so both ends of a share read as one story.

## 4. "Send Diagnostics..." (Help menu, both apps)

A sentence of what goes (the last two runs' log files, scrubbed as above, and the open
stores' shape), a box for a note ("my edits don't reach Joe"), and Send. It sends even with
continuous diagnostics off, because pressing it is asking. The answer is a short code
(the report's id) to tell us. This is also the one way to get a whole log rather than its
warnings.

## 5. The browser

The web app sends the same records from the page (its console log through the same
scrubber, the same `stores` event, panics through `console_error_panic_hook`), queued in
IndexedDB, under the same switch; "Send Diagnostics..." is in its Help menu too (the web
must match the desktop).

## 6. What testers are told

In the installer's first page (Windows), the download page, and once in the app at first
start, a notice like beardictate's: what is sent (warnings, errors, how syncing goes,
the computer's kind, your stores' names and where they are on disk), what is never sent
(anything you wrote in them, your notes' titles, your email), that it is tied to a random install number (and to the account id
when signed in, if decision 2 says so), the switch that turns it off, and "Send
Diagnostics..." for when something goes wrong.

## 7. Decisions (Joe, 2026-11-19: every recommendation below, and paths and store names kept)

1. **On by default for alpha builds**, with the notice and a switch to turn it off (as
   beardictate), or **off until a tester turns it on**? Recommended: on, with the notice;
   off by default would leave us blind exactly where it matters, on the other person's
   machine. Either way "Send Diagnostics..." always works when pressed.
2. **Tie records to the account id** (a UUID, never the email) when signed in, so a
   tester's devices and both ends of a share line up? Recommended: yes; without it, Will's
   records are found only by asking him for his install number.
3. **Where it runs**: its own jkbase project `pimble-diagnostics` (recommended: kept apart
   from the accounts service and its data, removable after the alpha), or a route in
   `pimble-cloud` (`/api/v1/diagnostics`, one deploy fewer).
4. **Which builds**: every build for now, as beardictate (recommended, while every
   tester is an alpha tester), or a separate alpha channel.

## 8. Waves

1. The log file everywhere (section 1) and the scrubber with its test. Local only.
   **Built 2026-11-19**: `pimble_server::logs` (`open`, `LogWriter`; the app's `pimble.log`
   and an MCP-run server's `server.log`, each `.previous.log` and, past 20 MB, `.older.log`),
   `pimble_core::diagnostics::Scrubber` (home to `~`, emails, secrets by shape, known titles),
   `crates/pimble-core/tests/fixtures/log_lines.corpus` with `tests/scrub_corpus.rs`. The four
   log lines that printed a note's title name it by id now.
2. The queue, the uploader, `system`/`log`/`stores`/`panic`/`previous_run`, the server,
   `tools/diagnostics.sh`; the notice and the switch.
   **Built 2026-11-19**: `pimble_server::diagnostics` (the `Layer` in the app's and
   `pimble-mcp`'s subscribers, the queue under `<data dir>/pimble/diagnostics/`, shared by
   every program of the computer: a file is renamed `sending-<session>-...` by the one that
   sends it, and one left that way ten minutes goes back; a `running-<session>` file per run
   for `previous_run`, skipped while its process lives); the account id from the keystore;
   `stores` three seconds after a store opens or closes; Settings > Diagnostics (desktop),
   opened once at the first start that sends, as the notice; the notice on the download page
   and as the Windows Setup's first page (`tools/windows/alpha-notice.txt`). The server is
   `diagnostics/` (jkbase project `pimble-diagnostics`, deployed; its README), with reports
   (`POST /v1/reports`, an 8-character code) ready for wave 3. Builds carry the send key
   from the repository secret `PIMBLE_DIAGNOSTICS_KEY` (release.yml); a build without it,
   any local one, sends nothing. Keys: `~/.config/pimble-release/diagnostics-send-key` and
   `diagnostics-read-key` on Joe's machine. Verified end to end against `diagnostics/dev.sh`
   (system, log, stores and previous_run records; the home folder written `~`). No `report`
   record kind: reports have their endpoint.
3. "Send Diagnostics..." in both apps; the browser's records.
   **Built 2026-11-19**: Help > "Send Diagnostics..." (`pimble_app::diagnostics_ui`, both
   apps; `BackendCommand::SendDiagnostics`, `BackendEvent::DiagnosticsSent`): a note and
   Send, answered with the report's code. The desktop sends the last 8 MB of each of
   `pimble.log`, `pimble.previous.log`, `server.log` and `server.previous.log`, scrubbed, from
   its own process (`pimble_server::diagnostics::send_report`), whether or not records are
   on. The browser (`web/src/diagnostics.rs`) records as the desktop does, queued in
   `localStorage` (1 MB), keeps its last 3,000 log lines for a report, and sends both through
   the accounts service (`POST /api/v1/diagnostics/records` and `/reports`, session
   required), which adds the send key (jkbase secret `PIMBLE_DIAGNOSTICS_KEY` on `pimble`)
   and the account (`X-Pimble-Account`); no page carries the key. Records go only as the
   signed-in account, by the rule the worker's queue follows too
   (`pimble_core::diagnostics::may_send`/`sendable`): signed out, nothing is sent and they
   wait; an account's records go only as that account, no one's as whoever signs in next.
   The page names that account in `X-Pimble-Account`, and the service refuses a cookie of
   another (409 `account_changed`; another tab signed in again), so the lines wait; a sender
   without the header (the worker, its bearer the session it chose) is not asked. Settings > Diagnostics is
   in both apps (`diagnostics_ui::Hooks`). Tests: `report_round_trip` (ignored; against
   `dev.sh`, run once), `diagnostics_from_a_page_go_on_with_the_key_and_the_account`,
   `diagnostics_named_for_another_account_than_the_cookies_are_refused`.
