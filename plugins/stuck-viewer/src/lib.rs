//! The `stuck` viewer: a test plugin with a button whose handler never returns. The
//! host gives every turn a budget and stops a plugin that overruns it with a notice
//! (docs/PLUGINS_CONTRACT.md section 3.2), which is what this is for.

use pimble_plugin_sdk::prelude::*;

fn describe() -> Manifest {
    manifest("stuck-viewer", "Stuck viewer", env!("CARGO_PKG_VERSION")).with(viewer("stuck", "Stuck", &[SourceShape::Any]))
}

fn view(__scope: &mut RenderScope, _mount: Mount) -> NodeHandle {
    rsx! {
        div {
            style: "padding: 16px 20px; display: flex; flex-direction: column; gap: 12px;",
            p { style: "margin: 0;", "This plugin's button runs for ever." }
            div {
                Button {
                    onclick: move || {
                        let mut n: u64 = 0;
                        loop {
                            n = std::hint::black_box(n.wrapping_add(1));
                        }
                    },
                    "Hang"
                }
            }
        }
    }
}

pimble_plugin_sdk::plugin!(describe: describe, view: view);
