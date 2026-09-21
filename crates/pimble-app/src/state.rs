//! Application state management
//!
//! Uses per-entity reactive signals so that data changes (rename, mount state)
//! only trigger effects on the affected node, while structural changes (children
//! loaded, node moved, store opened/closed) bump `tree_structure_version` to
//! trigger a full tree rebuild.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use pimble_core::{MountRef, MountState, Node, NodeId, RemoteEndpoint, Store, StoreId, SyncState};
use rinch::components::TreeNodeData;
use rinch::prelude::*;

use crate::protocol::BackendHandle;

thread_local! {
    /// The tree value (with any mount-path suffix, decision 7) of the last
    /// drag-and-drop target, set by `app.rs`'s `on_drop` right before sending
    /// `MoveNode` and consumed once by the `NodeMoved` handler in `events.rs`
    /// to auto-expand exactly the place the user dropped into, rather than
    /// reconstructing an unqualified value that would not match a target
    /// reached only through a mount.
    static LAST_DROP_TARGET_VALUE: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Record the qualified tree value of a drag-and-drop target, just before the
/// `MoveNode` command that will result in a `NodeMoved` event for it.
pub fn set_last_drop_target_value(value: String) {
    LAST_DROP_TARGET_VALUE.with(|cell| *cell.borrow_mut() = Some(value));
}

/// Take (and clear) the last recorded drop-target tree value, if any.
pub fn take_last_drop_target_value() -> Option<String> {
    LAST_DROP_TARGET_VALUE.with(|cell| cell.borrow_mut().take())
}

/// A pending mount operation waiting for a store to be opened.
#[derive(Debug, Clone)]
pub struct PendingMount {
    pub target_store_id: StoreId,
    pub target_parent_id: NodeId,
    pub source_path: String,
}

/// Current state of the search box: idle (query empty), the last successful
/// results (possibly empty), or an error/status message from the server (a
/// connection failure, or the index still building — `SearchError::IndexBuilding`
/// arrives as a plain RPC error whose text already says so, so it displays as-is).
#[derive(Debug, Clone, Default)]
pub enum SearchState {
    #[default]
    Idle,
    Results(Vec<pimble_rpc::SearchResultItem>),
    Error(String),
}

/// Mount metadata for a tree node.
#[derive(Debug, Clone)]
pub struct MountInfo {
    pub is_mount: bool,
    pub mount_state: Option<MountState>,
    /// The mount's target: source store and node. Populated as soon as we see
    /// the mount node's own data (`Node::mount_ref()`), and refreshed whenever
    /// `getMountState` answers (which is authoritative, e.g. after Agent A's
    /// `source_path` hint is filled in server-side).
    pub mount_ref: Option<MountRef>,
}

/// Extract plain-text content from node content bytes, for tree previews / labels.
///
/// `content` is a yrs snapshot (or empty). Blocks are joined by `\n`; `""` for
/// empty or unreadable content, in which case the caller falls back to the
/// node's title.
pub fn get_node_content_text(content: &[u8]) -> String {
    pimble_crdt::NodeDoc::text_of(content)
}

/// Compute a display label from an explicit-title flag/title plus a plain-text
/// content projection: an explicit title wins; otherwise the first non-empty
/// line of `content_text` (truncated to 25 chars with an ellipsis); otherwise
/// `title`; otherwise `"Untitled"`.
///
/// Shared by [`display_label_from_node`] (which projects `content_text` from
/// stored node bytes) and the editor's debounced live-label refresh
/// (`editor::schedule_label_refresh`), which supplies `content_text` straight
/// from the in-memory document instead of decoding a CRDT snapshot.
pub fn label_from_title_and_content(has_explicit_title: bool, title: &str, content_text: &str) -> String {
    if has_explicit_title && !title.is_empty() {
        return title.to_string();
    }

    let first_line = content_text
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if !first_line.is_empty() {
        let char_count = first_line.chars().count();
        if char_count > 25 {
            let truncated: String = first_line.chars().take(25).collect();
            return format!("{truncated}…");
        } else {
            return first_line.to_string();
        }
    }

    if !title.is_empty() {
        return title.to_string();
    }

    "Untitled".to_string()
}

/// The suffix a mount node's tree label carries for its current state
/// (docs/history/REMOTE_MOUNTS_CONTRACT.md "B: app side"). One function, used
/// everywhere a mount row is labelled.
pub fn mount_label_suffix(state: &MountState) -> &'static str {
    match state {
        MountState::Live => "",
        MountState::Connecting => " (connecting...)",
        MountState::Cached { .. } => " (offline copy)",
        MountState::Unavailable { .. } => " (unavailable)",
    }
}

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
    }
}

/// Why "Share..." is disabled on a node of a store that reached this device
/// as someone else's share: sharing is an owner's to do, and the server
/// refuses it from anyone else with a sentence that opens with this one
/// (docs/NODE_DOCUMENT_CONTRACT.md section 5). An item that can only be
/// refused is not offered; this says why.
pub const ONLY_AN_OWNER_SHARES_NOTE: &str = "Only an owner of this store can share from it.";

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
pub fn share_item_disabled(access: pimble_core::StoreAccess, held_as_share: bool, is_shared_root: bool) -> bool {
    if held_as_share {
        !is_shared_root
    } else {
        !access.allows_write()
    }
}

/// The sentence that says why "Share..." is disabled for a reason of its own,
/// or empty: a node of someone else's store that is not a shared root. (A
/// store of one's own held to read is covered by [`access_note`].)
pub fn share_item_note(held_as_share: bool, is_shared_root: bool) -> &'static str {
    if held_as_share && !is_shared_root {
        ONLY_AN_OWNER_SHARES_NOTE
    } else {
        ""
    }
}

/// The one dimmed line at the top of a row's menu: every reason something
/// below it is disabled, each said once, in the order given. Empty when
/// nothing is.
pub fn menu_note_text(notes: &[&str]) -> String {
    notes.iter().copied().filter(|note| !note.is_empty()).collect::<Vec<_>>().join(" ")
}

/// Whether the person at this device manages a share: invites to it, removes
/// from it, stops it. Decided from what the app already holds, never asked of
/// anyone (the server judges every request whatever the dialog offers).
///
/// The account's own row in the share's member list says it where there is
/// one, since that is the accounts service answering now: `owner` manages,
/// anything else does not. Without that row (the list has not arrived, could
/// not be loaded, or nobody is signed in) the store says it: one that reached
/// this device as someone else's share (`Store::shared_by`) is not this
/// account's to manage, and any other store is.
pub fn manages_share(shared_by: Option<&str>, members: &[pimble_rpc::ShareMember], account_email: &str) -> bool {
    let account_email = account_email.trim();
    let own_row = members
        .iter()
        .find(|member| !account_email.is_empty() && member.email.trim().eq_ignore_ascii_case(account_email));
    match own_row {
        Some(row) => row.role == pimble_rpc::MemberRole::Owner,
        None => shared_by.is_none(),
    }
}

/// The member's face of the Share modal in one sentence: whose share this
/// is, and that the list under it is not theirs to change. The owner is who
/// the store says shared it, or the member list's owner row when the store
/// does not say.
pub fn shared_by_sentence(shared_by: Option<&str>, members: &[pimble_rpc::ShareMember]) -> String {
    let owner = shared_by
        .map(str::to_string)
        .or_else(|| members.iter().find(|member| member.role == pimble_rpc::MemberRole::Owner).map(|member| member.email.clone()));
    match owner {
        Some(owner) => format!("Shared by {owner}. Only an owner changes who it is shared with."),
        None => "Only an owner changes who it is shared with.".to_string(),
    }
}

// ── Sharing from an unhosted store (docs/RELAY_CONTRACT.md, "The apps") ──

/// Which face the Share modal shows.
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

/// The Share modal's face. `shared` is whether the node carries a share (its
/// marker, then the server's answers), `manages` is [`manages_share`], and
/// `link` is what the store's link is (`Store::sync_mode`): `Vault` for a
/// store hosted on Pimble Cloud and for one shared from this computer alike,
/// which is the very thing the server asks before it lets a node be shared
/// (`pimble_server::share`, `NOT_HOSTED_REFUSAL`). A node that is shared
/// already shows its share whatever the link says.
pub fn share_face(shared: bool, manages: bool, link: pimble_core::StoreKind) -> ShareFace {
    match (shared, manages) {
        (true, true) => ShareFace::Owner,
        (true, false) => ShareFace::Member,
        (false, _) if link != pimble_core::StoreKind::Vault => ShareFace::Ways,
        (false, _) => ShareFace::Name,
    }
}

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

/// Whether "Host on Pimble Cloud..." is off for a store: one that has a link
/// already (to another Pimble server, to its hosted twin, or to its twin on
/// this computer), a copy of a store that lives somewhere else, or encrypted
/// storage, which is nobody's store to host. One rule for the store row's
/// menu and for the Share modal's two ways, because the server would not
/// refuse all of these: hosting a store linked elsewhere would replace its
/// link.
pub fn host_item_disabled(linked: bool, replica: bool, vault: bool) -> bool {
    linked || replica || vault
}

/// Why neither way of sharing is offered for a store the Share modal's
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

/// Why "Host on Pimble Cloud...", "Link to Remote..." and "Unlink from
/// Remote" are all off on the row of a store shared from this computer: it
/// is one or the other, and the person says which
/// (`pimble_server::relay_face::RELAYED_LINK_REFUSAL` is what the server
/// would answer any of them with).
pub const RELAYED_STORE_NOTE: &str =
    "This store is shared from this computer. To host it or link it elsewhere, stop sharing it from this computer first.";

