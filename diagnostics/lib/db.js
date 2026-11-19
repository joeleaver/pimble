// RhypeDB over its HTTP query plane. The query language has no bind
// variables, so every value goes through `lit`.

const URL_ = process.env.RHYPEDB_URL || "http://127.0.0.1:4200";

/// Queries are capped well under the server's request limit.
const MAX_QUERY = 1_500_000;

export class DbError extends Error {}

export async function query(q) {
  const response = await fetch(`${URL_}/query`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ query: q }),
  });
  const body = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new DbError(`${response.status}: ${body.error ?? "no answer"} (${q.slice(0, 120)})`);
  }
  return body;
}

/// The objects a query returned, as `{id, ...fields}`.
export async function rows(q) {
  const body = await query(q);
  const objects = body.objects ?? (body.object ? [body.object] : []);
  return objects.map((o) => ({ id: o.id, ...o.fields }));
}

/// A string literal. The parser knows \" \\ \n \t \r; other control
/// characters are dropped.
export function str(s) {
  let out = '"';
  for (const ch of String(s)) {
    switch (ch) {
      case '"': out += '\\"'; break;
      case "\\": out += "\\\\"; break;
      case "\n": out += "\\n"; break;
      case "\t": out += "\\t"; break;
      case "\r": out += "\\r"; break;
      default:
        if (ch >= " " && ch !== "\u007f") out += ch;
    }
  }
  return out + '"';
}

/// A value for a field: strings quoted, numbers and booleans as they are,
/// objects and arrays as raw JSON (for `Json` fields).
export function lit(v) {
  if (v === null || v === undefined) return "null";
  if (typeof v === "string") return str(v);
  if (typeof v === "number") {
    if (!Number.isFinite(v)) return "null";
    return String(v);
  }
  if (typeof v === "boolean") return String(v);
  return JSON.stringify(v);
}

/// An object literal; fields that are undefined are left out.
export function obj(fields) {
  const parts = [];
  for (const [k, v] of Object.entries(fields)) {
    if (v !== undefined) parts.push(`${k}: ${lit(v)}`);
  }
  return `{ ${parts.join(", ")} }`;
}

/// Creates the objects in as few `create_batch` calls as the size cap
/// allows.
export async function createMany(type, objects) {
  let batch = [];
  let size = 0;
  const flush = async () => {
    if (batch.length) await query(`${type}.create_batch([${batch.join(", ")}])`);
    batch = [];
    size = 0;
  };
  for (const o of objects) {
    const text = obj(o);
    if (size + text.length > MAX_QUERY) await flush();
    batch.push(text);
    size += text.length + 2;
  }
  await flush();
}

/// Every object a filter finds, a page at a time.
export async function* scan(source, page = 500) {
  for (let offset = 0; ; offset += page) {
    const got = await rows(`${source}.offset(${offset}).limit(${page})`);
    yield* got;
    if (got.length < page) return;
  }
}
