//! The rows this service reads, and how each is read from an object.

use super::*;

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
    /// Phase 2a-2 (account recovery). Same empty-string/epoch-zero sentinel
    /// as `verify_token_hash`/`verify_expires_at_ms`: absent on every row
    /// from before this field existed, and cleared the same way once a
    /// recovery token is consumed or superseded.
    pub recovery_token_hash: String,
    pub recovery_token_expires_at_ms: i64,
    /// The name this person goes by where a share has none of their own for
    /// them (docs/PRESENCE_CONTRACT.md section 3). `""` until they choose
    /// one, and on every row from before the field.
    pub display_name: String,
    /// The notice key's public half and its envelope
    /// (docs/NOTIFICATIONS_CONTRACT.md section 2); `""` until one is made.
    pub notice_public_key: String,
    pub notice_key_envelope: String,
    /// Whether the person turned notifications off (section 6).
    pub notifications_off: bool,
    /// Whether the person turned the email, or the web push, channel off
    /// (section 6, wave 2).
    pub notify_email_off: bool,
    pub notify_push_off: bool,
    /// Whether the person turned watched notes off (section 3, wave 3).
    pub notify_watches_off: bool,
    /// When the last notice email went to them; `0` for never.
    pub notice_emailed_at_ms: i64,
}

impl UserRow {
    /// What this person is called where nothing more particular applies:
    /// their chosen name, else the local part of their email.
    pub fn default_name(&self) -> String {
        let chosen = self.display_name.trim();
        if !chosen.is_empty() {
            return chosen.to_string();
        }
        self.email.split('@').next().filter(|n| !n.is_empty()).unwrap_or("someone").to_string()
    }

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
    /// [`TIER_HOSTED`] or [`TIER_RELAY`] (docs/RELAY_CONTRACT.md). A row from
    /// before the field existed reads as hosted.
    pub tier: String,
}

impl HostedStoreRow {
    /// Whether nothing of this store is on Pimble Cloud: the owner's own
    /// machine serves it through the relay, and no code path may call the
    /// hosted Pimble server about it.
    pub fn is_relayed(&self) -> bool {
        self.tier == TIER_RELAY
    }
}

/// The store's encrypted twin lives on the hosted Pimble server. The default,
/// and what a row with no `tier` field is.
pub const TIER_HOSTED: &str = "hosted";
/// The store is served by its owner's machine through the relay; Pimble Cloud
/// holds a record of it (for grants and key envelopes) and nothing else.
pub const TIER_RELAY: &str = "relay";

