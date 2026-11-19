// Reading a records batch: gzip NDJSON, one envelope per line
// (docs/DIAGNOSTICS_CONTRACT.md, sections 2 and 3).

import { gunzipSync } from "node:zlib";

export const MAX_COMPRESSED = 4 << 20;
export const MAX_UNCOMPRESSED = 20 << 20;

/// What a record may be about. Anything else is refused, so a typo in the
/// app shows up as rejected lines rather than as a kind nobody reads.
export const KINDS = new Set(["system", "log", "stores", "panic", "previous_run", "state"]);
export const LEVELS = new Set(["error", "warn", "info", "debug", "trace"]);

export const ID = /^[0-9a-f-]{8,64}$/;
export const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const TS = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})$/;

export class BadBatch extends Error {}

/// A gzipped (or plain) body as text, at most `max` bytes once unpacked.
/// Throws `BadBatch` when it can't be read at all.
export function inflate(body, gzipped, max = MAX_UNCOMPRESSED) {
  let text;
  try {
    text = (gzipped ? gunzipSync(body, { maxOutputLength: max }) : body).toString("utf8");
  } catch (e) {
    throw new BadBatch(e.code === "ERR_BUFFER_TOO_LARGE" ? "too large" : "not gzip");
  }
  if (Buffer.byteLength(text) > max) throw new BadBatch("too large");
  return text;
}

/// The batch's lines.
export function unpack(body, gzipped) {
  return inflate(body, gzipped).split("\n").filter((l) => l.trim());
}

/// An RFC 3339 instant, or false.
export const isTs = (ts) => typeof ts === "string" && TS.test(ts) && !Number.isNaN(Date.parse(ts));

/// An account id as stored (lowercase), undefined when there is none, or
/// null when there is one and it is not a UUID.
export function accountOf(account) {
  if (account === undefined || account === null || account === "") return undefined;
  if (typeof account !== "string") return null;
  const a = account.toLowerCase();
  return UUID.test(a) ? a : null;
}

/// One line as a stored `Record`, or null when it isn't a record.
export function toRecord(line, received) {
  let r;
  try {
    r = JSON.parse(line);
  } catch {
    return null;
  }
  if (typeof r !== "object" || r === null || Array.isArray(r)) return null;
  const { v, install, session, seq, ts, app, os, kind, data } = r;
  if (!Number.isInteger(v) || v < 1) return null;
  if (typeof install !== "string" || !ID.test(install)) return null;
  if (typeof session !== "string" || !ID.test(session)) return null;
  if (!Number.isInteger(seq) || seq < 0) return null;
  if (!isTs(ts)) return null;
  if (typeof kind !== "string" || !KINDS.has(kind)) return null;
  if (typeof data !== "object" || data === null || Array.isArray(data)) return null;
  const account = accountOf(r.account);
  if (account === null) return null;
  let body = data;
  if (kind === "log") {
    const { level, target, message } = data;
    if (typeof level !== "string" || !LEVELS.has(level.toLowerCase())) return null;
    if (typeof target !== "string" || typeof message !== "string") return null;
    body = { ...data, level: level.toLowerCase() };
  }
  return {
    v,
    install,
    session,
    seq,
    ts,
    received,
    app: typeof app === "string" ? app.slice(0, 64) : "",
    os: typeof os === "string" ? os.slice(0, 64) : "",
    kind,
    account,
    text: words(kind, body),
    data: body,
  };
}

/// What a record says, for search: a log line's or a panic's message.
export function words(kind, data) {
  if ((kind === "log" || kind === "panic") && data && typeof data.message === "string") {
    return data.message.slice(0, 8000);
  }
  return "";
}
