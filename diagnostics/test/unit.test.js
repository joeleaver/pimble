import { test } from "node:test";
import assert from "node:assert/strict";
import { gzipSync } from "node:zlib";

import { lit, obj, str } from "../lib/db.js";
import { BadBatch, toRecord, unpack, words } from "../lib/records.js";
import { CODE_LENGTH, cut, newCode, normalizeCode, toReport } from "../lib/reports.js";
import { Limiter } from "../lib/installs.js";

const record = {
  v: 1,
  install: "7f3a9c0e5b1d4f2a8c6e0b9d7a5f3e1c",
  session: "a1b2c3d4e5f60718293a4b5c6d7e8f90",
  seq: 17,
  ts: "2026-11-19T16:02:11.482Z",
  app: "0.29.0",
  os: "windows 11",
  kind: "log",
  account: "6F1C2E9A-3B4D-4E5F-8A7B-9C0D1E2F3A4B",
  data: { level: "WARN", target: "pimble_server::vault_link", message: "client 5596681811603945 held back 3 edits" },
};
const line = (over = {}) => JSON.stringify({ ...record, ...over });

test("strings are escaped the way the query parser reads them", () => {
  assert.equal(str('say "hi"\\ok\n\tx\r'), '"say \\"hi\\"\\\\ok\\n\\tx\\r"');
  assert.equal(str("bell\u0007 é ☃"), '"bell é ☃"');
  assert.equal(lit(null), "null");
  assert.equal(lit({ a: ['"'] }), '{"a":["\\""]}');
  assert.equal(obj({ a: 1, b: undefined, c: "x" }), '{ a: 1, c: "x" }');
});

test("a log line becomes a record, its message copied out for search", () => {
  const r = toRecord(line(), "2026-11-19T16:03:00.000Z");
  assert.equal(r.kind, "log");
  assert.equal(r.account, "6f1c2e9a-3b4d-4e5f-8a7b-9c0d1e2f3a4b", "stored lowercase");
  assert.equal(r.data.level, "warn", "stored lowercase");
  assert.equal(r.text, record.data.message);
  assert.equal(r.received, "2026-11-19T16:03:00.000Z");
  const panic = toRecord(line({ kind: "panic", data: { thread: "main", location: "a.rs:1", message: "boom", backtrace: "..." } }), "");
  assert.equal(panic.text, "boom");
  const system = toRecord(line({ kind: "system", account: undefined, data: { version: "0.29.0", message: "not a log" } }), "");
  assert.equal(system.text, "");
  assert.equal(system.account, undefined);
  for (const kind of ["stores", "previous_run", "state"]) assert.ok(toRecord(line({ kind, data: {} }), ""), kind);
  assert.ok(toRecord(line({ account: null }), ""), "a null account is no account");
  assert.ok(toRecord(line({ ts: "2026-11-19T09:02:11-07:00" }), ""), "an offset");
});

test("bad lines are refused", () => {
  for (const bad of [
    { install: "NOT HEX" },
    { session: "" },
    { seq: -1 },
    { seq: 1.5 },
    { ts: "yesterday" },
    { ts: "2026-11-19" },
    { kind: "report" },
    { kind: "cleaned" },
    { v: 0 },
    { account: "joe@example.com" },
    { account: "6f1c2e9a3b4d4e5f8a7b9c0d1e2f3a4b" },
    { account: 7 },
    { data: null },
    { data: "text" },
    { data: [1] },
    { data: { target: "x", message: "m" } },
    { data: { level: "loud", target: "x", message: "m" } },
    { data: { level: "warn", message: "m" } },
    { data: { level: "warn", target: "x", message: 3 } },
  ]) {
    assert.equal(toRecord(line(bad), ""), null, JSON.stringify(bad));
  }
  assert.equal(toRecord("{not json", ""), null);
  assert.equal(toRecord("[1,2]", ""), null);
  assert.equal(toRecord("null", ""), null);
});

