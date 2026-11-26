use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use super::library_client::{forget_library, install_failed, installed_from_library};
use super::window::say;
use super::*;
use pimble_plugin_host::registry::id_from_file;
use pimble_plugin_host::wasmtime_engine::{load_bytes, load_file, NotLoaded, DEFAULT_BUDGET};

pub const CAN_ADD: bool = true;

pub static WHERE: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!("Plugins are .wasm files in {}. One added there, replaced or removed takes effect at once. Each asks once on this device before it reads or changes anything.", plugins_dir().display())
});

type Loaded = Result<Plugin, NotLoaded>;

/// What a file was when it was loaded: its length and modified time.
type Stamp = (u64, Option<SystemTime>);

/// What each file in the folder loaded as, and the file as it was then: a
/// file unchanged since is not compiled again.
static CACHE: Mutex<Option<HashMap<PathBuf, (Stamp, Loaded)>>> = Mutex::new(None);

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()))
}

/// The plugin a file holds: its manifest's id, as far as that was read, else
/// its name read as one (the same rule as `Refused::id`).
fn id_of(path: &Path, loaded: &Loaded) -> String {
    match loaded {
        Ok(plugin) => plugin.manifest.id.clone(),
        Err(NotLoaded { head: Some(head), .. }) if !head.id.is_empty() => head.id.clone(),
        Err(_) => id_from_file(path),
    }
}

/// Every file in the folder that holds plugin `id`, as last scanned.
fn files_of(id: &str) -> Vec<PathBuf> {
    let guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().map(|cache| cache.iter().filter(|(path, (_, loaded))| id_of(path, loaded) == id).map(|(path, _)| path.clone()).collect()).unwrap_or_default()
}

/// Every `.wasm` file in `dir`, each compiled again only when its file
/// changed, and the ids whose plugin changed: new, replaced or gone.
fn scan(dir: &Path) -> (Registry, Vec<String>) {
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = guard.get_or_insert_with(HashMap::new);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "wasm")).collect())
        .unwrap_or_default();
    paths.sort();
    let mut changed: Vec<String> = Vec::new();
    let mut note = |id: String| {
        if !changed.contains(&id) {
            changed.push(id);
        }
    };
    let mut registry = Registry::default();
    for path in &paths {
        let now = stamp(path);
        let same = now.is_some() && cache.get(path).is_some_and(|(was, _)| Some(*was) == now);
        if !same {
            if let Some((_, old)) = cache.get(path) {
                note(id_of(path, old));
            }
            let loaded = load_file(path, DEFAULT_BUDGET);
            note(id_of(path, &loaded));
            cache.insert(path.clone(), (now.unwrap_or((0, None)), loaded));
        }
        let (_, loaded) = &cache[path];
        registry.take(path.clone(), loaded.clone());
    }
    let gone: Vec<PathBuf> = cache.keys().filter(|p| !paths.contains(p)).cloned().collect();
    for path in gone {
        if let Some((_, old)) = cache.remove(&path) {
            note(id_of(&path, &old));
        }
    }
    (registry, changed)
}

/// Look at the folder again and hand what changed to the window.
fn rescan(dir: &Path) {
    let (registry, changed) = scan(dir);
    if changed.is_empty() {
        return;
    }
    tracing::info!("plugins: {} changed in {}", changed.join(", "), dir.display());
    rinch::core::reactive::run_on_main_thread(move || swapped(registry, &changed));
}

/// Watch the folder for as long as the app runs: a burst of events (a copy
/// writes in several steps) is one rescan once it has been quiet for half
/// a second.
fn watch(dir: PathBuf) {
    use notify::Watcher;
    let _ = std::fs::create_dir_all(&dir);
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let mut watcher = match notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.is_ok_and(|e| e.paths.iter().any(|p| p.extension().is_some_and(|x| x == "wasm"))) {
            let _ = tx.send(());
        }
    }) {
        Ok(w) => w,
        Err(e) => return tracing::warn!("plugins: the folder cannot be watched ({e}); a change there shows at the next start"),
    };
    if let Err(e) = watcher.watch(&dir, notify::RecursiveMode::NonRecursive) {
        return tracing::warn!("plugins: {} cannot be watched ({e}); a change there shows at the next start", dir.display());
    }
    while rx.recv().is_ok() {
        while rx.recv_timeout(Duration::from_millis(500)).is_ok() {}
        rescan(&dir);
    }
}

/// Where plugins are installed on this device.
pub fn plugins_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("pimble").join("plugins")
}

