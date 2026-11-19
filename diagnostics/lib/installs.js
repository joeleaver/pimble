// Per-install counts (`Install` rows), and a request budget per install.

import { query, rows, obj, str } from "./db.js";

/// The server is one process, so a promise chain keeps the
/// read-modify-write of an install's row in order.
let chain = Promise.resolve();

/// Adds to an install's counts. `seen` is `{app, os, account, records,
/// reports}`; an account given replaces the one held (a tester who signs
/// in, or into another account).
export function count(install, seen) {
  const next = chain.then(() => apply(install, seen));
  chain = next.catch(() => {});
  return next;
}

async function apply(install, { app, os, account, records = 0, reports = 0 }) {
  const now = new Date().toISOString();
  const [row] = await rows(`Install.filter(.install == ${str(install)}).limit(1)`);
  if (!row) {
    await query(`Install.create(${obj({ install, account, first_seen: now, last_seen: now, app: app ?? "", os: os ?? "", records, reports })})`);
    return;
  }
  const changes = {
    last_seen: now,
    account: account || undefined,
    app: app || undefined,
    os: os || undefined,
    records: (row.records ?? 0) + records,
    reports: (row.reports ?? 0) + reports,
  };
  await query(`Install.get(${row.id}).update(${obj(changes)})`);
}

/// At most `limit` requests a minute from one install.
export class Limiter {
  constructor(limit = 60) {
    this.limit = limit;
    this.windows = new Map();
  }

  /// Seconds to wait, or 0 when this request may go ahead.
  take(install, now = Date.now()) {
    const minute = Math.floor(now / 60_000);
    let w = this.windows.get(install);
    if (!w || w.minute !== minute) {
      w = { minute, n: 0 };
      this.windows.set(install, w);
      if (this.windows.size > 10_000) this.sweep(minute);
    }
    w.n += 1;
    return w.n > this.limit ? 60 - Math.floor((now % 60_000) / 1000) : 0;
  }

  sweep(minute) {
    for (const [k, w] of this.windows) if (w.minute !== minute) this.windows.delete(k);
  }
}
