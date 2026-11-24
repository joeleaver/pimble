//! What this device keeps for the signed-in account
//! (`<data dir>/pimble/notices/<account>.json`).

use std::collections::BTreeMap;

use pimble_core::NodeId;
use pimble_rpc::{Notice, StorePeople, WatchEntry};
use serde::{Deserialize, Serialize};

use super::{HELD_KEPT_MS, HELD_MAX, OUTBOX_TTL_MS, SENT_KEPT_MS, SENT_MAX};


#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Outgoing {
    pub(super) recipient: String,
    pub(super) notice: Notice,
    pub(super) queued_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Held {
    /// The accounts service's id of the item, what an acknowledgement names.
    pub(super) item: String,
    pub(super) notice: Notice,
    pub(super) received_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Kept {
    pub(super) account: String,
    #[serde(default = "yes")]
    pub(super) on: bool,
    /// What was notified: key, when.
    #[serde(default)]
    pub(super) sent: BTreeMap<String, i64>,
    #[serde(default)]
    pub(super) outbox: Vec<Outgoing>,
    /// What arrived, oldest first.
    #[serde(default)]
    pub(super) held: Vec<Held>,
    /// Items read here and not yet acknowledged to the service.
    #[serde(default)]
    pub(super) acks: Vec<String>,
    /// Wave 2: the account's email and push switches, as the service last
    /// said them.
    #[serde(default = "yes")]
    pub(super) email: bool,
    #[serde(default = "yes")]
    pub(super) push: bool,
    /// Wave 2: the service's VAPID public key, none when it sends no pushes.
    #[serde(default)]
    pub(super) push_key: Option<String>,
    /// Wave 2: the push subscriptions this device registered for the
    /// account, taken back when it signs out.
    #[serde(default)]
    pub(super) push_endpoints: Vec<String>,
    /// Wave 3: whether watched notes are on, as the service last said.
    #[serde(default = "yes")]
    pub(super) watches_on: bool,
    /// Wave 3: the watches the account chose, as last read or written, and
    /// the version the service keeps them at.
    #[serde(default)]
    pub(super) watches: Vec<WatchEntry>,
    #[serde(default)]
    pub(super) watches_version: u64,
    /// Wave 3: the documents registered per store (by its id), as the
    /// service was last told or said.
    #[serde(default)]
    pub(super) registered: BTreeMap<String, Vec<NodeId>>,
}

pub(super) fn yes() -> bool {
    true
}

impl Kept {
    pub(super) fn new(account: &str) -> Self {
        Self {
            account: account.to_string(),
            on: true,
            sent: BTreeMap::new(),
            outbox: Vec::new(),
            held: Vec::new(),
            acks: Vec::new(),
            email: true,
            push: true,
            push_key: None,
            push_endpoints: Vec::new(),
            watches_on: true,
            watches: Vec::new(),
            watches_version: 0,
            registered: BTreeMap::new(),
        }
    }

    /// The account's watches are gone (the switch, or everything off): what
    /// is held of them goes too.
    pub(super) fn forget_watches(&mut self) {
        self.watches.clear();
        self.watches_version = 0;
        self.registered.clear();
    }

    pub(super) fn prune(&mut self, now: i64) {
        self.sent.retain(|_, at| now - *at < SENT_KEPT_MS);
        if self.sent.len() > SENT_MAX {
            let mut ats: Vec<i64> = self.sent.values().copied().collect();
            ats.sort_unstable();
            let cut = ats[self.sent.len() - SENT_MAX];
            self.sent.retain(|_, at| *at >= cut);
        }
        self.outbox.retain(|o| now - o.queued_ms < OUTBOX_TTL_MS);
        self.held.retain(|h| now - h.received_ms < HELD_KEPT_MS);
        if self.held.len() > HELD_MAX {
            let drop = self.held.len() - HELD_MAX;
            self.held.drain(..drop);
        }
    }
}

pub(super) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The people of the share a node is in that name `recipient`, innermost
/// first: the scope and its root (`None` for the whole store).
pub(super) fn scope_for<'a>(people: &'a StorePeople, chain: &[NodeId], recipient: &str) -> Option<&'a pimble_rpc::ScopePeople> {
    let depth = |root: Option<NodeId>| match root {
        Some(root) => chain.iter().position(|n| *n == root),
        None => Some(chain.len()),
    };
    let mut scopes: Vec<(usize, &pimble_rpc::ScopePeople)> = people.scopes.iter().filter_map(|s| depth(s.root).map(|d| (d, s))).collect();
    scopes.sort_by_key(|(d, _)| *d);
    scopes.into_iter().map(|(_, s)| s).find(|s| s.people.iter().any(|p| p.user_id == recipient))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_core::StoreId;
    use pimble_rpc::NoticeKind;

    #[test]
    fn the_innermost_share_that_names_the_recipient_addresses_the_notice() {
        let (note, folder) = (NodeId::new(), NodeId::new());
        let person = |id: &str| pimble_rpc::Person { user_id: id.into(), name: id.into(), notice_key: Some("k".into()) };
        let scope = |root, ids: &[&str]| pimble_rpc::ScopePeople { root, people: ids.iter().map(|i| person(i)).collect(), my_name: String::new(), my_name_chosen: true, share_name: String::new() };
        let people = StorePeople { scopes: vec![scope(None, &["joe", "ann"]), scope(Some(folder), &["joe", "will"])] };
        assert_eq!(scope_for(&people, &[note, folder], "will").map(|s| s.root), Some(Some(folder)));
        assert_eq!(scope_for(&people, &[note, folder], "ann").map(|s| s.root), Some(None), "the whole store's");
        assert!(scope_for(&people, &[note], "will").is_none(), "Will cannot open a note outside his share");
    }

    #[test]
    fn what_is_kept_is_pruned() {
        let mut kept = Kept::new("me");
        let now = now_ms();
        kept.sent.insert("old".into(), now - SENT_KEPT_MS - 1);
        kept.sent.insert("new".into(), now);
        let notice = Notice { id: "n".into(), kind: NoticeKind::Mention, from: "a".into(), from_name: "A".into(), store: StoreId::new(), node: NodeId::new(), discussion: None, anchor: None, snippet: String::new(), at: String::new(), count: 0 };
        kept.outbox.push(Outgoing { recipient: "r".into(), notice: notice.clone(), queued_ms: now - OUTBOX_TTL_MS - 1 });
        kept.outbox.push(Outgoing { recipient: "r".into(), notice, queued_ms: now });
        kept.prune(now);
        assert_eq!(kept.sent.keys().collect::<Vec<_>>(), vec!["new"]);
        assert_eq!(kept.outbox.len(), 1, "a notice older than a day is dropped rather than sent late");
    }
}