fn approvals_path() -> PathBuf {
    plugins_dir().join("approvals.json")
}

fn provenance_path() -> PathBuf {
    plugins_dir().join(library::PROVENANCE_FILE)
}

pub fn read_provenance() -> Provenance {
    std::fs::read(provenance_path()).map(|bytes| Provenance::parse(&bytes)).unwrap_or_default()
}

pub fn write_provenance(json: &[u8]) {
    let path = provenance_path();
    let written = std::fs::create_dir_all(plugins_dir()).and_then(|_| std::fs::write(&path, json));
    if let Err(e) = written {
        tracing::warn!("plugins: {} could not be saved: {e}", path.display());
    }
}

/// pimble.app's library, or another service's (`PIMBLE_LIBRARY_URL`, for
/// the local stack).
pub fn library_base() -> String {
    std::env::var("PIMBLE_LIBRARY_URL").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| "https://pimble.app/api/v1/plugins".to_string())
}

/// Fetch `url` off the UI thread; `done` hears the bytes on it.
pub fn fetch(url: String, done: impl FnOnce(Result<Vec<u8>, String>) + Send + 'static) {
    let spawned = std::thread::Builder::new().name("pimble-library".into()).spawn(move || {
        let fetched = (|| {
            let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(120)).build().map_err(|e| e.to_string())?;
            let resp = client.get(&url).send().map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("HTTP {}", resp.status().as_u16()));
            }
            resp.bytes().map(|b| b.to_vec()).map_err(|e| e.to_string())
        })();
        rinch::core::reactive::run_on_main_thread(move || done(fetched));
    });
    if let Err(e) = spawned {
        tracing::warn!("plugins: could not start a download: {e}");
    }
}

/// A verified download from the library: loaded (it must be the plugin
/// the index listed) and put in the folder as "Add from a file..." puts one.
pub fn install(entry: Entry, bytes: Vec<u8>) {
    let dir = plugins_dir();
    let spawned = std::thread::Builder::new().name("pimble-plugin-install".into()).spawn(move || {
        let put = load_bytes(Path::new(&entry.wasm), &bytes, DEFAULT_BUDGET)
            .map_err(|refused| refused.sentence)
            .and_then(|plugin| library::matches(&entry, &plugin.manifest).map(|_| plugin))
            .and_then(|plugin| put(&dir, &bytes, &plugin.manifest));
        match put {
            Ok(()) => {
                rescan(&dir);
                rinch::core::reactive::run_on_main_thread(move || installed_from_library(&entry));
            }
            Err(e) => rinch::core::reactive::run_on_main_thread(move || install_failed(&entry, e)),
        }
    });
    if let Err(e) = spawned {
        tracing::warn!("plugins: could not start installing: {e}");
    }
}

pub fn read_approvals() -> HashMap<String, Approval> {
    std::fs::read(approvals_path()).map(|bytes| parse_approvals(&bytes)).unwrap_or_default()
}

pub fn write_approvals(json: &[u8]) {
    let path = approvals_path();
    let written = std::fs::create_dir_all(plugins_dir()).and_then(|_| std::fs::write(&path, json));
    if let Err(e) = written {
        tracing::warn!("plugins: the approvals could not be saved to {}: {e}", path.display());
    }
}

pub fn load() {
    let dir = plugins_dir();
    let spawned = std::thread::Builder::new().name("pimble-plugins".into()).spawn(move || {
        let (registry, changed) = scan(&dir);
        tracing::info!("plugins: {} loaded and {} refused from {}", registry.plugins.len(), registry.refused.len(), dir.display());
        rinch::core::reactive::run_on_main_thread(move || {
            crate::startup::mark("plugins loaded");
            swapped(registry, &changed)
        });
        watch(dir);
    });
    if let Err(e) = spawned {
        tracing::warn!("plugins: could not start loading: {e}");
    }
}

/// A wasmtime instance is made on the spot.
pub fn ready(_plugin: &Plugin, _store: AppStore) -> bool {
    true
}

pub fn instantiate(plugin: &Plugin, key: HostKey) -> Result<Box<dyn GuestEngine>, Failure> {
    plugin.instantiate(key, DEFAULT_BUDGET)
}

