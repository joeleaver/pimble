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
//! How a page starts it (wave 4): the leader tab (`web/hub.js`) starts a
//! dedicated worker from the loader trunk writes for this crate built with the
//! `entry` feature; `entry.rs` then answers the page's messages (`start`,
//! `connect` with a port, `stop`, `forget`). The server is kept on OPFS sealed
//! with the remembered device's disk key, or on memory ("don't remember this
//! browser"): `worker.rs` [`start_browser`], `device.rs`. The page keeps the
//! other end of each port: `PimbleClient::over_message_port(port)`.
//! Natively this crate is empty.

#[cfg(target_arch = "wasm32")]
mod device;
#[cfg(all(target_arch = "wasm32", feature = "entry"))]
mod entry;
#[cfg(target_arch = "wasm32")]
mod worker;

#[cfg(target_arch = "wasm32")]
pub use worker::*;
