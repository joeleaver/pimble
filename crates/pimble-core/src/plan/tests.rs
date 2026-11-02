//! The judgements against a small tree held in memory.

use std::collections::{HashMap, HashSet};

use super::*;
use crate::StoreAccess::{Full, Read};
use crate::{NodeId, Place, RelaySide, StoreAccess, StoreId};

#[derive(Default)]
struct Mem {
    stores: HashMap<StoreId, StoreFacts>,
    parents: HashMap<(StoreId, NodeId), NodeId>,
    children: HashMap<(StoreId, NodeId), Vec<NodeId>>,
    titles: HashMap<(StoreId, NodeId), String>,
    mounts: HashMap<(StoreId, NodeId), Option<(StoreId, NodeId)>>,
    shared: HashSet<(StoreId, NodeId)>,
    access: HashMap<(StoreId, NodeId), StoreAccess>,
}

impl TreeView for Mem {
    fn store(&self, store: StoreId) -> Option<StoreFacts> {
        self.stores.get(&store).cloned()
    }
    fn parent(&self, store: StoreId, node: NodeId) -> Option<NodeId> {
        self.parents.get(&(store, node)).copied()
    }
    fn children(&self, store: StoreId, node: NodeId) -> Option<Vec<NodeId>> {
        self.children.get(&(store, node)).cloned()
    }
    fn is_mount(&self, store: StoreId, node: NodeId) -> bool {
        self.mounts.contains_key(&(store, node))
    }
    fn mount_source(&self, store: StoreId, node: NodeId) -> Option<(StoreId, NodeId)> {
        self.mounts.get(&(store, node)).copied().flatten()
    }
    fn is_shared(&self, store: StoreId, node: NodeId) -> bool {
        self.shared.contains(&(store, node))
    }
    fn held_access(&self, store: StoreId, node: NodeId) -> Option<StoreAccess> {
        self.access.get(&(store, node)).copied()
    }
    fn label(&self, store: StoreId, node: NodeId) -> String {
        self.titles.get(&(store, node)).cloned().unwrap_or_else(|| UNTITLED.to_string())
    }
}

impl Mem {
    fn add_store(&mut self, name: &str) -> (StoreId, NodeId) {
        let (store, root) = (StoreId::new(), NodeId::new());
        self.stores.insert(store, StoreFacts { name: name.into(), root, ..Default::default() });
        self.titles.insert((store, root), name.into());
        self.children.insert((store, root), Vec::new());
        (store, root)
    }

    fn add(&mut self, store: StoreId, parent: NodeId, title: &str) -> NodeId {
        let node = NodeId::new();
        self.parents.insert((store, node), parent);
        self.children.entry((store, parent)).or_default().push(node);
        self.children.insert((store, node), Vec::new());
        self.titles.insert((store, node), title.into());
        node
    }
}

/// Family: MEDICAL (Boo Kitty Medical, Vet), Taxes (shared; 2025).
struct Family {
    view: Mem,
    s: StoreId,
    root: NodeId,
    medical: NodeId,
    boo: NodeId,
    vet: NodeId,
    taxes: NodeId,
    y2025: NodeId,
}

fn family() -> Family {
    let mut view = Mem::default();
    let (s, root) = view.add_store("Family");
    let medical = view.add(s, root, "MEDICAL");
    let boo = view.add(s, medical, "Boo Kitty Medical");
    let vet = view.add(s, medical, "Vet");
    let taxes = view.add(s, root, "Taxes");
    let y2025 = view.add(s, taxes, "2025");
    view.shared.insert((s, taxes));
    Family { view, s, root, medical, boo, vet, taxes, y2025 }
}

#[test]
fn a_move_within_one_store_is_one_request_at_its_place() {
    let f = family();
    let plan = plan_move(&f.view, &[(f.s, f.vet)], (f.s, Some(f.root)), Place::After(f.medical)).unwrap();
    assert_eq!(plan.line, "In Family, after MEDICAL");
    assert_eq!(plan.warning, None);
    assert_eq!(plan.steps, vec![MoveStep::Move { store: f.s, node: f.vet, parent: f.root, place: Place::After(f.medical) }]);
}

