//! The status bar's parts and the store rows' sync badges.

use crate::state::AppStore;
use super::CAN_ADMINISTER_STORES;

/// How one part of the status bar is doing: its dot's color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Light {
    Good,
    Waiting,
    Bad,
    Quiet,
}

/// One part of the status bar: a dot, a name, a dimmer detail beside it and
/// a sentence on hover. Every part reads the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StatusPart {
    pub(super) light: Light,
    pub(super) detail: String,
    pub(super) tooltip: String,
}

impl StatusPart {
    pub(super) fn dot_class(&self) -> &'static str {
        match self.light {
            Light::Good => "pimble-status-bar__dot pimble-status-bar__dot--good",
            Light::Waiting => "pimble-status-bar__dot pimble-status-bar__dot--waiting",
            Light::Bad => "pimble-status-bar__dot pimble-status-bar__dot--bad",
            Light::Quiet => "pimble-status-bar__dot",
        }
    }

    /// Whether the part is waiting on something: it shows a small spinner
    /// in place of its dot (docs/STARTUP_CONTRACT.md "Said").
    pub(super) fn spinning(&self) -> bool {
        self.light == Light::Waiting
    }
}

/// The Pimble Cloud part as the status bar shows it: "connecting..." until
/// the server has said what account it holds, "disconnected" only once
/// asking failed, and [`cloud_part`] after (docs/STARTUP_CONTRACT.md).
pub(super) fn cloud_status_part(answer: &crate::state::CloudAnswer, signed_in: bool, email: &str, connection: Option<&pimble_rpc::CloudConnection>) -> StatusPart {
    use crate::state::CloudAnswer;
    match answer {
        CloudAnswer::Waiting => StatusPart { light: Light::Waiting, detail: "connecting...".to_string(), tooltip: "Asking Pimble's server which Pimble Cloud account is signed in.".to_string() },
        CloudAnswer::Failed(why) => StatusPart { light: Light::Bad, detail: "disconnected".to_string(), tooltip: format!("Pimble's server could not say which account is signed in: {why}") },
        CloudAnswer::Answered => cloud_part(signed_in, email, connection),
    }
}

/// The Pimble Cloud part now, read from the store (tracked).
pub(super) fn cloud_part_now(store: AppStore) -> StatusPart {
    cloud_status_part(&store.cloud_answer.get(), store.cloud_signed_in.get(), &store.cloud_email.get(), store.cloud_connection.get().as_ref())
}

/// What the first part is: Pimble's server on this computer, or in the
/// browser Pimble Cloud itself.
pub(super) fn server_part_name() -> &'static str {
    if CAN_ADMINISTER_STORES { "This computer" } else { "Pimble Cloud" }
}

/// The first part, from `AppStore::connection_status` ("Connected",
/// "Disconnected", "Error: ...") and the address connected to.
pub(super) fn server_part(status: &str, addr: &str) -> StatusPart {
    let place = if CAN_ADMINISTER_STORES { "Pimble's server on this computer" } else { "Pimble Cloud" };
    let shown = url::Url::parse(addr).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_else(|| addr.to_string());
    if status == "Connected" {
        let at = if addr.is_empty() { String::new() } else { format!(" at {addr}") };
        StatusPart { light: Light::Good, detail: shown, tooltip: format!("Connected to {place}{at}.") }
    } else if let Some(message) = status.strip_prefix("Error: ") {
        StatusPart { light: Light::Bad, detail: message.to_string(), tooltip: format!("{place}: {message}") }
    } else {
        StatusPart { light: Light::Waiting, detail: "connecting".to_string(), tooltip: format!("Connecting to {place}.") }
    }
}

