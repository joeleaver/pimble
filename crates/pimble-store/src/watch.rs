//! Watching store folders for what other computers write into them
//! (docs/STORE_FOLDERS_CONTRACT.md rule 3).
//!
//! One `notify` watcher for the whole process, whatever the number of open
//! stores and servers: each watcher is an inotify instance on Linux, and the
//! default limit is 128 per user, shared with every other program (a test
//! binary starts dozens of servers at once). A store's folder is watched in
//! three places, none with recursion: `nodes/` (the documents and the
//! conflict copies beside them), the folder itself (`manifest.json`) and
//! `computers/` (who else has it open).
//! Whoever asks gets the changed paths on a channel; reading them, merging
//! and debouncing are theirs (`LocalStore::absorb_folder_paths`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tracing::warn;

#[derive(Default)]
struct Routes {
    next: u64,
    /// Watched directory -> who hears about changes in it. Two servers in
    /// one process (a test) may watch the same folder, so a list.
    by_dir: HashMap<PathBuf, Vec<(u64, UnboundedSender<PathBuf>)>>,
}

/// Two locks, and `routes` is never held across a call into the watcher:
/// `notify`'s `watch` and `unwatch` hand the request to the watcher's own
/// thread and wait for its answer, and that thread takes `routes` to deliver
/// an event, so holding `routes` across either deadlocked a test's server
/// the moment an event arrived meanwhile (found 2026-11-19). The watcher
/// lock alone orders watching against unwatching.
struct Global {
    watcher: Mutex<notify::RecommendedWatcher>,
    routes: Arc<Mutex<Routes>>,
}

static GLOBAL: OnceLock<Option<Global>> = OnceLock::new();

fn global() -> Option<&'static Global> {
    GLOBAL
        .get_or_init(|| {
            let routes: Arc<Mutex<Routes>> = Arc::default();
            let heard = Arc::clone(&routes);
            let made = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let Ok(event) = event else { return };
                if matches!(event.kind, EventKind::Access(_)) {
                    return;
                }
                let routes = heard.lock().unwrap();
                for path in event.paths {
                    let Some(dir) = path.parent() else { continue };
                    if let Some(listeners) = routes.by_dir.get(dir) {
                        for (_, tx) in listeners {
                            let _ = tx.send(path.clone());
                        }
                    }
                }
            });
            match made {
                Ok(watcher) => Some(Global { watcher: Mutex::new(watcher), routes }),
                Err(e) => {
                    warn!("Store folders cannot be watched ({}); changes other computers make arrive only when a document is written or the store reopens", e);
                    None
                }
            }
        })
        .as_ref()
}

/// A store folder being watched; dropping it stops the watch.
pub struct FolderWatch {
    token: u64,
    dirs: Vec<PathBuf>,
}

/// Watch the store folder at `store_path` (`nodes/`, `computers/` and the
/// folder itself):
/// every path that changes in either arrives on the receiver, as often as
/// the system reports it (a sync tool's burst is many). `None` when watching
/// is not possible here; the caller carries on without it.
pub fn watch_folder(store_path: &Path) -> Option<(FolderWatch, UnboundedReceiver<PathBuf>)> {
    let global = global()?;
    let (tx, rx) = unbounded_channel();
    let dirs = vec![store_path.join("nodes"), store_path.to_path_buf(), store_path.join(crate::LocalStore::COMPUTERS_DIR)];
    let mut watcher = global.watcher.lock().unwrap();
    let (token, fresh) = {
        let mut routes = global.routes.lock().unwrap();
        routes.next += 1;
        let token = routes.next;
        let mut fresh = Vec::new();
        for dir in &dirs {
            let listeners = routes.by_dir.entry(dir.clone()).or_default();
            if listeners.is_empty() {
                fresh.push(dir.clone());
            }
            listeners.push((token, tx.clone()));
        }
        (token, fresh)
    };
    for dir in fresh {
        if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
            warn!("Could not watch {}: {}", dir.display(), e);
        }
    }
    Some((FolderWatch { token, dirs }, rx))
}

impl Drop for FolderWatch {
    fn drop(&mut self) {
        let Some(global) = global() else { return };
        let mut watcher = global.watcher.lock().unwrap();
        let mut gone = Vec::new();
        {
            let mut routes = global.routes.lock().unwrap();
            for dir in &self.dirs {
                let Some(listeners) = routes.by_dir.get_mut(dir) else { continue };
                listeners.retain(|(token, _)| *token != self.token);
                if listeners.is_empty() {
                    routes.by_dir.remove(dir);
                    gone.push(dir.clone());
                }
            }
        }
        for dir in gone {
            let _ = watcher.unwatch(&dir);
        }
    }
}
