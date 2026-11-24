//! The Inbox (docs/NOTIFICATIONS_CONTRACT.md sections 4 to 6): the notices
//! this person has received — mentions of them, replies to them and others'
//! changes to notes they watch (`crate::watches`) — shown
//! as they arrive in an operating-system notification (rinch's
//! `notifications` on the desktop, the Notifications API in the browser),
//! listed in View > "Inbox..." newest first with the unread on top, and
//! counted in the status bar. Opening one opens the note at the mention (or
//! the discussion), marks it read in the personal document so it clears on
//! the person's other devices, and acknowledges it to the accounts service.
//!
//! The server holds the notices (it drains the account's inbox and opens
//! them); this is only how they are shown.

use pimble_core::{NodeId, StoreId};
use pimble_rpc::{Notice, NoticeKind};
use rinch::prelude::*;

use crate::protocol::BackendCommand;
use crate::state::{AppStore, LinkOpen};

/// The line a notice is shown under: who, and what they did.
pub fn title_of(notice: &Notice) -> String {
    let who = if notice.from_name.trim().is_empty() { "Someone" } else { notice.from_name.trim() };
    match (notice.kind, notice.discussion.is_some()) {
        (NoticeKind::Mention, false) => format!("{who} mentioned you"),
        (NoticeKind::Mention, true) => format!("{who} mentioned you in a discussion"),
        (NoticeKind::Reply, _) => format!("{who} replied in a discussion"),
        (NoticeKind::Watch, _) => pimble_core::watch::watch_title(who, notice.count),
    }
}

/// What the system's notification says under the title: the words around a
/// mention or of a reply, or the name of the note a watch notice is about
/// (the accounts service never knows it; this device's own documents do,
/// read through the app's API: [`ask_for_names`]).
fn body_of(store: AppStore, notice: &Notice) -> String {
    match notice.kind {
        NoticeKind::Watch => untracked(|| name_of(store, notice.store, notice.node)).unwrap_or_else(|| UNNAMED_NOTE.to_string()),
        _ => notice.snippet.clone(),
    }
}

/// A watch notice's body when this device cannot read the note it names.
const UNNAMED_NOTE: &str = "A note you watch";

/// The name of a note a notice names, as the explorer shows it, when the
/// app holds it; tracked, so the Inbox's line fills in when it arrives.
fn name_of(store: AppStore, store_id: StoreId, node_id: NodeId) -> Option<String> {
    let node = store.node_data.with(|map| map.get(&(store_id, node_id)).copied())?;
    Some(node.with(crate::state::display_label_from_node))
}

/// The app holds only the notes some view showed. A notice can name any
/// note of the person's stores (a watched note deep in a folder nobody
/// opened, on a device that never showed it), so the ones it does not hold
/// are read from this device's documents (`BackendCommand::NameNodes`, the
/// shared command path on the desktop and in the browser); each arrives as
/// `NodeLoaded`, which names it in the Inbox and lets a notification waiting
/// for its name go ([`node_loaded`]).
fn ask_for_names(store: AppStore, notices: &[Notice]) {
    let held = |key: &(StoreId, NodeId)| store.get_node_signal(key.0, key.1).is_some();
    let nodes = unnamed(notices.iter().map(|n| (n.store, n.node)), held);
    if !nodes.is_empty() {
        store.send(BackendCommand::NameNodes { nodes });
    }
}

/// The notes in `named`, once each, that `held` says the app does not hold.
fn unnamed(named: impl Iterator<Item = (StoreId, NodeId)>, held: impl Fn(&(StoreId, NodeId)) -> bool) -> Vec<(StoreId, NodeId)> {
    let mut seen = std::collections::HashSet::new();
    named.filter(|key| !held(key) && seen.insert(*key)).collect()
}

