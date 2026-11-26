//! Application state management
//!
//! Uses per-entity reactive signals so that data changes (rename, mount state)
//! only trigger effects on the affected node, while structural changes (children
//! loaded, node moved, store opened/closed) bump `tree_structure_version` to
//! trigger a full tree rebuild.
//!
//! `AppStore` itself (its signals) is here, and what they start as is in
//! `starting`; its methods and the values around it are kept by what they
//! are about:
//! `stores` (open and opening stores, indexes, sync links), `nodes` (node
//! signals, labels, children loading), `mounts`, `access` (what this device
//! may do, a share's roots and its modal), `pane_state` (the four panes),
//! `tree_rows` (the explorer's rows), `found` (rows a query metanode found),
//! `pending` (what the explorer waits for), `share_words` and `deleted` (what
//! the sharing UI and "Recently Deleted..." say) and `values` (the small
//! values the signals carry).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use pimble_core::{MountRef, MountState, Node, NodeId, RemoteEndpoint, Store, StoreId, SyncState};
use rinch::components::TreeNodeData;
use rinch::prelude::*;

use pimble_core::plan::{share_face, share_ways_note};
pub use pimble_core::plan::{ShareFace, ONLY_AN_OWNER_SHARES_NOTE};

use crate::panes::{PaneId, Tiling, MAX_PANES};
use crate::protocol::BackendHandle;

mod access;
mod deleted;
mod found;
mod internal;
mod mounts;
mod nodes;
mod pane_state;
mod pending;
mod share_words;
mod starting;
mod stores;
mod tree_rows;
mod values;
mod viewers;
#[cfg(test)]
mod test_fixtures;

