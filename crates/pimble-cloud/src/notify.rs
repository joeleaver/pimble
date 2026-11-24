//! Telling a person about an inbox item when no device of theirs is running
//! (docs/NOTIFICATIONS_CONTRACT.md section 4, wave 2): web push the moment
//! an item is queued, and email, the fallback, for an item nobody has seen
//! fifteen minutes later.
//!
//! Both read the same inbox and agree on what is outstanding: an item is
//! **seen** (`InboxItem.delivered`) once a running device drained it or a
//! browser's service worker showed its push (`POST /inbox/seen/{id}`, the
//! token the push carried), and **read** once a device acknowledged it
//! (deleted). Email goes only for an item neither seen nor read; read on one
//! device, an item stops its email everywhere because it is gone.
//!
//! Only what the service already knows goes out: the name the sender goes by
//! in the share, the share's name, and whether the sender called it a
//! mention or a reply. Never a word of a note: a push carries the sealed
//! notice, which only the recipient's notice key opens.

use std::time::Duration;

use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;

use crate::db::{InboxItemRow, UserRow};
use crate::error::CloudResult;
use crate::mail::NoticeLine;
use crate::state::AppState;

/// "an item no device has acknowledged within 15 minutes is emailed".
pub const EMAIL_AFTER_MS: i64 = 15 * 60 * 1000;
/// "at most one email per hour per person".
pub const EMAIL_AT_MOST_EVERY_MS: i64 = 60 * 60 * 1000;
/// How often the background job looks.
pub const EMAIL_PASS_EVERY: Duration = Duration::from_secs(60);
/// The most lines one email lists; the rest are counted.
const EMAIL_MAX_LINES: usize = 20;
/// How long a push service keeps a push for a browser that is away.
const PUSH_TTL_SECS: u64 = 24 * 60 * 60;

/// The push services of the browsers Pimble runs in, by host suffix: the only
/// endpoints a subscription may name, so nobody can make this service post
/// to an address of their choosing.
const PUSH_HOSTS: &[&str] = &[
    "fcm.googleapis.com",
    "push.services.mozilla.com",
    "push.apple.com",
    "notify.windows.com",
];

/// Whether a subscription may name `endpoint`.
pub fn push_endpoint_allowed(any: bool, endpoint: &str) -> bool {
    let Ok(url) = url::Url::parse(endpoint) else { return false };
    if any {
        return matches!(url.scheme(), "http" | "https");
    }
    if url.scheme() != "https" || url.port().is_some_and(|p| p != 443) {
        return false;
    }
    let Some(host) = url.host_str() else { return false };
    PUSH_HOSTS.iter().any(|allowed| host == *allowed || host.ends_with(&format!(".{allowed}")))
}

// ── Seen, from a push ─────────────────────────────────────────────────────

fn seen_mac(state: &AppState, item_uuid: &str) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(&state.kdf_decoy_secret).expect("HMAC takes any key length");
    mac.update(b"pimble-inbox-seen:");
    mac.update(item_uuid.as_bytes());
    mac
}

/// The token a push carries that lets the service worker which showed it
/// say so, without a session: it names that item and nothing else.
pub fn seen_token(state: &AppState, item_uuid: &str) -> String {
    hex::encode(seen_mac(state, item_uuid).finalize().into_bytes())
}

pub fn seen_token_valid(state: &AppState, item_uuid: &str, token: &str) -> bool {
    match hex::decode(token) {
        Ok(bytes) => seen_mac(state, item_uuid).verify_slice(&bytes).is_ok(),
        Err(_) => false,
    }
}

// ── What is said ──────────────────────────────────────────────────────────

/// The line an item is told in, from what the service knows.
pub async fn line_of(state: &AppState, item: &InboxItemRow) -> CloudResult<NoticeLine> {
    let (sender, share) = crate::routes::inbox::names_for(state, item).await?;
    let changes = if item.kind == "watch" { item.count.max(1) } else { 0 };
    Ok(NoticeLine { sender, kind: item.kind.clone(), share, count: 1, changes })
}

