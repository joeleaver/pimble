//! How [`Pimble`] hears the stores it holds: the subscriptions it drains
//! before every read, the changes it keeps for a listener, and the
//! subscription made again (with a catch-up of what was held) when one ends.

use super::*;

impl Pimble {
    // ── Changes ──────────────────────────────────────────────────────────

    /// Merge whatever the subscriptions have brought, without waiting.
    pub(crate) fn drain(&self) {
        let Some(mut subs) = self.subs.try_lock() else { return };
        for s in self.new_subs.lock().unwrap().drain(..) {
            subs.push(s);
        }
        while let Some(Some(event)) = subs.next().now_or_never() {
            self.process(event);
        }
    }

    fn process(&self, event: Event) {
        match event {
            Event::Change(change) => {
                let store = change.store;
                let rejudged = self.held.lock().unwrap().apply(&change);
                self.keep(Change::Store(change));
                if !rejudged.is_empty() {
                    self.keep(Change::Rejudged { store, nodes: rejudged });
                }
            }
            Event::Ended(store) => self.held.lock().unwrap().lost(store),
        }
    }

    fn keep(&self, change: Change) {
        if self.listening.load(Ordering::Relaxed) {
            self.queue.lock().unwrap().push_back(change);
        }
    }

    /// Keep every change from now on for [`Pimble::next_change`], including
    /// the ones reads merge on their way (a client that never asks for
    /// changes keeps none).
    pub fn listen(&self) {
        self.listening.store(true, Ordering::Relaxed);
    }

    /// The next change to a held store, merged into what is held before it
    /// is answered (and listening from the first call, [`Pimble::listen`]).
    /// With nothing subscribed it waits for a subscription to be made. A
    /// subscription that ends is made again when its store is watched
    /// ([`Pimble::watch`]) or held, and its store brought up to date, before
    /// the next change is answered (`Change::Resynced`); one that cannot be
    /// made now (the connection is down, the store not open there yet) is
    /// made at the next [`Pimble::watch`] or [`Pimble::rewatch`].
    pub async fn next_change(&self) -> Option<Change> {
        self.listen();
        loop {
            if let Some(change) = self.queue.lock().unwrap().pop_front() {
                return Some(change);
            }
            let event = {
                let mut subs = self.subs.lock().await;
                let mut woken = self.woken.lock().await;
                loop {
                    for s in self.new_subs.lock().unwrap().drain(..) {
                        subs.push(s);
                    }
                    if subs.is_empty() {
                        woken.next().await?;
                        continue;
                    }
                    match futures::future::select(subs.next(), woken.next()).await {
                        futures::future::Either::Left((Some(event), _)) => break event,
                        // Every subscription ended at once; wait for the next.
                        futures::future::Either::Left((None, _)) => continue,
                        futures::future::Either::Right((Some(()), _)) => continue,
                        futures::future::Either::Right((None, _)) => return None,
                    }
                }
            };
            match event {
                Event::Change(change) => {
                    let store = change.store;
                    let rejudged = self.held.lock().unwrap().apply(&change);
                    if !rejudged.is_empty() {
                        // After the change that caused it.
                        self.queue.lock().unwrap().push_back(Change::Rejudged { store, nodes: rejudged });
                    }
                    return Some(Change::Store(change));
                }
                Event::Ended(store) => {
                    self.held.lock().unwrap().lost(store);
                    if self.watched.lock().unwrap().contains(&store) {
                        let _ = self.ensure_live(store).await;
                    }
                }
            }
        }
    }

    /// Hear `store`'s changes from now on, through [`Pimble::next_change`],
    /// and keep hearing them across a lost connection: a subscription that
    /// ends is made again. Answers why it could not be made now, if it
    /// could not (it is made again at the next [`Pimble::rewatch`]). A store
    /// whose backend holds its documents itself is not subscribed to here:
    /// its backend says what changed in it.
    pub async fn watch(&self, store: StoreId) -> Result<(), ApiError> {
        self.watched.lock().unwrap().insert(store);
        self.subscribe_now(store).await
    }

    /// Stop hearing `store` (it was closed): what is held of it goes too.
    pub fn forget(&self, store: StoreId) {
        self.watched.lock().unwrap().remove(&store);
        self.held.lock().unwrap().held.remove(&store);
    }

    /// Make again every watched store's subscription that is not live (a new
    /// connection: the old one's subscriptions went with it).
    pub async fn rewatch(&self) {
        let watched: Vec<StoreId> = self.watched.lock().unwrap().iter().copied().collect();
        for store in watched {
            if let Err(e) = self.subscribe_now(store).await {
                tracing::debug!("store {store} could not be heard again yet: {e}");
            }
        }
    }

    /// Whether `store`'s backend holds its documents itself, so nothing of
    /// it is held here ([`crate::StoreCapabilities::local_documents`]).
    pub(crate) fn passes_through(&self, store: StoreId) -> bool {
        self.backend.capabilities(store).local_documents
    }

    /// Subscribe to `store` if what is held there is not kept current (the
    /// first read of it, or its subscription ended), and bring what is held
    /// up to date. A store that cannot be subscribed to is read afresh
    /// every time instead.
    pub(crate) async fn ensure_live(&self, store: StoreId) -> Result<(), ApiError> {
        if let Err(e) = self.subscribe_now(store).await {
            tracing::debug!("store {store} could not be subscribed to; reading it afresh: {e}");
        }
        Ok(())
    }

    /// [`Pimble::ensure_live`], answering why a subscription could not be
    /// made.
    async fn subscribe_now(&self, store: StoreId) -> Result<(), ApiError> {
        if self.passes_through(store) {
            return Ok(());
        }
        self.drain();
        let had = {
            let mut held = self.held.lock().unwrap();
            let h = held.store_mut(store);
            if h.live {
                return Ok(());
            }
            !h.tree.ids().is_empty()
        };
        if !self.subscribing.lock().unwrap().insert(store) {
            return Ok(());
        }
        let subscribed = self.backend.subscribe(store).await;
        self.subscribing.lock().unwrap().remove(&store);
        let stream = subscribed?;
        let tagged: Events = Box::pin(stream.map(Event::Change).chain(futures::stream::once(async move { Event::Ended(store) })));
        self.new_subs.lock().unwrap().push(tagged);
        let _ = self.wake.unbounded_send(());
        let have = {
            let mut held = self.held.lock().unwrap();
            let h = held.store_mut(store);
            h.live = true;
            // What the server judged may have changed while nothing was heard.
            for j in h.judged.values_mut() {
                j.stale = true;
            }
            if had {
                h.state_vectors()
            } else {
                Vec::new()
            }
        };
        if !have.is_empty() {
            let (docs, _) = self.backend.sync(store, have, false).await?;
            let mut held = self.held.lock().unwrap();
            let h = held.store_mut(store);
            let mut changed = Vec::new();
            for d in docs {
                if h.merge_changed(d.node, &d.diff) {
                    changed.push(d.node);
                }
            }
            drop(held);
            self.keep(Change::Resynced { store, changed });
        }
        Ok(())
    }
}
