// The alpha diagnostics server: takes Pimble's record batches and its
// "Send Diagnostics..." reports into RhypeDB. The contract is
// docs/DIAGNOSTICS_CONTRACT.md in the Pimble repo; the endpoints are listed
// in README.md.
//
// Environment:
//   SEND_KEY     the key every build sends with (required); it only adds
//   READ_KEY     the key for reading back (stats, export, reports); never in
//                a build, so a key pulled out of one cannot read anything
//   PORT         default 8080
//   RHYPEDB_URL  default http://127.0.0.1:4200

import { createServer } from "node:http";
import { timingSafeEqual } from "node:crypto";

import { createMany, query, obj, rows, scan, str } from "./lib/db.js";
import * as records from "./lib/records.js";
import * as reports from "./lib/reports.js";
import { exportLines } from "./lib/export.js";
import { Limiter, count } from "./lib/installs.js";

const KEY = process.env.SEND_KEY ?? "";
const READ_KEY = process.env.READ_KEY ?? "";
if (!KEY) console.error("SEND_KEY is not set; every upload will be refused");
if (!READ_KEY) console.error("READ_KEY is not set; every read will be refused");

const limiter = new Limiter(60);

function send(res, status, body, headers = {}) {
  const text = typeof body === "string" ? body : JSON.stringify(body);
  res.writeHead(status, { "content-type": typeof body === "string" ? "text/plain" : "application/json", ...headers });
  res.end(text);
}

function holds(req, key) {
  const given = Buffer.from(req.headers.authorization ?? "");
  const want = Buffer.from(`Bearer ${key}`);
  return key !== "" && given.length === want.length && timingSafeEqual(given, want);
}

/// The builds' key: it may send.
const sender = (req) => holds(req, KEY);
/// The read key: it may read back.
const reader = (req) => holds(req, READ_KEY);

/// The request body, or null past `max` bytes.
function body(req, max) {
  return new Promise((resolve, reject) => {
    const parts = [];
    let size = 0;
    req.on("data", (chunk) => {
      size += chunk.length;
      if (size > max) {
        resolve(null);
        req.destroy();
        return;
      }
      parts.push(chunk);
    });
    req.on("end", () => resolve(Buffer.concat(parts)));
    req.on("error", reject);
  });
}

/// The body unpacked, or null once an answer has been sent.
async function unpacked(req, res, maxCompressed, read) {
  if (Number(req.headers["content-length"] ?? 0) > maxCompressed) return send(res, 413, { error: "too large" }), null;
  const raw = await body(req, maxCompressed);
  if (raw === null) return send(res, 413, { error: "too large" }), null;
  try {
    return read(raw, /gzip/i.test(req.headers["content-encoding"] ?? ""));
  } catch (e) {
    if (e instanceof records.BadBatch) return send(res, e.message === "too large" ? 413 : 400, { error: e.message }), null;
    throw e;
  }
}

async function postRecords(req, res) {
  const lines = await unpacked(req, res, records.MAX_COMPRESSED, records.unpack);
  if (!lines) return;
  const received = new Date().toISOString();
  const got = lines.map((l) => records.toRecord(l, received)).filter(Boolean);
  if (!got.length) return send(res, 400, { error: "no records" });
  const install = got[0].install;
  const wait = limiter.take(install);
  if (wait) return send(res, 429, { error: "slow down" }, { "retry-after": String(wait) });
  await createMany("Record", got);
  // A batch is one install's, but count each install's own lines anyway.
  const per = new Map();
  for (const r of got) {
    const seen = per.get(r.install) ?? { records: 0 };
    per.set(r.install, { app: r.app, os: r.os, account: r.account ?? seen.account, records: seen.records + 1 });
  }
  for (const [id, seen] of per) await count(id, seen);
  send(res, 200, { accepted: got.length, rejected: lines.length - got.length });
}

async function postReport(req, res) {
  const text = await unpacked(req, res, reports.MAX_COMPRESSED, (raw, gz) => records.inflate(raw, gz, reports.MAX_UNCOMPRESSED));
  if (text === null) return;
  const checked = reports.toReport(text);
  if (typeof checked === "string") return send(res, 400, { error: checked });
  const { report, logs } = checked;
  const wait = limiter.take(report.install);
  if (wait) return send(res, 429, { error: "slow down" }, { "retry-after": String(wait) });
  if (JSON.stringify(report).length > reports.MAX_BODY) return send(res, 413, { error: "stores and note too large" });
  const received = new Date().toISOString();
  const { install, account, session, app, os, ts, note } = report;
  // 40 random bits: a clash is rare, and `code` is unique, so try again.
  let code;
  for (let attempt = 0; ; attempt++) {
    code = reports.newCode();
    const [held] = await rows(`Report.filter(.code == ${str(code)}).limit(1)`);
    if (!held) break;
    if (attempt > 5) throw new Error("no free report code");
  }
  const parts = [];
  logs.forEach((t, log) => reports.cut(t).forEach((text, n) => parts.push({ code, log, n, text })));
  // The parts first: a report is listed only once all of it is stored.
  await createMany("ReportPart", parts);
  await query(`Report.create(${obj({ code, install, account, session, app, os, ts, received, note, body: report })})`);
  await count(install, { app, os, account, reports: 1 });
  send(res, 200, { report: code });
}