/// What the status bar says of the notices not opened yet, worded for what
/// it counts; `None` when there are none.
pub(crate) fn unread_line(mentions: usize, replies: usize, watches: usize) -> Option<String> {
    let total = mentions + replies + watches;
    let plural = |n: usize, one: &str, many: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {many}") };
    Some(match (mentions, replies, watches) {
        (0, 0, 0) => return None,
        (_, 0, 0) => plural(mentions, "new mention", "new mentions"),
        (0, _, 0) => plural(replies, "new reply", "new replies"),
        (0, 0, _) => plural(watches, "change to a note you watch", "changes to notes you watch"),
        (_, _, 0) => format!("{total} new mentions and replies"),
        _ => plural(total, "new notice", "new notices"),
    })
}

/// The status bar's line for the notices not opened yet; tracked.
pub fn unread_status(store: AppStore) -> Option<String> {
    let notices = store.notices.get();
    let unread: Vec<&Notice> = notices.iter().filter(|n| !is_read(store, n)).collect();
    let of = |kind: NoticeKind| unread.iter().filter(|n| n.kind == kind).count();
    unread_line(of(NoticeKind::Mention), of(NoticeKind::Reply), of(NoticeKind::Watch))
}

/// Whether the person has opened `notice`, on any of their devices; tracked
/// on the personal document.
pub(crate) fn is_read(store: AppStore, notice: &Notice) -> bool {
    store.read_version.get();
    crate::personal::with(|p| p.notice_read(&notice.id))
}

/// How many notices wait unopened (the status bar's count); tracked.
pub fn unread_count(store: AppStore) -> usize {
    let notices = store.notices.get();
    notices.iter().filter(|n| !is_read(store, n)).count()
}

/// The notices as the Inbox lists them: unread first, then newest first;
/// tracked. Each with whether it was read, and apart, the time it came and
/// the note it names, which the row's live labels keep for themselves.
pub(crate) fn rows(store: AppStore) -> Vec<(Notice, bool, String, (StoreId, NodeId))> {
    let mut rows: Vec<(Notice, bool, String, (StoreId, NodeId))> = store.notices.get().into_iter().map(|n| {
        let read = is_read(store, &n);
        let (at, place) = (n.at.clone(), (n.store, n.node));
        (n, read, at, place)
    }).collect();
    rows.sort_by(|(a, a_read, ..), (b, b_read, ..)| a_read.cmp(b_read).then_with(|| b.at.cmp(&a.at)));
    rows
}

/// The server's list arrived (at connect, or after the switch).
pub(crate) fn listed(store: AppStore, notices: Vec<Notice>, on: bool) {
    ask_for_names(store, &notices);
    store.notices.set(notices);
    store.notifications_on.set(on);
    open_wanted(store);
}

/// Wave 2's channels, as the server last heard them from the accounts
/// service; in the browser, this browser's push subscription follows them.
pub(crate) fn channels(store: AppStore, email: bool, push: bool, push_key: Option<String>) {
    store.notify_email.set(email);
    store.notify_push.set(push);
    // Nobody signed in lists no key: the server answers its empty list.
    let on = untracked(|| store.notifications_on.get());
    browser_push::follow(store, on && push, push_key);
}

/// The email fallback's switch (wave 2), for the account.
pub(crate) fn set_email(store: AppStore, on: bool) {
    store.notify_email.set(on);
    store.send(BackendCommand::SetNotificationChannels { email: Some(on), push: None });
}

/// Web push's switch (wave 2), for the account.
pub(crate) fn set_push(store: AppStore, on: bool) {
    store.notify_push.set(on);
    store.send(BackendCommand::SetNotificationChannels { email: None, push: Some(on) });
    if on {
        ask_permission();
    }
}

