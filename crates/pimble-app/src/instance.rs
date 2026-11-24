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
//! included: there is no pid file to go stale. One lock per server, so a
//! test app on its own port (`PIMBLE_APP_ADDR`) is the primary of its own
//! server; the address is normalized first ([`lock_key`]), so two spellings
//! of one server share one lock.
//!
//! A plain launch while a primary runs hands "come to the front" to it and
//! stops once it says it came ([`launch`], `main.rs`); `pimble --new-window`
//! and a link no running window took start a window process of their own,
//! which waits on the lock in a thread and becomes the primary if the one
//! before it quits. A primary that quits gives the lock up first
//! ([`resign`]), so the next one takes over while this one is still
//! writing what it remembers.

use std::fs::File;
use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// One process's hold on a window lock: whether it is the primary, and the
/// open lock file that makes it so.
pub(crate) struct WindowLock {
    primary: AtomicBool,
    held: Mutex<Option<File>>,
    /// The process is quitting: a lock its waiting thread gets now is let go
    /// at once rather than taken over.
    resigned: AtomicBool,
}

/// This process's.
static LOCK: WindowLock = WindowLock::new();

impl WindowLock {
    pub(crate) const fn new() -> WindowLock {
        WindowLock { primary: AtomicBool::new(false), held: Mutex::new(None), resigned: AtomicBool::new(false) }
    }

    pub(crate) fn is_primary(&self) -> bool {
        self.primary.load(Ordering::SeqCst)
    }

    /// Try to become the primary now; whether this is. A lock file that
    /// cannot be opened at all (a read-only config folder) makes this the
    /// primary: one that could show nothing would be worse than a rare
    /// second icon.
    pub(crate) fn claim(&self, path: &Path) -> bool {
        let file = match open_lock(path) {
            Ok(file) => file,
            Err(e) => {
                tracing::warn!("The window lock {} could not be opened ({e}); this window owns the tray", path.display());
                self.primary.store(true, Ordering::SeqCst);
                return true;
            }
        };
        if file.try_lock().is_err() {
            return false;
        }
        self.hold(file)
    }

    /// Keep `file`'s lock and become the primary, unless this process
    /// resigned: then the file is dropped and the lock goes at once. Judged
    /// under the same guard [`resign`](Self::resign) takes, so a resign
    /// that lands while the waiting thread is taking the lock is never
    /// undone by it; whether this became the primary.
    fn hold(&self, file: File) -> bool {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        if self.resigned.load(Ordering::SeqCst) {
            return false;
        }
        *held = Some(file);
        self.primary.store(true, Ordering::SeqCst);
        true
    }

    /// Whether this process resigned: it is quitting.
    pub(crate) fn is_resigned(&self) -> bool {
        self.resigned.load(Ordering::SeqCst)
    }

    /// Wait for the lock in a thread of its own; when its holder quits (or
    /// crashes) this takes it and `became` runs, on that thread. Nothing
    /// happens when this is the primary already, or once it resigned.
    pub(crate) fn wait(&'static self, path: PathBuf, became: impl FnOnce() + Send + 'static) {
        if self.is_primary() {
            return;
        }
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
            if !self.hold(file) {
                return;
            }
            tracing::info!("The window that owned the tray quit; this one owns it now");
            became();
        });
        if let Err(e) = spawned {
            tracing::warn!("Could not wait for the window lock ({e}); this window stays without a tray");
        }
    }

    /// This process is quitting: it shows nothing more and lets the lock go
    /// now, so a window waiting for it takes over at once.
    pub(crate) fn resign(&self) {
        let file = {
            let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
            self.resigned.store(true, Ordering::SeqCst);
            self.primary.store(false, Ordering::SeqCst);
            held.take()
        };
        drop(file);
    }
}

/// What a launch ended in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    /// Open a window in this process.
    OpenWindow,
    /// The running primary said it came to the front; this process stops.
    HandedOver,
}

/// The decision a launch makes, given what it was asked for and whether it
/// took the lock. A launch that wants a window of its own (`--new-window`, or
/// a link no running window took) opens one, and so does one that became the
/// primary, there being nobody to hand over to; neither asks anyone. Any
/// other launch asks the primary to come to the front (`hand_over`, which
/// answers whether that window said it did) and stops if it did, else opens
/// a window here: a primary that is hung or partway through quitting never
/// swallows the launch.
pub fn launch(wants_own_window: bool, primary: bool, hand_over: impl FnOnce() -> bool) -> Launch {
    if wants_own_window || primary {
        return Launch::OpenWindow;
    }
    if hand_over() {
        tracing::info!("Pimble is already running; it came to the front");
        Launch::HandedOver
    } else {
        tracing::info!("Pimble is running but no window came to the front; opening one here");
        Launch::OpenWindow
    }
}

