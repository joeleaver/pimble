//! The app's side of the plugin library (docs/SETTINGS_CONTRACT.md section 4;
//! `pimble_plugin_host::library` reads and checks what it serves): its index, the
//! rows Settings > Plugins lists from it, verified installs, and updates on
//! their own (section 5).

use super::window::{asks_sentence, refresh_window, LibraryRow};
use super::*;

/// Where the library's index (`None`) or one of its files is.
fn library_url(file: Option<&str>) -> String {
    let base = place::library_base();
    match file {
        Some(file) => format!("{}/{file}", base.trim_end_matches('/')),
        None => base,
    }
}

/// Read the index again, as Settings > Plugins is shown.
pub(super) fn read_library() {
    if INDEX.with(|i| i.borrow().is_none()) {
        library_said("Looking at the library...".to_string());
    }
    place::fetch(library_url(None), |fetched| {
        let read = fetched.and_then(|bytes| Index::parse(&bytes));
        match read {
            Ok(index) => {
                let empty = index.plugins.is_empty();
                INDEX.with(|i| *i.borrow_mut() = Some(index));
                library_said(if empty { "The library has no plugins yet.".to_string() } else { String::new() });
            }
            Err(e) => {
                tracing::warn!("plugins: the library could not be read: {e}");
                let known = INDEX.with(|i| i.borrow().is_some());
                library_said(if known { String::new() } else { format!("The library could not be reached ({e}).") });
            }
        }
        refresh_library();
    });
}

fn library_said(line: String) {
    if let Some(ui) = UI.with(Cell::get) {
        ui.library_line.set(line);
    }
}

fn library_rows() -> Vec<LibraryRow> {
    let Some(index) = INDEX.with(|i| i.borrow().clone()) else { return Vec::new() };
    index
        .plugins
        .iter()
        .map(|entry| {
            let installed = manifest_of(&entry.id).map(|m| m.version);
            let from_library = PROVENANCE.with(|p| p.borrow().from_library(&entry.id));
            let offer = library::offer(entry, installed.as_deref(), from_library);
            let mut asks = asks_sentence(&entry.asks).to_string();
            if let Some(first) = asks.get_mut(..1) {
                first.make_ascii_uppercase();
            }
            let asked = ASKED.with(|a| a.borrow().as_deref() == Some(entry.id.as_str()));
            let line = ROW_LINES.with(|l| l.borrow().get(&entry.id).cloned()).unwrap_or_else(|| match (&offer, asked) {
                (Offer::Install, true) => format!("Press Install to add {}.", entry.name),
                (Offer::Update { from }, true) => format!("You have {from}. Press Update to get {}.", entry.version),
                (Offer::Update { from }, false) => format!("You have {from}."),
                (Offer::Installed, true) => format!("{} is installed already.", entry.name),
                (Offer::FromFile, true) => format!("{} was added from a file. Remove it above to install the library's.", entry.name),
                (Offer::NeedsNewer, true) => format!("{} needs a newer Pimble. Update Pimble first.", entry.name),
                _ => String::new(),
            });
            let button = match offer {
                Offer::Install => Some("Install"),
                Offer::Update { .. } => Some("Update"),
                _ => None,
            };
            LibraryRow { asked, button, id: entry.id.clone(), title: format!("{} {}", entry.name, entry.version), summary: entry.summary.clone(), asks: format!("{asks}."), offer, line }
        })
        .collect()
}

pub(super) fn refresh_library() {
    if let Some(ui) = UI.with(Cell::get) {
        ui.library.set(library_rows());
    }
}

fn row_said(id: &str, line: String) {
    ROW_LINES.with(|l| l.borrow_mut().insert(id.to_string(), line));
    refresh_library();
}

/// "Install" or "Update".
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
    let line = if quiet { format!("{} updated to {}.", entry.name, entry.version) } else { format!("Installed {} {}.", entry.name, entry.version) };
    tracing::info!("plugins: {line}");
    row_said(&entry.id, line);
    refresh_window();
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

// ── Updates on their own (docs/SETTINGS_CONTRACT.md section 5) ───────────────

/// The first look at the library after the app starts, then one every six
/// hours.
pub(super) const FIRST_UPDATE_CHECK_MS: u32 = 20_000;
const UPDATE_CHECK_EVERY_MS: u32 = 6 * 60 * 60 * 1000;

