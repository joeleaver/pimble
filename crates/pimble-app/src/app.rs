//! Pimble's application UI.
//!
//! Built with the Rinch UI framework. Everything here is target-independent
//! except [`run`], which opens the desktop window; the browser entry point in
//! `web/` mounts the very same component through `rinch_web`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rinch::prelude::*;
use rinch::core::{request_focus, set_keyboard_interceptor, clear_keyboard_interceptor};
use rinch_tabler_icons::{TablerIcon, TablerIconStyle, render_tabler_icon};

use crate::rinch_editor::Editor;
use crate::protocol::{BackendCommand, CloudOp};
#[cfg(feature = "native")]
use crate::protocol::BackendHandle;
use crate::editor::{start_editing, stop_editing};
use crate::events::{EVENT_PROCESSOR, process_backend_events};
use crate::appearance::{display_color, icon_by_name, icons_matching, IconGlyph, COLOR_CHOICES};
use crate::persistence::{load_dark_mode, save_dark_mode};
use crate::state::{parse_tree_value, display_label_from_node, mount_is_dimmed, mount_label_suffix, AppStore, SearchState};
// Only "Mount Store Here...", which picks a directory on this machine, sets one.
#[cfg(feature = "native")]
use crate::state::PendingMount;
use crate::styles::{APP_CSS, EDITOR_CSS};

/// How long after a click a second one on the same row still counts as a
/// double-click, and so starts an inline rename.
const DOUBLE_CLICK_MS: f64 = 500.0;

/// Whether this build may administer the stores on its server: open one from a
/// path, add a remote as a replica, link, unlink, remove a replica, close one,
/// create one.
///
/// Those are the service principal's RPCs. The desktop app owns the server it
/// talks to and connects as that principal, so it offers them all. A browser
/// connects as a signed-in user, whose token carries per-store grants and
/// nothing else, so the server refuses every one of them; the account pages
/// create and share stores instead. Offering a menu item that can only fail is
/// worse than not offering it, so the browser build hides them.
const CAN_ADMINISTER_STORES: bool = cfg!(feature = "native");

/// Whether this build can make a store on the signed-in account.
///
/// The exact complement of [`CAN_ADMINISTER_STORES`] for creating one: a
/// desktop build makes a store as a directory through a native dialog, a
/// browser build asks the accounts service for a hosted one and mints its key.
/// Both are "New Store...", and no build offers neither.
const CAN_CREATE_HOSTED_STORES: bool = !CAN_ADMINISTER_STORES;

/// Why "Share..." is disabled on a mount row: the node itself lives in
/// another store, and that is where a share of it belongs.
const MOUNT_NOTE: &str = "A mount stands in for a node in another store. Share it there.";

/// Milliseconds on a clock that only moves forward, for the double-click
/// window above. `std::time::Instant` has no implementation on
/// `wasm32-unknown-unknown` and panics the first time it is read, so the
/// browser build reads the page's own clock instead. Only differences between
/// two readings matter here, so the two origins need not agree.
#[cfg(feature = "native")]
fn now_ms() -> f64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

#[cfg(not(feature = "native"))]
fn now_ms() -> f64 {
    js_sys::Date::now()
}

/// rinch's theme for the app: one function so the startup theme and a runtime
/// switch (View > "Toggle Dark Mode", `rinch::update_theme`) agree on everything
/// but the scheme. The editor's own stylesheet follows the same flag through
/// `EditorHandle::set_dark_mode`, and colours the app derives follow
/// `AppStore::dark_mode` reactively.
pub fn theme_props(dark_mode: bool) -> ThemeProviderProps {
    ThemeProviderProps {
        primary_color: Some("blue".into()),
        dark_mode,
        default_radius: Some("sm".into()),
        ..Default::default()
    }
}

/// Switch between the dark and light themes: the theme CSS, the editor's
/// stylesheet, every colour the app derives (they read `dark_mode`), and the
/// saved preference.
pub fn toggle_dark_mode(store: AppStore) {
    let dark = !untracked(|| store.dark_mode.get());
    store.dark_mode.set(dark);
    rinch::update_theme(&theme_props(dark));
    crate::editor::editor().set_dark_mode(dark);
    save_dark_mode(dark);
}

thread_local! {
    /// What the sidebar's account button does, when there is one.
    ///
    /// The browser build has account pages of its own (`/app/account`) and has
    /// to be able to reach them without a page load, because a reload throws
    /// away the keys that make an encrypted store readable. It registers the
    /// move here; the desktop registers nothing, and the button is not drawn.
    /// A hook rather than a `cfg` so the shared UI stays free of target
    /// spelling, and rather than a `BackendCommand` because nothing about it
    /// involves the backend.
    static ACCOUNT_ACTION: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
    /// What "Sign Out" does, for a build that has an account to sign out of.
    /// Registered by the browser entry point, like [`ACCOUNT_ACTION`].
    static SIGN_OUT_ACTION: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
    /// The search box's `NodeHandle`, captured once when the toolbar is built
    /// so the View menu's "Focus Search" (Ctrl+K) action — constructed earlier,
    /// before the box exists — can reach it later. Same reach-across-closures
    /// need as `EVENT_PROCESSOR`/`CLOSE_HANDLER` below.
    static SEARCH_INPUT: RefCell<Option<NodeHandle>> = const { RefCell::new(None) };
    /// The debounced `Search` command pending from the last keystroke, if any.
    static SEARCH_DEBOUNCE: RefCell<Option<TimeoutHandle>> = const { RefCell::new(None) };
}

/// Give the sidebar an account button and say what it does. Called before
/// [`build_view`]; calling it again replaces the action.
pub fn set_account_action(action: impl Fn() + 'static) {
    ACCOUNT_ACTION.with(|slot| *slot.borrow_mut() = Some(Rc::new(action)));
}

/// Whether a build registered one. Read once, at render time: the browser
/// registers before the view is built and the desktop never does, so this never
/// changes while a view is on screen.
fn has_account_action() -> bool {
    ACCOUNT_ACTION.with(|slot| slot.borrow().is_some())
}

pub fn run_account_action() {
    // Cloned out of the slot before it runs: the action navigates, which
    // unmounts this view, and a borrow held across that would be live while
    // the thing that owns it goes away.
    let action = ACCOUNT_ACTION.with(|slot| slot.borrow().clone());
    if let Some(action) = action {
        action();
    }
}

/// Give "Sign Out" something to do. Called before [`build_view`].
pub fn set_sign_out_action(action: impl Fn() + 'static) {
    SIGN_OUT_ACTION.with(|slot| *slot.borrow_mut() = Some(Rc::new(action)));
}

pub fn run_sign_out_action() {
    let action = SIGN_OUT_ACTION.with(|slot| slot.borrow().clone());
    if let Some(action) = action {
        action();
    }
}

/// "New Store...", the desktop's way: pick a path, create, open.
#[cfg(feature = "native")]
pub fn pick_and_create_store(store: AppStore) {
    let dialog = rinch::dialogs::save_file()
        .set_title("Create New Store")
        .add_filter("Pimble Store", &["pimble"]);

    let Some(path) = dialog.save() else { return };
    let path_str = path.to_string_lossy().to_string();
    let path_str = if path_str.ends_with(".pimble") {
        path_str
    } else {
        format!("{}.pimble", path_str)
    };
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "New Store".to_string());
    tracing::info!("Creating new store at: {}", path_str);
    store.pending_create_path.set(Some(path_str.clone()));
    store.send(BackendCommand::CreateStore { path: path_str, name });
}

/// "Open Store...", the desktop's way: pick a directory and open it.
#[cfg(feature = "native")]
pub fn pick_and_open_store(store: AppStore) {
    let Some(path) = rinch::dialogs::pick_folder().set_title("Open Store").pick() else {
        return;
    };
    let path_str = path.to_string_lossy().to_string();
    tracing::info!("Opening store at: {}", path_str);
    store.send(BackendCommand::OpenStore { path: path_str });
}

/// Put the caret in the search box.
///
/// The desktop reaches this through the View menu's "Focus Search"; a browser
/// build, which has no menu bar to carry an accelerator, binds Ctrl+K to it
/// itself. The search box advertises the shortcut in its placeholder, so
/// exactly one of the two must always be in place.
pub fn focus_search() {
    SEARCH_INPUT.with(|cell| {
        if let Some(handle) = cell.borrow().as_ref() {
            handle.focus();
        }
    });
}

/// Rebuild the search index of every open store that has one.
///
/// An encrypted store has no server-side index and cannot have one — the
/// server has never seen a word of it — so it is skipped rather than asked.
pub fn rebuild_search_indexes(store: AppStore) {
    let store_ids = untracked(|| store.store_ids.get());
    for store_id in store_ids {
        if store.is_vault(store_id) {
            continue;
        }
        store.send(BackendCommand::RebuildIndex { store_id });
    }
}

/// Open the "Mount Store..." picker for a target node.
///
/// The desktop's "Mount Store..." picks a directory; a browser has none to pick
/// from, so it picks one of the stores the account already grants. Either way
/// the RPC is the same `createMount`, which an editor is allowed to call.
pub fn open_mount_picker(store: AppStore, target: (pimble_core::StoreId, pimble_core::NodeId)) {
    // The target goes in first: the picker's own option list reads it to leave
    // out the store being mounted into, and a list built before it was set
    // would offer that store as a place to put itself.
    store.mount_picker_target.set(Some(target));
    let first = untracked(|| {
        store.store_ids.with(|ids| ids.iter().find(|&&id| id != target.0).copied())
    });
    store.mount_picker_selected.set(first.map(|id| id.to_string()).unwrap_or_default());
    store.mount_picker_error.set(String::new());
    store.mount_picker_pending.set(false);
}

/// Open the "New Store..." modal, empty.
pub fn open_new_store_modal(store: AppStore) {
    store.new_store_modal_name.set(String::new());
    store.new_store_modal_error.set(String::new());
    store.new_store_modal_pending.set(false);
    store.new_store_modal_open.set(true);
}

/// Select and open a node or store root identified by a tree value
/// (`"store_{uuid}"` / `"node_{store_uuid}_{node_uuid}"`) — the single
/// implementation behind both a tree click and a search-result click, per
/// CLAUDE.md's "one way to write node content": there is one way to open one.
fn open_node(store: AppStore, tree_state: UseTreeReturn, value: String) {
    store.selected_id.set(Some(value.clone()));
    tree_state.controller.select(&value);

    // Offline-first: a document opens from the cached content at once and
    // `start_editing` then reconciles the session with the server (state
    // vector in, diff out), so a cache that is behind the server (this
    // window's own earlier session, another window, a replica) is merged
    // in place rather than reloaded. A node the cache has never seen is
    // fetched; `NodeLoaded` starts the session then.
    let parsed = parse_tree_value(&value);
    let (title_opt, content_bytes_opt, is_document, sel_ids) = if let Some((s_id, node_id_opt)) = parsed {
        if let Some(n_id) = node_id_opt {
            let result = store.get_node_signal(s_id, n_id).map(|sig| {
                sig.with(|node| {
                    let label = if !node.metadata.title.is_empty() {
                        node.metadata.title.clone()
                    } else {
                        "Untitled".to_string()
                    };
                    let is_doc = node.node_type == pimble_core::node_types::DOCUMENT;
                    (label, node.content.clone(), is_doc)
                })
            });
            match result {
                Some((label, content, is_doc)) => (Some(label), Some(content), is_doc, Some((s_id, n_id))),
                None => {
                    store.send(BackendCommand::GetNode { store_id: s_id, node_id: n_id });
                    (None, None, false, None)
                }
            }
        } else {
            let title = store.get_store_signal(s_id)
                .map(|sig| sig.with(|s| s.name.clone()));
            match title {
                Some(t) => (Some(t), Some(Vec::new()), false, None),
                None => (None, None, false, None),
            }
        }
    } else {
        (None, None, false, None)
    };

    store.show_editor.set(is_document);

    if let Some(title) = title_opt {
        store.node_title.set(title);
    }

    if is_document {
        let content_bytes = content_bytes_opt.unwrap_or_default();
        if let Some((s_id, n_id)) = sel_ids {
            start_editing(store, s_id, n_id, &content_bytes);
        }
    } else {
        stop_editing(store);
    }
    store.editor_dirty.set(false);
}

/// "Copy as Mount Source": record the canonical pair and display title of the
/// node or store root identified by `value` into `store.mount_source`, for a
/// subsequent "Paste Mount Here" elsewhere in the tree.
fn copy_as_mount_source(store: AppStore, value: &str) {
    let Some((s_id, node_id_opt)) = parse_tree_value(value) else { return };
    match node_id_opt {
        Some(n_id) => {
            let title = store.display_label(s_id, n_id);
            store.mount_source.set(Some((s_id, n_id, title)));
        }
        None => {
            if let Some(root_id) = store.root_node_id(s_id) {
                let name = store.get_store_signal(s_id)
                    .map(|sig| untracked(|| sig.with(|s| s.name.clone())))
                    .unwrap_or_default();
                store.mount_source.set(Some((s_id, root_id, name)));
            }
        }
    }
    bump_tree_after_menu_closes(store);
}

/// Re-render every row's context menu (so "Paste Mount Here" reads its fresh
/// `disabled` value: every row's `TreeNodeData` carries that flag) **after**
/// the menu item's click has finished, never inside it.
///
/// A rinch `DropdownMenuItem` closes its menu by writing the `ContextMenu`'s
/// `opened` signal *after* the item's own callback returns. That signal is
/// owned by the row's render scope, and bumping the tree disposes every row —
/// so a bump inside the callback frees the signal the close is about to write,
/// the write is dropped ("Signal::set() on a freed signal"), and the menu's
/// portal is left on screen with nothing alive to hide it. It is parented to
/// the body rather than to the row, so disposing the row does not take it
/// away either: the menu stays up for good.
///
/// `run_on_main_thread` does not help here — called from the main thread it
/// runs the closure straight through, which is exactly the case this has to
/// avoid. A zero-length timeout is a real turn of the loop on both targets:
/// the click handler returns, the item writes `opened = false`, the portal
/// hides, and only then does the tree rebuild.
fn bump_tree_after_menu_closes(store: AppStore) {
    // Nothing cancels this; the tree must be rebuilt whatever happens next.
    let _ = set_timeout(0, move || store.bump_tree_structure());
}

/// "Paste Mount Here": create a mount at the tree location identified by
/// `target_value`, sourced from whatever "Copy as Mount Source" last recorded
/// in `store.mount_source`. No-op if nothing was copied. Clears the copied
/// source on success so the menu item disappears again.
fn paste_mount_here(store: AppStore, target_value: &str) {
    let Some(source) = untracked(|| store.mount_source.get()) else { return };
    let (source_store, source_node, title) = source;
    let Some((target_store, target_node_opt)) = parse_tree_value(target_value) else { return };
    let Some(parent_id) = target_node_opt.or_else(|| store.root_node_id(target_store)) else { return };
    store.send(BackendCommand::CreateMount {
        store_id: target_store,
        parent_id,
        source_store_id: source_store,
        source_node_id: source_node,
        title: Some(title),
    });
    store.mount_source.set(None);
    // Re-render every row's context menu so "Paste Mount Here" reads as
    // disabled again (see the `no_mount_source` note in `run`).
    bump_tree_after_menu_closes(store);
}

/// Open the "Add Remote Store..." modal, resetting everything a previous run
/// left behind. `target` picks the mode (docs/history/REMOTE_MOUNTS_CONTRACT.md
/// decision 9): `None` is the plain File-menu flow that only adds a replica,
/// `Some(canonical pair)` is "Mount Remote Store Here...", whose action adds
/// the replica when this server does not already have the store and then
/// mounts its root under that pair.
pub fn open_connect_modal(store: AppStore, target: Option<(pimble_core::StoreId, pimble_core::NodeId)>) {
    store.connect_modal_target.set(target);
    store.connect_modal_stores.set(Vec::new());
    store.connect_modal_selected.set(String::new());
    store.connect_modal_error.set(String::new());
    store.connect_modal_busy.set(false);
    store.connect_modal_pending_add.set(false);
    store.connect_modal_token.set(String::new());
    store.connect_modal_token_visible.set(false);
    if untracked(|| store.connect_modal_url.get()).is_empty() {
        store.connect_modal_url.set("http://".to_string());
    }
    store.connect_modal_open.set(true);
}