/// The sentence that says why a store row's hosting and linking items are
/// disabled for a reason of their own, or empty: a store shared from this
/// computer.
pub fn relayed_store_note(relay: pimble_core::RelaySide) -> &'static str {
    match relay {
        pimble_core::RelaySide::Owner => RELAYED_STORE_NOTE,
        pimble_core::RelaySide::None | pimble_core::RelaySide::Member => "",
    }
}

/// Whether "Stop Sharing from This Computer..." is off: on every store but
/// one shared from this computer.
pub fn stop_relaying_item_disabled(relay: pimble_core::RelaySide) -> bool {
    relay != pimble_core::RelaySide::Owner
}

/// Whether "Unlink from Remote" is off: a store with no link, and a store
/// shared from this computer, whose link is to its own twin on this machine
/// and is ended by "Stop Sharing from This Computer..." (the server refuses
/// an unlink of one).
pub fn unlink_item_disabled(linked: bool, relay: pimble_core::RelaySide) -> bool {
    !linked || relay == pimble_core::RelaySide::Owner
}

/// What a store not yet named here is called in a list: the accounts service
/// holds no name for a store shared from a computer (nothing on Pimble Cloud
/// needs the owner's name for it), and the server calls the replica of one
/// the same until the person names it.
pub const RELAYED_PLACEHOLDER_NAME: &str = "Shared from another computer";

/// Whether a mount row's icon and label render dimmed: its source is out of
/// reach (`Unavailable`) or not reachable yet (`Connecting`). A `Cached`
/// mount still shows its replica's content, so it reads normally and only
/// carries the "(offline copy)" suffix.
pub fn mount_is_dimmed(state: &MountState) -> bool {
    matches!(state, MountState::Unavailable { .. } | MountState::Connecting)
}

/// Which of the four mount states a `MountState` is, without its payload.
/// `MountState` is not `PartialEq`, and neither `last_sync` nor `reason`
/// changes anything the tree does, so this is what the app compares by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountStateKind {
    Live,
    Cached,
    Unavailable,
    Connecting,
}

pub fn mount_state_kind(state: &MountState) -> MountStateKind {
    match state {
        MountState::Live => MountStateKind::Live,
        MountState::Cached { .. } => MountStateKind::Cached,
        MountState::Unavailable { .. } => MountStateKind::Unavailable,
        MountState::Connecting => MountStateKind::Connecting,
    }
}

/// Compute display label from a Node reference (no signal dependency).
pub fn display_label_from_node(node: &Node) -> String {
    let has_explicit_title = node.metadata.custom
        .get("explicit_title")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let content = get_node_content_text(&node.content);
    label_from_title_and_content(has_explicit_title, &node.metadata.title, &content)
}

/// Global application state with per-entity reactive signals.
///
/// Structural changes bump `tree_structure_version` to trigger tree rebuilds.
/// Data changes (rename, mount state) only update per-entity signals, so only
/// the affected node's render Effects fire.
#[derive(Clone, Copy)]
pub struct AppStore {
    // Backend
    pub backend: Signal<Option<BackendHandle>>,
    pub connection: Signal<ConnectionState>,
    pub pending_create_path: Signal<Option<String>>,

    // Per-entity signal registries
    pub store_ids: Signal<Vec<StoreId>>,
    pub store_data: Signal<HashMap<StoreId, Signal<Store>>>,
    pub node_data: Signal<HashMap<(StoreId, NodeId), Signal<Node>>>,
    /// Each parent's children, as their OWN canonical `(StoreId, NodeId)`
    /// pairs — the same as the parent's store for an ordinary parent, the
    /// mount's source store when the parent is a mount node (decision 1).
    pub children_of: Signal<HashMap<(StoreId, NodeId), Signal<Vec<(StoreId, NodeId)>>>>,
    pub mount_data: Signal<HashMap<(StoreId, NodeId), Signal<MountInfo>>>,

    /// Each registered store's remote endpoint (`None` when unlinked) and
    /// current sync state (docs/SYNC_CONTRACT.md "B: app side"). An entry
    /// exists for every store as soon as it's registered (see
    /// `ensure_sync_entry`), so the tree row's badge signal is always
    /// available by the time the row renders, before `GetStoreSync` answers.
    pub sync_data: Signal<HashMap<StoreId, Signal<(Option<RemoteEndpoint>, SyncState)>>>,

    /// Bumped only on structural changes (store open/close, children loaded, node moved).
    /// The data_source closure subscribes to this to trigger tree rebuilds.
    pub tree_structure_version: Signal<u64>,

    pub expanded: Signal<HashSet<(StoreId, NodeId)>>,

    // UI state
    pub connection_status: Signal<String>,
    pub server_addr: Signal<String>,
    pub selected_id: Signal<Option<String>>,
    pub node_title: Signal<String>,
    pub show_editor: Signal<bool>,

    // Inline rename
    pub renaming_node: Signal<Option<String>>,
    pub rename_text: Signal<String>,

    // Drag-and-drop
    pub drop_target: Signal<Option<String>>,

    // Rename input DOM node IDs (tree_value → input element node ID for request_focus)
    /// Rename inputs by tree value as `(doc_key, node_id)`, so the double-click
    /// handler can focus one via `request_focus`.
    pub rename_input_ids: Signal<HashMap<String, (u64, usize)>>,

    // Mount picker
    pub pending_mount: Signal<Option<PendingMount>>,

    // "Copy as Mount Source" / "Paste Mount Here": the canonical pair and
    // display title of the node last copied as a mount source, if any.
    pub mount_source: Signal<Option<(StoreId, NodeId, String)>>,

    // Editor dirty flag — set when user edits content, cleared on save/load
    pub editor_dirty: Signal<bool>,

    // Active editing state — which node is currently open in the shared editor.
    // Its content lives in the editor's collab session; edits broadcast to the
    // server live rather than being saved by wholesale replacement.
    pub active_edit: Signal<Option<ActiveEdit>>,

    // Locally-computed display label for the node currently being typed into,
    // refreshed on a debounce by `editor::schedule_label_refresh` straight from
    // the editor's in-memory document. Takes priority over the content-derived
    // label in the tree render, since the per-node signal's cached `content`
    // isn't updated while a node is under active local edit (see
    // `upsert_node`, which clears an entry once authoritative content arrives).
    pub live_label: Signal<HashMap<(StoreId, NodeId), String>>,

    // This client's unique ID (for echo suppression in notifications)
    pub client_id: Signal<String>,

    // Search: the toolbar search box's text and the last response for it.
    // The results panel replaces the tree whenever `search_query` is non-empty.
    pub search_query: Signal<String>,
    pub search_results: Signal<SearchState>,

    // "Add Remote Store..." modal (File menu, docs/history/HARDENING_CONTRACT.md "B:
    // app"): browse a remote's open stores and add one as a local replica.
    // No folder/path field — the server places the replica in its own data
    // directory, so nothing here opens a native OS dialog.
    pub connect_modal_open: Signal<bool>,
    pub connect_modal_url: Signal<String>,
    /// Empty means "use what the server saved for this remote's origin"
    /// (docs/history/HARDENING_CONTRACT.md decision 4); non-empty is sent as
    /// `AuthMethod::Bearer`.
    pub connect_modal_token: Signal<String>,
    /// Whether the token field's `PasswordInput` shows the token in clear text.
    pub connect_modal_token_visible: Signal<bool>,
    pub connect_modal_stores: Signal<Vec<Store>>,
    /// The selected store's id, as a string (the raw `Select` value).
    pub connect_modal_selected: Signal<String>,
    pub connect_modal_error: Signal<String>,
    /// True while a `ListRemoteStores` or `AddRemoteStore` request is in flight.
    pub connect_modal_busy: Signal<bool>,
    /// True while specifically waiting for `AddRemoteStore`'s answer, so the
    /// generic `BackendEvent::Error`/`StoreOpened` outcome routes into this
    /// modal's own error line / auto-close instead of the global status bar.
    pub connect_modal_pending_add: Signal<bool>,
    /// Set by "Mount Remote Store Here..." to the canonical target the mount
    /// is created under (a store root's target is its own root node,
    /// docs/history/REMOTE_MOUNTS_CONTRACT.md "B: app side"). `None` is the plain
    /// "Add Remote Store..." mode: the same modal, but the action only adds
    /// the replica. Cleared whenever the modal closes.
    pub connect_modal_target: Signal<Option<(StoreId, NodeId)>>,

    // "Link to Remote..." modal (store root context menu). `Some(store_id)`
    // is the store the modal is open for; `None` means closed.
    pub link_modal_store: Signal<Option<StoreId>>,
    pub link_modal_url: Signal<String>,
    /// Same token semantics as `connect_modal_token`.
    pub link_modal_token: Signal<String>,
    /// Whether the token field's `PasswordInput` shows the token in clear text.
    pub link_modal_token_visible: Signal<bool>,
    pub link_modal_error: Signal<String>,
    /// True while a `SetStoreSync` request from this modal is in flight.
    pub link_modal_pending: Signal<bool>,

    // "Appearance..." modal (node context menu): the node whose icon and
    // colour are being picked; `None` means closed. `appearance_icon_query`
    // filters the icon grid over the whole Tabler set; `appearance_tags_text`
    // is the tags field's text (comma separated), seeded when the modal opens.
    pub appearance_modal_node: Signal<Option<(StoreId, NodeId)>>,
    pub appearance_icon_query: Signal<String>,
    pub appearance_tags_text: Signal<String>,