/// Whether this process is the primary: it owns the tray icon and shows
/// notifications.
pub fn is_primary() -> bool {
    LOCK.is_primary()
}

/// Try to become the primary now ([`WindowLock::claim`] on this server's
/// lock); whether this process is.
pub fn claim() -> bool {
    LOCK.claim(&lock_path())
}

/// A window process that is not the primary waits for the lock in a thread
/// of its own; when the primary quits (or crashes) this one takes it, and
/// `became` runs on the main thread (the tray goes up there).
pub fn wait_to_become_primary(became: fn()) {
    LOCK.wait(lock_path(), move || rinch::core::reactive::run_on_main_thread(became));
}

/// This process is quitting ([`WindowLock::resign`]).
pub fn resign() {
    LOCK.resign();
}

/// Whether this process is quitting: it takes no link handed to it and
/// says it took none (`appLinkTaken`), so the launch that handed it opens a
/// window of its own.
pub fn is_quitting() -> bool {
    LOCK.is_resigned()
}

/// The one spelling of a server address the lock is named after. The
/// loopback names (`localhost`, `127.0.0.1`, `::1`, bracketed or not) are
/// this computer's one server on that port, `127.0.0.1:<port>`; any other
/// address is kept as it parses (another loopback address such as
/// `127.0.0.2` can be another server, so it is not folded in); a host name
/// is resolved, to the lowest address when it names several. Something that
/// is no address at all is kept as written. Resolving a name can block, so
/// this is worked out once per process ([`lock_path`]).
pub(crate) fn lock_key(addr: &str) -> String {
    if let Some(port) = loopback_port(addr) {
        return SocketAddr::from((Ipv4Addr::LOCALHOST, port)).to_string();
    }
    if let Ok(one) = addr.parse::<SocketAddr>() {
        return one.to_string();
    }
    let mut resolved: Vec<SocketAddr> = addr.to_socket_addrs().map(|all| all.collect()).unwrap_or_default();
    resolved.sort();
    resolved.first().map_or_else(|| addr.to_string(), SocketAddr::to_string)
}

/// The port of `host:port` when the host is one of the loopback names.
fn loopback_port(addr: &str) -> Option<u16> {
    let (host, port) = addr.rsplit_once(':')?;
    let port = port.parse::<u16>().ok()?;
    let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    (host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1").then_some(port)
}

/// The lock file's name for a normalized server address: `127.0.0.1:7462`
/// is `app-127.0.0.1-7462.lock`, `[2001:db8::1]:7473` is
/// `app-2001_db8__1-7473.lock`.
pub(crate) fn lock_name(key: &str) -> String {
    let safe: String = key
        .chars()
        .filter(|c| !matches!(c, '[' | ']'))
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else if c == ':' { '_' } else { '-' })
        .collect();
    // The port is after the last `_`: written with a dash so the name reads
    // as address-port.
    match safe.rfind('_') {
        Some(i) => format!("app-{}-{}.lock", &safe[..i], &safe[i + 1..]),
        None => format!("app-{safe}.lock"),
    }
}

/// Where this process's server's lock lives: worked out the first time it
/// is asked for (the claim, before the window opens) and kept, so no notice
/// shown later ([`crate::shown_notices`]) waits on a name lookup.
pub(crate) fn lock_path() -> PathBuf {
    static PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let config = pimble_server::local::state_file_path();
        let dir = config.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        dir.join(lock_name(&lock_key(&pimble_server::local::server_addr())))
    })
    .clone()
}

