//! The Inbox (docs/NOTIFICATIONS_CONTRACT.md sections 4 to 6): the notices
//! this person has received — mentions of them and replies to them — shown
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
    }
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

/// The notices as the Inbox lists them: unread first, then newest first; tracked.
pub(crate) fn rows(store: AppStore) -> Vec<(Notice, bool)> {
    let mut rows: Vec<(Notice, bool)> = store.notices.get().into_iter().map(|n| {
        let read = is_read(store, &n);
        (n, read)
    }).collect();
    rows.sort_by(|(a, a_read), (b, b_read)| a_read.cmp(b_read).then_with(|| b.at.cmp(&a.at)));
    rows
}

/// The server's list arrived (at connect, or after the switch).
pub(crate) fn listed(store: AppStore, notices: Vec<Notice>, on: bool) {
    store.notices.set(notices);
    store.notifications_on.set(on);
}

/// A notice arrived: it heads the list, and unless it was opened already
/// (on another device) the system shows it.
pub(crate) fn received(store: AppStore, notice: Notice) {
    let fresh = untracked(|| store.notices.with(|all| !all.iter().any(|n| n.id == notice.id)));
    if !fresh {
        return;
    }
    store.notices.update(|all| all.insert(0, notice.clone()));
    if untracked(|| is_read(store, &notice)) {
        return;
    }
    show_system_notification(store, &notice);
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
    let clicked = notice.clone();
    let shown = rinch::notification::Notification::new(title_of(notice))
        .body(notice.snippet.clone())
        .icon_png(include_bytes!("../../../assets/brand/png/pimble-256.png"))
        .on_click(move || {
            crate::app::bring_to_front(None);
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
    options.set_body(&notice.snippet);
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
        let _ = web_sys::Notification::request_permission();
    }
}

#[cfg(not(all(feature = "web", not(feature = "native"))))]
fn ask_permission() {}

// ── The Inbox's rows ──────────────────────────────────────────────────────

/// The notices section of View > "Inbox...".
pub(crate) fn render_notices(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    rsx! {
        div {
            class: "pimble-inbox",
            div {
                class: "pimble-inbox__head",
                span { class: "pimble-inbox__heading", "Mentions and replies" }
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
                div { class: "pimble-discussion__empty", "Nobody has mentioned you or replied to you yet." }
            }
            for (notice, read) in rows(store) {
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
                        span { class: "pimble-inbox__when", {crate::discussions::ago(&notice.at, crate::boards::now_ms())} }
                    }
                    div { class: "pimble-inbox__snippet", {notice.snippet.clone()} }
                    div { class: "pimble-inbox__where", {store.display_label(notice.store, notice.node)} }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(kind: NoticeKind, discussion: Option<NodeId>, snippet: &str) -> Notice {
        Notice { id: "n".into(), kind, from: "joe".into(), from_name: "Joe".into(), store: StoreId::new(), node: NodeId::new(), discussion, anchor: Some("m1".into()), snippet: snippet.into(), at: "2026-11-23T10:00:00.000Z".into() }
    }

    #[test]
    fn a_notice_says_who_and_what() {
        assert_eq!(title_of(&notice(NoticeKind::Mention, None, "")), "Joe mentioned you");
        assert_eq!(title_of(&notice(NoticeKind::Mention, Some(NodeId::new()), "")), "Joe mentioned you in a discussion");
        assert_eq!(title_of(&notice(NoticeKind::Reply, Some(NodeId::new()), "")), "Joe replied in a discussion");
        let mut nameless = notice(NoticeKind::Reply, None, "");
        nameless.from_name = " ".into();
        assert_eq!(title_of(&nameless), "Someone replied in a discussion");
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
