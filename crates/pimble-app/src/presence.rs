//! Presence in the app (docs/PRESENCE_CONTRACT.md): who else has a note open,
//! shown as initials on the pane and dots on the tree row, and this
//! window's own panes said to everyone else.
//!
//! Each pane that holds a document is one session (`{client}:{pane}`, the
//! backend fills in the client): a frame when what it holds or its focus
//! changes, one every heartbeat while it holds it, and a goodbye when it lets
//! go. What others send arrives as `PresenceFrame` events and is held until
//! its session says goodbye or goes quiet. Nothing here is stored.

use std::cell::RefCell;
use std::collections::HashMap;

use pimble_core::{NodeId, StoreId};
use pimble_rpc::{PresenceBody, PresenceFrame, PresenceWho, PRESENCE_HEARTBEAT_MS};
use rinch::prelude::*;

use crate::panes::PaneId;
use crate::protocol::BackendCommand;
use crate::state::AppStore;

/// One session someone else has in a document.
#[derive(Debug, Clone, PartialEq)]
pub struct Present {
    pub session: String,
    pub who: Option<PresenceWho>,
    pub device: String,
    pub focused: bool,
    /// When it lapses without another frame (milliseconds since the epoch).
    pub expires_ms: f64,
}

/// A person in a document, their sessions merged: what a pane's initials and
/// a tree row's dot show.
#[derive(Debug, Clone, PartialEq)]
pub struct Person {
    /// Their account (or agent, or session when signed out).
    pub key: String,
    pub name: String,
    pub initials: String,
    /// `#rrggbb`.
    pub color: String,
    /// Name and devices, for the hover.
    pub title: String,
}

thread_local! {
    /// What each pane last said it holds: `(store, node, focused)`.
    static SAID: RefCell<HashMap<PaneId, (StoreId, NodeId, bool)>> = RefCell::new(HashMap::new());
}

// ── This window's panes ──────────────────────────────────────────────────

/// Start saying where this window's panes are: an effect over every pane's
/// document and the focus, and the heartbeat. Called once, while the view is
/// built.
pub fn start(store: AppStore) {
    let _ = rinch::Effect::new(move || {
        let focused = store.focused_pane.get();
        let now: HashMap<PaneId, (StoreId, NodeId, bool)> = PaneId::ALL
            .into_iter()
            .filter_map(|pane| crate::pane_view::pane_document(store, pane).map(|(s, n)| (pane, (s, n, pane == focused))))
            .collect();
        untracked(|| say(store, now));
    });
    heartbeat(store);
}

/// Tell everyone what changed since the last time: a goodbye for each pane
/// that let a document go, a frame for each that holds a new one or changed
/// focus.
fn say(store: AppStore, now: HashMap<PaneId, (StoreId, NodeId, bool)>) {
    let before = SAID.with(|said| said.borrow().clone());
    for (pane, (store_id, node_id, _)) in &before {
        if now.get(pane).is_none_or(|(s, n, _)| (s, n) != (store_id, node_id)) {
            store.send(BackendCommand::PublishPresence { store_id: *store_id, node_id: *node_id, pane: pane.index(), focused: false, left: true });
        }
    }
    for (pane, held) in &now {
        if before.get(pane) != Some(held) {
            let (store_id, node_id, focused) = *held;
            store.send(BackendCommand::PublishPresence { store_id, node_id, pane: pane.index(), focused, left: false });
        }
    }
    SAID.with(|said| *said.borrow_mut() = now);
}

/// Every heartbeat: say it all again, and let go of whoever went quiet.
fn heartbeat(store: AppStore) {
    let _ = set_timeout(PRESENCE_HEARTBEAT_MS as u32, move || {
        let held = SAID.with(|said| said.borrow().clone());
        for (pane, (store_id, node_id, focused)) in held {
            store.send(BackendCommand::PublishPresence { store_id, node_id, pane: pane.index(), focused, left: false });
        }
        prune(store);
        heartbeat(store);
    });
}

// ── Everyone else ────────────────────────────────────────────────────────

/// A frame from someone else (`BackendEvent::PresenceFrame`).
pub(crate) fn received(store: AppStore, frame: &PresenceFrame) {
    let now = crate::boards::now_ms();
    store.presence.update(|all| {
        // A session holds one document: wherever it was, it is not now.
        for sessions in all.values_mut() {
            sessions.retain(|p| p.session != frame.session);
        }
        if !frame.left {
            let body = PresenceBody::decode(&frame.body).unwrap_or_default();
            all.entry((frame.store_id, frame.doc_id)).or_default().push(Present {
                session: frame.session.clone(),
                who: frame.who.clone(),
                device: body.device,
                focused: body.focused,
                expires_ms: now + frame.ttl_ms as f64,
            });
        }
        all.retain(|_, sessions| !sessions.is_empty());
    });
}

/// Drop the sessions that went quiet, and everything of a store that closed.
pub(crate) fn prune(store: AppStore) {
    let now = crate::boards::now_ms();
    let stale = store.presence.with(|all| all.values().flatten().any(|p| p.expires_ms <= now));
    if stale {
        store.presence.update(|all| {
            for sessions in all.values_mut() {
                sessions.retain(|p| p.expires_ms > now);
            }
            all.retain(|_, sessions| !sessions.is_empty());
        });
    }
}

/// Forget a store's presence (it closed here).
pub(crate) fn forget_store(store: AppStore, store_id: StoreId) {
    store.presence.update(|all| all.retain(|(s, _), _| *s != store_id));
}

