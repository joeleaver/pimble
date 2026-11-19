// The server end to end, against a real RhypeDB (RHYPEDB_BIN, default the
// sibling checkout's release build, then its debug build). Skipped when
// there's no RhypeDB binary.

import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir, homedir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { gzipSync } from "node:zlib";
import { createServer } from "node:net";

const here = dirname(fileURLToPath(import.meta.url));
const BIN =
  process.env.RHYPEDB_BIN ||
  [join(homedir(), "dev/rhypedb/target/release/rhypedb-server"), join(homedir(), "dev/rhypedb/target/debug/rhypedb-server")].find(existsSync) ||
  join(homedir(), "dev/rhypedb/target/release/rhypedb-server");
const KEY = "test-key";
const READ = "read-key";
const skip = !existsSync(BIN) && `no RhypeDB at ${BIN}`;

let tmp, db, server, base;

function freePort() {
  return new Promise((resolve) => {
    const s = createServer().listen(0, "127.0.0.1", () => {
      const { port } = s.address();
      s.close(() => resolve(port));
    });
  });
}

async function until(url) {
  for (let i = 0; i < 100; i++) {
    try {
      const r = await fetch(url);
      if (r.ok) return;
    } catch {}
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`${url} never came up`);
}

before(async () => {
  if (skip) return;
  tmp = mkdtempSync(join(tmpdir(), "pimble-diagnostics-"));
  const [dbPort, tcpPort, port] = [await freePort(), await freePort(), await freePort()];
  db = spawn(BIN, ["--schema", join(here, "..", "schema.rhype"), "--data-dir", join(tmp, "db"), "--listen", `127.0.0.1:${dbPort}`, "--tcp-listen", `127.0.0.1:${tcpPort}`], { stdio: ["ignore", "ignore", "inherit"] });
  await until(`http://127.0.0.1:${dbPort}/health`);
  server = spawn(process.execPath, [join(here, "..", "server.js")], {
    env: { ...process.env, PORT: String(port), SEND_KEY: KEY, READ_KEY: READ, RHYPEDB_URL: `http://127.0.0.1:${dbPort}` },
    stdio: ["ignore", "ignore", "inherit"],
  });
  base = `http://127.0.0.1:${port}`;
  await until(`${base}/v1/health`);
});

const exited = (child) => (child && child.exitCode === null ? new Promise((r) => child.once("exit", r)) : Promise.resolve());

after(async () => {
  server?.kill();
  db?.kill();
  await Promise.all([exited(server), exited(db)]);
  if (tmp) rmSync(tmp, { recursive: true, force: true });
});

const will = { install: "7f3a9c0e5b1d4f2a8c6e0b9d7a5f3e1c", session: "a1b2c3d4e5f60718293a4b5c6d7e8f90", account: "6f1c2e9a-3b4d-4e5f-8a7b-9c0d1e2f3a4b" };
const joe = { install: "0e1d2c3b4a5968778695a4b3c2d1e0f1", session: "f0e1d2c3b4a5968778695a4b3c2d1e0f", account: "11111111-2222-4333-8444-555555555555" };
const auth = { authorization: `Bearer ${KEY}` };
const readAuth = { authorization: `Bearer ${READ}` };

function line(who, seq, ts, kind, data) {
  return JSON.stringify({ v: 1, install: who.install, session: who.session, seq, ts, app: "0.29.0", os: "windows 11", kind, account: who.account, data });
}
const log = (message, level = "warn") => ({ level, target: "pimble_server::vault_link", message });

const post = (path, body, headers = {}) => fetch(`${base}${path}`, { method: "POST", headers: { ...auth, "content-encoding": "gzip", ...headers }, body });
async function exported(q = "") {
  const r = await fetch(`${base}/v1/export${q}`, { headers: readAuth });
  assert.equal(r.status, 200);
  return (await r.text()).split("\n").filter(Boolean).map((l) => JSON.parse(l));
}

async function dbQuery(q) {
  const port = db.spawnargs[db.spawnargs.indexOf("--listen") + 1];
  const r = await fetch(`http://${port}/query`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ query: q }) });
  return r.json();
}