/// Open (creating) the lock file at `path`.
fn open_lock(path: &Path) -> std::io::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    File::options().create(true).truncate(false).write(true).open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::time::Duration;

    fn lock_in(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("pimble").join(lock_name("127.0.0.1:1"))
    }

    fn leaked() -> &'static WindowLock {
        Box::leak(Box::new(WindowLock::new()))
    }

    #[test]
    fn a_plain_launch_hands_over_and_stops_when_the_primary_came_to_the_front() {
        let asked = Cell::new(false);
        let outcome = launch(false, false, || {
            asked.set(true);
            true
        });
        assert!(asked.get(), "the primary was asked");
        assert_eq!(outcome, Launch::HandedOver);
    }

    /// A primary that hears the launch and never says it came (hung, or
    /// partway through quitting) does not swallow it.
    #[test]
    fn a_plain_launch_no_window_came_for_opens_its_own() {
        assert_eq!(launch(false, false, || false), Launch::OpenWindow);
    }

    #[test]
    fn the_primary_and_a_new_window_open_without_asking_anyone() {
        for (wants_own, primary) in [(false, true), (true, false), (true, true)] {
            let outcome = launch(wants_own, primary, || panic!("nobody is asked"));
            assert_eq!(outcome, Launch::OpenWindow);
        }
    }

    #[test]
    fn one_server_has_one_lock_whatever_its_address_is_spelled() {
        let one = lock_key("127.0.0.1:7462");
        assert_eq!(one, "127.0.0.1:7462");
        for spelling in ["localhost:7462", "LOCALHOST:7462", "[::1]:7462", "::1:7462"] {
            assert_eq!(lock_key(spelling), one, "{spelling}");
        }
        assert_ne!(lock_key("127.0.0.1:7473"), one, "another port is another server");
        assert_eq!(lock_key("192.0.2.5:7462"), "192.0.2.5:7462", "an address elsewhere is kept");
        assert_eq!(lock_key("not an address"), "not an address");
    }

    /// Only the loopback names are folded into one: another loopback
    /// address, or the unspecified one, can be a server of its own.
    #[test]
    fn only_the_loopback_names_are_one_host() {
        assert_eq!(lock_key("127.0.0.2:7462"), "127.0.0.2:7462");
        assert_eq!(lock_key("0.0.0.0:7462"), "0.0.0.0:7462");
        assert_eq!(lock_key("[::2]:7462"), "[::2]:7462");
        assert_eq!(loopback_port("localhost:notaport"), None);
    }

    #[test]
    fn the_lock_is_named_after_the_server_address() {
        assert_eq!(lock_name("127.0.0.1:7462"), "app-127.0.0.1-7462.lock");
        assert_eq!(lock_name("127.0.0.1:7473"), "app-127.0.0.1-7473.lock");
        assert_eq!(lock_name("[2001:db8::1]:7473"), "app-2001_db8__1-7473.lock");
        assert!(!lock_name("[2001:db8::1]:7473").contains([':', '[', ']']));
    }

    /// The first to claim is the primary; a second (another process's open
    /// of the same file: a lock is per open file) is not, until the first
    /// resigns.
    #[test]
    fn one_primary_at_a_time() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = lock_in(&dir);
        let (first, second) = (leaked(), leaked());
        assert!(first.claim(&path), "nobody holds it yet");
        assert!(first.is_primary());
        assert!(!second.claim(&path), "held by the first");
        assert!(!second.is_primary());
        first.resign();
        assert!(!first.is_primary());
        assert!(second.claim(&path), "free once the first resigned");
    }

    /// A window waiting for the lock takes over when its holder goes, and
    /// not before.
    #[test]
    fn a_waiting_window_takes_over_when_the_primary_goes() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = lock_in(&dir);
        let (primary, waiting) = (leaked(), leaked());
        assert!(primary.claim(&path));
        assert!(!waiting.claim(&path));
        let (tx, rx) = std::sync::mpsc::channel();
        waiting.wait(path.clone(), move || tx.send(()).unwrap());
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "not while the primary holds it");
        assert!(!waiting.is_primary());
        primary.resign();
        rx.recv_timeout(Duration::from_secs(5)).expect("took over");
        assert!(waiting.is_primary());
        assert!(!leaked().claim(&path), "and holds it");
    }

    /// The waiting thread's take and a resign judged under one guard: once
    /// resigned, a lock taken after it is let go, never held.
    #[test]
    fn a_lock_taken_after_the_resign_is_not_held() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = lock_in(&dir);
        let quitting = leaked();
        quitting.resign();
        assert!(quitting.is_resigned());
        let file = open_lock(&path).unwrap();
        file.try_lock().unwrap();
        assert!(!quitting.hold(file), "resigned: not held");
        assert!(!quitting.is_primary());
        assert!(leaked().claim(&path), "and the lock went at once");
    }

    /// A process that resigned while it waited (it is quitting) does not take
    /// the lock over when it comes free: the next window gets it.
    #[test]
    fn a_window_that_resigned_while_waiting_lets_the_lock_go() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = lock_in(&dir);
        let (primary, quitting) = (leaked(), leaked());
        assert!(primary.claim(&path));
        let (tx, rx) = std::sync::mpsc::channel();
        quitting.wait(path.clone(), move || tx.send(()).unwrap());
        quitting.resign();
        primary.resign();
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err(), "never became the primary");
        assert!(!quitting.is_primary());
        let next = leaked();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !next.claim(&path) {
            assert!(std::time::Instant::now() < deadline, "the lock came free");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
