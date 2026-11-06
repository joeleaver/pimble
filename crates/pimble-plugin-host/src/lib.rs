//! Runs Pimble plugins (docs/PLUGINS_CONTRACT.md): WASM guests that render plain
//! rinch into a document of their own and send the host their DOM writes.
//!
//! - [`applier`]: applies a guest's op log to the app's `dyn DomDocument` under the
//!   plugin's container, proxies its event handlers, answers the reads only the
//!   host can. The guest-to-host node map is the sandbox.
//! - [`bridge`]: the Pimble API across the boundary: what the person approved for
//!   a plugin, and making its calls through the app's own backend.
//! - [`instance`]: one running plugin over any [`GuestEngine`]: mount, props,
//!   events, timers, and stopping it (with a sentence) when it runs too long, is
//!   refused or traps.
//! - [`library`]: the plugin library's index, what it offers this device, and
//!   the check every downloaded plugin passes.
//! - [`registry`]: the plugins loaded and the ones refused, whatever engine
//!   compiled them.
//! - `wasmtime_engine` (feature `wasm-host`, the desktop): loading `.wasm` files from
//!   the plugins folder, checking what they say they are, and running them under
//!   wasmtime with epoch interruption.
//! - `js_engine` (feature `js-host`, the browser, wave 3): the same modules under
//!   the browser's own `WebAssembly`, with a small preview1 shim; a turn that ran
//!   past its budget stops the plugin when it returns, since nothing can
//!   interrupt it.
//!
//! Everything but the engine is backend-agnostic: both engines share the
//! applier, the bridge, the instance and the registry.

pub mod applier;
pub mod bridge;
pub mod instance;
#[cfg(feature = "js-host")]
pub mod js_engine;
pub mod library;
pub mod registry;
#[cfg(feature = "wasm-host")]
pub mod wasmtime_engine;

pub use applier::{Applier, ApplierOptions, Refusal};
pub use bridge::{Approval, Approved};
pub use instance::{host_calls, Answered, ApiSide, Call, Failure, GuestEngine, HostCalls, HostKey, Instance, InstanceOptions};
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
    // Decision 6: no plugin reaches the network in these waves, so one that
    // needs it would not work if it ran.
    if manifest.asks.network {
        return Err(format!("The {name} plugin asks to use the network, which Pimble does not allow plugins yet."));
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
            asks: Default::default(),
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
        // Decision 6: no network in these waves.
        let mut online = manifest(ABI_VERSION);
        online.asks.network = true;
        assert_eq!(check_manifest(&online).unwrap_err(), "The Hello viewer plugin asks to use the network, which Pimble does not allow plugins yet.");
    }
}
