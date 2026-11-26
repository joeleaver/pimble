//! What the explorer and the Share modal say about sharing, in plain words
//! (docs/NODE_DOCUMENT_CONTRACT.md section 5, docs/RELAY_CONTRACT.md): whose
//! store it is, what this device may do in it, the members, and where the
//! notes are kept.

use super::*;

/// What the owner's side of a share is doing, in one plain sentence for the
/// Share modal (docs/NODE_DOCUMENT_CONTRACT.md section 5, "What the owner's
/// side keeps doing": publishing the scope and handing out the key).
/// `Conflict` cannot happen to a share today, but a state needs words rather
/// than a gap in the match.
pub fn share_state_text(state: &SyncState) -> &'static str {
    match state {
        SyncState::Synced { .. } => "Everyone invited has what they need.",
        SyncState::Syncing => "Handing the key and the scope over...",
        SyncState::Offline => "Offline. The rest goes up when this device reconnects.",
        SyncState::Conflict { .. } => "This share could not be set up.",
    }
}

/// What this device may do in a store someone shared with it, in words
/// (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles"). An editor edits
/// everything in scope, so their row says nothing beyond who shared it; only
/// a reader's row has something to add.
pub fn access_words(access: pimble_core::StoreAccess) -> &'static str {
    match access {
        pimble_core::StoreAccess::Full => "",
        pimble_core::StoreAccess::Read => "read only",
        pimble_core::StoreAccess::Comment => "can comment",
    }
}

/// What a removed member is told, once per folder, when a share ends (they
/// were removed from it, or its owner stopped sharing it) and the folder
/// leaves the explorer (Joe, 2026-09-21). `title` is the shared folder's, from
/// the app's own node cache; without one the sentence does not name it.
pub fn share_ended_sentence(title: Option<&str>) -> String {
    match title.map(str::trim).filter(|title| !title.is_empty()) {
        Some(title) => format!("\"{title}\" is no longer shared with you."),
        None => "A shared folder is no longer shared with you.".to_string(),
    }
}

/// What the row of a share's replica says once every share it held has
/// ended, in place of who shared it, what may be done with it and how its
/// link is doing: none of that is true of it any more. The row stays because
/// the files do, until "Remove Replica..." deletes them.
pub const NO_LONGER_SHARED_WORDS: &str = "no longer shared";

/// The one line at the top of that row's menu: why everything that changes
/// anything is off, and what the one item left is for.
pub const ENDED_REPLICA_NOTE: &str =
    "This is no longer shared with you. \"Remove Replica...\" deletes its files from this computer.";

/// The words after a store row's name that say whose store it is and what
/// this device may do in it: "shared by <owner>", with "read only" for a
/// reader, and [`NO_LONGER_SHARED_WORDS`] alone once every share has ended.
/// Empty for one's own store.
pub fn shared_by_words(store: &Store) -> String {
    if store.every_share_ended() {
        return NO_LONGER_SHARED_WORDS.to_string();
    }
    match &store.shared_by {
        Some(email) => {
            let words = access_words(store.access);
            if words.is_empty() {
                format!("shared by {email}")
            } else {
                format!("shared by {email} · {words}")
            }
        }
        None => String::new(),
    }
}

/// Why a row's menu items are disabled, or empty when they are not. Reading
/// is the only access that disables anything, and it disables everything that
/// writes, so one sentence at the top of the menu says it once: the same
/// sentence a refused write comes back with, so the person reads it in one
/// wording wherever it appears.
pub fn access_note(access: pimble_core::StoreAccess) -> &'static str {
    match access {
        pimble_core::StoreAccess::Full => "",
        pimble_core::StoreAccess::Read => pimble_core::StoreAccess::READ_ONLY_REFUSAL,
        pimble_core::StoreAccess::Comment => pimble_core::StoreAccess::COMMENT_ONLY_REFUSAL,
    }
}

/// The one dimmed line at the top of a row's menu: every reason something
/// below it is disabled, each said once, in the order given. Empty when
/// nothing is.
pub fn menu_note_text(notes: &[&str]) -> String {
    notes.iter().copied().filter(|note| !note.is_empty()).collect::<Vec<_>>().join(" ")
}

