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
}

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
