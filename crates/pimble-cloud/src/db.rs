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

fn get_u64(o: &Object, field: &str) -> CloudResult<u64> {
    match o.fields.get(field) {
        Some(Value::U64(v)) => Ok(*v),
        other => Err(CloudError::Internal(format!(
            "{}.{field}: expected a u64, got {other:?}",
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

#[derive(Debug, Clone)]
pub struct UserRow {
    pub rid: u64,
    pub user_uuid: String,
    pub email: String,
    pub password_hash: String,
}

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub rid: u64,
    pub user_rid: u64,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct HostedStoreRow {
    pub rid: u64,
    pub store_id: String,
    pub name: String,
    pub created_at_ms: i64,
    pub deleted: bool,
}

#[derive(Debug, Clone)]
pub struct GrantRow {
    pub rid: u64,
    pub user_rid: u64,
    pub store_rid: u64,
    pub store_uuid: String,
    pub role: String,
}

fn user_from_object(o: &Object) -> CloudResult<UserRow> {
    Ok(UserRow {
        rid: o.id,
        user_uuid: get_string(o, "user_uuid")?.to_string(),
        email: get_string(o, "email")?.to_string(),
        password_hash: get_string(o, "password_hash")?.to_string(),
    })
}

fn session_from_object(o: &Object) -> CloudResult<SessionRow> {
    Ok(SessionRow { rid: o.id, user_rid: get_u64(o, "user_rid")?, expires_at_ms: get_datetime_ms(o, "expires_at")? })
}

fn hosted_store_from_object(o: &Object) -> CloudResult<HostedStoreRow> {
    Ok(HostedStoreRow {
        rid: o.id,
        store_id: get_string(o, "store_id")?.to_string(),
        name: get_string(o, "name")?.to_string(),
        created_at_ms: get_datetime_ms(o, "created_at")?,
        deleted: get_bool(o, "deleted")?,
    })
}

fn grant_from_object(o: &Object) -> CloudResult<GrantRow> {
    Ok(GrantRow {
        rid: o.id,
        user_rid: get_u64(o, "user_rid")?,
        store_rid: get_u64(o, "store_rid")?,
        store_uuid: get_string(o, "store_uuid")?.to_string(),
        role: get_string(o, "role")?.to_string(),
    })
}

pub struct RhypeDb {
    client: AsyncClient,
}

impl RhypeDb {
    pub async fn connect(addr: &str) -> CloudResult<Self> {
        let client = AsyncClient::connect(addr)
            .await
            .map_err(|e| CloudError::Internal(format!("connecting to rhypedb at {addr}: {e}")))?;
        Ok(Self { client })
    }

    async fn objects(&self, query: &str) -> CloudResult<Vec<Object>> {
        Ok(self.client.query(query).await?.into_objects())
    }

    async fn one(&self, query: &str) -> CloudResult<Option<Object>> {
        Ok(self.objects(query).await?.into_iter().next())
    }

    // ── Users ──────────────────────────────────────────────────────────

    /// `None` on a duplicate `email_lower` (the caller turns that into 409);
    /// any other RhypeDB failure is `Err`.
    pub async fn create_user(&self, email: &str, password_hash: &str) -> CloudResult<Option<UserRow>> {
        let user_uuid = uuid::Uuid::new_v4().to_string();
        let email_lower = email.to_lowercase();
        let q = format!(
            "User.create({{ user_uuid: {uuid}, email: {email}, email_lower: {email_lower}, password_hash: {hash}, created_at: {now} }})",
            uuid = ql_str(&user_uuid),
            email = ql_str(email),
            email_lower = ql_str(&email_lower),
            hash = ql_str(password_hash),
            now = now_literal(),
        );
        match self.client.query(&q).await {
            Ok(result) => {
                let obj = result.into_objects().into_iter().next().ok_or_else(|| CloudError::Internal("User.create returned nothing".into()))?;
                Ok(Some(user_from_object(&obj)?))
            }
            Err(e) if is_unique_violation(&e, "email_lower") => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn find_user_by_email(&self, email: &str) -> CloudResult<Option<UserRow>> {
        let email_lower = email.to_lowercase();
        let q = format!("User.filter(.email_lower == {})", ql_str(&email_lower));
        self.one(&q).await?.map(|o| user_from_object(&o)).transpose()
    }

    pub async fn find_user_by_uuid(&self, user_uuid: &str) -> CloudResult<Option<UserRow>> {
        let q = format!("User.filter(.user_uuid == {})", ql_str(user_uuid));
        self.one(&q).await?.map(|o| user_from_object(&o)).transpose()
    }

    pub async fn get_user(&self, user_rid: u64) -> CloudResult<Option<UserRow>> {
        let q = format!("User.get({user_rid})");
        self.one(&q).await?.map(|o| user_from_object(&o)).transpose()
    }

    // ── Sessions ───────────────────────────────────────────────────────

    pub async fn create_session(&self, user_rid: u64, token_hash: &str, expires_at_ms: i64) -> CloudResult<SessionRow> {
        let q = format!(
            "Session.create({{ token_hash: {hash}, user: User.get({uid}), user_rid: {uid}, expires_at: {exp}, created_at: {now} }})",
            hash = ql_str(token_hash),
            uid = user_rid,
            exp = datetime_literal(expires_at_ms),
            now = now_literal(),
        );
        let obj = self
            .objects(&q)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| CloudError::Internal("Session.create returned nothing".into()))?;
        session_from_object(&obj)
    }

    pub async fn find_session_by_token_hash(&self, token_hash: &str) -> CloudResult<Option<SessionRow>> {
        let q = format!("Session.filter(.token_hash == {})", ql_str(token_hash));
        self.one(&q).await?.map(|o| session_from_object(&o)).transpose()
    }

    pub async fn delete_session(&self, session_rid: u64) -> CloudResult<()> {
        self.objects(&format!("Session.get({session_rid}).delete()")).await?;
        Ok(())
    }

    // ── Hosted stores ──────────────────────────────────────────────────

    pub async fn create_hosted_store(&self, store_id: &str, name: &str, dir_name: &str) -> CloudResult<HostedStoreRow> {
        let q = format!(
            "HostedStore.create({{ store_id: {sid}, name: {name}, dir_name: {dir}, created_at: {now}, deleted: false }})",
            sid = ql_str(store_id),
            name = ql_str(name),
            dir = ql_str(dir_name),
            now = now_literal(),
        );
        let obj = self
            .objects(&q)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| CloudError::Internal("HostedStore.create returned nothing".into()))?;
        hosted_store_from_object(&obj)
    }

    pub async fn find_hosted_store(&self, store_id: &str) -> CloudResult<Option<HostedStoreRow>> {
        let q = format!("HostedStore.filter(.store_id == {})", ql_str(store_id));
        self.one(&q).await?.map(|o| hosted_store_from_object(&o)).transpose()
    }

    pub async fn get_hosted_store(&self, store_rid: u64) -> CloudResult<Option<HostedStoreRow>> {
        let q = format!("HostedStore.get({store_rid})");
        self.one(&q).await?.map(|o| hosted_store_from_object(&o)).transpose()
    }

    /// Soft-deletes a store (docs/CLOUD_CONTRACT.md: "marks deleted, removes
    /// grants; the server-side directory stays"). Does not remove grants —
    /// the caller does that separately via [`Self::delete_grants_for_store`].
    pub async fn mark_store_deleted(&self, store_rid: u64) -> CloudResult<()> {
        self.objects(&format!("HostedStore.get({store_rid}).update({{ deleted: true }})")).await?;
        Ok(())
    }

    // ── Grants ─────────────────────────────────────────────────────────

    pub async fn create_grant(&self, user_rid: u64, store_rid: u64, store_uuid: &str, role: &str) -> CloudResult<GrantRow> {
        let q = format!(
            "Grant.create({{ user: User.get({uid}), store: HostedStore.get({sid}), role: {role}, user_rid: {uid}, store_rid: {sid}, store_uuid: {suuid} }})",
            uid = user_rid,
            sid = store_rid,
            role = ql_str(role),
            suuid = ql_str(store_uuid),
        );
        let obj = self
            .objects(&q)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| CloudError::Internal("Grant.create returned nothing".into()))?;
        grant_from_object(&obj)
    }

    pub async fn find_grant(&self, user_rid: u64, store_rid: u64) -> CloudResult<Option<GrantRow>> {
        let q = format!("Grant.filter(.user_rid == {user_rid} && .store_rid == {store_rid})");
        self.one(&q).await?.map(|o| grant_from_object(&o)).transpose()
    }

    pub async fn update_grant_role(&self, grant_rid: u64, role: &str) -> CloudResult<()> {
        self.objects(&format!("Grant.get({grant_rid}).update({{ role: {} }})", ql_str(role))).await?;
        Ok(())
    }

    pub async fn delete_grant(&self, grant_rid: u64) -> CloudResult<()> {
        self.objects(&format!("Grant.get({grant_rid}).delete()")).await?;
        Ok(())
    }

    /// Every grant on `store_rid`, deleted one at a time (the query language
    /// has no bulk `.filter(...).delete()`).
    pub async fn delete_grants_for_store(&self, store_rid: u64) -> CloudResult<()> {
        for grant in self.grants_for_store(store_rid).await? {
            self.delete_grant(grant.rid).await?;
        }
        Ok(())
    }

    pub async fn grants_for_user(&self, user_rid: u64) -> CloudResult<Vec<GrantRow>> {
        let q = format!("Grant.filter(.user_rid == {user_rid})");
        self.objects(&q).await?.iter().map(grant_from_object).collect()
    }

    pub async fn grants_for_store(&self, store_rid: u64) -> CloudResult<Vec<GrantRow>> {
        let q = format!("Grant.filter(.store_rid == {store_rid})");
        self.objects(&q).await?.iter().map(grant_from_object).collect()
    }
}
