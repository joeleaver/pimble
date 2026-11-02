//! What may be done with a store or a node from this device: shared,
//! hosted, linked, unlinked, stopped being shared from here, put back. The
//! rules a row's menu and the Share dialog read, so that an item is offered
//! exactly where the server would let it do something.

use crate::{NodeId, RelaySide, StoreAccess, StoreId, StoreKind};

use super::{access, store_access, TreeView};

/// Why "Share..." is disabled on a node of a store that reached this device
/// as someone else's share: sharing is an owner's to do, and the server
/// refuses it from anyone else with a sentence that opens with this one
/// (docs/NODE_DOCUMENT_CONTRACT.md section 5). An item that can only be
/// refused is not offered; this says why.
pub const ONLY_AN_OWNER_SHARES_NOTE: &str = "Only an owner of this store can share from it.";

/// Why "Host on Pimble Cloud...", "Link to Remote..." and "Unlink from
/// Remote" are all off on the row of a store shared from this computer: it
/// is one or the other, and the person says which
/// (`pimble_server::relay_face::RELAYED_LINK_REFUSAL` is what the server
/// would answer any of them with).
pub const RELAYED_STORE_NOTE: &str =
    "This store is shared from this computer. To host it or link it elsewhere, stop sharing it from this computer first.";

/// Whether a row's "Share..." is disabled. `held_as_share` is whether the
/// node's store reached this device as someone else's share
/// (`Store::shared_by`), `is_shared_root` whether the node carries a share
/// marker.
///
/// In one's own store the item shares a node or opens the share it already
/// has, and only a store held to read disables it, as ever. In someone else's
/// store nothing can be shared from here, so the item opens only on a shared
/// root, where it shows the share's read-only face: who shared it and who is
/// on it. That changes nothing, so a reader gets it as an editor does.
pub fn share_item_disabled(access: StoreAccess, held_as_share: bool, is_shared_root: bool) -> bool {
    if held_as_share {
        !is_shared_root
    } else {
        !access.allows_write()
    }
}

/// The sentence that says why "Share..." is disabled for a reason of its own,
/// or empty: a node of someone else's store that is not a shared root. (A
/// store of one's own held to read is said by the access note.)
pub fn share_item_note(held_as_share: bool, is_shared_root: bool) -> &'static str {
    if held_as_share && !is_shared_root {
        ONLY_AN_OWNER_SHARES_NOTE
    } else {
        ""
    }
}

/// Whether "Host on Pimble Cloud..." is off for a store: one that has a link
/// already (to another Pimble server, to its hosted twin, or to its twin on
/// this computer), a copy of a store that lives somewhere else, or encrypted
/// storage, which is nobody's store to host. One rule for the store row's
/// menu and for the Share dialog's two ways, because the server would not
/// refuse all of these: hosting a store linked elsewhere would replace its
/// link.
pub fn host_item_disabled(linked: bool, replica: bool, vault: bool) -> bool {
    linked || replica || vault
}

/// Why neither way of sharing is offered for a store the Share dialog's
/// [`ShareFace::Ways`] opened on, or empty when both are. The server refuses
/// `cloudRelayStore` for the same stores, and says the same.
pub fn share_ways_note(linked: bool, replica: bool, vault: bool) -> &'static str {
    if replica {
        "This is a copy of a store that lives somewhere else, and can only be shared from there."
    } else if vault {
        "This is encrypted storage, not a store of its own."
    } else if linked {
        "This store is linked to another Pimble server. Unlink it first."
    } else {
        ""
    }
}

/// The sentence that says why a store row's hosting and linking items are
/// disabled for a reason of their own, or empty: a store shared from this
/// computer.
pub fn relayed_store_note(relay: RelaySide) -> &'static str {
    match relay {
        RelaySide::Owner => RELAYED_STORE_NOTE,
        RelaySide::None | RelaySide::Member => "",
    }
}

/// Whether "Stop Sharing from This Computer..." is off: on every store but
/// one shared from this computer.
pub fn stop_relaying_item_disabled(relay: RelaySide) -> bool {
    relay != RelaySide::Owner
}

