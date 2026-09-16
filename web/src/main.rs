//! Pimble in the browser.
//!
//! One wasm binary serves everything under `/app/`: the account pages
//! (`/app/signup`, `/app/login`, `/app/account`) and the app itself — the same
//! UI as the desktop, `pimble_app` built without its `native` feature, mounted
//! through `rinch-web` instead of a window.
//!
//! They are one binary because they share something that cannot be handed
//! between pages: the account keys. Signing in derives them from the password
//! and they stay in this page's memory, never in storage and never on the wire
//! (docs/CRYPTO_CONTRACT.md). A link that reloads would lose them, so
//! `crate::route` moves between pages in place.
//!
//! The collaboration path is unchanged and deliberately so: one editor pane,
//! one `EditorHandle`, edits travelling as yrs bytes through
//! `BroadcastChanges` and `RemoteChanges`. For an encrypted store those bytes
//! are encrypted on their way out and decrypted on their way in
//! (`crate::vault`), and the editor never learns the difference.

mod accounts;
mod api;
mod app_page;
mod backend;
mod endpoints;
mod http;
mod keys;
mod menu;
mod pages;
mod route;
mod session;
mod shortcuts;
mod util;
mod vault;

use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    // `pimble-app`, `pimble-client` and this crate log through `tracing`; this
    // puts those lines in the browser console alongside everything else.
    tracing_wasm::set_as_global_default();
    let _ = console_log::init_with_level(log::Level::Info);

    shortcuts::install();
    route::start();
}

fn main() {
    // The entry point is `start()`, through #[wasm_bindgen(start)].
}