/// Open the "Account..." modal (docs/DESKTOP_ACCOUNT_CONTRACT.md decision 1):
/// the sign-in form while nothing is signed in, the signed-in view otherwise.
/// `hint` is why it opened when it did not open from the menu ("Sign in to
/// host a store"), or empty. The URL field starts at `https://pimble.app` and
/// then keeps whatever was last typed in this run.
pub fn open_account_modal(store: AppStore, hint: &str) {
    store.account_modal_error.set(String::new());
    store.account_modal_busy.set(false);
    store.account_modal_password.set(String::new());
    store.account_modal_password_visible.set(false);
    store.account_modal_hint.set(hint.to_string());
    if untracked(|| store.account_modal_url.get()).is_empty() {
        store.account_modal_url.set("https://pimble.app".to_string());
    }
    store.account_modal_open.set(true);
}

/// "Host on Pimble Cloud..." for `store_id`: the confirmation when an account
/// is signed in, the Account modal with a hint otherwise (decision 5).
pub fn open_host_modal(store: AppStore, store_id: pimble_core::StoreId) {
    if !untracked(|| store.cloud_signed_in.get()) {
        open_account_modal(store, "Sign in to host a store");
        return;
    }
    store.host_modal_error.set(String::new());
    store.host_modal_busy.set(false);
    store.host_modal_store.set(Some(store_id));
}

/// Open the "Add Hosted Store..." modal and ask for the account's stores at
/// once (decision 6). Not signed in, the modal says so and offers the
/// Account modal instead of a list.
pub fn open_hosted_modal(store: AppStore) {
    store.hosted_modal_stores.set(Vec::new());
    store.hosted_modal_selected.set(String::new());
    store.hosted_modal_error.set(String::new());
    let signed_in = untracked(|| store.cloud_signed_in.get());
    store.hosted_modal_busy.set(signed_in);
    store.hosted_modal_open.set(true);
    if signed_in {
        store.send(BackendCommand::CloudListHostedStores);
    }
}

/// "Share..." for `(store_id, node_id)` (docs/NODE_DOCUMENT_CONTRACT.md
/// section 5): a share is a scoped grant on this store — the node and the
/// documents under it — so it needs an account, and the Account modal opens
/// with a hint when there is none, the same rule "Host on Pimble Cloud..."
/// follows.
///
/// The node's own share marker decides which face opens, so a node already
/// shared shows its share at once; `CloudShareInfo` then fills in the members
/// the accounts service has. A share carries a name of its own, seeded here
/// from the node's title because that is usually what the owner would type.
///
/// In a store that reached this device as someone else's share nothing can be
/// shared from here (`share_item_disabled` keeps the menu item off every node
/// but a shared root), and a shared root opens on the member's face: whose
/// share it is and who is on it, read only (`AppStore::share_modal_manages`).
/// That face is drawn from what this device already holds, so it needs no
/// sign-in; the member list is asked for when there is an account to ask as.
pub fn open_share_modal(store: AppStore, store_id: pimble_core::StoreId, node_id: pimble_core::NodeId) {
    let marker = store
        .get_node_signal(store_id, node_id)
        .and_then(|sig| untracked(|| sig.with(|n| n.metadata.share())));
    let held_as_share = store.shared_by(store_id).is_some();
    if held_as_share && marker.is_none() {
        // The menu never offers this; if something else asks, it hears the
        // reason rather than a form the server would refuse.
        crate::events::show_notice(store, crate::state::ONLY_AN_OWNER_SHARES_NOTE.to_string());
        return;
    }
    let signed_in = untracked(|| store.cloud_signed_in.get());
    if !signed_in && !held_as_share {
        open_account_modal(store, "Sign in to share a node");
        return;
    }
    let ask_for_members = marker.is_some() && signed_in;
    let name = match &marker {
        Some(marker) => marker.name.clone(),
        None => store.display_label(store_id, node_id),
    };
    store.share_modal_shared.set(marker.is_some());
    store.share_modal_name.set(name);
    store.share_modal_members.set(Vec::new());
    store.share_modal_state.set(String::new());
    store.share_modal_invite_email.set(String::new());
    store.share_modal_invite_role.set("editor".to_string());
    store.share_modal_error.set(String::new());
    store.share_modal_confirm_stop.set(false);
    store.share_modal_pending.set(ask_for_members.then_some(crate::protocol::CloudOp::ShareInfo));
    store.share_modal_node.set(Some((store_id, node_id)));
    if ask_for_members {
        store.send(BackendCommand::CloudShareInfo { store_id, node_id });
    }
}

/// How a hosted store reads in the "Add Hosted Store..." list. A row with a
/// `root` is a share — a scope inside someone else's store — so it reads as
/// the share's own name and who shared it, never the owner's store name,
/// which no recipient ever sees (docs/NODE_DOCUMENT_CONTRACT.md section 5,
/// "A share has a name of its own").
fn hosted_store_label(info: &pimble_rpc::CloudHostedStoreInfo) -> String {
    match (info.root.is_some(), &info.shared_by) {
        (true, Some(email)) => format!("{}, shared by {}", info.name, email),
        _ => info.name.clone(),
    }
}

/// How far a member is from being able to open the share, in words
/// (docs/NODE_DOCUMENT_CONTRACT.md section 5). An invitation to an address
/// with no account yet, and a member whose key no device of ours has wrapped
/// yet, are both ordinary states rather than failures, so both say what is
/// happening. The key is handed over by an owner's Pimble, which is "your
/// Pimble" only to someone who manages the share (`manages`); a member
/// reading the list is told whose it is.
fn member_status_text(status: pimble_rpc::ShareMemberStatus, manages: bool) -> &'static str {
    match (status, manages) {
        (pimble_rpc::ShareMemberStatus::Invited, _) => "invited, no account yet",
        (pimble_rpc::ShareMemberStatus::WaitingForKey, true) => "waiting for your Pimble to hand over the key",
        (pimble_rpc::ShareMemberStatus::WaitingForKey, false) => "waiting for an owner's Pimble to hand over the key",
        (pimble_rpc::ShareMemberStatus::Active, _) => "active",
    }
}

/// A member's role in words, for the member list. An editor edits everything
/// in the share — the text, the titles, the structure — so "can edit" is the
/// whole of it (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles").
fn member_role_text(role: pimble_rpc::MemberRole) -> &'static str {
    match role {
        pimble_rpc::MemberRole::Owner => "owner",
        pimble_rpc::MemberRole::Editor => "can edit",
        pimble_rpc::MemberRole::Reader => "can read",
    }
}

/// Open the "Appearance..." picker for `(store_id, node_id)`: seed the tags field
/// from the node and clear the icon search.
fn open_appearance_modal(store: AppStore, store_id: pimble_core::StoreId, node_id: pimble_core::NodeId) {
    let tags = store
        .get_node_signal(store_id, node_id)
        .map(|sig| untracked(|| sig.with(|n| n.metadata.tags.join(", "))))
        .unwrap_or_default();
    store.appearance_tags_text.set(tags);
    store.appearance_icon_query.set(String::new());
    store.appearance_modal_node.set(Some((store_id, node_id)));
}

/// Cancel a pending debounce timer and reset the search box to empty/idle.
/// Also drops the search box's Escape interceptor, installed by `oninput`
/// only while the box is non-empty (see there) so it never fights the tree
/// rename feature's own interceptor, which owns the same single global slot.
fn clear_search(store: AppStore) {
    if let Some(handle) = SEARCH_DEBOUNCE.with(|slot| slot.borrow_mut().take()) {
        clear_timeout(handle);
    }
    store.search_query.set(String::new());
    store.search_results.set(SearchState::Idle);
    clear_keyboard_interceptor();
}

/// Debounce search-as-you-type by 250ms (per the step 5 contract) and dispatch
/// a `BackendCommand::Search` over every open store when the timer fires.
fn schedule_search(store: AppStore, query: String) {
    if let Some(handle) = SEARCH_DEBOUNCE.with(|slot| slot.borrow_mut().take()) {
        clear_timeout(handle);
    }
    if query.is_empty() {
        store.search_results.set(SearchState::Idle);
        return;
    }
    let timeout = set_timeout(250, move || {
        SEARCH_DEBOUNCE.with(|slot| { slot.borrow_mut().take(); });
        let stores = untracked(|| store.store_ids.get());
        store.send(BackendCommand::Search { query, stores, limit: 50 });
    });
    SEARCH_DEBOUNCE.with(|slot| { *slot.borrow_mut() = Some(timeout); });
}

/// One search-result row, pre-resolved to plain fields the results panel
/// renders directly — `SearchResultItem` itself isn't `PartialEq`, which the
/// rsx `for` loop requires (see `rinch:rinch` Rule 9), and pimble-rpc isn't
/// this crate's to change.
#[derive(Clone, PartialEq)]
struct SearchRow {
    key: String,
    value: String,
    title: String,
    store_name: String,
    snippet: String,
    kind: &'static str,
}

/// Resolve raw search hits into display-ready `SearchRow`s (looking up each
/// store's name from already-cached signals — cheap, no new fetch). A plain
/// function so the rsx `for`'s iterable expression is a bare call with no
/// turbofish or closure braces for the macro's brace-stopping parser to trip
/// on (see `parse_expr_before_brace` in rinch-macros).
fn search_rows(store: AppStore, items: Vec<pimble_rpc::SearchResultItem>) -> Vec<SearchRow> {
    items.into_iter().map(|item| SearchRow {
        key: format!("{}_{}", item.store_id, item.node_id),
        value: format!("node_{}_{}", item.store_id, item.node_id),
        title: if item.title.is_empty() { "Untitled".to_string() } else { item.title },
        store_name: store.get_store_signal(item.store_id)
            .map(|sig| untracked(|| sig.with(|s| s.name.clone())))
            .unwrap_or_default(),
        snippet: item.snippet,
        kind: kind_label(&item.kind),
    }).collect()
}

/// A subtle, human-readable label for a search hit's `kind` (the index unit
/// kind the hit's chunk carries: prose, a heading, code, a table row, or a
/// structured field). Empty or unrecognized kinds render nothing.
fn kind_label(kind: &str) -> &'static str {
    match kind {
        "prose" | "document" => "",
        "heading" => "heading",
        "code" => "code",
        "table_row" | "table-row" | "tablerow" => "table row",
        "field" => "field",
        "folder" => "folder",
        _ => "",
    }
}

/// Text for a linked store's status badge (docs/SYNC_CONTRACT.md "B: app
/// side"). `Conflict` is out of scope for this contract (there are no
/// conflicts to show yet) but still needs a label rather than a match gap.
/// An encrypting vault link says so first (docs/DESKTOP_ACCOUNT_CONTRACT.md
/// decision 4): `encrypted · synced`.
fn sync_badge_text(state: &pimble_core::SyncState, mode: pimble_core::StoreKind) -> &'static str {
    match (mode, state) {
        (pimble_core::StoreKind::Plain, pimble_core::SyncState::Offline) => "offline",
        (pimble_core::StoreKind::Plain, pimble_core::SyncState::Syncing) => "syncing",
        (pimble_core::StoreKind::Plain, pimble_core::SyncState::Synced { .. }) => "synced",
        (pimble_core::StoreKind::Plain, pimble_core::SyncState::Conflict { .. }) => "conflict",
        (pimble_core::StoreKind::Vault, pimble_core::SyncState::Offline) => "encrypted · offline",
        (pimble_core::StoreKind::Vault, pimble_core::SyncState::Syncing) => "encrypted · syncing",
        (pimble_core::StoreKind::Vault, pimble_core::SyncState::Synced { .. }) => "encrypted · synced",
        (pimble_core::StoreKind::Vault, pimble_core::SyncState::Conflict { .. }) => "encrypted · conflict",
    }
}

/// Walk the DOM subtree to find a `data-oncontextmenu` handler and copy it
/// to `target`. Used to hoist the context menu handler from the invisible
/// ContextMenuTarget (display:contents) to a visible wrapper div.
fn hoist_context_menu_handler(from: &NodeHandle, target: &NodeHandle) {
    if let Some(handler_id) = find_context_menu_handler(from) {
        target.set_attribute("data-oncontextmenu", &handler_id);
    }
}

fn find_context_menu_handler(node: &NodeHandle) -> Option<String> {
    if let Some(val) = node.get_attribute("data-oncontextmenu") {
        return Some(val);
    }
    for child in node.children() {
        if let Some(val) = find_context_menu_handler(&child) {
            return Some(val);
        }
    }
    None
}

