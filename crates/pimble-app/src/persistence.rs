//! State file persistence: the open stores and the theme choice, in
//! `<config dir>/pimble/state.json`.
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

    fn state_file_path() -> PathBuf {
        let config_dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        config_dir.join("pimble").join("state.json")
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
        read_state()["open_stores"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default()
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
}

pub(crate) use imp::{load_app_state_file, load_dark_mode, save_app_state_file, save_dark_mode};
