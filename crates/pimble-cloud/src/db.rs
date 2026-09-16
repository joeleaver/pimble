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

#[derive(Debug, Clone)]
pub struct UserRow {
    pub rid: u64,
    pub user_uuid: String,
    pub email: String,
    pub password_hash: String,
    /// Phase 1b (email verification).
    pub verified: bool,
    pub verify_token_hash: String,
    pub verify_expires_at_ms: i64,
    /// Phase 2a (docs/CRYPTO_CONTRACT.md "Data model additions"). `None` on
    /// every field together, never some but not others, for a row created
    /// before this phase (this crate's own `create_user` always writes all
    /// nine at once) — see [`Self::has_key_material`].
    pub kdf_salt: Option<String>,
    pub kdf_m_cost: Option<u32>,
    pub kdf_t_cost: Option<u32>,
    pub kdf_p_cost: Option<u32>,
    pub public_encryption_key: Option<String>,
    pub public_signing_key: Option<String>,
    /// `pimble_crypto::AccountKeyBlob` as stored: a JSON string, opaque here.
    pub account_key_blob: Option<String>,
    pub recovery_salt: Option<String>,
    /// `pimble_crypto::AccountKeyBlob` as stored: a JSON string, opaque here.
    pub recovery_key_blob: Option<String>,
}

impl UserRow {
    /// Whether this row has real Phase 2a key material — `false` for a
    /// pre-Phase-2a row (checked via `kdf_salt`, the field the startup
    /// migration keys its cleanup on too; every field here is written
    /// together by `create_user`, so this one field stands for all nine).
    /// A legacy user is treated as if unknown throughout: `GET /kdf` answers
    /// the decoy, `POST /login` answers a plain 401.
    pub fn has_key_material(&self) -> bool {
        self.kdf_salt.is_some()
    }

    /// This user's `KdfParams`, or `None` for a legacy row with no key
    /// material.
    pub fn kdf_params(&self) -> Option<pimble_crypto::KdfParams> {
        Some(pimble_crypto::KdfParams {
            salt: self.kdf_salt.clone()?,
            m_cost: self.kdf_m_cost?,
            t_cost: self.kdf_t_cost?,
            p_cost: self.kdf_p_cost?,
        })
    }

    /// This user's `AccountPublicKeys`, or `None` for a legacy row with no
    /// key material.
    pub fn public_keys(&self) -> Option<pimble_crypto::AccountPublicKeys> {
        Some(pimble_crypto::AccountPublicKeys { encryption: self.public_encryption_key.clone()?, signing: self.public_signing_key.clone()? })
    }
}

/// Everything [`RhypeDb::create_user`] needs beyond email and the auth-key
/// hash (docs/CRYPTO_CONTRACT.md "Data model additions"). Bundled so the
/// call site reads as one unit of "the account's key material" rather than
/// nine positional strings.
#[derive(Debug, Clone)]
pub struct NewUserKeyMaterial {
    pub kdf_salt: String,
    pub kdf_m_cost: u32,
    pub kdf_t_cost: u32,
    pub kdf_p_cost: u32,
    pub public_encryption_key: String,
    pub public_signing_key: String,
    pub account_key_blob: String,
    pub recovery_salt: String,
    pub recovery_key_blob: String,
}

impl NewUserKeyMaterial {
    /// Placeholder key material for a test that needs a `User` row to exist
    /// (e.g. to exercise mail or rate-limiting behaviour) without exercising
    /// any crypto endpoint — every field is schema-required, but nothing
    /// checks these values are real key material below the `/me/keys`,
    /// `/users/lookup` and `/stores/{id}/keys` handlers a test actually
    /// calls.
    pub fn placeholder_for_tests() -> Self {
        Self {
            kdf_salt: "placeholder-salt".to_string(),
            kdf_m_cost: pimble_crypto::KDF_M_COST_KIB,
            kdf_t_cost: pimble_crypto::KDF_T_COST,
            kdf_p_cost: pimble_crypto::KDF_P_COST,
            public_encryption_key: "placeholder-encryption-key".to_string(),
            public_signing_key: "placeholder-signing-key".to_string(),
            account_key_blob: "{}".to_string(),
            recovery_salt: "placeholder-recovery-salt".to_string(),
            recovery_key_blob: "{}".to_string(),
        }
    }
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
    /// `"plain"` or `"vault"` (docs/CRYPTO_CONTRACT.md), matching
    /// `pimble_core::StoreKind`'s serde spelling.
    pub kind: String,
}