/// The application: its reactive state, its backend-event pump, and the component
/// that renders the whole UI. Both entry points call this and then differ only
/// in the platform chrome around it — [`run`] wraps it in a desktop window with
/// a menu bar, the web app hands the component straight to `rinch_web::mount`.
///
/// The returned [`AppStore`] is the same one the component uses, so a caller can
/// install a backend into it (the web app does, before mounting) or read the
/// theme choice out of it (the desktop does, to build the theme props).
pub fn build_view() -> (AppStore, impl FnOnce(&mut RenderScope) -> NodeHandle) {
    let store = AppStore::new();
    store.dark_mode.set(load_dark_mode());

    // Double-click detection for rename: (last_click_time, last_click_value)
    let last_click: Rc<Cell<(f64, String)>> = Rc::new(Cell::new((now_ms(), String::new())));

    // Persistent tree state — created once, preserves expanded/selected across data changes
    let tree_state = UseTreeReturn::new(UseTreeOptions::default());

    // Drag-and-drop state for tree node rearrangement
    let drag_ctx: DragContext<String> = DragContext::new();

    // Set up event processing via thread-local so run_on_main_thread
    // can trigger it without capturing non-Send types. (No autosave loop
    // needed — the collab session persists edits live.)
    EVENT_PROCESSOR.with(|cell| {
        *cell.borrow_mut() = Some(Box::new(move || {
            process_backend_events(store, tree_state);
        }));
    });
    // Build app component - parameter must be named __scope for the rsx! macro
    let app_component = move |__scope: &mut RenderScope| -> NodeHandle {
        // Cancel any in-progress rename, optionally committing the change.
        let cancel_last_click = last_click.clone();
        let cancel_rename = move |commit: bool| {
            if let Some(prev_value) = untracked(|| store.renaming_node.get()) {
                if commit {
                    let new_title = untracked(|| store.rename_text.get());
                    if let Some((sid, Some(nid))) = parse_tree_value(&prev_value) {
                        store.send(BackendCommand::RenameNode {
                            store_id: sid, node_id: nid, title: new_title,
                        });
                    }
                }
                store.renaming_node.set(None);
                clear_keyboard_interceptor();
            }
            // Reset double-click timer so stale timestamps can't cause
            // a spurious rename on the next click.
            cancel_last_click.set((now_ms(), String::new()));
        };

        // Tree callbacks
        let select_last_click = last_click.clone();
        let cancel_rename_for_select = cancel_rename.clone();
        let on_tree_select = ValueCallback::new(move |value: String| {
            tracing::info!("Tree node selected: {}", value);

            // Double-click on a node → enter rename mode
            {
                let now = now_ms();
                let (prev_time, prev_value) = select_last_click.replace((now, value.clone()));
                let is_double_click = prev_value == value && now - prev_time < DOUBLE_CLICK_MS;

                if is_double_click && value.starts_with("node_") {
                    // Don't allow rename on mount nodes
                    let is_mount_node = if let Some((sid, Some(nid))) = parse_tree_value(&value) {
                        store.is_mount(sid, nid)
                    } else {
                        false
                    };
                    if is_mount_node {
                        return;
                    }

                    // A node this device may only read changes in no way
                    // (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles"):
                    // the context menu's "Rename" is disabled there, so the
                    // double-click shortcut is not a way around it. An editor
                    // renames like anyone else. Judged per node: a reader's
                    // root may sit beside an editor's in the same store.
                    let renameable = match parse_tree_value(&value) {
                        Some((s_id, Some(n_id))) => store.node_access(s_id, n_id).allows_write(),
                        Some((s_id, None)) => store.store_access(s_id).allows_write(),
                        None => true,
                    };
                    if !renameable {
                        return;
                    }

                    let edit_text = if let Some((store_id, Some(node_id))) = parse_tree_value(&value) {
                        store.get_node_signal(store_id, node_id)
                            .map(|sig| sig.with(|n| n.metadata.title.clone()))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    store.rename_text.set(edit_text);
                    store.renaming_node.set(Some(value.clone()));
                    let nv_interceptor = value.clone();
                    set_keyboard_interceptor(move |data| {
                        if data.key == "Escape" {
                            run_on_main_thread(move || {
                                store.renaming_node.set(None);
                                clear_keyboard_interceptor();
                            });
                            return true;
                        }
                        if data.key == "Enter" {
                            let nv = nv_interceptor.clone();
                            run_on_main_thread(move || {
                                let new_title = untracked(|| store.rename_text.get());
                                store.renaming_node.set(None);
                                clear_keyboard_interceptor();
                                if let Some((s_id, Some(n_id))) = parse_tree_value(&nv) {
                                    store.send(BackendCommand::RenameNode {
                                        store_id: s_id, node_id: n_id, title: new_title,
                                    });
                                }
                            });
                            return true;
                        }
                        false
                    });
                    if let Some((doc_key, input_node_id)) = store.rename_input_ids.with(|ids| ids.get(&value).copied()) {
                        request_focus(doc_key, input_node_id);
                    }
                    return;
                }

                // If we were renaming a node, commit it before switching
                cancel_rename_for_select(true);
            }

            // (The previously-edited node's changes already persisted live through
            // the collab delta relay — no explicit save-on-switch is needed.)

            // Update selection and load new node — the same `open_node` a
            // search-result click uses, so there is exactly one way to open one.
            open_node(store, tree_state, value);
        });

        let on_tree_expand = ValueCallback::new(move |value: String| {
            tracing::info!("Tree node expanded: {}", value);
            if let Some((s_id, node_id_opt)) = parse_tree_value(&value) {
                let resolved = node_id_opt.or_else(|| store.root_node_id(s_id));
                if let Some(nid) = resolved {
                    store.expanded.update(|e| { e.insert((s_id, nid)); });
                    let needs_fetch = !store.has_children_loaded(s_id, nid);
                    if needs_fetch {
                        store.send(BackendCommand::GetChildren { store_id: s_id, node_id: nid });
                    }
                }
            }
            // No signal mutation — UseTreeReturn handles expanded state internally
        });

        let on_tree_collapse = ValueCallback::new(move |value: String| {
            tracing::info!("Tree node collapsed: {}", value);
            if let Some((s_id, node_id_opt)) = parse_tree_value(&value) {
                let resolved = node_id_opt.or_else(|| store.root_node_id(s_id));
                if let Some(nid) = resolved {
                    store.expanded.update(|e| { e.remove(&(s_id, nid)); });
                }
            }
        });

        // Build the tree with reactive data_source.
        // ONLY subscribes to tree_structure_version; everything else is untracked.
        let data_source: Rc<dyn Fn() -> Vec<TreeNodeData>> =
            Rc::new(move || {
                let _ = store.tree_structure_version.get(); // structural subscription
                untracked(|| store.build_tree_data_structural())
            });

        // Custom render_node with per-node reactive Effects for label/icon/mount
        let render_node_fn: RenderTreeNode = Rc::new(move |payload: &RenderTreeNodePayload, __scope: &mut RenderScope| {
            let node_value = payload.node.value.clone();
            let is_store_root = node_value.starts_with("store_");
            let has_children = payload.has_children;

            // Parse once for reuse
            let parsed = parse_tree_value(&node_value);

            // Determine if this node is a mount point (static — mount status is structural)
            let is_mount = if !is_store_root {
                if let Some((s_id, Some(n_id))) = parsed {
                    store.is_mount(s_id, n_id)
                } else {
                    false
                }
            } else {
                false
            };

            // Capture per-entity signals for reactive label/icon Effects.
            // These are looked up once (untracked) and the inner Signal is captured.
            let node_sig = if !is_store_root {
                if let Some((s_id, Some(n_id))) = parsed {
                    store.get_node_signal(s_id, n_id)
                } else {
                    None
                }
            } else {
                None
            };

            let store_sig = if is_store_root {
                parsed.and_then(|(s_id, _)| store.get_store_signal(s_id))
            } else {
                None
            };

            let mount_sig = if is_mount {
                if let Some((s_id, Some(n_id))) = parsed {
                    store.get_mount_signal(s_id, n_id)
                } else {
                    None
                }
            } else {
                None
            };

            // Sync status (store roots only, docs/SYNC_CONTRACT.md "B: app
            // side"): looked up once (an entry always exists by the time a
            // registered store's row renders — see `ensure_sync_entry`) and
            // read reactively below for the badge; `is_linked_now` is a
            // static (untracked) snapshot for the context menu's `disabled`
            // values, rebuilt only when the tree structurally re-renders this
            // row (rinch #714 — see the `no_mount_source` note further down).
            let sync_sig = if is_store_root {
                parsed.and_then(|(s_id, _)| store.get_sync_signal(s_id))
            } else {
                None
            };
            let is_linked_now = sync_sig
                .map(|sig| untracked(|| sig.with(|(remote, _)| remote.is_some())))
                .unwrap_or(false);

            // Same static-snapshot-rebuilt-on-structural-rebuild pattern as
            // `is_linked_now` (rinch #714 — see the `no_mount_source` note
            // further down): whether this store row is a replica, for
            // "Remove Replica..."'s `disabled` value.
            let is_replica_now = store_sig
                .map(|sig| untracked(|| sig.with(|s| s.is_replica)))
                .unwrap_or(false);
            // And whether it is itself an encrypted store, for "Host on
            // Pimble Cloud..."'s `disabled` value (decision 5): the server
            // holds only blobs for one, so there is nothing here to host.
            let is_vault_now = parsed
                .map(|(s_id, _)| store.is_vault(s_id))
                .unwrap_or(false);

            // What this device may change of this row's node
            // (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles"): the
            // server's own judgement of the node (`AppStore::node_access`),
            // because one store can hold a root this account reads beside a
            // root it edits, and only the rows under the first have anything
            // greyed out. A store row asks about the store. `Full` is one's
            // own store and an editor's scope alike, so an editor of a share
            // has nothing greyed out; only a reader does.
            // `parse_tree_value` returns the CANONICAL pair, so a node
            // reached through a mount is judged as the node it is in its
            // source store, which is where the RPC would go. Snapshotted like
            // `is_linked_now` (rinch #714); the row's data carries it, so a
            // node whose access changes re-renders its row with fresh items.
            let access_now = match parsed {
                Some((s_id, Some(n_id))) => store.node_access(s_id, n_id),
                Some((s_id, None)) => store.store_access(s_id),
                None => Default::default(),
            };
            let can_write_tree = access_now.allows_write();
            // A mount's "New Node" creates under the mount's SOURCE node, so
            // that node's access is what governs that one item.
            let can_write_mount_source = mount_sig
                .and_then(|sig| untracked(|| sig.with(|m| m.mount_ref.as_ref().map(|r| (r.source_store, r.source_node)))))
                .map_or(can_write_tree, |(source_store, source_node)| store.node_access(source_store, source_node).allows_write());
            // Whether this row's node carries a share marker (a store row
            // takes its root node's, like its icon and colour, and a partial
            // replica's row has no root of its own): the badge.
            let is_shared_now = if is_store_root {
                parsed
                    .and_then(|(s_id, _)| store.store_row_node(s_id).map(|root| store.is_shared(s_id, root)))
                    .unwrap_or(false)
            } else {
                parsed
                    .and_then(|(s_id, n_id)| n_id.map(|n_id| store.is_shared(s_id, n_id)))
                    .unwrap_or(false)
            };
            // "Share...": in a store that reached this device as someone
            // else's share nothing can be shared from here, so the item opens
            // only on a shared root (the member's read-only face of the
            // modal) and every other row says why it is off. `parsed` is the
            // canonical pair, so a node reached through a mount is judged by
            // the store it lives in. Snapshotted like `is_linked_now` (rinch
            // #714): the row's data carries the store's `shared_by`, the
            // marker and the access, so a change of any re-renders the row.
            let held_as_share_now = parsed
                .map(|(s_id, _)| store.shared_by(s_id).is_some())
                .unwrap_or(false);
            let share_disabled = crate::state::share_item_disabled(access_now, held_as_share_now, is_shared_now);
            // A browser build has no "Share..." item to explain.
            let share_note_text = if CAN_ADMINISTER_STORES {
                crate::state::share_item_note(held_as_share_now, is_shared_now)
            } else {
                ""
            };

            // Choose icon (static — changes only on structural rebuild). Folders
            // are folders even when empty; documents are documents even with
            // children, so the icon follows node_type, not has_children.
            let is_folder = node_sig
                .map(|sig| sig.with(|n| n.node_type == pimble_core::node_types::FOLDER))
                .unwrap_or(has_children);
            // A custom icon or colour from the node's metadata (the "Appearance..."
            // picker, or an import); a store row takes its root node's, and a
            // partial replica's row has none. Snapshotted here like the type
            // icon: the row's `TreeNodeData` carries both, so a change
            // re-renders the row.
            let appearance_sig = if is_store_root {
                parsed.and_then(|(s_id, _)| store.store_row_node(s_id).and_then(|root| store.get_node_signal(s_id, root)))
            } else {
                node_sig
            };
            let (custom_icon, node_color) = appearance_sig
                .map(|sig| sig.with(|n| (n.metadata.icon().and_then(icon_by_name), n.metadata.color().map(String::from))))
                .unwrap_or((None, None));
            let icon = if let Some(custom) = custom_icon {
                custom
            } else if is_store_root {
                TablerIcon::Database
            } else if is_mount {
                TablerIcon::Link
            } else if is_folder {
                TablerIcon::Folder
            } else {
                TablerIcon::File
            };
            // The CSS for the node's colour on the current theme; reactive on the
            // theme so a toggle recolours rows in place. One clone per closure
            // (rinch: two closures cannot share one moved String).
            let color_css = move |color: &Option<String>| -> String {
                color.as_ref().map(|c| format!(" color: {};", display_color(c, store.dark_mode.get()))).unwrap_or_default()
            };
            let icon_color = node_color.clone();
            let label_color = node_color;

            let icon_class = if is_mount {
                "rinch-tree__icon rinch-tree__icon--mount"
            } else {
                "rinch-tree__icon"
            };

            let base_label_style = if is_store_root {
                "cursor: default; font-weight: 600; text-transform: uppercase; font-size: 11px; letter-spacing: 0.03em;"
            } else {
                "cursor: default;"
            };

            // Drag-and-drop callbacks
            let nv_dragstart = node_value.clone();
            let nv_drop = node_value.clone();
            let nv_enter = node_value.clone();
            let nv_leave = node_value.clone();

            let on_dragstart = move || {
                tracing::info!("DragStart: {}", nv_dragstart);
                drag_ctx.set(nv_dragstart.clone());
            };
            let on_dragend = move || {
                tracing::info!("DragEnd");
                drag_ctx.clear();
                store.drop_target.set(None);
            };
            let on_drop = {
                let nv = nv_drop.clone();
                move || {
                    // Always clear drag state first, even if the drop is invalid
                    let dragged_value = drag_ctx.take();
                    store.drop_target.set(None);

                    let Some(dragged_value) = dragged_value else { return; };
                    if dragged_value == nv { return; }
                    let Some((drag_store_id, Some(drag_node_id))) = parse_tree_value(&dragged_value) else { return; };
                    let new_parent_id = if let Some((target_store_id, target_node_id_opt)) = parse_tree_value(&nv) {
                        // What is only read takes no moves: such rows do not
                        // drag and the server would refuse it anyway
                        // (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles").
                        // An editor moves nodes inside their scope freely. A
                        // move edits three documents, and the server judges
                        // all three: the node, the list it leaves and the
                        // list it joins. So does this, before anything is
                        // sent, each by the server's own word on that node.
                        // A drop on a store row lands in the root that row
                        // stands for, so that node is the one judged.
                        let target_access = match target_node_id_opt.or_else(|| store.root_node_id(target_store_id)) {
                            Some(nid) => store.node_access(target_store_id, nid),
                            None => store.store_access(target_store_id),
                        };
                        let leaves_access = store
                            .cached_parent_id(drag_store_id, drag_node_id)
                            .map_or(pimble_core::StoreAccess::Full, |old_parent| store.node_access(drag_store_id, old_parent));
                        if !target_access.allows_write()
                            || !leaves_access.allows_write()
                            || !store.node_access(drag_store_id, drag_node_id).allows_write()
                        {
                            tracing::info!("Ignoring drop of {:?} into {}: one of them is shared with us to read", drag_node_id, nv);
                            return;
                        }
                        if drag_store_id != target_store_id {
                            tracing::warn!(
                                "Ignoring drop: source store {:?} differs from target store {:?} (cross-store move not supported yet)",
                                drag_store_id, target_store_id
                            );
                            return;
                        }
                        match target_node_id_opt {
                            Some(nid) => {
                                if store.is_mount(target_store_id, nid) {
                                    tracing::warn!("Ignoring drop onto mount node {:?}/{:?}", target_store_id, nid);
                                    return;
                                }
                                nid
                            }
                            None => {
                                match store.root_node_id(target_store_id) {
                                    Some(id) => id,
                                    None => return,
                                }
                            }
                        }
                    } else {
                        return;
                    };
                    tracing::info!("Drop: moving {:?} into {:?}", drag_node_id, new_parent_id);
                    // Record the exact (possibly mount-path-qualified) tree
                    // value dropped onto, so the resulting `NodeMoved` event
                    // can auto-expand precisely this place (decision 7).
                    crate::state::set_last_drop_target_value(nv.clone());
                    store.send(BackendCommand::MoveNode {
                        store_id: drag_store_id, node_id: drag_node_id,
                        new_parent_id, position: None,
                    });
                }
            };
            let on_dragenter = move || { store.drop_target.set(Some(nv_enter.clone())); };
            let on_dragleave = {
                let nv = nv_leave.clone();
                move || {
                    if store.drop_target.get().as_deref() == Some(&nv) {
                        store.drop_target.set(None);
                    }
                }
            };

            // Context menu actions
            let nv_ctx = node_value.clone();
            let on_new_child = {
                let nv = nv_ctx.clone();
                move || {
                    if let Some((s_id, node_id_opt)) = parse_tree_value(&nv) {
                        let parent_id = node_id_opt.or_else(|| store.root_node_id(s_id));
                        if let Some(pid) = parent_id {
                            store.send(BackendCommand::CreateNode {
                                store_id: s_id, parent_id: Some(pid), title: String::new(),
                            });
                        }
                    }
                }
            };
            let on_close_store = {
                let nv = nv_ctx.clone();
                move || {
                    if let Some((s_id, _)) = parse_tree_value(&nv) {
                        store.send(BackendCommand::CloseStore { store_id: s_id });
                    }
                }
            };
            let on_link_to_remote = {
                let nv = nv_ctx.clone();
                move || {
                    if let Some((s_id, _)) = parse_tree_value(&nv) {
                        store.link_modal_store.set(Some(s_id));
                        store.link_modal_url.set(String::new());
                        store.link_modal_token.set(String::new());
                        store.link_modal_token_visible.set(false);
                        store.link_modal_error.set(String::new());
                    }
                }
            };
            let on_unlink_from_remote = {
                let nv = nv_ctx.clone();
                move || {
                    if let Some((s_id, _)) = parse_tree_value(&nv) {
                        store.send(BackendCommand::SetStoreSync { store_id: s_id, remote: None });
                    }
                }
            };
            let on_host_on_cloud = {
                let nv = nv_ctx.clone();
                move || {
                    if let Some((s_id, _)) = parse_tree_value(&nv) {
                        open_host_modal(store, s_id);
                    }
                }
            };
            // "Share...": grant people a scope on this store rooted at this
            // node (a store row shares its root node), so they hold the same
            // documents and co-author them (docs/NODE_DOCUMENT_CONTRACT.md
            // section 5). Never disabled for a store that is not hosted: the
            // server answers with the sentence that says what is missing, and
            // the modal shows it. Disabled in someone else's store on every
            // node but a shared root (`share_disabled` above), where it opens
            // the member's face of the modal.
            let on_share = {
                let nv = nv_ctx.clone();
                move || {
                    if let Some((s_id, node_id_opt)) = parse_tree_value(&nv) {
                        if let Some(n_id) = node_id_opt.or_else(|| store.root_node_id(s_id)) {
                            open_share_modal(store, s_id, n_id);
                        }
                    }
                }
            };
            let on_remove_replica = {
                let nv = nv_ctx.clone();
                move || {
                    if let Some((s_id, _)) = parse_tree_value(&nv) {
                        store.remove_replica_modal_store.set(Some(s_id));
                        store.remove_replica_modal_error.set(String::new());
                        store.remove_replica_modal_pending.set(false);
                    }
                }
            };
            let on_mount_store = {
                let nv = nv_ctx.clone();
                move || {
                    if let Some((s_id, node_id_opt)) = parse_tree_value(&nv) {
                        let parent_id = node_id_opt.or_else(|| store.root_node_id(s_id));
                        if let Some(pid) = parent_id {
                            #[cfg(feature = "native")]
                            {
                                let dialog = rinch::dialogs::pick_folder()
                                    .set_title("Select Store to Mount");
                                if let Some(path) = dialog.pick() {
                                    let path_str = path.to_string_lossy().to_string();
                                    tracing::info!("Mounting store from: {} into {:?}", path_str, s_id);
                                    store.send(BackendCommand::OpenStore { path: path_str.clone() });
                                    store.pending_mount.set(Some(PendingMount {
                                        target_store_id: s_id,
                                        target_parent_id: pid,
                                        source_path: path_str,
                                    }));
                                }
                            }
                            // A browser has no directories to offer, so it
                            // picks from the stores the account already grants.
                            // The RPC either way is `createMount`, which an
                            // editor may call.
                            #[cfg(not(feature = "native"))]
                            open_mount_picker(store, (s_id, pid));
                        }
                    }
                }
            };

            // "Mount Remote Store Here..." — store roots and ordinary nodes
            // only. Opens the connect modal in mount mode with this row's
            // canonical target (a store root's target is its own root node).
            let on_mount_remote_store = {
                let nv = nv_ctx.clone();
                move || {
                    if let Some((s_id, node_id_opt)) = parse_tree_value(&nv) {
                        let parent_id = node_id_opt.or_else(|| store.root_node_id(s_id));
                        match parent_id {
                            Some(pid) => open_connect_modal(store, Some((s_id, pid))),
                            None => tracing::warn!("No target node for a remote mount under {:?}", s_id),
                        }
                    }
                }
            };

            let on_delete = {
                let nv = nv_ctx.clone();
                move || {
                    if let Some((s_id, Some(n_id))) = parse_tree_value(&nv) {
                        store.send(BackendCommand::DeleteNode {
                            store_id: s_id, node_id: n_id,
                        });
                    }
                }
            };
            // "Appearance...": open the icon, colour and tags picker for this
            // node (a store row edits its root node).
            let on_appearance = {
                let nv = nv_ctx.clone();
                move || {
                    if let Some((s_id, node_id_opt)) = parse_tree_value(&nv) {
                        if let Some(n_id) = node_id_opt.or_else(|| store.root_node_id(s_id)) {
                            open_appearance_modal(store, s_id, n_id);
                        }
                    }
                }
            };

            // Mount-node-only "New Node": creates under the mount's source
            // (mount_ref), never under the mount node itself (decision 2 —
            // the server rejects a mount node as a create parent).
            let on_new_child_mount = {
                let sig = mount_sig;
                move || {
                    let Some(sig) = sig else { return };
                    let mount_ref = untracked(|| sig.with(|m| m.mount_ref.clone()));
                    match mount_ref {
                        Some(mount_ref) => {
                            store.send(BackendCommand::CreateNode {
                                store_id: mount_ref.source_store,
                                parent_id: Some(mount_ref.source_node),
                                title: String::new(),
                            });
                        }
                        None => tracing::warn!("Mount node has no mount_ref yet; cannot create a child under it"),
                    }
                }
            };

            // "Copy as Mount Source" — store roots and ordinary nodes only
            // (not mount nodes, which are placeholders). Not reactive, so a
            // single `move` closure built once is fine (Rule 4/16).
            let on_copy_as_mount_source = {
                let nv = nv_ctx.clone();
                move || copy_as_mount_source(store, &nv)
            };
            // "Paste Mount Here" is always rendered, disabled while nothing is
            // copied, because a `DropdownMenuItem`'s `disabled` is a value
            // snapshotted at render time rather than a reactive one.
            // `copy_as_mount_source`/`paste_mount_here` bump the tree structure
            // afterwards (see `bump_tree_after_menu_closes` for why that has to
            // wait a turn), so every row's menu re-renders with a fresh value.
            //
            // An item may sit inside an `if` in rsx: `show_dom` renders the
            // branch synchronously the first time, while `ContextMenu`'s close
            // signal is still published, so the item closes the menu like any
            // other. Verified on the desktop with the rinch debug tools.
            let no_mount_source = untracked(|| store.mount_source.get().is_none());

            // Mounts do not exist in an encrypted store: the server holds only
            // blobs there and has no tree to point a mount at. Hiding the two
            // items rather than disabling them keeps the menu honest — there is
            // nothing the reader could do to enable them.
            let mounts_apply = parsed
                .map(|(s_id, _)| !store.is_vault(s_id))
                .unwrap_or(true);
            let on_paste_mount = {
                let nv = nv_ctx.clone();
                move || paste_mount_here(store, &nv)
            };

            // Build the wrapper span with drag-and-drop via rsx. A node this
            // device may not change does not drag out of its place, as the
            // drop handler refuses drops into one.
            let draggable = if is_store_root || !can_write_tree { "false" } else { "true" };

            let icon_el = render_tabler_icon(__scope, icon, TablerIconStyle::Outline);

            // The badge a shared node's row carries after its label. Drawn
            // from the render-time snapshot, like the type icon: the row's
            // data holds the marker, so it re-renders when one appears or goes
            // (docs/NODE_DOCUMENT_CONTRACT.md section 5).
            let share_badge: Option<NodeHandle> = if is_shared_now {
                let badge_icon = render_tabler_icon(__scope, TablerIcon::Share, TablerIconStyle::Outline);
                Some(rsx! {
                    span {
                        class: "pimble-tree__share",
                        title: "Shared with other people",
                        {badge_icon}
                    }
                })
            } else {
                None
            };

            // A store someone else shared with this account says so on its
            // row, and what it lets this device do. Reactive on the store's
            // own signal so it appears as soon as the store arrives.
            let shared_by_note: Option<NodeHandle> = if is_store_root {
                // The sidebar is narrow enough to clip this, so the same
                // sentence is the row's tooltip.
                let shared_by_text = move || -> String {
                    store_sig
                        .map(|sig| {
                            sig.with(|s| match &s.shared_by {
                                Some(email) => {
                                    let words = crate::state::access_words(s.access);
                                    if words.is_empty() {
                                        format!("shared by {email}")
                                    } else {
                                        format!("shared by {email} · {words}")
                                    }
                                }
                                None => String::new(),
                            })
                        })
                        .unwrap_or_default()
                };
                Some(rsx! {
                    span {
                        class: "pimble-tree__shared-by",
                        style: {
                            move || {
                                let shared = store_sig.map_or(false, |sig| sig.with(|s| s.shared_by.is_some()));
                                if shared { "" } else { "display: none;" }
                            }
                        },
                        title: {move || shared_by_text()},
                        {move || shared_by_text()}
                    }
                })
            } else {
                None
            };

            let nv_submit = node_value.clone();
            let nv_effect = node_value.clone();
            let nv_for_context_menu = node_value.clone();

            // Memo: only propagates when THIS node's rename state actually changes.
            let is_renaming = {
                let nv = node_value.clone();
                Memo::new(move || store.renaming_node.get().as_deref() == Some(&nv))
            };

            let rename_input = rsx! {
                input {
                    r#type: "text",
                    class: "rinch-text-input__input",
                    style: {
                        move || {
                            if is_renaming.get() {
                                "width: 100%; font-size: inherit; padding: 0 2px; height: 22px; line-height: 22px;"
                            } else {
                                "display: none;"
                            }
                        }
                    },
                    value_fn: move || store.rename_text.get(),
                    oninput: move |value: String| {
                        store.rename_text.set(value);
                    },
                    onsubmit: move || {
                        let new_title = store.rename_text.get();
                        store.renaming_node.set(None);
                        clear_keyboard_interceptor();
                        if let Some((s_id, Some(n_id))) = parse_tree_value(&nv_submit) {
                            store.send(BackendCommand::RenameNode {
                                store_id: s_id, node_id: n_id, title: new_title,
                            });
                        }
                    },
                }
            };

            // Register this input's DOM node ID so the double-click handler can focus it
            store.rename_input_ids.update(|ids| {
                ids.insert(node_value.clone(), (rename_input.doc_key(), rename_input.node_id().0));
            });

            // Clone before wrapper rsx moves rename_input
            let rename_input_for_focus = rename_input.clone();

            let wrapper = rsx! {
                span {
                    class: "rinch-tree__label",
                    style: "flex: 1; display: inline-flex; align-items: center;",

                    span {
                        class: icon_class,
                        style: {
                            // Reactive icon opacity: a mount whose source is
                            // out of reach or not reachable yet renders dimmed.
                            || {
                                let dimmed = mount_sig.map_or(false, |ms| {
                                    ms.with(|m| m.mount_state.as_ref().map_or(false, mount_is_dimmed))
                                });
                                let color = color_css(&icon_color);
                                if dimmed {
                                    format!("width: 1rem; height: 1rem; margin-right: 4px; opacity: 0.4;{color}")
                                } else {
                                    color
                                }
                            }
                        },
                        {icon_el}
                    }

                    span {
                        style: {
                            // Reactive label style — hides during rename, dims
                            // a mount whose source is out of reach or not
                            // reachable yet.
                            || {
                                if is_renaming.get() {
                                    return "display: none;".to_string();
                                }
                                let dimmed = mount_sig.map_or(false, |ms| {
                                    ms.with(|m| m.mount_state.as_ref().map_or(false, mount_is_dimmed))
                                });
                                let color = color_css(&label_color);
                                if dimmed {
                                    format!("cursor: default; opacity: 0.4;{color}")
                                } else {
                                    format!("{base_label_style}{color}")
                                }
                            }
                        },

                        // Reactive label text — subscribes to per-node/per-store signal
                        // (and, for a node under active local edit, `live_label`).
                        {move || {
                            let label = if is_store_root {
                                store_sig.map(|s| s.with(|st| st.name.clone()))
                                    .unwrap_or_default()
                            } else {
                                let live = parsed.and_then(|(s_id, n_id)| {
                                    let n_id = n_id?;
                                    store.live_label.with(|m| m.get(&(s_id, n_id)).cloned())
                                });
                                live.or_else(|| node_sig.map(|s| s.with(|n| display_label_from_node(n))))
                                    .unwrap_or_else(|| "Untitled".to_string())
                            };

                            if let Some(ms) = mount_sig {
                                let suffix = ms.with(|m| {
                                    m.mount_state.as_ref().map_or("", mount_label_suffix)
                                });
                                if suffix.is_empty() { label } else { format!("{label}{suffix}") }
                            } else {
                                label
                            }
                        }}
                    }

                    {share_badge}
                    {shared_by_note}

                    // Sync status badge (store roots only, linked stores
                    // only) — reactive per store, no tree rebuild
                    // (docs/SYNC_CONTRACT.md "B: app side").
                    span {
                        style: {
                            move || {
                                let linked = sync_sig
                                    .map(|sig| sig.with(|(remote, _)| remote.is_some()))
                                    .unwrap_or(false);
                                if linked {
                                    "margin-left: 6px; cursor: default; font-size: 10px; font-weight: 400; \
                                     text-transform: none; letter-spacing: normal; opacity: 0.6;"
                                } else {
                                    "display: none;"
                                }
                            }
                        },
                        {move || {
                            // The mode is the store's own signal (decision 4),
                            // read here so the badge follows a new vault link.
                            let mode = store_sig
                                .map(|sig| sig.with(|s| s.sync_mode))
                                .unwrap_or_default();
                            sync_sig
                                .map(|sig| sig.with(|(_, state)| sync_badge_text(state, mode).to_string()))
                                .unwrap_or_default()
                        }}
                    }

                    {rename_input}
                }
            };

            // Why the items below are disabled, when some of them are: one
            // dimmed line at the top of the menu, rather than the same reason
            // repeated on every item or (worse) items greyed out saying
            // nothing. What is shared to read has one, and so has a node of
            // someone else's store that "Share..." is off for; an editor's
            // menu is otherwise a full menu (docs/NODE_DOCUMENT_CONTRACT.md
            // section 5).
            let access_note_text = crate::state::access_note(access_now);
            let menu_note_text = crate::state::menu_note_text(&[access_note_text, share_note_text]);
            let menu_note: Option<NodeHandle> = if menu_note_text.is_empty() {
                None
            } else {
                Some(rsx! { div { class: "pimble-menu-note", {menu_note_text} } })
            };
            let mount_menu_note: Option<NodeHandle> = if is_mount {
                let text = crate::state::menu_note_text(&[access_note_text, MOUNT_NOTE]);
                Some(rsx! { div { class: "pimble-menu-note", {text} } })
            } else {
                None
            };

            // Wrap in ContextMenu — different items for store roots vs nodes
            let context_menu = if is_store_root {
                rsx! {
                    ContextMenu {
                        ContextMenuTarget { {wrapper} }
                        ContextMenuDropdown {
                            {menu_note}
                            DropdownMenuItem {
                                left_section: TablerIcon::FilePlus,
                                disabled: !can_write_tree,
                                onclick: on_new_child,
                                "New Node"
                            }
                            if mounts_apply {
                                DropdownMenuItem {
                                    left_section: TablerIcon::Link,
                                    disabled: !can_write_tree,
                                    onclick: on_mount_store.clone(),
                                    "Mount Store..."
                                }
                            }
                            if CAN_ADMINISTER_STORES {
                                DropdownMenuItem {
                                    left_section: TablerIcon::CloudDownload,
                                    disabled: !can_write_tree,
                                    onclick: on_mount_remote_store.clone(),
                                    "Mount Remote Store Here..."
                                }
                            }
                            if mounts_apply {
                                DropdownMenuItem {
                                    left_section: TablerIcon::Copy,
                                    onclick: on_copy_as_mount_source.clone(),
                                    "Copy as Mount Source"
                                }
                            }
                            if mounts_apply {
                                DropdownMenuItem {
                                    left_section: TablerIcon::ClipboardCopy,
                                    disabled: no_mount_source || !can_write_tree,
                                    onclick: on_paste_mount.clone(),
                                    "Paste Mount Here"
                                }
                            }
                            if CAN_ADMINISTER_STORES {
                                DropdownMenuItem {
                                    left_section: TablerIcon::Cloud,
                                    disabled: is_linked_now,
                                    onclick: on_link_to_remote.clone(),
                                    "Link to Remote..."
                                }
                            }
                            if CAN_ADMINISTER_STORES {
                                DropdownMenuItem {
                                    left_section: TablerIcon::CloudUpload,
                                    disabled: is_linked_now || is_replica_now || is_vault_now,
                                    onclick: on_host_on_cloud.clone(),
                                    "Host on Pimble Cloud..."
                                }
                            }
                            if CAN_ADMINISTER_STORES {
                                DropdownMenuItem {
                                    left_section: TablerIcon::Share,
                                    disabled: share_disabled,
                                    onclick: on_share.clone(),
                                    "Share..."
                                }
                            }
                            if CAN_ADMINISTER_STORES {
                                DropdownMenuItem {
                                    left_section: TablerIcon::Unlink,
                                    disabled: !is_linked_now,
                                    onclick: on_unlink_from_remote.clone(),
                                    "Unlink from Remote"
                                }
                            }
                            if CAN_ADMINISTER_STORES {
                                DropdownMenuItem {
                                    left_section: TablerIcon::Trash,
                                    disabled: !is_replica_now,
                                    onclick: on_remove_replica.clone(),
                                    "Remove Replica..."
                                }
                            }
                            DropdownMenuItem {
                                left_section: TablerIcon::Palette,
                                disabled: !can_write_tree,
                                onclick: on_appearance,
                                "Appearance..."
                            }
                            if CAN_ADMINISTER_STORES {
                                DropdownMenuItem {
                                    left_section: TablerIcon::X,
                                    onclick: on_close_store.clone(),
                                    "Close Store"
                                }
                            }
                        }
                    }
                }
            } else if is_mount {
                rsx! {
                    ContextMenu {
                        ContextMenuTarget { {wrapper} }
                        ContextMenuDropdown {
                            {mount_menu_note}
                            DropdownMenuItem {
                                left_section: TablerIcon::FilePlus,
                                disabled: !can_write_mount_source,
                                onclick: on_new_child_mount,
                                "New Node"
                            }
                            if CAN_ADMINISTER_STORES {
                                DropdownMenuItem {
                                    left_section: TablerIcon::Share,
                                    disabled: true,
                                    "Share..."
                                }
                            }
                            DropdownMenuItem {
                                left_section: TablerIcon::Palette,
                                disabled: !can_write_tree,
                                onclick: on_appearance,
                                "Appearance..."
                            }
                            DropdownMenuItem {
                                left_section: TablerIcon::Trash,
                                disabled: !can_write_tree,
                                onclick: on_delete,
                                "Delete"
                            }
                        }
                    }
                }
            } else {
                // Regular nodes: new child, mount store, rename
                let nv_rename_ctx = nv_for_context_menu.clone();
                let on_rename = move || {
                    if let Some((s_id, Some(n_id))) = parse_tree_value(&nv_rename_ctx) {
                        let edit_text = store.get_node_signal(s_id, n_id)
                            .map(|sig| sig.with(|n| n.metadata.title.clone()))
                            .unwrap_or_default();
                        store.rename_text.set(edit_text);
                        store.renaming_node.set(Some(nv_rename_ctx.clone()));
                        let nv_interceptor = nv_rename_ctx.clone();
                        set_keyboard_interceptor(move |data| {
                            if data.key == "Escape" {
                                run_on_main_thread(move || {
                                    store.renaming_node.set(None);
                                    clear_keyboard_interceptor();
                                });
                                return true;
                            }
                            if data.key == "Enter" {
                                let nv = nv_interceptor.clone();
                                run_on_main_thread(move || {
                                    let new_title = untracked(|| store.rename_text.get());
                                    store.renaming_node.set(None);
                                    clear_keyboard_interceptor();
                                    if let Some((s_id, Some(n_id))) = parse_tree_value(&nv) {
                                        store.send(BackendCommand::RenameNode {
                                            store_id: s_id, node_id: n_id, title: new_title,
                                        });
                                    }
                                });
                                return true;
                            }
                            false
                        });
                        rename_input_for_focus.focus();
                    }
                };

                rsx! {
                    ContextMenu {
                        ContextMenuTarget { {wrapper} }
                        ContextMenuDropdown {
                            {menu_note}
                            DropdownMenuItem {
                                left_section: TablerIcon::FilePlus,
                                disabled: !can_write_tree,
                                onclick: on_new_child,
                                "New Node"
                            }
                            if mounts_apply {
                                DropdownMenuItem {
                                    left_section: TablerIcon::Link,
                                    disabled: !can_write_tree,
                                    onclick: on_mount_store.clone(),
                                    "Mount Store..."
                                }
                            }
                            if CAN_ADMINISTER_STORES {
                                DropdownMenuItem {
                                    left_section: TablerIcon::CloudDownload,
                                    disabled: !can_write_tree,
                                    onclick: on_mount_remote_store.clone(),
                                    "Mount Remote Store Here..."
                                }
                            }
                            if mounts_apply {
                                DropdownMenuItem {
                                    left_section: TablerIcon::Copy,
                                    onclick: on_copy_as_mount_source.clone(),
                                    "Copy as Mount Source"
                                }
                            }
                            if mounts_apply {
                                DropdownMenuItem {
                                    left_section: TablerIcon::ClipboardCopy,
                                    disabled: no_mount_source || !can_write_tree,
                                    onclick: on_paste_mount.clone(),
                                    "Paste Mount Here"
                                }
                            }
                            DropdownMenuItem {
                                left_section: TablerIcon::Edit,
                                disabled: !can_write_tree,
                                onclick: on_rename,
                                "Rename"
                            }
                            if CAN_ADMINISTER_STORES {
                                DropdownMenuItem {
                                    left_section: TablerIcon::Share,
                                    disabled: share_disabled,
                                    onclick: on_share.clone(),
                                    "Share..."
                                }
                            }
                            DropdownMenuItem {
                                left_section: TablerIcon::Palette,
                                disabled: !can_write_tree,
                                onclick: on_appearance,
                                "Appearance..."
                            }
                            DropdownMenuItem {
                                left_section: TablerIcon::Trash,
                                disabled: !can_write_tree,
                                onclick: on_delete,
                                "Delete"
                            }
                        }
                    }
                }
            };

            // The ContextMenu sets data-oncontextmenu on its ContextMenuTarget,
            // but both rinch-context-menu and rinch-context-menu__target have
            // display:contents — they're invisible to hit testing. Wrap in a
            // visible div that carries:
            //  - context menu handler (hoisted from ContextMenuTarget)
            //  - drag-and-drop attributes (draggable, ondragstart, etc.)
            //  - visual feedback for drag state (opacity, highlight)
            // This ensures right-click, drag, and drop all work on the full row.
            let hit_area = rsx! {
                div {
                    style: {
                        let nv = nv_effect.clone();
                        move || {
                            let base = "flex: 1; display: flex; align-items: center; border-radius: 2px; transition: background 0.1s, opacity 0.1s;";
                            let is_being_dragged = drag_ctx.get().as_deref() == Some(&nv);
                            let is_target = store.drop_target.get().as_deref() == Some(&nv)
                                && drag_ctx.is_active();
                            if is_being_dragged {
                                format!("{} opacity: 0.3;", base)
                            } else if is_target {
                                format!("{} background: var(--rinch-color-blue-1); outline: 1px solid var(--rinch-color-blue-4);", base)
                            } else {
                                base.to_string()
                            }
                        }
                    },
                    draggable: draggable,
                    ondragstart: on_dragstart,
                    ondragend: on_dragend,
                    ondrop: on_drop,
                    ondragenter: on_dragenter,
                    ondragleave: on_dragleave,
                    {context_menu}
                }
            };
            hoist_context_menu_handler(&hit_area, &hit_area);
            hit_area
        });

        let tree_scroll = rsx! {
            div {
                class: "pimble-sidebar__tree",
                Tree {
                    data: untracked(|| store.build_tree_data_structural()),
                    tree: tree_state,
                    data_source: data_source,
                    level_offset: "xs",
                    select_on_click: true,
                    expand_on_click: false,
                    onselect: on_tree_select,
                    onexpand: on_tree_expand,
                    oncollapse: on_tree_collapse,
                    render_node: render_node_fn,
                }
            }
        };

        // Toolbar button handlers — new node (in selected store, or first store)
        let on_new_node = move || {
            // Try to use the currently selected store
            let result = store.selected_id.get()
                .and_then(|sel| parse_tree_value(&sel))
                .and_then(|(s_id, _)| {
                    store.root_node_id(s_id).map(|rid| (s_id, rid))
                })
                // Fall back to first store
                .or_else(|| {
                    let first_sid = untracked(|| store.store_ids.with(|ids| ids.first().copied()));
                    first_sid.and_then(|sid| {
                        store.root_node_id(sid).map(|rid| (sid, rid))
                    })
                })
                // Not under a node this device may only read
                // (docs/NODE_DOCUMENT_CONTRACT.md section 5): the row's "New
                // Node" is disabled there, and so is this.
                .filter(|(s_id, root_id)| store.node_access(*s_id, *root_id).allows_write());
            match result {
                Some((s_id, root_id)) => store.send(BackendCommand::CreateNode {
                    store_id: s_id,
                    parent_id: Some(root_id),
                    title: String::new(),
                }),
                // Nothing to create a node under. On the desktop "New
                // Store..." is a menu item away; a browser has no menu bar, so
                // "+" offers the store instead of doing nothing at all.
                None if CAN_CREATE_HOSTED_STORES => open_new_store_modal(store),
                None => {}
            }
        };

        // Rich text editor (the rinch `Editor {}` component over the shared
        // thread-local `EditorHandle`). Collaboration is wired in `start_editing`:
        // local edits broadcast their deltas through the server relay, and remote
        // deltas arrive via `BackendEvent::RemoteChanges`. There is no manual
        // autosave — the collab session persists edits live (the server applies
        // and relays each delta).
        let editor_view = rsx! {
            Editor { editor: crate::editor::editor() }
        };

        let toolbar_handle = crate::toolbar::render_pimble_toolbar(__scope);

        // Whether the document in the pane is one this device may only read
        // (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles"): its store is
        // held to read, or the server judged this node so (a reader's root
        // in a store where other roots are edited) — `AppStore::node_access`,
        // read here with tracking. `active_edit` holds the node's CANONICAL
        // pair — where the edit would be written, even when the node was
        // reached through a mount — so that is what decides. Reactive on the
        // active node, on its store's own signal and on the node's, so the
        // pane follows an access that arrives or changes. The toolbar gives
        // way to a line saying so; `editor::reject_local_edit` is what
        // happens if someone types anyway.
        //
        // The store's own signal comes out of the registry first and is read
        // after that borrow is released: rinch keeps every signal in one
        // `RefCell`, and a *tracked* read takes it mutably to record the
        // subscription — so reading one signal inside another's `with` panics.
        // (`AppStore`'s own helpers nest freely because they are `untracked`.)
        let editor_read_only = move || -> bool {
            let Some(active) = store.active_edit.get() else { return false };
            let sig = store.store_data.with(|map| map.get(&active.store_id).copied());
            if sig.map_or(false, |sig| sig.with(|s| !s.access.allows_write())) {
                return true;
            }
            let node_sig = store.node_data.with(|map| map.get(&(active.store_id, active.node_id)).copied());
            node_sig.map_or(false, |sig| sig.with(|n| !n.access.allows_write()))
        };

        // Editor empty state icon
        let empty_icon = render_tabler_icon(__scope, TablerIcon::FileText, TablerIconStyle::Outline);

        // Connection status color (pure derivation → Memo)
        let status_color = Memo::new(move || {
            let status = store.connection_status.get();
            if status == "Connected" {
                "green".to_string()
            } else if status.starts_with("Error") {
                "red".to_string()
            } else {
                "yellow".to_string()
            }
        });

        // ── Search box + results panel ────────────────────────────────
        // The results panel replaces the tree while `search_query` is
        // non-empty; Esc (via `clear_search`'s interceptor) restores it.
        let search_icon = render_tabler_icon(__scope, TablerIcon::Search, TablerIconStyle::Outline);
        let search_input = rsx! {
            input {
                r#type: "text",
                class: "pimble-search-bar__input",
                placeholder: "Search... (Ctrl+K)",
                // Raw elements have no special "value_fn" prop — that's a
                // `TextInput`-component-only field. On a raw `<input>` any
                // closure-valued prop just becomes a reactively-set attribute
                // named literally after the prop, so `value_fn` was writing a
                // phantom `value_fn` attribute the renderer never reads
                // (`paint_input_value` only reads `"value"`). Bind `value`
                // itself so clearing the signal actually clears the box.
                value: move || store.search_query.get(),
                oninput: move |val: String| {
                    let was_empty = untracked(|| store.search_query.get()).is_empty();
                    let now_empty = val.is_empty();
                    store.search_query.set(val.clone());
                    if was_empty && !now_empty {
                        // Only while the box is non-empty — see `clear_search`.
                        set_keyboard_interceptor(move |data| {
                            if data.key == "Escape" {
                                run_on_main_thread(move || clear_search(store));
                                return true;
                            }
                            false
                        });
                    } else if !was_empty && now_empty {
                        clear_keyboard_interceptor();
                    }
                    schedule_search(store, val);
                },
                onsubmit: move || {
                    let first = untracked(|| match store.search_results.get() {
                        SearchState::Results(items) => items.into_iter().next(),
                        _ => None,
                    });
                    if let Some(item) = first {
                        let value = format!("node_{}_{}", item.store_id, item.node_id);
                        clear_search(store);
                        open_node(store, tree_state, value);
                    }
                },
            }
        };
        SEARCH_INPUT.with(|cell| { *cell.borrow_mut() = Some(search_input.clone()); });

        let search_bar = rsx! {
            div {
                class: "pimble-search-bar",
                span { class: "pimble-search-bar__icon", {search_icon} }
                {search_input}
                if !store.search_query.get().is_empty() {
                    ActionIcon {
                        icon: TablerIcon::X,
                        variant: "subtle",
                        size: "xs",
                        class: "pimble-search-bar__clear",
                        onclick: move || clear_search(store),
                    }
                }
            }
        };

        let search_panel = rsx! {
            div {
                class: "pimble-search-results",
                match store.search_results.get() {
                    SearchState::Idle => "",
                    SearchState::Error(_message) => div {
                        class: "pimble-search-results__message", {_message}
                    },
                    SearchState::Results(items) if items.is_empty() => div {
                        class: "pimble-search-results__message", "No results"
                    },
                    SearchState::Results(_items) => div {
                        for row in search_rows(store, _items.clone()) {
                            div {
                                key: row.key.clone(),
                                class: "pimble-search-result",
                                onclick: {
                                    let value = row.value.clone();
                                    move || {
                                        clear_search(store);
                                        open_node(store, tree_state, value.clone());
                                    }
                                },
                                div {
                                    class: "pimble-search-result__title-row",
                                    span { class: "pimble-search-result__title", {row.title.clone()} }
                                    if !row.kind.is_empty() {
                                        span { class: "pimble-search-result__kind", {row.kind} }
                                    }
                                }
                                div { class: "pimble-search-result__store", {row.store_name.clone()} }
                                div { class: "pimble-search-result__snippet", {row.snippet.clone()} }
                            }
                        }
                    }
                }
            }
        };

        // ── "Add Remote Store..." modal (File menu) ─────────────────────
        // docs/SYNC_CONTRACT.md "B: app side". No folder/path field: the
        // server places the replica in its own data directory, so nothing
        // in this flow opens a native OS dialog (the rinch debug tools can
        // drive it end to end).
        //
        // The same modal serves "Mount Remote Store Here..." when
        // `connect_modal_target` names a target node: its action adds the
        // replica (unless the store is already open here) and then mounts it
        // there, in one command (docs/history/REMOTE_MOUNTS_CONTRACT.md decision 9).
        let connect_modal = rsx! {
            Modal {
                opened_fn: move || store.connect_modal_open.get(),
                onclose: move || {
                    store.connect_modal_open.set(false);
                    store.connect_modal_error.set(String::new());
                    store.connect_modal_target.set(None);
                },
                title: {|| if store.connect_modal_target.get().is_some() {
                    "Mount Remote Store"
                } else {
                    "Add Remote Store"
                }},
                size: "sm",

                div {
                    style: "display: flex; flex-direction: column; gap: 10px;",

                    TextInput {
                        label: "Server URL",
                        placeholder: "http://host:7462",
                        value_fn: move || store.connect_modal_url.get(),
                        oninput: move |val: String| store.connect_modal_url.set(val),
                    }

                    PasswordInput {
                        label: "Token",
                        placeholder: "Leave empty to use a saved token",
                        value_fn: move || store.connect_modal_token.get(),
                        oninput: move |val: String| store.connect_modal_token.set(val),
                        visible_fn: move || store.connect_modal_token_visible.get(),
                        ontoggle: move || store.connect_modal_token_visible.update(|v| *v = !*v),
                    }

                    Button {
                        variant: "light",
                        size: "sm",
                        disabled: {|| store.connect_modal_busy.get()},
                        onclick: move || {
                            let url = untracked(|| store.connect_modal_url.get());
                            let token = untracked(|| store.connect_modal_token.get());
                            store.connect_modal_busy.set(true);
                            store.connect_modal_error.set(String::new());
                            store.send(BackendCommand::ListRemoteStores { url, token });
                        },
                        "List stores"
                    }

                    Select {
                        label: "Store",
                        placeholder: "Select a store",
                        value_fn: move || store.connect_modal_selected.get(),
                        onchange: move |val: String| store.connect_modal_selected.set(val),
                        data: {|| {
                            store.connect_modal_stores.get().iter()
                                .map(|s| SelectOption::new(s.id.to_string(), s.name.clone()))
                                .collect::<Vec<_>>()
                        }},
                    }

                    Button {
                        variant: "filled",
                        size: "sm",
                        disabled: {|| store.connect_modal_busy.get() || store.connect_modal_selected.get().is_empty()},
                        onclick: move || {
                            let selected = untracked(|| store.connect_modal_selected.get());
                            let Ok(remote_uuid) = selected.parse::<uuid::Uuid>() else {
                                store.connect_modal_error.set("Select a store first".to_string());
                                return;
                            };
                            let remote_store_id = pimble_core::StoreId(remote_uuid);
                            let url = untracked(|| store.connect_modal_url.get());
                            let token = untracked(|| store.connect_modal_token.get());
                            // Both modes report their outcome through the same
                            // route: `connect_modal_pending_add` claims the
                            // generic `Error` event for this modal's error line.
                            store.connect_modal_pending_add.set(true);
                            store.connect_modal_busy.set(true);
                            store.connect_modal_error.set(String::new());
                            match untracked(|| store.connect_modal_target.get()) {
                                Some((target_store_id, target_parent_id)) => {
                                    store.send(BackendCommand::MountRemoteStore {
                                        url,
                                        remote_store_id,
                                        token,
                                        target_store_id,
                                        target_parent_id,
                                    });
                                }
                                None => {
                                    store.send(BackendCommand::AddRemoteStore { url, remote_store_id, token });
                                }
                            }
                        },
                        {|| if store.connect_modal_target.get().is_some() { "Mount" } else { "Add" }}
                    }

                    if !store.connect_modal_error.get().is_empty() {
                        div {
                            style: "color: var(--rinch-color-red-6); font-size: 12px;",
                            {|| store.connect_modal_error.get()}
                        }
                    }
                }
            }
        };

        // ── "Link to Remote..." modal (store root context menu) ─────────
        // docs/SYNC_CONTRACT.md "B: app side".
        let link_modal = rsx! {
            Modal {
                opened_fn: move || store.link_modal_store.get().is_some(),
                onclose: move || {
                    store.link_modal_store.set(None);
                    store.link_modal_error.set(String::new());
                },
                title: "Link to Remote",
                size: "sm",

                div {
                    style: "display: flex; flex-direction: column; gap: 10px;",

                    TextInput {
                        label: "Server URL",
                        placeholder: "http://host:7462",
                        value_fn: move || store.link_modal_url.get(),
                        oninput: move |val: String| store.link_modal_url.set(val),
                    }

                    PasswordInput {
                        label: "Token",
                        placeholder: "Leave empty to use a saved token",
                        value_fn: move || store.link_modal_token.get(),
                        oninput: move |val: String| store.link_modal_token.set(val),
                        visible_fn: move || store.link_modal_token_visible.get(),
                        ontoggle: move || store.link_modal_token_visible.update(|v| *v = !*v),
                    }

                    Button {
                        variant: "filled",
                        size: "sm",
                        disabled: {|| store.link_modal_pending.get()},
                        onclick: move || {
                            let Some(store_id) = untracked(|| store.link_modal_store.get()) else { return };
                            let url_str = untracked(|| store.link_modal_url.get());
                            let Ok(url) = url_str.parse::<url::Url>() else {
                                store.link_modal_error.set("Invalid URL".to_string());
                                return;
                            };
                            let token = untracked(|| store.link_modal_token.get());
                            let auth = if token.is_empty() {
                                pimble_core::AuthMethod::None
                            } else {
                                pimble_core::AuthMethod::Bearer { token }
                            };
                            store.link_modal_pending.set(true);
                            store.link_modal_error.set(String::new());
                            store.send(BackendCommand::SetStoreSync {
                                store_id,
                                remote: Some(pimble_core::RemoteEndpoint { url, auth }),
                            });
                        },
                        "Link"
                    }

                    if !store.link_modal_error.get().is_empty() {
                        div {
                            style: "color: var(--rinch-color-red-6); font-size: 12px;",
                            {|| store.link_modal_error.get()}
                        }
                    }
                }
            }
        };

        // ── "Remove Replica..." confirmation modal (store root context
        // menu) ──────────────────────────────────────────────────────────
        // docs/history/HARDENING_CONTRACT.md "B: app". Names the store and its
        // remote; when the link isn't `Synced`, warns before removing with
        // `force`.
        // ── "Appearance..." modal (node context menu) ───────────────────
        // Picks a colour and an icon for one node; every click applies at
        // once through `SetNodeAppearance`, and the selection shown follows
        // the node's own signal, so a change from another window shows too.
        let appearance_node_color = move || -> Option<String> {
            let (s_id, n_id) = store.appearance_modal_node.get()?;
            store.get_node_signal(s_id, n_id)?.with(|n| n.metadata.color().map(String::from))
        };
        let appearance_node_icon = move || -> Option<String> {
            let (s_id, n_id) = store.appearance_modal_node.get()?;
            store.get_node_signal(s_id, n_id)?.with(|n| n.metadata.icon().map(String::from))
        };
        let send_appearance = move |icon: Option<Option<String>>, color: Option<Option<String>>| {
            if let Some((store_id, node_id)) = untracked(|| store.appearance_modal_node.get()) {
                store.send(BackendCommand::SetNodeAppearance { store_id, node_id, icon, color, tags: None });
            }
        };
        let send_tags = move || {
            if let Some((store_id, node_id)) = untracked(|| store.appearance_modal_node.get()) {
                let tags: Vec<String> = untracked(|| store.appearance_tags_text.get())
                    .split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect();
                store.send(BackendCommand::SetNodeAppearance { store_id, node_id, icon: None, color: None, tags: Some(tags) });
            }
        };
        let swatches: Vec<NodeHandle> = COLOR_CHOICES
            .iter()
            .map(|&(name, hex)| {
                rsx! {
                    div {
                        class: {move || {
                            if appearance_node_color().as_deref() == Some(hex) { "pimble-swatch pimble-swatch--active" } else { "pimble-swatch" }
                        }},
                        style: format!("background: {hex};"),
                        title: name,
                        onclick: move || send_appearance(None, Some(Some(hex.to_string()))),
                    }
                }
            })
            .collect();
        let appearance_modal = rsx! {
            Modal {
                opened_fn: move || store.appearance_modal_node.get().is_some(),
                onclose: move || store.appearance_modal_node.set(None),
                title: "Appearance",
                size: "md",

                div {
                    div { class: "pimble-appearance__label", "Colour" }
                    div {
                        class: "pimble-appearance__row",
                        div {
                            class: {move || {
                                if appearance_node_color().is_none() { "pimble-swatch pimble-swatch--none pimble-swatch--active" } else { "pimble-swatch pimble-swatch--none" }
                            }},
                            onclick: move || send_appearance(None, Some(None)),
                            "None"
                        }
                        {swatches}
                    }
                    div { class: "pimble-appearance__label", "Icon" }
                    TextInput {
                        placeholder: "Search all icons...",
                        value_fn: move || store.appearance_icon_query.get(),
                        oninput: move |val: String| store.appearance_icon_query.set(val),
                    }
                    div {
                        class: "pimble-appearance__row",
                        style: "margin-top: 6px;",
                        div {
                            class: {move || {
                                if appearance_node_icon().is_none() { "pimble-swatch pimble-swatch--none pimble-swatch--active" } else { "pimble-swatch pimble-swatch--none" }
                            }},
                            onclick: move || send_appearance(Some(None), None),
                            "Default"
                        }
                        // The curated set, or every Tabler icon matching the
                        // search: reactive on the query, one component per icon.
                        for icon in icons_matching(&store.appearance_icon_query.get()) {
                            div {
                                key: icon.name(),
                                class: {move || {
                                    if appearance_node_icon().as_deref() == Some(icon.name()) { "pimble-icon-choice pimble-icon-choice--active" } else { "pimble-icon-choice" }
                                }},
                                title: icon.name(),
                                onclick: move || send_appearance(Some(Some(icon.name().to_string())), None),
                                IconGlyph { name: icon.name() }
                            }
                        }
                    }
                    div { class: "pimble-appearance__label", "Tags (comma separated)" }
                    TextInput {
                        placeholder: "e.g. Important, Tax",
                        value_fn: move || store.appearance_tags_text.get(),
                        oninput: move |val: String| store.appearance_tags_text.set(val),
                        onsubmit: move || send_tags(),
                    }
                    div {
                        style: "margin-top: 14px; display: flex; justify-content: flex-end;",
                        Button {
                            variant: "filled",
                            onclick: move || {
                                send_tags();
                                store.appearance_modal_node.set(None);
                            },
                            "Done"
                        }
                    }
                }
            }
        };

        let create_hosted_store = move || {
            let name = untracked(|| store.new_store_modal_name.get()).trim().to_string();
            if name.is_empty() {
                store.new_store_modal_error.set("Give the store a name.".to_string());
                return;
            }
            store.new_store_modal_error.set(String::new());
            store.new_store_modal_pending.set(true);
            store.send(BackendCommand::CreateHostedStore { name, kind: "vault".to_string() });
        };

        // ── "New Store..." modal ────────────────────────────────────────
        // A browser build's stores live on the account, not on disk, so there
        // is no path to pick: a name and a kind is the whole of it. The
        // backend makes the store, mints its key if it is encrypted, and asks
        // for a token that carries the new grant; the store then arrives
        // through `StoresListed` like any other.
        let new_store_modal = rsx! {
            Modal {
                opened_fn: move || store.new_store_modal_open.get(),
                onclose: move || {
                    store.new_store_modal_open.set(false);
                    store.new_store_modal_error.set(String::new());
                },
                title: "New Store",
                size: "sm",

                div {
                    style: "display: flex; flex-direction: column; gap: 10px;",

                    TextInput {
                        label: "Name",
                        placeholder: "Notes",
                        value_fn: move || store.new_store_modal_name.get(),
                        oninput: move |val: String| store.new_store_modal_name.set(val),
                        onsubmit: create_hosted_store,
                    }

                    div {
                        style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                        "Encrypted. Its key is made here and sealed to your account; \
                         the server stores what it cannot read."
                    }

                    div {
                        style: {
                            move || if store.new_store_modal_error.get().is_empty() {
                                "display: none;"
                            } else {
                                "color: var(--rinch-color-red-6); font-size: 12px;"
                            }
                        },
                        {|| store.new_store_modal_error.get()}
                    }

                    Button {
                        variant: "filled",
                        size: "sm",
                        loading: {|| store.new_store_modal_pending.get()},
                        disabled: {|| store.new_store_modal_pending.get()},
                        onclick: create_hosted_store,
                        "Create"
                    }
                }
            }
        };

        // ── "Mount Store..." picker ─────────────────────────────────────
        // The desktop picks a directory on this machine. A browser has none to
        // pick from: its stores live on the account, so it picks one of those.
        // Either way the RPC is `createMount`, which an editor may call — the
        // picker is a different way to name the same thing, not a lesser one.
        let do_mount_store = move || {
            let Some((target_store, target_parent)) = untracked(|| store.mount_picker_target.get())
            else {
                return;
            };
            let selected = untracked(|| store.mount_picker_selected.get());
            let Ok(uuid) = selected.parse::<uuid::Uuid>() else {
                store.mount_picker_error.set("Choose a store to mount.".to_string());
                return;
            };
            let source_store_id = pimble_core::StoreId(uuid);
            // A store's root is what gets mounted; the source keeps its own name.
            let Some(source_root) = store.root_node_id(source_store_id) else {
                store.mount_picker_error.set("That store has not finished opening.".to_string());
                return;
            };
            let title = store
                .get_store_signal(source_store_id)
                .map(|sig| untracked(|| sig.with(|s| s.name.clone())));

            store.mount_picker_error.set(String::new());
            store.mount_picker_pending.set(true);
            store.send(BackendCommand::CreateMount {
                store_id: target_store,
                parent_id: target_parent,
                source_store_id,
                source_node_id: source_root,
                title,
            });
        };

        let mount_picker_modal = rsx! {
            Modal {
                opened_fn: move || store.mount_picker_target.get().is_some(),
                onclose: move || {
                    store.mount_picker_target.set(None);
                    store.mount_picker_error.set(String::new());
                },
                title: "Mount Store",
                size: "sm",

                div {
                    style: "display: flex; flex-direction: column; gap: 10px;",

                    Select {
                        label: "Store",
                        placeholder: "Choose a store",
                        value_fn: move || store.mount_picker_selected.get(),
                        onchange: move |val: String| store.mount_picker_selected.set(val),
                        data: {|| {
                            // Everything open except the one being mounted into:
                            // a store cannot contain itself.
                            let target = store.mount_picker_target.get().map(|(sid, _)| sid);
                            store.store_ids.get().iter()
                                .filter(|sid| Some(**sid) != target)
                                .filter_map(|sid| {
                                    store.get_store_signal(*sid)
                                        .map(|sig| sig.with(|s| SelectOption::new(sid.to_string(), s.name.clone())))
                                })
                                .collect::<Vec<_>>()
                        }},
                    }

                    div {
                        style: {
                            move || if store.mount_picker_error.get().is_empty() {
                                "display: none;"
                            } else {
                                "color: var(--rinch-color-red-6); font-size: 12px;"
                            }
                        },
                        {|| store.mount_picker_error.get()}
                    }

                    Button {
                        variant: "filled",
                        size: "sm",
                        loading: {|| store.mount_picker_pending.get()},
                        disabled: {|| store.mount_picker_pending.get() || store.mount_picker_selected.get().is_empty()},
                        onclick: do_mount_store,
                        "Mount"
                    }
                }
            }
        };

        let remove_replica_modal = rsx! {
            Modal {
                opened_fn: move || store.remove_replica_modal_store.get().is_some(),
                onclose: move || {
                    store.remove_replica_modal_store.set(None);
                    store.remove_replica_modal_error.set(String::new());
                },
                title: "Remove Replica",
                size: "sm",

                div {
                    style: "display: flex; flex-direction: column; gap: 10px;",

                    div {
                        {|| {
                            let Some(store_id) = store.remove_replica_modal_store.get() else {
                                return String::new();
                            };
                            let name = store.get_store_signal(store_id)
                                .map(|sig| sig.with(|s| s.name.clone()))
                                .unwrap_or_default();
                            let remote_url = store.get_sync_signal(store_id)
                                .and_then(|sig| sig.with(|(remote, _)| {
                                    remote.as_ref().map(|r| r.url.as_str().trim_end_matches('/').to_string())
                                }));
                            match remote_url {
                                Some(url) => format!("Remove the local copy of \"{}\"? The store stays on {}.", name, url),
                                None => format!("Remove the local copy of \"{}\"?", name),
                            }
                        }}
                    }

                    div {
                        style: {
                            move || {
                                let unsynced = store.remove_replica_modal_store.get()
                                    .and_then(|sid| store.get_sync_signal(sid))
                                    .map(|sig| sig.with(|(_, state)| {
                                        !matches!(state, pimble_core::SyncState::Synced { .. })
                                    }))
                                    .unwrap_or(false);
                                if unsynced {
                                    "color: var(--rinch-color-yellow-6); font-size: 12px;"
                                } else {
                                    "display: none;"
                                }
                            }
                        },
                        "This replica is not synced right now. Anything changed here since it last synced will be lost."
                    }

                    Button {
                        variant: "filled",
                        color: "red",
                        size: "sm",
                        disabled: {|| store.remove_replica_modal_pending.get()},
                        onclick: move || {
                            let Some(store_id) = untracked(|| store.remove_replica_modal_store.get()) else { return };
                            let force = store.get_sync_signal(store_id)
                                .map(|sig| untracked(|| sig.with(|(_, state)| {
                                    !matches!(state, pimble_core::SyncState::Synced { .. })
                                })))
                                .unwrap_or(false);
                            store.remove_replica_modal_pending.set(true);
                            store.remove_replica_modal_error.set(String::new());
                            store.send(BackendCommand::RemoveReplica { store_id, force });
                        },
                        "Remove"
                    }

                    if !store.remove_replica_modal_error.get().is_empty() {
                        div {
                            style: "color: var(--rinch-color-red-6); font-size: 12px;",
                            {|| store.remove_replica_modal_error.get()}
                        }
                    }
                }
            }
        };

        // ── "Account..." modal ──────────────────────────────────────────
        // docs/DESKTOP_ACCOUNT_CONTRACT.md decision 1: one modal, two faces.
        // The sign-in form while nothing is signed in; the account and a
        // "Sign Out" button once something is. Both outcomes arrive as
        // `CloudStatusChanged`, which flips the face and leaves the modal
        // open so the person sees it worked.
        let sign_in = move || {
            let url = untracked(|| store.account_modal_url.get()).trim().to_string();
            let email = untracked(|| store.account_modal_email.get()).trim().to_string();
            let password = untracked(|| store.account_modal_password.get());
            if url.is_empty() || email.is_empty() || password.is_empty() {
                store.account_modal_error.set("Fill in the service URL, email and password.".to_string());
                return;
            }
            store.account_modal_error.set(String::new());
            store.account_modal_busy.set(true);
            store.send(BackendCommand::CloudSignIn { url, email, password });
        };
        let account_modal = rsx! {
            Modal {
                opened_fn: move || store.account_modal_open.get(),
                onclose: move || {
                    store.account_modal_open.set(false);
                    store.account_modal_error.set(String::new());
                    store.account_modal_password.set(String::new());
                    store.account_modal_hint.set(String::new());
                },
                title: "Account",
                size: "sm",

                div {
                    style: "display: flex; flex-direction: column; gap: 10px;",

                    if !store.account_modal_hint.get().is_empty() {
                        div {
                            style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                            {|| store.account_modal_hint.get()}
                        }
                    }

                    // The sign-in face.
                    if !store.cloud_signed_in.get() {
                        div {
                            style: "display: flex; flex-direction: column; gap: 10px;",

                            TextInput {
                                label: "Service URL",
                                placeholder: "https://pimble.app",
                                value_fn: move || store.account_modal_url.get(),
                                oninput: move |val: String| store.account_modal_url.set(val),
                            }

                            TextInput {
                                label: "Email",
                                placeholder: "you@example.com",
                                value_fn: move || store.account_modal_email.get(),
                                oninput: move |val: String| store.account_modal_email.set(val),
                            }

                            PasswordInput {
                                label: "Password",
                                value_fn: move || store.account_modal_password.get(),
                                oninput: move |val: String| store.account_modal_password.set(val),
                                visible_fn: move || store.account_modal_password_visible.get(),
                                ontoggle: move || store.account_modal_password_visible.update(|v| *v = !*v),
                            }

                            Button {
                                variant: "filled",
                                size: "sm",
                                loading: {|| store.account_modal_busy.get()},
                                disabled: {|| store.account_modal_busy.get()},
                                onclick: sign_in,
                                "Sign In"
                            }
                        }
                    }

                    // The signed-in face.
                    if store.cloud_signed_in.get() {
                        div {
                            style: "display: flex; flex-direction: column; gap: 10px;",

                            div {
                                {|| format!("Signed in as {}", store.cloud_email.get())}
                            }
                            div {
                                style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                                {|| store.cloud_url.get()}
                            }

                            Button {
                                variant: "light",
                                size: "sm",
                                loading: {|| store.account_modal_busy.get()},
                                disabled: {|| store.account_modal_busy.get()},
                                onclick: move || {
                                    store.account_modal_error.set(String::new());
                                    store.account_modal_busy.set(true);
                                    store.send(BackendCommand::CloudSignOut);
                                },
                                "Sign Out"
                            }
                        }
                    }

                    if !store.account_modal_error.get().is_empty() {
                        div {
                            style: "color: var(--rinch-color-red-6); font-size: 12px;",
                            {|| store.account_modal_error.get()}
                        }
                    }
                }
            }
        };

        // ── "Host on Pimble Cloud..." confirmation (store root context
        // menu) ─────────────────────────────────────────────────────────
        // decision 5: names the store and the account, then `CloudHostStore`.
        // `CloudStoreHosted` closes it; a `CloudError { HostStore }` lands
        // on its error line.
        let host_modal = rsx! {
            Modal {
                opened_fn: move || store.host_modal_store.get().is_some(),
                onclose: move || {
                    store.host_modal_store.set(None);
                    store.host_modal_error.set(String::new());
                },
                title: "Host on Pimble Cloud",
                size: "sm",

                div {
                    style: "display: flex; flex-direction: column; gap: 10px;",

                    div {
                        {|| {
                            let Some(store_id) = store.host_modal_store.get() else {
                                return String::new();
                            };
                            let name = store.get_store_signal(store_id)
                                .map(|sig| sig.with(|s| s.name.clone()))
                                .unwrap_or_default();
                            format!("Host \"{}\" on Pimble Cloud as {}?", name, store.cloud_email.get())
                        }}
                    }

                    div {
                        style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                        "Encrypted before it leaves this machine. Its key is made here and \
                         sealed to your account; the server stores what it cannot read."
                    }

                    if !store.host_modal_error.get().is_empty() {
                        div {
                            style: "color: var(--rinch-color-red-6); font-size: 12px;",
                            {|| store.host_modal_error.get()}
                        }
                    }

                    div {
                        style: "display: flex; justify-content: flex-end; gap: 8px;",
                        Button {
                            variant: "light",
                            size: "sm",
                            disabled: {|| store.host_modal_busy.get()},
                            onclick: move || {
                                store.host_modal_store.set(None);
                                store.host_modal_error.set(String::new());
                            },
                            "Cancel"
                        }
                        Button {
                            variant: "filled",
                            size: "sm",
                            loading: {|| store.host_modal_busy.get()},
                            disabled: {|| store.host_modal_busy.get()},
                            onclick: move || {
                                let Some(store_id) = untracked(|| store.host_modal_store.get()) else { return };
                                store.host_modal_error.set(String::new());
                                store.host_modal_busy.set(true);
                                store.send(BackendCommand::CloudHostStore { store_id });
                            },
                            "Host"
                        }
                    }
                }
            }
        };

        // ── "Add Hosted Store..." modal (Account menu) ──────────────────
        // decision 6: the account's encrypted stores and shares not already
        // open here (the event handler filters `CloudHostedStoresListed`),
        // one of which becomes a local replica — a whole one for a store of
        // one's own, a partial one for a share. The store arrives as
        // `StoreOpened`, which closes the modal and carries the roots this
        // device now holds (docs/NODE_DOCUMENT_CONTRACT.md section 5).
        let add_hosted_store = move || {
            let selected = untracked(|| store.hosted_modal_selected.get());
            let Ok(uuid) = selected.parse::<uuid::Uuid>() else {
                store.hosted_modal_error.set("Select a store first".to_string());
                return;
            };
            store.hosted_modal_error.set(String::new());
            store.hosted_modal_busy.set(true);
            store.send(BackendCommand::CloudAddHostedStore { store_id: pimble_core::StoreId(uuid) });
        };
        let hosted_modal = rsx! {
            Modal {
                opened_fn: move || store.hosted_modal_open.get(),
                onclose: move || {
                    store.hosted_modal_open.set(false);
                    store.hosted_modal_error.set(String::new());
                },
                title: "Add Hosted Store",
                size: "sm",

                div {
                    style: "display: flex; flex-direction: column; gap: 10px;",

                    // Nothing to list without an account.
                    if !store.cloud_signed_in.get() {
                        div {
                            style: "display: flex; flex-direction: column; gap: 10px;",
                            div { "Sign in first." }
                            Button {
                                variant: "filled",
                                size: "sm",
                                onclick: move || {
                                    store.hosted_modal_open.set(false);
                                    store.hosted_modal_error.set(String::new());
                                    open_account_modal(store, "");
                                },
                                "Account..."
                            }
                        }
                    }

                    if store.cloud_signed_in.get() {
                        div {
                            style: "display: flex; flex-direction: column; gap: 10px;",

                            Select {
                                label: "Store",
                                placeholder: "Select a store",
                                value_fn: move || store.hosted_modal_selected.get(),
                                onchange: move |val: String| store.hosted_modal_selected.set(val),
                                data: {|| {
                                    store.hosted_modal_stores.get().iter()
                                        .map(|s| SelectOption::new(s.store_id.clone(), hosted_store_label(s)))
                                        .collect::<Vec<_>>()
                                }},
                            }

                            div {
                                style: {
                                    move || {
                                        let nothing = !store.hosted_modal_busy.get()
                                            && store.hosted_modal_error.get().is_empty()
                                            && store.hosted_modal_stores.with(|s| s.is_empty());
                                        if nothing {
                                            "font-size: 12px; color: var(--rinch-color-dimmed);"
                                        } else {
                                            "display: none;"
                                        }
                                    }
                                },
                                "Every encrypted store and share on this account is already open here."
                            }

                            Button {
                                variant: "filled",
                                size: "sm",
                                loading: {|| store.hosted_modal_busy.get()},
                                disabled: {|| store.hosted_modal_busy.get() || store.hosted_modal_selected.get().is_empty()},
                                onclick: add_hosted_store,
                                "Add"
                            }
                        }
                    }

                    if !store.hosted_modal_error.get().is_empty() {
                        div {
                            style: "color: var(--rinch-color-red-6); font-size: 12px;",
                            {|| store.hosted_modal_error.get()}
                        }
                    }
                }
            }
        };

        // ── "Share..." modal (node context menu) ────────────────────────
        // docs/NODE_DOCUMENT_CONTRACT.md section 5: a share is a scoped grant
        // on this store, so everyone on it edits the same documents. One
        // modal, two faces: a name and "Share" while the node is not shared;
        // the share, its members and "Stop sharing" once it is. A member
        // looking at the share they are in gets a third, read only
        // (`AppStore::share_modal_manages` decides). Every outcome
        // arrives as `CloudShareUpdated`, `CloudSharingStopped` or
        // `CloudError { op }`, so `events.rs` fills the busy and error lines
        // without guessing which request answered (the Account modal's rule,
        // decision 3). A store that is not hosted is refused by the server
        // with a sentence naming the relay; it lands in the error line like
        // any other answer, which is how the person finds out.
        let share_node_now = move || {
            let Some((store_id, node_id)) = untracked(|| store.share_modal_node.get()) else { return };
            let name = untracked(|| store.share_modal_name.get()).trim().to_string();
            if name.is_empty() {
                store.share_modal_error.set("Give the share a name.".to_string());
                return;
            }
            store.share_modal_error.set(String::new());
            store.share_modal_pending.set(Some(CloudOp::Share));
            store.send(BackendCommand::CloudShareNode { store_id, node_id, name });
        };
        let invite_now = move || {
            let Some((store_id, node_id)) = untracked(|| store.share_modal_node.get()) else { return };
            let email = untracked(|| store.share_modal_invite_email.get()).trim().to_string();
            if email.is_empty() {
                store.share_modal_error.set("Type the address to invite.".to_string());
                return;
            }
            let role = match untracked(|| store.share_modal_invite_role.get()).as_str() {
                "reader" => pimble_rpc::MemberRole::Reader,
                _ => pimble_rpc::MemberRole::Editor,
            };
            store.share_modal_error.set(String::new());
            store.share_modal_pending.set(Some(CloudOp::ShareInvite));
            store.send(BackendCommand::CloudShareInvite { store_id, node_id, email, role });
        };
        let remove_member = move |email: String| {
            let Some((store_id, node_id)) = untracked(|| store.share_modal_node.get()) else { return };
            store.share_modal_error.set(String::new());
            store.share_modal_pending.set(Some(CloudOp::ShareRemoveMember));
            store.send(BackendCommand::CloudShareRemoveMember { store_id, node_id, email });
        };
        let stop_sharing_now = move || {
            let Some((store_id, node_id)) = untracked(|| store.share_modal_node.get()) else { return };
            store.share_modal_error.set(String::new());
            store.share_modal_pending.set(Some(CloudOp::StopSharing));
            store.send(BackendCommand::CloudStopSharing { store_id, node_id });
        };
        let share_modal = rsx! {
            Modal {
                opened_fn: move || store.share_modal_node.get().is_some(),
                onclose: move || {
                    store.share_modal_node.set(None);
                    store.share_modal_error.set(String::new());
                    store.share_modal_confirm_stop.set(false);
                    store.share_modal_pending.set(None);
                },
                title: "Share",
                size: "md",

                div {
                    style: "display: flex; flex-direction: column; gap: 10px;",

                    // The node is not shared yet: name it and share it.
                    if !store.share_modal_shared.get() {
                        div {
                            style: "display: flex; flex-direction: column; gap: 10px;",

                            TextInput {
                                label: "Name",
                                placeholder: "Recipes",
                                value_fn: move || store.share_modal_name.get(),
                                oninput: move |val: String| store.share_modal_name.set(val),
                                onsubmit: share_node_now,
                            }

                            div {
                                class: "pimble-share__note",
                                "This name is the share's own: it is what the people you invite \
                                 see, and the name of your store is not. Pimble Cloud sees it \
                                 too. The notes themselves stay encrypted."
                            }

                            div {
                                class: "pimble-share__note",
                                "Everyone you invite to edit gets this folder and everything in \
                                 it, and edits all of it: the writing, the titles, and where \
                                 things sit. Their changes and yours are the same notes."
                            }

                            div {
                                style: "display: flex; justify-content: flex-end;",
                                Button {
                                    variant: "filled",
                                    size: "sm",
                                    loading: {|| store.share_modal_pending.get() == Some(CloudOp::Share)},
                                    disabled: {|| store.share_modal_pending.get().is_some()},
                                    onclick: share_node_now,
                                    "Share"
                                }
                            }
                        }
                    }

                    // The node is shared and this person manages the share:
                    // who has it, and how to stop.
                    if store.share_modal_shared.get() && store.share_modal_manages() {
                        div {
                            style: "display: flex; flex-direction: column; gap: 10px;",

                            div {
                                style: "font-weight: 600;",
                                {|| store.share_modal_name.get()}
                            }
                            div {
                                class: "pimble-share__note",
                                {|| store.share_modal_state.get()}
                            }

                            // The confirmation takes the modal over, so that
                            // "Stop sharing" is never one stray click.
                            if store.share_modal_confirm_stop.get() {
                                div {
                                    style: "display: flex; flex-direction: column; gap: 10px;",
                                    // Stopping a share ends the grant. It
                                    // deletes nothing: the notes are the
                                    // owner's own documents and stay where
                                    // they are (docs/NODE_DOCUMENT_CONTRACT.md
                                    // section 5).
                                    div {
                                        {|| format!(
                                            "Stop sharing \"{}\"? The people on it lose access and get nothing \
                                             new from it, and what they have already synced stays on their own \
                                             devices. The notes stay where they are, in your store and hosted \
                                             as before.",
                                            store.share_modal_name.get(),
                                        )}
                                    }
                                    div {
                                        style: "display: flex; justify-content: flex-end; gap: 8px;",
                                        Button {
                                            variant: "light",
                                            size: "sm",
                                            onclick: move || store.share_modal_confirm_stop.set(false),
                                            "Cancel"
                                        }
                                        Button {
                                            variant: "filled",
                                            color: "red",
                                            size: "sm",
                                            loading: {|| store.share_modal_pending.get() == Some(CloudOp::StopSharing)},
                                            disabled: {|| store.share_modal_pending.get().is_some()},
                                            onclick: stop_sharing_now,
                                            "Stop sharing"
                                        }
                                    }
                                }
                            }

                            if !store.share_modal_confirm_stop.get() {
                                div {
                                    style: "display: flex; flex-direction: column; gap: 10px;",

                                    div {
                                        class: "pimble-share__members",
                                        // An empty bordered box would read as
                                        // a list that failed to load; the line
                                        // below says it plainly instead.
                                        style: {
                                            move || if store.share_modal_members.with(|m| m.is_empty()) {
                                                "display: none;"
                                            } else {
                                                ""
                                            }
                                        },
                                        for member in store.share_modal_members.get() {
                                            div {
                                                key: member.email.clone(),
                                                class: "pimble-share__member",
                                                span {
                                                    class: "pimble-share__member-email",
                                                    {member.email.clone()}
                                                }
                                                span {
                                                    class: "pimble-share__member-status",
                                                    {format!("{}, {}", member_role_text(member.role), member_status_text(member.status, true))}
                                                }
                                                // The owner is the account this
                                                // Pimble is signed in as; there
                                                // is no share without them.
                                                if member.role != pimble_rpc::MemberRole::Owner {
                                                    ActionIcon {
                                                        icon: TablerIcon::X,
                                                        variant: "subtle",
                                                        size: "xs",
                                                        onclick: {
                                                            let email = member.email.clone();
                                                            move || remove_member(email.clone())
                                                        },
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    div {
                                        style: {
                                            move || if store.share_modal_members.with(|m| m.is_empty()) {
                                                "font-size: 12px; color: var(--rinch-color-dimmed);"
                                            } else {
                                                "display: none;"
                                            }
                                        },
                                        "Nobody else yet."
                                    }

                                    div {
                                        class: "pimble-share__invite",
                                        div {
                                            style: "flex: 1;",
                                            TextInput {
                                                label: "Invite",
                                                placeholder: "them@example.com",
                                                value_fn: move || store.share_modal_invite_email.get(),
                                                oninput: move |val: String| store.share_modal_invite_email.set(val),
                                                onsubmit: invite_now,
                                            }
                                        }
                                        Select {
                                            value_fn: move || store.share_modal_invite_role.get(),
                                            onchange: move |val: String| store.share_modal_invite_role.set(val),
                                            data: {|| vec![
                                                SelectOption::new("editor", "Editor"),
                                                SelectOption::new("reader", "Reader"),
                                            ]},
                                        }
                                        Button {
                                            variant: "filled",
                                            size: "sm",
                                            loading: {|| store.share_modal_pending.get() == Some(CloudOp::ShareInvite)},
                                            disabled: {|| store.share_modal_pending.get().is_some()},
                                            onclick: invite_now,
                                            "Invite"
                                        }
                                    }

                                    div {
                                        class: "pimble-share__note",
                                        "Someone who can edit changes everything in this folder: the writing, \
                                         the titles, and where things sit. Someone who can read changes nothing."
                                    }

                                    div {
                                        class: "pimble-share__note",
                                        "Pimble Cloud stores the notes encrypted, and sees who is invited and the \
                                         share's name. It never sees what is written in them, or their titles."
                                    }

                                    div {
                                        style: "display: flex; justify-content: flex-end;",
                                        Button {
                                            variant: "light",
                                            color: "red",
                                            size: "sm",
                                            disabled: {|| store.share_modal_pending.get().is_some()},
                                            onclick: move || store.share_modal_confirm_stop.set(true),
                                            "Stop sharing"
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // The node is shared and the share is someone else's (a
                    // member looking at the share they are in): whose it is
                    // and who is on it, as the accounts service answers a
                    // member. Inviting, removing and stopping are an owner's
                    // and the server refuses them from anyone else, so none
                    // of them is offered, and the owner's state line (what
                    // the owner's devices are handing over) is not this
                    // device's to report. The list is a courtesy: when it
                    // cannot be loaded the sentence stands alone, with no
                    // error line (`events.rs`, `CloudOp::ShareInfo`).
                    if store.share_modal_shared.get() && !store.share_modal_manages() {
                        div {
                            style: "display: flex; flex-direction: column; gap: 10px;",

                            div {
                                style: "font-weight: 600;",
                                {|| store.share_modal_name.get()}
                            }
                            div {
                                class: "pimble-share__note",
                                {|| store.share_modal_shared_by_sentence()}
                            }
                            div {
                                class: "pimble-share__members",
                                style: {
                                    move || if store.share_modal_members.with(|m| m.is_empty()) {
                                        "display: none;"
                                    } else {
                                        ""
                                    }
                                },
                                for member in store.share_modal_members.get() {
                                    div {
                                        key: member.email.clone(),
                                        class: "pimble-share__member",
                                        span {
                                            class: "pimble-share__member-email",
                                            {member.email.clone()}
                                        }
                                        span {
                                            class: "pimble-share__member-status",
                                            {format!("{}, {}", member_role_text(member.role), member_status_text(member.status, false))}
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if !store.share_modal_error.get().is_empty() {
                        div {
                            style: "color: var(--rinch-color-red-6); font-size: 12px;",
                            {|| store.share_modal_error.get()}
                        }
                    }
                }
            }
        };

        // Spawn the backend now that the UI and event loop are fully set up.
        // The EVENT_PROCESSOR thread-local is already registered, so signal_ui
        // callbacks will be processed correctly via run_on_main_thread. The web
        // build has no thread to spawn: its entry point puts a backend into the
        // store before it mounts, and this leaves that one alone.
        #[cfg(feature = "native")]
        if store.backend.with(|b| b.is_none()) {
            let backend = BackendHandle::spawn(move || {
                run_on_main_thread(crate::events::pump_backend_events);
            });
            store.backend.set(Some(backend));
        }

        // The page itself, with no window chrome around it: identical on both
        // targets. Only the frame differs — a borderless native window that
        // draws its own titlebar, or the browser's viewport.
        let body = rsx! {
            // Outer wrapper — flex column fills the content area, pushes
            // status bar to the very bottom.
            div {
                    style: "display: flex; flex-direction: column; height: 100%;",

                    {search_bar}

                    // ── Main content ──────────────────────────────
                    div {
                        style: "display: flex; flex: 1; overflow: hidden;",

                        // ── Sidebar ───────────────────────────────
                        div {
                            class: "pimble-sidebar",

                            // Header: store name + new-node button
                            div {
                                class: "pimble-sidebar__header",

                                span {
                                    class: "pimble-sidebar__heading",
                                    "EXPLORER"
                                }

                                // Only the browser build registers this: it is
                                // the way back to the account pages, which the
                                // desktop reaches through its menu bar instead.
                                if has_account_action() {
                                    ActionIcon {
                                        icon: TablerIcon::User,
                                        variant: "subtle",
                                        size: "xs",
                                        onclick: move || run_account_action(),
                                    }
                                }

                                ActionIcon {
                                    icon: TablerIcon::Plus,
                                    variant: "subtle",
                                    size: "xs",
                                    onclick: on_new_node,
                                }
                            }

                            div {
                                style: {|| if store.search_query.get().is_empty() { "" } else { "display: none;" }},
                                {tree_scroll}
                            }
                            div {
                                style: {|| if store.search_query.get().is_empty() { "display: none;" } else { "" }},
                                {search_panel}
                            }
                        }

                        // ── Editor panel ──────────────────────────
                        div {
                            class: "pimble-editor",
                            onclick: move || cancel_rename(true),

                            div {
                                class: "pimble-editor__toolbar-wrap",
                                style: {move || if store.show_editor.get() && !editor_read_only() { "" } else { "display: none;" }},
                                {toolbar_handle}
                            }
                            // The same sentence a refused write comes back
                            // with, so a reader meets one wording everywhere,
                            // plus what it means for the pane in front of them.
                            div {
                                class: "pimble-editor__read-only",
                                style: {move || if store.show_editor.get() && editor_read_only() { "" } else { "display: none;" }},
                                {format!("{} Nothing typed here is kept.", pimble_core::StoreAccess::READ_ONLY_REFUSAL)}
                            }
                            div {
                                class: "pimble-editor__content-wrap",
                                style: {|| if store.show_editor.get() { "" } else { "display: none;" }},
                                {editor_view}
                            }
                            div {
                                class: "pimble-empty-state",
                                style: {|| if store.show_editor.get() { "display: none;" } else { "" }},
                                div {
                                    class: "pimble-empty-state__icon",
                                    {empty_icon}
                                }
                                div {
                                    class: "pimble-empty-state__text",
                                    "Select a document to start editing"
                                }
                                if CAN_ADMINISTER_STORES {
                                    div {
                                        class: "pimble-empty-state__hint",
                                        "Or press Ctrl+N to create a new store"
                                    }
                                }
                                if CAN_CREATE_HOSTED_STORES {
                                    div {
                                        class: "pimble-empty-state__hint",
                                        "Or use + above the tree to make a store"
                                    }
                                }
                            }
                        }
                    }

                    // ── Status bar ────────────────────────────────
                    div {
                        class: "pimble-status-bar",

                        Badge {
                            variant: "dot",
                            color: {|| status_color.get()},
                            size: "xs",
                            {|| store.connection_status.get()}
                        }

                        span {
                            class: "pimble-status-bar__addr",
                            {|| {
                                let addr = store.server_addr.get();
                                if addr.is_empty() { String::new() } else { addr }
                            }}
                        }

                        // A refusal the server sent back, in its own words
                        // (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles").
                        // It is not a connection failure, so it does not touch
                        // the badge beside it.
                        span {
                            class: "pimble-status-bar__notice",
                            style: {|| if store.notice.get().is_empty() { "display: none;" } else { "" }},
                            {|| store.notice.get()}
                        }

                        // The signed-in account, when there is one; a click
                        // opens the Account modal (docs/DESKTOP_ACCOUNT_CONTRACT.md
                        // decision 7). Only a build whose server keeps an
                        // account: the browser signs in through its own pages.
                        if CAN_ADMINISTER_STORES {
                            span {
                                class: "pimble-status-bar__account",
                                style: {|| if store.cloud_signed_in.get() { "cursor: pointer;" } else { "display: none;" }},
                                onclick: move || open_account_modal(store, ""),
                                {|| format!("Signed in as {}", store.cloud_email.get())}
                            }
                        }

                        div { style: "flex: 1;", }
                    }
                }
        };

        #[cfg(feature = "native")]
        let root = rsx! {
            BorderlessWindow {
                title: "Pimble",
                show_minimize: true,
                show_maximize: true,
                show_close: true,
                on_close: || close_current_window(),

                style { {APP_CSS} }
                style { {EDITOR_CSS} }

                {connect_modal}
                {link_modal}
                {remove_replica_modal}
                {new_store_modal}
                {mount_picker_modal}
                {appearance_modal}
                {account_modal}
                {host_modal}
                {hosted_modal}
                {share_modal}
                {body}
            }
        };

        // A browser tab has its own chrome; the app just fills it.
        #[cfg(not(feature = "native"))]
        let root = rsx! {
            div {
                style: "height: 100vh; display: flex; flex-direction: column;",

                style { {APP_CSS} }
                style { {EDITOR_CSS} }

                {connect_modal}
                {link_modal}
                {remove_replica_modal}
                {new_store_modal}
                {mount_picker_modal}
                {appearance_modal}
                {account_modal}
                {host_modal}
                {hosted_modal}
                {share_modal}
                {body}
            }
        };

        root
    };

    (store, app_component)
}

/// Open the desktop window: the same UI [`build_view`] builds, wrapped in a
/// native menu bar, the borderless window and the saved theme.
#[cfg(feature = "native")]
pub fn run() {
    use std::sync::Arc;

    let (store, app_component) = build_view();

    // One spec, two shells: `crates/pimble-app/src/menus.rs` says what the
    // menus hold and which target each item belongs on, so the browser's menu
    // bar cannot drift from this one.
    let menus = crate::menus::build_menus(store);

    // Theme
    let theme = theme_props(untracked(|| store.dark_mode.get()));

    // Save-on-close: register a thread-local that the close callback invokes.
    // WindowProps requires Send+Sync but our state is Rc-based (main thread only),
    // so we use the same thread-local pattern as EVENT_PROCESSOR.
    thread_local! {
        static CLOSE_HANDLER: RefCell<Option<Box<dyn Fn()>>> = RefCell::new(None);
    }
    CLOSE_HANDLER.with(|cell| {
        *cell.borrow_mut() = Some(Box::new(move || {
            // Edits persist live through the collab relay; nothing to flush on close.
            let _ = store;
        }));
    });
    let on_close: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(|| {
        CLOSE_HANDLER.with(|cell| {
            if let Some(f) = cell.borrow().as_ref() {
                f();
            }
        });
        true // proceed with close
    });

    let props = WindowProps {
        title: "Pimble".into(),
        width: 1200,
        height: 800,
        borderless: true,
        transparent: true,
        resizable: true,
        menu_in_titlebar: true,
        on_close_requested: Some(on_close),
        ..Default::default()
    };

    App::new(app_component)
        .window_props(props)
        .theme(theme)
        .menu(menus)
        .run();

    EVENT_PROCESSOR.with(|cell| {
        *cell.borrow_mut() = None;
    });
    CLOSE_HANDLER.with(|cell| {
        *cell.borrow_mut() = None;
    });
}
#[cfg(test)]
mod tests {
    use super::{hosted_store_label, member_role_text, member_status_text, open_share_modal, sync_badge_text, MOUNT_NOTE};
    use crate::protocol::{BackendCommand, BackendHandle, CloudOp};
    use crate::state::AppStore;
    use pimble_core::{Node, NodeId, Store, StoreId, StoreKind, SyncState};
    use pimble_rpc::{CloudHostedStoreInfo, MemberRole, ShareMemberStatus};
    use rinch::prelude::untracked;

    /// A store with a shared folder and a plain document under its root, a
    /// backend whose commands the test can read, and an account signed in.
    /// `shared_by` is who shared the store with this account, if anyone did.
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
        assert_eq!(app.share_modal_pending.get(), None);
        assert!(!asked_for_members(&commands, plain_id));

        app.share_modal_node.set(None);
        app.cloud_signed_in.set(false);
        open_share_modal(app, store_id, plain_id);
        assert_eq!(app.share_modal_node.get(), None);
        assert!(app.account_modal_open.get());
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
        assert!(!app.account_modal_open.get(), "a member is asked to sign in to read who shared it");
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
        assert!(!app.account_modal_open.get());
        assert_eq!(app.notice.get(), "Only an owner of this store can share from it.");
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
        assert_eq!(hosted_store_label(&hosted("Notes", false, None)), "Notes");
        assert_eq!(
            hosted_store_label(&hosted("Recipes", true, Some("ann@example.com"))),
            "Recipes, shared by ann@example.com"
        );
        // A share whose owner the service did not name is still just a name.
        assert_eq!(hosted_store_label(&hosted("Recipes", true, None)), "Recipes");
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
        assert_eq!(sync_badge_text(&SyncState::Offline, StoreKind::Plain), "offline");
        assert_eq!(sync_badge_text(&SyncState::Syncing, StoreKind::Plain), "syncing");
        assert_eq!(sync_badge_text(&synced, StoreKind::Plain), "synced");
        assert_eq!(sync_badge_text(&SyncState::Offline, StoreKind::Vault), "encrypted · offline");
        assert_eq!(sync_badge_text(&SyncState::Syncing, StoreKind::Vault), "encrypted · syncing");
        assert_eq!(sync_badge_text(&synced, StoreKind::Vault), "encrypted · synced");
    }
}
