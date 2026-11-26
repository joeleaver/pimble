//! What this device may do in each store and node, and the shape of a share
//! as the app holds it: access, the roots a replica shows and the ones that
//! ended, the Share modal's face, the relay side, and documents waiting or
//! out of step (docs/NODE_DOCUMENT_CONTRACT.md section 5).

use super::*;

/// What changed in the roots a store's row shows (`AppStore::set_store_roots`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RootsChange {
    /// Shown until now, and ended: the share is no longer this account's.
    /// These are the ones a notice is owed for.
    pub ended: Vec<NodeId>,
    /// Shown until now and no longer among the store's roots at all, with
    /// nothing said about why.
    pub gone: Vec<NodeId>,
    /// Not shown until now: another share of the store, or one granted again.
    pub arrived: Vec<NodeId>,
}

impl RootsChange {
    pub fn is_empty(&self) -> bool {
        self.ended.is_empty() && self.gone.is_empty() && self.arrived.is_empty()
    }
}

impl AppStore {
    /// What this device may change in a store (docs/NODE_DOCUMENT_CONTRACT.md
    /// section 5, "Roles"), untracked. `Full` for one's own stores and for an
    /// editor's scope — an editor edits everything in it, structure included —
    /// and `Read` for a reader. A store the app has not registered yet reads
    /// as `Full`: nothing is disabled on a guess.
    pub fn store_access(&self, store_id: StoreId) -> pimble_core::StoreAccess {
        pimble_core::plan::store_access(&self.view(), store_id)
    }

    /// What this device may change of ONE node (untracked): the judgement
    /// the server that answered for the node put on it (`Node::access`),
    /// which is what a write of it would be answered with. A store held to
    /// read answers for everything in it; otherwise the loaded node says (a
    /// reader's root beside an editor's in the same store, a role a token
    /// carries); a node the app has not loaded reads as `Full`, because
    /// nothing is disabled on a guess and the server still judges the write.
    ///
    /// Everything that decides something about one node asks this, with the
    /// node's CANONICAL pair: the editor, a row's menu, rename, delete, drag
    /// and drop. A store row's own items ask [`AppStore::store_access`].
    pub fn node_access(&self, store_id: StoreId, node_id: NodeId) -> pimble_core::StoreAccess {
        pimble_core::plan::access(&self.view(), store_id, node_id)
    }

    /// [`AppStore::node_access`], tracked, for a reactive closure: one signal
    /// read at a time (a tracked read inside another signal's `with` panics,
    /// see `pane_view::document::editor_read_only`).
    pub fn node_access_tracked(&self, store_id: StoreId, node_id: NodeId) -> pimble_core::StoreAccess {
        // The rule is `pimble_core::plan::node_access`; this only reads what
        // it judges from, tracked.
        let store_sig = self.store_data.with(|map| map.get(&store_id).copied());
        let store_access = store_sig.map(|sig| sig.with(|s| s.access));
        if store_access == Some(pimble_core::StoreAccess::Read) {
            return pimble_core::StoreAccess::Read;
        }
        let node_sig = self.node_data.with(|map| map.get(&(store_id, node_id)).copied());
        pimble_core::plan::node_access(store_access, node_sig.map(|sig| sig.with(|n| n.access)))
    }

    /// Record what this device may change in a store as the server has it
    /// now (`GetStoreSync`'s answer). Answers whether either value differs
    /// from what was held, which is when every `Node::access` held of the
    /// store may be stale; the store's signal is written only then, so its
    /// subscribers do not re-run on every answer.
    pub fn set_store_access(&self, store_id: StoreId, access: pimble_core::StoreAccess, read_only_roots: &[NodeId]) -> bool {
        let Some(sig) = self.get_store_signal(store_id) else { return false };
        // The roots are a set: two answers may name them in different orders.
        let same_roots = |held: &[NodeId]| held.len() == read_only_roots.len() && held.iter().all(|root| read_only_roots.contains(root));
        let differs = untracked(|| sig.with(|s| s.access != access || !same_roots(&s.read_only_roots)));
        if differs {
            sig.update(|s| {
                s.access = access;
                s.read_only_roots = read_only_roots.to_vec();
            });
        }
        differs
    }

