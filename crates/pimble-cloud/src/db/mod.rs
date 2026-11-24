//! The RhypeDB repository: every query this service runs against
//! `schema.rhype`, in one place. See that file's header comment for why
//! `Grant` and `Session` carry denormalized scalar copies of their relation
//! fields.
//!
//! Deliberately hand-rolled query strings (`Query::raw` equivalents) rather
//! than `rhypedb-codegen`'s generated typed seeds: this schema is four small
//! types, and a raw string plus a handful of `FieldMap` accessors is less
//! machinery than wiring a build-time codegen step into a crate this size.

use rhypedb_client::AsyncClient;
use rhypedb_wire::object::{Object, Value};

use crate::error::{CloudError, CloudResult};

/// Escape `s` as a query-language string literal — the two escapes the
/// server's parser understands (docs/src/queries.md's `Literal` grammar):
/// `"` and `\`. (`rhypedb_client::query::ql_string_literal` does the same
/// thing but is private to that crate.)
fn ql_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn get_string<'a>(o: &'a Object, field: &str) -> CloudResult<&'a str> {
    match o.fields.get(field) {
        Some(Value::String(s)) => Ok(s.as_str()),
        other => Err(CloudError::Internal(format!(
            "{}.{field}: expected a String, got {other:?}",
            o.type_name
        ))),
    }
}

fn get_bool(o: &Object, field: &str) -> CloudResult<bool> {
    match o.fields.get(field) {
        Some(Value::Bool(b)) => Ok(*b),
        other => Err(CloudError::Internal(format!(
            "{}.{field}: expected a Bool, got {other:?}",
            o.type_name
        ))),
    }
}

/// Like [`get_string_opt`], for a `Bool` field.
fn get_bool_opt(o: &Object, field: &str) -> CloudResult<Option<bool>> {
    match o.fields.get(field) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        other => Err(CloudError::Internal(format!(
            "{}.{field}: expected a Bool or to be absent, got {other:?}",
            o.type_name
        ))),
    }
}

fn get_u64(o: &Object, field: &str) -> CloudResult<u64> {
    match o.fields.get(field) {
        Some(Value::U64(v)) => Ok(*v),
        other => Err(CloudError::Internal(format!(
            "{}.{field}: expected a u64, got {other:?}",
            o.type_name
        ))),
    }
}

/// Like [`get_u64`], for a field absent on older rows.
fn get_u64_opt(o: &Object, field: &str) -> CloudResult<Option<u64>> {
    match o.fields.get(field) {
        None => Ok(None),
        Some(Value::U64(v)) => Ok(Some(*v)),
        other => Err(CloudError::Internal(format!(
            "{}.{field}: expected a u64 or to be absent, got {other:?}",
            o.type_name
        ))),
    }
}

/// Like [`get_string`], but a field simply absent from the row — every
/// `User` created before Phase 2a's nine key-material fields existed has
/// exactly this shape, since RhypeDB does not backfill a schema addition
/// onto old rows — is `Ok(None)` rather than the `Internal` error `get_string`
/// would raise. A field present but the wrong type is still `Internal`: that
/// is real corruption, not "an older row".
fn get_string_opt<'a>(o: &'a Object, field: &str) -> CloudResult<Option<&'a str>> {
    match o.fields.get(field) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        other => Err(CloudError::Internal(format!(
            "{}.{field}: expected a String or to be absent, got {other:?}",
            o.type_name
        ))),
    }
}

/// Like [`get_string_opt`], for a `u32` field.
fn get_u32_opt(o: &Object, field: &str) -> CloudResult<Option<u32>> {
    match o.fields.get(field) {
        None => Ok(None),
        Some(Value::U32(v)) => Ok(Some(*v)),
        other => Err(CloudError::Internal(format!(
            "{}.{field}: expected a u32 or to be absent, got {other:?}",
            o.type_name
        ))),
    }
}

/// `DateTime` fields come back as raw epoch-millis (`Value::DateTime`) from
/// the binary protocol — the RFC 3339 rendering is `value_to_query_json`'s
/// job for the HTTP `/query`/typed-row paths, which this repository doesn't
/// use.
fn get_datetime_ms(o: &Object, field: &str) -> CloudResult<i64> {
    match o.fields.get(field) {
        Some(Value::DateTime(ms)) => Ok(*ms),
        other => Err(CloudError::Internal(format!(
            "{}.{field}: expected a DateTime, got {other:?}",
            o.type_name
        ))),
    }
}

/// Like [`get_string_opt`], for a `DateTime` field.
fn get_datetime_ms_opt(o: &Object, field: &str) -> CloudResult<Option<i64>> {
    match o.fields.get(field) {
        None => Ok(None),
        Some(Value::DateTime(ms)) => Ok(Some(*ms)),
        other => Err(CloudError::Internal(format!(
            "{}.{field}: expected a DateTime or to be absent, got {other:?}",
            o.type_name
        ))),
    }
}

fn datetime_literal(ms: i64) -> String {
    // RFC 3339 is easier to read in a query than an epoch-millis integer
    // (both parse identically per docs/src/queries.md's Literals table).
    let dt = chrono::DateTime::from_timestamp_millis(ms).unwrap_or_default();
    ql_str(&dt.to_rfc3339())
}

fn now_literal() -> String {
    ql_str(&chrono::Utc::now().to_rfc3339())
}