pub use access::RootsChange;
pub use deleted::{deleted_row, deleted_row_meta, deleted_row_read_only, DeletedRow, readable_instant};
pub use found::{first_found, Found, found_row, FoundRow};
pub use internal::hidden_count_text;
pub use mounts::{mount_is_dimmed, mount_label_suffix, mount_state_kind, MountInfo, MountStateKind, PendingMount};
pub use nodes::SLOW_CHILDREN_MS;
pub use pane_state::{ActiveEdit, PaneState};
pub use pending::{
    open_when_created, set_last_drop_target_value, set_pending_create, take_last_drop_target_value,
    take_open_when_created, take_pending_create, take_pending_rename,
};
pub use share_words::{
    access_note, access_words, ENDED_REPLICA_NOTE, HOST_ON_CLOUD_LABEL, HOST_ON_CLOUD_SENTENCE,
    manages_share, member_label, menu_note_text, NO_LONGER_SHARED_WORDS, RELAYED_PLACEHOLDER_NAME,
    share_ended_sentence, SHARE_FROM_HERE_LABEL, SHARE_FROM_HERE_SENTENCE, share_keeping_note,
    share_state_text, shared_by_sentence, shared_by_words,
};
pub use stores::{OPENING_ROW, OpeningStore};
pub use tree_rows::{display_label_from_node, get_node_content_text, parse_tree_value};
pub use values::{
    CloudAnswer, ConnectionState, LinkOpen, LinkTooltip, PluginRecipe, SearchState, ServerVersion,
    SIDEBAR_DEFAULT_WIDTH, SIDEBAR_MAX_WIDTH, SIDEBAR_MIN_WIDTH, UpdateStatus,
};

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
    /// What each query metanode the explorer has opened finds, shown under
    /// its own children (docs/QUERY_METANODES_CONTRACT.md section 4); kept
    /// live by `crate::metanodes`.
    pub finds: Signal<HashMap<(StoreId, NodeId), Vec<Found>>>,

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

    // The split view (docs/SPLIT_VIEW_CONTRACT.md): how the area beside the
    // explorer is tiled, which pane has the focus, and what each of the four
    // pane slots holds. `selected_id` above is the tree's selected row, which
    // is the focused pane's document: `select_in_pane` and `focus_pane` are
    // the two places that keep it so.
    pub tiling: Signal<Tiling>,
    pub focused_pane: Signal<PaneId>,
    pub panes: [PaneState; MAX_PANES],
    /// The pane a link being resolved was followed from, until it opens or
    /// is refused.
    pub link_from: Signal<Option<PaneId>>,
    /// True while a divider between panes is being dragged: the layout is
    /// saved when the drag ends, not on every move.
    pub pane_dragging: Signal<bool>,

    // Inline rename
    pub renaming_node: Signal<Option<String>>,
    pub rename_text: Signal<String>,

    /// What the strip at the foot of the explorer says during a drag in the
    /// tree: where the dragged row lands, or why it cannot
    /// (docs/TREE_CONTRACT.md "Feedback").
    pub drop_hint: Signal<Option<crate::tree_drop::DropHint>>,


    // Mount picker
    pub pending_mount: Signal<Option<PendingMount>>,

    // "Copy as Mount Source" / "Paste Mount Here": the canonical pair and
    // display title of the node last copied as a mount source, if any.
    pub mount_source: Signal<Option<(StoreId, NodeId, String)>>,

    // "Paste" with more than one thing it could do: the row it was chosen on
    // and the choices, while the chooser is open (`crate::pasting`).
    pub paste_offer: Signal<Option<crate::pasting::Offer>>,
    // The explorer's "+" list is open (`crate::creating`; File > New > More...
    // opens it too).
    pub new_menu_open: Signal<bool>,

    // Editor dirty flag — set when user edits content, cleared on save/load
    pub editor_dirty: Signal<bool>,

    // Locally-computed display label for the node currently being typed into,
    // refreshed on a debounce by `editor::schedule_label_refresh` straight from
    // the editor's in-memory document. Takes priority over the content-derived
    // label in the tree render, since the per-node signal's cached `content`
    // isn't updated while a node is under active local edit (see
    // `upsert_node`, which clears an entry once authoritative content arrives).
    //
    // One signal per node, reached through `live_label_signal`, and the map
    // itself is only ever read untracked: a tree row subscribes to its own
    // node's entry and nothing else. (Until 2026-09-22 this was one signal
    // holding the map, so every refresh, and every `upsert_node`, re-ran the
    // label of every visible row.)
    live_label: Signal<HashMap<(StoreId, NodeId), Signal<Option<String>>>>,

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
    // colors are being picked; `None` means closed. `appearance_icon_query`
    // filters the icon grid over the whole Tabler set. Tags are the
    // Properties panel's (docs/PROPERTIES_CONTRACT.md).
    pub appearance_modal_node: Signal<Option<(StoreId, NodeId)>>,
    /// The node the Theme picker is open for (Node > Theme..., a row's
    /// "Theme...", the Properties panel, docs/THEMES_CONTRACT.md section 9);
    /// `None` when closed.
    pub theme_picker_node: Signal<Option<(StoreId, NodeId)>>,
    /// Every open store's theme nodes as last listed
    /// (`BackendEvent::ThemesListed`): what a store's chains resolve through,
    /// and the picker's "This store's themes" and "From other open stores".
    pub store_themes: Signal<HashMap<StoreId, Vec<pimble_api::StoreTheme>>>,
    /// Whether the explorer shows each internal kind, by
    /// `InternalKind::index` (View > "Show Internal", each a check item),
    /// remembered on this device (`views.rs`).
    pub internal_shown: [Signal<bool>; pimble_core::internal::InternalKind::ALL.len()],
    pub appearance_icon_query: Signal<String>,

    /// The Properties panel (docs/PROPERTIES_CONTRACT.md): whether it is
    /// open (View > "Properties", `Alt+Enter`) and its width, both remembered
    /// on this device (`views.rs`); a new field's row is showing (Edit >
    /// "Add Field"); and what the backend last said of the node it shows.
    pub properties_open: Signal<bool>,
    pub properties_width: Signal<f32>,
    pub properties_adding_field: Signal<bool>,
    pub properties_context: Signal<Option<crate::properties::Context>>,
    /// What points at the node the panel shows, as last answered
    /// (`BackendEvent::ReferencesLoaded`).
    pub properties_references: Signal<Option<(StoreId, NodeId, crate::properties::References)>>,

    /// The theme in use (View > "Toggle Dark Mode"), persisted in `state.json`.
    /// Read reactively wherever a colour depends on the scheme.
    pub dark_mode: Signal<bool>,

    /// The board tools (docs/IMAGE_MARKUP_CONTRACT.md section 4), one set for
    /// the one toolbar: the tool, and the style new elements take (and the
    /// style panel shows), with this viewer's grid and snapping. They act on
    /// whichever board pane has the focus.
    pub board_tool: Signal<crate::boards::Tool>,
    pub board_style: crate::boards::StyleUi,

    /// The explorer's width in pixels: dragged at its right edge, persisted
    /// with the theme.
    pub sidebar_width: Signal<f32>,

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

    // File > Import (docs/IMPORT_CONTRACT.md). The picked files wait in
    // `crate::import` while the modal asks where they go.
    pub import_modal_open: Signal<bool>,
    /// What was picked, as the modal names it ("Novel.scriv, a Scrivener project").
    pub import_modal_label: Signal<String>,
    /// The node an import would land under, and its title for the button;
    /// `None` when nothing writable is selected.
    pub import_modal_under: Signal<Option<(StoreId, NodeId, String)>>,
    /// The name a new store made for the import gets.
    pub import_modal_store_name: Signal<String>,
    pub import_modal_error: Signal<String>,

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
    /// How this computer's connections to Pimble Cloud are doing, while signed
    /// in (`BackendEvent::CloudConnectionChanged`); `None` until the server
    /// says, and from a server too old to.
    pub cloud_connection: Signal<Option<pimble_rpc::CloudConnection>>,
    /// Whether the server has answered what account it holds since this
    /// connection was made (docs/STARTUP_CONTRACT.md "Said"): until it has,
    /// the status bar says "connecting..." with a spinner, never
    /// "disconnected" or "sign in".
    pub cloud_answer: Signal<CloudAnswer>,
    /// The saved stores asked to open and not open yet, each a row of its
    /// own in the explorer from the start (docs/STARTUP_CONTRACT.md "Each
    /// saved store has its row at once").
    pub opening_stores: Signal<Vec<OpeningStore>>,
    /// The branches whose children were asked for and have taken longer
    /// than `SLOW_CHILDREN_MS`: their rows show a spinner where the chevron
    /// goes (docs/STARTUP_CONTRACT.md "Children loading").
    pub slow_children: Signal<HashSet<(StoreId, NodeId)>>,
    /// The branches whose children were asked for and have not arrived: a
    /// row that appears expanded is not asked for again while its store's
    /// first load is on its way, and the start is "ready" when this empties
    /// (`startup::check`).
    pub children_pending: Signal<HashSet<(StoreId, NodeId)>>,

    // "Account..." modal (Account menu, the status bar, and "Host on Pimble
    // Cloud..." with nothing signed in). One modal, two faces: the sign-in
    // form while nothing is signed in, the signed-in view otherwise
    // (decision 1).
    /// The Settings window's chosen section, `None` while it is closed
    /// (docs/SETTINGS_CONTRACT.md section 2). The Account page is the
    /// Account section.
    pub settings: Signal<Option<crate::settings::Section>>,
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

    /// The documents each store's link is holding back, with the sentence
    /// that says why (`StoreSyncChanged::waiting`,
    /// docs/SYNC_LIMITS_CONTRACT.md): the row's badge adds "· N notes
    /// waiting", and a waiting node's Properties panel shows its reason.
    pub waiting: Signal<HashMap<StoreId, Vec<pimble_core::WaitingDoc>>>,
    /// The notes out of step on this device, per store
    /// (docs/CONVERGENCE_CONTRACT.md section 4): shown on each note's row,
    /// its pane and its Properties, never on the store row.
    pub out_of_step: Signal<HashMap<StoreId, Vec<pimble_core::OutOfStep>>>,

    /// A sentence the server sent back refusing a command (`Forbidden`, e.g.
    /// a store shared read-only), shown in the status bar. Not an error
    /// state: the connection is fine, the command was simply not allowed
    /// (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles").
    pub notice: Signal<String>,
    /// A verified update staged beside a managed install, waiting for "Restart
    /// to update" (docs/UPDATE_CONTRACT.md); `None` everywhere else.
    pub update_ready: Signal<Option<String>>,
    /// Help > About (`about.rs`): whether it is open, where updating stands,
    /// and the version of the server this window talks to.
    pub about_open: Signal<bool>,
    /// Settings > Diagnostics opened as the first start's notice
    /// (`diagnostics_ui::notice_once`): its heading says so.
    pub diagnostics_notice: Signal<bool>,
    /// Help > "Send Diagnostics..." (`diagnostics_ui`): open, the note being
    /// written, sending, and what came of it (the report's code, or why not).
    pub diagnostics_send_open: Signal<bool>,
    pub diagnostics_note: Signal<String>,
    pub diagnostics_sending: Signal<bool>,
    pub diagnostics_sent: Signal<Option<Result<String, String>>>,
    pub update_status: Signal<UpdateStatus>,
    pub server_version: Signal<Option<ServerVersion>>,

    /// A followed link's target, for `app.rs` to open with `open_node` (the
    /// one way to open a node): its tree value and, for a deep link, the
    /// anchor the editor puts the caret at once the session is up
    /// (docs/LINKS_CONTRACT.md "Following a link").
    pub link_open: Signal<Option<LinkOpen>>,
    /// A step back (`true`) or forward (`false`) asked of the focused pane's
    /// history ([`crate::nav`]), from the menu or its shortcut; the explorer,
    /// which opens things, takes it.
    pub nav_step: Signal<Option<bool>>,
    /// Account > "AI Agents..." ([`crate::agents`]).
    pub agents_ui: crate::agents::AgentsUi,
    /// Account > "Shares...", who is who in each store's shares, and the
    /// first-open question (docs/PRESENCE_CONTRACT.md section 3).
    pub shares_ui: crate::shares::SharesUi,
    /// Who else is in which document, as their presence frames say
    /// (docs/PRESENCE_CONTRACT.md; `crate::presence`). Never stored.
    pub presence: Signal<HashMap<(StoreId, NodeId), Vec<crate::presence::Present>>>,
    /// The recipes installed plugins register ("New Kanban"), offered in the
    /// explorer's new-node menu (`crate::plugins`).
    pub recipes: Signal<Vec<PluginRecipe>>,
    /// The child viewers installed plugins provide (a kanban's column), each
    /// for the children of a viewer's metanodes (`viewers.rs`).
    pub child_viewers: Signal<Vec<pimble_plugin_abi::ChildViewerDecl>>,
    /// The tooltip over a hovered link, once the pointer has rested on it
    /// (docs/LINKS_CONTRACT.md "What a link looks like").
    pub link_hover: Signal<Option<LinkTooltip>>,
    /// The link picker while it is open (`[[` or Ctrl+L,
    /// docs/LINKS_CONTRACT.md "Making a link"); `link_picker.rs` drives it.
    pub link_picker: Signal<Option<crate::link_picker::PickerView>>,

    // "Recently Deleted..." modal (View menu, store row context menu;
    // docs/MOVE_CONTRACT.md "Seeing and undoing what was removed"). One
    // store at a time: `Some(store_id)` is the store it is open for.
    pub deleted_modal_store: Signal<Option<StoreId>>,
    /// Filled by `DeletedListed`; cleared whenever the modal opens on a store.
    pub deleted_modal_nodes: Signal<Vec<DeletedRow>>,
    /// True while a `ListDeleted`, `UndeleteNode` or "Put Back" `MoveNode`
    /// this modal sent is still in flight. An `Error` only reaches this
    /// modal's own error line while this is set — otherwise it is some
    /// other command's refusal or failure and takes the path it always did
    /// (the notice or the status bar), never something the modal claims for
    /// its own "Put Back" that never asked.
    pub deleted_modal_pending: Signal<bool>,
    /// A `BackendEvent::Error` while this modal is open lands here rather
    /// than the connection status bar.
    pub deleted_modal_error: Signal<String>,

    /// The discussions on each node whose list this window asked for
    /// (`ListAttachments`, docs/DISCUSSIONS_CONTRACT.md section 1), oldest
    /// first; each discussion's node is in the node cache.
    pub attachments: Signal<HashMap<(StoreId, NodeId), Vec<NodeId>>>,
    /// Bumped whenever the personal document changes (`crate::personal`):
    /// what shows read state reads it, to follow.
    pub read_version: Signal<u64>,
    /// The Account window's "Keep what I've read in step across my devices".
    pub personal_sync: Signal<bool>,
    /// View > "Discussions..." (docs/DISCUSSIONS_CONTRACT.md section 8): open,
    /// and the discussions the open stores hold (`ListDiscussions`).
    pub discussions_modal: Signal<bool>,
    pub all_discussions: Signal<Vec<(StoreId, NodeId)>>,
    /// The notices this device has received, newest first
    /// (docs/NOTIFICATIONS_CONTRACT.md section 5): what View > "Inbox..."
    /// lists and the status bar counts (the unread ones, by the personal
    /// document).
    pub notices: Signal<Vec<pimble_rpc::Notice>>,
    /// The names of the notes notices name that no view holds, as this
    /// device read them for the Inbox (`NotesNamed`, `crate::inbox::named`).
    pub notice_names: Signal<std::collections::HashMap<(StoreId, NodeId), crate::protocol::NoteName>>,
    /// Whether notifications are on for the account (section 6): the switch
    /// in the Account window and in Settings.
    pub notifications_on: Signal<bool>,
    /// Wave 2: the email fallback's switch, and web push's.
    pub notify_email: Signal<bool>,
    pub notify_push: Signal<bool>,
    /// Wave 3: whether changes to watched notes are told, and the watches
    /// the account chose (`crate::watches`); nothing is watched by default.
    pub notify_watches: Signal<bool>,
    pub watches: Signal<Vec<pimble_rpc::WatchEntry>>,
}

impl AppStore {
    /// Get the store_id and node_id of the selected node (if any)
    pub fn selected_store_and_node(&self) -> Option<(StoreId, NodeId)> {
        let selected_id = self.selected_id.get()?;
        let (store_id, node_id) = parse_tree_value(&selected_id)?;
        let node_id = node_id?;
        Some((store_id, node_id))
    }

    /// The node the explorer's selected row stands for: a node row's node, a
    /// store row's root node. What the menu bar's Node menu acts on, as a
    /// row's own menu does (`app/explorer_menu.rs`).
    pub fn selected_row_node(&self) -> Option<(StoreId, NodeId)> {
        let selected_id = self.selected_id.get()?;
        let (store_id, node_id) = parse_tree_value(&selected_id)?;
        let node_id = node_id.or_else(|| self.root_node_id(store_id))?;
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
