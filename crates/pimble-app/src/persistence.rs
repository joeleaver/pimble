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

    /// The personal document (docs/DISCUSSIONS_CONTRACT.md section 8):
    /// `<data dir>/pimble/personal.yrs`, empty when there is none.
    fn personal_path() -> PathBuf {
        dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("pimble").join("personal.yrs")
    }

    pub(crate) fn load_personal() -> Vec<u8> {
        std::fs::read(personal_path()).unwrap_or_default()
    }

    /// Whether the personal document follows the account (on unless off).
    pub(crate) fn load_personal_sync() -> bool {
        read_state()["personal_sync"].as_bool().unwrap_or(true)
    }

    pub(crate) fn save_personal_sync(on: bool) {
        let mut state = read_state();
        state["personal_sync"] = serde_json::json!(on);
        write_state(&state);
    }

    #[cfg(not(test))]
    pub(crate) fn save_personal(bytes: &[u8]) {
        let path = personal_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Written beside and renamed, so a crash never leaves half a file.
        let tmp = path.with_extension("yrs.tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    #[cfg(test)]
    pub(crate) fn save_personal(_bytes: &[u8]) {}
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
    /// Off wasm (the web crate's own tests) there is no browser at all.
    fn storage() -> Option<web_sys::Storage> {
        if !cfg!(target_arch = "wasm32") {
            return None;
        }
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

    const PERSONAL_SYNC_KEY: &str = "pimble.personal_sync";

    pub(crate) fn load_personal_sync() -> bool {
        storage().and_then(|s| s.get_item(PERSONAL_SYNC_KEY).ok().flatten()).map(|v| v != "false").unwrap_or(true)
    }

    pub(crate) fn save_personal_sync(on: bool) {
        if let Some(storage) = storage() {
            let _ = storage.set_item(PERSONAL_SYNC_KEY, if on { "true" } else { "false" });
        }
    }

    /// Where the personal document lives in the browser, base64.
    const PERSONAL_KEY: &str = "pimble.personal";

    pub(crate) fn load_personal() -> Vec<u8> {
        use base64::Engine as _;
        storage()
            .and_then(|s| s.get_item(PERSONAL_KEY).ok().flatten())
            .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
            .unwrap_or_default()
    }

    pub(crate) fn save_personal(bytes: &[u8]) {
        use base64::Engine as _;
        if let Some(storage) = storage() {
            let _ = storage.set_item(PERSONAL_KEY, &base64::engine::general_purpose::STANDARD.encode(bytes));
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
    load_app_state_file, load_dark_mode, load_personal, load_personal_sync, load_sidebar_width, save_app_state_file, save_dark_mode,
    save_personal, save_personal_sync, save_sidebar_width,
};