/// Whether "Unlink from Remote" is off: a store with no link, and a store
/// shared from this computer, whose link is to its own twin on this machine
/// and is ended by "Stop Sharing from This Computer..." (the server refuses
/// an unlink of one).
pub fn unlink_item_disabled(linked: bool, relay: RelaySide) -> bool {
    !linked || relay == RelaySide::Owner
}

/// Whether "Put Back" may act on a deleted node: neither its store nor the
/// node itself is read only for this account (docs/NODE_DOCUMENT_CONTRACT.md
/// section 5, "Roles").
pub fn can_put_back(store_access: StoreAccess, node_access: StoreAccess) -> bool {
    store_access.allows_write() && node_access.allows_write()
}

/// Whether the person at this device manages a share: invites to it, removes
/// from it, stops it. Decided from what the client already holds, never asked
/// of anyone (the server judges every request whatever the dialog offers).
///
/// `members` is the share's member list as (email, is an owner). The
/// account's own row in it says it where there is one, since that is the
/// accounts service answering now. Without that row (the list has not
/// arrived, could not be loaded, or nobody is signed in) the store says it:
/// one that reached this device as someone else's share (`shared_by`) is not
/// this account's to manage, and any other store is.
pub fn manages_share<'a>(shared_by: Option<&str>, members: impl IntoIterator<Item = (&'a str, bool)>, account_email: &str) -> bool {
    let account_email = account_email.trim();
    let own_row =
        members.into_iter().find(|(email, _)| !account_email.is_empty() && email.trim().eq_ignore_ascii_case(account_email));
    match own_row {
        Some((_, owner)) => owner,
        None => shared_by.is_none(),
    }
}

/// Which face the Share dialog shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareFace {
    /// The node's store is neither hosted on Pimble Cloud nor shared from
    /// this computer, so there is nowhere for a share to live yet: the two
    /// ways to give it one, each with its sentence. Nothing is chosen for
    /// the person, and neither button is a default.
    Ways,
    /// The node is not shared yet: a name and "Share".
    Name,
    /// The node is shared and this person manages the share.
    Owner,
    /// The node is shared and the share is someone else's.
    Member,
}

/// The Share dialog's face. `shared` is whether the node carries a share (its
/// marker, then the server's answers), `manages` is [`manages_share`], and
/// `link` is what the store's link is (`Store::sync_mode`): `Vault` for a
/// store hosted on Pimble Cloud and for one shared from this computer alike,
/// which is the very thing the server asks before it lets a node be shared
/// (`pimble_server::share`, `NOT_HOSTED_REFUSAL`). A node that is shared
/// already shows its share whatever the link says.
pub fn share_face(shared: bool, manages: bool, link: StoreKind) -> ShareFace {
    match (shared, manages) {
        (true, true) => ShareFace::Owner,
        (true, false) => ShareFace::Member,
        (false, _) if link != StoreKind::Vault => ShareFace::Ways,
        (false, _) => ShareFace::Name,
    }
}

/// What may be done with one row: a store row (`node` is `None`) or a node,
/// with the facts of the store it is in. The answers are methods, so every
/// menu and dialog asks the same rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities {
    /// What this device may change of the row: the node's own access, or the
    /// store's for a store row.
    pub access: StoreAccess,
    pub linked: bool,
    pub replica: bool,
    pub vault: bool,
    pub relay: RelaySide,
    /// The store reached this device as someone else's share.
    pub held_as_share: bool,
    /// The row's node carries a share marker (a store row: its root's, when
    /// this device holds the root).
    pub shared_root: bool,
    /// A share's replica whose every share has ended.
    pub every_share_ended: bool,
}

/// What may be done with the row of `node` in `store`, or with the store
/// row itself when `node` is `None`.
pub fn capabilities<V: TreeView + ?Sized>(view: &V, store: StoreId, node: Option<NodeId>) -> Capabilities {
    let facts = view.store(store).unwrap_or_default();
    let (row_access, shared_root) = match node {
        Some(node) => (access(view, store, node), view.is_shared(store, node)),
        // A store row takes its root's marker, like its icon; a partial
        // replica's row stands for a root this device does not hold.
        None => (store_access(view, store), !facts.partial && view.store(store).is_some() && view.is_shared(store, facts.root)),
    };
    Capabilities {
        access: row_access,
        linked: facts.linked,
        replica: facts.replica,
        vault: facts.vault,
        relay: facts.relay,
        held_as_share: facts.shared_by.is_some(),
        shared_root,
        every_share_ended: facts.every_share_ended,
    }
}