    /// The theme in use (View > "Toggle Dark Mode"), persisted in `state.json`.
    /// Read reactively wherever a colour depends on the scheme.
    pub dark_mode: Signal<bool>,

    // "Mount Store..." picker (browser builds). The desktop picks a directory
    // on this machine; a browser has no directories to pick from, so it picks
    // one of the stores the account already grants. `Some(target)` is the
    // canonical node the mount is created under; `None` means closed.
    pub mount_picker_target: Signal<Option<(StoreId, NodeId)>>,
    /// The chosen source store's id, as the raw `Select` value.
    pub mount_picker_selected: Signal<String>,
    pub mount_picker_error: Signal<String>,
    /// True while this modal's `CreateMount` is in flight.
    pub mount_picker_pending: Signal<bool>,

    // "New Store..." modal (the explorer's "+" with nothing open yet, and the
    // menu item). Only builds that can create a hosted store show it: the
    // accounts service owns those, and a desktop build makes its stores as
    // files through a native dialog instead.
    pub new_store_modal_open: Signal<bool>,
    pub new_store_modal_name: Signal<String>,
    pub new_store_modal_error: Signal<String>,
    /// True while a `CreateHostedStore` request from this modal is in flight,
    /// so its outcome lands in the modal rather than the status bar.
    pub new_store_modal_pending: Signal<bool>,

    // "Remove Replica..." confirmation modal (store root context menu,
    // docs/history/HARDENING_CONTRACT.md "B: app"). `Some(store_id)` is the replica
    // the modal is open for; `None` means closed.
    pub remove_replica_modal_store: Signal<Option<StoreId>>,
    pub remove_replica_modal_error: Signal<String>,
    /// True while a `RemoveReplica` request from this modal is in flight.
    pub remove_replica_modal_pending: Signal<bool>,

    // Pimble Cloud account (docs/DESKTOP_ACCOUNT_CONTRACT.md): what the
    // server's keystore reports, refreshed by `CloudStatus` on every connect
    // and by every sign-in or sign-out.
    pub cloud_signed_in: Signal<bool>,
    pub cloud_email: Signal<String>,
    pub cloud_url: Signal<String>,

    // "Account..." modal (Account menu, the status bar, and "Host on Pimble
    // Cloud..." with nothing signed in). One modal, two faces: the sign-in
    // form while nothing is signed in, the signed-in view otherwise
    // (decision 1).
    pub account_modal_open: Signal<bool>,
    /// The service URL field; `"https://pimble.app"` when first opened, then
    /// whatever was last typed in this run (not persisted).
    pub account_modal_url: Signal<String>,
    pub account_modal_email: Signal<String>,
    /// Cleared whenever the modal closes and on a successful sign-in.
    pub account_modal_password: Signal<String>,
    pub account_modal_password_visible: Signal<bool>,
    /// True while a `CloudSignIn` or `CloudSignOut` is in flight.
    pub account_modal_busy: Signal<bool>,
    pub account_modal_error: Signal<String>,
    /// Why the modal opened, when it did not open from the menu: "Sign in to
    /// host a store", or empty.
    pub account_modal_hint: Signal<String>,

    // "Host on Pimble Cloud..." confirmation (store root context menu).
    // `Some(store_id)` is the store it is open for; `None` means closed.
    pub host_modal_store: Signal<Option<StoreId>>,
    /// True while a `CloudHostStore` is in flight.
    pub host_modal_busy: Signal<bool>,
    pub host_modal_error: Signal<String>,

    // "Add Hosted Store..." modal (Account menu): the account's encrypted
    // stores not already open here, one of which becomes a local replica.
    pub hosted_modal_open: Signal<bool>,
    pub hosted_modal_stores: Signal<Vec<pimble_rpc::CloudHostedStoreInfo>>,
    /// The ids among `hosted_modal_stores` that are not hosted at all: served
    /// from their owner's computer through Pimble Cloud's relay
    /// (docs/RELAY_CONTRACT.md). The list says so on each.
    pub hosted_modal_relayed: Signal<Vec<String>>,
    /// The selected store's id, as a string (the raw `Select` value).
    pub hosted_modal_selected: Signal<String>,
    /// True while a `CloudListHostedStores` or `CloudAddHostedStore` is in flight.
    pub hosted_modal_busy: Signal<bool>,
    pub hosted_modal_error: Signal<String>,

    // "Share..." modal (node context menu, docs/NODE_DOCUMENT_CONTRACT.md
    // section 5). A share is a scoped grant on this store: the node and the
    // documents under it, with a role per member. One modal, two faces: the
    // name field and "Share" while the node is not shared, the members and
    // "Stop sharing" once it is. Someone who does not manage the share (a
    // member looking at the share they are in, `share_modal_manages`) gets a
    // third: who shared it and who is on it, with nothing to press.
    /// The canonical node the modal is open for; `None` means closed.
    pub share_modal_node: Signal<Option<(StoreId, NodeId)>>,
    /// The share's own name, which is not the node's title: the owner types it
    /// and it is what a recipient's store list shows. Pimble Cloud sees it,
    /// and so does everyone invited.
    pub share_modal_name: Signal<String>,
    /// Which face the modal shows. Seeded from the node's own share marker
    /// when the modal opens (that is the local truth about whether a node is
    /// shared) and moved by `CloudShareUpdated` / `CloudSharingStopped`.
    pub share_modal_shared: Signal<bool>,
    pub share_modal_members: Signal<Vec<pimble_rpc::ShareMember>>,
    /// What the owner's side of the share is doing, in words. Kept apart from
    /// `share_modal_shared` because `ShareStateChanged` updates it on its own.
    pub share_modal_state: Signal<String>,
    pub share_modal_invite_email: Signal<String>,
    /// `"editor"` or `"reader"` — the raw `Select` value of the role field.
    pub share_modal_invite_role: Signal<String>,
    /// Which sharing request is in flight, so its own button shows the
    /// spinner and the others are held back; `None` when nothing is.
    pub share_modal_pending: Signal<Option<crate::protocol::CloudOp>>,
    pub share_modal_error: Signal<String>,
    /// The "Stop sharing" confirmation is showing instead of the member list.
    pub share_modal_confirm_stop: Signal<bool>,
    /// The node whose Share modal sent the person to "Host on Pimble
    /// Cloud..." (docs/RELAY_CONTRACT.md, the second of the two ways): once
    /// the store is hosted the modal opens on it again, as it does at once
    /// after "Share from this computer". Cleared when the hosting
    /// confirmation is closed without hosting.
    pub share_after_host: Signal<Option<(StoreId, NodeId)>>,

    // "Stop Sharing from This Computer..." confirmation (store root context
    // menu, docs/RELAY_CONTRACT.md). `Some(store_id)` is the store it is open
    // for; `None` means closed.
    pub stop_relay_modal_store: Signal<Option<StoreId>>,
    /// True while a `CloudStopRelaying` is in flight.
    pub stop_relay_modal_busy: Signal<bool>,
    pub stop_relay_modal_error: Signal<String>,

    /// The stores a backend has said are out of reach because their owner's
    /// computer is (`StoreSyncChanged::owner_offline`): the row's badge reads
    /// `owner offline`.
    pub owner_offline: Signal<HashSet<StoreId>>,

    /// A sentence the server sent back refusing a command (`Forbidden`, e.g.
    /// a store shared read-only), shown in the status bar. Not an error
    /// state: the connection is fine, the command was simply not allowed
    /// (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles").
    pub notice: Signal<String>,
}

/// Identifies the node currently open in the shared editor.
#[derive(Clone)]
pub struct ActiveEdit {
    pub store_id: StoreId,
    pub node_id: NodeId,
}

