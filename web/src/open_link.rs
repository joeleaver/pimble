//! `/app/?open=<pimble: link>` (docs/SETTINGS_CONTRACT.md section 12: the
//! website's "Open in the web app"): kept until the app is up, then opened as
//! the desktop opens a link it is started with.

/// Where a `?open=` link waits while the visitor signs in: this tab's session
/// storage, so it survives the trip to the sign-in page.
const OPEN_KEY: &str = "pimble.open";

/// Keep this page's `?open=` link, if it has one.
pub(crate) fn keep_open_link() {
    let Some(link) = crate::util::query_pairs().into_iter().find(|(k, _)| k == "open").map(|(_, v)| v) else { return };
    if let Some(storage) = web_sys::window().and_then(|w| w.session_storage().ok().flatten()) {
        let _ = storage.set_item(OPEN_KEY, &link);
    }
}

/// The kept link, once: taking it forgets it.
pub(crate) fn take_open_link() -> Option<String> {
    let storage = web_sys::window()?.session_storage().ok().flatten()?;
    let link = storage.get_item(OPEN_KEY).ok().flatten()?;
    let _ = storage.remove_item(OPEN_KEY);
    Some(link)
}