/// True when `err` is the `@unique` violation on exactly `field` (the error
/// text is `rhypedb-engine`'s own: `"unique constraint violated: {type}.{field} = {value}"`).
/// Used to turn a plausible race (two signups for the same email) into 409
/// instead of a generic 500.
fn is_unique_violation(err: &rhypedb_client::Error, field: &str) -> bool {
    matches!(err, rhypedb_client::Error::Server(msg) if msg.contains("unique constraint violated") && msg.contains(field))
}

/// True when `err` is `Type.get(id)` on an id that doesn't exist — the
/// engine raises an error for this rather than an empty result, unlike
/// `.filter(...)` matching nothing.
fn is_not_found(err: &rhypedb_client::Error) -> bool {
    matches!(err, rhypedb_client::Error::Server(msg) if msg.contains("object not found"))
}

mod inbox;
mod rows;
mod stores;
mod users;
mod watches;

pub use rows::*;
pub use watches::{WatchBatchRow, WatchedDocRow};

pub struct RhypeDb {
    client: AsyncClient,
}

impl RhypeDb {
    pub async fn connect(addr: &str) -> CloudResult<Self> {
        Self::connect_waiting(addr, crate::DEPENDENCY_WAIT).await
    }

    /// Dial `addr`, retrying a refused or failed connection for up to `wait`.
    /// Any other error, and a connection still failing after `wait`, is
    /// returned as before.
    async fn connect_waiting(addr: &str, wait: std::time::Duration) -> CloudResult<Self> {
        let deadline = tokio::time::Instant::now() + wait;
        let mut delay = std::time::Duration::from_millis(100);
        let mut logged = false;
        loop {
            match AsyncClient::connect(addr).await {
                Ok(client) => return Ok(Self { client }),
                Err(rhypedb_client::Error::Connect(e)) if tokio::time::Instant::now() + delay < deadline => {
                    if !logged {
                        tracing::info!(%addr, error = %e, "rhypedb is not accepting connections yet; waiting");
                        logged = true;
                    }
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(std::time::Duration::from_secs(2));
                }
                Err(e) => return Err(CloudError::Internal(format!("connecting to rhypedb at {addr}: {e}"))),
            }
        }
    }

    async fn objects(&self, query: &str) -> CloudResult<Vec<Object>> {
        Ok(self.client.query(query).await?.into_objects())
    }

    async fn one(&self, query: &str) -> CloudResult<Option<Object>> {
        Ok(self.objects(query).await?.into_iter().next())
    }

    /// Link `source_rid` (of `source_type`) to `target_rid` (of
    /// `target_type`) on whichever relation field connects the two types.
    ///
    /// A `.create({...})` object literal only accepts scalar `Literal`
    /// values (`rhypedb-query::parser::Parser::parse_object` builds a
    /// `HashMap<String, Literal>`, full stop) — `docs/src/queries.md`'s
    /// `Post.create({ title: "Hello", author: User.get(1) })` example does
    /// not parse against this build. Every relation this service sets is
    /// therefore a separate `.link()` call after the object exists, in the
    /// query language's actual supported form: `Type.get(id).link(Other.get(id))`
    /// with NO preceding `.fieldName` traversal step. (`docs/src/queries.md`'s
    /// `.favorite_movies.link(...)` form, with the field name spelled out,
    /// also doesn't do what it looks like: `Step::Link` resolves the field
    /// to link purely from `(current_type, target_type)` via
    /// `resolve_relation_field`, using the ORIGINAL query source's type when
    /// the preceding traversal's result is empty — which it always is right
    /// after create — and using the traversed-to type otherwise, which
    /// isn't the source type `resolve_relation_field` needs at all. The bare
    /// form matches `rhypedb-query`'s own `execute_link_via_query` test
    /// exactly: `User.get(id).link(User.get(other_id))`, no field name.)
    /// Every relation in this schema is the only one between its two types,
    /// so the auto-resolution is always unambiguous.
    async fn link(&self, source_type: &str, source_rid: u64, target_type: &str, target_rid: u64) -> CloudResult<()> {
        let q = format!("{source_type}.get({source_rid}).link({target_type}.get({target_rid}))");
        self.objects(&q).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A free loopback port: bound, read, released.
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    /// A database that starts listening a moment after the service is the
    /// jkbase deployment case: the connect waits for it instead of failing.
    #[tokio::test]
    async fn connect_waits_for_a_database_that_is_still_starting() {
        let addr = format!("127.0.0.1:{}", free_port());
        let listen_at = addr.clone();
        let listener = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            let listener = tokio::net::TcpListener::bind(&listen_at).await.unwrap();
            let _held = listener.accept().await.unwrap();
        });
        RhypeDb::connect_waiting(&addr, Duration::from_secs(10)).await.expect("connected once the database listened");
        listener.await.unwrap();
    }

    /// Nothing ever listening is still an error, once the wait is over.
    #[tokio::test]
    async fn connect_gives_up_after_the_wait() {
        let addr = format!("127.0.0.1:{}", free_port());
        let started = std::time::Instant::now();
        let err = RhypeDb::connect_waiting(&addr, Duration::from_millis(500)).await.err().expect("nothing listens");
        assert!(err.to_string().contains(&format!("connecting to rhypedb at {addr}")), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5), "gave up late: {:?}", started.elapsed());
    }
}
