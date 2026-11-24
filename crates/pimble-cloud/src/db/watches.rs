//! Watches (docs/NOTIFICATIONS_CONTRACT.md section 3, wave 3): the sealed
//! choices, the registered documents, and the writes gathered per watcher.

use super::*;

/// One document a person watches (`WatchedDoc`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedDocRow {
    pub rid: u64,
    pub user_uuid: String,
    pub store_uuid: String,
    pub doc_uuid: String,
}

fn watched_doc_from_object(o: &Object) -> CloudResult<WatchedDocRow> {
    Ok(WatchedDocRow {
        rid: o.id,
        user_uuid: get_string(o, "user_uuid")?.to_string(),
        store_uuid: get_string(o, "store_uuid")?.to_string(),
        doc_uuid: get_string(o, "doc_uuid")?.to_string(),
    })
}

/// Writes gathered for one watcher, per (document, writer) (`WatchBatch`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchBatchRow {
    pub rid: u64,
    pub batch_key: String,
    pub watcher_uuid: String,
    pub store_uuid: String,
    pub doc_uuid: String,
    pub writer_uuid: String,
    /// The share the watcher was told through: `""` for the whole store.
    pub root: String,
    /// Spells of editing (`pimble_core::watch::Gathered::add_spell`).
    pub count: u64,
    pub first_at_ms: i64,
    pub last_at_ms: i64,
}

impl WatchBatchRow {
    /// "watcher/store/doc/writer": one batch per such key.
    pub fn key(watcher: &str, store: &str, doc: &str, writer: &str) -> String {
        format!("{watcher}/{store}/{doc}/{writer}")
    }

    /// What `pimble_core::watch` judges a batch by.
    pub fn gathered(&self) -> pimble_core::watch::Gathered {
        pimble_core::watch::Gathered { count: u32::try_from(self.count).unwrap_or(u32::MAX), first_ms: self.first_at_ms, last_ms: self.last_at_ms }
    }
}

fn watch_batch_from_object(o: &Object) -> CloudResult<WatchBatchRow> {
    Ok(WatchBatchRow {
        rid: o.id,
        batch_key: get_string(o, "batch_key")?.to_string(),
        watcher_uuid: get_string(o, "watcher_uuid")?.to_string(),
        store_uuid: get_string(o, "store_uuid")?.to_string(),
        doc_uuid: get_string(o, "doc_uuid")?.to_string(),
        writer_uuid: get_string(o, "writer_uuid")?.to_string(),
        root: get_string(o, "root")?.to_string(),
        count: get_u64(o, "count")?,
        first_at_ms: get_datetime_ms(o, "first_at")?,
        last_at_ms: get_datetime_ms(o, "last_at")?,
    })
}

impl RhypeDb {
    // ── What a person chose ───────────────────────────────────────────

    /// The sealed choices: `(rid, version, sealed)`.
    pub async fn get_watch_choices(&self, user_uuid: &str) -> CloudResult<Option<(u64, u64, String)>> {
        let q = format!("WatchChoices.filter(.user_uuid == {})", ql_str(user_uuid));
        match self.one(&q).await? {
            Some(o) => Ok(Some((o.id, get_u64(&o, "version")?, get_string(&o, "sealed")?.to_string()))),
            None => Ok(None),
        }
    }

    /// Write them as `version` (the caller has checked the one replaced).
    pub async fn put_watch_choices(&self, user_uuid: &str, existing: Option<u64>, version: u64, sealed: &str) -> CloudResult<()> {
        let q = match existing {
            Some(rid) => format!("WatchChoices.get({rid}).update({{ sealed: {}, version: {version}, updated_at: {} }})", ql_str(sealed), now_literal()),
            None => format!("WatchChoices.create({{ user_uuid: {}, sealed: {}, version: {version}, updated_at: {} }})", ql_str(user_uuid), ql_str(sealed), now_literal()),
        };
        self.objects(&q).await?;
        Ok(())
    }

    // ── What their devices registered ─────────────────────────────────

    pub async fn watched_docs_of(&self, user_uuid: &str) -> CloudResult<Vec<WatchedDocRow>> {
        let q = format!("WatchedDoc.filter(.user_uuid == {})", ql_str(user_uuid));
        self.objects(&q).await?.iter().map(watched_doc_from_object).collect()
    }

    /// Every document registered in `store_uuid`, everyone's.
    pub async fn watched_docs_in(&self, store_uuid: &str) -> CloudResult<Vec<WatchedDocRow>> {
        let q = format!("WatchedDoc.filter(.store_uuid == {})", ql_str(store_uuid));
        self.objects(&q).await?.iter().map(watched_doc_from_object).collect()
    }

    /// Every document registered, everyone's in every store.
    pub async fn all_watched_docs(&self) -> CloudResult<Vec<WatchedDocRow>> {
        self.objects("WatchedDoc").await?.iter().map(watched_doc_from_object).collect()
    }

