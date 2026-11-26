//! The browser on its own Pimble server (docs/ONE_CODEBASE_CONTRACT.md
//! sections 4 and 6, built in waves 4 and 5): the only way the web app runs.
//!
//! The server runs in a dedicated worker of the leader tab (`hub.js`,
//! [`hub`]), keeps this browser's stores in OPFS sealed with the remembered
//! device's disk key (or in memory), holds the account's keys, and is reached
//! through the same `pimble_api::Pimble` and `pimble_app::commands` the
//! desktop uses ([`backend`]). Signing in goes to it ([`pages`]).

pub mod backend;
pub mod hub;
mod account_sections;
pub mod pages;

use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;

use crate::route::{self, Route};

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// Start the hub: this tab asks for the leader's lock, and whichever tab
/// holds it runs the server. Called once, before routing.
pub fn begin() {
    tracing::info!("This browser runs its own Pimble server");
    hub::start("auto", inherited());
    // A new build took over the kept files (the service worker, hub.js): this
    // page is still the one it loaded.
    if let Some(window) = web_sys::window() {
        let ready = wasm_bindgen::closure::Closure::<dyn FnMut()>::new(|| {
            backend::local().notice("A new version of Pimble is ready: reload the page to use it.".to_string());
        });
        let _ = window.add_event_listener_with_callback("pimble-update-ready", ready.as_ref().unchecked_ref());
        ready.forget();
    }
}

/// The device this browser reported as before it ran its own server, and the
/// reports it answered, from `localStorage` (where the page's own vault client
/// kept them until wave 5 removed it: `pimble.vault-device`,
/// `pimble.vault-answered.<store>`), handed to the
/// server so the browser keeps its report slot (wave 4). JSON, or nothing
/// when this browser never had one.
fn inherited() -> Option<String> {
    let storage = storage()?;
    let device = storage.get_item("pimble.vault-device").ok().flatten().filter(|d| !d.is_empty())?;
    let mut answered = serde_json::Map::new();
    let count = storage.length().unwrap_or(0);
    for i in 0..count {
        let Some(key) = storage.key(i).ok().flatten() else { continue };
        let Some(store) = key.strip_prefix("pimble.vault-answered.") else { continue };
        let Some(value) = storage.get_item(&key).ok().flatten().and_then(|v| serde_json::from_str::<serde_json::Value>(&v).ok()) else { continue };
        answered.insert(store.to_string(), value);
    }
    tracing::info!("Handing this browser's device id and {} stores' answered reports to its server", answered.len());
    Some(serde_json::json!({ "device": device, "answered": answered }).to_string())
}

/// Ask the browser to keep this origin's storage under pressure (section 10):
/// the stores here are replicas, so eviction loses only what was not sent
/// yet. Asked beside everything else, never waited on: Firefox asks the
/// person, and its answer may take as long as they do (headless, for ever).
/// A refusal is said once in the status bar, as a notice.
pub fn ask_to_persist() {
    wasm_bindgen_futures::spawn_local(async {
        let Some(storage) = web_sys::window().map(|w| w.navigator().storage()) else { return };
        let Ok(promise) = storage.persist() else { return };
        match wasm_bindgen_futures::JsFuture::from(promise).await.map(|v| v.as_bool()) {
            Ok(Some(true)) => tracing::info!("This browser keeps Pimble's storage under pressure (persistent storage granted)"),
            _ => {
                tracing::info!("Persistent storage was not granted: this browser may clear its copy of the stores when it runs short of space");
                backend::local().notice(PERSIST_REFUSED.to_string());
            }
        }
    });
}

/// What the status bar says when persistent storage is refused.
const PERSIST_REFUSED: &str =
    "This browser may clear its copy of your stores if it runs short of space. Everything already sent is safe on Pimble Cloud.";

/// Build the app into its host, once: connected to this browser's server and
/// signed in, or to the sign-in page.
pub fn start_app() {
    crate::open_link::keep_open_link();
    crate::diagnostics::on_save(backend::diagnostics_saved);
    spawn_local(async move {
        let started = js_sys::Date::now();
        let local = backend::local();
        let client = match local.client().await {
            Ok(client) => client,
            Err(e) => {
                route::app_start_failed();
                route::show_startup_error(&format!("Pimble could not start this browser's server: {e}"));
                return;
            }
        };
        let status = match client.cloud_status().await {
            Ok(status) => status,
            Err(e) => {
                route::app_start_failed();
                route::show_startup_error(&format!("Pimble could not start: {e}"));
                return;
            }
        };
        if !status.signed_in {
            tracing::info!("Nobody is signed in to this browser's server; asking");
            route::app_start_failed();
            route::replace_with(Route::Login);
            return;
        }
        let hub = hub::status();
        tracing::info!("Signed in to this browser's server as {:?} ({} ms after the page started; this tab leads: {}, stores kept: {:?})", status.email, js_sys::Date::now() - started, hub.leading, hub.mode);

        pimble_app::app::set_account_action(|| route::go(Route::Account));
        pimble_app::app::set_sign_out_action(|| {
            spawn_local(async move {
                if let Err(e) = pages::leave(true).await {
                    tracing::error!("Signing out: {e}");
                }
                if let Some(window) = web_sys::window() {
                    let _ = window.location().replace(Route::Login.path());
                }
            });
        });

        let (store, view) = pimble_app::app::build_view();
        // The note editor's spellchecking: the browser lends rinch no
        // dictionaries, so the app fetches its own.
        pimble_app::spelling::start_web();
        store.backend.set(Some(backend::spawn()));
        if let Some(link) = crate::open_link::take_open_link() {
            pimble_app::app_links::set_launch_link(link);
        }
        // A notification or a notice email opened this page (wave 2).
        // The diagnostics notice, once, the first time this browser records.
        pimble_app::diagnostics_ui::notice_once(store);
        pimble_app::inbox::browser_push::hear_clicks(store);
        route::mount_app(store, view);
        mark_usable(started);
    });
}

/// Say in the log when the tree first has rows: the cold start's measure.
fn mark_usable(started: f64) {
    spawn_local(async move {
        for _ in 0..600 {
            let rows = web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.query_selector_all(".rinch-tree__node, [role=treeitem]").ok())
                .map(|l| l.length())
                .unwrap_or(0);
            if rows > 0 {
                tracing::info!("The tree has {rows} rows, {} ms after the app started", js_sys::Date::now() - started);
                return;
            }
            crate::util::sleep_ms(50).await;
        }
    });
}
