//! The Pimble server in a browser's Web Worker (docs/ONE_CODEBASE_CONTRACT.md
//! section 4 and wave 3): the same `pimble-server` the desktop runs, built for
//! `wasm32-unknown-unknown` and handed the worker's adapters (a memory disk
//! until OPFS in wave 4, `LocalRuntime`, `fetch`), serving the transport-free
//! dispatch over `MessagePort`s with the `Service` principal the desktop's
//! loopback app has.
//!
//! A crate of its own rather than a module of `web/`: it is a host of the
//! server, as the desktop app's backend is, and `web/` holds the page; the
//! worker's module is built from this crate (a `cdylib`) and the page's from
//! `web/`, and neither links the other's code.
//!
//! How a page starts it (wave 4: the leader tab):
//!
//! ```js
//! // worker.js, a module worker: new Worker("worker.js", { type: "module" })
//! import init, { startServer } from "./pimble_worker.js";
//! await init();
//! const server = await startServer();
//! self.onmessage = (event) => {
//!     // The page sends one end of a MessageChannel per connection.
//!     if (event.data === "connect") server.connect(event.ports[0]);
//! };
//! ```
//!
//! and the page keeps the other end: `PimbleClient::over_message_port(port)`.
//! Natively this crate is empty.

#[cfg(target_arch = "wasm32")]
mod worker;

#[cfg(target_arch = "wasm32")]
pub use worker::*;