#[test]
fn the_sentence_names_the_place() {
    let f = family();
    let line = |place| plan_move(&f.view, &[(f.s, f.y2025)], (f.s, Some(f.medical)), place).unwrap().line;
    assert_eq!(line(Place::Last), "Into MEDICAL");
    assert_eq!(line(Place::First), "Into MEDICAL, first");
    assert_eq!(line(Place::After(f.boo)), "In MEDICAL, after Boo Kitty Medical");
    // A store row is its root, named by the store.
    let plan = plan_move(&f.view, &[(f.s, f.boo)], (f.s, None), Place::Last).unwrap();
    assert_eq!((plan.parent, plan.line.as_str()), ((f.s, f.root), "Into Family"));
}

#[test]
fn several_nodes_move_as_one_request_and_are_counted() {
    let mut f = family();
    let more = f.view.add(f.s, f.root, "More");
    let plan = plan_move(&f.view, &[(f.s, f.boo), (f.s, more), (f.s, f.boo)], (f.s, Some(f.y2025)), Place::Last).unwrap();
    assert_eq!(plan.line, "2 items into 2025", "a node named twice moves once");
    assert_eq!(plan.steps, vec![MoveStep::MoveMany { store: f.s, nodes: vec![f.boo, more], parent: f.y2025, place: Place::Last }]);
}

/// docs/MOVE_CONTRACT.md: a move between stores is always a transplant,
/// one request per node, the last first at one place so they land in order.
#[test]
fn nodes_from_another_store_go_last_first_and_say_so() {
    let mut f = family();
    let (work, work_root) = f.view.add_store("Work");
    let (a, b) = (f.view.add(work, work_root, "A"), f.view.add(work, work_root, "B"));
    let plan = plan_move(&f.view, &[(work, a), (work, b)], (f.s, Some(f.medical)), Place::First).unwrap();
    let t = |node| MoveStep::Transplant { from: work, node, to: f.s, parent: f.medical, place: Place::First };
    assert_eq!(plan.steps, vec![t(b), t(a)]);
    assert_eq!(plan.warning.as_deref(), Some("New nodes in Family; they are deleted from Work (Put Back in Recently Deleted)."));

    let plan = plan_move(&f.view, &[(work, a), (work, b)], (f.s, Some(f.medical)), Place::Last).unwrap();
    assert_eq!(plan.steps, vec![t(a), t(b)].into_iter().map(|s| match s {
        MoveStep::Transplant { from, node, to, parent, .. } => MoveStep::Transplant { from, node, to, parent, place: Place::Last },
        other => other,
    }).collect::<Vec<_>>(), "in order at the end");

    let plan = plan_move(&f.view, &[(work, a)], (f.s, None), Place::Last).unwrap();
    assert_eq!(plan.warning.as_deref(), Some("A new node in Family; it is deleted from Work (Put Back in Recently Deleted)."));
}

#[test]
fn leaving_a_share_is_said_before_the_move() {
    let f = family();
    let plan = plan_move(&f.view, &[(f.s, f.y2025)], (f.s, Some(f.medical)), Place::Last).unwrap();
    assert_eq!(plan.warning.as_deref(), Some("Leaves the share Taxes: its members will see it deleted and can put it back."));
    // Within the share it is a move like any other.
    let inner = plan_move(&f.view, &[(f.s, f.y2025)], (f.s, Some(f.taxes)), Place::First).unwrap();
    assert_eq!(inner.warning, None);
}

#[test]
fn a_node_cannot_go_inside_itself() {
    let f = family();
    assert_eq!(plan_move(&f.view, &[(f.s, f.medical)], (f.s, Some(f.boo)), Place::Last), Err(Refusal::IntoItself));
    assert_eq!(plan_move(&f.view, &[(f.s, f.vet), (f.s, f.medical)], (f.s, Some(f.medical)), Place::Last), Err(Refusal::IntoItself));
    assert_eq!(Refusal::IntoItself.sentence(), INTO_ITSELF);
}

