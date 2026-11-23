//! No test may touch the real config or data directories (`~/.config/pimble`,
//! `~/.local/share/pimble`): a server built on `ServerConfig::default()`
//! keeps its replicas, each store's state, its relay twins and devices file
//! under the data directory, and reads and writes the keystore, the saved
//! credentials and the agents file under the config one, and a handler built
//! with `RpcHandler::new` reads the same keystore and credentials.
//!
//! This reads every Rust source in the workspace's crates and refuses, outside
//! the few production places that mean the defaults, a `ServerConfig { .. }`
//! filled from `Default::default()`, `ServerConfig::default()`,
//! `PimbleServer::new()`, and the `RpcHandler` constructors that take the
//! default paths. A test builds on `ServerConfig::in_dir(dir)` (or
//! `tests/isolated`'s `TempServer`) and `RpcHandler::in_dir(manager, dir)`.
//! A test binary that points `XDG_DATA_HOME` and `XDG_CONFIG_HOME` at temp
//! directories itself (to test the defaults) may use them.

use std::path::{Path, PathBuf};

/// Where the defaults are the point: the app's own server, the CLI's server,
/// and the definitions themselves.
const PRODUCTION: &[&str] = &[
    "pimble-server/src/server.rs",
    "pimble-server/src/handler.rs",
    "pimble-server/src/local.rs",
    "pimble-server/src/relay_face.rs",
    "pimble-cli/src/main.rs",
    // The browser worker's entry: wasm32 only, its defaults the worker's own
    // disk (`/pimble`, `/pimble-config`), never the machine's directories.
    "pimble-worker/src/worker.rs",
];

const REFUSED_CALLS: &[&str] = &[
    "ServerConfig::default()",
    "PimbleServer::new()",
    "RpcHandler::new(",
    "RpcHandler::with_semantic_available(",
    "RpcHandler::with_credentials_path(",
    "RpcHandler::with_paths(",
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The text of each `ServerConfig { ... }` literal in `source`, with its line.
fn config_literals(source: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = source[from..].find("ServerConfig {") {
        let start = from + at;
        from = start + "ServerConfig {".len();
        let before = source[..start].trim_end_matches(|c: char| c.is_alphanumeric() || c == '_' || c == ':');
        if before.ends_with("struct ") || before.ends_with("for ") {
            continue;
        }
        let mut depth = 1;
        let mut end = from;
        for (i, c) in source[from..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                end = from + i;
                break;
            }
        }
        let line = source[..start].lines().count().max(1);
        found.push((line, source[from..end].split_whitespace().collect::<Vec<_>>().join(" ")));
    }
    found
}

#[test]
fn no_test_server_uses_the_real_config_or_data_directories() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("the crates directory").to_path_buf();
    let mut files = Vec::new();
    rust_files(&crates, &mut files);
    assert!(files.len() > 50, "found only {} sources under {}", files.len(), crates.display());

    let mut offences = Vec::new();
    for file in files {
        let rel = file.strip_prefix(&crates).unwrap().to_string_lossy().replace('\\', "/");
        if PRODUCTION.contains(&rel.as_str()) || rel == "pimble-server/tests/test_isolation.rs" {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&file) else { continue };
        if !source.contains("ServerConfig") && !source.contains("RpcHandler") && !source.contains("PimbleServer") {
            continue;
        }
        let own_dirs = source.contains("set_var(\"XDG_DATA_HOME\"") && source.contains("set_var(\"XDG_CONFIG_HOME\"");
        if own_dirs {
            continue;
        }
        for (line, literal) in config_literals(&source) {
            if literal.contains("..Default::default()") || literal.contains("ServerConfig::default()") {
                offences.push(format!("{rel}:{line}: a ServerConfig filled from the defaults; build on `..ServerConfig::in_dir(dir)`"));
            }
        }
        for (n, text) in source.lines().enumerate() {
            let code = text.split("//").next().unwrap_or("");
            for call in REFUSED_CALLS {
                if code.contains(call) {
                    offences.push(format!("{rel}:{}: `{call}` uses the real config and data directories; use the `in_dir` constructors", n + 1));
                }
            }
        }
    }
    assert!(offences.is_empty(), "tests that would touch ~/.config/pimble or ~/.local/share/pimble:\n{}", offences.join("\n"));
}
