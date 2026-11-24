//! Notices (docs/NOTIFICATIONS_CONTRACT.md): notice keys and switches, inboxes,
//! push subscriptions, and personal documents.

use super::*;

impl RhypeDb {
    // ── Notices (docs/NOTIFICATIONS_CONTRACT.md) ─────────────────────

    pub async fn set_notice_key(&self, user_rid: u64, public_key: &str, envelope_json: &str) -> CloudResult<()> {
        let q = format!(
            "User.get({user_rid}).update({{ notice_public_key: {}, notice_key_envelope: {} }})",
            ql_str(public_key),
            ql_str(envelope_json)
        );
        self.objects(&q).await?;
        Ok(())
    }

    pub async fn set_notifications_off(&self, user_rid: u64, off: bool) -> CloudResult<()> {
        self.objects(&format!("User.get({user_rid}).update({{ notifications_off: {off} }})")).await?;
        Ok(())
    }

    /// The email and push switches (wave 2); `None` leaves one as it is.
    pub async fn set_notify_channels(&self, user_rid: u64, email_off: Option<bool>, push_off: Option<bool>) -> CloudResult<()> {
        let mut fields = Vec::new();
        if let Some(off) = email_off {
            fields.push(format!("notify_email_off: {off}"));
        }
        if let Some(off) = push_off {
            fields.push(format!("notify_push_off: {off}"));
        }
        if fields.is_empty() {
            return Ok(());
        }
        self.objects(&format!("User.get({user_rid}).update({{ {} }})", fields.join(", "))).await?;
        Ok(())
    }

    pub async fn set_notify_watches_off(&self, user_rid: u64, off: bool) -> CloudResult<()> {
        self.objects(&format!("User.get({user_rid}).update({{ notify_watches_off: {off} }})")).await?;
        Ok(())
    }

    pub async fn set_notice_emailed_at(&self, user_rid: u64, at_ms: i64) -> CloudResult<()> {
        self.objects(&format!("User.get({user_rid}).update({{ notice_emailed_at: {} }})", datetime_literal(at_ms))).await?;
        Ok(())
    }

    /// Queue a sealed notice; answers the new item.
    pub async fn create_inbox_item(&self, recipient: &str, sender: &str, store_uuid: &str, root: &str, sealed: &str, kind: &str) -> CloudResult<InboxItemRow> {
        self.create_inbox_item_at(recipient, sender, store_uuid, root, sealed, kind, chrono::Utc::now().timestamp_millis()).await
    }

    /// [`Self::create_inbox_item`] as if it were queued at `at_ms` (the
    /// email fallback's tests move time rather than wait for it).
    #[allow(clippy::too_many_arguments)]
    pub async fn create_inbox_item_at(&self, recipient: &str, sender: &str, store_uuid: &str, root: &str, sealed: &str, kind: &str, at_ms: i64) -> CloudResult<InboxItemRow> {
        let item_uuid = uuid::Uuid::new_v4().to_string();
        let q = format!(
            "InboxItem.create({{ item_uuid: {item}, recipient_uuid: {recipient}, sender_uuid: {sender}, store_uuid: {store}, root: {root}, sealed: {sealed}, created_at: {now}, delivered: false, delivered_at: {epoch}, kind: {kind}, emailed: false }})",
            kind = ql_str(kind),
            item = ql_str(&item_uuid),
            recipient = ql_str(recipient),
            sender = ql_str(sender),
            store = ql_str(store_uuid),
            root = ql_str(root),
            sealed = ql_str(sealed),
            now = datetime_literal(at_ms),
            epoch = datetime_literal(0),
        );
        let o = self.one(&q).await?.ok_or_else(|| CloudError::Internal("InboxItem.create returned nothing".into()))?;
        inbox_item_from_object(&o)
    }

    /// Queue a watch notice (section 3, wave 3): sealed by this service to
    /// the watcher, from `writer`, standing for `count` changes, under the
    /// scope `root` the watcher holds the store through (`""`: the whole).
    #[allow(clippy::too_many_arguments)]
    pub async fn create_watch_item_at(&self, item_uuid: &str, recipient: &str, writer: &str, store_uuid: &str, root: &str, sealed: &str, count: u64, at_ms: i64) -> CloudResult<InboxItemRow> {
        let q = format!(
            "InboxItem.create({{ item_uuid: {item}, recipient_uuid: {recipient}, sender_uuid: {sender}, store_uuid: {store}, root: {root}, sealed: {sealed}, created_at: {now}, delivered: false, delivered_at: {epoch}, kind: \"watch\", emailed: false, count: {count} }})",
            item = ql_str(item_uuid),
            recipient = ql_str(recipient),
            sender = ql_str(writer),
            store = ql_str(store_uuid),
            root = ql_str(root),
            sealed = ql_str(sealed),
            now = datetime_literal(at_ms),
            epoch = datetime_literal(0),
        );
        let o = self.one(&q).await?.ok_or_else(|| CloudError::Internal("InboxItem.create returned nothing".into()))?;
        inbox_item_from_object(&o)
    }

    /// Every item waiting for `recipient`, oldest first.
    pub async fn inbox_of(&self, recipient: &str) -> CloudResult<Vec<InboxItemRow>> {
        let q = format!("InboxItem.filter(.recipient_uuid == {})", ql_str(recipient));
        let mut rows: Vec<InboxItemRow> = self.objects(&q).await?.iter().map(inbox_item_from_object).collect::<CloudResult<_>>()?;
        rows.sort_by_key(|r| r.rid);
        Ok(rows)
    }