/// Any of the three documents a move touches being read only refuses it,
/// and one node that may not move refuses all of them.
#[test]
fn a_read_only_place_refuses_the_move() {
    for read_only in ["node", "old parent", "new parent"] {
        let mut f = family();
        let at = match read_only {
            "node" => f.vet,
            "old parent" => f.medical,
            _ => f.taxes,
        };
        f.view.access.insert((f.s, at), Read);
        let refused = plan_move(&f.view, &[(f.s, f.y2025), (f.s, f.vet)], (f.s, Some(f.taxes)), Place::Last);
        assert_eq!(refused, Err(Refusal::ReadOnly), "{read_only}");
        assert_eq!(Refusal::ReadOnly.sentence(), StoreAccess::READ_ONLY_REFUSAL);
    }
    // A store held to read refuses whatever its nodes say.
    let mut f = family();
    f.view.stores.get_mut(&f.s).unwrap().access = Read;
    f.view.access.insert((f.s, f.vet), Full);
    assert_eq!(plan_move(&f.view, &[(f.s, f.vet)], (f.s, None), Place::Last), Err(Refusal::ReadOnly));
}

#[test]
fn a_mount_is_its_source_and_refused_when_that_is_not_here() {
    let mut f = family();
    let (work, work_root) = f.view.add_store("Work");
    let projects = f.view.add(work, work_root, "Projects");
    let mount = f.view.add(f.s, f.root, "Work projects");
    f.view.mounts.insert((f.s, mount), Some((work, projects)));
    let plan = plan_move(&f.view, &[(f.s, f.vet)], (f.s, Some(mount)), Place::Last).unwrap();
    assert_eq!(plan.parent, (work, projects));
    assert_eq!(plan.line, "Into Projects");
    assert!(matches!(plan.steps[..], [MoveStep::Transplant { to, parent, .. }] if (to, parent) == (work, projects)));
    assert_eq!(create_parent(&f.view, f.s, Some(mount)), Ok((work, projects)));

    f.view.stores.remove(&work);
    assert_eq!(plan_move(&f.view, &[(f.s, f.vet)], (f.s, Some(mount)), Place::Last), Err(Refusal::MountNotHere));
    assert_eq!(create_parent(&f.view, f.s, Some(mount)), Err(Refusal::MountNotHere));
    // A mount whose target is not known yet is as far out of reach.
    f.view.mounts.insert((f.s, mount), None);
    assert_eq!(create_parent(&f.view, f.s, Some(mount)), Err(Refusal::MountNotHere));
}

#[test]
fn a_move_to_where_it_is_sends_nothing() {
    let f = family();
    for place in [Place::Last, Place::After(f.boo), Place::After(f.vet)] {
        let plan = plan_move(&f.view, &[(f.s, f.vet)], (f.s, Some(f.medical)), place).unwrap();
        assert!(plan.stays(), "{place:?}");
        assert_eq!(plan.line, WHERE_IT_IS);
    }
    assert!(!plan_move(&f.view, &[(f.s, f.vet)], (f.s, Some(f.medical)), Place::First).unwrap().stays());
}

#[test]
fn nothing_to_move_and_no_parent_are_refused() {
    let f = family();
    assert_eq!(plan_move(&f.view, &[], (f.s, Some(f.medical)), Place::Last), Err(Refusal::NotANode));
    assert_eq!(plan_move(&f.view, &[(f.s, f.vet)], (StoreId::new(), None), Place::Last), Err(Refusal::NotANode));
}

