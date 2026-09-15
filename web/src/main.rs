//! Pimble in the browser.
//!
//! The same UI as the desktop app — `pimble_app` built without its `native`
//! feature — mounted through `rinch-web` instead of a window, and talking to a
//! hosted Pimble server instead of an embedded one. The collaboration path is
//! unchanged and deliberately so: one editor pane, one `EditorHandle`, edits
//! travelling as yrs bytes through `BroadcastChanges` and `RemoteChanges`.
//!
//! Startup is three steps: mint a token from the session cookie, start the
//! backend around it, then mount. A visitor with no session never gets as far
//! as the second.

mod api;
mod backend;

use rinch::prelude::untracked;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use api::TokenError;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    // `pimble-app` and `pimble-client` log through `tracing`; this puts those
    // lines in the browser console alongside everything else.
    tracing_wasm::set_as_global_default();
    let _ = console_log::init_with_level(log::Level::Info);

    spawn_local(async {
        let session = match api::fetch_token().await {
            Ok(session) => session,
            Err(TokenError::Unauthorized) => {
                api::go_to_login();
                return;
            }
            Err(e) => {
                show_startup_error(&format!("Pimble could not start: {}", e));
                return;
            }
        };

        tracing::info!("Signed in; the Pimble server is at {}", session.rpc_url);

        // The UI and its state. The desktop spawns its backend from inside the
        // component; here the backend exists first and is handed in, which is
        // the same seam either way.
        let (store, view) = pimble_app::app::build_view();
        store.backend.set(Some(backend::spawn(session)));

        let theme = pimble_app::app::theme_props(untracked(|| store.dark_mode.get()));
        rinch_web::mount(theme, view);
    });
}

/// Put a plain message on the page when there is no app to show.
///
/// Deliberately not a rinch tree: whatever went wrong happened before the app
/// existed, and a failure to start should not depend on the thing that failed.
fn show_startup_error(message: &str) {
    tracing::error!("{}", message);
    if let Some(body) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.body())
    {
        body.set_inner_text(message);
        let _ = body.set_attribute(
            "style",
            "font: 15px/1.5 system-ui, sans-serif; color: #c1c2c5; padding: 48px;",
        );
    }
}

fn main() {
    // The entry point is `start()`, through #[wasm_bindgen(start)].
}