/// Whether the person at this device manages a share
/// (`pimble_core::plan::manages_share`, the rule), from the share's member
/// list as the accounts service sends it.
pub fn manages_share(shared_by: Option<&str>, members: &[pimble_rpc::ShareMember], account_email: &str) -> bool {
    let rows = members.iter().map(|member| (member.email.as_str(), member.role == pimble_rpc::MemberRole::Owner));
    pimble_core::plan::manages_share(shared_by, rows, account_email)
}

/// How a row of the Share modal names a member (docs/PRESENCE_CONTRACT.md
/// section 3): the name they go by in the share, with the email beside it
/// for the owner, who manages the share by email. Members never see an
/// email: the server sends none. An invitation has only its address.
pub fn member_label(member: &pimble_rpc::ShareMember, with_email: bool) -> String {
    let name = member.name.trim();
    let email = member.email.trim();
    match (name.is_empty(), email.is_empty() || !with_email) {
        (false, false) => format!("{name} ({email})"),
        (false, true) => name.to_string(),
        (true, _) if !email.is_empty() => email.to_string(),
        (true, _) => "someone".to_string(),
    }
}

/// The member's face of the Share modal in one sentence: whose share this
/// is, and that the list under it is not theirs to change. The owner is who
/// the store says shared it, or the member list's owner row when the store
/// does not say.
pub fn shared_by_sentence(shared_by: Option<&str>, members: &[pimble_rpc::ShareMember]) -> String {
    let owner = shared_by
        .map(str::to_string)
        .or_else(|| members.iter().find(|member| member.role == pimble_rpc::MemberRole::Owner).map(|member| member_label(member, false)));
    match owner {
        Some(owner) => format!("Shared by {owner}. Only an owner changes who it is shared with."),
        None => "Only an owner changes who it is shared with.".to_string(),
    }
}

// ── Sharing from an unhosted store (docs/RELAY_CONTRACT.md, "The apps") ──

/// What Pimble Cloud holds of a share, said under the member list. It is a
/// promise, so it has to be the true one for the store's tier: a hosted
/// store's notes are kept there as ciphertext, a store shared from this
/// computer keeps nothing there at all (docs/RELAY_CONTRACT.md).
pub fn share_keeping_note(relay: pimble_core::RelaySide) -> &'static str {
    match relay {
        pimble_core::RelaySide::Owner => {
            "The notes stay on this computer. Pimble Cloud stores none of them: it passes them along, \
             encrypted, while this computer is on, and sees who is invited and the share's name."
        }
        _ => {
            "Pimble Cloud stores the notes encrypted, and sees who is invited and the share's name. \
             It never sees what is written in them, or their titles."
        }
    }
}

/// The first way to share from a store that is not hosted, and its sentence
/// (docs/RELAY_CONTRACT.md, "The apps", word for word).
pub const SHARE_FROM_HERE_LABEL: &str = "Share from this computer";

pub const SHARE_FROM_HERE_SENTENCE: &str =
    "Nothing is uploaded. People you invite reach it while this computer is on and online.";

/// The second way, and its sentence.
pub const HOST_ON_CLOUD_LABEL: &str = "Host on Pimble Cloud...";

pub const HOST_ON_CLOUD_SENTENCE: &str = "An encrypted copy is kept there, so it works while this computer is off.";

