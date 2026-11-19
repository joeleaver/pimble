# Pimble diagnostics server

Takes alpha builds' diagnostic records and their "Send Diagnostics..." reports
into RhypeDB. What is sent and why: [`docs/DIAGNOSTICS_CONTRACT.md`](../docs/DIAGNOSTICS_CONTRACT.md).
Plain Node (no packages), adapted from beardictate's telemetry server without
its audio. The jkbase project `pimble-diagnostics`, at
`https://pimble-diagnostics.jkbase.app`.

| File | |
|---|---|
| `server.js` | the HTTP server |
| `lib/` | RhypeDB queries (`db.js`), record checks (`records.js`), reports and their codes (`reports.js`), the export, install counts and the request limit |
| `schema.rhype` | `Record`, `Report`, `ReportPart`, `Install` |
| `dev.sh` | a local server with its own RhypeDB |
| `test/` | `npm test` (`node --test`): unit tests, and the server against a real RhypeDB |

## Endpoints

Sending, with `Authorization: Bearer <SEND_KEY>` (the key compiled into builds):

| Request | Body | Answers |
|---|---|---|
| `POST /v1/records` | NDJSON, one record per line, gzip with `Content-Encoding: gzip`; at most 4 MiB as sent, 20 MiB unpacked | `200 {"accepted": n, "rejected": m}`; `400` when no line is a record |
| `POST /v1/reports` | one JSON report, gzip allowed; at most 8 MiB as sent, 40 MiB unpacked | `200 {"report": "<code>"}` |
| `GET /v1/health` | | `200 ok` (no key) |

Errors: `401` (key), `413` (too big), `429` with `Retry-After` (an install may
make 60 requests a minute), `400` (unreadable), `503` (try later).

A record:

```json
{"v":1,"install":"7f3a…","session":"a1b2…","seq":17,"ts":"2026-11-19T16:02:11.482Z",
 "app":"0.29.0","os":"windows 11","kind":"log","account":"6f1c2e9a-…",
 "data":{"level":"warn","target":"pimble_server::vault_link","message":"…"}}
```

`install` and `session` are 8 to 64 characters of lowercase hex and dashes,
`seq` an integer from 0, `ts` RFC 3339 with a zone, `kind` one of `system`,
`log`, `stores`, `panic`, `previous_run`, `state`, `data` an object, `account`
a UUID or absent (stored lowercase). A `log` record's `data` has `level`
(`error`, `warn`, `info`, `debug`, `trace`, any case; stored lowercase),
`target` and `message`, all strings. A line that breaks any of this is counted
in `rejected` and dropped; the rest of its batch is kept.

A report: `{v, install, session, account?, app, os, ts, note, logs: [{name, text}], stores}`
(`note` up to 20,000 characters, at most 16 logs, `stores` any JSON object or
array). Its code is 8 characters of Crockford's base32 (no I, L, O, U); when
reading one back, case, a dash or spaces, and O for 0 or I/L for 1 don't
matter.

Reading, with `Authorization: Bearer <READ_KEY>` (never the send key):

| Request | Answers |
|---|---|
| `GET /v1/stats` | `{installs: [{install, account, app, os, first_seen, last_seen, records, reports}], records, reports}`, most recently seen first |
| `GET /v1/export?install=&account=&kind=&since=&until=` | JSONL, a record per line as sent plus `received`, by `ts` then session and `seq`, repeats (same session and `seq`) dropped; `since` inclusive, `until` exclusive, a bare date is midnight UTC |
| `GET /v1/reports` | `{reports: [{code, install, account, app, ts, received, note}]}`, most recent first |
| `GET /v1/reports/<code>` | the report as sent, with `code` and `received` |

`ts` comes back in UTC (RhypeDB normalizes it).

## Locally

```
./dev.sh          # http://127.0.0.1:8788, send key dev-key, read key dev-read-key, data in .data/
```

The tests and `dev.sh` use the RhypeDB build in `~/dev/rhypedb`
(`RHYPEDB_BIN` to change it); the server tests skip without it. To read the
local server with the reading tool:

```
echo dev-read-key > /tmp/dev-read-key
PIMBLE_DIAGNOSTICS_URL=http://127.0.0.1:8788 PIMBLE_DIAGNOSTICS_READ_KEY_FILE=/tmp/dev-read-key \
  tools/diagnostics.sh stats
```

## Deploying (first time)

From this folder, with Joe's jkbase login:

```
jkbase project create pimble-diagnostics
jkbase secret set SEND_KEY=<the key builds are built with>
jkbase secret set READ_KEY=<a different key, for reading back>
jkbase deploy
curl https://pimble-diagnostics.jkbase.app/v1/health
```

Keep the read key in `~/.config/pimble-release/diagnostics-read-key` (0600)
on Joe's machine only.

## Reading it

Agents and people alike, from the repo root:

```
tools/diagnostics.sh stats
tools/diagnostics.sh export <out.jsonl> [--install <id>] [--account <id>] [--since <date>] [--until <date>] [--kind <kind>]
tools/diagnostics.sh reports
tools/diagnostics.sh report <code> <out.json>
```

An export without `--install` lines up every install's records by time, so
`--account` for one tester's devices, or `--since` around a fault for both
ends of a share, reads as one story. Write exports outside the repo: they
name stores and where they live.