    /// Every node of `store_id` the window holds a row's copy of, and the
    /// node of every pane opened on one (untracked): what is judged again
    /// when what this device may do in the store changes, since each node
    /// carries the server's judgement of itself and no document says which
    /// changed.
    pub fn shown_node_ids(&self, store_id: StoreId) -> Vec<NodeId> {
        let mut ids: Vec<NodeId> = untracked(|| self.node_data.with(|map| map.keys().filter(|(s, _)| *s == store_id).map(|(_, n)| *n).collect()));
        for active in self.edited_nodes().into_iter().filter(|a| a.store_id == store_id) {
            if !ids.contains(&active.node_id) {
                ids.push(active.node_id);
            }
        }
        ids
    }

    /// The email of whoever shared a store with this account, when it is
    /// someone else's share (untracked).
    pub fn shared_by(&self, store_id: StoreId) -> Option<String> {
        untracked(|| {
            self.store_data
                .with(|map| map.get(&store_id).and_then(|sig| sig.with(|s| s.shared_by.clone())))
        })
    }

    /// Whether the person at this device manages the share the Share modal is
    /// open for ([`manages_share`]): the owner's face when they do, the
    /// member's read-only face when they do not. Reads the modal's node, that
    /// store's `shared_by`, the member list and the signed-in address as
    /// TRACKED reads, so the modal's faces follow them; event handlers wrap it
    /// in `untracked`. True while the modal is closed.
    pub fn share_modal_manages(&self) -> bool {
        let Some((store_id, _)) = self.share_modal_node.get() else { return true };
        let shared_by = self.get_store_signal(store_id).and_then(|sig| sig.with(|s| s.shared_by.clone()));
        let account_email = self.cloud_email.get();
        self.share_modal_members
            .with(|members| manages_share(shared_by.as_deref(), members, &account_email))
    }

    /// Which face the Share modal shows ([`share_face`]), from TRACKED reads
    /// of the modal's node, whether it is shared, who manages it and what the
    /// store's link is: the modal moves on by itself when the store becomes
    /// shared from this computer, or hosted. `Name` while the modal is closed.
    pub fn share_modal_face(&self) -> ShareFace {
        let Some((store_id, _)) = self.share_modal_node.get() else { return ShareFace::Name };
        let link = self
            .get_store_signal(store_id)
            .map(|sig| sig.with(|s| s.sync_mode))
            .unwrap_or_default();
        share_face(self.share_modal_shared.get(), self.share_modal_manages(), link)
    }

