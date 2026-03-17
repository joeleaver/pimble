//! State file persistence for remembering open stores across sessions.

pub(crate) fn state_file_path() -> std::path::PathBuf {
    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    config_dir.join("pimble").join("state.json")
}

pub(crate) fn load_app_state_file() -> Vec<String> {
    let path = state_file_path();
    let Ok(json) = std::fs::read_to_string(&path) else { return Vec::new() };
    serde_json::from_str::<serde_json::Value>(&json)
        .ok()
        .and_then(|v| v["open_stores"].as_array().cloned())
        .map(|arr| arr.into_iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

pub(crate) fn save_app_state_file(paths: &[String]) {
    let path = state_file_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::json!({ "open_stores": paths });
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&json).unwrap_or_default());
}