async function stats(res) {
  const installs = [];
  for await (const r of scan("Install")) {
    const { id: _, ...fields } = r;
    installs.push(fields);
  }
  installs.sort((a, b) => Date.parse(b.last_seen) - Date.parse(a.last_seen));
  send(res, 200, {
    installs,
    records: installs.reduce((n, i) => n + (i.records ?? 0), 0),
    reports: installs.reduce((n, i) => n + (i.reports ?? 0), 0),
  });
}

async function listReports(res) {
  const list = [];
  for await (const r of scan("Report")) {
    const { code, install, account, app, ts, received, note } = r;
    list.push({ code, install, account, app, ts, received, note });
  }
  list.sort((a, b) => Date.parse(b.received) - Date.parse(a.received));
  send(res, 200, { reports: list });
}

async function getReport(res, given) {
  const code = reports.normalizeCode(given);
  if (!code) return send(res, 400, { error: "not a report code" });
  const [r] = await rows(`Report.filter(.code == ${str(code)}).limit(1)`);
  if (!r) return send(res, 404, { error: "no such report" });
  const texts = r.body.logs.map(() => []);
  for await (const p of scan(`ReportPart.filter(.code == ${str(code)})`)) {
    if (texts[p.log]) texts[p.log][p.n] = p.text;
  }
  const whole = {
    code,
    received: r.received,
    ...r.body,
    logs: r.body.logs.map((l, i) => ({ name: l.name, text: texts[i].join("") })),
  };
  send(res, 200, whole);
}

async function handle(req, res) {
  const url = new URL(req.url, "http://x");
  const path = url.pathname;
  if (req.method === "GET" && (path === "/v1/health" || path === "/health")) {
    await rows("Install.limit(1)");
    return send(res, 200, "ok");
  }
  if (!path.startsWith("/v1/")) return send(res, 404, { error: "not found" });
  if (req.method === "GET") return read(req, res, url);
  if (!sender(req)) return send(res, 401, { error: "key" });
  if (req.method === "POST" && path === "/v1/records") return postRecords(req, res);
  if (req.method === "POST" && path === "/v1/reports") return postReport(req, res);
  send(res, 404, { error: "not found" });
}

/// The read endpoints, with READ_KEY.
async function read(req, res, url) {
  if (!reader(req)) return send(res, 401, { error: "key" });
  const path = url.pathname;
  if (path === "/v1/stats") return stats(res);
  if (path === "/v1/reports") return listReports(res);
  const one = path.match(/^\/v1\/reports\/([^/]+)$/);
  if (one) return getReport(res, decodeURIComponent(one[1]));
  if (path === "/v1/export") {
    const q = (k) => url.searchParams.get(k) || undefined;
    const filters = { install: q("install"), account: q("account"), kind: q("kind"), since: q("since"), until: q("until") };
    for (const k of ["since", "until"]) {
      if (filters[k] && Number.isNaN(Date.parse(filters[k]))) return send(res, 400, { error: `${k}: a date` });
    }
    if (filters.install && !records.ID.test(filters.install)) return send(res, 400, { error: "install: an id" });
    if (filters.account) {
      filters.account = records.accountOf(filters.account);
      if (!filters.account) return send(res, 400, { error: "account: a UUID" });
    }
    if (filters.kind && !records.KINDS.has(filters.kind)) return send(res, 400, { error: `kind: one of ${[...records.KINDS].join(", ")}` });
    // The export reads everything before its first line, so a database
    // error surfaces here, before the 200: never an empty "success".
    const lines = exportLines(filters);
    const first = await lines.next();
    res.writeHead(200, { "content-type": "application/x-ndjson" });
    if (!first.done) {
      res.write(`${first.value}\n`);
      for await (const line of lines) {
        if (!res.write(`${line}\n`)) await new Promise((r) => res.once("drain", r));
      }
    }
    return res.end();
  }
  send(res, 404, { error: "not found" });
}

const server = createServer((req, res) => {
  handle(req, res).catch((e) => {
    console.error(`${req.method} ${req.url}: ${e.stack ?? e}`);
    if (!res.headersSent) send(res, 503, { error: "try later" });
    // Cut short, not ended: a clean end would pass a partial export as whole.
    else res.destroy();
  });
});

const port = Number(process.env.PORT || 8080);
server.listen(port, () => console.log(`diagnostics listening on ${port}`));