/// Which items of a store row's menu are off (`true` is disabled). "Close
/// Store" is never off and is not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreRowMenu {
    pub new_node: bool,
    pub mount_store: bool,
    pub mount_remote_store: bool,
    pub copy_as_mount_source: bool,
    pub paste_mount: bool,
    pub link_to_remote: bool,
    pub host: bool,
    pub share: bool,
    pub stop_relaying: bool,
    pub unlink: bool,
    pub remove_replica: bool,
    pub appearance: bool,
    /// "Recently Deleted...": off only once every share has ended, when
    /// there is no scope left to list (docs/MOVE_CONTRACT.md "Seeing and
    /// undoing what was removed"). A reader sees it like an editor does;
    /// only "Put Back" on a row is gated by what this account may write.
    pub recently_deleted: bool,
}

impl Capabilities {
    pub fn writable(&self) -> bool {
        self.access.allows_write()
    }

    /// "Share..." ([`share_item_disabled`]).
    pub fn can_share(&self) -> bool {
        !share_item_disabled(self.access, self.held_as_share, self.shared_root)
    }

    /// Why "Share..." is off for a reason of its own ([`share_item_note`]).
    pub fn share_note(&self) -> &'static str {
        share_item_note(self.held_as_share, self.shared_root)
    }

    /// "Host on Pimble Cloud..." ([`host_item_disabled`]).
    pub fn can_host(&self) -> bool {
        !host_item_disabled(self.linked, self.replica, self.vault)
    }

    /// "Link to Remote...": a store not linked already.
    pub fn can_link(&self) -> bool {
        !self.linked
    }

    /// "Unlink from Remote" ([`unlink_item_disabled`]).
    pub fn can_unlink(&self) -> bool {
        !unlink_item_disabled(self.linked, self.relay)
    }

    /// "Stop Sharing from This Computer..." ([`stop_relaying_item_disabled`]).
    pub fn can_stop_relaying(&self) -> bool {
        !stop_relaying_item_disabled(self.relay)
    }

    /// Why hosting and linking are off on a store shared from this computer
    /// ([`relayed_store_note`]).
    pub fn relayed_note(&self) -> &'static str {
        relayed_store_note(self.relay)
    }

    /// Why neither way of sharing is offered ([`share_ways_note`]).
    pub fn share_ways_note(&self) -> &'static str {
        share_ways_note(self.linked, self.replica, self.vault)
    }

    /// "Put Back" on a deleted node whose own access is `node_access`
    /// ([`can_put_back`]).
    pub fn can_put_back(&self, node_access: StoreAccess) -> bool {
        can_put_back(self.access, node_access)
    }

    /// The `disabled` value of every item of a store row's menu.
    /// `mount_source_copied` is whether something is copied for "Paste Mount
    /// Here". A replica whose every share has ended (Joe, 2026-09-21) keeps
    /// its row because it keeps its files: everything that would change it,
    /// its link or another store from it is off, and "Remove Replica..." is
    /// where the files go.
    pub fn store_row_menu(&self, mount_source_copied: bool) -> StoreRowMenu {
        let remove_replica = !self.replica;
        if self.every_share_ended {
            return StoreRowMenu {
                new_node: true,
                mount_store: true,
                mount_remote_store: true,
                copy_as_mount_source: true,
                paste_mount: true,
                link_to_remote: true,
                host: true,
                share: true,
                stop_relaying: true,
                unlink: true,
                remove_replica,
                appearance: true,
                recently_deleted: true,
            };
        }
        let read_only = !self.writable();
        StoreRowMenu {
            new_node: read_only,
            mount_store: read_only,
            mount_remote_store: read_only,
            copy_as_mount_source: false,
            paste_mount: !mount_source_copied || read_only,
            link_to_remote: !self.can_link(),
            host: !self.can_host(),
            share: !self.can_share(),
            stop_relaying: !self.can_stop_relaying(),
            unlink: !self.can_unlink(),
            remove_replica,
            appearance: read_only,
            recently_deleted: false,
        }
    }
}
