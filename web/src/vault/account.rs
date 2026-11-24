//! The account's store list: its grants on each store, and the shares that ended.

use std::collections::HashMap;

use pimble_core::{NodeId, StoreAccess, StoreId};

/// One grant this account holds on a store, as the accounts service lists it
/// (`GET /api/v1/stores`, one row per grant).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountGrant {
    /// The shared node for a share, `None` for the whole store.
    pub root: Option<NodeId>,
    pub role: String,
    /// The share's own name — never the owner's store name — for a scoped
    /// grant; the store's for a whole-store one.
    pub name: String,
}

/// What the account's store list says about one store: every grant it holds on
/// it, so a store shared twice is one entry with two roots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountStore {
    /// `"vault"` or `"plain"`.
    pub kind: String,
    pub grants: Vec<AccountGrant>,
    /// An owner's email when the store reached this account as a share.
    pub shared_by: Option<String>,
    /// The row's tier is `relay` (docs/RELAY_CONTRACT.md): nothing of the
    /// store is on Pimble Cloud, and it is reached at an endpoint of its own
    /// while its owner's computer is on.
    pub relayed: bool,
}

impl AccountStore {
    /// What this account may do in the store: `Read` only when every grant it
    /// holds is a reader's, `Comment` when none is an editor's and some are a
    /// commenter's. A reader of one folder and an editor of another gets
    /// `Full`, and the server refuses the writes under the read-only root one
    /// at a time — a store-wide `Read` would take away the editing they do
    /// have (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Token"). The same rule
    /// as the desktop's `HeldAs::from_rows`.
    pub fn access(&self) -> StoreAccess {
        if let Some(whole) = self.grants.iter().find(|g| g.root.is_none()) {
            return StoreAccess::of_role(&whole.role);
        }
        StoreAccess::widest(self.grants.iter().map(|g| StoreAccess::of_role(&g.role))).unwrap_or(StoreAccess::Full)
    }

    /// The shared roots, in the order the service listed them; empty when any
    /// grant is of the whole store, which covers every node in it.
    pub fn roots(&self) -> Vec<NodeId> {
        if self.grants.iter().any(|g| g.root.is_none()) {
            return Vec::new();
        }
        self.grants.iter().filter_map(|g| g.root).collect()
    }

    /// The shared roots this account does not edit while it edits others, or
    /// comments on some, as a desktop replica's `sync.json` names its own
    /// (`Store::read_only_roots`); empty when [`AccountStore::access`]
    /// answers for the whole store.
    pub fn read_only_roots(&self) -> Vec<NodeId> {
        if self.access() == StoreAccess::Read || self.grants.iter().any(|g| g.root.is_none()) {
            return Vec::new();
        }
        self.grants.iter().filter(|g| !StoreAccess::of_role(&g.role).allows_write()).filter_map(|g| g.root).collect()
    }

    /// What this account may change of one node, by the rule the hosted
    /// server judges a write with (docs/NODE_DOCUMENT_CONTRACT.md section 5,
    /// "Token"): a whole-store grant wins over any share and its role
    /// answers for every node; otherwise the roles of the shared roots the
    /// node is under (`reached`: the grant roots on its parent chain, itself
    /// included) decide, and a node under a reader's root nested in an
    /// editor's takes the wider role. A node that reaches no root this
    /// account holds is not this rule's to judge and reads as the store does.
    ///
    /// The answer is for the node's own document: a commenter's `Comment`
    /// still writes a discussion, which [`VaultStore::access_of`] judges.
    pub fn node_access(&self, reached: &[NodeId]) -> StoreAccess {
        if let Some(whole) = self.grants.iter().find(|g| g.root.is_none()) {
            return StoreAccess::of_role(&whole.role);
        }
        let roles = self.grants.iter().filter(|g| g.root.is_some_and(|root| reached.contains(&root))).map(|g| StoreAccess::of_role(&g.role));
        StoreAccess::widest(roles).unwrap_or_else(|| self.access())
    }

    /// The name to show: the share's for a store reached through one share,
    /// "Shared by <owner>" for one reached through several (each root under
    /// it carries its own title), and `None` for a whole-store grant, whose
    /// name the server already carries. The same words the desktop gives a
    /// share's replica (`pimble_server::cloud::HeldAs::from_rows`), so one
    /// person's two devices do not call one thing by two names.
    pub fn name(&self) -> Option<String> {
        if self.grants.iter().any(|g| g.root.is_none()) {
            return None;
        }
        if self.grants.len() > 1 {
            return Some(match &self.shared_by {
                Some(owner) => format!("Shared by {owner}"),
                None => "Shared with you".to_string(),
            });
        }
        self.grants.iter().find(|g| !g.name.is_empty()).map(|g| g.name.clone())
    }
}

/// Shares of one store this page was showing that the account's store list
/// no longer names: the account was removed from them, or their owner
/// stopped sharing them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndedShares {
    pub store_id: StoreId,
    /// The shared roots that ended, in the order they were listed.
    pub roots: Vec<NodeId>,
    /// Nothing of the store is left to this account.
    pub every: bool,
}

/// What ended between two readings of the account's store list: for every
/// store held as shares, the roots the new list no longer names. A store the
/// account holds whole now (a whole-store grant covers every node) has lost
/// nothing, and a store held whole before was never a share.
pub fn shares_ended(was: &HashMap<StoreId, AccountStore>, now: &HashMap<StoreId, AccountStore>) -> Vec<EndedShares> {
    let mut ended = Vec::new();
    for (store_id, held) in was {
        let held_roots = held.roots();
        if held_roots.is_empty() {
            continue;
        }
        let Some(row) = now.get(store_id) else {
            ended.push(EndedShares { store_id: *store_id, roots: held_roots, every: true });
            continue;
        };
        if row.grants.iter().any(|g| g.root.is_none()) {
            continue;
        }
        let left = row.roots();
        let roots: Vec<NodeId> = held_roots.into_iter().filter(|root| !left.contains(root)).collect();
        if !roots.is_empty() {
            ended.push(EndedShares { store_id: *store_id, roots, every: left.is_empty() });
        }
    }
    ended
}