test("records are stored and exported in time order, a bad line rejected, repeats dropped", { skip }, async () => {
  const awkward = 'held back "3" edits \\ from\nclient 5596681811603945 ☃';
  const willLines = [
    line(will, 0, "2026-11-18T10:00:00.000Z", "system", { version: "0.29.0", host: "app" }),
    line(will, 1, "2026-11-18T10:00:02.000Z", "log", log(awkward)),
    line(will, 2, "2026-11-19T09:00:00.000Z", "stores", { stores: [{ id: "s1", name: "Personal", path: "~/Dropbox/Personal.pimble" }] }),
    "not json",
    line(will, 3, "2026-11-19T09:00:01.000Z", "report", {}),
  ];
  let r = await post("/v1/records", gzipSync(willLines.join("\n")), { "content-type": "application/x-ndjson" });
  assert.equal(r.status, 200);
  assert.deepEqual(await r.json(), { accepted: 3, rejected: 2 });
  // The other end of the share, in between; and a retry of one of Will's.
  const joeLines = [line(joe, 0, "2026-11-18T10:00:01.000Z", "log", log("repair storm", "INFO")), line(will, 1, "2026-11-18T10:00:02.000Z", "log", log(awkward))];
  r = await post("/v1/records", gzipSync(joeLines.join("\n")));
  assert.equal(r.status, 200);

  const all = await exported();
  assert.deepEqual(all.map((l) => [l.install === will.install ? "will" : "joe", l.seq]), [["will", 0], ["joe", 0], ["will", 1], ["will", 2]]);
  const msg = all.find((l) => l.kind === "log" && l.install === will.install);
  assert.equal(msg.data.message, awkward);
  assert.equal(msg.account, will.account);
  assert.equal(msg.app, "0.29.0");
  assert.ok(msg.received);
  assert.equal(all.find((l) => l.install === joe.install).data.level, "info");
  const search = await dbQuery(`Record.matches(.text, "storm", k: 5)`);
  assert.equal(search.objects.length, 1);
});

test("the export filters by install, account, kind, since and until", { skip }, async () => {
  assert.deepEqual((await exported(`?account=${will.account}`)).map((l) => l.seq), [0, 1, 2]);
  assert.deepEqual((await exported(`?account=${will.account.toUpperCase()}`)).length, 3);
  assert.deepEqual((await exported(`?install=${joe.install}`)).map((l) => l.kind), ["log"]);
  assert.deepEqual((await exported(`?kind=log`)).length, 2);
  assert.deepEqual((await exported(`?account=${will.account}&since=2026-11-19`)).map((l) => l.kind), ["stores"]);
  assert.deepEqual((await exported(`?since=2026-11-18T10:00:01Z&until=2026-11-18T10:00:02Z`)).map((l) => l.install), [joe.install]);
  assert.deepEqual((await exported(`?until=2026-11-19`)).length, 3);
  assert.equal((await exported(`?since=2099-01-01`)).length, 0);
  for (const q of ["since=nonsense", "until=nonsense", "account=joe%40example.com", "kind=cleaned", "install=NOT%20HEX"]) {
    assert.equal((await fetch(`${base}/v1/export?${q}`, { headers: readAuth })).status, 400, q);
  }
});

test("the keys, the size and the body are checked", { skip }, async () => {
  const ok = gzipSync(line(will, 9, "2026-11-19T09:10:00Z", "state", { state: "idle" }));
  assert.equal((await post("/v1/records", ok, { authorization: "Bearer nope" })).status, 401);
  assert.equal((await post("/v1/records", ok, readAuth)).status, 401, "the read key cannot send");
  assert.equal((await post("/v1/reports", ok, readAuth)).status, 401, "the read key cannot send a report");
  for (const path of ["/v1/stats", "/v1/export", "/v1/reports", "/v1/reports/00000000"]) {
    assert.equal((await fetch(`${base}${path}`, { headers: auth })).status, 401, `the send key cannot read ${path}`);
    assert.equal((await fetch(`${base}${path}`)).status, 401, path);
  }
  assert.equal((await post("/v1/records", Buffer.from("not gzip"))).status, 400);
  assert.equal((await post("/v1/records", gzipSync("garbage\n"))).status, 400, "no line is a record");
  assert.equal((await post("/v1/records", Buffer.alloc((4 << 20) + 1), { "content-encoding": "identity" })).status, 413);
  assert.equal((await post("/v1/records", gzipSync(Buffer.alloc(21 << 20, 0x20)))).status, 413, "too large unpacked");
  assert.equal((await post("/v1/reports", Buffer.alloc((8 << 20) + 1), { "content-encoding": "identity" })).status, 413);
  assert.equal((await post("/v1/records", ok)).status, 200);
});

