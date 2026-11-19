// A "Send Diagnostics..." report (docs/DIAGNOSTICS_CONTRACT.md, section 4):
// one JSON body with the last runs' log files, the open stores' shape and
// the person's note. It is answered with a short code the person tells us.
//
// A query is capped under RhypeDB's request limit (lib/db.js), and a report
// can hold 40 MiB of logs, so the logs' text is kept apart from the report:
// `Report.body` is the report with each log's `text` replaced by its
// length, and the text is cut into `ReportPart`s, put back together when
// the report is read.

import { ID, accountOf, isTs } from "./records.js";

export const MAX_COMPRESSED = 8 << 20;
export const MAX_UNCOMPRESSED = 40 << 20;
/// The report without its logs' text must fit in one query.
export const MAX_BODY = 1 << 20;
/// UTF-16 units per part: at most 3 bytes of UTF-8 each, so a part's query
/// stays under a megabyte however its text is escaped.
export const PART = 256 * 1024;
export const MAX_NOTE = 20_000;
export const MAX_LOGS = 16;

/// Crockford's base32: no I, L, O or U, so a code read aloud or typed from
/// a screenshot comes out right.
const ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
export const CODE_LENGTH = 8;

/// A new report code: 8 characters, 40 random bits.
export function newCode(random = crypto.getRandomValues.bind(crypto)) {
  const bytes = random(new Uint8Array(CODE_LENGTH));
  let code = "";
  for (const b of bytes) code += ALPHABET[b & 31];
  return code;
}

/// A code as someone typed it (lowercase, with a dash or spaces, an O for
/// a zero, an I or L for a one), as stored; null when it can't be one.
export function normalizeCode(text) {
  if (typeof text !== "string") return null;
  const code = text
    .toUpperCase()
    .replace(/[\s-]/g, "")
    .replace(/O/g, "0")
    .replace(/[IL]/g, "1");
  if (code.length !== CODE_LENGTH) return null;
  for (const ch of code) if (!ALPHABET.includes(ch)) return null;
  return code;
}

/// The checked report, `{report, logs}` (`report` with each log's text
/// left out, `logs` the texts), or a string saying what is wrong.
export function toReport(json) {
  let r;
  try {
    r = JSON.parse(json);
  } catch {
    return "not JSON";
  }
  if (typeof r !== "object" || r === null || Array.isArray(r)) return "not an object";
  const { v, install, session, app, os, ts, note = "", logs = [], stores = null } = r;
  if (!Number.isInteger(v) || v < 1) return "v";
  if (typeof install !== "string" || !ID.test(install)) return "install";
  if (typeof session !== "string" || !ID.test(session)) return "session";
  const account = accountOf(r.account);
  if (account === null) return "account";
  if (!isTs(ts)) return "ts";
  if (typeof note !== "string" || note.length > MAX_NOTE) return "note";
  if (!Array.isArray(logs) || logs.length > MAX_LOGS) return "logs";
  for (const l of logs) {
    if (typeof l !== "object" || l === null) return "logs";
    if (typeof l.name !== "string" || !l.name || l.name.length > 200) return "logs: name";
    if (typeof l.text !== "string") return "logs: text";
  }
  if (stores !== null && typeof stores !== "object") return "stores";
  const report = {
    v,
    install,
    session,
    account,
    app: typeof app === "string" ? app.slice(0, 64) : "",
    os: typeof os === "string" ? os.slice(0, 64) : "",
    ts,
    note,
    logs: logs.map((l) => ({ name: l.name, length: l.text.length })),
    stores,
  };
  return { report, logs: logs.map((l) => l.text) };
}

/// A text cut into parts of at most `size` UTF-16 units, never inside a
/// surrogate pair.
export function cut(text, size = PART) {
  const parts = [];
  for (let at = 0; at < text.length; ) {
    let end = Math.min(at + size, text.length);
    const last = text.charCodeAt(end - 1);
    if (end < text.length && last >= 0xd800 && last <= 0xdbff) end -= 1;
    parts.push(text.slice(at, end));
    at = end;
  }
  return parts;
}
