//! Pimble in the browser.
//!
//! One wasm binary serves everything under `/app/`: the account pages
//! (`/app/signup`, `/app/login`, `/app/account`, ...) and the app itself — the
//! same UI as the desktop, `pimble_app` built without its `native` feature,
//! mounted through `rinch-web` instead of a window.
//!
//! Behind the UI runs the same Pimble server the desktop embeds, in a worker
//! of this origin's leader tab (`crate::worker_mode`, docs/ONE_CODEBASE_CONTRACT.md).
//! It holds the account's keys and this browser's replicas of the stores, and
//! the page reaches it through the same `pimble_api::Pimble` and
//! `pimble_app::commands` the desktop uses, so there is one path for every
//! read and write.
//!
//! The collaboration path is the desktop's: the app's four pane slots, an
//! `EditorHandle` each, edits travelling as yrs bytes through
//! `BroadcastChanges` and `RemoteChanges`. An encrypted store's bytes are
//! encrypted and decrypted by the server's vault link, and the editor never
//! learns the difference.

mod accounts;
mod diagnostics;
mod http;
mod kdf;
mod open_link;
mod pages;
mod route;
mod shortcuts;
mod util;
mod worker_mode;

use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    // `pimble-app`, `pimble-client` and this crate log through `tracing`; this
    // puts those lines in the browser console alongside everything else. At
    // INFO: the default (everything) lets rinch's per-effect DEBUG lines
    // through by the thousand, and the console keeps only the most recent
    // few hundred messages, so the app's own warnings were gone before anyone
    // could read them (found looking for one, 2026-09-21).
    // Beside the console, the alpha's diagnostics (docs/DIAGNOSTICS_CONTRACT.md
    // section 5), with Settings > Diagnostics reaching them.
    diagnostics::start();
    pimble_app::diagnostics_ui::set_hooks(diagnostics::hooks());
    {
        use tracing_subscriber::prelude::*;
        let console = tracing_wasm::WASMLayer::new(tracing_wasm::WASMLayerConfigBuilder::new().set_max_level(tracing::Level::INFO).build());
        let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry().with(console).with(diagnostics::Layer));
    }
    let _ = console_log::init_with_level(log::Level::Info);

    shortcuts::install();
    worker_mode::begin();
    route::start();
}

fn main() {
    // The entry point is `start()`, through #[wasm_bindgen(start)].
}
