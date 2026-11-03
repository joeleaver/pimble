//! The `hello` viewer: a toy plugin that shows a metanode's forest with plain rinch
//! components, and takes clicks and keys, to show the plugin system working end to
//! end (docs/PLUGINS_CONTRACT.md wave 1). It writes nothing: the API a plugin
//! writes through arrives in wave 2.

use pimble_plugin_sdk::prelude::*;

fn describe() -> Manifest {
    manifest("hello-viewer", "Hello viewer", env!("CARGO_PKG_VERSION")).with(viewer("hello", "Hello", &[SourceShape::Any]))
}

/// The view: a heading with the metanode's name, the forest as an indented list, a
/// button counting its clicks, and a field echoing what is typed and the last key.
pub fn view(__scope: &mut RenderScope, mount: Mount) -> NodeHandle {
    let props = mount.props;
    let viewer_id = mount.viewer;
    let clicks = Signal::new(0u32);
    let typed = Signal::new(String::new());
    let last_key = Signal::new(String::new());
    rsx! {
        div {
            class: "hello-viewer",
            style: "display: flex; flex-direction: column; gap: 12px; padding: 16px 20px;",
            h2 {
                style: "margin: 0;",
                {move || format!("Hello from a plugin: {}", props.get().title)}
            }
            p {
                style: "margin: 0; opacity: 0.7;",
                {format!("The {viewer_id} viewer runs in a WebAssembly sandbox. ")}
                {move || format!("It shows {} nodes.", props.get().forest.len())}
            }
            ul {
                class: "hello-viewer__forest",
                style: "margin: 0; padding: 0; list-style: none;",
                for row in props.get().forest {
                    li {
                        key: format!("{}/{}", row.addr.store, row.addr.node),
                        style: format!("padding: 2px 0 2px {}px;", 16 * row.depth.saturating_sub(1)),
                        {row.label.clone()}
                    }
                }
            }
            div {
                Button {
                    onclick: move || clicks.update(|n| *n += 1),
                    {move || match clicks.get() {
                        0 => "Click me".to_string(),
                        1 => "Clicked once".to_string(),
                        n => format!("Clicked {n} times"),
                    }}
                }
            }
            div {
                onkeydown: move |key: &KeyEventData| {
                    last_key.set(key.key.clone());
                    false
                },
                TextInput {
                    label: "Type here",
                    placeholder: "Anything",
                    value_fn: move || typed.get(),
                    oninput: move |value: String| typed.set(value),
                }
            }
            p {
                class: "hello-viewer__echo",
                style: "margin: 0;",
                {move || format!("You typed: {}", typed.get())}
            }
            p {
                class: "hello-viewer__key",
                style: "margin: 0;",
                {move || format!("Last key: {}", last_key.get())}
            }
        }
    }
}

pimble_plugin_sdk::plugin!(describe: describe, view: view);
