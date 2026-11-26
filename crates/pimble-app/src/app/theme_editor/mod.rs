//! The theme editor (docs/THEMES_CONTRACT.md section 9): a theme node opens
//! in a pane as two columns, its tokens on the left and on the right a
//! sample text drawn live in the theme as it stands, with a switch between
//! the light and the dark scheme.
//!
//! The editor holds nothing of its own: what it shows is the store's themes
//! as listed (`AppStore::store_themes`, listed again whenever a theme's
//! document changes, from here or anywhere), and each change is one
//! `applyEdit` of the keys of `data.theme` it names
//! (`ThemeCommand::Edit`), so two people editing one theme see each
//! other's changes as they land.

mod model;
mod view;

use rinch::prelude::*;
use serde_json::{Map, Value as Json};

use pimble_core::{NodeId, StoreAccess, StoreId};

use crate::panes::PaneId;
use crate::protocol::{BackendCommand, ThemeCommand};
use crate::state::{ActiveEdit, AppStore};

pub(crate) use view::render_theme_editor;

/// What one pane's theme editor shows beyond the theme itself.
#[derive(Clone, Copy)]
pub struct ThemeUi {
    /// "Show only what this theme changes".
    pub only_changed: Signal<bool>,
    /// The scheme the sample is drawn in: the app's when the theme opens.
    pub dark: Signal<bool>,
}

impl ThemeUi {
    pub fn new() -> Self {
        Self { only_changed: Signal::new(false), dark: Signal::new(false) }
    }
}

impl Default for ThemeUi {
    fn default() -> Self {
        Self::new()
    }
}

/// Pane `pane` shows theme node `node_id` of `store_id` in the editor;
/// whatever it held goes.
pub(crate) fn start(store: AppStore, pane: PaneId, store_id: StoreId, node_id: NodeId) {
    crate::editor::stop_editing(store, pane);
    crate::sheets::stop(store, pane);
    crate::metanodes::stop(store, pane);
    let state = store.pane(pane);
    state.show_editor.set(false);
    state.show_board.set(false);
    state.show_sheet.set(false);
    state.show_discussion.set(false);
    state.theme_ui.dark.set(untracked(|| store.dark_mode.get()));
    state.theme.set(Some(ActiveEdit { store_id, node_id }));
    state.show_theme.set(true);
    // Listed now: the editor draws from the store's themes, and asked even
    // when they are listed in case this one was made a moment ago elsewhere
    // and its change is still on its way.
    store.send(BackendCommand::Themes(ThemeCommand::List { store_id }));
}

/// Pane `pane` stops showing a theme (another node opened there, or the
/// pane emptied).
pub(crate) fn stop(store: AppStore, pane: PaneId) {
    let state = store.pane(pane);
    if untracked(|| state.theme.get()).is_some() {
        state.theme.set(None);
    }
    if untracked(|| state.show_theme.get()) {
        state.show_theme.set(false);
    }
}

/// The theme pane `pane` shows, tracked.
fn held(store: AppStore, pane: PaneId) -> Option<(StoreId, NodeId)> {
    store.pane(pane).theme.get().map(|t| (t.store_id, t.node_id))
}

/// What theme `node_id` of `store_id` stores, tracked: nothing while the
/// store's themes are not listed yet.
fn stored(store: AppStore, store_id: StoreId, node_id: NodeId) -> Option<Map<String, Json>> {
    store.store_themes.with(|all| all.get(&store_id)?.iter().find(|t| t.node == node_id).map(|t| t.stored.clone()))
}

/// Whether this device may change the theme; says why not when it may not.
fn may_write(store: AppStore, store_id: StoreId, node_id: NodeId) -> bool {
    let writable = untracked(|| store.node_access(store_id, node_id).allows_write());
    if !writable {
        crate::events::show_notice(store, StoreAccess::READ_ONLY_REFUSAL.to_string());
    }
    writable
}

/// Set (or with `None` clear) one key of the theme pane `pane` shows.
fn write(store: AppStore, pane: PaneId, key: &str, value: Option<Json>) {
    let Some((store_id, node_id)) = untracked(|| held(store, pane)) else { return };
    if !may_write(store, store_id, node_id) {
        return;
    }
    let edits = vec![(key.to_string(), value)];
    store.send(BackendCommand::Themes(ThemeCommand::Edit { store_id, node_id, edits }));
}
