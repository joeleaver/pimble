//! The personal document in the app (docs/DISCUSSIONS_CONTRACT.md sections
//! 8 and 8b): what this person has read in discussions and where they were
//! in each node, kept on this device (`persistence::load_personal`), saved a
//! moment after it changes. Never shared, never in a store.

use std::cell::{Cell, RefCell};

use pimble_discussion::Personal;
use rinch::prelude::*;

use crate::state::AppStore;

thread_local! {
    static DOC: RefCell<Option<Personal>> = const { RefCell::new(None) };
    static SAVE_DUE: Cell<bool> = const { Cell::new(false) };
    static SYNC_DUE: Cell<bool> = const { Cell::new(false) };
    static TICKING: Cell<bool> = const { Cell::new(false) };
}

/// How long after a change it is sent to the account's other devices, and
/// how often theirs are asked for.
const SYNC_AFTER_MS: u32 = 5000;
const SYNC_EVERY_MS: u32 = 60_000;

/// Run `f` with the personal document, read from this device the first time.
pub(crate) fn with<R>(f: impl FnOnce(&Personal) -> R) -> R {
    DOC.with(|d| {
        let mut d = d.borrow_mut();
        let doc = d.get_or_insert_with(|| Personal::load(&crate::persistence::load_personal()));
        f(doc)
    })
}

/// The document changed: what shows read state follows, and it is saved
/// about a second later (one save for a burst of changes).
pub(crate) fn changed(store: AppStore) {
    store.read_version.update(|v| *v += 1);
    sync_soon(store);
    if SAVE_DUE.with(|s| s.replace(true)) {
        return;
    }
    rinch::core::reactive::unowned(|| {
        set_timeout(1000, || {
            SAVE_DUE.with(|s| s.set(false));
            let bytes = with(|p| p.save());
            crate::persistence::save_personal(&bytes);
        })
    });
}

// Whether the document follows the account is the account's, not this
// device's: an emptied hosted copy says it is off (any device of theirs
// turned it off), anything else that it is on. Every device keeps asking
// while signed in and mirrors the answer in its switch.

/// The switch: off deletes the hosted copy; on sends this device's now.
pub(crate) fn set_sync_enabled(store: AppStore, on: bool) {
    crate::persistence::save_personal_sync(on);
    store.personal_sync.set(on);
    let state = with(|p| p.save());
    store.send(crate::protocol::BackendCommand::SyncPersonal { state, enabled: on, turn_on: on });
}

/// Send this device's copy now (signed in, or the timer), and ask again
/// every minute for the other devices'.
pub fn sync_now(store: AppStore) {
    let state = with(|p| p.save());
    store.send(crate::protocol::BackendCommand::SyncPersonal { state, enabled: true, turn_on: false });
    if !TICKING.with(|t| t.replace(true)) {
        tick(store);
    }
}

fn tick(store: AppStore) {
    rinch::core::reactive::unowned(|| {
        set_timeout(SYNC_EVERY_MS, move || {
            if untracked(|| store.cloud_signed_in.get()) {
                let state = with(|p| p.save());
                store.send(crate::protocol::BackendCommand::SyncPersonal { state, enabled: true, turn_on: false });
            }
            tick(store);
        })
    });
}

fn sync_soon(store: AppStore) {
    if !untracked(|| store.cloud_signed_in.get()) || !untracked(|| store.personal_sync.get()) || SYNC_DUE.with(|s| s.replace(true)) {
        return;
    }
    rinch::core::reactive::unowned(|| {
        set_timeout(SYNC_AFTER_MS, move || {
            SYNC_DUE.with(|s| s.set(false));
            let state = with(|p| p.save());
            store.send(crate::protocol::BackendCommand::SyncPersonal { state, enabled: true, turn_on: false });
        })
    });
}

/// The account's merged copy came back: it goes into this device's, and
/// what shows read state follows when it brought anything.
pub(crate) fn synced(store: AppStore, state: &[u8], off: bool) {
    if off {
        // Turned off here or on another device of theirs: the switch says so.
        if untracked(|| store.personal_sync.get()) {
            crate::persistence::save_personal_sync(false);
            store.personal_sync.set(false);
        }
        return;
    }
    if !untracked(|| store.personal_sync.get()) {
        // Turned on again on another device.
        crate::persistence::save_personal_sync(true);
        store.personal_sync.set(true);
    }
    let changed = with(|p| {
        let before = p.save();
        p.merge(state);
        p.save() != before
    });
    if changed {
        store.read_version.update(|v| *v += 1);
        crate::discussions::recompute_all(store);
        let bytes = with(|p| p.save());
        crate::persistence::save_personal(&bytes);
    }
}
