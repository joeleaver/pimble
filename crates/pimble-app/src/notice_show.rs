//! A notice shown as the system's notification (docs/NOTIFICATIONS_CONTRACT.md
//! section 5, "Built in wave 4"): rinch's `notifications` on the desktop,
//! the Notifications API in the browser. A watch notice whose note this
//! device has not named yet waits for the name (`crate::inbox::named`, or a
//! view loading the note) up to [`NAME_WAIT_MS`], and is shown once, by
//! whichever comes first. Whatever shows it asks [`still_shows`] at that
//! moment: a window that resigned the tray during the wait, one quitting,
//! or a notice read on another device meanwhile shows nothing.

use pimble_core::{NodeId, StoreId};
use pimble_rpc::{Notice, NoticeKind};
use rinch::prelude::*;

use crate::state::AppStore;

thread_local! {
    /// Watch notices waiting for the name of their note before the system
    /// shows them.
    static AWAITING_NAME: std::cell::RefCell<Vec<Notice>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// How long a watch notice waits for its note's name before it is shown
/// without it.
const NAME_WAIT_MS: u32 = 3000;

/// Show `notice` as the system's notification: at once, or, for a watch
/// notice whose note is not named yet, when its name arrives
/// ([`names_arrived`]) or the wait for it runs out. Every show then goes
/// through `show_system_notification`, which on the desktop asks
/// `crate::shown_notices` first, so none is shown twice.
pub(crate) fn show(store: AppStore, notice: Notice) {
    let named = notice.kind != NoticeKind::Watch || untracked(|| crate::inbox::name_of(store, notice.store, notice.node)).is_some();
    if named {
        show_if_still(store, &notice);
        return;
    }
    let id = notice.id.clone();
    await_name(notice);
    rinch::core::reactive::unowned(|| {
        let _ = set_timeout(NAME_WAIT_MS, move || {
            for late in timed_out(&id) {
                show_if_still(store, &late);
            }
        });
    });
}

/// The names of `notes` arrived, or this device found it cannot read them:
/// the watch notices waiting for them are shown now.
pub(crate) fn names_arrived(store: AppStore, notes: &[(StoreId, NodeId)]) {
    for ready in name_came(notes) {
        show_if_still(store, &ready);
    }
}

fn await_name(notice: Notice) {
    AWAITING_NAME.with(|a| a.borrow_mut().push(notice));
}

/// The notice `id`'s wait ran out: it, if it still waits.
fn timed_out(id: &str) -> Vec<Notice> {
    take_awaiting(|n| n.id == id)
}

/// The notices waiting for one of `notes`' names.
fn name_came(notes: &[(StoreId, NodeId)]) -> Vec<Notice> {
    take_awaiting(|n| notes.contains(&(n.store, n.node)))
}

fn take_awaiting(which: impl Fn(&Notice) -> bool) -> Vec<Notice> {
    AWAITING_NAME.with(|a| {
        let mut all = a.borrow_mut();
        let (taken, kept): (Vec<Notice>, Vec<Notice>) = all.drain(..).partition(|n| which(n));
        *all = kept;
        taken
    })
}

fn show_if_still(store: AppStore, notice: &Notice) {
    if still_shows(store, notice) {
        show_system_notification(store, notice);
    }
}

/// Whether `notice` is still to be shown here, now.
pub(crate) fn still_shows(store: AppStore, notice: &Notice) -> bool {
    shows(notifies_here(), quitting(), untracked(|| crate::inbox::is_read(store, notice)))
}

/// Shown only by the window that notifies, while it is not quitting, and
/// only while the person has not read it anywhere.
fn shows(notifies_here: bool, quitting: bool, read: bool) -> bool {
    notifies_here && !quitting && !read
}

/// Whether this process shows the system's notifications: on the desktop
/// only the window process that owns the tray (`crate::instance`), so two
/// windows never notify twice; a browser tab always (the browser itself
/// files them per site). Every window keeps the notice in its Inbox all the
/// same.
#[cfg(feature = "native")]
pub(crate) fn notifies_here() -> bool {
    crate::instance::is_primary()
}

#[cfg(not(feature = "native"))]
pub(crate) fn notifies_here() -> bool {
    true
}

#[cfg(feature = "native")]
fn quitting() -> bool {
    crate::instance::is_quitting()
}

#[cfg(not(feature = "native"))]
fn quitting() -> bool {
    false
}

#[cfg(feature = "native")]
fn show_system_notification(store: AppStore, notice: &Notice) {
    // Asked and written down first, in one place: a notice another window
    // (or this one) showed is not shown again, and a window taking the tray
    // over never shows this one again.
    if !crate::shown_notices::first_show(&notice.id) {
        return;
    }
    let clicked = notice.clone();
    let shown = rinch::notification::Notification::new(crate::inbox::title_of(notice))
        .body(crate::inbox::body_of(store, notice))
        .icon_png(include_bytes!("../../../assets/brand/png/pimble-256.png"))
        .on_click(move || {
            crate::desktop::bring_to_front(None);
            crate::inbox::open(store, &clicked);
        })
        .on_error(|e| tracing::info!("A notice was not shown by the system: {e:?}"))
        .show();
    if let Err(e) = shown {
        tracing::info!("A notice was not shown by the system: {e:?}");
    }
}

#[cfg(all(feature = "web", not(feature = "native")))]
fn show_system_notification(store: AppStore, notice: &Notice) {
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;
    if web_sys::Notification::permission() != web_sys::NotificationPermission::Granted {
        return;
    }
    let options = web_sys::NotificationOptions::new();
    options.set_body(&crate::inbox::body_of(store, notice));
    options.set_tag(&notice.id);
    options.set_icon("/app/pimble-icon.svg");
    let Ok(shown) = web_sys::Notification::new_with_options(&crate::inbox::title_of(notice), &options) else { return };
    let clicked = notice.clone();
    let shown_again = shown.clone();
    let onclick = Closure::<dyn FnMut()>::new(move || {
        if let Some(window) = web_sys::window() {
            let _ = window.focus();
        }
        shown_again.close();
        crate::inbox::open(store, &clicked);
    });
    shown.set_onclick(Some(onclick.as_ref().unchecked_ref()));
    onclick.forget();
}

#[cfg(not(any(feature = "native", feature = "web")))]
fn show_system_notification(_store: AppStore, _notice: &Notice) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn watch_notice(id: &str) -> Notice {
        Notice { id: id.into(), kind: NoticeKind::Watch, from: "joe".into(), from_name: "Joe".into(), store: StoreId::new(), node: NodeId::new(), discussion: None, anchor: None, snippet: String::new(), at: "2026-11-23T10:00:00.000Z".into(), count: 1 }
    }

    /// The name arrives first: the notice goes then, and the wait running
    /// out later finds nothing.
    #[test]
    fn a_name_that_arrives_shows_the_notice_and_the_wait_finds_nothing() {
        let (a, other) = (watch_notice("a"), watch_notice("other"));
        await_name(a.clone());
        await_name(other.clone());
        assert_eq!(name_came(&[(a.store, a.node)]), vec![a.clone()]);
        assert!(timed_out("a").is_empty(), "never shown twice");
        assert!(name_came(&[(a.store, a.node)]).is_empty());
        assert_eq!(timed_out("other"), vec![other], "the other still waited");
    }

    /// The wait runs out first: the notice goes without its name, and the
    /// name arriving later shows nothing.
    #[test]
    fn a_wait_that_runs_out_shows_the_notice_and_the_name_finds_nothing() {
        let b = watch_notice("b");
        await_name(b.clone());
        assert_eq!(timed_out("b"), vec![b.clone()]);
        assert!(name_came(&[(b.store, b.node)]).is_empty(), "never shown twice");
    }

    /// Asked again when it goes: a window that resigned or is quitting, or
    /// a notice read meanwhile on another device, shows nothing.
    #[test]
    fn a_notice_shows_only_from_the_window_that_notifies_and_only_unread() {
        assert!(shows(true, false, false));
        assert!(!shows(false, false, false), "this window resigned the tray during the wait");
        assert!(!shows(true, true, false), "this window is quitting");
        assert!(!shows(true, false, true), "read on another device during the wait");
    }
}