impl AppStore {
    pub fn new() -> Self {
        Self {
            backend: Signal::new(None),
            connection: Signal::new(ConnectionState::Disconnected),
            pending_create_path: Signal::new(None),
            store_ids: Signal::new(Vec::new()),
            store_data: Signal::new(HashMap::new()),
            node_data: Signal::new(HashMap::new()),
            children_of: Signal::new(HashMap::new()),
            mount_data: Signal::new(HashMap::new()),
            sync_data: Signal::new(HashMap::new()),
            tree_structure_version: Signal::new(0),
            expanded: Signal::new(HashSet::new()),
            connection_status: Signal::new("Connecting...".to_string()),
            server_addr: Signal::new(String::new()),
            selected_id: Signal::new(None),
            node_title: Signal::new(String::new()),
            show_editor: Signal::new(false),
            renaming_node: Signal::new(None),
            rename_text: Signal::new(String::new()),
            drop_target: Signal::new(None),
            rename_input_ids: Signal::new(HashMap::new()),
            pending_mount: Signal::new(None),
            mount_source: Signal::new(None),
            editor_dirty: Signal::new(false),
            active_edit: Signal::new(None),
            live_label: Signal::new(HashMap::new()),
            client_id: Signal::new(String::new()),
            search_query: Signal::new(String::new()),
            search_results: Signal::new(SearchState::Idle),
            connect_modal_open: Signal::new(false),
            connect_modal_url: Signal::new(String::new()),
            connect_modal_token: Signal::new(String::new()),
            connect_modal_token_visible: Signal::new(false),
            connect_modal_stores: Signal::new(Vec::new()),
            connect_modal_selected: Signal::new(String::new()),
            connect_modal_error: Signal::new(String::new()),
            connect_modal_busy: Signal::new(false),
            connect_modal_pending_add: Signal::new(false),
            connect_modal_target: Signal::new(None),
            link_modal_store: Signal::new(None),
            link_modal_url: Signal::new(String::new()),
            link_modal_token: Signal::new(String::new()),
            link_modal_token_visible: Signal::new(false),
            link_modal_error: Signal::new(String::new()),
            link_modal_pending: Signal::new(false),
            appearance_modal_node: Signal::new(None),
            appearance_icon_query: Signal::new(String::new()),
            appearance_tags_text: Signal::new(String::new()),
            dark_mode: Signal::new(true),
            mount_picker_target: Signal::new(None),
            mount_picker_selected: Signal::new(String::new()),
            mount_picker_error: Signal::new(String::new()),
            mount_picker_pending: Signal::new(false),
            new_store_modal_open: Signal::new(false),
            new_store_modal_name: Signal::new(String::new()),
            new_store_modal_error: Signal::new(String::new()),
            new_store_modal_pending: Signal::new(false),
            remove_replica_modal_store: Signal::new(None),
            remove_replica_modal_error: Signal::new(String::new()),
            remove_replica_modal_pending: Signal::new(false),
            cloud_signed_in: Signal::new(false),
            cloud_email: Signal::new(String::new()),
            cloud_url: Signal::new(String::new()),
            account_modal_open: Signal::new(false),
            account_modal_url: Signal::new(String::new()),
            account_modal_email: Signal::new(String::new()),
            account_modal_password: Signal::new(String::new()),
            account_modal_password_visible: Signal::new(false),
            account_modal_busy: Signal::new(false),
            account_modal_error: Signal::new(String::new()),
            account_modal_hint: Signal::new(String::new()),
            host_modal_store: Signal::new(None),
            host_modal_busy: Signal::new(false),
            host_modal_error: Signal::new(String::new()),
            hosted_modal_open: Signal::new(false),
            hosted_modal_stores: Signal::new(Vec::new()),
            hosted_modal_relayed: Signal::new(Vec::new()),
            hosted_modal_selected: Signal::new(String::new()),
            hosted_modal_busy: Signal::new(false),
            hosted_modal_error: Signal::new(String::new()),
            share_modal_node: Signal::new(None),
            share_modal_name: Signal::new(String::new()),
            share_modal_shared: Signal::new(false),
            share_modal_members: Signal::new(Vec::new()),
            share_modal_state: Signal::new(String::new()),
            share_modal_invite_email: Signal::new(String::new()),
            share_modal_invite_role: Signal::new("editor".to_string()),
            share_modal_pending: Signal::new(None),
            share_modal_error: Signal::new(String::new()),
            share_modal_confirm_stop: Signal::new(false),
            share_after_host: Signal::new(None),
            stop_relay_modal_store: Signal::new(None),
            stop_relay_modal_busy: Signal::new(false),
            stop_relay_modal_error: Signal::new(String::new()),
            owner_offline: Signal::new(HashSet::new()),
            notice: Signal::new(String::new()),
        }
    }

    /// What this device may change in a store (docs/NODE_DOCUMENT_CONTRACT.md
    /// section 5, "Roles"), untracked. `Full` for one's own stores and for an
    /// editor's scope — an editor edits everything in it, structure included —
    /// and `Read` for a reader. A store the app has not registered yet reads
    /// as `Full`: nothing is disabled on a guess.
    pub fn store_access(&self, store_id: StoreId) -> pimble_core::StoreAccess {
        untracked(|| {
            self.store_data.with(|map| {
                map.get(&store_id)
                    .map_or(pimble_core::StoreAccess::Full, |sig| sig.with(|s| s.access))
            })
        })
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
        untracked(|| self.node_access_in(store_id, node_id))
    }

    /// The access the held copy of a node carries (untracked), whatever its
    /// store says: what an answer's `Node::access` is compared with to tell
    /// that the server's judgement of the node changed. `Full` for a node
    /// not held.
    pub fn node_held_access(&self, store_id: StoreId, node_id: NodeId) -> pimble_core::StoreAccess {
        untracked(|| {
            self.node_data
                .with(|map| map.get(&(store_id, node_id)).map_or(pimble_core::StoreAccess::Full, |sig| sig.with(|n| n.access)))
        })
    }