test("a report is stored and read back by its code", { skip }, async () => {
  // Bigger than one part, with a character that takes two UTF-16 units.
  const big = `${"warn  pimble_server::share: sweep\n".repeat(12_000)}😀 the end\n`;
  const report = {
    v: 1,
    ...will,
    app: "0.29.0",
    os: "windows 11",
    ts: "2026-11-19T09:30:00.000Z",
    note: 'my edits don\'t reach Joe "at all"',
    logs: [{ name: "pimble.log", text: big }, { name: "pimble.previous.log", text: "short\n" }],
    stores: [{ id: "s1", name: "Personal", tier: "replica", waiting: 3 }],
  };
  const r = await post("/v1/reports", gzipSync(JSON.stringify(report)), { "content-type": "application/json" });
  assert.equal(r.status, 200);
  const { report: code } = await r.json();
  assert.match(code, /^[0-9A-HJKMNP-TV-Z]{8}$/);

  const got = await fetch(`${base}/v1/reports/${code.toLowerCase().slice(0, 4)}-${code.toLowerCase().slice(4)}`, { headers: readAuth });
  assert.equal(got.status, 200);
  const whole = await got.json();
  assert.equal(whole.code, code);
  assert.equal(whole.note, report.note);
  assert.deepEqual(whole.stores, report.stores);
  assert.equal(whole.account, will.account);
  assert.equal(whole.logs.length, 2);
  assert.equal(whole.logs[0].name, "pimble.log");
  assert.equal(whole.logs[0].text, big);
  assert.equal(whole.logs[1].text, "short\n");

  const plain = await post("/v1/reports", Buffer.from(JSON.stringify({ ...report, ...joe, logs: [], note: "" })), { "content-encoding": "identity" });
  assert.equal(plain.status, 200, "a report need not be gzipped");
  const list = (await (await fetch(`${base}/v1/reports`, { headers: readAuth })).json()).reports;
  assert.equal(list.length, 2);
  const mine = list.find((l) => l.code === code);
  assert.deepEqual(
    { install: mine.install, account: mine.account, app: mine.app, note: mine.note, ts: Date.parse(mine.ts) },
    { install: will.install, account: will.account, app: "0.29.0", note: report.note, ts: Date.parse(report.ts) },
  );
  assert.equal((await fetch(`${base}/v1/reports/ZZZZZZZZ`, { headers: readAuth })).status, 404);
  assert.equal((await fetch(`${base}/v1/reports/nope`, { headers: readAuth })).status, 400);
  assert.equal((await post("/v1/reports", gzipSync(JSON.stringify({ ...report, install: "x" })))).status, 400);
});

test("stats count records and reports per install", { skip }, async () => {
  const stats = await (await fetch(`${base}/v1/stats`, { headers: readAuth })).json();
  const w = stats.installs.find((i) => i.install === will.install);
  const j = stats.installs.find((i) => i.install === joe.install);
  // Will: 3, then the retried line (stored twice, exported once), then the
  // state line; Joe: 1.
  assert.deepEqual([w.records, w.reports, w.account, w.app, w.os], [5, 1, will.account, "0.29.0", "windows 11"]);
  assert.deepEqual([j.records, j.reports, j.account], [1, 1, joe.account]);
  assert.ok(w.first_seen && w.last_seen);
  assert.equal(stats.installs[0].install, joe.install, "most recent first");
  assert.equal(stats.records, 6);
  assert.equal(stats.reports, 2);
});

test("an install is held to 60 requests a minute", { skip }, async () => {
  const other = { install: "0123456789abcdef0123456789abcdef", session: will.session };
  const body = gzipSync(line(other, 0, "2026-11-19T16:02:11Z", "state", { state: "idle" }));
  // 121: even across a minute's turn, one of the two minutes sees more than 60.
  const statuses = [];
  for (let i = 0; i < 121; i++) {
    const r = await post("/v1/records", body);
    statuses.push(r.status);
    if (r.status === 429) assert.ok(Number(r.headers.get("retry-after")) > 0);
  }
  assert.ok(statuses.slice(0, 60).every((s) => s === 200));
  assert.ok(statuses.includes(429));
});

// Last: it stops the database.
test("an export the database fails is an error, not an empty 200", { skip }, async () => {
  db.kill();
  await exited(db);
  const r = await fetch(`${base}/v1/export?install=${will.install}`, { headers: readAuth });
  assert.equal(r.status, 503, `got ${r.status} with ${(await r.text()).length} bytes`);
});
