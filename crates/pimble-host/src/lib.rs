//! What a Pimble server needs from the machine it runs on
//! (docs/ONE_CODEBASE_CONTRACT.md section 2, seams B-E, and section 6).
//!
//! The server's core (`pimble-store`, `pimble-server`) reaches the machine only
//! through these, handed to it in its configuration, never as globals, so a
//! test, the desktop's embedded server, the browser's worker and a phone's
//! in-process server each hand their own:
//!
//! - [`Disk`]: files and directories (seam B). Native: `tokio::fs`, the same
//!   files, names, atomic renames and `fsync`s as before the seam existed.
//!   Memory: [`MemoryDisk`], for tests and "don't remember this browser".
//! - [`Paths`]: where the data, config and state roots are (seam E). Native:
//!   the platform's directories (`dirs`); elsewhere, whatever the host says.
//! - [`KeyRest`]: where the keystore's bytes rest (section 6). Native:
//!   `keys.json`, mode `0600`, on the disk.
//!
//! - [`http::Http`]: outbound calls to the accounts service (seam D). Native:
//!   `reqwest`. wasm32: `fetch`.
//!
//! - [`Runtime`]: tasks, timers and clocks (seam C). Native: tokio, on the
//!   caller's runtime. wasm32: `spawn_local`, `setTimeout`, `performance.now()`.

pub mod disk;
pub mod http;
pub mod key_rest;
pub mod paths;
pub mod runtime;

pub use disk::{Disk, DiskBackend, DiskKind, Entry, MemoryDisk, Metadata, SealedDisk, TmpName};
#[cfg(target_arch = "wasm32")]
pub use disk::OpfsDisk;
pub use key_rest::{FileKeyRest, KeyRest, MemoryKeyRest};
pub use paths::Paths;
pub use runtime::{Instant, Interval, Runtime, RuntimeBackend, Sleep, Task, TaskSet};