/// What a store not yet named here is called in a list: the accounts service
/// holds no name for a store shared from a computer (nothing on Pimble Cloud
/// needs the owner's name for it), and the server calls the replica of one
/// the same until the person names it.
pub const RELAYED_PLACEHOLDER_NAME: &str = "Shared from another computer";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_fixtures::member;
    use pimble_core::StoreAccess;

    /// The note under the member list is a promise about where the notes are
    /// kept, and the two tiers keep them in different places.
    #[test]
    fn the_share_dialog_says_where_the_notes_are_kept() {
        use pimble_core::RelaySide;
        assert!(share_keeping_note(RelaySide::None).starts_with("Pimble Cloud stores the notes encrypted"));
        assert!(share_keeping_note(RelaySide::Member).starts_with("Pimble Cloud stores the notes encrypted"));
        let from_here = share_keeping_note(RelaySide::Owner);
        assert!(from_here.starts_with("The notes stay on this computer. Pimble Cloud stores none of them"), "{from_here}");
    }

    /// The two ways of sharing a store that is not hosted, and their
    /// sentences, as docs/RELAY_CONTRACT.md "The apps" has them (which way
    /// is offered is `pimble_core::plan::share_face`'s).
    #[test]
    fn the_two_ways_say_what_they_do() {
        assert_eq!(SHARE_FROM_HERE_LABEL, "Share from this computer");
        assert_eq!(
            SHARE_FROM_HERE_SENTENCE,
            "Nothing is uploaded. People you invite reach it while this computer is on and online."
        );
        assert_eq!(HOST_ON_CLOUD_LABEL, "Host on Pimble Cloud...");
        assert_eq!(HOST_ON_CLOUD_SENTENCE, "An encrypted copy is kept there, so it works while this computer is off.");
    }

    /// What the recipient's side says, in words. An editor's store says
    /// nothing and disables nothing; a reader's says "read only" and gives the
    /// one refusal sentence as the reason, the same one a refused write comes
    /// back with (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles").
    #[test]
    fn access_reads_as_plain_words() {
        assert_eq!(access_words(StoreAccess::Full), "");
        assert_eq!(access_words(StoreAccess::Read), "read only");
        assert!(access_note(StoreAccess::Full).is_empty());
        assert_eq!(access_note(StoreAccess::Read), StoreAccess::READ_ONLY_REFUSAL);
    }

    /// A member row names the person by the name they go by in the share,
    /// with the email beside it for the owner only (docs/PRESENCE_CONTRACT.md
    /// section 3); an invitation is its address.
    #[test]
    fn a_member_row_says_the_name_and_the_email_only_to_the_owner() {
        use pimble_rpc::MemberRole::{Editor, Owner};
        let named = |email: &str, name: &str, role| pimble_rpc::ShareMember { name: name.to_string(), ..member(email, role) };
        assert_eq!(member_label(&named("ann@example.com", "Chef Ann", Editor), true), "Chef Ann (ann@example.com)");
        assert_eq!(member_label(&named("", "Chef Ann", Editor), false), "Chef Ann");
        assert_eq!(member_label(&named("ann@example.com", "Chef Ann", Editor), false), "Chef Ann");
        assert_eq!(member_label(&member("new@example.com", Editor), true), "new@example.com");
        assert_eq!(member_label(&member("", Owner), false), "someone");
        assert_eq!(shared_by_sentence(None, &[named("", "Mom", Owner)]), "Shared by Mom. Only an owner changes who it is shared with.");
    }

    /// Who manages a share: the account's own row in the member list where
    /// there is one, the store's `shared_by` otherwise. The Share modal
    /// offers inviting, removing and stopping only to someone who does; a
    /// member of someone else's share was offered all three and refused each.
    #[test]
    fn who_manages_a_share() {
        use pimble_rpc::MemberRole::{Editor, Owner, Reader};
        let me = "me@example.com";

        // One's own store: before the list arrives, and once it has.
        assert!(manages_share(None, &[], me));
        assert!(manages_share(None, &[member(me, Owner), member("ann@example.com", Editor)], me));

        // Someone else's share: before the list arrives, when it never does,
        // and with the account's own row in it, whatever the role.
        let ann = Some("ann@example.com");
        assert!(!manages_share(ann, &[], me));
        assert!(!manages_share(ann, &[member("ann@example.com", Owner), member(me, Editor)], me));
        assert!(!manages_share(ann, &[member("ann@example.com", Owner), member(me, Reader)], me));
        // A list without the account's own row says nothing about it.
        assert!(!manages_share(ann, &[member("ann@example.com", Owner), member("bob@example.com", Editor)], me));
        assert!(manages_share(None, &[member("bob@example.com", Editor)], me));

        // Where the row and the store disagree the row is the fresher word.
        assert!(manages_share(ann, &[member(me, Owner)], me));
        assert!(!manages_share(None, &[member("ann@example.com", Owner), member(me, Editor)], me));

        // Addresses compare as addresses do, and nobody signed in has no row.
        assert!(!manages_share(None, &[member("Me@Example.com", Editor)], " me@example.com "));
        assert!(manages_share(None, &[member("", Editor)], ""));
        assert!(!manages_share(ann, &[member("", Owner)], ""));
    }

    /// The member's face says whose share it is: the store's `shared_by`,
    /// else the list's owner row, else only what is true of any share.
    #[test]
    fn the_members_face_says_who_shared_it() {
        use pimble_rpc::MemberRole::{Editor, Owner};
        assert_eq!(
            shared_by_sentence(Some("ann@example.com"), &[]),
            "Shared by ann@example.com. Only an owner changes who it is shared with."
        );
        assert_eq!(
            shared_by_sentence(None, &[member("me@example.com", Editor), member("ann@example.com", Owner)]),
            "Shared by ann@example.com. Only an owner changes who it is shared with."
        );
        assert_eq!(shared_by_sentence(None, &[]), "Only an owner changes who it is shared with.");
    }

    /// A menu's one note says every reason once, and nothing when there is none.
    #[test]
    fn a_menus_note_joins_its_reasons() {
        assert_eq!(menu_note_text(&["", ""]), "");
        assert_eq!(menu_note_text(&["", ONLY_AN_OWNER_SHARES_NOTE]), ONLY_AN_OWNER_SHARES_NOTE);
        assert_eq!(
            menu_note_text(&[access_note(StoreAccess::Read), ONLY_AN_OWNER_SHARES_NOTE]),
            format!("{} {}", StoreAccess::READ_ONLY_REFUSAL, ONLY_AN_OWNER_SHARES_NOTE)
        );
    }

    /// Every state a share can be in says what it means.
    #[test]
    fn share_state_reads_as_a_sentence() {
        let synced: SyncState =
            serde_json::from_str(r#"{"state":"synced","last_sync":"2026-09-17T00:00:00Z"}"#).unwrap();
        assert_eq!(share_state_text(&synced), "Everyone invited has what they need.");
        assert_eq!(share_state_text(&SyncState::Syncing), "Handing the key and the scope over...");
        assert!(share_state_text(&SyncState::Offline).starts_with("Offline."));
    }

    /// The sentence a removed member reads, word for word (Joe, 2026-09-21).
    #[test]
    fn the_sentence_for_a_share_that_ended() {
        assert_eq!(share_ended_sentence(Some("Trips")), "\"Trips\" is no longer shared with you.");
        assert_eq!(share_ended_sentence(None), "A shared folder is no longer shared with you.");
        assert_eq!(share_ended_sentence(Some("  ")), "A shared folder is no longer shared with you.", "an untitled folder is not named");
    }

    /// The words on the row of a replica whose every share has ended, in
    /// place of whose it was and what it allowed.
    #[test]
    fn the_row_of_an_ended_replica_says_no_longer_shared() {
        let mut store = Store::new_local("Holiday Plans", "/tmp/replica.pimble".into());
        assert_eq!(shared_by_words(&store), "", "one's own store says nothing");
        let (trips, recipes) = (NodeId::new(), NodeId::new());
        store.shared_by = Some("ann@example.com".to_string());
        store.roots = vec![trips, recipes];
        assert_eq!(shared_by_words(&store), "shared by ann@example.com");
        store.access = StoreAccess::Read;
        assert_eq!(shared_by_words(&store), "shared by ann@example.com · read only");
        store.ended_roots = vec![trips];
        assert_eq!(shared_by_words(&store), "shared by ann@example.com · read only", "one share of it is still held");
        store.ended_roots = vec![trips, recipes];
        assert_eq!(shared_by_words(&store), "no longer shared");
        assert_eq!(shared_by_words(&store), NO_LONGER_SHARED_WORDS);
    }
}
