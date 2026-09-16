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

/// Build the app into its host, once.
pub fn start() {
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
        store.backend.set(Some(backend::spawn(session_token)));
        route::mount_app(view);
    });
}
