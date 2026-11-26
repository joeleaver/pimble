//! Where these tests find a `rhypedb-server` binary, and whether they may skip
//! without one.
//!
//! Looked for in order: `$RHYPEDB_SERVER_BIN`,
//! `~/dev/rhypedb/target/{release,debug}/rhypedb-server`, then `rhypedb-server`
//! or `rhypedb` on `$PATH`. With none, a test skips with a `SKIP:` line on
//! stderr, which is right on a machine without rhypedb checked out.
//!
//! CI builds the server and sets `RHYPEDB_SERVER_REQUIRED=1` beside
//! `RHYPEDB_SERVER_BIN` (`.github/workflows/ci.yml`). Then a missing binary is
//! a failure rather than a skip, and so is a `$RHYPEDB_SERVER_BIN` that names
//! no file, so a broken CI step can never pass by skipping every test that
//! needs the server. The same rule is in
//! `crates/pimble-cloud/tests/integration/rhypedb_bin.rs`.

use std::path::PathBuf;

/// The variable that makes a missing server a failure.
pub const REQUIRED_VAR: &str = "RHYPEDB_SERVER_REQUIRED";

/// Whether a missing server fails the test (`RHYPEDB_SERVER_REQUIRED` is `1`
/// or `true`).
pub fn required() -> bool {
    matches!(std::env::var(REQUIRED_VAR).as_deref(), Ok("1") | Ok("true"))
}

/// The binary, or `None` to skip. Panics instead of answering `None` when
/// [`required`].
pub fn find() -> Option<PathBuf> {
    let found = look();
    if required() {
        if let Some(named) = std::env::var_os("RHYPEDB_SERVER_BIN").map(PathBuf::from) {
            assert!(named.is_file(), "{REQUIRED_VAR} is set and RHYPEDB_SERVER_BIN names {}, which is not a file", named.display());
        }
        assert!(
            found.is_some(),
            "{REQUIRED_VAR} is set and no rhypedb-server binary was found (checked $RHYPEDB_SERVER_BIN, \
             ~/dev/rhypedb/target/{{release,debug}}/rhypedb-server, and $PATH)"
        );
    }
    found
}

fn look() -> Option<PathBuf> {
    if let Some(named) = std::env::var_os("RHYPEDB_SERVER_BIN").map(PathBuf::from) {
        if named.is_file() {
            return Some(named);
        }
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        for rel in ["dev/rhypedb/target/release/rhypedb-server", "dev/rhypedb/target/debug/rhypedb-server"] {
            let p = home.join(rel);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).flat_map(|dir| ["rhypedb-server", "rhypedb"].map(|name| dir.join(name))).find(|p| p.is_file())
}
