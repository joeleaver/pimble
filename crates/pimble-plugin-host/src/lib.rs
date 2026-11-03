//! Runs Pimble plugins (docs/PLUGINS_CONTRACT.md): WASM guests that render plain
//! rinch into a document of their own and send the host their DOM writes.
//!
//! - [`applier`]: applies a guest's op log to the app's `dyn DomDocument` under the
//!   plugin's container, proxies its event handlers, answers the reads only the
//!   host can. The guest-to-host node map is the sandbox.
//! - [`instance`]: one running plugin over any [`GuestEngine`]: mount, props,
//!   events, timers, and stopping it (with a sentence) when it runs too long, is
//!   refused or traps.
//! - `wasmtime_engine` (feature `wasm-host`, the desktop): loading `.wasm` files from
//!   the plugins folder, checking what they say they are, and running them under
//!   wasmtime with epoch interruption.
//!
//! Everything but the engine is backend-agnostic, so the browser's engine (wave 3)
//! reuses it.

pub mod applier;
pub mod instance;
#[cfg(feature = "wasm-host")]
pub mod wasmtime_engine;

pub use applier::{Applier, ApplierOptions, Refusal};
pub use instance::{host_calls, Answered, Call, Failure, GuestEngine, HostCalls, HostKey, Instance, InstanceOptions};
pub use pimble_plugin_abi as abi;

use pimble_plugin_abi::{refusal_for_abi, Manifest, ABI_VERSION};

/// The rinch git revision the app is built against (from the workspace's
/// `Cargo.lock`), to compare with the one each plugin's manifest names.
pub const HOST_RINCH_REVISION: &str = env!("PIMBLE_RINCH_REVISION");

/// Whether the app runs a plugin that describes itself as `manifest`, or the
/// sentence a person reads when it does not.
///
/// The ABI must match exactly. The rinch revision is not compared: rinch's version
/// is 0.1.0 at every revision and git revisions have no order outside the
/// repository, so "an incompatible rinch" has no test the host can run yet, and an
/// exact match would refuse every plugin each time the app moves its rinch pin.
/// What can drift between two revisions is markup and styling (a guest's
/// components drawn with the host's theme), never the event and op shapes, which
/// are this ABI's. The loader logs a plugin built against another revision.
pub fn check_manifest(manifest: &Manifest) -> Result<(), String> {
    let name = if manifest.name.is_empty() { manifest.id.as_str() } else { manifest.name.as_str() };
    if manifest.abi != ABI_VERSION {
        return Err(refusal_for_abi(name, manifest.abi));
    }
    if manifest.id.is_empty() {
        return Err("A plugin described itself with no id.".to_string());
    }
    if manifest.viewers().any(|v| v.id.is_empty()) {
        return Err(format!("The {name} plugin provides a viewer with no id."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_plugin_abi::{Provision, SourceShape, ViewerDecl};

    fn manifest(abi: u32) -> Manifest {
        Manifest {
            id: "hello-viewer".into(),
            name: "Hello viewer".into(),
            version: "0.1.0".into(),
            abi,
            rinch: "somewhere-else".into(),
            provides: vec![Provision::Viewer(ViewerDecl { id: "hello".into(), name: "Hello".into(), takes: vec![SourceShape::Any] })],
        }
    }

    #[test]
    fn a_plugin_built_for_another_abi_is_refused_with_a_sentence() {
        assert_eq!(check_manifest(&manifest(ABI_VERSION)), Ok(()));
        let refused = check_manifest(&manifest(ABI_VERSION + 1)).unwrap_err();
        assert_eq!(
            refused,
            format!("The Hello viewer plugin was built for plugin ABI {}, and this Pimble runs ABI {ABI_VERSION} plugins.", ABI_VERSION + 1)
        );
        let mut nameless = manifest(ABI_VERSION);
        nameless.id.clear();
        assert!(check_manifest(&nameless).is_err());
    }
}