    pub async fn mark_inbox_item_delivered(&self, rid: u64) -> CloudResult<()> {
        self.objects(&format!("InboxItem.get({rid}).update({{ delivered: true, delivered_at: {} }})", now_literal())).await?;
        Ok(())
    }

    /// Every item no device has seen yet, across all inboxes: what the
    /// email fallback looks over.
    pub async fn unseen_inbox_items(&self) -> CloudResult<Vec<InboxItemRow>> {
        let mut rows: Vec<InboxItemRow> = self.objects("InboxItem.filter(.delivered == false)").await?.iter().map(inbox_item_from_object).collect::<CloudResult<_>>()?;
        rows.sort_by_key(|r| r.rid);
        Ok(rows)
    }

    pub async fn find_inbox_item(&self, item_uuid: &str) -> CloudResult<Option<InboxItemRow>> {
        let q = format!("InboxItem.filter(.item_uuid == {})", ql_str(item_uuid));
        self.one(&q).await?.map(|o| inbox_item_from_object(&o)).transpose()
    }

    pub async fn mark_inbox_item_emailed(&self, rid: u64) -> CloudResult<()> {
        match self.client.query(&format!("InboxItem.get({rid}).update({{ emailed: true }})")).await {
            Ok(_) => Ok(()),
            Err(e) if is_not_found(&e) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    // ── Push subscriptions (docs/NOTIFICATIONS_CONTRACT.md section 4) ──

    /// Keep `user_uuid`'s subscription at `endpoint`: one row per endpoint,
    /// whoever held it before (a browser signed in to another account).
    pub async fn put_push_subscription(&self, user_uuid: &str, endpoint: &str, p256dh: &str, auth: &str) -> CloudResult<()> {
        let existing = self.push_subscriptions_at(endpoint).await?;
        let mut kept = false;
        for row in existing {
            if !kept {
                let q = format!(
                    "PushSubscription.get({}).update({{ user_uuid: {}, p256dh: {}, auth: {} }})",
                    row.rid,
                    ql_str(user_uuid),
                    ql_str(p256dh),
                    ql_str(auth)
                );
                self.objects(&q).await?;
                kept = true;
            } else {
                self.delete_push_subscription(row.rid).await?;
            }
        }
        if !kept {
            let q = format!(
                "PushSubscription.create({{ user_uuid: {}, endpoint: {}, p256dh: {}, auth: {}, created_at: {} }})",
                ql_str(user_uuid),
                ql_str(endpoint),
                ql_str(p256dh),
                ql_str(auth),
                now_literal()
            );
            self.objects(&q).await?;
        }
        Ok(())
    }

    pub async fn push_subscriptions_at(&self, endpoint: &str) -> CloudResult<Vec<PushSubscriptionRow>> {
        let q = format!("PushSubscription.filter(.endpoint == {})", ql_str(endpoint));
        self.objects(&q).await?.iter().map(push_subscription_from_object).collect()
    }

    pub async fn push_subscriptions_of(&self, user_uuid: &str) -> CloudResult<Vec<PushSubscriptionRow>> {
        let q = format!("PushSubscription.filter(.user_uuid == {})", ql_str(user_uuid));
        let mut rows: Vec<PushSubscriptionRow> = self.objects(&q).await?.iter().map(push_subscription_from_object).collect::<CloudResult<_>>()?;
        rows.sort_by_key(|r| r.rid);
        Ok(rows)
    }

    pub async fn delete_push_subscription(&self, rid: u64) -> CloudResult<()> {
        match self.client.query(&format!("PushSubscription.get({rid}).delete()")).await {
            Ok(_) => Ok(()),
            Err(e) if is_not_found(&e) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn delete_inbox_item(&self, rid: u64) -> CloudResult<()> {
        match self.client.query(&format!("InboxItem.get({rid}).delete()")).await {
            Ok(_) => Ok(()),
            Err(e) if is_not_found(&e) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    // ── Personal documents ────────────────────────────────────────────

    /// An account's personal document: `(rid, version, blob)`.
    pub async fn get_personal(&self, user_uuid: &str) -> CloudResult<Option<(u64, u64, String)>> {
        let q = format!("PersonalDoc.filter(.user_uuid == {})", ql_str(user_uuid));
        match self.one(&q).await? {
            Some(o) => Ok(Some((o.id, get_u64(&o, "version")?, get_string(&o, "blob")?.to_string()))),
            None => Ok(None),
        }
    }

    /// Write it as `version` (the caller has checked the one it replaces).
    pub async fn put_personal(&self, user_uuid: &str, existing: Option<u64>, version: u64, blob: &str) -> CloudResult<()> {
        let q = match existing {
            Some(rid) => format!(
                "PersonalDoc.get({rid}).update({{ blob: {blob}, version: {version}, updated_at: {now} }})",
                blob = ql_str(blob),
                now = now_literal(),
            ),
            None => format!(
                "PersonalDoc.create({{ user_uuid: {user}, blob: {blob}, version: {version}, updated_at: {now} }})",
                user = ql_str(user_uuid),
                blob = ql_str(blob),
                now = now_literal(),
            ),
        };
        self.objects(&q).await?;
        Ok(())
    }

    pub async fn delete_personal(&self, rid: u64) -> CloudResult<()> {
        self.objects(&format!("PersonalDoc.get({rid}).delete()")).await?;
        Ok(())
    }
}
