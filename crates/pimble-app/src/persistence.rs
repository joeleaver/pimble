//! State file persistence: the open stores and the theme choice, in
//! `<config dir>/pimble/state.json`.
//!
//! Native only. A web build has no config directory and no store paths to
//! remember (its stores come from the account service's `listStores`), so the
//! same four calls answer with defaults and drop what they are given. Keeping
//! the module's surface identical on both targets is what lets `app.rs` and
//! `events.rs` stay free of `cfg`.

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

    fn write_state(state: &serde_json::Value) {
        let path = state_file_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, serde_json::to_string_pretty(state).unwrap_or_default());
    }

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
    /// Nothing is remembered between page loads yet, so the web app starts with
    /// no stores of its own and asks the server which ones the account may see.
    pub(crate) fn load_app_state_file() -> Vec<String> {
        Vec::new()
    }

    pub(crate) fn save_app_state_file(_paths: &[String]) {}

    /// The same default the desktop app uses when it has never been told.
    pub(crate) fn load_dark_mode() -> bool {
        true
    }

    pub(crate) fn save_dark_mode(_dark: bool) {}
}

pub(crate) use imp::{load_app_state_file, load_dark_mode, save_app_state_file, save_dark_mode};