#[test]
fn a_new_node_goes_under_the_row_when_it_may() {
    let mut f = family();
    assert_eq!(create_parent(&f.view, f.s, None), Ok((f.s, f.root)), "a store row is its root");
    assert_eq!(create_parent(&f.view, f.s, Some(f.vet)), Ok((f.s, f.vet)));
    f.view.access.insert((f.s, f.vet), Read);
    assert_eq!(create_parent(&f.view, f.s, Some(f.vet)), Err(Refusal::ReadOnly));
    f.view.stores.get_mut(&f.s).unwrap().access = Read;
    assert_eq!(create_parent(&f.view, f.s, None), Err(Refusal::ReadOnly));
}

#[test]
fn deleting_is_all_or_none() {
    let mut f = family();
    assert_eq!(can_delete(&f.view, &[(f.s, f.vet), (f.s, f.boo)]), Ok(()));
    f.view.access.insert((f.s, f.boo), StoreAccess::Comment);
    assert_eq!(can_delete(&f.view, &[(f.s, f.vet), (f.s, f.boo)]), Err(Refusal::ReadOnly));
    assert_eq!(can_delete(&f.view, &[]), Err(Refusal::NotANode));
}

#[test]
fn unknown_access_is_full() {
    assert_eq!(node_access(None, None), Full);
    assert_eq!(node_access(Some(Full), None), Full);
    assert_eq!(node_access(Some(Full), Some(Read)), Read, "the node's own judgement");
    assert_eq!(node_access(Some(Read), Some(Full)), Read, "a store held to read answers for all of it");
    assert_eq!(node_access(Some(StoreAccess::Comment), None), StoreAccess::Comment);
    let f = family();
    assert_eq!(access(&f.view, StoreId::new(), f.vet), Full);
    assert_eq!(store_access(&f.view, StoreId::new()), Full);
}

#[test]
fn walks_end_on_a_cycle_and_where_the_chain_breaks() {
    let mut f = family();
    assert_eq!(ancestors(&f.view, f.s, f.boo), vec![f.boo, f.medical, f.root]);
    assert!(is_at_or_under(&f.view, f.s, f.boo, f.medical) && is_at_or_under(&f.view, f.s, f.medical, f.medical));
    assert!(!is_at_or_under(&f.view, f.s, f.boo, f.taxes));
    assert!(!is_at_or_under(&f.view, f.s, NodeId::new(), f.root), "a node not held");
    assert_eq!(nearest(&f.view, f.s, f.y2025, |n| f.view.is_shared(f.s, n)), Some(f.taxes));
    // A concurrent move can leave a cycle until repair runs.
    f.view.parents.insert((f.s, f.medical), f.boo);
    assert_eq!(ancestors(&f.view, f.s, f.boo), vec![f.boo, f.medical]);
}

#[test]
fn labels() {
    assert_eq!(label_from_title_and_content(true, "Taxes", "first line"), "Taxes");
    assert_eq!(label_from_title_and_content(false, "Taxes", "\n  first line \nsecond"), "first line");
    assert_eq!(label_from_title_and_content(false, "", "abcdefghijklmnopqrstuvwxyz0"), "abcdefghijklmnopqrstuvwxy…");
    assert_eq!(label_from_title_and_content(false, "Taxes", ""), "Taxes");
    assert_eq!(label_from_title_and_content(false, "", ""), UNTITLED);

    let mut node = crate::Node::new(crate::node_types::DOCUMENT);
    node.metadata.title = "Old".into();
    node.metadata.custom.insert(crate::custom_keys::EXPLICIT_TITLE.into(), serde_json::json!(true));
    assert_eq!(display_label(&node, |_| panic!("an explicit title decodes nothing")), "Old");
    node.metadata.custom.clear();
    assert_eq!(display_label(&node, |_| "From the text".into()), "From the text");
    node.node_type = crate::node_types::DISCUSSION.into();
    node.metadata.title = " ".into();
    assert_eq!(display_label(&node, |_| unreachable!()), "Discussion");
}

