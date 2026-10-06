//! State file persistence: the open stores, the theme choice, the explorer's
//! width and the split view's layout, in `<config dir>/pimble/state.json`.
//!
//! A web build has no config directory and no store paths to remember (its
//! stores come from the account service's `listStores`), so those two calls
//! answer with defaults and drop what they are given. The theme choice it does
//! remember, in `localStorage` — per browser, which is the right scope for it.
//! Keeping the module's surface identical on both targets is what lets `app.rs`
//! and `events.rs` stay free of `cfg`.

#[cfg(feature = "native")]
mod imp {
    use std::path::PathBuf;

    /// The file is the one `pimble_server::local` names: the open-store list
    /// in it is shared with `pimble-mcp` (docs/MCP_CONTRACT.md "The open-store
    /// list").
    fn state_file_path() -> PathBuf {
        pimble_server::local::state_file_path()
    }

    fn read_state() -> serde_json::Value {
        let Ok(json) = std::fs::read_to_string(state_file_path()) else {
            return serde_json::json!({});
        };
        serde_json::from_str(&json).unwrap_or_else(|_| serde_json::json!({}))
    }

    #[cfg(not(test))]
    fn write_state(state: &serde_json::Value) {
        let path = state_file_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, serde_json::to_string_pretty(state).unwrap_or_default());
    }

    /// The unit tests drive the same event handlers the app does, and those
    /// save the open-store list as they go. `state_file_path()` is the real
    /// one, so a test run would rewrite the list of stores whoever ran it has
    /// open. Under test the file is never written.
    #[cfg(test)]
    fn write_state(_state: &serde_json::Value) {}

    /// The saved open-store paths.
    pub(crate) fn load_app_state_file() -> Vec<String> {
        pimble_server::local::load_open_stores()
    }

    /// Save the open-store paths, keeping every other saved preference.
    pub(crate) fn save_app_state_file(paths: &[String]) {
        let mut state = read_state();
        state["open_stores"] = serde_json::json!(paths);
        write_state(&state);
    }

    /// Whether the app uses the dark theme (the default when nothing is saved).
    pub(crate) fn load_dark_mode() -> bool {
        read_state()["dark_mode"].as_bool().unwrap_or(true)
    }

    /// Save the theme choice, keeping the open-store list.
    pub(crate) fn save_dark_mode(dark: bool) {
        let mut state = read_state();
        state["dark_mode"] = serde_json::json!(dark);
        write_state(&state);
    }

    /// The explorer's width in pixels, when the person has dragged it.
    pub(crate) fn load_sidebar_width() -> Option<f32> {
        read_state()["sidebar_width"].as_f64().map(|w| w as f32)
    }

    /// Save the explorer's width, keeping every other saved preference.
    pub(crate) fn save_sidebar_width(width: f32) {
        let mut state = read_state();
        state["sidebar_width"] = serde_json::json!(width.round());
        write_state(&state);
    }

    /// The split view's layout as it was last saved, as JSON
    /// (docs/SPLIT_VIEW_CONTRACT.md "Persistence").
    pub(super) fn load_panes_json() -> Option<serde_json::Value> {
        let state = read_state();
        (!state["panes"].is_null()).then(|| state["panes"].clone())
    }

    /// Save the split view's layout, keeping every other saved preference.
    pub(super) fn save_panes_json(panes: serde_json::Value) {
        let mut state = read_state();
        state["panes"] = panes;
        write_state(&state);
    }
}

#[cfg(not(feature = "native"))]
mod imp {
    /// Where the theme choice lives in the browser.
    const DARK_MODE_KEY: &str = "pimble.dark_mode";

    /// The app's stores come from the account, not from a remembered list of
    /// paths, so there is nothing here to load or save.
    pub(crate) fn load_app_state_file() -> Vec<String> {
        Vec::new()
    }

    pub(crate) fn save_app_state_file(_paths: &[String]) {}

    /// `localStorage`, when the browser has one. It can be missing or throw
    /// outright in a private window or with site data blocked, so every read
    /// falls back to the same default the desktop uses when it has never been
    /// told, and every write is allowed to fail silently.
    fn storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok().flatten()
    }

    pub(crate) fn load_dark_mode() -> bool {
        storage()
            .and_then(|s| s.get_item(DARK_MODE_KEY).ok().flatten())
            .map(|value| value != "false")
            .unwrap_or(true)
    }

    pub(crate) fn save_dark_mode(dark: bool) {
        if let Some(storage) = storage() {
            let _ = storage.set_item(DARK_MODE_KEY, if dark { "true" } else { "false" });
        }
    }

    /// Where the explorer's width lives in the browser.
    const SIDEBAR_WIDTH_KEY: &str = "pimble.sidebar_width";

    pub(crate) fn load_sidebar_width() -> Option<f32> {
        storage()?.get_item(SIDEBAR_WIDTH_KEY).ok().flatten()?.parse().ok()
    }

    pub(crate) fn save_sidebar_width(width: f32) {
        if let Some(storage) = storage() {
            let _ = storage.set_item(SIDEBAR_WIDTH_KEY, &width.round().to_string());
        }
    }

    /// Where the split view's layout lives in the browser: the same JSON
    /// the desktop keeps under `panes` in `state.json`.
    const PANES_KEY: &str = "pimble.panes";

    pub(super) fn load_panes_json() -> Option<serde_json::Value> {
        serde_json::from_str(&storage()?.get_item(PANES_KEY).ok().flatten()?).ok()
    }

    pub(super) fn save_panes_json(panes: serde_json::Value) {
        if let Some(storage) = storage() {
            let _ = storage.set_item(PANES_KEY, &panes.to_string());
        }
    }
}

/// The split view's layout as it was last saved, safe to restore; `None`
/// when none was saved or what is there cannot be read (the single empty
/// pane is the answer to both).
pub(crate) fn load_panes() -> Option<crate::panes::SavedPanes> {
    let saved: crate::panes::SavedPanes = serde_json::from_value(imp::load_panes_json()?).ok()?;
    Some(saved.sanitized())
}

/// Save the split view's layout: the panes, their sizes, their documents.
pub(crate) fn save_panes(panes: &crate::panes::SavedPanes) {
    if let Ok(json) = serde_json::to_value(panes) {
        imp::save_panes_json(json);
    }
}

pub(crate) use imp::{
    load_app_state_file, load_dark_mode, load_sidebar_width, save_app_state_file, save_dark_mode, save_sidebar_width,
};