/// One address invited to a store that has no verified account yet. Becomes a
/// [`GrantRow`] — with the same [`root`](Self::root) — the moment the address
/// is claimed.
#[derive(Debug, Clone)]
pub struct InvitationRow {
    pub rid: u64,
    pub store_rid: u64,
    pub store_uuid: String,
    /// As typed by the inviter (what the mail was addressed to); the lookup
    /// key is the lowercased copy the schema indexes.
    pub email: String,
    pub role: String,
    /// The scope invited to: `""` for the whole store, a node id for a share
    /// (docs/NODE_DOCUMENT_CONTRACT.md section 5). `""` for a row written
    /// before the field existed.
    pub root: String,
    /// What the owner called the share this invites to, carried into the
    /// grant when the address is claimed. `""` for a whole-store invitation
    /// (the store has a name of its own), and for a row written before the
    /// field existed.
    pub share_name: String,
    /// The `User.rid` of whoever sent it — may name a row that no longer
    /// exists (see schema.rhype's comment on the field).
    pub invited_by_rid: u64,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct GrantRow {
    pub rid: u64,
    pub user_rid: u64,
    pub store_rid: u64,
    pub store_uuid: String,
    pub role: String,
    /// The scope granted: `""` for the whole store, a node id for a share
    /// (docs/NODE_DOCUMENT_CONTRACT.md section 5). `""` for a row written
    /// before the field existed, which is what every such grant really is.
    pub root: String,
    /// What the owner called this share (Joe, 2026-09-21): what a recipient's
    /// store list shows instead of the store's own name. `""` for a
    /// whole-store grant, and for a row written before the field existed —
    /// a scoped row with no name is shown as "Shared folder" rather than
    /// falling back to the store's name, which is not the recipient's to see.
    pub share_name: String,
}

impl GrantRow {
    /// Whether this grant covers the whole store rather than one shared
    /// subtree — the only kind an owner ever is, and the only kind the
    /// last-owner rule counts.
    pub fn is_whole_store(&self) -> bool {
        self.root.is_empty()
    }
}

/// One member's envelope for one key id on one store (docs/CRYPTO_CONTRACT.md
/// "Data model additions": `KeyGrant`).
#[derive(Debug, Clone)]
pub struct KeyGrantRow {
    pub rid: u64,
    pub user_rid: u64,
    pub store_rid: u64,
    pub key_id: String,
    /// Which scope key this envelope carries: `""` for the store key, a node
    /// id for that share's key (docs/NODE_DOCUMENT_CONTRACT.md section 5).
    /// `""` for a row written before the field existed.
    pub scope_root: String,
    /// A `pimble_crypto::KeyEnvelope` as stored: a JSON string.
    pub envelope: String,
}

/// The name one person goes by in one share (docs/PRESENCE_CONTRACT.md
/// section 3).
#[derive(Debug, Clone)]
pub struct ShareNameRow {
    pub rid: u64,
    pub user_rid: u64,
    pub store_rid: u64,
    /// `""` for a whole-store grant, a node id for a share.
    pub root: String,
    pub name: String,
}

pub(super) fn user_from_object(o: &Object) -> CloudResult<UserRow> {
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
        recovery_token_hash: get_string_opt(o, "recovery_token_hash")?.map(str::to_string).unwrap_or_default(),
        recovery_token_expires_at_ms: get_datetime_ms_opt(o, "recovery_token_expires_at")?.unwrap_or(0),
        display_name: get_string_opt(o, "display_name")?.map(str::to_string).unwrap_or_default(),
        notice_public_key: get_string_opt(o, "notice_public_key")?.map(str::to_string).unwrap_or_default(),
        notice_key_envelope: get_string_opt(o, "notice_key_envelope")?.map(str::to_string).unwrap_or_default(),
        notifications_off: get_bool_opt(o, "notifications_off")?.unwrap_or(false),
        notify_email_off: get_bool_opt(o, "notify_email_off")?.unwrap_or(false),
        notify_push_off: get_bool_opt(o, "notify_push_off")?.unwrap_or(false),
        notify_watches_off: get_bool_opt(o, "notify_watches_off")?.unwrap_or(false),
        notice_emailed_at_ms: get_datetime_ms_opt(o, "notice_emailed_at")?.unwrap_or(0),
    })
}

/// One sealed notice in an inbox (`InboxItem`).
#[derive(Debug, Clone)]
pub struct InboxItemRow {
    pub rid: u64,
    pub item_uuid: String,
    pub recipient_uuid: String,
    pub sender_uuid: String,
    pub store_uuid: String,
    pub root: String,
    pub sealed: String,
    pub created_at_ms: i64,
    pub delivered: bool,
    pub delivered_at_ms: i64,
    /// What the sender says it is: `"mention"`, `"reply"` or `""`.
    pub kind: String,
    /// Whether the email fallback has told the recipient of it.
    pub emailed: bool,
    /// Wave 3: how many changes a `"watch"` item stands for; 0 otherwise.
    pub count: u64,
}

