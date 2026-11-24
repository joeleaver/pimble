use super::hosting::hosted_store_label;
use super::share_modal::{member_role_text, member_status_text};
use super::status::{badge_with_waiting, cloud_part, cloud_status_part, server_part, sync_badge_text, Light};
use super::{account_showing, host_to_share, open_share_modal, open_stop_relay_modal, share_from_this_computer, MOUNT_NOTE};
use crate::protocol::{BackendCommand, BackendHandle, CloudOp};
use crate::state::{AppStore, ShareFace};
use pimble_core::{Node, NodeId, RelaySide, Store, StoreId, StoreKind, SyncState};
use pimble_rpc::{CloudHostedStoreInfo, MemberRole, ShareMemberStatus};
use rinch::prelude::untracked;

/// A hosted store with a shared folder and a plain document under its
/// root, a backend whose commands the test can read, and an account
/// signed in. `shared_by` is who shared the store with this account, if
/// anyone did.
fn store_with_a_share(shared_by: Option<&str>) -> (AppStore, crossbeam_channel::Receiver<BackendCommand>, StoreId, NodeId, NodeId) {
    let app = AppStore::new();
    let (cmd_tx, cmd_rx) = crossbeam_channel::bounded::<BackendCommand>(16);
    let (_event_tx, event_rx) = crossbeam_channel::bounded(16);
    app.backend.set(Some(BackendHandle { cmd_tx, event_rx }));
    app.cloud_signed_in.set(true);
    app.cloud_email.set("me@example.com".to_string());

    let mut store = Store::new_local("Notes", "/tmp/notes.pimble".into());
    let root = Node::folder("Notes");
    store.root_node_id = root.id;
    store.shared_by = shared_by.map(str::to_string);
    // Hosted: its link is an encrypting one, which is what a share needs.
    store.sync_mode = StoreKind::Vault;
    let mut recipes = Node::folder("Recipes");
    recipes.metadata.set_share(Some(&pimble_core::ShareMarker {
        v: pimble_core::ShareMarker::VERSION,
        key_id: uuid::Uuid::new_v4(),
        url: "https://pimble.app".to_string(),
        name: "Family recipes".to_string(),
    }));
    let plain = Node::document("Pasta");
    let (store_id, shared_id, plain_id) = (store.id, recipes.id, plain.id);
    app.upsert_store(store);
    app.upsert_node(store_id, root);
    app.upsert_node(store_id, recipes);
    app.upsert_node(store_id, plain);
    (app, cmd_rx, store_id, shared_id, plain_id)
}

fn asked_for_members(commands: &crossbeam_channel::Receiver<BackendCommand>, node_id: NodeId) -> bool {
    commands
        .try_iter()
        .any(|cmd| matches!(cmd, BackendCommand::CloudShareInfo { node_id: n, .. } if n == node_id))
}

/// One's own store keeps the dialog it always had: a shared node opens on
/// the owner's face and asks for its members, any other node opens on the
/// name field, and neither opens without an account.
#[test]
fn share_opens_the_owners_dialog_in_ones_own_store() {
    let (app, commands, store_id, shared_id, plain_id) = store_with_a_share(None);

    open_share_modal(app, store_id, shared_id);
    assert_eq!(app.share_modal_node.get(), Some((store_id, shared_id)));
    assert!(app.share_modal_shared.get());
    assert!(untracked(|| app.share_modal_manages()));
    assert_eq!(app.share_modal_name.get(), "Family recipes");
    assert_eq!(app.share_modal_pending.get(), Some(CloudOp::ShareInfo));
    assert!(asked_for_members(&commands, shared_id));

    open_share_modal(app, store_id, plain_id);
    assert_eq!(app.share_modal_node.get(), Some((store_id, plain_id)));
    assert!(!app.share_modal_shared.get());
    assert!(untracked(|| app.share_modal_manages()));
    assert_eq!(untracked(|| app.share_modal_face()), ShareFace::Name);
    assert_eq!(app.share_modal_pending.get(), None);
    assert!(!asked_for_members(&commands, plain_id));

    app.share_modal_node.set(None);
    app.cloud_signed_in.set(false);
    open_share_modal(app, store_id, plain_id);
    assert_eq!(app.share_modal_node.get(), None);
    assert!(account_showing(app));
    assert_eq!(app.account_modal_hint.get(), "Sign in to share a node");
}

