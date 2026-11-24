//! The notices a window process showed as the system's notifications, so
//! that one taking the tray over (`crate::instance`) shows the rest, once
//! (docs/NOTIFICATIONS_CONTRACT.md "Built in wave 4"). Desktop only.
//!
//! Every window process hears every notice, but only the primary shows one.
//! Between the primary quitting and a waiting window becoming the primary, a
//! notice can reach the waiting one while it is not yet the primary, and
//! nobody would show it. So a window that is not the primary keeps the ids
//! of the notices it did not show ([`held_back`]), the primary writes the id
//! of each notice it shows beside the window lock
//! (`app-<server address>.shown`, the last [`KEPT`]), and the window taking
//! over shows the ones it held back that the old primary never wrote and the
//! person has not opened ([`to_show`]). A notice the old primary showed is
//! in its file, so it is never shown twice.
//!
//! The file is the record of one computer's windows between themselves, not
//! the person's data: nothing syncs it.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

/// How many shown ids the file keeps, and how many held-back ids a window
/// keeps: far more than can arrive while one window hands the tray to
/// another.
pub(crate) const KEPT: usize = 200;

thread_local! {
    /// The notices this process heard while it was not the primary, oldest
    /// first.
    static HELD_BACK: std::cell::RefCell<VecDeque<String>> = const { std::cell::RefCell::new(VecDeque::new()) };
}

/// This process heard `id` and, not being the primary, did not show it.
pub(crate) fn held_back(id: &str) {
    HELD_BACK.with(|held| keep_last(&mut held.borrow_mut(), id));
}

/// The primary is about to show `id`: whether it may, and if so it is
/// written down, for itself and for whoever takes over. Every show asks
/// here first (`crate::inbox`), so a notice the old primary showed and that
/// reaches this window only once it took over is not shown again.
pub(crate) fn first_show(id: &str) -> bool {
    first_show_in(&path(), id)
}

/// [`first_show`] against the record at `path`. A record that cannot be
/// written does not stop the show: a rare second notification is better
/// than none.
pub(crate) fn first_show_in(path: &Path, id: &str) -> bool {
    if read(path).contains(id) {
        return false;
    }
    if let Err(e) = record(path, id) {
        tracing::debug!("The notice shown was not written down ({e}); a window taking over may show it again");
    }
    true
}

/// This process became the primary: the ids of the notices it held back
/// that it should show now, oldest first, and the held-back list is
/// emptied. `unread` says whether the person has yet to open one.
pub(crate) fn take_over(unread: impl Fn(&str) -> bool) -> Vec<String> {
    let held: Vec<String> = HELD_BACK.with(|held| held.borrow_mut().drain(..).collect());
    let shown = read(&path());
    to_show(&held, &shown, unread)
}

/// Of the notices held back, the ones nobody showed and nobody opened.
pub(crate) fn to_show(held: &[String], shown: &HashSet<String>, unread: impl Fn(&str) -> bool) -> Vec<String> {
    let mut seen = HashSet::new();
    held.iter().filter(|id| !shown.contains(*id) && unread(id) && seen.insert(id.as_str())).cloned().collect()
}

fn keep_last(list: &mut VecDeque<String>, id: &str) {
    if list.iter().any(|held| held == id) {
        return;
    }
    list.push_back(id.to_string());
    while list.len() > KEPT {
        list.pop_front();
    }
}

/// Beside this server's window lock (its path worked out once, no lookup
/// per notice).
fn path() -> PathBuf {
    crate::instance::lock_path().with_extension("shown")
}

/// The ids written in `path`, one per line; none when it is missing.
pub(crate) fn read(path: &Path) -> HashSet<String> {
    std::fs::read_to_string(path).map(|text| text.lines().filter(|l| !l.is_empty()).map(str::to_string).collect()).unwrap_or_default()
}

/// Add `id` to `path`, keeping the last [`KEPT`]; written whole through a
/// file beside it, so a reader never sees half of it.
pub(crate) fn record(path: &Path, id: &str) -> std::io::Result<()> {
    let mut ids: VecDeque<String> = std::fs::read_to_string(path)
        .map(|text| text.lines().filter(|l| !l.is_empty()).map(str::to_string).collect())
        .unwrap_or_default();
    keep_last(&mut ids, id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let partial = path.with_extension("shown.partial");
    let mut text = ids.into_iter().collect::<Vec<_>>().join("\n");
    text.push('\n');
    std::fs::write(&partial, text)?;
    std::fs::rename(&partial, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_window_taking_over_shows_what_nobody_showed_or_opened() {
        let held = ids(&["shown-by-the-old-primary", "missed", "opened-already", "missed-too"]);
        let shown: HashSet<String> = ["shown-by-the-old-primary".to_string()].into();
        let unread = |id: &str| id != "opened-already";
        assert_eq!(to_show(&held, &shown, unread), ids(&["missed", "missed-too"]));
    }

    #[test]
    fn a_notice_held_twice_is_shown_once() {
        let mut held = VecDeque::new();
        keep_last(&mut held, "a");
        keep_last(&mut held, "a");
        assert_eq!(held.len(), 1);
        assert_eq!(to_show(&ids(&["a", "a"]), &HashSet::new(), |_| true), ids(&["a"]));
    }

    #[test]
    fn the_record_keeps_the_last_ids_and_reads_back() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("pimble").join("app-127.0.0.1-1.shown");
        assert!(read(&path).is_empty(), "nothing written yet");
        for i in 0..KEPT + 5 {
            record(&path, &format!("n{i}")).unwrap();
        }
        record(&path, "n10").unwrap();
        let shown = read(&path);
        assert_eq!(shown.len(), KEPT);
        assert!(!shown.contains("n0"), "the oldest went");
        assert!(shown.contains("n10") && shown.contains(&format!("n{}", KEPT + 4)));
        assert!(!path.with_extension("shown.partial").exists(), "nothing half-written is left");
    }

    /// The old primary showed X and quit; this window took over, and only
    /// then did its own copy of X arrive: it is not shown again. A new one
    /// is, once.
    #[test]
    fn a_notice_shown_before_the_takeover_is_not_shown_when_it_arrives_after() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("app-127.0.0.1-1.shown");
        assert!(first_show_in(&path, "x"), "the old primary shows it");
        assert!(!first_show_in(&path, "x"), "the new primary's copy is not shown");
        assert!(first_show_in(&path, "y"));
        assert!(!first_show_in(&path, "y"), "each once");
    }

    /// The whole hand-over: what the old primary showed and wrote down is
    /// not shown again by the window taking over; what reached that window
    /// in between is.
    #[test]
    fn what_the_old_primary_wrote_down_is_not_shown_again() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("app-127.0.0.1-1.shown");
        record(&path, "before-the-quit").unwrap();
        let held = ids(&["before-the-quit", "in-the-gap"]);
        assert_eq!(to_show(&held, &read(&path), |_| true), ids(&["in-the-gap"]));
    }
}