#[test]
fn a_store_row_offers_what_its_store_can_do() {
    let mut f = family();
    let own = capabilities(&f.view, f.s, None);
    assert!(own.can_share() && own.can_host() && own.can_link() && !own.can_unlink() && !own.can_stop_relaying());
    assert!(!own.shared_root, "the root carries no marker");

    let facts = f.view.stores.get_mut(&f.s).unwrap();
    facts.linked = true;
    facts.relay = RelaySide::Owner;
    let relayed = capabilities(&f.view, f.s, None);
    assert!(!relayed.can_host() && !relayed.can_link() && !relayed.can_unlink() && relayed.can_stop_relaying());
    assert_eq!(relayed.relayed_note(), RELAYED_STORE_NOTE);
    assert_eq!(relayed.share_ways_note(), "This store is linked to another Pimble server. Unlink it first.");

    // Someone else's store: only a shared root opens "Share...".
    let facts = f.view.stores.get_mut(&f.s).unwrap();
    *facts = StoreFacts { shared_by: Some("ann@example.com".into()), access: Read, ..facts.clone() };
    let node = capabilities(&f.view, f.s, Some(f.vet));
    assert!(!node.can_share() && node.share_note() == ONLY_AN_OWNER_SHARES_NOTE);
    let root = capabilities(&f.view, f.s, Some(f.taxes));
    assert!(root.can_share() && root.share_note().is_empty(), "a reader sees who it is shared with");
    assert!(!root.can_put_back(Full), "a store held to read puts nothing back");
}

/// What a store row's menu offers a store shared from this computer, one
/// that is not, and a member's replica of one (docs/RELAY_CONTRACT.md, "The
/// apps").
#[test]
fn menu_states_for_a_store_shared_from_this_computer() {
    use RelaySide::{Member, None as NotRelayed, Owner};

    // "Stop Sharing from This Computer...": only where it is shared from.
    assert!(stop_relaying_item_disabled(NotRelayed));
    assert!(stop_relaying_item_disabled(Member));
    assert!(!stop_relaying_item_disabled(Owner));

    // "Host on Pimble Cloud...": a store with no link that is nobody's copy.
    // A store shared from here has a link (to its own twin), so it is off,
    // and the menu says why.
    assert!(!host_item_disabled(false, false, false));
    assert!(host_item_disabled(true, false, false));
    assert!(host_item_disabled(false, true, false));
    assert!(host_item_disabled(false, false, true));
    assert_eq!(
        relayed_store_note(Owner),
        "This store is shared from this computer. To host it or link it elsewhere, stop sharing it from this computer first."
    );
    assert_eq!(relayed_store_note(NotRelayed), "");
    assert_eq!(relayed_store_note(Member), "");

    // "Unlink from Remote": any linked store but one shared from here, whose
    // link "Stop Sharing from This Computer..." ends.
    assert!(unlink_item_disabled(false, NotRelayed));
    assert!(!unlink_item_disabled(true, NotRelayed));
    assert!(!unlink_item_disabled(true, Member));
    assert!(unlink_item_disabled(true, Owner));
}

/// "Share..." is offered only where it can do something.
#[test]
fn share_is_offered_only_where_it_can_do_something() {
    // One's own store: any node one may write, shared or not.
    assert!(!share_item_disabled(Full, false, false));
    assert!(!share_item_disabled(Full, false, true));
    assert!(share_item_disabled(Read, false, false));
    assert_eq!(share_item_note(false, false), "");
    assert_eq!(share_item_note(false, true), "");

    // Someone else's: a shared root opens, for a reader as for an editor,
    // since that face changes nothing; any other node is off with the note.
    assert!(!share_item_disabled(Full, true, true));
    assert!(!share_item_disabled(Read, true, true));
    assert!(share_item_disabled(Full, true, false));
    assert!(share_item_disabled(Read, true, false));
    assert_eq!(share_item_note(true, true), "");
    assert_eq!(share_item_note(true, false), "Only an owner of this store can share from it.");
    assert_eq!(share_item_note(true, false), ONLY_AN_OWNER_SHARES_NOTE);
    assert!(can_put_back(Full, Full) && !can_put_back(Read, Full) && !can_put_back(Full, Read));
}