/// In a store that reached this device as someone else's share, a shared
/// root opens on the member's face (the share's own name, no owner's
/// controls) and the member list is asked for as a courtesy. Signed out,
/// the face still opens from what this device holds, and asks nobody.
#[test]
fn share_opens_the_members_face_in_someone_elses_share() {
    let (app, commands, store_id, shared_id, _) = store_with_a_share(Some("ann@example.com"));

    open_share_modal(app, store_id, shared_id);
    assert_eq!(app.share_modal_node.get(), Some((store_id, shared_id)));
    assert!(app.share_modal_shared.get());
    assert!(!untracked(|| app.share_modal_manages()), "a member is shown the owner's dialog");
    assert_eq!(app.share_modal_name.get(), "Family recipes");
    assert_eq!(
        untracked(|| app.share_modal_shared_by_sentence()),
        "Shared by ann@example.com. Only an owner changes who it is shared with."
    );
    assert!(app.share_modal_state.get().is_empty());
    assert!(asked_for_members(&commands, shared_id));

    app.share_modal_node.set(None);
    app.cloud_signed_in.set(false);
    open_share_modal(app, store_id, shared_id);
    assert_eq!(app.share_modal_node.get(), Some((store_id, shared_id)));
    assert!(!account_showing(app), "a member is asked to sign in to read who shared it");
    assert_eq!(app.share_modal_pending.get(), None);
    assert!(!asked_for_members(&commands, shared_id));
}

/// The menu never offers "Share..." on any other node of someone else's
/// store; if something asks anyway, no form the server would refuse opens
/// and the status bar says why.
#[test]
fn share_does_not_open_on_a_node_that_is_not_ones_to_share() {
    let (app, commands, store_id, _, plain_id) = store_with_a_share(Some("ann@example.com"));

    open_share_modal(app, store_id, plain_id);
    assert_eq!(app.share_modal_node.get(), None);
    assert!(!account_showing(app));
    assert_eq!(app.notice.get(), "Only an owner of this store can share from it.");
    assert!(commands.try_iter().next().is_none());
}

/// The same store before anyone hosted it or shared it from anywhere: it
/// is only on this computer.
fn unhosted_store() -> (AppStore, crossbeam_channel::Receiver<BackendCommand>, StoreId, NodeId, NodeId) {
    let (app, commands, store_id, shared_id, plain_id) = store_with_a_share(None);
    app.get_store_signal(store_id).unwrap().update(|s| s.sync_mode = StoreKind::Plain);
    app.ensure_sync_entry(store_id);
    (app, commands, store_id, shared_id, plain_id)
}

/// In a store that is neither hosted nor shared from this computer the
/// dialog opens on the two ways and does nothing else: nothing is asked
/// of the server until the person picks one, and picking "Share from this
/// computer" asks for exactly that, never for hosting
/// (docs/RELAY_CONTRACT.md, "The apps"; `CLAUDE.md`, "Nothing is hosted
/// unless the person asked for it").
#[test]
fn share_in_a_store_that_is_not_hosted_offers_the_two_ways() {
    let (app, commands, store_id, shared_id, plain_id) = unhosted_store();

    open_share_modal(app, store_id, plain_id);
    assert_eq!(app.share_modal_node.get(), Some((store_id, plain_id)));
    assert_eq!(untracked(|| app.share_modal_face()), ShareFace::Ways);
    assert_eq!(untracked(|| app.share_modal_ways_note()), "");
    assert_eq!(app.share_modal_pending.get(), None);
    assert!(commands.try_iter().next().is_none(), "opening the dialog asked the server for something");

    share_from_this_computer(app);
    assert_eq!(app.share_modal_pending.get(), Some(CloudOp::RelayStore));
    let sent: Vec<BackendCommand> = commands.try_iter().collect();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(matches!(sent[0], BackendCommand::CloudRelayStore { store_id: s } if s == store_id), "{sent:?}");
    // Still the two ways until the server has answered.
    assert_eq!(untracked(|| app.share_modal_face()), ShareFace::Ways);

    // The store's link is what moves the dialog on, whoever reports it.
    app.get_store_signal(store_id).unwrap().update(|s| {
        s.sync_mode = StoreKind::Vault;
        s.relay = RelaySide::Owner;
    });
    assert_eq!(untracked(|| app.share_modal_face()), ShareFace::Name);

    // A node that carries a share shows it whatever the link says.
    app.get_store_signal(store_id).unwrap().update(|s| {
        s.sync_mode = StoreKind::Plain;
        s.relay = RelaySide::None;
    });
    open_share_modal(app, store_id, shared_id);
    assert_eq!(untracked(|| app.share_modal_face()), ShareFace::Owner);
}