/// The people in `(store_id, node_id)`, tracked: one per account (an agent
/// apart from its account, someone signed out per device), in a stable order.
pub fn people_in(store: AppStore, store_id: StoreId, node_id: NodeId) -> Vec<Person> {
    let sessions = store.presence.with(|all| all.get(&(store_id, node_id)).cloned().unwrap_or_default());
    if sessions.is_empty() {
        return Vec::new();
    }
    let me = crate::boards::current_author().map(|a| a.id);
    let mut people: Vec<(Person, Vec<String>)> = Vec::new();
    for p in sessions {
        let key = match &p.who {
            Some(PresenceWho { sub, agent: Some(agent) }) => format!("{sub}/agent/{agent}"),
            Some(PresenceWho { sub, agent: None }) if !sub.is_empty() => sub.clone(),
            _ => p.session.clone(),
        };
        if let Some((_, devices)) = people.iter_mut().find(|(person, _)| person.key == key) {
            if !devices.contains(&p.device) {
                devices.push(p.device.clone());
            }
            continue;
        }
        let name = name_of(store, store_id, node_id, &p, me.as_deref());
        let color_key = match &p.who {
            Some(w) if w.agent.is_some() => key.clone(),
            Some(w) if !w.sub.is_empty() => w.sub.clone(),
            _ => key.clone(),
        };
        let c = pimble_board::author_color(&color_key);
        let person = Person { initials: initials(&name), color: format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b), title: String::new(), name, key };
        people.push((person, vec![p.device.clone()]));
    }
    let mut out: Vec<Person> = people
        .into_iter()
        .map(|(mut person, devices)| {
            let devices: Vec<String> = devices.into_iter().filter(|d| !d.is_empty()).collect();
            person.title = if devices.is_empty() { person.name.clone() } else { format!("{}, on {}", person.name, devices.join(", ")) };
            person
        })
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.key.cmp(&b.key)));
    out
}

/// What someone is called here (docs/PRESENCE_CONTRACT.md section 3): the
/// name they go by in the note's share, "you" on another device of one's
/// own, an agent by its name, someone signed out by their device.
fn name_of(store: AppStore, store_id: StoreId, node_id: NodeId, p: &Present, me: Option<&str>) -> String {
    match &p.who {
        Some(PresenceWho { sub, agent: Some(agent) }) => {
            if me == Some(sub.as_str()) || sub.is_empty() {
                agent.clone()
            } else {
                let owner = crate::shares::Names::of(store, store_id, node_id).name(sub);
                owner.map(|o| format!("{agent} ({o}'s agent)")).unwrap_or_else(|| agent.clone())
            }
        }
        Some(PresenceWho { sub, agent: None }) if me == Some(sub.as_str()) => "You".to_string(),
        Some(PresenceWho { sub, agent: None }) if !sub.is_empty() => {
            crate::shares::Names::of(store, store_id, node_id).name(sub).unwrap_or_else(|| "Someone".to_string())
        }
        _ if !p.device.is_empty() => p.device.clone(),
        _ => "Someone".to_string(),
    }
}

/// One or two letters for a name: the first of its first two words.
pub fn initials(name: &str) -> String {
    let letters: String = name.split_whitespace().take(2).filter_map(|w| w.chars().find(|c| c.is_alphanumeric())).flat_map(char::to_uppercase).collect();
    if letters.is_empty() {
        "?".to_string()
    } else {
        letters
    }
}

/// The frame pane `pane` of client `client_id` says, in plain words (an
/// encrypted store's is sealed by whoever sends it on).
pub fn frame_for(client_id: &str, store_id: StoreId, node_id: NodeId, pane: usize, focused: bool, left: bool) -> PresenceFrame {
    let body = if left { String::new() } else { PresenceBody { device: device_label(), focused }.encode() };
    PresenceFrame {
        store_id,
        doc_id: node_id,
        session: format!("{client_id}:{pane}"),
        seq: next_seq(),
        who: None,
        left,
        body,
        sealed: false,
        ttl_ms: pimble_rpc::PRESENCE_TTL_MS,
    }
}

/// A frame's sequence number: rising for the life of this process, and from
/// the clock, so a client that comes back starts above where it was.
fn next_seq() -> u64 {
    static SEQ: std::sync::OnceLock<std::sync::atomic::AtomicU64> = std::sync::OnceLock::new();
    SEQ.get_or_init(|| std::sync::atomic::AtomicU64::new(crate::boards::now_ms() as u64)).fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// The device label a frame carries: the computer's name, or "Web".
pub fn device_label() -> String {
    #[cfg(feature = "native")]
    {
        let from_env = std::env::var("HOSTNAME").ok().or_else(|| std::env::var("COMPUTERNAME").ok());
        let from_file = || std::fs::read_to_string("/etc/hostname").ok().map(|s| s.trim().to_string());
        from_env.filter(|s| !s.is_empty()).or_else(from_file).filter(|s| !s.is_empty()).unwrap_or_else(|| "a computer".to_string())
    }
    #[cfg(not(feature = "native"))]
    {
        "Web".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_take_the_first_letter_of_two_words() {
        assert_eq!(initials("Chef Ann"), "CA");
        assert_eq!(initials("bob"), "B");
        assert_eq!(initials("  mary   jane watson "), "MJ");
        assert_eq!(initials("(Grandma)"), "G");
        assert_eq!(initials(""), "?");
    }
}