    /// What the Share modal says Pimble Cloud holds of this share
    /// ([`share_keeping_note`]), for the store the modal is open on. Tracked.
    pub fn share_modal_keeping_note(&self) -> &'static str {
        let relay = self
            .share_modal_node
            .get()
            .and_then(|(store_id, _)| self.get_store_signal(store_id))
            .map_or(pimble_core::RelaySide::None, |sig| sig.with(|s| s.relay));
        share_keeping_note(relay)
    }

    /// Why the Share modal's two ways are both off for the store it is open
    /// on ([`share_ways_note`]), or empty. Tracked reads, like the face.
    pub fn share_modal_ways_note(&self) -> &'static str {
        let Some((store_id, _)) = self.share_modal_node.get() else { return "" };
        let linked = self
            .sync_data
            .with(|map| map.get(&store_id).copied())
            .map_or(false, |sig| sig.with(|(remote, _)| remote.is_some()));
        let (replica, vault) = self
            .get_store_signal(store_id)
            .map(|sig| sig.with(|s| (s.is_replica, s.kind == pimble_core::StoreKind::Vault)))
            .unwrap_or((false, false));
        share_ways_note(linked, replica, vault)
    }

    /// The sentence the member's face of the Share modal shows
    /// ([`shared_by_sentence`]), from the same tracked reads.
    pub fn share_modal_shared_by_sentence(&self) -> String {
        let shared_by = self
            .share_modal_node
            .get()
            .and_then(|(store_id, _)| self.get_store_signal(store_id))
            .and_then(|sig| sig.with(|s| s.shared_by.clone()));
        self.share_modal_members.with(|members| shared_by_sentence(shared_by.as_deref(), members))
    }

    /// The nodes of a store this device holds the subtree of: the store's own
    /// root for a whole store, the scope roots for a partial replica — one per
    /// share of that store (docs/NODE_DOCUMENT_CONTRACT.md section 5, "The
    /// recipient's replica"). Untracked; empty for a store the app has not
    /// registered.
    pub fn shown_roots(&self, store_id: StoreId) -> Vec<NodeId> {
        untracked(|| {
            self.store_data.with(|map| {
                map.get(&store_id).map(|sig| sig.with(|s| s.shown_roots())).unwrap_or_default()
            })
        })
    }

    /// Whether a store is a share's replica whose every share has ended
    /// (`Store::every_share_ended`): its row shows no folder, says
    /// [`NO_LONGER_SHARED_WORDS`], and offers "Remove Replica..." and nothing
    /// that changes anything. Untracked.
    pub fn every_share_ended(&self, store_id: StoreId) -> bool {
        untracked(|| {
            self.store_data
                .with(|map| map.get(&store_id).map_or(false, |sig| sig.with(|s| s.every_share_ended())))
        })
    }

    /// Bring the roots a store's row shows in line with what the backend says
    /// now: `roots` when it sent the whole list (a `Store` it listed; `None`
    /// keeps the list held), and the roots among them that have ended.
    /// Answers what changed on the screen, so the caller can say so, drop
    /// what left and fetch what arrived; the store's signal is written only
    /// when something did.
    pub fn set_store_roots(&self, store_id: StoreId, roots: Option<&[NodeId]>, ended_roots: &[NodeId]) -> RootsChange {
        let Some(sig) = self.get_store_signal(store_id) else { return RootsChange::default() };
        let (was_roots, was_ended, was_shown, own_root) =
            untracked(|| sig.with(|s| (s.roots.clone(), s.ended_roots.clone(), s.shown_roots(), s.root_node_id)));
        let same = |a: &[NodeId], b: &[NodeId]| a.len() == b.len() && a.iter().all(|id| b.contains(id));
        let roots = roots.unwrap_or(&was_roots);
        // Only a root of the store can have ended: one it lists, or a whole
        // store's own (`Store::held_roots`).
        let ended_roots: Vec<NodeId> =
            ended_roots.iter().copied().filter(|root| if roots.is_empty() { *root == own_root } else { roots.contains(root) }).collect();
        if roots == was_roots.as_slice() && same(&ended_roots, &was_ended) {
            return RootsChange::default();
        }
        sig.update(|s| {
            s.roots = roots.to_vec();
            s.ended_roots = ended_roots.clone();
        });
        let now_shown = self.shown_roots(store_id);
        let left: Vec<NodeId> = was_shown.iter().copied().filter(|root| !now_shown.contains(root)).collect();
        RootsChange {
            ended: left.iter().copied().filter(|root| ended_roots.contains(root)).collect(),
            gone: left.iter().copied().filter(|root| !ended_roots.contains(root)).collect(),
            arrived: now_shown.into_iter().filter(|root| !was_shown.contains(root)).collect(),
        }
    }

    /// Record that the shares rooted at `roots` have ended, beside whatever
    /// had already: the live notification names only the ones that ended
    /// just now. Answers the ones this store was showing until this moment.
    pub fn end_shares(&self, store_id: StoreId, roots: &[NodeId]) -> RootsChange {
        let Some(sig) = self.get_store_signal(store_id) else { return RootsChange::default() };
        let mut ended = untracked(|| sig.with(|s| s.ended_roots.clone()));
        for root in roots {
            if !ended.contains(root) {
                ended.push(*root);
            }
        }
        self.set_store_roots(store_id, None, &ended)
    }

    /// A cached node's title (untracked); `None` when the node is not cached.
    pub fn cached_title(&self, store_id: StoreId, node_id: NodeId) -> Option<String> {
        self.get_node_signal(store_id, node_id).map(|sig| untracked(|| sig.with(|n| n.metadata.title.clone())))
    }

    /// Whether `node_id` is `root` or under it, as far as the cached parent
    /// chain says (untracked). A chain that breaks before it reaches `root`
    /// (an ancestor the app never loaded) answers no.
    pub fn is_at_or_under(&self, store_id: StoreId, node_id: NodeId, root: NodeId) -> bool {
        pimble_core::plan::is_at_or_under(&self.view(), store_id, node_id, root)
    }

    /// Whether a store is a partial replica — this device holds some shared
    /// subtrees of it rather than the whole thing, so each of those roots is a
    /// row of its own under the store row. A whole store's `roots` is empty.
    pub fn is_partial_replica(&self, store_id: StoreId) -> bool {
        untracked(|| {
            self.store_data
                .with(|map| map.get(&store_id).map_or(false, |sig| sig.with(|s| !s.roots.is_empty())))
        })
    }

    /// The node a store row draws itself from: its root, whose icon, colour
    /// and share marker the row shows as its own. A partial replica has none —
    /// the row stands for someone else's store, whose root this device does
    /// not hold, and each shared root is its own row with its own icon and
    /// badge (docs/NODE_DOCUMENT_CONTRACT.md section 5).
    pub fn store_row_node(&self, store_id: StoreId) -> Option<NodeId> {
        untracked(|| {
            self.store_data.with(|map| {
                map.get(&store_id)
                    .and_then(|sig| sig.with(|s| s.roots.is_empty().then_some(s.root_node_id)))
            })
        })
    }

    /// Whether a node carries a share marker — it is shared, and the row
    /// shows the badge (untracked).
    pub fn is_shared(&self, store_id: StoreId, node_id: NodeId) -> bool {
        untracked(|| {
            self.node_data.with(|map| {
                map.get(&(store_id, node_id))
                    .map_or(false, |sig| sig.with(|n| n.metadata.share().is_some()))
            })
        })
    }

    /// Record what a store's sync link is (`Plain` or `Vault`) on the store's
    /// own signal, where the row's badge reads it reactively
    /// (docs/DESKTOP_ACCOUNT_CONTRACT.md decision 4). A no-op when nothing
    /// changed, so the badge's effect does not re-run on every `GetStoreSync`.
    pub fn set_store_sync_mode(&self, store_id: StoreId, sync_mode: pimble_core::StoreKind) {
        let Some(sig) = self.get_store_signal(store_id) else { return };
        let differs = untracked(|| sig.with(|s| s.sync_mode != sync_mode));
        if differs {
            sig.update(|s| s.sync_mode = sync_mode);
        }
    }

    /// Which end of Pimble Cloud's relay this device is for a store, if
    /// either (untracked, `Store::relay`): `Owner` for a store shared from
    /// this computer.
    pub fn store_relay(&self, store_id: StoreId) -> pimble_core::RelaySide {
        untracked(|| {
            self.store_data
                .with(|map| map.get(&store_id).map_or(pimble_core::RelaySide::None, |sig| sig.with(|s| s.relay)))
        })
    }

    /// Record which end of the relay this device is for a store, on the
    /// store's own signal, where the row's badge reads it reactively. Answers
    /// whether it changed: the store row's menu snapshots it ("Stop Sharing
    /// from This Computer...", and what a store shared from here turns off),
    /// so the caller rebuilds the tree then.
    pub fn set_store_relay(&self, store_id: StoreId, relay: pimble_core::RelaySide) -> bool {
        let Some(sig) = self.get_store_signal(store_id) else { return false };
        let differs = untracked(|| sig.with(|s| s.relay != relay));
        if differs {
            sig.update(|s| s.relay = relay);
        }
        differs
    }

    /// Record whether a backend knows a store's owner's computer to be off
    /// (`StoreSyncChanged::owner_offline`). A no-op when nothing changed, so
    /// the badges do not re-run on every `GetStoreSync`.
    pub fn set_owner_offline(&self, store_id: StoreId, offline: bool) {
        let held = untracked(|| self.owner_offline.with(|set| set.contains(&store_id)));
        if held != offline {
            self.owner_offline.update(|set| {
                if offline {
                    set.insert(store_id);
                } else {
                    set.remove(&store_id);
                }
            });
        }
    }

    /// Record what a store's link is holding back. A no-op when nothing
    /// changed, so the badges do not re-run on every `GetStoreSync`.
    pub fn set_waiting(&self, store_id: StoreId, waiting: Vec<pimble_core::WaitingDoc>) {
        let held = untracked(|| self.waiting.with(|map| map.get(&store_id).cloned().unwrap_or_default()));
        if held != waiting {
            self.waiting.update(|map| {
                if waiting.is_empty() {
                    map.remove(&store_id);
                } else {
                    map.insert(store_id, waiting);
                }
            });
        }
    }

    /// Record which notes of a store are out of step. Answers whether that
    /// changed anything: the rows snapshot it, so the caller rebuilds them.
    pub fn set_out_of_step(&self, store_id: StoreId, list: Vec<pimble_core::OutOfStep>) -> bool {
        let held = untracked(|| self.out_of_step.with(|map| map.get(&store_id).cloned().unwrap_or_default()));
        if held == list {
            return false;
        }
        self.out_of_step.update(|map| {
            if list.is_empty() {
                map.remove(&store_id);
            } else {
                map.insert(store_id, list);
            }
        });
        true
    }

    /// Whether, since when and why a note is out of step on this device.
    pub fn out_of_step_of(&self, store_id: StoreId, node_id: NodeId) -> Option<pimble_core::OutOfStep> {
        self.out_of_step.with(|map| map.get(&store_id).and_then(|docs| docs.iter().find(|o| o.node_id == node_id)).cloned())
    }

    /// [`AppStore::out_of_step_of`] when the note shows it (its row's mark,
    /// its pane's line): only what someone can act on (Joe, 2026-11-20).
    /// Properties says every kind.
    pub fn out_of_step_shown(&self, store_id: StoreId, node_id: NodeId) -> Option<pimble_core::OutOfStep> {
        self.out_of_step_of(store_id, node_id).filter(|entry| entry.is_shown())
    }

    /// Why a node waits to sync, when its store's link is holding it back.
    pub fn waiting_reason(&self, store_id: StoreId, node_id: NodeId) -> Option<String> {
        self.waiting.with(|map| map.get(&store_id).and_then(|docs| docs.iter().find(|w| w.node_id == node_id)).map(|w| w.reason.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_fixtures::{a_replica_of_two_shares, marker, member, store_with_a_child};
    use pimble_core::StoreAccess;

    /// A backend that knows a store's owner's computer to be off says so, and
    /// the store forgets it when it goes.
    #[test]
    fn owner_offline_is_kept_per_store() {
        let (app, store_id, _) = store_with_a_child();
        assert!(!app.owner_offline.with(|set| set.contains(&store_id)));
        app.set_owner_offline(store_id, true);
        assert!(app.owner_offline.with(|set| set.contains(&store_id)));
        app.set_owner_offline(store_id, false);
        assert!(!app.owner_offline.with(|set| set.contains(&store_id)));
        app.set_owner_offline(store_id, true);
        app.remove_store(store_id);
        assert!(!app.owner_offline.with(|set| set.contains(&store_id)));
    }

    /// One store can hold a root this account reads beside one it edits
    /// (`Store::access` is `Full`, the read root is in `read_only_roots`), and
    /// what may be done with ONE node is the server's judgement of that node
    /// (`Node::access`). Judging per store is how the editor took typing into
    /// a document the server then refused: the text was on screen and nowhere
    /// else.
    #[test]
    fn a_node_is_judged_by_its_own_access() {
        let (app, store_id, child_id) = store_with_a_child();
        let mut read_only = Node::document("Under the root that is read");
        read_only.access = StoreAccess::Read;
        let read_only_id = read_only.id;
        app.upsert_node(store_id, read_only);

        assert_eq!(app.store_access(store_id), StoreAccess::Full);
        assert_eq!(app.node_access(store_id, read_only_id), StoreAccess::Read, "the store's `Full` spoke for it");
        assert_eq!(app.node_access(store_id, child_id), StoreAccess::Full, "and its neighbour is still edited");
        // Nothing is disabled on a guess: a node, or a store, the app does not hold.
        assert_eq!(app.node_access(store_id, NodeId::new()), StoreAccess::Full);
        assert_eq!(app.node_access(StoreId::new(), child_id), StoreAccess::Full);

        // A store held to read answers for every node in it, whatever an
        // older answer said of one.
        if let Some(sig) = app.get_store_signal(store_id) {
            sig.update(|s| s.access = StoreAccess::Read);
        }
        assert_eq!(app.node_access(store_id, child_id), StoreAccess::Read);
        assert_eq!(app.node_access(store_id, NodeId::new()), StoreAccess::Read);
    }

    /// `set_store_access` says whether anything changed, which is when the
    /// app fetches what it holds again, and the roots are a set.
    #[test]
    fn store_access_reports_a_change_once() {
        let (app, store_id, _) = store_with_a_child();
        let (a, b) = (NodeId::new(), NodeId::new());
        assert!(!app.set_store_access(store_id, StoreAccess::Full, &[]), "nothing changed");
        assert!(app.set_store_access(store_id, StoreAccess::Full, &[a, b]));
        assert!(!app.set_store_access(store_id, StoreAccess::Full, &[b, a]), "the same roots in another order");
        assert!(app.set_store_access(store_id, StoreAccess::Read, &[]));
        assert_eq!(app.store_access(store_id), StoreAccess::Read);
        assert!(!app.set_store_access(StoreId::new(), StoreAccess::Read, &[]), "a store the app does not hold");
    }

    /// A store nobody shared reads as `Full`, and so does one the app has not
    /// registered: nothing is ever disabled on a guess.
    #[test]
    fn access_defaults_to_full() {
        let (app, store_id, _) = store_with_a_child();
        assert_eq!(app.store_access(store_id), StoreAccess::Full);
        assert_eq!(app.store_access(StoreId::new()), StoreAccess::Full);
        assert_eq!(app.shared_by(store_id), None);
    }

    /// An editor of a share edits everything in it, so their rows have nothing
    /// greyed out and their menus carry no note — only a reader's do
    /// (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles"). Every `disabled:`
    /// on a tree-changing item, and every guard in front of one, is this
    /// predicate.
    #[test]
    fn only_a_reader_has_anything_disabled() {
        let (app, store_id, _) = store_with_a_child();
        if let Some(sig) = app.get_store_signal(store_id) {
            sig.update(|s| {
                s.access = StoreAccess::Full;
                s.shared_by = Some("ann@example.com".to_string());
            });
        }
        assert!(app.store_access(store_id).allows_write(), "an editor's share disables tree actions");
        assert!(access_note(app.store_access(store_id)).is_empty(), "an editor's menu carries a note");
        assert_eq!(access_words(app.store_access(store_id)), "", "an editor's row says more than who shared it");

        if let Some(sig) = app.get_store_signal(store_id) {
            sig.update(|s| s.access = StoreAccess::Read);
        }
        assert!(!app.store_access(store_id).allows_write());
        assert_eq!(access_note(app.store_access(store_id)), StoreAccess::READ_ONLY_REFUSAL);
        assert_eq!(access_words(app.store_access(store_id)), "read only");
    }

    /// The modal decides its face from the store it is open on, the members
    /// it has loaded and the signed-in address.
    #[test]
    fn the_modal_knows_whether_its_share_is_managed_here() {
        let (app, store_id, child_id) = store_with_a_child();
        app.cloud_email.set("me@example.com".to_string());
        assert!(untracked(|| app.share_modal_manages()), "a closed modal has nothing to hold back");

        // One's own store.
        app.share_modal_node.set(Some((store_id, child_id)));
        assert!(untracked(|| app.share_modal_manages()));

        // The same store held as someone else's share: the member's face at
        // once, before any list, and still once the list names this account.
        if let Some(sig) = app.get_store_signal(store_id) {
            sig.update(|s| s.shared_by = Some("ann@example.com".to_string()));
        }
        assert!(!untracked(|| app.share_modal_manages()));
        assert_eq!(
            untracked(|| app.share_modal_shared_by_sentence()),
            "Shared by ann@example.com. Only an owner changes who it is shared with."
        );
        app.share_modal_members.set(vec![
            member("ann@example.com", pimble_rpc::MemberRole::Owner),
            member("me@example.com", pimble_rpc::MemberRole::Editor),
        ]);
        assert!(!untracked(|| app.share_modal_manages()));
    }

    /// "Share..." in someone else's store opens only on a shared root (the
    /// member's read-only face), and every other node says why it is off. In
    /// one's own store it is what it always was.
    #[test]
    fn share_is_offered_only_where_it_can_do_something() {
        // The rule is `pimble_core::plan`'s; this is the app's view of the
        // store and the node feeding it.
        let (app, store_id, child_id) = store_with_a_child();
        let item = |app: &AppStore| {
            let caps = pimble_core::plan::capabilities(&app.view(), store_id, Some(child_id));
            (!caps.can_share(), caps.share_note())
        };
        assert_eq!(item(&app), (false, ""));
        if let Some(sig) = app.get_store_signal(store_id) {
            sig.update(|s| s.shared_by = Some("ann@example.com".to_string()));
        }
        assert_eq!(item(&app), (true, ONLY_AN_OWNER_SHARES_NOTE));
        if let Some(sig) = app.get_node_signal(store_id, child_id) {
            sig.update(|n| n.metadata.set_share(Some(&marker("Recipes"))));
        }
        assert_eq!(item(&app), (false, ""));
    }

    /// An ended share's root is no row, and the store row's data says which
    /// have ended: the last one to end leaves the row with no child to
    /// change, and its menu and words snapshot that every share has ended.
    #[test]
    fn an_ended_root_is_not_a_row_and_the_store_rows_data_says_so() {
        let (app, store_id, [recipes, trips], _) = a_replica_of_two_shares();
        let before = untracked(|| app.build_tree_data_structural());
        assert_eq!(before[0].children.len(), 2);

        let change = app.end_shares(store_id, &[trips]);
        assert_eq!(change, RootsChange { ended: vec![trips], ..Default::default() });
        assert_eq!(app.shown_roots(store_id), vec![recipes]);
        assert!(!app.every_share_ended(store_id));
        assert!(app.is_partial_replica(store_id), "still a share's replica");
        let one_left = untracked(|| app.build_tree_data_structural());
        assert_eq!(one_left[0].children.iter().map(|row| row.value.clone()).collect::<Vec<_>>(), vec![format!("node_{store_id}_{recipes}")]);
        assert_ne!(before[0].label, one_left[0].label);
        assert!(app.end_shares(store_id, &[trips]).is_empty(), "told twice, nothing changes");

        let change = app.end_shares(store_id, &[recipes]);
        assert_eq!(change.ended, vec![recipes]);
        assert!(app.every_share_ended(store_id));
        assert!(app.shown_roots(store_id).is_empty(), "never the fallback to the store's root: that is the folder that ended");
        let none_left = untracked(|| app.build_tree_data_structural());
        assert_eq!(none_left.len(), 1, "the store row stays: the files do");
        assert!(none_left[0].children.is_empty());
        assert_ne!(one_left[0].label, none_left[0].label, "the row never re-renders with its ended menu");

        // Granted again: the whole set as the server has it now.
        let change = app.set_store_roots(store_id, None, &[trips]);
        assert_eq!(change, RootsChange { arrived: vec![recipes], ..Default::default() });
        assert_eq!(app.shown_roots(store_id), vec![recipes]);
        assert!(app.set_store_roots(store_id, None, &[trips]).is_empty());

        // A listing whose roots no longer name one at all (the browser,
        // which holds no replica): it left, and nothing says it ended.
        let change = app.set_store_roots(store_id, Some(&[trips]), &[]);
        assert_eq!(change, RootsChange { gone: vec![recipes], arrived: vec![trips], ..Default::default() });
    }

    /// A whole replica the account holds no more ends at its own root, as a
    /// share's replica does at its last share: no child under its row, the
    /// row's data changes, and holding it again brings the root back.
    #[test]
    fn a_whole_replica_whose_root_ended_shows_no_children() {
        let app = AppStore::new();
        let mut store = Store::new_local("Notes", "/tmp/notes.pimble".into());
        store.is_replica = true;
        let (store_id, root) = (store.id, store.root_node_id);
        app.upsert_store(store);
        let mut note = Node::document("Kept");
        note.parent_id = Some(root);
        app.set_children(store_id, root, vec![(store_id, note.id)]);
        app.upsert_node(store_id, note);
        let before = untracked(|| app.build_tree_data_structural());
        assert_eq!(before[0].children.len(), 1);

        let change = app.set_store_roots(store_id, None, &[root, NodeId::new()]);
        assert_eq!(change, RootsChange { ended: vec![root], ..Default::default() }, "only its own root can end");
        assert!(app.every_share_ended(store_id) && app.shown_roots(store_id).is_empty());
        let after = untracked(|| app.build_tree_data_structural());
        assert!(after[0].children.is_empty(), "the row stays, with nothing under it");
        assert_ne!(before[0].label, after[0].label, "the row re-renders with its ended menu");

        let change = app.set_store_roots(store_id, None, &[]);
        assert_eq!(change, RootsChange { arrived: vec![root], ..Default::default() });
    }

    /// Whether a node is under a root is read off the cached parent chain.
    #[test]
    fn a_node_is_under_a_root_by_its_cached_parents() {
        let (app, store_id, [recipes, trips], [pasta, rome]) = a_replica_of_two_shares();
        assert!(app.is_at_or_under(store_id, rome, trips) && app.is_at_or_under(store_id, trips, trips));
        assert!(!app.is_at_or_under(store_id, pasta, trips));
        assert!(app.is_at_or_under(store_id, pasta, recipes));
        assert!(!app.is_at_or_under(store_id, NodeId::new(), trips), "a node the app never loaded");
        assert_eq!(app.cached_title(store_id, trips).as_deref(), Some("Trips"));
        assert_eq!(app.cached_title(store_id, NodeId::new()), None);
    }
}