/// "Host on Pimble Cloud..." hands over to the hosting confirmation, which
/// is where hosting is said in full and agreed to: the dialog itself
/// uploads nothing. The node is remembered so that the share carries on
/// once the store is hosted.
#[test]
fn hosting_from_the_share_dialog_goes_through_the_confirmation() {
    let (app, commands, store_id, _, plain_id) = unhosted_store();

    open_share_modal(app, store_id, plain_id);
    host_to_share(app);
    assert_eq!(app.share_modal_node.get(), None);
    assert_eq!(app.host_modal_store.get(), Some(store_id));
    assert_eq!(app.share_after_host.get(), Some((store_id, plain_id)));
    assert!(commands.try_iter().next().is_none(), "the store was hosted without the confirmation");
}

/// Neither way is open to a copy of a store that lives somewhere else, or
/// to a store linked to another Pimble server: the dialog says which, and
/// asks nothing of the server (hosting a linked store would replace its
/// link, and the server does not refuse it).
#[test]
fn the_two_ways_say_why_they_are_off() {
    let (app, _commands, store_id, _, plain_id) = unhosted_store();
    open_share_modal(app, store_id, plain_id);

    app.set_sync(
        store_id,
        Some(pimble_core::RemoteEndpoint { url: "http://pimble.example:7462".parse().unwrap(), auth: pimble_core::AuthMethod::None }),
        SyncState::Offline,
    );
    assert_eq!(untracked(|| app.share_modal_face()), ShareFace::Ways);
    assert_eq!(
        untracked(|| app.share_modal_ways_note()),
        "This store is linked to another Pimble server. Unlink it first."
    );

    app.get_store_signal(store_id).unwrap().update(|s| s.is_replica = true);
    assert_eq!(
        untracked(|| app.share_modal_ways_note()),
        "This is a copy of a store that lives somewhere else, and can only be shared from there."
    );
}

/// "Stop Sharing from This Computer..." asks before it sends anything.
#[test]
fn stopping_sharing_from_this_computer_asks_first() {
    let (app, commands, store_id, _, _) = unhosted_store();
    app.stop_relay_modal_error.set("left over".to_string());

    open_stop_relay_modal(app, store_id);
    assert_eq!(app.stop_relay_modal_store.get(), Some(store_id));
    assert!(app.stop_relay_modal_error.get().is_empty());
    assert!(!app.stop_relay_modal_busy.get());
    assert!(commands.try_iter().next().is_none());
}

fn hosted(name: &str, share: bool, shared_by: Option<&str>) -> CloudHostedStoreInfo {
    CloudHostedStoreInfo {
        store_id: uuid::Uuid::new_v4().to_string(),
        name: name.to_string(),
        role: "editor".to_string(),
        kind: "vault".to_string(),
        created_at: String::new(),
        root: share.then(pimble_core::NodeId::new),
        shared_by: shared_by.map(str::to_string),
    }
}

