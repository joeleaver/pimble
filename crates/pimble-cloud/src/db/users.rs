//! Accounts: users, their keys and recovery, display and share names.

use super::*;

impl RhypeDb {
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
        // Trimmed like `GET /kdf` trims: a pasted address with a stray space
        // must find the same row.
        let email_lower = email.trim().to_lowercase();
        let q = format!("User.filter(.email_lower == {})", ql_str(&email_lower));
        self.one(&q).await?.map(|o| user_from_object(&o)).transpose()
    }

    // ── Recovery (docs/CRYPTO_CONTRACT.md "Phase 2a-2") ──────────────────

    /// (Re)issues a recovery token for `user_rid` (`POST /recover/start`).
    pub async fn set_recovery_token(&self, user_rid: u64, recovery_token_hash: &str, expires_at_ms: i64) -> CloudResult<()> {
        let q = format!(
            "User.get({user_rid}).update({{ recovery_token_hash: {hash}, recovery_token_expires_at: {exp} }})",
            hash = ql_str(recovery_token_hash),
            exp = datetime_literal(expires_at_ms),
        );
        self.objects(&q).await?;
        Ok(())
    }

    /// Clears `user_rid`'s recovery token so it can't be replayed — used
    /// whenever it's consumed (`.../complete`, `.../delete-account`).
    pub async fn clear_recovery_token(&self, user_rid: u64) -> CloudResult<()> {
        self.objects(&format!("User.get({user_rid}).update({{ recovery_token_hash: {empty} }})", empty = ql_str(""))).await?;
        Ok(())
    }

    /// Looks up a user by the hash of a recovery token from a
    /// `/recover/{token}` link. The caller must never pass an empty
    /// `recovery_token_hash` — same reasoning as
    /// [`Self::find_user_by_verify_token_hash`].
    pub async fn find_user_by_recovery_token_hash(&self, recovery_token_hash: &str) -> CloudResult<Option<UserRow>> {
        debug_assert!(!recovery_token_hash.is_empty(), "must not query the empty recovery_token_hash sentinel");
        let q = format!("User.filter(.recovery_token_hash == {})", ql_str(recovery_token_hash));
        self.one(&q).await?.map(|o| user_from_object(&o)).transpose()
    }

    /// Replaces `user_rid`'s password material — `POST /me/password`
    /// (current session kept) and the password half of
    /// `POST /recover/{token}/complete` both go through this. Public keys
    /// and recovery material are untouched.
    pub async fn update_password_material(
        &self,
        user_rid: u64,
        password_hash: &str,
        kdf_salt: &str,
        kdf_m_cost: u32,
        kdf_t_cost: u32,
        kdf_p_cost: u32,
        account_key_blob: &str,
    ) -> CloudResult<()> {
        let q = format!(
            "User.get({user_rid}).update({{ password_hash: {hash}, kdf_salt: {salt}, kdf_m_cost: {m}, kdf_t_cost: {t}, kdf_p_cost: {p}, account_key_blob: {blob} }})",
            hash = ql_str(password_hash),
            salt = ql_str(kdf_salt),
            m = kdf_m_cost,
            t = kdf_t_cost,
            p = kdf_p_cost,
            blob = ql_str(account_key_blob),
        );
        self.objects(&q).await?;
        Ok(())
    }

    /// Replaces `user_rid`'s recovery material only — `POST
    /// /me/recovery-code` (a new code generated while already holding the
    /// keys).
    pub async fn update_recovery_material(&self, user_rid: u64, recovery_salt: &str, recovery_key_blob: &str) -> CloudResult<()> {
        let q = format!(
            "User.get({user_rid}).update({{ recovery_salt: {salt}, recovery_key_blob: {blob} }})",
            salt = ql_str(recovery_salt),
            blob = ql_str(recovery_key_blob),
        );
        self.objects(&q).await?;
        Ok(())
    }

    /// `POST /recover/{token}/complete`: password material and recovery
    /// material both rotate (the client generates a new recovery code too),
    /// and the token that got the caller here is consumed — all in the one
    /// `.update()` call so there's no window where only some of it landed.
    /// Public keys are unchanged (docs/CRYPTO_CONTRACT.md: "so every store
    /// envelope stays valid").
    #[allow(clippy::too_many_arguments)]
    pub async fn complete_recovery(
        &self,
        user_rid: u64,
        password_hash: &str,
        kdf_salt: &str,
        kdf_m_cost: u32,
        kdf_t_cost: u32,
        kdf_p_cost: u32,
        account_key_blob: &str,
        recovery_salt: &str,
        recovery_key_blob: &str,
    ) -> CloudResult<()> {
        let q = format!(
            "User.get({user_rid}).update({{ password_hash: {hash}, kdf_salt: {salt}, kdf_m_cost: {m}, kdf_t_cost: {t}, kdf_p_cost: {p}, account_key_blob: {blob}, recovery_salt: {rsalt}, recovery_key_blob: {rblob}, recovery_token_hash: {empty} }})",
            hash = ql_str(password_hash),
            salt = ql_str(kdf_salt),
            m = kdf_m_cost,
            t = kdf_t_cost,
            p = kdf_p_cost,
            blob = ql_str(account_key_blob),
            rsalt = ql_str(recovery_salt),
            rblob = ql_str(recovery_key_blob),
            empty = ql_str(""),
        );
        self.objects(&q).await?;
        Ok(())
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

    pub async fn set_display_name(&self, user_rid: u64, display_name: &str) -> CloudResult<()> {
        self.objects(&format!("User.get({user_rid}).update({{ display_name: {} }})", ql_str(display_name))).await?;
        Ok(())
    }

    // ── Names in shares (docs/PRESENCE_CONTRACT.md section 3) ─────────

    /// Every name one person has chosen, in any share of any store.
    pub async fn share_names_for_user(&self, user_rid: u64) -> CloudResult<Vec<ShareNameRow>> {
        let q = format!("ShareName.filter(.user_rid == {user_rid})");
        self.objects(&q).await?.iter().map(share_name_from_object).collect()
    }

    /// Every name chosen by anyone in any share of one store.
    pub async fn share_names_for_store(&self, store_rid: u64) -> CloudResult<Vec<ShareNameRow>> {
        let q = format!("ShareName.filter(.store_rid == {store_rid})");
        self.objects(&q).await?.iter().map(share_name_from_object).collect()
    }

    /// Set (or, with an empty `name`, clear) what `user_rid` goes by in the
    /// share of `store_rid` rooted at `root`. One row per (user, store,
    /// root): an existing one is rewritten.
    pub async fn put_share_name(&self, user_rid: u64, store_rid: u64, store_uuid: &str, root: &str, name: &str) -> CloudResult<()> {
        let existing = self
            .share_names_for_user(user_rid)
            .await?
            .into_iter()
            .find(|row| row.store_rid == store_rid && row.root == root);
        match (existing, name.is_empty()) {
            (Some(row), true) => {
                self.objects(&format!("ShareName.get({}).delete()", row.rid)).await?;
            }
            (Some(row), false) => {
                self.objects(&format!("ShareName.get({}).update({{ name: {} }})", row.rid, ql_str(name))).await?;
            }
            (None, true) => {}
            (None, false) => {
                let q = format!(
                    "ShareName.create({{ user_rid: {user_rid}, store_rid: {store_rid}, store_uuid: {suuid}, root: {root}, name: {name} }})",
                    suuid = ql_str(store_uuid),
                    root = ql_str(root),
                    name = ql_str(name),
                );
                let obj = self
                    .objects(&q)
                    .await?
                    .into_iter()
                    .next()
                    .ok_or_else(|| CloudError::Internal("ShareName.create returned nothing".into()))?;
                let row = share_name_from_object(&obj)?;
                self.link("ShareName", row.rid, "User", user_rid).await?;
                self.link("ShareName", row.rid, "HostedStore", store_rid).await?;
            }
        }
        Ok(())
    }

    /// Every name an account chose, deleted with the account.
    pub async fn delete_share_names_for_user(&self, user_rid: u64) -> CloudResult<()> {
        for row in self.share_names_for_user(user_rid).await? {
            self.objects(&format!("ShareName.get({}).delete()", row.rid)).await?;
        }
        Ok(())
    }

    /// Every chosen name on a store, deleted with it.
    pub async fn delete_share_names_for_store(&self, store_rid: u64) -> CloudResult<()> {
        for row in self.share_names_for_store(store_rid).await? {
            self.objects(&format!("ShareName.get({}).delete()", row.rid)).await?;
        }
        Ok(())
    }
}