    /// [`AppStore::node_access`] for a caller already inside `untracked`
    /// (`build_tree_data_structural`).
    fn node_access_in(&self, store_id: StoreId, node_id: NodeId) -> pimble_core::StoreAccess {
        let store_access = self
            .store_data
            .with(|map| map.get(&store_id).map_or(pimble_core::StoreAccess::Full, |sig| sig.with(|s| s.access)));
        if !store_access.allows_write() {
            return store_access;
        }
        self.node_data
            .with(|map| map.get(&(store_id, node_id)).map_or(pimble_core::StoreAccess::Full, |sig| sig.with(|n| n.access)))
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

    /// Every loaded children list that holds nodes of `store_id` (untracked):
    /// the lists of its own parents, and the list of any mount, in whatever
    /// store, whose children are this store's. What is refetched when the
    /// store's access changes, since each node carries the server's
    /// judgement of itself and no notification names the nodes it changed for.
    pub fn loaded_lists_holding(&self, store_id: StoreId) -> Vec<(StoreId, NodeId)> {
        untracked(|| {
            self.children_of.with(|map| {
                map.iter()
                    .filter(|((list_store, _), sig)| {
                        *list_store == store_id || sig.with(|children| children.iter().any(|(child_store, _)| *child_store == store_id))
                    })
                    .map(|(&key, _)| key)
                    .collect()
            })
        })
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

    /// Bump the tree structure version to trigger a tree rebuild.
    pub fn bump_tree_structure(&self) {
        self.tree_structure_version.update(|v| *v += 1);
    }

    /// Insert or update a store's signal and add to store_ids if new.
    ///
    /// Looks up the inner signal first, then sets it *outside* the outer
    /// HashMap borrow to avoid RefCell re-entrancy if subscribers read back.
    pub fn upsert_store(&self, store: Store) {
        let sid = store.id;
        let existing = self.store_data.with(|map| map.get(&sid).copied());
        if let Some(sig) = existing {
            sig.set(store);
        } else {
            // Pre-create signal outside .update() — Signal::new() and .update()
            // both borrow SIGNAL_STORE mutably, so nesting them panics.
            let new_sig = Signal::new(store);
            self.store_data.update(|map| {
                map.insert(sid, new_sig);
            });
        }
        self.store_ids.update(|ids| {
            if !ids.contains(&sid) {
                ids.push(sid);
            }
        });
    }

    /// Remove a store and all its associated per-entity signals.
    pub fn remove_store(&self, store_id: StoreId) {
        self.store_ids.update(|ids| ids.retain(|&id| id != store_id));
        self.store_data.update(|map| { map.remove(&store_id); });
        self.node_data.update(|map| { map.retain(|(sid, _), _| *sid != store_id); });
        self.children_of.update(|map| { map.retain(|(sid, _), _| *sid != store_id); });
        self.mount_data.update(|map| { map.retain(|(sid, _), _| *sid != store_id); });
        self.live_label.update(|map| { map.retain(|(sid, _), _| *sid != store_id); });
        self.sync_data.update(|map| { map.remove(&store_id); });
        self.set_owner_offline(store_id, false);
    }

    /// Remove a node and its per-entity signals (node_data, mount_data, children_of).
    pub fn remove_node(&self, store_id: StoreId, node_id: NodeId) {
        let key = (store_id, node_id);
        self.node_data.update(|map| { map.remove(&key); });
        self.mount_data.update(|map| { map.remove(&key); });
        self.children_of.update(|map| { map.remove(&key); });
        self.live_label.update(|map| { map.remove(&key); });
    }

    /// Remove `node_id` and every cached descendant from the app's caches,
    /// and clear the selection if it was anywhere inside — the server
    /// deletes a whole subtree in one go (docs/history/HARDENING_CONTRACT.md item
    /// 10), so both the local echo of a delete and a remote `NodeDeleted`
    /// notification (which names only the subtree's root) need to drop the
    /// rest of it here too.
    ///
    /// Recurses through each cached node's own `children` field, which is
    /// always addressed within `store_id` — never through the `children_of`
    /// cache, whose entry for a MOUNT node holds pairs from the mount's
    /// SOURCE store: those are a different store's real nodes, unaffected by
    /// deleting the mount placeholder, and must not be swept up here.
    pub fn remove_subtree(&self, store_id: StoreId, node_id: NodeId) {
        let child_ids: Vec<NodeId> = untracked(|| {
            self.node_data.with(|map| {
                map.get(&(store_id, node_id))
                    .map(|sig| sig.with(|n| n.children.clone()))
                    .unwrap_or_default()
            })
        });
        for child_id in child_ids {
            self.remove_subtree(store_id, child_id);
        }
        self.remove_node(store_id, node_id);

        if let Some(selected_id) = self.selected_id.get() {
            if let Some((sel_sid, Some(sel_nid))) = parse_tree_value(&selected_id) {
                if sel_sid == store_id && sel_nid == node_id {
                    self.selected_id.set(None);
                    self.node_title.set(String::new());
                    self.show_editor.set(false);
                }
            }
        }
    }

    /// Insert or update a node's per-entity signal.
    ///
    /// Sets the inner signal outside the outer borrow to avoid re-entrancy.
    pub fn upsert_node(&self, store_id: StoreId, node: Node) {
        let key = (store_id, node.id);
        let existing = self.node_data.with(|map| map.get(&key).copied());
        if let Some(sig) = existing {
            sig.set(node);
        } else {
            let new_sig = Signal::new(node);
            self.node_data.update(|map| {
                map.insert(key, new_sig);
            });
        }
        // Authoritative content just replaced the cached node — drop any stale
        // locally-computed label override for it (see `live_label`).
        self.live_label.update(|m| { m.remove(&key); });
    }

    /// Set children for a parent node's per-entity signal. `children` are
    /// each child's own canonical `(StoreId, NodeId)` pair (decision 1).
    ///
    /// Sets the inner signal outside the outer borrow to avoid re-entrancy.
    pub fn set_children(&self, store_id: StoreId, parent_id: NodeId, children: Vec<(StoreId, NodeId)>) {
        let key = (store_id, parent_id);
        let existing = self.children_of.with(|map| map.get(&key).copied());
        if let Some(sig) = existing {
            sig.set(children);
        } else {
            let new_sig = Signal::new(children);
            self.children_of.update(|map| {
                map.insert(key, new_sig);
            });
        }
    }

    /// Get the per-node signal (untracked read of the registry).
    pub fn get_node_signal(&self, store_id: StoreId, node_id: NodeId) -> Option<Signal<Node>> {
        untracked(|| self.node_data.with(|map| map.get(&(store_id, node_id)).copied()))
    }

    /// Get the per-store signal (untracked read of the registry).
    pub fn get_store_signal(&self, store_id: StoreId) -> Option<Signal<Store>> {
        untracked(|| self.store_data.with(|map| map.get(&store_id).copied()))
    }

    /// Get the per-mount signal (untracked read of the registry).
    pub fn get_mount_signal(&self, store_id: StoreId, node_id: NodeId) -> Option<Signal<MountInfo>> {
        untracked(|| self.mount_data.with(|map| map.get(&(store_id, node_id)).copied()))
    }

    /// Get the children signal for a parent (untracked read of the registry).
    pub fn get_children_signal(&self, store_id: StoreId, node_id: NodeId) -> Option<Signal<Vec<(StoreId, NodeId)>>> {
        untracked(|| self.children_of.with(|map| map.get(&(store_id, node_id)).copied()))
    }

    /// Check if children have been loaded for a parent.
    pub fn has_children_loaded(&self, store_id: StoreId, node_id: NodeId) -> bool {
        untracked(|| self.children_of.with(|map| map.contains_key(&(store_id, node_id))))
    }

    /// Get the root node id for a store (untracked).
    pub fn root_node_id(&self, store_id: StoreId) -> Option<NodeId> {
        self.get_store_signal(store_id).map(|sig| untracked(|| sig.with(|s| s.root_node_id)))
    }

    /// Get all local paths of open stores (untracked, for persistence).
    pub fn all_store_local_paths(&self) -> Vec<String> {
        untracked(|| {
            let ids = self.store_ids.get();
            ids.iter().filter_map(|&sid| {
                self.store_data.with(|map| {
                    map.get(&sid).and_then(|sig| {
                        sig.with(|s| s.local_path().map(|p| p.to_string_lossy().to_string()))
                    })
                })
            }).collect()
        })
    }

    /// Update mount info for a node. Call this whenever a node is inserted into the cache.
    /// Picks up `mount_ref` straight from the node's own data
    /// (`Node::mount_ref()`) when present, so a mount's target is known as
    /// soon as its node is seen, without waiting on `getMountState`.
    pub fn track_mount_info(&self, store_id: StoreId, node: &pimble_core::Node) {
        if node.is_mount() {
            let key = (store_id, node.id);
            let mount_ref = node.mount_ref();
            let existing = self.mount_data.with(|map| map.get(&key).copied());
            if let Some(sig) = existing {
                sig.update(|m| {
                    m.is_mount = true;
                    if mount_ref.is_some() {
                        m.mount_ref = mount_ref.clone();
                    }
                });
            } else {
                let new_sig = Signal::new(MountInfo {
                    is_mount: true,
                    mount_state: None,
                    mount_ref,
                });
                self.mount_data.update(|map| {
                    map.insert(key, new_sig);
                });
            }
        }
    }

    /// Update the mount state (and the authoritative `mount_ref` the
    /// `getMountState` RPC returns alongside it) for a specific node.
    pub fn set_mount_state(&self, store_id: StoreId, node_id: NodeId, state: MountState, mount_ref: MountRef) {
        let key = (store_id, node_id);
        let existing = self.mount_data.with(|map| map.get(&key).copied());
        if let Some(sig) = existing {
            sig.update(|m| { m.mount_state = Some(state); m.mount_ref = Some(mount_ref); });
        } else {
            let new_sig = Signal::new(MountInfo {
                is_mount: true,
                mount_state: Some(state),
                mount_ref: Some(mount_ref),
            });
            self.mount_data.update(|map| {
                map.insert(key, new_sig);
            });
        }
    }

    /// Update just a mount's state, keeping whatever `mount_ref` is already
    /// known — for a `MountStateChanged` notification, which carries only the
    /// node and the new state (docs/history/REMOTE_MOUNTS_CONTRACT.md decision 5).
    /// [`set_mount_state`] stays for the `getMountState` answer, which is
    /// authoritative about the ref as well.
    ///
    /// Returns whether this is news, which is what decides if the caller
    /// refetches. It is `false` in two cases:
    ///
    /// - the state's kind is the one the row already shows. The server
    ///   re-derives a mount's state on every category transition of the
    ///   source's sync link, and a link whose remote is down flaps `Syncing`
    ///   ↔ `Offline` on a backoff forever — every one of those maps to the
    ///   same `Cached`, and refetching the mount's children on each would be
    ///   an endless poll.
    /// - the app holds no entry for that node. Stale `resolved_mounts`
    ///   entries for a deleted mount are tolerated server-side (decision 5),
    ///   so a notification can name a mount this app has already dropped;
    ///   caching state for a node that no longer exists would only produce
    ///   `GetChildren` calls the server answers with "node not found".
    pub fn update_mount_state(&self, store_id: StoreId, node_id: NodeId, state: MountState) -> bool {
        let key = (store_id, node_id);
        let Some(sig) = untracked(|| self.mount_data.with(|map| map.get(&key).copied())) else {
            return false;
        };
        let kind = mount_state_kind(&state);
        let was = untracked(|| sig.with(|m| m.mount_state.as_ref().map(mount_state_kind)));
        sig.update(|m| { m.is_mount = true; m.mount_state = Some(state); });
        was != Some(kind)
    }

    /// The source store a mount node points at, if its `mount_ref` is known
    /// (untracked).
    pub fn mount_source_store(&self, store_id: StoreId, node_id: NodeId) -> Option<StoreId> {
        untracked(|| {
            self.mount_data.with(|map| {
                map.get(&(store_id, node_id))
                    .and_then(|sig| sig.with(|m| m.mount_ref.as_ref().map(|r| r.source_store)))
            })
        })
    }

    /// Whether a node's row is currently expanded in the tree (untracked).
    pub fn is_expanded(&self, store_id: StoreId, node_id: NodeId) -> bool {
        untracked(|| self.expanded.with(|e| e.contains(&(store_id, node_id))))
    }

    /// Ensure a store has a sync-status entry, defaulting to unlinked/offline.
    /// Called as soon as a store is registered (`register_opened_store`) so
    /// the tree row's badge signal always exists by the time the row
    /// renders, ahead of `GetStoreSync`'s answer.
    pub fn ensure_sync_entry(&self, store_id: StoreId) {
        let exists = self.sync_data.with(|map| map.contains_key(&store_id));
        if !exists {
            let new_sig = Signal::new((None, SyncState::Offline));
            self.sync_data.update(|map| {
                map.insert(store_id, new_sig);
            });
        }
    }

    /// Get the per-store sync signal (untracked read of the registry).
    pub fn get_sync_signal(&self, store_id: StoreId) -> Option<Signal<(Option<RemoteEndpoint>, SyncState)>> {
        untracked(|| self.sync_data.with(|map| map.get(&store_id).copied()))
    }

    /// Set a store's remote endpoint and sync state (answer to
    /// `SetStoreSync`/`GetStoreSync`, which always carry both).
    pub fn set_sync(&self, store_id: StoreId, remote: Option<RemoteEndpoint>, state: SyncState) {
        let existing = self.sync_data.with(|map| map.get(&store_id).copied());
        if let Some(sig) = existing {
            sig.set((remote, state));
        } else {
            let new_sig = Signal::new((remote, state));
            self.sync_data.update(|map| {
                map.insert(store_id, new_sig);
            });
        }
    }

    /// Update just a store's sync state, preserving whatever remote endpoint
    /// is already known — for a bare `SyncStateChanged` notification, which
    /// carries only the new state (decision 6, docs/SYNC_CONTRACT.md).
    pub fn update_sync_state(&self, store_id: StoreId, state: SyncState) {
        let existing = self.sync_data.with(|map| map.get(&store_id).copied());
        if let Some(sig) = existing {
            sig.update(|(_, s)| *s = state);
        } else {
            let new_sig = Signal::new((None, state));
            self.sync_data.update(|map| {
                map.insert(store_id, new_sig);
            });
        }
    }

    /// Whether a store holds only encrypted blobs (untracked).
    ///
    /// Nothing about mounts, replication or a server-side index applies to one,
    /// so the tree hides those where they would only fail.
    pub fn is_vault(&self, store_id: StoreId) -> bool {
        untracked(|| {
            self.store_data.with(|map| {
                map.get(&store_id)
                    .map_or(false, |sig| sig.with(|s| s.kind == pimble_core::StoreKind::Vault))
            })
        })
    }

    /// Whether a store currently has a remote endpoint linked (untracked).
    pub fn is_linked(&self, store_id: StoreId) -> bool {
        untracked(|| {
            self.sync_data.with(|map| {
                map.get(&store_id).map_or(false, |sig| sig.with(|(remote, _)| remote.is_some()))
            })
        })
    }

    /// Canonical pairs of every mount node whose `mount_ref` names exactly
    /// `(source_store, source_node)` (untracked). Used for the exact refetch
    /// (docs/history/HARDENING_CONTRACT.md "B: app"): a structural notification names
    /// every parent it touched, so only mounts sourced from those precise
    /// `(store, parent)` pairs need refreshing — never a whole-store scan.
    pub fn mounts_sourced_from_node(&self, source_store: StoreId, source_node: NodeId) -> Vec<(StoreId, NodeId)> {
        untracked(|| {
            self.mount_data.with(|map| {
                map.iter()
                    .filter_map(|(&key, sig)| {
                        let matches = sig.with(|m| {
                            m.mount_ref.as_ref().map_or(false, |r| {
                                r.source_store == source_store && r.source_node == source_node
                            })
                        });
                        if matches { Some(key) } else { None }
                    })
                    .collect()
            })
        })
    }

    /// A node's cached parent id (untracked) — `None` if the node isn't
    /// cached or has no parent (a store root). Used by the `TreeStructure`
    /// exact refetch to find "that listed id's cached parent"
    /// (docs/history/HARDENING_CONTRACT.md "B: app").
    pub fn cached_parent_id(&self, store_id: StoreId, node_id: NodeId) -> Option<NodeId> {
        untracked(|| {
            self.node_data.with(|map| {
                map.get(&(store_id, node_id)).and_then(|sig| sig.with(|n| n.parent_id))
            })
        })
    }

    /// Check if a node is a mount point (untracked).
    pub fn is_mount(&self, store_id: StoreId, node_id: NodeId) -> bool {
        untracked(|| {
            self.mount_data.with(|map| {
                map.get(&(store_id, node_id)).map_or(false, |sig| sig.with(|m| m.is_mount))
            })
        })
    }

    /// Everything a node's row snapshots from the node itself at render time:
    /// its custom icon and colour, and whether it is shared. Part of the row's
    /// `TreeNodeData` label so a change re-renders the row (rinch #714), and
    /// nothing reads it back.
    fn row_snapshot(&self, store_id: StoreId, node_id: NodeId) -> String {
        self.node_data.with(|map| {
            map.get(&(store_id, node_id))
                .map(|sig| {
                    sig.with(|n| {
                        format!(
                            "{}|{}|{}",
                            n.metadata.icon().unwrap_or(""),
                            n.metadata.color().unwrap_or(""),
                            if n.metadata.share().is_some() { "shared" } else { "" },
                        )
                    })
                })
                .unwrap_or_default()
        })
    }

    /// Build structural tree data for the Tree component's data_source.
    ///
    /// Must be called from within an `untracked()` context. Builds TreeNodeData
    /// with empty labels for nodes (render_node Effects fill them reactively)
    /// and store names for store roots.
    pub fn build_tree_data_structural(&self) -> Vec<TreeNodeData> {
        let store_ids = self.store_ids.get();
        // Same rinch #714 reason as the link state below: "Paste Mount Here"
        // snapshots whether a mount source is copied at render time, so
        // every row's data must change when that flips, or no row re-renders
        // and the item never enables.
        let paste = self.mount_source.with(|s| s.is_some());
        let mut result = Vec::new();
        for &sid in &store_ids {
            let store_info = self.store_data.with(|map| {
                map.get(&sid).map(|sig| sig.with(|s| (s.name.clone(), s.root_node_id, s.roots.clone())))
            });
            let Some((store_name, root_id, roots)) = store_info else { continue };

            // rinch's Tree re-renders a row only when its `TreeNodeData`
            // changes, and the store row's context menu snapshots whether the
            // store is linked at render time (rinch #714 keeps those items
            // static). The row renderer draws its own label from the store
            // signal and never reads this one, so the label carries the link
            // state: linking or unlinking changes the data and re-renders the
            // row with fresh menu items.
            let linked = self.sync_data.with(|map| {
                map.get(&sid).map_or(false, |sig| sig.with(|(remote, _)| remote.is_some()))
            });
            // A store row's icon and colour come from its root node's
            // metadata — a partial replica's row has no root of its own to
            // take them from (see `store_row_node`).
            let root_appearance =
                if roots.is_empty() { self.row_snapshot(sid, root_id) } else { String::new() };
            // What this device may change here, and who shared it: the row's
            // menu snapshots both (docs/NODE_DOCUMENT_CONTRACT.md section 5),
            // so a change has to change the data. The scope roots are in there
            // too: a second share of the same store extends this replica, and
            // the row has to rebuild with the new root under it.
            let access = self.store_data.with(|map| {
                map.get(&sid).map(|sig| sig.with(|s| format!("{:?}{:?}", s.access, s.shared_by))).unwrap_or_default()
            });
            // Which end of the relay this device is for the store
            // (docs/RELAY_CONTRACT.md): the row's menu snapshots it for "Stop
            // Sharing from This Computer..." and for what a store shared from
            // here turns off, so sharing from this computer, or stopping,
            // has to change the data.
            let relay = self
                .store_data
                .with(|map| map.get(&sid).map_or(pimble_core::RelaySide::None, |sig| sig.with(|s| s.relay)));
            let roots_key: Vec<String> = roots.iter().map(|r| r.to_string()).collect();
            let store_node = TreeNodeData::new(
                format!("store_{}", sid),
                format!(
                    "{store_name} (linked: {linked}, relay: {relay:?}, paste: {paste}, {access}, {root_appearance}, roots: {})",
                    roots_key.join(",")
                ),
            );
            // A whole store's row stands for its root: the root's children hang
            // straight off it, as they always have. A partial replica's row
            // stands for someone else's store, of which this device holds only
            // the shared subtrees — so each scope root is a row of its own
            // (docs/NODE_DOCUMENT_CONTRACT.md section 5, "The recipient's
            // replica"). Its tree value is the ordinary `node_{store}_{node}`,
            // so opening, renaming and the context menu need no special case.
            let children = if roots.is_empty() {
                self.build_children_structural(sid, root_id, paste, &[])
            } else {
                roots.iter().map(|&root| self.structural_node(sid, root, paste, &[])).collect()
            };
            if children.is_empty() {
                result.push(store_node);
            } else {
                result.push(store_node.with_children(children));
            }
        }
        result
    }

    /// Build structural children recursively (called from untracked context).
    ///
    /// `mount_path` accumulates the canonical `(mount_store, mount_node)` pair
    /// of every mount node crossed to reach this level, outermost first
    /// (decision 7) — empty for a node reached directly from its own store's
    /// root. It disambiguates the same canonical node appearing in several
    /// places, since rinch's Tree keys expansion and selection by value string.
    fn build_children_structural(
        &self,
        store_id: StoreId,
        parent_id: NodeId,
        paste: bool,
        mount_path: &[(StoreId, NodeId)],
    ) -> Vec<TreeNodeData> {
        let children = self.children_of.with(|map| {
            map.get(&(store_id, parent_id)).map(|sig| sig.get())
        });
        let Some(children) = children else { return Vec::new() };

        children
            .iter()
            .map(|&(child_store, child_id)| self.structural_node(child_store, child_id, paste, mount_path))
            .collect()
    }

    /// One row: the node itself plus whatever hangs below it. Used for every
    /// child of a loaded parent, and for each scope root of a partial replica,
    /// which is a row under the store row rather than a child of anything
    /// (see `build_tree_data_structural`).
    fn structural_node(
        &self,
        child_store: StoreId,
        child_id: NodeId,
        paste: bool,
        mount_path: &[(StoreId, NodeId)],
    ) -> TreeNodeData {
        let suffix = mount_path_suffix(mount_path);
        let is_mount = self.mount_data.with(|map| {
            map.get(&(child_store, child_id)).map_or(false, |sig| sig.with(|m| m.is_mount))
        });

        // The renderer never reads this label (render_node Effects draw
        // it reactively); it only carries what the row snapshots at render
        // time — the "Paste Mount Here" state, the node's custom icon and
        // colour, its share marker, and what this device may change of the
        // node (`node_access`: its own store's access, then the server's
        // judgement of this node, so a reader's root beside an editor's
        // disables its own rows and no others) — so the row re-renders when
        // any of them changes (see `build_tree_data_structural`). A mount
        // row's "New Node" goes to the mount's source, so that node's access
        // rides along too. "Share..." also snapshots whether the node's store
        // reached this device as someone else's share (`share_item_disabled`),
        // which the access does not say: an editor of a share and an owner
        // both read as `Full`.
        let appearance = self.row_snapshot(child_store, child_id);
        let access = format!("{:?}", self.node_access_in(child_store, child_id));
        let held_as_share = self
            .store_data
            .with(|map| map.get(&child_store).map_or(false, |sig| sig.with(|s| s.shared_by.is_some())));
        let source_access = self
            .mount_data
            .with(|map| map.get(&(child_store, child_id)).and_then(|sig| sig.with(|m| m.mount_ref.as_ref().map(|r| (r.source_store, r.source_node)))))
            .map(|(source_store, source_node)| format!("{:?}", self.node_access_in(source_store, source_node)))
            .unwrap_or_default();
        let tree_node = TreeNodeData::new(
            format!("node_{}_{}{}", child_store, child_id, suffix),
            format!(
                "{}|{}{}|{}|{}",
                if paste { "paste" } else { "" },
                access,
                source_access,
                if held_as_share { "theirs" } else { "" },
                appearance
            ),
        );

        // Crossing a mount node adds it to the path for everything below it.
        let children_data = if is_mount {
            let mut next_path = mount_path.to_vec();
            next_path.push((child_store, child_id));
            self.build_children_structural(child_store, child_id, paste, &next_path)
        } else {
            self.build_children_structural(child_store, child_id, paste, mount_path)
        };
        let has_children = !children_data.is_empty();
        let has_loaded_children = self.children_of.with(|map| {
            map.contains_key(&(child_store, child_id))
        });

        // Check if the node itself reports having children (from its
        // children list) even if we haven't fetched them yet. This
        // lets the tree show an expand chevron for unfetched subtrees.
        let node_reports_children = !has_loaded_children && self.node_data.with(|map| {
            map.get(&(child_store, child_id))
                .map_or(false, |sig| sig.with(|n| !n.children.is_empty()))
        });

        if has_children {
            tree_node.with_children(children_data)
        } else if is_mount && !has_loaded_children {
            let placeholder = TreeNodeData::new(
                format!("mount_loading_{}_{}{}", child_store, child_id, suffix),
                "Loading...",
            );
            tree_node.with_children(vec![placeholder])
        } else if node_reports_children {
            // Node has children we haven't fetched yet — show a
            // placeholder so the tree renders an expand chevron.
            let placeholder = TreeNodeData::new(
                format!("placeholder_{}_{}{}", child_store, child_id, suffix),
                "",
            );
            tree_node.with_children(vec![placeholder])
        } else {
            tree_node
        }
    }

    /// Compute the display label for a node (untracked, for use in callbacks).
    pub fn display_label(&self, store_id: StoreId, node_id: NodeId) -> String {
        untracked(|| {
            self.node_data.with(|map| {
                map.get(&(store_id, node_id))
                    .map(|sig| sig.with(|node| display_label_from_node(node)))
                    .unwrap_or_else(|| "Untitled".to_string())
            })
        })
    }

    /// Get the store_id and node_id of the selected node (if any)
    pub fn selected_store_and_node(&self) -> Option<(StoreId, NodeId)> {
        let selected_id = self.selected_id.get()?;
        let (store_id, node_id) = parse_tree_value(&selected_id)?;
        let node_id = node_id?;
        Some((store_id, node_id))
    }

    /// Send a command to the backend if connected.
    pub fn send(&self, cmd: crate::protocol::BackendCommand) {
        self.backend.with(|b| {
            if let Some(backend) = b {
                backend.send(cmd);
            }
        });
    }
}

/// Build the mount-path suffix for a tree value (decision 7): one
/// `/{mount_store}_{mount_node}` segment per mount level crossed, innermost
/// last. Empty for a node reached directly (no mounts crossed).
fn mount_path_suffix(mount_path: &[(StoreId, NodeId)]) -> String {
    let mut s = String::new();
    for (mount_store, mount_node) in mount_path {
        s.push('/');
        s.push_str(&mount_store.to_string());
        s.push('_');
        s.push_str(&mount_node.to_string());
    }
    s
}

/// Parse a tree value ID like "store_{uuid}" or
/// "node_{store_uuid}_{node_uuid}[/{mount_store}_{mount_node}...]" — always
/// returning the CANONICAL pair (the first 73 characters after "node_"),
/// ignoring any mount-path suffix (decision 7). Every existing consumer wants
/// the canonical node identity regardless of which place in the tree it was
/// reached through.
pub fn parse_tree_value(value: &str) -> Option<(StoreId, Option<NodeId>)> {
    if let Some(rest) = value.strip_prefix("store_") {
        let uuid: uuid::Uuid = rest.parse().ok()?;
        Some((StoreId(uuid), None))
    } else if let Some(rest) = value.strip_prefix("node_") {
        // Format: node_{store_uuid}_{node_uuid}[/...path suffix]
        // UUIDs are 36 chars each; 36 + 1 ('_') + 36 = 73.
        if rest.len() >= 73 {
            let store_str = &rest[..36];
            let node_str = &rest[37..73];
            let store_uuid: uuid::Uuid = store_str.parse().ok()?;
            let node_uuid: uuid::Uuid = node_str.parse().ok()?;
            Some((StoreId(store_uuid), Some(NodeId(node_uuid))))
        } else {
            None
        }
    } else {
        None
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum ConnectionState {
    #[default]
    Disconnected,
    Connected,
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_core::{Node, Store, StoreAccess, StoreId};

    /// A store with one child under its root, and the child's canonical pair.
    fn store_with_a_child() -> (AppStore, StoreId, NodeId) {
        let app = AppStore::new();
        let mut store = Store::new_local("Notes", "/tmp/notes.pimble".into());
        let root = Node::folder("Notes");
        store.root_node_id = root.id;
        let child = Node::document("Recipes");
        let (store_id, child_id) = (store.id, child.id);
        app.upsert_store(store);
        app.upsert_node(store_id, root.clone());
        app.upsert_node(store_id, child);
        app.set_children(store_id, root.id, vec![(store_id, child_id)]);
        (app, store_id, child_id)
    }

    /// rinch re-renders a tree row only when its `TreeNodeData` changes, and a
    /// row's menu snapshots the store's access and the node's share marker at
    /// render time — so both have to be in the data
    /// (docs/NODE_DOCUMENT_CONTRACT.md section 5, CLAUDE.md "Hardening").
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

    #[test]
    fn row_data_carries_access_and_the_share_marker() {
        let (app, store_id, child_id) = store_with_a_child();
        let before = untracked(|| app.build_tree_data_structural());

        // A store shared read-only: every row that could write the tree has
        // to re-render with its items disabled.
        if let Some(sig) = app.get_store_signal(store_id) {
            sig.update(|s| {
                s.access = StoreAccess::Read;
                s.shared_by = Some("ann@example.com".to_string());
            });
        }
        let after_access = untracked(|| app.build_tree_data_structural());
        assert_ne!(before[0].label, after_access[0].label, "the store row never re-renders");
        assert_ne!(
            before[0].children[0].label, after_access[0].children[0].label,
            "the node row never re-renders"
        );

        // A marker appearing is the badge appearing.
        if let Some(sig) = app.get_node_signal(store_id, child_id) {
            sig.update(|n| {
                n.metadata.set_share(Some(&pimble_core::ShareMarker {
                    v: pimble_core::ShareMarker::VERSION,
                    key_id: uuid::Uuid::new_v4(),
                    url: "https://pimble.app".to_string(),
                    name: "Recipes".to_string(),
                }));
            });
        }
        let after_share = untracked(|| app.build_tree_data_structural());
        assert_ne!(
            after_access[0].children[0].label, after_share[0].children[0].label,
            "a shared node's row never re-renders"
        );
        assert!(app.is_shared(store_id, child_id));
    }

    /// The store row's menu snapshots which end of the relay this device is
    /// ("Stop Sharing from This Computer...", and what a store shared from
    /// here turns off), so sharing from this computer and stopping both have
    /// to change the row's data, and `set_store_relay` says when they did
    /// (docs/RELAY_CONTRACT.md; CLAUDE.md "Hardening", the row data rule).
    #[test]
    fn store_row_data_carries_the_relay_side() {
        let (app, store_id, _) = store_with_a_child();
        assert_eq!(app.store_relay(store_id), pimble_core::RelaySide::None);
        let before = untracked(|| app.build_tree_data_structural());

        assert!(app.set_store_relay(store_id, pimble_core::RelaySide::Owner));
        assert!(!app.set_store_relay(store_id, pimble_core::RelaySide::Owner), "an answer that changes nothing rebuilt the tree");
        assert_eq!(app.store_relay(store_id), pimble_core::RelaySide::Owner);
        let relayed = untracked(|| app.build_tree_data_structural());
        assert_ne!(before[0].label, relayed[0].label, "the store row never re-renders");

        assert!(app.set_store_relay(store_id, pimble_core::RelaySide::None));
        let stopped = untracked(|| app.build_tree_data_structural());
        assert_eq!(before[0].label, stopped[0].label);
    }

    /// What a store row's menu offers a store shared from this computer, one
    /// that is not, and a member's replica of one (docs/RELAY_CONTRACT.md,
    /// "The apps").
    #[test]
    fn menu_states_for_a_store_shared_from_this_computer() {
        use pimble_core::RelaySide::{Member, None as NotRelayed, Owner};

        // "Stop Sharing from This Computer...": only where it is shared from.
        assert!(stop_relaying_item_disabled(NotRelayed));
        assert!(stop_relaying_item_disabled(Member));
        assert!(!stop_relaying_item_disabled(Owner));

        // "Host on Pimble Cloud...": a store with no link that is nobody's
        // copy. A store shared from here has a link (to its own twin), so it
        // is off, and the menu says why.
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
        assert_eq!(
            menu_note_text(&["", "", relayed_store_note(Owner)]),
            "This store is shared from this computer. To host it or link it elsewhere, stop sharing it from this computer first."
        );

        // "Unlink from Remote": any linked store but one shared from here,
        // whose link "Stop Sharing from This Computer..." ends.
        assert!(unlink_item_disabled(false, NotRelayed));
        assert!(!unlink_item_disabled(true, NotRelayed));
        assert!(!unlink_item_disabled(true, Member));
        assert!(unlink_item_disabled(true, Owner));
    }

    /// The Share dialog's four faces (docs/RELAY_CONTRACT.md, "The apps"): a
    /// node that is shared shows its share, to whoever manages it or not; one
    /// that is not shows the name form only where a share has somewhere to
    /// live, and the two ways everywhere else.
    #[test]
    fn the_share_dialog_has_four_faces() {
        use pimble_core::StoreKind::{Plain, Vault};

        assert_eq!(share_face(false, true, Plain), ShareFace::Ways);
        assert_eq!(share_face(false, true, Vault), ShareFace::Name);
        assert_eq!(share_face(true, true, Vault), ShareFace::Owner);
        assert_eq!(share_face(true, false, Vault), ShareFace::Member);
        // A marker left on a node of a store that is no longer hosted still
        // opens on the share it names, never on a form over it.
        assert_eq!(share_face(true, true, Plain), ShareFace::Owner);
        assert_eq!(share_face(true, false, Plain), ShareFace::Member);

        // The two ways and their sentences, as the contract has them.
        assert_eq!(SHARE_FROM_HERE_LABEL, "Share from this computer");
        assert_eq!(
            SHARE_FROM_HERE_SENTENCE,
            "Nothing is uploaded. People you invite reach it while this computer is on and online."
        );
        assert_eq!(HOST_ON_CLOUD_LABEL, "Host on Pimble Cloud...");
        assert_eq!(HOST_ON_CLOUD_SENTENCE, "An encrypted copy is kept there, so it works while this computer is off.");

        // Both ways are open to an ordinary local store and to nothing else.
        assert_eq!(share_ways_note(false, false, false), "");
        assert!(!share_ways_note(true, false, false).is_empty());
        assert!(!share_ways_note(false, true, false).is_empty());
        assert!(!share_ways_note(false, false, true).is_empty());
    }

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

    /// A row's menu snapshots what may be done with its node at render time,
    /// so the node's own access has to be in the row's data: when the server
    /// answers `read` for one node, that row re-renders with its items
    /// disabled and its neighbour's does not change at all.
    #[test]
    fn row_data_carries_the_nodes_own_access() {
        let (app, store_id, child_id) = store_with_a_child();
        let sibling = Node::document("Sibling");
        let sibling_id = sibling.id;
        let root = app.root_node_id(store_id).unwrap();
        app.upsert_node(store_id, sibling);
        app.set_children(store_id, root, vec![(store_id, child_id), (store_id, sibling_id)]);
        let before = untracked(|| app.build_tree_data_structural());

        if let Some(sig) = app.get_node_signal(store_id, child_id) {
            sig.update(|n| n.access = StoreAccess::Read);
        }
        let after = untracked(|| app.build_tree_data_structural());
        assert_ne!(before[0].children[0].label, after[0].children[0].label, "the read-only node's row never re-renders");
        assert_eq!(before[0].children[1].label, after[0].children[1].label, "its sibling has nothing to re-render for");
        assert_eq!(before[0].label, after[0].label, "nor has the store row: the store's access did not change");

        // And back, when the owner makes the account an editor again.
        if let Some(sig) = app.get_node_signal(store_id, child_id) {
            sig.update(|n| n.access = StoreAccess::Full);
        }
        let again = untracked(|| app.build_tree_data_structural());
        assert_eq!(before[0].children[0].label, again[0].children[0].label);
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

    fn member(email: &str, role: pimble_rpc::MemberRole) -> pimble_rpc::ShareMember {
        pimble_rpc::ShareMember { email: email.to_string(), role, status: pimble_rpc::ShareMemberStatus::Active }
    }

    fn marker(name: &str) -> pimble_core::ShareMarker {
        pimble_core::ShareMarker {
            v: pimble_core::ShareMarker::VERSION,
            key_id: uuid::Uuid::new_v4(),
            url: "https://pimble.app".to_string(),
            name: name.to_string(),
        }
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

    /// "Share..." in someone else's store opens only on a shared root (the
    /// member's read-only face), and every other node says why it is off. In
    /// one's own store it is what it always was.
    #[test]
    fn share_is_offered_only_where_it_can_do_something() {
        // One's own store: any node one may write, shared or not.
        assert!(!share_item_disabled(StoreAccess::Full, false, false));
        assert!(!share_item_disabled(StoreAccess::Full, false, true));
        assert!(share_item_disabled(StoreAccess::Read, false, false));
        assert_eq!(share_item_note(false, false), "");
        assert_eq!(share_item_note(false, true), "");

        // Someone else's: a shared root opens, for a reader as for an editor,
        // since that face changes nothing; any other node is off with the note.
        assert!(!share_item_disabled(StoreAccess::Full, true, true));
        assert!(!share_item_disabled(StoreAccess::Read, true, true));
        assert!(share_item_disabled(StoreAccess::Full, true, false));
        assert!(share_item_disabled(StoreAccess::Read, true, false));
        assert_eq!(share_item_note(true, true), "");
        assert_eq!(share_item_note(true, false), "Only an owner of this store can share from it.");
        assert_eq!(share_item_note(true, false), ONLY_AN_OWNER_SHARES_NOTE);

        // The same judgement from the store and the node the app holds.
        let (app, store_id, child_id) = store_with_a_child();
        let item = |app: &AppStore| {
            let (theirs, shared) = (app.shared_by(store_id).is_some(), app.is_shared(store_id, child_id));
            (share_item_disabled(app.node_access(store_id, child_id), theirs, shared), share_item_note(theirs, shared))
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

    /// A node row's "Share..." snapshots whether its store is someone else's,
    /// and an editor of a share reads as `Full` like an owner does, so the
    /// access in the row's data does not say it: `shared_by` has to be there
    /// itself, or the item never goes off (rinch re-renders a row only when
    /// its `TreeNodeData` changes).
    #[test]
    fn row_data_carries_whose_store_it_is() {
        let (app, store_id, _) = store_with_a_child();
        let before = untracked(|| app.build_tree_data_structural());
        if let Some(sig) = app.get_store_signal(store_id) {
            sig.update(|s| s.shared_by = Some("ann@example.com".to_string()));
        }
        assert_eq!(app.store_access(store_id), StoreAccess::Full, "an editor's share");
        let after = untracked(|| app.build_tree_data_structural());
        assert_ne!(before[0].label, after[0].label, "the store row never re-renders");
        assert_ne!(before[0].children[0].label, after[0].children[0].label, "the node row never re-renders");
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

    /// A partial replica of someone else's store shows each shared root as a
    /// row of its own under the store row, addressed like any other node, and
    /// with the root's own children below it — the store row stands for the
    /// owner's store, not for the shared folder
    /// (docs/NODE_DOCUMENT_CONTRACT.md section 5, "The recipient's replica").
    #[test]
    fn a_partial_replica_shows_every_shared_root() {
        let app = AppStore::new();
        let mut store = Store::new_local("Ann's notes", "/tmp/anns.pimble".into());
        let recipes = Node::folder("Recipes");
        let trips = Node::folder("Trips");
        let pasta = Node::document("Pasta");
        store.root_node_id = recipes.id;
        store.roots = vec![recipes.id, trips.id];
        store.shared_by = Some("ann@example.com".to_string());
        let store_id = store.id;
        app.upsert_store(store);
        app.upsert_node(store_id, recipes.clone());
        app.upsert_node(store_id, trips.clone());
        app.upsert_node(store_id, pasta.clone());
        app.set_children(store_id, recipes.id, vec![(store_id, pasta.id)]);

        let data = untracked(|| app.build_tree_data_structural());
        let store_row = &data[0];
        assert_eq!(store_row.value, format!("store_{store_id}"));
        assert_eq!(store_row.children.len(), 2, "both shared roots are rows under the store row");
        assert_eq!(store_row.children[0].value, format!("node_{store_id}_{}", recipes.id));
        assert_eq!(store_row.children[1].value, format!("node_{store_id}_{}", trips.id));
        // The root's own children hang off its row, not off the store row.
        assert_eq!(store_row.children[0].children[0].value, format!("node_{store_id}_{}", pasta.id));
        assert!(store_row.children[1].children.is_empty());
        assert_eq!(app.shown_roots(store_id), vec![recipes.id, trips.id]);
        assert!(app.is_partial_replica(store_id));
        // The store row stands for someone else's store, whose root this
        // device does not hold: it takes no node's icon, colour or badge.
        assert_eq!(app.store_row_node(store_id), None);

        // A second share of the same store extends the replica: the row's data
        // has to change, or rinch re-renders nothing and the new root never
        // appears.
        let before = store_row.label.clone();
        let walks = Node::folder("Walks");
        app.upsert_node(store_id, walks.clone());
        if let Some(sig) = app.get_store_signal(store_id) {
            sig.update(|s| s.roots.push(walks.id));
        }
        let after = untracked(|| app.build_tree_data_structural());
        assert_ne!(before, after[0].label, "the store row never re-renders with the new root");
        assert_eq!(after[0].children.len(), 3);
    }

    /// A whole store keeps the behaviour it always had: the store row stands
    /// for the root, and the root's children hang straight off it.
    #[test]
    fn a_whole_store_still_shows_its_roots_children() {
        let (app, store_id, child_id) = store_with_a_child();
        let data = untracked(|| app.build_tree_data_structural());
        assert_eq!(data[0].children.len(), 1);
        assert_eq!(data[0].children[0].value, format!("node_{store_id}_{child_id}"));
        assert!(!app.is_partial_replica(store_id));
        assert_eq!(app.shown_roots(store_id), vec![app.root_node_id(store_id).unwrap()]);
        assert_eq!(app.store_row_node(store_id), app.root_node_id(store_id));
    }
}