    /// Everyone watching `doc_uuid` of `store_uuid`.
    pub async fn watchers_of(&self, store_uuid: &str, doc_uuid: &str) -> CloudResult<Vec<WatchedDocRow>> {
        let q = format!("WatchedDoc.filter(.doc_uuid == {})", ql_str(doc_uuid));
        Ok(self.objects(&q).await?.iter().map(watched_doc_from_object).collect::<CloudResult<Vec<_>>>()?.into_iter().filter(|w| w.store_uuid == store_uuid).collect())
    }

    pub async fn create_watched_doc(&self, user_uuid: &str, store_uuid: &str, doc_uuid: &str) -> CloudResult<()> {
        let q = format!("WatchedDoc.create({{ user_uuid: {}, store_uuid: {}, doc_uuid: {}, created_at: {} }})", ql_str(user_uuid), ql_str(store_uuid), ql_str(doc_uuid), now_literal());
        self.objects(&q).await?;
        Ok(())
    }

    pub async fn delete_watched_doc(&self, rid: u64) -> CloudResult<()> {
        delete_quietly(self, &format!("WatchedDoc.get({rid}).delete()")).await
    }

    // ── What was gathered ─────────────────────────────────────────────

    pub async fn find_watch_batch(&self, batch_key: &str) -> CloudResult<Option<WatchBatchRow>> {
        let q = format!("WatchBatch.filter(.batch_key == {})", ql_str(batch_key));
        self.one(&q).await?.as_ref().map(watch_batch_from_object).transpose()
    }

    /// A write by `writer` to `doc` at `at_ms`, gathered for `watcher`
    /// (told through `root`): a new batch, or another spell of editing in
    /// the one gathering when it is half a minute or more from the rest.
    /// A read and then a write: the caller holds `AppState::watches_lock`,
    /// as `crate::watches::watch_pass` does to take batches out.
    pub async fn gather_watch_write(&self, watcher: &str, store: &str, doc: &str, writer: &str, root: &str, at_ms: i64) -> CloudResult<()> {
        let key = WatchBatchRow::key(watcher, store, doc, writer);
        let q = match self.find_watch_batch(&key).await? {
            Some(batch) => {
                let mut gathered = batch.gathered();
                gathered.add_spell(at_ms);
                format!(
                    "WatchBatch.get({}).update({{ count: {}, first_at: {}, last_at: {}, root: {} }})",
                    batch.rid,
                    gathered.count,
                    datetime_literal(gathered.first_ms),
                    datetime_literal(gathered.last_ms),
                    ql_str(root),
                )
            }
            None => format!(
                "WatchBatch.create({{ batch_key: {}, watcher_uuid: {}, store_uuid: {}, doc_uuid: {}, writer_uuid: {}, root: {}, count: 1, first_at: {at}, last_at: {at} }})",
                ql_str(&key),
                ql_str(watcher),
                ql_str(store),
                ql_str(doc),
                ql_str(writer),
                ql_str(root),
                at = datetime_literal(at_ms),
            ),
        };
        self.objects(&q).await?;
        Ok(())
    }

    /// Every batch gathering, everyone's.
    pub async fn watch_batches(&self) -> CloudResult<Vec<WatchBatchRow>> {
        self.objects("WatchBatch").await?.iter().map(watch_batch_from_object).collect()
    }

    pub async fn watch_batches_of(&self, watcher: &str) -> CloudResult<Vec<WatchBatchRow>> {
        let q = format!("WatchBatch.filter(.watcher_uuid == {})", ql_str(watcher));
        self.objects(&q).await?.iter().map(watch_batch_from_object).collect()
    }

    pub async fn delete_watch_batch(&self, rid: u64) -> CloudResult<()> {
        delete_quietly(self, &format!("WatchBatch.get({rid}).delete()")).await
    }

    /// Everything of `user_uuid`'s watches: the registered documents, what
    /// was gathered for them, and the choices, emptied with their version
    /// moved on (never deleted: a version must never go back, or a device
    /// holding older choices could write them again). The switch, turned
    /// off; the caller holds `AppState::watches_lock`. Answers the stores
    /// whose documents were registered.
    pub async fn delete_watches_of(&self, user_uuid: &str) -> CloudResult<Vec<String>> {
        let mut stores = std::collections::BTreeSet::new();
        for doc in self.watched_docs_of(user_uuid).await? {
            self.delete_watched_doc(doc.rid).await?;
            stores.insert(doc.store_uuid);
        }
        for batch in self.watch_batches_of(user_uuid).await? {
            self.delete_watch_batch(batch.rid).await?;
        }
        if let Some((rid, version, sealed)) = self.get_watch_choices(user_uuid).await? {
            if !sealed.is_empty() {
                self.put_watch_choices(user_uuid, Some(rid), version + 1, "").await?;
            }
        }
        Ok(stores.into_iter().collect())
    }
}

/// A delete of a row another request deleted first is done all the same.
async fn delete_quietly(db: &RhypeDb, q: &str) -> CloudResult<()> {
    match db.client.query(q).await {
        Ok(_) => Ok(()),
        Err(e) if is_not_found(&e) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
