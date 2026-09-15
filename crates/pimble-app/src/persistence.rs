//! State file persistence: the open stores and the theme choice, in
//! `<config dir>/pimble/state.json`.

use std::path::PathBuf;

pub(crate) fn state_file_path() -> PathBuf {
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