/// A row with a `root` is a share: its own name and who shared it, never
/// the owner's store name (docs/NODE_DOCUMENT_CONTRACT.md section 5, "A
/// share has a name of its own"). A store of one's own is just its name.
#[test]
fn a_shared_store_says_who_shared_it() {
    assert_eq!(hosted_store_label(&hosted("Notes", false, None), false), "Notes");
    assert_eq!(
        hosted_store_label(&hosted("Recipes", true, Some("ann@example.com")), false),
        "Recipes, shared by ann@example.com"
    );
    // A share whose owner the service did not name is still just a name.
    assert_eq!(hosted_store_label(&hosted("Recipes", true, None), false), "Recipes");
}

/// A relayed row is not hosted anywhere: it says whose computer it is
/// served from (docs/RELAY_CONTRACT.md). Pimble Cloud holds no name for
/// such a store, so a row that is not a share has words in place of one.
#[test]
fn a_relayed_store_says_whose_computer_it_is_on() {
    assert_eq!(
        hosted_store_label(&hosted("Recipes", true, Some("alice@example.com")), true),
        "Recipes, shared from alice@example.com's computer"
    );
    // A grant on the whole of someone's relayed store: no name to show.
    assert_eq!(
        hosted_store_label(&hosted("", false, Some("alice@example.com")), true),
        "Shared from alice@example.com's computer"
    );
    // One's own, seen from another of one's computers.
    assert_eq!(hosted_store_label(&hosted("", false, None), true), "Shared from another computer");
    assert_eq!(hosted_store_label(&hosted("Recipes", true, None), true), "Recipes, shared from another computer");
}

/// Every member state the service can report has words for it, and none of
/// them is jargon: an invitation and a key that has not been handed over
/// are ordinary things to be waiting for.
#[test]
fn a_member_reads_in_plain_words() {
    assert_eq!(member_status_text(ShareMemberStatus::Invited, true), "invited, no account yet");
    assert_eq!(
        member_status_text(ShareMemberStatus::WaitingForKey, true),
        "waiting for your Pimble to hand over the key"
    );
    assert_eq!(member_status_text(ShareMemberStatus::Active, true), "active");
    // A member reading the list is not the one whose Pimble hands keys over.
    assert_eq!(
        member_status_text(ShareMemberStatus::WaitingForKey, false),
        "waiting for an owner's Pimble to hand over the key"
    );
    assert_eq!(member_status_text(ShareMemberStatus::Active, false), "active");
    assert_eq!(member_role_text(MemberRole::Owner), "owner");
    assert_eq!(member_role_text(MemberRole::Editor), "can edit");
    assert_eq!(member_role_text(MemberRole::Reader), "can read");
    assert!(MOUNT_NOTE.contains("another store"));
}

