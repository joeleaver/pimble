//! Which Pimble window process on this computer is the primary one
//! (docs/NOTIFICATIONS_CONTRACT.md, "Built in wave 4"): the one that owns
//! the tray icon and shows the operating system's notifications, so that
//! two window processes on one server never show two icons or the same
//! notice twice.
//!
//! The primary is whichever process holds an exclusive lock on
//! `<config dir>/pimble/app-<server address>.lock`. The lock is the
//! operating system's (`flock` on Linux, `LockFileEx` on Windows), held
//! through an open file, so it goes when the process does, a crash
//! included: there is no pid file to go stale. One lock per server address,
//! so a test app on its own port (`PIMBLE_APP_ADDR`) is the primary of its
//! own server.
//!
//! A plain launch while a primary runs hands "come to the front" to it and
//! stops (`main.rs`); `pimble --new-window` and a link no running window took
//! start a window process of their own, which waits on the lock in a thread
//! and becomes the primary if the one before it quits.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Whether this process holds the lock.
static PRIMARY: AtomicBool = AtomicBool::new(false);

/// The open lock file, held for the life of the process once it is ours.
static HELD: Mutex<Option<File>> = Mutex::new(None);

/// What a launch does, given what it was asked for and whether it took the
/// lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    /// Open a window in this process.
    Start,
    /// Ask the running primary to come to the front, and stop if it does.
    HandOver,
}

/// A launch that wants a window of its own (`--new-window`, or a link no
/// running window took) starts one; so does one that became the primary,
/// there being nobody to hand over to. Any other launch hands over.
pub fn launch(wants_own_window: bool, primary: bool) -> Launch {
    if wants_own_window || primary {
        Launch::Start
    } else {
        Launch::HandOver
    }
}

/// Whether this process is the primary: it owns the tray icon and shows
/// notifications.
pub fn is_primary() -> bool {
    PRIMARY.load(Ordering::SeqCst)
}

/// The lock file's name for a server address: `127.0.0.1:7462` is
/// `app-127.0.0.1-7462.lock`, `[::1]:7473` is `app-__1-7473.lock`.
pub(crate) fn lock_name(addr: &str) -> String {
    let safe: String = addr
        .chars()
        .filter(|c| !matches!(c, '[' | ']'))
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else if c == ':' { '_' } else { '-' })
        .collect();
    // The port is the last `_` (IPv4) or the one after the address (IPv6):
    // written with a dash so the name reads as address-port.
    match safe.rfind('_') {
        Some(i) => format!("app-{}-{}.lock", &safe[..i], &safe[i + 1..]),
        None => format!("app-{safe}.lock"),
    }
}

/// Where this process's server's lock lives.
fn lock_path() -> PathBuf {
    let config = pimble_server::local::state_file_path();
    let dir = config.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    dir.join(lock_name(&pimble_server::local::server_addr()))
}

/// Open (creating) the lock file at `path`.
pub(crate) fn open_lock(path: &Path) -> std::io::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    File::options().create(true).truncate(false).write(true).open(path)
}

/// Take the lock in `file` if nobody holds it: whether it is now held.
pub(crate) fn try_take(file: &File) -> bool {
    file.try_lock().is_ok()
}

/// Try to become the primary now; whether this process is. A lock file
/// that cannot be opened at all (a read-only config folder) makes this
/// process the primary: one that could show nothing would be worse than a
/// rare second icon.
pub fn claim() -> bool {
    let path = lock_path();
    let file = match open_lock(&path) {
        Ok(file) => file,
        Err(e) => {
            tracing::warn!("The window lock {} could not be opened ({e}); this window owns the tray", path.display());
            PRIMARY.store(true, Ordering::SeqCst);
            return true;
        }
    };
    if !try_take(&file) {
        return false;
    }
    hold(file);
    true
}

fn hold(file: File) {
    *HELD.lock().unwrap_or_else(|e| e.into_inner()) = Some(file);
    PRIMARY.store(true, Ordering::SeqCst);
}

/// A window process that is not the primary waits for the lock in a thread
/// of its own; when the primary quits (or crashes) this one takes it, and
/// `became` runs on the main thread (the tray goes up there).
pub fn wait_to_become_primary(became: fn()) {
    if is_primary() {
        return;
    }
    let path = lock_path();
    let spawned = std::thread::Builder::new().name("pimble-window-lock".into()).spawn(move || {
        let file = match open_lock(&path) {
            Ok(file) => file,
            Err(e) => {
                tracing::warn!("The window lock {} could not be opened ({e}); this window stays without a tray", path.display());
                return;
            }
        };
        if let Err(e) = file.lock() {
            tracing::warn!("Waiting for the window lock failed ({e}); this window stays without a tray");
            return;
        }
        hold(file);
        tracing::info!("The window that owned the tray quit; this one owns it now");
        rinch::core::reactive::run_on_main_thread(became);
    });
    if let Err(e) = spawned {
        tracing::warn!("Could not wait for the window lock ({e}); this window stays without a tray");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_launch_hands_over_unless_it_is_the_primary() {
        assert_eq!(launch(false, false), Launch::HandOver);
        assert_eq!(launch(false, true), Launch::Start);
    }

    #[test]
    fn a_new_window_or_an_untaken_link_starts_its_own_window() {
        assert_eq!(launch(true, false), Launch::Start);
        assert_eq!(launch(true, true), Launch::Start);
    }

    #[test]
    fn the_lock_is_named_after_the_server_address() {
        assert_eq!(lock_name("127.0.0.1:7462"), "app-127.0.0.1-7462.lock");
        assert_eq!(lock_name("127.0.0.1:7473"), "app-127.0.0.1-7473.lock");
        assert_ne!(lock_name("127.0.0.1:7462"), lock_name("127.0.0.1:7473"));
        assert!(!lock_name("[::1]:7473").contains([':', '[', ']']));
    }

    /// One holder at a time, and the lock goes with its holder: an open file
    /// closed is what a process exiting or crashing does to it.
    #[test]
    fn one_holder_at_a_time_and_released_when_the_holder_goes() {
        let dir = std::env::temp_dir().join(format!("pimble-instance-test-{}", std::process::id()));
        let path = dir.join(lock_name("127.0.0.1:1"));
        let first = open_lock(&path).unwrap();
        assert!(try_take(&first), "nobody holds it yet");
        let second = open_lock(&path).unwrap();
        assert!(!try_take(&second), "held by the first");
        drop(first);
        assert!(try_take(&second), "free once its holder is gone");
        drop(second);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