thread_local! {
    /// A notice to open once it is listed: the one a pushed notification
    /// that was clicked names (wave 2).
    static WANTED: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Open the notice `id` now if it is held, else as soon as it arrives.
pub fn open_when_held(store: AppStore, id: String) {
    WANTED.with(|w| *w.borrow_mut() = Some(id));
    open_wanted(store);
}

fn open_wanted(store: AppStore) {
    let Some(id) = WANTED.with(|w| w.borrow().clone()) else { return };
    let held = untracked(|| store.notices.with(|all| all.iter().find(|n| n.id == id).cloned()));
    if let Some(notice) = held {
        WANTED.with(|w| *w.borrow_mut() = None);
        open(store, &notice);
    }
}

/// A notice arrived: it heads the list, and unless it was opened already
/// (on another device) the system shows it.
pub(crate) fn received(store: AppStore, notice: Notice) {
    let fresh = untracked(|| store.notices.with(|all| !all.iter().any(|n| n.id == notice.id)));
    if !fresh {
        return;
    }
    ask_for_names(store, std::slice::from_ref(&notice));
    store.notices.update(|all| all.insert(0, notice.clone()));
    if WANTED.with(|w| w.borrow().as_deref() == Some(notice.id.as_str())) {
        open_wanted(store);
        return;
    }
    if untracked(|| is_read(store, &notice)) {
        return;
    }
    if notifies_here() {
        show(store, notice);
    } else {
        #[cfg(feature = "native")]
        crate::shown_notices::held_back(&notice.id);
    }
}

thread_local! {
    /// Watch notices waiting for the name of their note before the system
    /// shows them ([`ask_for_names`]).
    static AWAITING_NAME: std::cell::RefCell<Vec<Notice>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// How long a watch notice waits for its note's name before it is shown
/// without it.
const NAME_WAIT_MS: u32 = 3000;

/// Show `notice` as the system's notification: at once, or, for a watch
/// notice whose note the app does not hold yet, when its name arrives
/// ([`node_loaded`]) or the wait for it runs out. Every show then goes
/// through `show_system_notification`, which on the desktop asks
/// `crate::shown_notices` first, so none is shown twice.
fn show(store: AppStore, notice: Notice) {
    let named = notice.kind != NoticeKind::Watch || store.get_node_signal(notice.store, notice.node).is_some();
    if named {
        show_system_notification(store, &notice);
        return;
    }
    let id = notice.id.clone();
    AWAITING_NAME.with(|a| a.borrow_mut().push(notice));
    rinch::core::reactive::unowned(|| {
        let _ = set_timeout(NAME_WAIT_MS, move || {
            for late in take_awaiting(|n| n.id == id) {
                show_system_notification(store, &late);
            }
        });
    });
}

fn take_awaiting(which: impl Fn(&Notice) -> bool) -> Vec<Notice> {
    AWAITING_NAME.with(|a| {
        let mut all = a.borrow_mut();
        let (taken, kept): (Vec<Notice>, Vec<Notice>) = all.drain(..).partition(|n| which(n));
        *all = kept;
        taken
    })
}

/// A note arrived in the app (`crate::events`, `take_node`): the watch
/// notices waiting to say its name are shown now.
pub(crate) fn node_loaded(store: AppStore, store_id: StoreId, node_id: NodeId) {
    for ready in take_awaiting(|n| n.store == store_id && n.node == node_id) {
        show_system_notification(store, &ready);
    }
}

/// This window process took the tray over from one that quit
/// (`crate::tray::became_primary`): the notices that reached it while it
/// was not yet the primary, which the old one never showed and the person
/// has not opened, are shown now, each once (`crate::shown_notices`).
#[cfg(feature = "native")]
pub(crate) fn took_over(store: AppStore) {
    let held = untracked(|| store.notices.get());
    let unread = |id: &str| held.iter().find(|n| n.id == id).is_some_and(|n| !untracked(|| is_read(store, n)));
    for id in crate::shown_notices::take_over(unread) {
        if let Some(notice) = held.iter().find(|n| n.id == id) {
            show(store, notice.clone());
        }
    }
}

/// Whether this process shows the system's notifications: on the desktop
/// only the window process that owns the tray (`crate::instance`), so two
/// windows never notify twice; a browser tab always (the browser itself
/// files them per site). Every window keeps the notice in its Inbox all the
/// same.
#[cfg(feature = "native")]
fn notifies_here() -> bool {
    crate::instance::is_primary()
}

#[cfg(not(feature = "native"))]
fn notifies_here() -> bool {
    true
}

/// Open a notice: where it points, read everywhere, acknowledged.
pub fn open(store: AppStore, notice: &Notice) {
    let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    if crate::personal::with(|p| p.mark_notice_read(&notice.id, &at)) {
        crate::personal::changed(store);
    }
    store.send(BackendCommand::AcknowledgeNotices { ids: vec![notice.id.clone()] });
    store.discussions_modal.set(false);
    let pane = untracked(|| store.focused_pane.get());
    let (store_id, node_id, anchor) = target_of(notice);
    store.link_open.set(Some(LinkOpen { value: format!("node_{store_id}_{node_id}"), store_id, node_id, anchor, pane: Some(pane) }));
}

/// Where a notice opens: a discussion itself, or the note at the mention
/// (found by the words around it, as a deep link without a sticky index is).
pub(crate) fn target_of(notice: &Notice) -> (StoreId, NodeId, Option<pimble_core::Anchor>) {
    if let Some(discussion) = notice.discussion {
        return (notice.store, discussion, None);
    }
    let quote = notice.snippet.find('@').map(|at| notice.snippet[at..].trim_end_matches('…').to_string());
    (notice.store, notice.node, quote.filter(|q| !q.is_empty()).map(|q| pimble_core::Anchor::new(None, &q)))
}

/// Mark every notice read (the Inbox's "Mark All Read").
pub(crate) fn mark_all_read(store: AppStore) {
    let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let notices = untracked(|| store.notices.get());
    let ids: Vec<String> = notices.iter().map(|n| n.id.clone()).collect();
    let changed = crate::personal::with(|p| notices.iter().fold(false, |any, n| p.mark_notice_read(&n.id, &at) || any));
    if changed {
        crate::personal::changed(store);
    }
    if !ids.is_empty() {
        store.send(BackendCommand::AcknowledgeNotices { ids });
    }
}

/// The switch (section 6): for the whole account; off stops sending and
/// receiving and deletes what is queued.
pub(crate) fn set_on(store: AppStore, on: bool) {
    store.notifications_on.set(on);
    store.send(BackendCommand::SetNotifications { on });
    if on {
        ask_permission();
    }
}

/// View > "Inbox...": the notices, and the discussions with unread messages.
pub(crate) fn open_inbox(store: AppStore) {
    ask_permission();
    store.send(BackendCommand::ListNotices);
    crate::discussions::open_unread(store);
}

// ── The system's notification ─────────────────────────────────────────────

#[cfg(feature = "native")]
fn show_system_notification(store: AppStore, notice: &Notice) {
    // Asked and written down first, in one place: a notice another window
    // (or this one) showed is not shown again, and a window taking the tray
    // over never shows this one again.
    if !crate::shown_notices::first_show(&notice.id) {
        return;
    }
    let clicked = notice.clone();
    let shown = rinch::notification::Notification::new(title_of(notice))
        .body(body_of(store, notice))
        .icon_png(include_bytes!("../../../assets/brand/png/pimble-256.png"))
        .on_click(move || {
            crate::desktop::bring_to_front(None);
            open(store, &clicked);
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
    options.set_body(&body_of(store, notice));
    options.set_tag(&notice.id);
    options.set_icon("/app/pimble-icon.svg");
    let Ok(shown) = web_sys::Notification::new_with_options(&title_of(notice), &options) else { return };
    let clicked = notice.clone();
    let shown_again = shown.clone();
    let onclick = Closure::<dyn FnMut()>::new(move || {
        if let Some(window) = web_sys::window() {
            let _ = window.focus();
        }
        shown_again.close();
        open(store, &clicked);
    });
    shown.set_onclick(Some(onclick.as_ref().unchecked_ref()));
    onclick.forget();
}

#[cfg(not(any(feature = "native", feature = "web")))]
fn show_system_notification(_store: AppStore, _notice: &Notice) {}

/// The browser asks the person once whether Pimble may show notifications,
/// from something they did (opening the Inbox, turning the switch on).
#[cfg(all(feature = "web", not(feature = "native")))]
fn ask_permission() {
    if web_sys::Notification::permission() == web_sys::NotificationPermission::Default {
        if let Ok(asked) = web_sys::Notification::request_permission() {
            // Allowed: the next listing subscribes this browser to pushes.
            wasm_bindgen_futures::spawn_local(async move {
                let _ = wasm_bindgen_futures::JsFuture::from(asked).await;
                if web_sys::Notification::permission() == web_sys::NotificationPermission::Granted {
                    if let Some(store) = browser_push::store() {
                        store.send(BackendCommand::ListNotices);
                    }
                }
            });
        }
    }
}

#[cfg(not(all(feature = "web", not(feature = "native"))))]
fn ask_permission() {}

// ── Web push in the browser (wave 2) ──────────────────────────────────────

/// This browser's push subscription (docs/NOTIFICATIONS_CONTRACT.md, wave 2),
/// made through hub.js (`PimbleHub.pushSubscribe`) and handed to the
/// browser's own server, which gives it to the accounts service under the
/// signed-in account and takes it back at sign-out. The service worker shows
/// what arrives (build/sw.template.js).
#[cfg(all(feature = "web", not(feature = "native")))]
pub mod browser_push {
    use super::*;
    use wasm_bindgen::{JsCast, JsValue};

    thread_local! {
        static STORE: std::cell::Cell<Option<AppStore>> = const { std::cell::Cell::new(None) };
        /// What was registered from this page: (key, endpoint).
        static REGISTERED: std::cell::RefCell<Option<(String, String)>> = const { std::cell::RefCell::new(None) };
    }

    pub(crate) fn store() -> Option<AppStore> {
        STORE.with(|s| s.get())
    }

    fn hub_call(name: &str, arg: Option<&JsValue>) -> Option<js_sys::Promise> {
        let window = web_sys::window()?;
        let hub = js_sys::Reflect::get(&window, &JsValue::from_str("PimbleHub")).ok().filter(|h| h.is_object())?;
        let f: js_sys::Function = js_sys::Reflect::get(&hub, &JsValue::from_str(name)).ok()?.dyn_into().ok()?;
        let answer = match arg {
            Some(arg) => f.call1(&hub, arg),
            None => f.call0(&hub),
        };
        answer.ok()?.dyn_into().ok()
    }

    fn field(value: &JsValue, name: &str) -> Option<String> {
        js_sys::Reflect::get(value, &JsValue::from_str(name)).ok()?.as_string()
    }

    /// Subscribed when `wanted` (notifications and push on, signed in) and the
    /// service sends pushes, unsubscribed otherwise.
    pub(crate) fn follow(store: AppStore, wanted: bool, push_key: Option<String>) {
        STORE.with(|s| s.set(Some(store)));
        match push_key.filter(|_| wanted) {
            Some(key) => {
                if web_sys::Notification::permission() != web_sys::NotificationPermission::Granted {
                    return;
                }
                if REGISTERED.with(|r| r.borrow().as_ref().is_some_and(|(k, _)| *k == key)) {
                    return;
                }
                let Some(promise) = hub_call("pushSubscribe", Some(&JsValue::from_str(&key))) else { return };
                wasm_bindgen_futures::spawn_local(async move {
                    match wasm_bindgen_futures::JsFuture::from(promise).await {
                        Ok(sub) if sub.is_object() => {
                            let (Some(endpoint), Some(p256dh), Some(auth)) = (field(&sub, "endpoint"), field(&sub, "p256dh"), field(&sub, "auth")) else { return };
                            REGISTERED.with(|r| *r.borrow_mut() = Some((key, endpoint.clone())));
                            store.send(BackendCommand::RegisterPush { endpoint, p256dh, auth });
                        }
                        Ok(_) => {}
                        Err(e) => tracing::info!("This browser did not subscribe to pushes: {e:?}"),
                    }
                });
            }
            None => {
                if !wanted {
                    wasm_bindgen_futures::spawn_local(async move {
                        if let Some(endpoint) = end().await {
                            store.send(BackendCommand::UnregisterPush { endpoint });
                        }
                    });
                }
            }
        }
    }

    /// End this browser's push subscription; its endpoint, when it had one.
    /// Signing out calls it before the server takes the subscription back.
    pub async fn end() -> Option<String> {
        REGISTERED.with(|r| *r.borrow_mut() = None);
        let promise = hub_call("pushUnsubscribe", None)?;
        wasm_bindgen_futures::JsFuture::from(promise).await.ok()?.as_string()
    }

    /// What the page was opened for by a notification or an email: `?notice=`
    /// (a pushed notice, opened once it is listed) or `?inbox` (the Inbox);
    /// and, while it is open, a notification clicked in another tab or with
    /// no notice inside (`pimble-open-notice` from hub.js).
    pub fn hear_clicks(store: AppStore) {
        STORE.with(|s| s.set(Some(store)));
        let Some(window) = web_sys::window() else { return };
        let search = window.location().search().unwrap_or_default();
        let pairs: Vec<(String, String)> = url::form_urlencoded::parse(search.trim_start_matches('?').as_bytes()).into_owned().collect();
        if let Some((_, id)) = pairs.iter().find(|(k, _)| k == "notice") {
            open_when_held(store, id.clone());
        } else if pairs.iter().any(|(k, _)| k == "inbox") {
            open_inbox(store);
        }
        let heard = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::CustomEvent)>::new(move |event: web_sys::CustomEvent| {
            let detail = event.detail();
            match field(&detail, "notice") {
                Some(id) => open_when_held(store, id),
                None => open_inbox(store),
            }
        });
        let _ = window.add_event_listener_with_callback("pimble-open-notice", heard.as_ref().unchecked_ref());
        heard.forget();
    }
}

#[cfg(not(all(feature = "web", not(feature = "native"))))]
pub mod browser_push {
    use super::*;

    pub(crate) fn follow(_store: AppStore, _wanted: bool, _push_key: Option<String>) {}
}

// ── The Inbox's rows ──────────────────────────────────────────────────────

/// How often the Inbox's relative times are told again.
const TICK_MS: u32 = 60_000;

/// The notices section of View > "Inbox...".
pub(crate) fn render_notices(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    // "just now" and "5 min ago" move on while the Inbox is open: a tick a
    // minute, read only by the time labels; it stops with the Inbox.
    let now = Signal::new(crate::boards::now_ms());
    let _ = set_interval(TICK_MS, move || now.set(crate::boards::now_ms()));
    rsx! {
        div {
            class: "pimble-inbox",
            div {
                class: "pimble-inbox__head",
                span { class: "pimble-inbox__heading", "Notices" }
                if unread_count(store) > 0 {
                    Button { variant: "subtle", size: "xs", onclick: move || mark_all_read(store), "Mark All Read" }
                }
            }
            if !store.cloud_signed_in.get() {
                div { class: "pimble-discussion__empty", "Sign in to hear when people who share notes with you write to you." }
            }
            if store.cloud_signed_in.get() && !store.notifications_on.get() {
                div { class: "pimble-discussion__empty", "Notifications are off (Settings > Account)." }
            }
            if store.cloud_signed_in.get() && store.notifications_on.get() && store.notices.get().is_empty() {
                div { class: "pimble-discussion__empty", "Nothing yet: mentions, replies and changes to notes you watch show here." }
            }
            for (notice, read, at, place) in rows(store) {
                div {
                    key: format!("{}:{read}", notice.id),
                    class: if read { "pimble-inbox__row" } else { "pimble-inbox__row pimble-inbox__row--unread" },
                    onclick: {
                        let notice = notice.clone();
                        move || open(store, &notice)
                    },
                    div {
                        class: "pimble-inbox__title",
                        {title_of(&notice)}
                        span {
                            class: "pimble-inbox__when",
                            {move || crate::discussions::ago(&at, now.get())}
                        }
                    }
                    if !notice.snippet.is_empty() {
                        div { class: "pimble-inbox__snippet", {notice.snippet.clone()} }
                    }
                    div {
                        class: "pimble-inbox__where",
                        {move || name_of(store, place.0, place.1).unwrap_or_default()}
                        // A change in a note watched: stop watching it from
                        // here (its own watch, or the subtree watch above).
                        if notice.kind == NoticeKind::Watch && crate::watches::can_watch(store) {
                            Button {
                                variant: "subtle",
                                size: "xs",
                                onclick: {
                                    let (s_id, n_id) = (notice.store, notice.node);
                                    move || crate::watches::unwatch(store, s_id, n_id)
                                },
                                "Unwatch"
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(kind: NoticeKind, discussion: Option<NodeId>, snippet: &str) -> Notice {
        Notice { id: "n".into(), kind, from: "joe".into(), from_name: "Joe".into(), store: StoreId::new(), node: NodeId::new(), discussion, anchor: Some("m1".into()), snippet: snippet.into(), at: "2026-11-23T10:00:00.000Z".into(), count: 0 }
    }

    #[test]
    fn a_notice_says_who_and_what() {
        assert_eq!(title_of(&notice(NoticeKind::Mention, None, "")), "Joe mentioned you");
        assert_eq!(title_of(&notice(NoticeKind::Mention, Some(NodeId::new()), "")), "Joe mentioned you in a discussion");
        assert_eq!(title_of(&notice(NoticeKind::Reply, Some(NodeId::new()), "")), "Joe replied in a discussion");
        let mut nameless = notice(NoticeKind::Reply, None, "");
        nameless.from_name = " ".into();
        assert_eq!(title_of(&nameless), "Someone replied in a discussion");
        let mut watched = notice(NoticeKind::Watch, None, "");
        watched.count = 3;
        assert_eq!(title_of(&watched), "Joe edited a note you watch 3 times");
        let (_, node, anchor) = target_of(&watched);
        assert_eq!(node, watched.node, "a watch notice opens the note changed");
        assert!(anchor.is_none());
    }

    #[test]
    fn the_status_bar_says_what_it_counts() {
        assert_eq!(unread_line(0, 0, 0), None);
        assert_eq!(unread_line(1, 0, 0).as_deref(), Some("1 new mention"));
        assert_eq!(unread_line(0, 2, 0).as_deref(), Some("2 new replies"));
        assert_eq!(unread_line(2, 1, 0).as_deref(), Some("3 new mentions and replies"));
        assert_eq!(unread_line(0, 0, 1).as_deref(), Some("1 change to a note you watch"));
        assert_eq!(unread_line(0, 0, 4).as_deref(), Some("4 changes to notes you watch"));
        assert_eq!(unread_line(1, 0, 1).as_deref(), Some("2 new notices"), "a watch notice is not called a mention");
    }

    /// The notes notices name that the app does not hold are asked for, each
    /// once.
    #[test]
    fn the_notes_not_held_are_asked_for_once() {
        let (s, held, missing) = (StoreId::new(), NodeId::new(), NodeId::new());
        let asked = unnamed([(s, held), (s, missing), (s, missing)].into_iter(), |key| key.1 == held);
        assert_eq!(asked, vec![(s, missing)]);
    }

    #[test]
    fn a_mention_opens_its_note_at_the_words_and_a_discussion_opens_itself() {
        let n = notice(NoticeKind::Mention, None, "…we need @Will can you check the oven…");
        let (store_id, node_id, anchor) = target_of(&n);
        assert_eq!((store_id, node_id), (n.store, n.node));
        assert_eq!(anchor.expect("an anchor").quote, pimble_core::Anchor::new(None, "@Will can you check the oven").quote);
        let d = NodeId::new();
        let (_, node_id, anchor) = target_of(&notice(NoticeKind::Reply, Some(d), "I will."));
        assert_eq!((node_id, anchor), (d, None));
    }
}
