//! Write a Pimble plugin (docs/PLUGINS_CONTRACT.md): plain rinch in a
//! `wasm32-wasip1` guest.
//!
//! A plugin author writes a `describe` function answering the plugin's
//! [`Manifest`] and a view: a rinch component taking a [`Mount`]. The
//! [`plugin!`] macro defines the module's exports around them.
//!
//! ```ignore
//! use pimble_plugin_sdk::prelude::*;
//!
//! fn describe() -> Manifest {
//!     manifest("hello-viewer", "Hello viewer", env!("CARGO_PKG_VERSION"))
//!         .with(viewer("hello", "Hello", &[SourceShape::Any]))
//!         .writes(StoreAsk::Chosen)
//! }
//!
//! fn view(__scope: &mut RenderScope, mount: Mount) -> NodeHandle {
//!     let title = Signal::new(String::new());
//!     let (api, metanode) = (mount.api.clone(), mount.metanode());
//!     spawn_local(async move {
//!         if let Some(addr) = metanode {
//!             if let Ok(forest) = api.metanode_source(addr).await {
//!                 title.set(pimble_api::label(&forest.node));
//!             }
//!         }
//!     });
//!     rsx! { h2 { {move || title.get()} } }
//! }
//!
//! pimble_plugin_sdk::plugin!(describe: describe, view: view);
//! ```
//!
//! Build it with `cargo build --release --target wasm32-wasip1` as a `cdylib`, and
//! put the `.wasm` in the plugins folder of Pimble's data directory.
//!
//! A plugin reads and writes through `mount.api`, a `pimble_api::Pimble` over
//! [`GuestBackend`]: the same API the app uses, allowed what the person approved
//! for the plugin (the manifest's `asks`) and judged by the server with the
//! person's role.
//!
//! The view renders into a [`RemoteDocument`]: a shadow of the plugin's tree that
//! answers rinch's structural reads, and a log of DOM writes the host applies to the
//! window under the plugin's area at the end of every turn. Events come back to the
//! plugin's own handlers, so `onclick`, `oninput` and `onkeydown` work as anywhere
//! else in rinch.

pub mod api;
pub mod document;
pub mod runtime;
mod selector;
#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub mod wasm;

pub use document::{HostLink, RemoteDocument};
pub use pimble_plugin_abi as abi;
pub use api::{ApiLink, Bridge, GuestBackend};
pub use pimble_api;
pub use pimble_core;
pub use pimble_plugin_abi::{HostState, Manifest, NodeAddr, Permissions, PresentPerson, Props, Provision, RecipeDecl, SourceShape, StoreAsk, ViewerDecl, ABI_VERSION};
/// Pimble's own components (presence, icons, colors), drawn alike by the app
/// and by plugins.
pub use pimble_ui;
pub use runtime::{log_line, person, Guest, Host, Mount, Seen, ViewFn};

/// How soon a second click on the same thing makes a double-click.
pub const DOUBLE_CLICK: std::time::Duration = std::time::Duration::from_millis(500);

thread_local! {
    static LAST_CLICK: std::cell::Cell<Option<(std::time::Instant, pimble_api::Addr)>> = const { std::cell::Cell::new(None) };
}

/// Whether this click on `addr` (a card, a row, a header) is the second of a
/// double-click: what renames each of them in a viewer, as a double-click
/// does a row of the explorer. rinch hands a click no count, so a guest times
/// its clicks itself (WASI's clock answers on both hosts).
pub fn clicked_again(addr: pimble_api::Addr) -> bool {
    LAST_CLICK.with(|last| {
        let now = std::time::Instant::now();
        let again = last.get().is_some_and(|(at, on)| on == addr && now.duration_since(at) < DOUBLE_CLICK);
        last.set(if again { None } else { Some((now, addr)) });
        again
    })
}

/// The rinch git revision this SDK was built against (from the plugin's
/// `Cargo.lock`, `unknown` when there was none to read). The manifest carries it.
pub const RINCH_REVISION: &str = env!("PIMBLE_RINCH_REVISION");

/// rinch, at the revision this SDK is built against. A plugin names no rinch of its
/// own: `rsx!` expands to `rinch::...` paths, which the prelude's `rinch` answers.
pub use rinch;

/// What a plugin module usually needs: rinch's prelude, and the SDK's names.
/// Glob-import it in every module that uses `rsx!`, which reaches rinch through the
/// `rinch` it brings into scope.
pub mod prelude {
    pub use crate::{pimble_api, rinch};
    pub use crate::{clicked_again, display_label, manifest, recipe, viewer, Host, ManifestExt, Mount, Seen};
    pub use crate::pimble_ui;
    pub use pimble_api::{plan, Addr, ApiError, Forest, ForestEntry, Judged, MetanodeEdit, Pimble};
    pub use pimble_core::Place;
    pub use pimble_plugin_abi::{Manifest, NodeAddr, Permissions, PresentPerson, Props, SourceShape, StoreAsk};
    pub use rinch::prelude::*;
}

/// A manifest for plugin `id` with nothing provided yet: the ABI version and the
/// rinch revision are filled in from what the plugin was built with.
pub fn manifest(id: &str, name: &str, version: &str) -> Manifest {
    Manifest {
        id: id.to_string(),
        name: name.to_string(),
        version: version.to_string(),
        abi: ABI_VERSION,
        rinch: RINCH_REVISION.to_string(),
        provides: Vec::new(),
        asks: Permissions::default(),
    }
}

