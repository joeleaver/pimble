//! Writes the plugin library for a release (docs/SETTINGS_CONTRACT.md section
//! 4): every plugin in the `plugins/` workspace whose Cargo.toml says
//! `[package.metadata.pimble] library = true`, copied as `<id>-<version>.wasm`
//! (with `screenshot.png` beside its Cargo.toml as `<id>-<version>.png`), and
//! `plugins.json` describing them, each entry read from the plugin's own
//! manifest. The Release workflow signs the modules afterwards.
//!
//!   cargo metadata --manifest-path plugins/Cargo.toml --no-deps --format-version 1 > meta.json
//!   pimble-plugin-index meta.json plugins/target/wasm32-wasip1/release out/

use std::path::{Path, PathBuf};

use pimble_plugin_host::library::{entry_for, Index, INDEX_FILE, INDEX_FORMAT};
use pimble_plugin_host::wasmtime_engine::{load_file, DEFAULT_BUDGET};

fn main() {
    if let Err(e) = run() {
        eprintln!("pimble-plugin-index: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [metadata, wasm_dir, out] = args.as_slice() else {
        return Err("usage: pimble-plugin-index <cargo metadata json> <wasm dir> <out dir>".into());
    };
    let metadata: serde_json::Value =
        serde_json::from_slice(&std::fs::read(metadata).map_err(|e| format!("{metadata}: {e}"))?).map_err(|e| format!("{metadata}: {e}"))?;
    let out = PathBuf::from(out);
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut index = Index { v: INDEX_FORMAT, plugins: Vec::new() };
    for package in metadata["packages"].as_array().into_iter().flatten() {
        let pimble = &package["metadata"]["pimble"];
        if pimble["library"].as_bool() != Some(true) {
            continue;
        }
        let crate_name = package["name"].as_str().unwrap_or_default();
        let wasm = Path::new(wasm_dir).join(format!("{}.wasm", crate_name.replace('-', "_")));
        let plugin = load_file(&wasm, DEFAULT_BUDGET).map_err(|(_, sentence)| format!("{}: {sentence}", wasm.display()))?;
        let manifest = &plugin.manifest;
        let summary = pimble["summary"].as_str().ok_or_else(|| format!("{crate_name} has no [package.metadata.pimble] summary"))?;
        let bytes = std::fs::read(&wasm).map_err(|e| format!("{}: {e}", wasm.display()))?;
        let crate_dir = package["manifest_path"].as_str().map(Path::new).and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default();
        let shot_from = crate_dir.join("screenshot.png");
        let shot = shot_from.is_file().then(|| format!("{}-{}.png", manifest.id, manifest.version));
        let entry = entry_for(manifest, summary, &bytes, shot.clone());
        std::fs::write(out.join(&entry.wasm), &bytes).map_err(|e| format!("{}: {e}", entry.wasm))?;
        if let Some(shot) = &shot {
            std::fs::copy(&shot_from, out.join(shot)).map_err(|e| format!("{shot}: {e}"))?;
        }
        println!("{} {} ({} bytes)", entry.id, entry.version, entry.bytes);
        index.plugins.push(entry);
    }
    if index.plugins.is_empty() {
        return Err("no package says [package.metadata.pimble] library = true".into());
    }
    index.plugins.sort_by(|a, b| a.name.cmp(&b.name));
    let json = serde_json::to_vec_pretty(&index).map_err(|e| e.to_string())?;
    std::fs::write(out.join(INDEX_FILE), json).map_err(|e| format!("{INDEX_FILE}: {e}"))?;
    Ok(())
}
