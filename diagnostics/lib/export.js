// The records stored, as JSONL: one record per line, as the app sent it
// (plus `received`), ordered by `ts` and then by session and `seq`, so two
// installs' records (both ends of a share) read as one story. Retried
// uploads can store a record twice; repeats are dropped by (session, seq).

import { scan, str } from "./db.js";

/// `filters`: `{install, account, kind, since, until}`. `since` keeps what
/// is at or after it, `until` what is before it (a bare date is that day's
/// first instant, UTC). Lines are JSON strings.
export async function* exportLines({ install, account, kind, since, until } = {}) {
  const where = [];
  if (install) where.push(`.install == ${str(install)}`);
  if (account) where.push(`.account == ${str(account)}`);
  if (kind) where.push(`.kind == ${str(kind)}`);
  if (since) where.push(`.ts >= ${str(new Date(since).toISOString())}`);
  if (until) where.push(`.ts < ${str(new Date(until).toISOString())}`);
  const source = where.length ? `Record.filter(${where.join(" && ")})` : "Record";

  const seen = new Set();
  const records = [];
  for await (const r of scan(source)) {
    const id = `${r.session}/${r.seq}`;
    if (seen.has(id)) continue;
    seen.add(id);
    records.push(r);
  }
  records.sort((a, b) => Date.parse(a.ts) - Date.parse(b.ts) || a.session.localeCompare(b.session) || a.seq - b.seq);
  for (const r of records) {
    const { v, install, session, seq, ts, received, app, os, kind, account, data } = r;
    yield JSON.stringify({ v, install, session, seq, ts, received, app, os, kind, account, data });
  }
}