/// One browser's push subscription (`PushSubscription`).
#[derive(Debug, Clone)]
pub struct PushSubscriptionRow {
    pub rid: u64,
    pub user_uuid: String,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

pub(super) fn push_subscription_from_object(o: &Object) -> CloudResult<PushSubscriptionRow> {
    Ok(PushSubscriptionRow {
        rid: o.id,
        user_uuid: get_string(o, "user_uuid")?.to_string(),
        endpoint: get_string(o, "endpoint")?.to_string(),
        p256dh: get_string(o, "p256dh")?.to_string(),
        auth: get_string(o, "auth")?.to_string(),
    })
}

pub(super) fn inbox_item_from_object(o: &Object) -> CloudResult<InboxItemRow> {
    Ok(InboxItemRow {
        rid: o.id,
        item_uuid: get_string(o, "item_uuid")?.to_string(),
        recipient_uuid: get_string(o, "recipient_uuid")?.to_string(),
        sender_uuid: get_string(o, "sender_uuid")?.to_string(),
        store_uuid: get_string(o, "store_uuid")?.to_string(),
        root: get_string(o, "root")?.to_string(),
        sealed: get_string(o, "sealed")?.to_string(),
        created_at_ms: get_datetime_ms(o, "created_at")?,
        delivered: get_bool(o, "delivered")?,
        delivered_at_ms: get_datetime_ms(o, "delivered_at")?,
        kind: get_string_opt(o, "kind")?.unwrap_or("").to_string(),
        emailed: get_bool_opt(o, "emailed")?.unwrap_or(false),
        count: get_u64_opt(o, "count")?.unwrap_or(0),
    })
}

pub(super) fn session_from_object(o: &Object) -> CloudResult<SessionRow> {
    Ok(SessionRow { rid: o.id, user_rid: get_u64(o, "user_rid")?, expires_at_ms: get_datetime_ms(o, "expires_at")? })
}

/// The store kind a row with no `kind` field is treated as — the only kind
/// that existed before Phase 2a added the field.
const DEFAULT_STORE_KIND: &str = "plain";

pub(super) fn hosted_store_from_object(o: &Object) -> CloudResult<HostedStoreRow> {
    Ok(HostedStoreRow {
        rid: o.id,
        store_id: get_string(o, "store_id")?.to_string(),
        name: get_string(o, "name")?.to_string(),
        created_at_ms: get_datetime_ms(o, "created_at")?,
        deleted: get_bool(o, "deleted")?,
        // Added after rows already existed, so read optional with the value
        // the older row really has — the rule from the `User.verified`
        // crash-loop (this crate's README, "Migration and legacy rows"). A
        // store hosted before Phase 2a is plain. (`share`, from the cut where
        // a share was a store of its own, is not read at all any more; a
        // production row may still carry it and that is simply ignored.)
        kind: get_string_opt(o, "kind")?.unwrap_or(DEFAULT_STORE_KIND).to_string(),
        // The same rule again (docs/RELAY_CONTRACT.md: "what a row without
        // the field reads as"): every store recorded before the relay tier
        // existed is hosted.
        tier: get_string_opt(o, "tier")?.unwrap_or(TIER_HOSTED).to_string(),
    })
}

/// The scope a row with no scope field is treated as: the whole store, which
/// is what every grant, invitation and envelope written before shares became
/// scopes really is.
const WHOLE_STORE: &str = "";

/// The share name a row with no `share_name` field is treated as: none. A
/// whole-store row really has none, and a scoped row from before the field
/// existed is one whose name was never typed.
const NO_SHARE_NAME: &str = "";

pub(super) fn invitation_from_object(o: &Object) -> CloudResult<InvitationRow> {
    Ok(InvitationRow {
        rid: o.id,
        store_rid: get_u64(o, "store_rid")?,
        store_uuid: get_string(o, "store_uuid")?.to_string(),
        email: get_string(o, "email")?.to_string(),
        role: get_string(o, "role")?.to_string(),
        root: get_string_opt(o, "root")?.unwrap_or(WHOLE_STORE).to_string(),
        share_name: get_string_opt(o, "share_name")?.unwrap_or(NO_SHARE_NAME).to_string(),
        invited_by_rid: get_u64(o, "invited_by_rid")?,
        created_at_ms: get_datetime_ms(o, "created_at")?,
    })
}

pub(super) fn grant_from_object(o: &Object) -> CloudResult<GrantRow> {
    Ok(GrantRow {
        rid: o.id,
        user_rid: get_u64(o, "user_rid")?,
        store_rid: get_u64(o, "store_rid")?,
        store_uuid: get_string(o, "store_uuid")?.to_string(),
        role: get_string(o, "role")?.to_string(),
        root: get_string_opt(o, "root")?.unwrap_or(WHOLE_STORE).to_string(),
        share_name: get_string_opt(o, "share_name")?.unwrap_or(NO_SHARE_NAME).to_string(),
    })
}

pub(super) fn share_name_from_object(o: &Object) -> CloudResult<ShareNameRow> {
    Ok(ShareNameRow {
        rid: o.id,
        user_rid: get_u64(o, "user_rid")?,
        store_rid: get_u64(o, "store_rid")?,
        root: get_string(o, "root")?.to_string(),
        name: get_string(o, "name")?.to_string(),
    })
}

pub(super) fn key_grant_from_object(o: &Object) -> CloudResult<KeyGrantRow> {
    Ok(KeyGrantRow {
        rid: o.id,
        user_rid: get_u64(o, "user_rid")?,
        store_rid: get_u64(o, "store_rid")?,
        key_id: get_string(o, "key_id")?.to_string(),
        scope_root: get_string_opt(o, "scope_root")?.unwrap_or(WHOLE_STORE).to_string(),
        envelope: get_string(o, "envelope")?.to_string(),
    })
}
