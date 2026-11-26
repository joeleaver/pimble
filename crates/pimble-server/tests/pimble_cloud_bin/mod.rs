//! Where these tests find a `pimble-cloud` binary (the accounts service), and
//! whether they may skip without one.
//!
//! Looked for in order: `$PIMBLE_CLOUD_BIN`, then beside the test binary
//! itself (the same target dir and profile it was built into, so a
//! `CARGO_TARGET_DIR` of one's own is found), then `$CARGO_TARGET_DIR` and the
//! workspace's `target/`, release before debug. With none, a test skips with
//! a `SKIPPING` line on stderr, which is right on a machine that has not
//! built the accounts service.
//!
//! CI sets `PIMBLE_CLOUD_REQUIRED=1` (`.github/workflows/ci.yml`), the
//! sibling of `RHYPEDB_SERVER_REQUIRED` (`rhypedb_bin`). Then a missing
//! binary is a failure rather than a skip, and so is a `$PIMBLE_CLOUD_BIN`
//! that names no file, so the tests against the real accounts service can
//! never pass in CI by skipping.

use std::path::{Path, PathBuf};

/// The variable that makes a missing binary a failure.
pub const REQUIRED_VAR: &str = "PIMBLE_CLOUD_REQUIRED";
/// The variable that names the binary.
pub const BIN_VAR: &str = "PIMBLE_CLOUD_BIN";

/// Whether a missing binary fails the test (`PIMBLE_CLOUD_REQUIRED` is `1`
/// or `true`).
pub fn required() -> bool {
    matches!(std::env::var(REQUIRED_VAR).as_deref(), Ok("1") | Ok("true"))
}

/// The binary, or `None` to skip. Panics instead of answering `None` when
/// [`required`].
pub fn find() -> Option<PathBuf> {
    let found = look();
    if required() {
        if let Some(named) = std::env::var_os(BIN_VAR).map(PathBuf::from) {
            assert!(named.is_file(), "{REQUIRED_VAR} is set and {BIN_VAR} names {}, which is not a file", named.display());
        }
        assert!(
            found.is_some(),
            "{REQUIRED_VAR} is set and no pimble-cloud binary was found (checked ${BIN_VAR}, beside this test binary, \
             $CARGO_TARGET_DIR and the workspace's target/{{release,debug}}; build it with `cargo build -p pimble-cloud --release`)"
        );
    }
    found
}

fn look() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = std::env::var_os(BIN_VAR).map(PathBuf::from).into_iter().collect();
    // `<target>/<profile>/deps/<test>`: the binary is in `<target>/<profile>`.
    if let Some(profile_dir) = std::env::current_exe().ok().as_deref().and_then(Path::parent).and_then(Path::parent) {
        candidates.push(profile_dir.join("pimble-cloud"));
    }
    let workspace_target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
    for target in std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).into_iter().chain([workspace_target]) {
        candidates.extend(["release", "debug"].map(|profile| target.join(profile).join("pimble-cloud")));
    }
    candidates.into_iter().find(|p| p.is_file())
}
