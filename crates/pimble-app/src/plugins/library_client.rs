//! The app's side of the plugin library (docs/SETTINGS_CONTRACT.md section 4,
//! docs/PLUGINS_CONTRACT.md section 7; `pimble_plugin_host::library` reads,
//! judges and checks what it serves): its index, verified installs, and every
//! installed plugin the library can replace kept up to date on its own.

use super::window::refresh_window;
use super::*;
use pimble_plugin_host::library::Have;

/// Where the library's index (`None`) or one of its files is.
fn library_url(file: Option<&str>) -> String {
    let base = place::library_base();
    match file {
        Some(file) => format!("{}/{file}", base.trim_end_matches('/')),
        None => base,
    }
}

/// Read the index again, as Settings > Plugins is shown, saying on the page
/// what kept it from being read.
pub(super) fn read_library() {
    if INDEX.with(|i| i.borrow().is_none()) {
        library_said("Looking at the library...".to_string());
    }
    read_index(true);
}

/// Read the index, keep it, and install every update it has for this device
/// (docs/PLUGINS_CONTRACT.md section 7). `on_page` says what came of reading
/// it on Settings > Plugins; the timer's reads only log.
fn read_index(on_page: bool) {
    place::fetch(library_url(None), move |fetched| {
        match fetched.and_then(|bytes| Index::parse(&bytes)) {
            Ok(index) => {
                let empty = index.plugins.is_empty();
                INDEX.with(|i| *i.borrow_mut() = Some(index));
                if on_page {
                    library_said(if empty { "The library has no plugins yet.".to_string() } else { String::new() });
                }
                update_what_is_due();
            }
            Err(e) => {
                tracing::info!("plugins: the library could not be read: {e}");
                let known = INDEX.with(|i| i.borrow().is_some());
                if on_page {
                    library_said(if known { String::new() } else { format!("The library could not be reached ({e}).") });
                }
            }
        }
        refresh_window();
    });
}

fn library_said(line: String) {
    if let Some(ui) = UI.with(Cell::get) {
        ui.library_line.set(line);
    }
}

fn row_said(id: &str, line: String) {
    ROW_LINES.with(|l| l.borrow_mut().insert(id.to_string(), line));
    refresh_window();
}

/// "Install", "Update" or "Use Library Version".
pub(super) fn install_from_library(id: &str) {
    let Some(entry) = INDEX.with(|i| i.borrow().as_ref().and_then(|index| index.plugins.iter().find(|e| e.id == id).cloned())) else {
        return;
    };
    row_said(&entry.id, format!("Downloading {}...", entry.name));
    fetch_and_install(entry);
}

/// The plugin and its signature are downloaded, checked against what the
/// index lists and the key this build carries, and only then installed. A
/// plugin that does not verify is never written anywhere.
fn fetch_and_install(entry: Entry) {
    place::fetch(library_url(Some(&entry.wasm)), move |wasm| {
        let sig_url = library_url(Some(&entry.sig));
        place::fetch(sig_url, move |sig| {
            let (wasm, sig) = match (wasm, sig) {
                (Ok(wasm), Ok(sig)) => (wasm, sig),
                (Err(e), _) | (_, Err(e)) => return install_failed(&entry, format!("The download did not finish ({e}).")),
            };
            if let Err(e) = library::verify_download(&entry, crate::RELEASE_KEY, &wasm, &sig) {
                tracing::warn!("plugins: {} from the library refused: {e}", entry.id);
                return install_failed(&entry, library::NOT_VERIFIED.to_string());
            }
            if !QUIET.with(|q| q.borrow().contains(&entry.id)) {
                row_said(&entry.id, format!("Installing {}...", entry.name));
            }
            place::install(entry, wasm);
        });
    });
}

/// A plugin from the library is installed: remember where it came from.
pub(super) fn installed_from_library(entry: &Entry) {
    let quiet = QUIET.with(|q| q.borrow_mut().remove(&entry.id));
    PROVENANCE.with(|p| p.borrow_mut().plugins.insert(entry.id.clone(), entry.version.clone()));
    write_provenance();
    let line = if quiet { format!("Updated to {}.", entry.version) } else { format!("Installed {} {}.", entry.name, entry.version) };
    tracing::info!("plugins: {} {line}", entry.id);
    row_said(&entry.id, line);
    if quiet {
        // Panes showing it say a newer version is ready.
        if let Some(store) = STORE.with(Cell::get) {
            crate::metanodes::plugins_ready(store);
        }
    }
}

/// An install from the library did not happen: the row says why. One made on
/// its own is tried again at the next check.
pub(super) fn install_failed(entry: &Entry, line: String) {
    let quiet = QUIET.with(|q| q.borrow_mut().remove(&entry.id));
    if quiet {
        tracing::warn!("plugins: updating {} to {} did not happen: {line}", entry.id, entry.version);
    }
    row_said(&entry.id, line);
}

// ── Updates on their own (docs/PLUGINS_CONTRACT.md section 7) ────────────────

/// The first look at the library after the app starts, then one every six
/// hours.
pub(super) const FIRST_UPDATE_CHECK_MS: u32 = 20_000;
const UPDATE_CHECK_EVERY_MS: u32 = 6 * 60 * 60 * 1000;

pub(super) fn schedule_update_check(after_ms: u32) {
    rinch::core::reactive::unowned(|| {
        let _ = rinch::core::set_timeout(after_ms, || {
            read_index(false);
            schedule_update_check(UPDATE_CHECK_EVERY_MS);
        });
    });
}

/// Install, without asking, every update the index has for a plugin
/// installed here: a newer version this Pimble runs, wherever the installed
/// one came from, or any version this Pimble runs of one it does not (built
/// for an older ABI). A development build at the library's version or later is
/// left alone. What each was allowed carries over, and a new version that asks
/// for more asks the way a first use does.
fn update_what_is_due() {
    let Some(index) = INDEX.with(|i| i.borrow().clone()) else { return };
    // Nothing is judged before the plugins here have loaded: a plugin not
    // loaded yet would read as not installed.
    if REGISTRY.with(|r| r.borrow().is_none()) {
        return;
    }
    let installed = super::window::installed();
    let provenance = PROVENANCE.with(|p| p.borrow().clone());
    let have = |id: &str| {
        installed.iter().find(|h| h.id == id).map(|h| Have {
            version: h.version.as_deref().unwrap_or_default(),
            from_library: provenance.from_library(id),
            runs: h.refusal.is_none(),
        })
    };
    for entry in library::updates_due(&index, have) {
        if !QUIET.with(|q| q.borrow_mut().insert(entry.id.clone())) {
            continue;
        }
        tracing::info!("plugins: updating {} to {}", entry.id, entry.version);
        row_said(&entry.id, format!("Updating to {}...", entry.version));
        fetch_and_install(entry);
    }
}

/// Plugin `id` was added from a file over the library's (`removed`: or taken
/// off this device, which also forgets that it was turned off): the library's
/// record of it goes.
pub(super) fn forget_library(id: &str, removed: bool) {
    let had = PROVENANCE.with(|p| {
        let mut p = p.borrow_mut();
        let was_library = p.plugins.remove(id).is_some();
        let was_off = removed && p.off.remove(id);
        was_library || was_off
    });
    if had {
        write_provenance();
    }
    ROW_LINES.with(|l| l.borrow_mut().remove(id));
    refresh_window();
}

pub(super) fn write_provenance() {
    let json = PROVENANCE.with(|p| p.borrow().to_json());
    place::write_provenance(&json);
}