pub(super) fn schedule_update_check(after_ms: u32) {
    rinch::core::reactive::unowned(|| {
        let _ = rinch::core::set_timeout(after_ms, || {
            check_for_updates();
            schedule_update_check(UPDATE_CHECK_EVERY_MS);
        });
    });
}

/// Read the index, and install every newer version of a plugin that came from
/// the library and that this Pimble can load, without asking. A plugin added
/// from a file is never touched; what each was allowed carries over, and a
/// new version that asks for more asks the way a first use does.
fn check_for_updates() {
    place::fetch(library_url(None), |fetched| {
        let index = match fetched.and_then(|bytes| Index::parse(&bytes)) {
            Ok(index) => index,
            Err(e) => return tracing::info!("plugins: no update check this time: {e}"),
        };
        INDEX.with(|i| *i.borrow_mut() = Some(index.clone()));
        let due = updates_due(&index, |id| manifest_of(id).map(|m| m.version), |id| PROVENANCE.with(|p| p.borrow().from_library(id)));
        refresh_library();
        for entry in due {
            if !QUIET.with(|q| q.borrow_mut().insert(entry.id.clone())) {
                continue;
            }
            tracing::info!("plugins: updating {} to {}", entry.id, entry.version);
            fetch_and_install(entry);
        }
    });
}

/// The entries of `index` that are updates to install on their own: newer
/// than the version installed, of a plugin installed from the library, for
/// this Pimble's plugin ABI.
fn updates_due(index: &Index, installed: impl Fn(&str) -> Option<String>, from_library: impl Fn(&str) -> bool) -> Vec<Entry> {
    index
        .plugins
        .iter()
        .filter(|entry| {
            let have = installed(&entry.id);
            matches!(library::offer(entry, have.as_deref(), from_library(&entry.id)), Offer::Update { .. })
        })
        .cloned()
        .collect()
}

/// Plugin `id` was removed, or added from a file over the library's: the
/// library no longer looks after it.
pub(super) fn forget_library(id: &str) {
    let had = PROVENANCE.with(|p| p.borrow_mut().plugins.remove(id).is_some());
    if had {
        write_provenance();
    }
    ROW_LINES.with(|l| l.borrow_mut().remove(id));
    refresh_library();
}

fn write_provenance() {
    let json = PROVENANCE.with(|p| p.borrow().to_json());
    place::write_provenance(&json);
}

#[cfg(test)]
mod update_tests {
    use super::*;
    use pimble_plugin_host::abi::{Permissions, ABI_VERSION};

    fn entry(id: &str, version: &str, abi: u32) -> Entry {
        Entry {
            id: id.into(),
            name: id.into(),
            version: version.into(),
            abi,
            author: "Pimble".into(),
            summary: String::new(),
            asks: Permissions::default(),
            wasm: format!("{id}-{version}.wasm"),
            sig: format!("{id}-{version}.wasm.sig"),
            shot: None,
            bytes: 1,
            sha256: String::new(),
        }
    }

    #[test]
    fn only_newer_library_plugins_this_pimble_runs_update_on_their_own() {
        let index = Index {
            v: 1,
            plugins: vec![
                entry("kanban-viewer", "0.2.0", ABI_VERSION),
                entry("todo-viewer", "0.2.0", ABI_VERSION),
                entry("timeline-viewer", "0.2.0", ABI_VERSION + 1),
                entry("calendar-viewer", "0.2.0", ABI_VERSION),
                entry("stuck-viewer", "0.1.0", ABI_VERSION),
            ],
        };
        let installed = |id: &str| match id {
            "kanban-viewer" | "todo-viewer" | "timeline-viewer" => Some("0.1.0".to_string()),
            "stuck-viewer" => Some("0.1.0".to_string()),
            _ => None,
        };
        // todo was added from a file; calendar is not installed; timeline's new
        // version needs a newer Pimble; stuck is current.
        let from_library = |id: &str| id != "todo-viewer";
        let due: Vec<String> = updates_due(&index, installed, from_library).into_iter().map(|e| e.id).collect();
        assert_eq!(due, vec!["kanban-viewer".to_string()]);
    }
}