// ── Web push ──────────────────────────────────────────────────────────────

/// Push `item` to every browser `to` subscribed, when push is configured and
/// their switches allow it. Answers how many push services took it. A
/// subscription its push service says is gone (404, 410) is deleted.
pub async fn push_item(state: &AppState, to: &UserRow, item: &InboxItemRow) -> usize {
    let Some(vapid) = state.vapid.as_ref() else { return 0 };
    if to.notifications_off || to.notify_push_off {
        return 0;
    }
    let subscriptions = match state.db.push_subscriptions_of(&to.user_uuid).await {
        Ok(s) if !s.is_empty() => s,
        Ok(_) => return 0,
        Err(e) => {
            tracing::warn!(error = %e, "push subscriptions not read");
            return 0;
        }
    };
    let line = match line_of(state, item).await {
        Ok(line) => line.text(),
        Err(_) => "Someone wrote to you on Pimble".to_string(),
    };
    let seen = format!("{}/api/v1/inbox/seen/{}?t={}", state.config.public_url.trim_end_matches('/'), item.item_uuid, seen_token(state, &item.item_uuid));
    let mut payload = json!({
        "v": 1,
        "id": item.item_uuid,
        "from": item.sender_uuid,
        "to": to.user_uuid,
        "kind": item.kind,
        "line": line,
        "seen": seen,
        "sealed": item.sealed,
    })
    .to_string();
    if payload.len() > crate::webpush::MAX_PLAINTEXT {
        // Too large to carry whole: the line alone, which the service worker
        // shows as a watch notice is shown.
        payload = json!({ "v": 1, "id": item.item_uuid, "from": item.sender_uuid, "to": to.user_uuid, "kind": item.kind, "line": line, "seen": seen }).to_string();
    }
    let now_secs = chrono::Utc::now().timestamp();
    let mut delivered = 0;
    for sub in subscriptions {
        if !push_endpoint_allowed(state.config.push_any_endpoint, &sub.endpoint) {
            let _ = state.db.delete_push_subscription(sub.rid).await;
            continue;
        }
        let body = match crate::webpush::encrypt(payload.as_bytes(), &sub.p256dh, &sub.auth) {
            Ok(body) => body,
            Err(e) => {
                tracing::info!(error = %e, "a push subscription's keys do not work; deleted");
                let _ = state.db.delete_push_subscription(sub.rid).await;
                continue;
            }
        };
        let Ok(authorization) = vapid.authorization(&sub.endpoint, &state.config.vapid_subject, now_secs) else { continue };
        let sent = state
            .push_http
            .post(&sub.endpoint)
            .header("TTL", PUSH_TTL_SECS.to_string())
            .header("Urgency", "high")
            .header("Content-Encoding", "aes128gcm")
            .header("Content-Type", "application/octet-stream")
            .header("Authorization", authorization)
            .body(body)
            .send()
            .await;
        match sent {
            Ok(resp) if resp.status().is_success() => delivered += 1,
            Ok(resp) if matches!(resp.status().as_u16(), 404 | 410) => {
                tracing::info!("a push subscription has ended; deleted");
                let _ = state.db.delete_push_subscription(sub.rid).await;
            }
            Ok(resp) => tracing::info!(status = %resp.status(), "a push service refused a push"),
            Err(e) => tracing::info!(error = %e, "a push did not reach its push service"),
        }
    }
    delivered
}

// ── Email, the fallback ───────────────────────────────────────────────────

