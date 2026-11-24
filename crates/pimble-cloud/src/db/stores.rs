//! Sessions, hosted stores, grants, key grants and invitations.

use super::*;

impl RhypeDb {
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

    /// Move a session's expiry (renewal while it is used).
    pub async fn renew_session(&self, session_rid: u64, expires_at_ms: i64) -> CloudResult<()> {
        self.objects(&format!("Session.get({session_rid}).update({{ expires_at: {} }})", datetime_literal(expires_at_ms))).await?;
        Ok(())
    }

    pub async fn delete_session(&self, session_rid: u64) -> CloudResult<()> {
        self.objects(&format!("Session.get({session_rid}).delete()")).await?;
        Ok(())
    }

    // ── Hosted stores ──────────────────────────────────────────────────

    /// `tier` is [`TIER_HOSTED`] or [`TIER_RELAY`]. A relay-tier row has no
    /// directory on the hosted server, so its `dir_name` is the empty string.
    pub async fn create_hosted_store(&self, store_id: &str, name: &str, dir_name: &str, kind: &str, tier: &str) -> CloudResult<HostedStoreRow> {
        let q = format!(
            "HostedStore.create({{ store_id: {sid}, name: {name}, dir_name: {dir}, created_at: {now}, deleted: false, kind: {kind}, tier: {tier} }})",
            sid = ql_str(store_id),
            name = ql_str(name),
            dir = ql_str(dir_name),
            now = now_literal(),
            kind = ql_str(kind),
            tier = ql_str(tier),
        );
        let obj = self
            .objects(&q)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| CloudError::Internal("HostedStore.create returned nothing".into()))?;
        hosted_store_from_object(&obj)
    }

    /// Takes over a **deleted** row for a store that is being hosted or
    /// relayed again under the same id (`store_id` is `@unique`, and deleting
    /// a store only marks its row): every field a fresh row would get, the
    /// id kept. The caller has already checked the row is deleted and removed
    /// whatever still named it.
    pub async fn revive_hosted_store(&self, store_rid: u64, name: &str, dir_name: &str, kind: &str, tier: &str) -> CloudResult<HostedStoreRow> {
        let q = format!(
            "HostedStore.get({store_rid}).update({{ name: {name}, dir_name: {dir}, created_at: {now}, deleted: false, kind: {kind}, tier: {tier} }})",
            name = ql_str(name),
            dir = ql_str(dir_name),
            now = now_literal(),
            kind = ql_str(kind),
            tier = ql_str(tier),
        );
        self.objects(&q).await?;
        self.get_hosted_store(store_rid).await?.ok_or_else(|| CloudError::Internal("a revived HostedStore row is gone".into()))
    }

    /// Test-only: creates a `HostedStore` row shaped like one from the cut
    /// where a share was a vault store of its own — `share: true`, a field
    /// nothing writes or reads any more — so a test can reproduce "a
    /// production row carries it" without a pre-migration database snapshot.
    /// Real stores go through [`Self::create_hosted_store`], which never
    /// writes it (compare [`Self::create_legacy_user_without_keys_for_tests`]).
    /// Such a row also predates `tier`, so this writes none: it is the shape
    /// `hosted_store_from_object` must read as a hosted store.
    pub async fn create_hosted_store_with_legacy_share_for_tests(&self, store_id: &str, name: &str, dir_name: &str, kind: &str) -> CloudResult<HostedStoreRow> {
        let q = format!(
            "HostedStore.create({{ store_id: {sid}, name: {name}, dir_name: {dir}, created_at: {now}, deleted: false, kind: {kind}, share: true }})",
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

    /// `root` is the scope: `""` for the whole store, a node id for a share
    /// (docs/NODE_DOCUMENT_CONTRACT.md section 5). `share_name` is what the
    /// owner called that share, and `""` for a whole-store grant.
    pub async fn create_grant(
        &self,
        user_rid: u64,
        store_rid: u64,
        store_uuid: &str,
        role: &str,
        root: &str,
        share_name: &str,
    ) -> CloudResult<GrantRow> {
        let q = format!(
            "Grant.create({{ role: {role}, root: {root}, share_name: {share_name}, user_rid: {uid}, store_rid: {sid}, store_uuid: {suuid} }})",
            uid = user_rid,
            sid = store_rid,
            role = ql_str(role),
            root = ql_str(root),
            share_name = ql_str(share_name),
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

    /// Test-only: creates a `Grant` row shaped like every one in production
    /// before shares became scopes — neither `root` nor `share_name` present
    /// at all, not written as `""` — so a test can reproduce the shape
    /// [`grant_from_object`] must read as an unnamed whole-store grant.
    /// Compare [`Self::create_hosted_store_with_legacy_share_for_tests`].
    pub async fn create_grant_without_root_for_tests(&self, user_rid: u64, store_rid: u64, store_uuid: &str, role: &str) -> CloudResult<GrantRow> {
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

    /// Test-only: a scoped `Grant` with no `share_name` field at all — the
    /// shape a share written before a share had a name of its own would
    /// have, which [`grant_from_object`] must read as "no name" and
    /// `GET /stores` must show as "Shared folder" rather than as the store.
    pub async fn create_scoped_grant_without_share_name_for_tests(
        &self,
        user_rid: u64,
        store_rid: u64,
        store_uuid: &str,
        role: &str,
        root: &str,
    ) -> CloudResult<GrantRow> {
        let q = format!(
            "Grant.create({{ role: {role}, root: {root}, user_rid: {uid}, store_rid: {sid}, store_uuid: {suuid} }})",
            uid = user_rid,
            sid = store_rid,
            role = ql_str(role),
            root = ql_str(root),
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

    /// The grant for exactly this scope — `(user, store, root)`, the
    /// uniqueness this service enforces itself. A user may hold several
    /// grants on one store (the whole store, or one per share), so a caller
    /// asking "may they touch this?" wants
    /// [`Self::grants_for_user_and_store`] instead.
    pub async fn find_grant(&self, user_rid: u64, store_rid: u64, root: &str) -> CloudResult<Option<GrantRow>> {
        Ok(self
            .grants_for_user_and_store(user_rid, store_rid)
            .await?
            .into_iter()
            .find(|g| g.root == root))
    }

    /// Every grant `user_rid` holds on `store_rid`: at most one whole-store
    /// grant plus one per share. Filtered in this process rather than by the
    /// query, since a `root` absent from an older row cannot be matched by a
    /// scalar comparison at all (`.filter(.root == "")` skips it).
    pub async fn grants_for_user_and_store(&self, user_rid: u64, store_rid: u64) -> CloudResult<Vec<GrantRow>> {
        let q = format!("Grant.filter(.user_rid == {user_rid} && .store_rid == {store_rid})");
        self.objects(&q).await?.iter().map(grant_from_object).collect()
    }

    pub async fn update_grant_role(&self, grant_rid: u64, role: &str) -> CloudResult<()> {
        self.objects(&format!("Grant.get({grant_rid}).update({{ role: {} }})", ql_str(role))).await?;
        Ok(())
    }

    /// Renames the share this grant is of. A share has one name, so the
    /// caller rewrites every grant and invitation of the same `(store, root)`
    /// when the owner types a new one.
    pub async fn update_grant_share_name(&self, grant_rid: u64, share_name: &str) -> CloudResult<()> {
        self.objects(&format!("Grant.get({grant_rid}).update({{ share_name: {} }})", ql_str(share_name))).await?;
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

    /// `scope_root` names which key this envelope carries: `""` for the store
    /// key, a node id for that share's key.
    pub async fn create_key_grant(
        &self,
        user_rid: u64,
        store_rid: u64,
        store_uuid: &str,
        key_id: &str,
        scope_root: &str,
        envelope_json: &str,
    ) -> CloudResult<KeyGrantRow> {
        let q = format!(
            "KeyGrant.create({{ key_id: {kid}, scope_root: {scope}, envelope: {env}, user_rid: {uid}, store_rid: {sid}, store_uuid: {suuid} }})",
            kid = ql_str(key_id),
            scope = ql_str(scope_root),
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

    /// The envelope for exactly `(user, store, key_id, scope_root)` — the
    /// uniqueness this service enforces itself.
    pub async fn find_key_grant(&self, user_rid: u64, store_rid: u64, key_id: &str, scope_root: &str) -> CloudResult<Option<KeyGrantRow>> {
        Ok(self
            .key_grants_for_user_and_store(user_rid, store_rid)
            .await?
            .into_iter()
            .find(|g| g.key_id == key_id && g.scope_root == scope_root))
    }

    /// One user's own envelopes for one store, every scope — the caller's,
    /// never another member's. [`Self::key_grants_for_user_store_and_root`]
    /// is what an endpoint scoped to one share wants.
    pub async fn key_grants_for_user_and_store(&self, user_rid: u64, store_rid: u64) -> CloudResult<Vec<KeyGrantRow>> {
        let q = format!("KeyGrant.filter(.user_rid == {user_rid} && .store_rid == {store_rid})");
        self.objects(&q).await?.iter().map(key_grant_from_object).collect()
    }

    /// One user's envelopes for one scope of one store — what
    /// `GET /stores/{id}/keys?root=` returns and what `has_key` reads.
    /// Filtered in this process for the same reason
    /// [`Self::grants_for_user_and_store`] is: an absent field matches no
    /// scalar comparison.
    pub async fn key_grants_for_user_store_and_root(&self, user_rid: u64, store_rid: u64, scope_root: &str) -> CloudResult<Vec<KeyGrantRow>> {
        Ok(self
            .key_grants_for_user_and_store(user_rid, store_rid)
            .await?
            .into_iter()
            .filter(|g| g.scope_root == scope_root)
            .collect())
    }

    /// Every key grant `user_rid` holds, across every store — used only by
    /// account deletion (`POST /recover/{token}/delete-account`).
    pub async fn key_grants_for_user(&self, user_rid: u64) -> CloudResult<Vec<KeyGrantRow>> {
        let q = format!("KeyGrant.filter(.user_rid == {user_rid})");
        self.objects(&q).await?.iter().map(key_grant_from_object).collect()
    }

    pub async fn delete_key_grant(&self, key_grant_rid: u64) -> CloudResult<()> {
        self.objects(&format!("KeyGrant.get({key_grant_rid}).delete()")).await?;
        Ok(())
    }

    /// Every key grant on `store_rid`, whoever holds it — what deleting a
    /// store removes along with its grants.
    pub async fn key_grants_for_store(&self, store_rid: u64) -> CloudResult<Vec<KeyGrantRow>> {
        let q = format!("KeyGrant.filter(.store_rid == {store_rid})");
        self.objects(&q).await?.iter().map(key_grant_from_object).collect()
    }

    /// Deletes one member's envelopes for one scope of one store — what
    /// removing them from that scope does. They keep whatever they already
    /// decrypted; this only stops the service handing them the key again.
    /// Scoped, not store-wide: losing a share must not take away the store
    /// key of somebody who also holds a whole-store grant.
    pub async fn delete_key_grants_for_user_store_and_root(&self, user_rid: u64, store_rid: u64, scope_root: &str) -> CloudResult<()> {
        for grant in self.key_grants_for_user_store_and_root(user_rid, store_rid, scope_root).await? {
            self.delete_key_grant(grant.rid).await?;
        }
        Ok(())
    }

    /// Every key grant on `store_rid`, deleted one at a time (the query
    /// language has no bulk `.filter(...).delete()`), like
    /// [`Self::delete_grants_for_store`].
    pub async fn delete_key_grants_for_store(&self, store_rid: u64) -> CloudResult<()> {
        for grant in self.key_grants_for_store(store_rid).await? {
            self.delete_key_grant(grant.rid).await?;
        }
        Ok(())
    }

    // ── Invitations ────────────────────────────────────────────────────

    /// `root` is the scope invited to and `share_name` what the owner called
    /// it; both are carried into the grant when the address is claimed. `""`
    /// and `""` for the whole store.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_invitation(
        &self,
        store_rid: u64,
        store_uuid: &str,
        email: &str,
        role: &str,
        root: &str,
        share_name: &str,
        invited_by_rid: u64,
    ) -> CloudResult<InvitationRow> {
        let q = format!(
            "Invitation.create({{ store_rid: {sid}, store_uuid: {suuid}, email: {email}, email_lower: {email_lower}, role: {role}, root: {root}, share_name: {share_name}, invited_by_rid: {by}, created_at: {now} }})",
            sid = store_rid,
            suuid = ql_str(store_uuid),
            email = ql_str(email),
            email_lower = ql_str(&email.trim().to_lowercase()),
            role = ql_str(role),
            root = ql_str(root),
            share_name = ql_str(share_name),
            by = invited_by_rid,
            now = now_literal(),
        );
        let obj = self
            .objects(&q)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| CloudError::Internal("Invitation.create returned nothing".into()))?;
        let invitation = invitation_from_object(&obj)?;
        self.link("Invitation", invitation.rid, "HostedStore", store_rid).await?;
        Ok(invitation)
    }

    /// The invitation for one address on one scope of one store, if any — the
    /// find-then-create/update half of the `(store, email_lower, root)`
    /// uniqueness this service enforces itself. Filtered in this process for
    /// the same reason [`Self::grants_for_user_and_store`] is.
    pub async fn find_invitation(&self, store_rid: u64, email: &str, root: &str) -> CloudResult<Option<InvitationRow>> {
        let q = format!("Invitation.filter(.store_rid == {store_rid} && .email_lower == {})", ql_str(&email.trim().to_lowercase()));
        Ok(self.objects(&q).await?.iter().map(invitation_from_object).collect::<CloudResult<Vec<_>>>()?.into_iter().find(|i| i.root == root))
    }

    pub async fn invitations_for_store(&self, store_rid: u64) -> CloudResult<Vec<InvitationRow>> {
        let q = format!("Invitation.filter(.store_rid == {store_rid})");
        self.objects(&q).await?.iter().map(invitation_from_object).collect()
    }

    /// Every invitation standing for one address, across every store — what
    /// the claim path (verification, and every login) turns into grants.
    pub async fn invitations_for_email(&self, email: &str) -> CloudResult<Vec<InvitationRow>> {
        let q = format!("Invitation.filter(.email_lower == {})", ql_str(&email.trim().to_lowercase()));
        self.objects(&q).await?.iter().map(invitation_from_object).collect()
    }

    pub async fn update_invitation_role(&self, invitation_rid: u64, role: &str) -> CloudResult<()> {
        self.objects(&format!("Invitation.get({invitation_rid}).update({{ role: {} }})", ql_str(role))).await?;
        Ok(())
    }

    /// Renames the share this invitation is to — the counterpart of
    /// [`Self::update_grant_share_name`], so that an address invited before
    /// the owner renamed the share is told the new name when it claims.
    pub async fn update_invitation_share_name(&self, invitation_rid: u64, share_name: &str) -> CloudResult<()> {
        self.objects(&format!("Invitation.get({invitation_rid}).update({{ share_name: {} }})", ql_str(share_name))).await?;
        Ok(())
    }

    pub async fn delete_invitation(&self, invitation_rid: u64) -> CloudResult<()> {
        self.objects(&format!("Invitation.get({invitation_rid}).delete()")).await?;
        Ok(())
    }

    pub async fn delete_invitations_for_store(&self, store_rid: u64) -> CloudResult<()> {
        for invitation in self.invitations_for_store(store_rid).await? {
            self.delete_invitation(invitation.rid).await?;
        }
        Ok(())
    }
}
