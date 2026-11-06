//! Starting the app itself: the tree, the editor, the stores.
//!
//! Three things have to be true before the app can be built. There has to be a
//! session (the cookie the accounts service set), the account keys have to be
//! unlocked in this page (they only ever live in memory, so a reload means
//! asking for the password again), and a token has to be minted for the Pimble
//! server. Anything missing sends the visitor to `/app/login`.

use wasm_bindgen_futures::spawn_local;

use crate::api::{self, TokenError};
use crate::backend;
use crate::route::{self, Route};
use crate::session;

/// Where a `?open=` link waits while the visitor signs in or unlocks: this
/// tab's session storage, so it survives the trip to the login page.
const OPEN_KEY: &str = "pimble.open";

/// `/app/?open=<pimble: link>` (docs/SETTINGS_CONTRACT.md section 12: the
/// website's "Open in the web app"): kept until the app is up, then opened as
/// the desktop opens a link it is started with.
fn keep_open_link() {
    let Some(link) = crate::util::query_pairs().into_iter().find(|(k, _)| k == "open").map(|(_, v)| v) else { return };
    if let Some(storage) = web_sys::window().and_then(|w| w.session_storage().ok().flatten()) {
        let _ = storage.set_item(OPEN_KEY, &link);
    }
}

fn take_open_link() -> Option<String> {
    let storage = web_sys::window()?.session_storage().ok().flatten()?;
    let link = storage.get_item(OPEN_KEY).ok().flatten()?;
    let _ = storage.remove_item(OPEN_KEY);
    Some(link)
}

/// Build the app into its host, once.
pub fn start() {
    keep_open_link();
    spawn_local(async move {
        let session_token = match api::fetch_token().await {
            Ok(session) => session,
            Err(TokenError::Unauthorized) => {
                route::app_start_failed();
                route::replace_with(Route::Login);
                return;
            }
            Err(e) => {
                route::app_start_failed();
                route::show_startup_error(&format!("Pimble could not start: {e}"));
                return;
            }
        };

        // The keys are what make an encrypted store readable, and nothing but
        // this page's memory has ever held them. Without them there is nothing
        // to show, so ask for the password rather than opening an app that
        // cannot decrypt anything.
        if !session::is_unlocked() {
            tracing::info!("The account is locked; asking for the password again");
            route::app_start_failed();
            route::replace_with(Route::Login);
            return;
        }

        tracing::info!("Signed in; the Pimble server is at {}", session_token.rpc_url);

        // What the account button and the menu's account items do. Registered
        // before the view is built, and navigating in place rather than
        // linking: a reload would throw away the keys this page holds.
        crate::menu::install_hooks();

        // The UI and its state. The desktop spawns its backend from inside the
        // component; here the backend exists first and is handed in, which is
        // the same seam either way.
        let (store, view) = pimble_app::app::build_view();
        // Who board edits are stamped as (docs/IMAGE_MARKUP_CONTRACT.md
        // section 6): the account this page is unlocked for.
        pimble_app::boards::set_author(session::user_id(), session::email());
        store.backend.set(Some(backend::spawn(session_token)));
        // An unlocked page is a signed-in account: its personal document
        // follows it (docs/DISCUSSIONS_CONTRACT.md section 8).
        store.cloud_signed_in.set(true);
        store.cloud_email.set(session::email().unwrap_or_default());
        pimble_app::personal::sync_now(store);
        // The name it goes by (docs/PRESENCE_CONTRACT.md section 3), which
        // stamps carry instead of the email's local part.
        store.send(pimble_app::protocol::BackendCommand::CloudListShares);
        if let Some(link) = take_open_link() {
            pimble_app::links::set_launch_link(link);
        }
        route::mount_app(store, view);
    });
}