/// One look over every inbox as of `now_ms` (the clock is the caller's, so
/// tests move it): for each person with items nobody has seen or read for
/// [`EMAIL_AFTER_MS`], one email listing them, unless they were emailed in
/// the last [`EMAIL_AT_MOST_EVERY_MS`] (then the items wait for the next
/// one) or their switches are off (then the items are never emailed).
/// Items older than a day are dropped, as a drain drops them. Answers how
/// many emails went.
pub async fn email_pass(state: &AppState, now_ms: i64) -> CloudResult<usize> {
    let mut by_recipient: std::collections::BTreeMap<String, Vec<InboxItemRow>> = Default::default();
    for item in state.db.unseen_inbox_items().await? {
        if item.emailed {
            continue;
        }
        if now_ms - item.created_at_ms > crate::routes::inbox::UNDELIVERED_TTL_MS {
            state.db.delete_inbox_item(item.rid).await?;
            continue;
        }
        by_recipient.entry(item.recipient_uuid.clone()).or_default().push(item);
    }
    let mut sent = 0;
    for (recipient, items) in by_recipient {
        let due: Vec<&InboxItemRow> = items.iter().filter(|i| now_ms - i.created_at_ms >= EMAIL_AFTER_MS).collect();
        if due.is_empty() {
            continue;
        }
        let Some(user) = state.db.find_user_by_uuid(&recipient).await? else { continue };
        if user.notifications_off || user.notify_email_off {
            // Never emailed: switched off, they stay for the devices only.
            for item in &due {
                state.db.mark_inbox_item_emailed(item.rid).await?;
            }
            continue;
        }
        if user.notice_emailed_at_ms > 0 && now_ms - user.notice_emailed_at_ms < EMAIL_AT_MOST_EVERY_MS {
            continue;
        }
        let mut lines: Vec<NoticeLine> = Vec::new();
        for item in &due {
            let line = line_of(state, item).await?;
            match lines.iter_mut().find(|l| l.sender == line.sender && l.kind == line.kind && l.share == line.share) {
                Some(same) => {
                    same.count += 1;
                    same.changes += line.changes;
                }
                None => lines.push(line),
            }
        }
        let more = lines.len().saturating_sub(EMAIL_MAX_LINES);
        lines.truncate(EMAIL_MAX_LINES);
        let link = format!("{}/app/?inbox", state.config.public_url.trim_end_matches('/'));
        let (subject, text, html) = crate::mail::notices_email(&lines, more, &link);
        match state.mailer.send(&user.email, &subject, &text, &html).await {
            Ok(()) => {
                state.db.set_notice_emailed_at(user.rid, now_ms).await?;
                for item in &due {
                    state.db.mark_inbox_item_emailed(item.rid).await?;
                }
                sent += 1;
            }
            Err(e) => tracing::warn!(error = %e, "a notice email was not sent; it is tried again"),
        }
    }
    Ok(sent)
}

/// The email fallback's job: [`email_pass`] every [`EMAIL_PASS_EVERY`] for
/// the life of the process. Started by `build_router` (the service), never by
/// the tests, which call [`email_pass`] with a clock of their own.
pub fn spawn(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(EMAIL_PASS_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if let Err(e) = email_pass(&state, chrono::Utc::now().timestamp_millis()).await {
                tracing::warn!(error = %e, "the notice email pass failed");
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_browsers_push_services_are_pushed_to() {
        for ok in [
            "https://fcm.googleapis.com/fcm/send/abc",
            "https://updates.push.services.mozilla.com/wpush/v2/abc",
            "https://web.push.apple.com/QGt",
            "https://wns2-par02p.notify.windows.com/w/?token=x",
        ] {
            assert!(push_endpoint_allowed(false, ok), "{ok}");
        }
        for bad in [
            "http://fcm.googleapis.com/fcm/send/abc",
            "https://127.0.0.1/push",
            "https://localhost/push",
            "https://evil.example/fcm.googleapis.com",
            "https://fcm.googleapis.com.evil.example/x",
            "https://fcm.googleapis.com:8443/x",
            "not a url",
        ] {
            assert!(!push_endpoint_allowed(false, bad), "{bad}");
        }
        assert!(push_endpoint_allowed(true, "http://127.0.0.1:9999/push"), "a test's own push service");
        assert!(!push_endpoint_allowed(true, "file:///etc/passwd"));
    }
}
