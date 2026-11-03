//! Record the rinch revision this plugin is built against, for its manifest
//! (docs/PLUGINS_CONTRACT.md section 3.5).
//!
//! rinch's own version is 0.1.0 at every revision, so the git revision cargo
//! resolved is the only thing that names it. A build script cannot ask cargo which
//! revision a dependency resolved to, but the lock file the build is using records
//! it. That lock usually sits above the target directory this build writes into,
//! so walk up from `OUT_DIR` to the first `Cargo.lock`; a build directory kept
//! elsewhere (cargo's `build-dir`) has none above it, and then the lock is the
//! plugins workspace's this crate belongs to. `PIMBLE_RINCH_REVISION` overrides
//! both, and a build that finds nothing says `unknown`.

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-env-changed=PIMBLE_RINCH_REVISION");
    let revision = std::env::var("PIMBLE_RINCH_REVISION").ok().or_else(|| {
        let out = std::env::var("OUT_DIR").ok()?;
        let manifest = std::env::var("CARGO_MANIFEST_DIR").ok()?;
        let lock = Path::new(&out)
            .ancestors()
            .map(|dir| dir.join("Cargo.lock"))
            .chain(std::iter::once(Path::new(&manifest).join("../../plugins/Cargo.lock")))
            // The first lock that pins rinch: a build directory can sit under
            // some other project's lock that does not.
            .filter(|p| p.is_file())
            .find_map(|p| Some((rinch_revision(&std::fs::read_to_string(&p).ok()?)?, p)))?;
        println!("cargo:rerun-if-changed={}", lock.1.display());
        Some(lock.0)
    });
    println!("cargo:rustc-env=PIMBLE_RINCH_REVISION={}", revision.unwrap_or_else(|| "unknown".into()));
}

/// The commit of the `rinch` package a lock file pins: the part after `#` in its
/// `source = "git+https://...rinch.git?branch=...#<sha>"` line.
fn rinch_revision(lock: &str) -> Option<String> {
    let mut in_rinch = false;
    for line in lock.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            in_rinch = false;
        } else if line == "name = \"rinch\"" {
            in_rinch = true;
        } else if in_rinch {
            if let Some(source) = line.strip_prefix("source = \"") {
                return source.trim_end_matches('"').rsplit_once('#').map(|(_, sha)| sha.to_string());
            }
        }
    }
    None
}