test("batches unpack, gzipped or not, and garbage is a bad batch", () => {
  const ndjson = `${line()}\n\n${line({ seq: 18 })}\n`;
  assert.equal(unpack(gzipSync(ndjson), true).length, 2);
  assert.equal(unpack(Buffer.from(ndjson), false).length, 2);
  assert.throws(() => unpack(Buffer.from("plain"), true), BadBatch);
  const bomb = gzipSync(Buffer.alloc(21 << 20, 0x20));
  assert.throws(() => unpack(bomb, true), (e) => e instanceof BadBatch && e.message === "too large");
});

test("words are only a log's or a panic's message", () => {
  assert.equal(words("log", { message: "m" }), "m");
  assert.equal(words("stores", { message: "m" }), "");
  assert.equal(words("panic", {}), "");
});

test("report codes are 8 unambiguous characters, read back however typed", () => {
  const seen = new Set();
  for (let i = 0; i < 2000; i++) {
    const c = newCode();
    assert.match(c, /^[0-9A-HJKMNP-TV-Z]{8}$/);
    seen.add(c);
  }
  assert.ok(seen.size > 1990, "random");
  assert.equal(CODE_LENGTH, 8);
  const fixed = newCode((a) => a.fill(255));
  assert.equal(fixed, "ZZZZZZZZ");
  assert.equal(newCode((a) => a.fill(0)), "00000000");
  assert.equal(normalizeCode("7k3m-q9xa"), "7K3MQ9XA");
  assert.equal(normalizeCode("o1lI 2345"), "01112345");
  assert.equal(normalizeCode("7K3MQ9XU"), null, "no U");
  assert.equal(normalizeCode("7K3MQ9X"), null, "too short");
  assert.equal(normalizeCode("../etc/p"), null);
  assert.equal(normalizeCode(undefined), null);
});

test("a report is checked, and its logs' text kept apart", () => {
  const report = {
    v: 1,
    install: record.install,
    session: record.session,
    account: record.account,
    app: "0.29.0",
    os: "linux",
    ts: record.ts,
    note: "my edits don't reach Joe",
    logs: [{ name: "pimble.log", text: "line one\nline two\n" }, { name: "pimble.previous.log", text: "" }],
    stores: [{ id: "s", name: "Personal", path: "~/Dropbox/Personal.pimble" }],
  };
  const got = toReport(JSON.stringify(report));
  assert.deepEqual(got.logs, ["line one\nline two\n", ""]);
  assert.deepEqual(got.report.logs, [{ name: "pimble.log", length: 18 }, { name: "pimble.previous.log", length: 0 }]);
  assert.equal(got.report.account, "6f1c2e9a-3b4d-4e5f-8a7b-9c0d1e2f3a4b");
  assert.deepEqual(got.report.stores, report.stores);
  const bare = toReport(JSON.stringify({ ...report, note: undefined, logs: undefined, stores: undefined, account: undefined }));
  assert.equal(bare.report.note, "");
  assert.deepEqual(bare.report.logs, []);
  for (const [bad, why] of [
    [{ install: "x" }, "install"],
    [{ ts: "now" }, "ts"],
    [{ account: "joe@example.com" }, "account"],
    [{ logs: [{ name: "a" }] }, "logs: text"],
    [{ logs: [{ text: "a" }] }, "logs: name"],
    [{ logs: "a" }, "logs"],
    [{ note: 3 }, "note"],
    [{ stores: "x" }, "stores"],
  ]) {
    assert.equal(toReport(JSON.stringify({ ...report, ...bad })), why, JSON.stringify(bad));
  }
  assert.equal(toReport("nope"), "not JSON");
});

test("text is cut into parts, never inside a surrogate pair", () => {
  assert.deepEqual(cut("", 4), []);
  assert.deepEqual(cut("abcdefghij", 4), ["abcd", "efgh", "ij"]);
  const snow = "abc😀def";
  const parts = cut(snow, 4);
  assert.deepEqual(parts, ["abc", "😀de", "f"]);
  assert.equal(parts.join(""), snow);
});

test("the limiter allows 60 a minute per install", () => {
  const l = new Limiter(60);
  const t = 1_800_000_000_000;
  for (let i = 0; i < 60; i++) assert.equal(l.take("a", t), 0);
  assert.ok(l.take("a", t + 1000) > 0);
  assert.equal(l.take("b", t + 1000), 0);
  assert.equal(l.take("a", t + 60_000), 0);
});