/// The badge is the server's `sync_mode` (docs/DESKTOP_ACCOUNT_CONTRACT.md
/// decision 4): a vault link says "encrypted" first, a plain link reads
/// as it always has.
#[test]
fn badge_text_follows_state_and_mode() {
    // Built through serde: this crate has no chrono of its own.
    let synced: SyncState =
        serde_json::from_str(r#"{"state":"synced","last_sync":"2026-09-16T00:00:00Z"}"#).unwrap();
    let none = RelaySide::None;
    assert_eq!(sync_badge_text(&SyncState::Offline, StoreKind::Plain, none, false), "offline");
    assert_eq!(sync_badge_text(&SyncState::Syncing, StoreKind::Plain, none, false), "syncing");
    assert_eq!(sync_badge_text(&synced, StoreKind::Plain, none, false), "synced");
    assert_eq!(sync_badge_text(&SyncState::Offline, StoreKind::Vault, none, false), "encrypted · offline");
    assert_eq!(sync_badge_text(&SyncState::Syncing, StoreKind::Vault, none, false), "encrypted · syncing");
    assert_eq!(sync_badge_text(&synced, StoreKind::Vault, none, false), "encrypted · synced");
    assert_eq!(badge_with_waiting("encrypted · synced", 1), "encrypted · synced · 1 note waiting");
    assert_eq!(badge_with_waiting("synced", 3), "synced · 3 notes waiting");
    assert_eq!(badge_with_waiting("", 2), "2 notes waiting");
    assert_eq!(badge_with_waiting("synced", 0), "synced");
}

#[test]
fn the_cloud_part_says_who_and_how_it_is_doing() {
    use pimble_rpc::{CloudConnection, CloudReach};
    assert_eq!(cloud_part(false, "", None).detail, "sign in");
    let connected = CloudConnection { reach: CloudReach::Connected, sentence: "Connected to pimble.app.".into(), session_expired: false };
    let part = cloud_part(true, "joe@example.com", Some(&connected));
    assert_eq!((part.light, part.detail.as_str()), (Light::Good, "joe@example.com"));
    assert_eq!(part.tooltip, "Signed in as joe@example.com. Connected to pimble.app.");
    let offline = CloudConnection { reach: CloudReach::Offline, sentence: "Not everything here can reach pimble.app.".into(), session_expired: false };
    let part = cloud_part(true, "joe@example.com", Some(&offline));
    assert_eq!((part.light, part.detail.as_str()), (Light::Bad, "joe@example.com · offline"));
    let expired = CloudConnection { reach: CloudReach::Offline, sentence: "Your sign-in to pimble.app expired.".into(), session_expired: true };
    let part = cloud_part(true, "joe@example.com", Some(&expired));
    assert_eq!((part.light, part.detail.as_str()), (Light::Bad, "sign-in expired"));
    // A server too old to say: the account alone.
    assert_eq!(cloud_part(true, "joe@example.com", None).light, Light::Quiet);

    // docs/STARTUP_CONTRACT.md: until the server answers, "connecting..."
    // with a spinner (never "sign in" or "disconnected"); "disconnected"
    // only once asking failed; the account once answered.
    use crate::state::CloudAnswer;
    let waiting = cloud_status_part(&CloudAnswer::Waiting, false, "", None);
    assert_eq!((waiting.detail.as_str(), waiting.spinning()), ("connecting...", true));
    let failed = cloud_status_part(&CloudAnswer::Failed("connection refused".into()), false, "", None);
    assert_eq!((failed.detail.as_str(), failed.light, failed.spinning()), ("disconnected", Light::Bad, false));
    assert!(failed.tooltip.ends_with("connection refused"));
    assert_eq!(cloud_status_part(&CloudAnswer::Answered, true, "joe@example.com", Some(&connected)), cloud_part(true, "joe@example.com", Some(&connected)));
}

#[test]
fn the_server_part_reads_the_connection_status() {
    assert_eq!(server_part("Connected", "127.0.0.1:7462").light, Light::Good);
    assert!(server_part("Disconnected", "127.0.0.1:7462").spinning(), "connecting shows the spinner");
    assert_eq!(server_part("Disconnected", "127.0.0.1:7462").detail, "connecting");
    let part = server_part("Error: refused", "127.0.0.1:7462");
    assert_eq!((part.light, part.detail.as_str()), (Light::Bad, "refused"));
}

/// A store shared from this computer says nothing: its link is to its own
/// twin here (docs/RELAY_CONTRACT.md, "The apps"). A member's
/// row reads as any encrypted store's does, and says `owner offline` only
/// when the backend knows that is why: a Pimble server reports a member's
/// link as `Offline` and no more, which no network looks like too.
#[test]
fn badge_text_says_which_end_of_the_relay_this_is() {
    let synced: SyncState =
        serde_json::from_str(r#"{"state":"synced","last_sync":"2026-09-21T00:00:00Z"}"#).unwrap();
    assert_eq!(sync_badge_text(&synced, StoreKind::Vault, RelaySide::Owner, false), "");
    assert_eq!(sync_badge_text(&SyncState::Offline, StoreKind::Vault, RelaySide::Owner, false), "");

    assert_eq!(sync_badge_text(&synced, StoreKind::Vault, RelaySide::Member, false), "encrypted · synced");
    assert_eq!(sync_badge_text(&SyncState::Offline, StoreKind::Vault, RelaySide::Member, false), "encrypted · offline");
    assert_eq!(sync_badge_text(&SyncState::Offline, StoreKind::Vault, RelaySide::Member, true), "owner offline");
}
