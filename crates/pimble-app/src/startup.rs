//! The start, as a timeline in the log (docs/STARTUP_CONTRACT.md "Built"):
//! each phase once, with the milliseconds since the app began, so a slow
//! start says where its time went (and diagnostics carry it). "ready" is
//! every saved store open (or failed), every row this device left expanded
//! loaded, and every pane's document restored.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;

use rinch::prelude::untracked;

use crate::panes::PaneId;
use crate::state::AppStore;

thread_local! {
    static BEGAN: Cell<Option<f64>> = const { Cell::new(None) };
    static SAID: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static READY: Cell<bool> = const { Cell::new(false) };
    /// Whether any saved store was asked to open: "ready" waits for one.
    static OPENING_SEEN: Cell<bool> = const { Cell::new(false) };
}

/// The app began: the timeline counts from here. Later calls change nothing.
pub fn began() {
    BEGAN.with(|b| {
        if b.get().is_none() {
            b.set(Some(crate::boards::now_ms()));
        }
    });
}

/// Milliseconds since the app began.
pub fn elapsed_ms() -> u64 {
    began();
    let start = BEGAN.with(|b| b.get()).unwrap_or_default();
    (crate::boards::now_ms() - start).max(0.0) as u64
}

/// Say `phase` in the log once, with when.
pub fn mark(phase: &str) {
    if READY.with(|r| r.get()) && phase != "ready" {
        return;
    }
    let new = SAID.with(|said| said.borrow_mut().insert(phase.to_string()));
    if new {
        tracing::info!("startup: {phase} at +{} ms", elapsed_ms());
    }
}

/// Saved stores were asked to open.
pub fn opening() {
    OPENING_SEEN.with(|o| o.set(true));
}

/// After every batch of events: say "ready" the first time everything the
/// start restores is in.
pub fn check(store: AppStore) {
    if READY.with(|r| r.get()) || !OPENING_SEEN.with(|o| o.get()) {
        return;
    }
    let opening = untracked(|| store.opening_stores.with(|list| list.iter().any(|o| o.error.is_none())));
    if opening {
        return;
    }
    mark("every saved store open");
    if store.children_pending_any() {
        return;
    }
    mark("restored rows loaded");
    let restoring = PaneId::ALL.into_iter().any(|pane| untracked(|| store.pane(pane).restore.get()).is_some());
    if restoring {
        return;
    }
    READY.with(|r| r.set(true));
    mark("ready");
}