#[derive(Debug, Clone)]
pub struct GrantRow {
    pub rid: u64,
    pub user_rid: u64,
    pub store_rid: u64,
    pub store_uuid: String,
    pub role: String,
}

/// One member's envelope for one key id on one store (docs/CRYPTO_CONTRACT.md
/// "Data model additions": `KeyGrant`).
#[derive(Debug, Clone)]
pub struct KeyGrantRow {
    pub rid: u64,
    pub user_rid: u64,
    pub store_rid: u64,
    pub key_id: String,
    /// A `pimble_crypto::KeyEnvelope` as stored: a JSON string.
    pub envelope: String,
}

fn user_from_object(o: &Object) -> CloudResult<UserRow> {
    Ok(UserRow {
        rid: o.id,
        // Absent on the very bare "only email/email_lower/password_hash/
        // created_at" phase-1 shape too (confirmed by
        // `create_phase1_shape_user_for_tests` in the integration tests,
        // which omits it and still creates successfully) — defaults to ""
        // rather than erroring, the same sentinel `verify_token_hash` uses.
        // A row this bare has no key material either, so it's gone by the
        // next startup cleanup regardless; nothing meaningful ever reads
        // this fallback value in the meantime (a keyless account can never
        // reach `issue_session`, the one place `user_uuid` becomes a JWT
        // `sub` or an API-visible id).
        user_uuid: get_string_opt(o, "user_uuid")?.map(str::to_string).unwrap_or_default(),
        email: get_string(o, "email")?.to_string(),
        password_hash: get_string(o, "password_hash")?.to_string(),
        // A phase-1 row (created before email verification existed) has
        // none of these three either — absent defaults to "unverified, no
        // live token", the same safe state a real never-verified account
        // has, not an error (the live crash this replaced).
        verified: get_bool_opt(o, "verified")?.unwrap_or(false),
        verify_token_hash: get_string_opt(o, "verify_token_hash")?.map(str::to_string).unwrap_or_default(),
        verify_expires_at_ms: get_datetime_ms_opt(o, "verify_expires_at")?.unwrap_or(0),
        kdf_salt: get_string_opt(o, "kdf_salt")?.map(str::to_string),
        kdf_m_cost: get_u32_opt(o, "kdf_m_cost")?,
        kdf_t_cost: get_u32_opt(o, "kdf_t_cost")?,
        kdf_p_cost: get_u32_opt(o, "kdf_p_cost")?,
        public_encryption_key: get_string_opt(o, "public_encryption_key")?.map(str::to_string),
        public_signing_key: get_string_opt(o, "public_signing_key")?.map(str::to_string),
        account_key_blob: get_string_opt(o, "account_key_blob")?.map(str::to_string),
        recovery_salt: get_string_opt(o, "recovery_salt")?.map(str::to_string),
        recovery_key_blob: get_string_opt(o, "recovery_key_blob")?.map(str::to_string),
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
        kind: get_string(o, "kind")?.to_string(),
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

fn key_grant_from_object(o: &Object) -> CloudResult<KeyGrantRow> {
    Ok(KeyGrantRow {
        rid: o.id,
        user_rid: get_u64(o, "user_rid")?,
        store_rid: get_u64(o, "store_rid")?,
        key_id: get_string(o, "key_id")?.to_string(),
        envelope: get_string(o, "envelope")?.to_string(),
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

    // ── Users ──────────────────────────────────────────────────────────

    /// Creates the user unverified (Phase 1b: signup issues a verify token
    /// separately via [`Self::set_verify_token`], right after this succeeds —
    /// see `routes/accounts.rs`'s `start_verification`) with the given key
    /// material (Phase 2a: `password_hash` is `Argon2id(auth_key)`, not a
    /// human password — see schema.rhype's `User.password_hash` comment).
    /// `None` on a duplicate `email_lower` (the caller turns that into 409);
    /// any other RhypeDB failure is `Err`.
    pub async fn create_user(&self, email: &str, password_hash: &str, keys: &NewUserKeyMaterial) -> CloudResult<Option<UserRow>> {
        let user_uuid = uuid::Uuid::new_v4().to_string();
        let email_lower = email.to_lowercase();
        let q = format!(
            "User.create({{ user_uuid: {uuid}, email: {email}, email_lower: {email_lower}, password_hash: {hash}, created_at: {now}, verified: false, verify_token_hash: {empty}, verify_expires_at: {now}, kdf_salt: {kdf_salt}, kdf_m_cost: {m_cost}, kdf_t_cost: {t_cost}, kdf_p_cost: {p_cost}, public_encryption_key: {pub_enc}, public_signing_key: {pub_sign}, account_key_blob: {acct_blob}, recovery_salt: {rec_salt}, recovery_key_blob: {rec_blob} }})",
            uuid = ql_str(&user_uuid),
            email = ql_str(email),
            email_lower = ql_str(&email_lower),
            hash = ql_str(password_hash),
            now = now_literal(),
            empty = ql_str(""),
            kdf_salt = ql_str(&keys.kdf_salt),
            m_cost = keys.kdf_m_cost,
            t_cost = keys.kdf_t_cost,
            p_cost = keys.kdf_p_cost,
            pub_enc = ql_str(&keys.public_encryption_key),
            pub_sign = ql_str(&keys.public_signing_key),
            acct_blob = ql_str(&keys.account_key_blob),
            rec_salt = ql_str(&keys.recovery_salt),
            rec_blob = ql_str(&keys.recovery_key_blob),
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

    /// (Re)issues a verify token for `user_rid` (Phase 1b: fresh signup,
    /// resend, or a signup retry against an existing unverified address).
    pub async fn set_verify_token(&self, user_rid: u64, verify_token_hash: &str, expires_at_ms: i64) -> CloudResult<()> {
        let q = format!(
            "User.get({user_rid}).update({{ verify_token_hash: {hash}, verify_expires_at: {exp} }})",
            hash = ql_str(verify_token_hash),
            exp = datetime_literal(expires_at_ms),
        );
        self.objects(&q).await?;
        Ok(())
    }

    /// Marks `user_rid` verified and clears its verify token (so the same
    /// link can't be replayed to re-verify or extend anything).
    pub async fn mark_user_verified(&self, user_rid: u64) -> CloudResult<()> {
        let q = format!("User.get({user_rid}).update({{ verified: true, verify_token_hash: {empty} }})", empty = ql_str(""));
        self.objects(&q).await?;
        Ok(())
    }

    /// Looks up a user by the hash of a verify token from a `/verify?token=`
    /// link. The caller must never pass an empty `verify_token_hash` — every
    /// verified or superseded user's row carries `verify_token_hash: ""`, so
    /// an empty hash would match an arbitrary (and ambiguous) set of rows;
    /// callers reject an empty query-string token before hashing it.
    pub async fn find_user_by_verify_token_hash(&self, verify_token_hash: &str) -> CloudResult<Option<UserRow>> {
        debug_assert!(!verify_token_hash.is_empty(), "must not query the empty verify_token_hash sentinel");
        let q = format!("User.filter(.verify_token_hash == {})", ql_str(verify_token_hash));
        self.one(&q).await?.map(|o| user_from_object(&o)).transpose()
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

    pub async fn delete_user(&self, user_rid: u64) -> CloudResult<()> {
        self.objects(&format!("User.get({user_rid}).delete()")).await?;
        Ok(())
    }

    /// Test-only: creates a `User` row shaped exactly like a real
    /// pre-Phase-2a account — every key-material field simply never set,
    /// not present as an empty string — so a test can reproduce "a legacy
    /// row exists" without needing a pre-migration database snapshot. Real
    /// signups always go through [`Self::create_user`], which sets every
    /// field; this is the one path that deliberately doesn't.
    pub async fn create_legacy_user_without_keys_for_tests(&self, email: &str, password_hash: &str) -> CloudResult<UserRow> {
        let user_uuid = uuid::Uuid::new_v4().to_string();
        let email_lower = email.to_lowercase();
        let q = format!(
            "User.create({{ user_uuid: {uuid}, email: {email}, email_lower: {email_lower}, password_hash: {hash}, created_at: {now}, verified: true, verify_token_hash: {empty}, verify_expires_at: {now} }})",
            uuid = ql_str(&user_uuid),
            email = ql_str(email),
            email_lower = ql_str(&email_lower),
            hash = ql_str(password_hash),
            now = now_literal(),
            empty = ql_str(""),
        );
        let obj = self.objects(&q).await?.into_iter().next().ok_or_else(|| CloudError::Internal("User.create returned nothing".into()))?;
        user_from_object(&obj)
    }

    /// Test-only: creates a `User` row shaped like the very first accounts
    /// this crate ever had — only the four fields present before email
    /// verification (`verified`/`verify_token_hash`/`verify_expires_at`),
    /// let alone Phase 2a's key material, existed. Returns the raw object
    /// id, not a `UserRow`: `user_from_object` itself fails on a row this
    /// bare (missing even `user_uuid`), which is exactly the shape
    /// [`Self::delete_legacy_users_without_keys`] must tolerate without
    /// ever deserializing it.
    pub async fn create_phase1_shape_user_for_tests(&self, email: &str, password_hash: &str) -> CloudResult<u64> {
        let email_lower = email.to_lowercase();
        let q = format!(
            "User.create({{ email: {email}, email_lower: {email_lower}, password_hash: {hash}, created_at: {now} }})",
            email = ql_str(email),
            email_lower = ql_str(&email_lower),
            hash = ql_str(password_hash),
            now = now_literal(),
        );
        let obj = self.objects(&q).await?.into_iter().next().ok_or_else(|| CloudError::Internal("User.create returned nothing".into()))?;
        Ok(obj.id)
    }

    /// Test-only: whether `User.get(user_rid)` still returns a row, without
    /// trying to deserialize it into a `UserRow` — a phase-1-shape row
    /// (see [`Self::create_phase1_shape_user_for_tests`]) would fail that
    /// parse even while it still exists.
    pub async fn user_exists_for_tests(&self, user_rid: u64) -> CloudResult<bool> {
        match self.client.query(&format!("User.get({user_rid})")).await {
            Ok(result) => Ok(!result.into_objects().is_empty()),
            Err(e) if is_not_found(&e) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Deletes every `User` row with no key material (`kdf_salt` absent —
    /// see [`UserRow::has_key_material`]), along with its sessions and
    /// grants, and returns how many were deleted. Run once at startup
    /// (`build_state`/`build_state_with_mailer`), so a legacy pre-Phase-2a
    /// account (docs/CRYPTO_CONTRACT.md's migration note; in production,
    /// Joe's own pre-encryption test accounts) is gone by the time this
    /// process answers its first request. The caller logs the count at
    /// `warn`.
    pub async fn delete_legacy_users_without_keys(&self) -> CloudResult<usize> {
        let mut deleted = 0usize;
        // Raw objects, not `UserRow`/`user_from_object`: a phase-1 row
        // (created before email verification, let alone Phase 2a's key
        // material, existed) can be missing almost any field this crate has
        // added since, and typed parsing would fail on it before this
        // cleanup ever got to decide whether to delete it — the startup
        // crash this replaced. The only thing that matters here is whether
        // `kdf_salt` is present as a String; everything else about the row
        // is irrelevant to that decision, and only its id is needed to
        // delete it.
        for user_object in self.objects("User").await? {
            let has_key_material = matches!(user_object.fields.get("kdf_salt"), Some(Value::String(_)));
            if has_key_material {
                continue;
            }
            let user_rid = user_object.id;
            for session in self.sessions_for_user(user_rid).await? {
                self.delete_session(session.rid).await?;
            }
            for grant in self.grants_for_user(user_rid).await? {
                self.delete_grant(grant.rid).await?;
            }
            self.delete_user(user_rid).await?;
            deleted += 1;
        }
        Ok(deleted)
    }

    // ── Sessions ───────────────────────────────────────────────────────

    pub async fn sessions_for_user(&self, user_rid: u64) -> CloudResult<Vec<SessionRow>> {
        let q = format!("Session.filter(.user_rid == {user_rid})");
        self.objects(&q).await?.iter().map(session_from_object).collect()
    }

    pub async fn create_session(&self, user_rid: u64, token_hash: &str, expires_at_ms: i64) -> CloudResult<SessionRow> {
        let q = format!(
            "Session.create({{ token_hash: {hash}, user_rid: {uid}, expires_at: {exp}, created_at: {now} }})",
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
        let session = session_from_object(&obj)?;
        self.link("Session", session.rid, "User", user_rid).await?;
        Ok(session)
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

    pub async fn create_hosted_store(&self, store_id: &str, name: &str, dir_name: &str, kind: &str) -> CloudResult<HostedStoreRow> {
        let q = format!(
            "HostedStore.create({{ store_id: {sid}, name: {name}, dir_name: {dir}, created_at: {now}, deleted: false, kind: {kind} }})",
            sid = ql_str(store_id),
            name = ql_str(name),
            dir = ql_str(dir_name),
            now = now_literal(),
            kind = ql_str(kind),
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
            "Grant.create({{ role: {role}, user_rid: {uid}, store_rid: {sid}, store_uuid: {suuid} }})",
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
        let grant = grant_from_object(&obj)?;
        self.link("Grant", grant.rid, "User", user_rid).await?;
        self.link("Grant", grant.rid, "HostedStore", store_rid).await?;
        Ok(grant)
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

    // ── Key grants (docs/CRYPTO_CONTRACT.md "Data model additions") ─────

    pub async fn create_key_grant(&self, user_rid: u64, store_rid: u64, store_uuid: &str, key_id: &str, envelope_json: &str) -> CloudResult<KeyGrantRow> {
        let q = format!(
            "KeyGrant.create({{ key_id: {kid}, envelope: {env}, user_rid: {uid}, store_rid: {sid}, store_uuid: {suuid} }})",
            kid = ql_str(key_id),
            env = ql_str(envelope_json),
            uid = user_rid,
            sid = store_rid,
            suuid = ql_str(store_uuid),
        );
        let obj = self
            .objects(&q)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| CloudError::Internal("KeyGrant.create returned nothing".into()))?;
        let grant = key_grant_from_object(&obj)?;
        self.link("KeyGrant", grant.rid, "User", user_rid).await?;
        self.link("KeyGrant", grant.rid, "HostedStore", store_rid).await?;
        Ok(grant)
    }

    pub async fn update_key_grant_envelope(&self, key_grant_rid: u64, envelope_json: &str) -> CloudResult<()> {
        self.objects(&format!("KeyGrant.get({key_grant_rid}).update({{ envelope: {} }})", ql_str(envelope_json))).await?;
        Ok(())
    }

    pub async fn find_key_grant(&self, user_rid: u64, store_rid: u64, key_id: &str) -> CloudResult<Option<KeyGrantRow>> {
        let q = format!("KeyGrant.filter(.user_rid == {user_rid} && .store_rid == {store_rid} && .key_id == {})", ql_str(key_id));
        self.one(&q).await?.map(|o| key_grant_from_object(&o)).transpose()
    }

    /// One user's own envelopes for one store — what `GET /stores/{id}/keys`
    /// returns (the caller's, never another member's).
    pub async fn key_grants_for_user_and_store(&self, user_rid: u64, store_rid: u64) -> CloudResult<Vec<KeyGrantRow>> {
        let q = format!("KeyGrant.filter(.user_rid == {user_rid} && .store_rid == {store_rid})");
        self.objects(&q).await?.iter().map(key_grant_from_object).collect()
    }
}