/// A metanode viewer with id `id` (what `metanode:viewer` names), read as `name`,
/// showing sources of the shapes `takes`.
pub fn viewer(id: &str, name: &str, takes: &[SourceShape]) -> Provision {
    Provision::Viewer(ViewerDecl { id: id.to_string(), name: name.to_string(), takes: takes.to_vec() })
}

/// A recipe "New <thing>" (`label`) making a metanode titled `title`, shown
/// with `viewer`, whose source is its own subtree `depth` levels deep, with
/// `children` titled in order. The app makes it through its own API when
/// the person picks it from the explorer's new-node menu.
pub fn recipe(id: &str, label: &str, title: &str, icon: Option<&str>, viewer: &str, depth: u32, children: &[&str]) -> Provision {
    Provision::Recipe(RecipeDecl {
        id: id.to_string(),
        label: label.to_string(),
        title: title.to_string(),
        icon: icon.map(str::to_string),
        viewer: viewer.to_string(),
        depth,
        children: children.iter().map(|c| c.to_string()).collect(),
    })
}

/// What a node is called wherever it is named, as the explorer's row names it
/// (`pimble_core::plan::display_label`): an explicit title, else the first
/// line of its text, else its title.
pub fn display_label(node: &pimble_core::Node) -> String {
    pimble_core::plan::display_label(node, pimble_crdt::NodeDoc::text_of)
}

/// Building a manifest.
pub trait ManifestExt {
    /// The manifest, also providing `provision`.
    fn with(self, provision: Provision) -> Self;
    /// The manifest, asking to read the stores `stores` names.
    fn reads(self, stores: StoreAsk) -> Self;
    /// The manifest, asking to read and change the stores `stores` names.
    fn writes(self, stores: StoreAsk) -> Self;
}

impl ManifestExt for Manifest {
    fn with(mut self, provision: Provision) -> Self {
        self.provides.push(provision);
        self
    }

    fn reads(mut self, stores: StoreAsk) -> Self {
        self.asks.stores = stores;
        self
    }

    fn writes(mut self, stores: StoreAsk) -> Self {
        self.asks.stores = stores;
        self.asks.write = true;
        self
    }
}

/// Define a plugin module's exports around its `describe` function and its view.
///
/// `describe: fn() -> Manifest`; `view: fn(&mut RenderScope, Mount) -> NodeHandle`
/// (a function written like a rinch component, whose first parameter is named
/// `__scope` so `rsx!` can use it). The exports exist only in the wasm build, so the
/// same crate also builds natively for its own tests.
#[macro_export]
macro_rules! plugin {
    (describe: $describe:path, view: $view:path $(,)?) => {
        #[cfg(target_arch = "wasm32")]
        const _: () = {
            unsafe extern "C" {
                fn __wasm_call_ctors();
            }
            // A reactor's entry, as wasi-libc's `crt1-reactor.o` defines it: run the
            // static constructors once, when the host first instantiates the module.
            // Without a reference to `__wasm_call_ctors` the linker treats the module
            // as a command and wraps every export in constructors before and
            // destructors after, which would run libc's exit handlers on each call.
            #[unsafe(no_mangle)]
            pub extern "C" fn _initialize() {
                unsafe { __wasm_call_ctors() }
            }
            #[unsafe(no_mangle)]
            pub extern "C" fn pimble_alloc(len: u32) -> u32 {
                $crate::wasm::alloc(len)
            }
            #[unsafe(no_mangle)]
            pub extern "C" fn pimble_free(ptr: u32, len: u32) {
                $crate::wasm::free(ptr, len)
            }
            #[unsafe(no_mangle)]
            pub extern "C" fn pimble_describe() -> u64 {
                $crate::wasm::describe($describe)
            }
            #[unsafe(no_mangle)]
            pub extern "C" fn pimble_mount(ptr: u32, len: u32) {
                $crate::wasm::mount($describe, $view, ptr, len)
            }
            #[unsafe(no_mangle)]
            pub extern "C" fn pimble_resolve(ptr: u32, len: u32) {
                $crate::wasm::resolve(ptr, len)
            }
            #[unsafe(no_mangle)]
            pub extern "C" fn pimble_dispatch(ptr: u32, len: u32) -> u32 {
                $crate::wasm::dispatch(ptr, len)
            }
            #[unsafe(no_mangle)]
            pub extern "C" fn pimble_props_changed(ptr: u32, len: u32) {
                $crate::wasm::props_changed(ptr, len)
            }
            #[unsafe(no_mangle)]
            pub extern "C" fn pimble_host_state(ptr: u32, len: u32) {
                $crate::wasm::host_state(ptr, len)
            }
            #[unsafe(no_mangle)]
            pub extern "C" fn pimble_drag_ended() {
                $crate::wasm::drag_ended()
            }
            #[unsafe(no_mangle)]
            pub extern "C" fn pimble_unmount() {
                $crate::wasm::unmount()
            }
            #[unsafe(no_mangle)]
            pub extern "C" fn pimble_fire_timeout(id: u64) {
                $crate::wasm::fire_timeout(id)
            }
            #[unsafe(no_mangle)]
            pub extern "C" fn pimble_poll_task(id: u64) {
                $crate::wasm::poll_task(id)
            }
            #[unsafe(no_mangle)]
            pub extern "C" fn pimble_drain_polls() -> i64 {
                $crate::wasm::drain_polls()
            }
        };
    };
}