/// The Pimble Cloud part: who is signed in and how this computer's
/// connections to Pimble Cloud are doing (`pimble_rpc::CloudConnection`).
pub(super) fn cloud_part(signed_in: bool, email: &str, connection: Option<&pimble_rpc::CloudConnection>) -> StatusPart {
    use pimble_rpc::CloudReach;
    if !signed_in {
        return StatusPart { light: Light::Quiet, detail: "sign in".to_string(), tooltip: "Not signed in. Sign in to host stores on Pimble Cloud and to share.".to_string() };
    }
    let Some(c) = connection else {
        return StatusPart { light: Light::Quiet, detail: email.to_string(), tooltip: format!("Signed in as {email}.") };
    };
    // Pimble Cloud refused the session: say so where the email was, and the
    // part opens the Account page as it always does.
    if c.session_expired {
        return StatusPart { light: Light::Bad, detail: "sign-in expired".to_string(), tooltip: c.sentence.clone() };
    }
    let (light, word) = match c.reach {
        CloudReach::Idle => (Light::Quiet, None),
        CloudReach::Connected => (Light::Good, None),
        CloudReach::Connecting => (Light::Waiting, Some("connecting")),
        CloudReach::Offline => (Light::Bad, Some("offline")),
    };
    let detail = match word {
        Some(word) => format!("{email} · {word}"),
        None => email.to_string(),
    };
    StatusPart { light, detail, tooltip: format!("Signed in as {email}. {}", c.sentence) }
}

/// Text for a linked store's status badge (docs/SYNC_CONTRACT.md "B: app
/// side"). `Conflict` is out of scope for this contract (there are no
/// conflicts to show yet) but still needs a label rather than a match gap.
/// An encrypting vault link says so first (docs/DESKTOP_ACCOUNT_CONTRACT.md
/// decision 4): `encrypted · synced`.
///
/// A store shared from this computer (`relay` is `Owner`,
/// docs/RELAY_CONTRACT.md) has no badge: its link is to its own twin on this
/// machine, so its state says nothing about whether the people it is shared
/// with can reach it, and the nodes shared carry the share icon (Joe,
/// 2026-11-06). A member's row reads as
/// any encrypted store's does ("a member sees no difference"), except that a
/// backend that knows the owner's computer to be off says `owner offline`.
/// Only the browser's knows: a Pimble server reports a member's link as
/// `Offline` whether the owner's computer is off or this one has no network,
/// and the badge does not say of someone else what it cannot tell.
pub(super) fn sync_badge_text(
    state: &pimble_core::SyncState,
    mode: pimble_core::StoreKind,
    relay: pimble_core::RelaySide,
    owner_offline: bool,
) -> &'static str {
    use pimble_core::{RelaySide, StoreKind, SyncState};
    if owner_offline {
        return "owner offline";
    }
    match (relay, mode, state) {
        (RelaySide::Owner, _, _) => "",
        (_, StoreKind::Plain, SyncState::Offline) => "offline",
        (_, StoreKind::Plain, SyncState::Syncing) => "syncing",
        (_, StoreKind::Plain, SyncState::Synced { .. }) => "synced",
        (_, StoreKind::Plain, SyncState::Conflict { .. }) => "conflict",
        (_, StoreKind::Vault, SyncState::Offline) => "encrypted · offline",
        (_, StoreKind::Vault, SyncState::Syncing) => "encrypted · syncing",
        (_, StoreKind::Vault, SyncState::Synced { .. }) => "encrypted · synced",
        (_, StoreKind::Vault, SyncState::Conflict { .. }) => "encrypted · conflict",
    }
}

/// A badge with what the link is holding back after it
/// (docs/SYNC_LIMITS_CONTRACT.md, "Saying what waits"): "encrypted · synced
/// · 1 note waiting", or the count alone where the row has no badge of its
/// own (the browser's encrypted stores, a store shared from here).
pub(super) fn badge_with_waiting(base: &str, waiting: usize) -> String {
    let waits = pimble_core::waiting_phrase(waiting);
    match (base.is_empty(), waits.is_empty()) {
        (_, true) => base.to_string(),
        (true, false) => waits,
        (false, false) => format!("{base} · {waits}"),
    }
}