/// The Share dialog's four faces (docs/RELAY_CONTRACT.md, "The apps"): a
/// node that is shared shows its share, to whoever manages it or not; one
/// that is not shows the name form only where a share has somewhere to live,
/// and the two ways everywhere else.
#[test]
fn the_share_dialog_has_four_faces() {
    use crate::StoreKind::{Plain, Vault};

    assert_eq!(share_face(false, true, Plain), ShareFace::Ways);
    assert_eq!(share_face(false, true, Vault), ShareFace::Name);
    assert_eq!(share_face(true, true, Vault), ShareFace::Owner);
    assert_eq!(share_face(true, false, Vault), ShareFace::Member);
    // A marker left on a node of a store that is no longer hosted still
    // opens on the share it names, never on a form over it.
    assert_eq!(share_face(true, true, Plain), ShareFace::Owner);
    assert_eq!(share_face(true, false, Plain), ShareFace::Member);

    // Both ways are open to an ordinary local store and to nothing else.
    assert_eq!(share_ways_note(false, false, false), "");
    assert!(!share_ways_note(true, false, false).is_empty());
    assert!(!share_ways_note(false, true, false).is_empty());
    assert!(!share_ways_note(false, false, true).is_empty());
}

/// The menu of a replica whose every share has ended: everything that
/// changes anything is off, and "Remove Replica..." is still there, because
/// that is where the files go.
#[test]
fn the_menu_of_an_ended_replica_offers_removal_and_nothing_that_changes_anything() {
    let caps = Capabilities {
        access: Read,
        linked: true,
        replica: true,
        vault: false,
        relay: RelaySide::None,
        held_as_share: true,
        shared_root: false,
        every_share_ended: true,
    };
    let menu = caps.store_row_menu(true);
    assert!(!menu.remove_replica, "\"Remove Replica...\" is available");
    let off = [
        menu.new_node,
        menu.mount_store,
        menu.mount_remote_store,
        menu.copy_as_mount_source,
        menu.paste_mount,
        menu.link_to_remote,
        menu.host,
        menu.share,
        menu.stop_relaying,
        menu.unlink,
        menu.appearance,
    ];
    assert!(off.iter().all(|disabled| *disabled), "{menu:?}");

    // The same replica while a share of it is held, as an editor: what it
    // always offered ("Unlink from Remote" included).
    let held = Capabilities { access: Full, every_share_ended: false, ..caps }.store_row_menu(true);
    assert!(!held.new_node && !held.paste_mount && !held.unlink && !held.remove_replica && !held.appearance && !held.copy_as_mount_source);
    assert!(held.link_to_remote && held.host && held.share && held.stop_relaying);
    // And one's own unlinked store, with nothing copied.
    let own = Capabilities { access: Full, linked: false, replica: false, held_as_share: false, every_share_ended: false, ..caps }
        .store_row_menu(false);
    assert!(!own.new_node && !own.link_to_remote && !own.host && !own.share && !own.appearance);
    assert!(own.paste_mount && own.unlink && own.remove_replica && own.stop_relaying);
    // A reader's own store: nothing that writes.
    let reader = Capabilities { access: Read, ..Capabilities::default() }.store_row_menu(true);
    assert!(reader.new_node && reader.paste_mount && reader.appearance && reader.share && !reader.copy_as_mount_source);
}

#[test]
fn who_manages_a_share() {
    let me = "me@example.com";
    let ann = Some("ann@example.com");
    assert!(manages_share(None, [], me));
    assert!(manages_share(None, [(me, true), ("ann@example.com", false)], me));
    assert!(!manages_share(ann, [], me));
    assert!(!manages_share(ann, [("ann@example.com", true), (me, false)], me));
    assert!(manages_share(ann, [(me, true)], me));
    assert!(!manages_share(None, [("Me@Example.com", false)], " me@example.com "));
    assert!(manages_share(None, [("", false)], ""), "no account names no row");
}