/// "Add from a file...": a `.wasm` the person picks, checked (it must
/// load), then copied into the folder: over the file that holds the same
/// plugin now, so a new version replaces the old, else as `<id>.wasm`.
pub fn pick() {
    let Some(picked) = rinch::dialogs::open_file().set_title("Add Plugin").add_filter("Pimble plugins", &["wasm"]).pick_file() else {
        return;
    };
    let dir = plugins_dir();
    let spawned = std::thread::Builder::new().name("pimble-plugin-add".into()).spawn(move || {
        let (id, line) = add(&dir, &picked);
        rinch::core::reactive::run_on_main_thread(move || {
            if let Some(id) = id {
                forget_library(&id, false);
            }
            say(line);
        });
    });
    if let Err(e) = spawned {
        tracing::warn!("plugins: could not start adding: {e}");
    }
}

/// The plugin added (by id, when one was) and the sentence saying so.
fn add(dir: &Path, picked: &Path) -> (Option<String>, String) {
    let file = picked.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let bytes = match std::fs::read(picked) {
        Ok(bytes) => bytes,
        Err(e) => return (None, format!("{file} could not be read: {e}.")),
    };
    let plugin = match load_bytes(picked, &bytes, DEFAULT_BUDGET) {
        Ok(plugin) => plugin,
        Err(refused) => return (None, refused.sentence),
    };
    let m = &plugin.manifest;
    if let Err(e) = put(dir, &bytes, m) {
        return (None, e);
    }
    rescan(dir);
    (Some(m.id.clone()), format!("Added the {} plugin {}.", m.name, m.version))
}

/// Write `bytes`, a plugin that loaded as `manifest`, into the folder as
/// `<id>.wasm` (beside it and renamed over it, so the watcher never reads
/// half a plugin), and take away every other file that held the same plugin
/// (an earlier version, one built for another ABI, `scripts/plugins/install.sh`'s
/// old `kanban_viewer.wasm`): one plugin is one file.
fn put(dir: &Path, bytes: &[u8], manifest: &Manifest) -> Result<(), String> {
    let id = &manifest.id;
    let dest = dir.join(format!("{id}.wasm"));
    let others: Vec<PathBuf> = files_of(id).into_iter().filter(|p| *p != dest).collect();
    if std::fs::read(&dest).is_ok_and(|have| have == bytes) {
        // The same bytes are there already, and they just loaded: whatever the
        // folder's last scan made of the file (refused once, say, when its
        // description ran past its time under load) is judged again.
        CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_mut().map(|cache| cache.remove(&dest));
    } else {
        let partial = dest.with_extension("wasm.partial");
        let written = std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&partial, bytes)).and_then(|_| std::fs::rename(&partial, &dest));
        written.map_err(|e| {
            let _ = std::fs::remove_file(&partial);
            format!("The {} plugin could not be put in {}: {e}.", manifest.name, dir.display())
        })?;
    }
    for other in others {
        if let Err(e) = std::fs::remove_file(&other) {
            tracing::warn!("plugins: {} (an earlier file of {id}) could not be removed: {e}", other.display());
        }
    }
    Ok(())
}

/// "Remove": the plugin's files leave the folder (a refused one's too), with
/// what it was allowed.
pub fn remove(id: &str) {
    let files = files_of(id);
    if files.is_empty() {
        return;
    }
    let name = manifest_of(id).map_or(id.to_string(), |m| m.name);
    // Every file is tried: one that will not go does not keep the others.
    let failed: Vec<String> = files
        .iter()
        .filter_map(|path| std::fs::remove_file(path).err().map(|e| format!("{} ({e})", path.display())))
        .collect();
    let dir = plugins_dir();
    let _ = std::thread::Builder::new().name("pimble-plugin-remove".into()).spawn(move || rescan(&dir));
    if !failed.is_empty() {
        // Something of it stays, so it keeps what it was allowed and where it
        // came from; Remove again finishes the job.
        return say(format!("The {name} plugin could not be removed: {} could not be deleted.", failed.join(", ")));
    }
    APPROVALS.with(|a| a.borrow_mut().remove(id));
    super::write_approvals();
    forget_library(id, true);
    say(format!("Removed the {name} plugin."));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_its_plugin_by_its_manifest_or_its_name() {
        let head = pimble_plugin_host::abi::ManifestHead { id: "kanban-viewer".into(), name: "Kanban".into(), version: "0.1.2".into(), abi: 6 };
        let old: Loaded = Err(NotLoaded::headed(head, "Old.".into()));
        assert_eq!(id_of(Path::new("/p/whatever.wasm"), &old), "kanban-viewer");
        let junk: Loaded = Err(NotLoaded::new("Junk.".into()));
        assert_eq!(id_of(Path::new("/p/todo_viewer.wasm"), &junk), "todo-viewer");
    }
}
