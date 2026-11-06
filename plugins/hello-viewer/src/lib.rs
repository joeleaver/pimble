//! The `hello` viewer: a toy plugin that shows a metanode's source with plain rinch
//! components and acts on it through the Pimble API, to show the plugin system
//! working end to end (docs/PLUGINS_CONTRACT.md waves 1 and 2).
//!
//! It reads the metanode's forest with `Pimble::metanode_source` and keeps it live
//! from `Pimble::next_change`, renames the chosen node with the one write
//! (`Pimble::edit`), and moves it with `Pimble::move_node`, which plans the move
//! first: a move the explorer would refuse (into itself) is refused here with the
//! explorer's sentence, before anything is sent.

use std::rc::Rc;
use std::sync::Arc;

use pimble_plugin_sdk::pimble_core::custom_keys;
use pimble_plugin_sdk::prelude::*;

fn describe() -> Manifest {
    manifest("hello-viewer", "Hello viewer", env!("CARGO_PKG_VERSION"))
        .with(viewer("hello", "Hello", &[SourceShape::Any]))
        .writes(StoreAsk::Chosen)
}

/// One node of the forest, as drawn.
#[derive(Clone, PartialEq)]
struct Row {
    addr: Addr,
    label: String,
    depth: u32,
}

/// Read the metanode's forest into `title` and `rows`; answers it, to test later
/// changes against.
async fn read(api: &Pimble, metanode: Addr, title: Signal<String>, rows: Signal<Vec<Row>>, status: Signal<String>) -> Option<Forest> {
    match api.metanode_source(metanode).await {
        Ok(forest) => {
            title.set(pimble_api::label(&forest.node));
            rows.set(
                forest
                    .entries
                    .iter()
                    .map(|e| Row { addr: e.addr, label: pimble_api::label(&e.node), depth: e.depth })
                    .collect(),
            );
            Some(forest)
        }
        Err(e) => {
            status.set(e.sentence());
            None
        }
    }
}

/// Rename `addr` to `title` with the one write, as the explorer's rename does.
async fn rename(api: &Pimble, addr: Addr, title: String) -> Result<(), ApiError> {
    api.edit(addr, move |doc, now| -> Result<(), ApiError> {
        doc.set_title(&title, now)?;
        doc.set_custom(custom_keys::EXPLICIT_TITLE, &serde_json::Value::Bool(true), now)?;
        Ok(())
    })
    .await
    .map(|_| ())
}

/// The view: the metanode's name, its forest (a click chooses a row), a field for
/// a new name, and three buttons acting on the chosen row through the API, with a
/// line saying what came of the last one.
pub fn view(__scope: &mut RenderScope, mount: Mount) -> NodeHandle {
    let api: Arc<Pimble> = mount.api.clone();
    let metanode = mount.metanode();
    let viewer_id = mount.viewer.clone();
    let title = Signal::new(String::new());
    let rows: Signal<Vec<Row>> = Signal::new(Vec::new());
    let chosen: Signal<Option<Addr>> = Signal::new(None);
    let status = Signal::new(String::new());
    let typed = Signal::new(String::new());
    let last_key = Signal::new(String::new());

    // Read the forest, then keep it live: every change the plugin's API hears is
    // merged into what it holds before it is answered, so reading again shows it.
    if let Some(metanode) = metanode {
        let api = api.clone();
        spawn_local(async move {
            let mut forest = read(&api, metanode, title, rows, status).await;
            while let Some(change) = api.next_change().await {
                if forest.as_ref().map_or(true, |f| f.touched_by(&change)) {
                    forest = read(&api, metanode, title, rows, status).await;
                }
            }
        });
    } else {
        status.set("This plugin was shown without a metanode.".to_string());
    }

    // What a button does with the chosen row, through the API; the outcome is
    // the line under the buttons.
    let act: Rc<dyn Fn(&'static str)> = Rc::new(move |what: &'static str| {
        let Some(addr) = chosen.get() else {
            status.set("Choose a row first.".to_string());
            return;
        };
        let Some(metanode) = metanode else { return };
        let api = api.clone();
        let new_title = typed.get();
        spawn_local(async move {
            let done = match what {
                "rename" if new_title.trim().is_empty() => Err(ApiError::refused("Type a new name first.")),
                "rename" => rename(&api, addr, new_title.trim().to_string()).await.map(|_| "Renamed."),
                "top" => api.move_node(addr, metanode, Place::First).await.map(|_| "Moved to the top."),
                _ => api.move_node(addr, addr, Place::Last).await.map(|_| "Moved into itself?"),
            };
            status.set(match done {
                Ok(line) => line.to_string(),
                Err(e) => e.sentence(),
            });
        });
    });
    let (rename_act, top_act, into_act) = (act.clone(), act.clone(), act);

    rsx! {
        div {
            class: "hello-viewer",
            style: "display: flex; flex-direction: column; gap: 12px; padding: 16px 20px;",
            h2 {
                style: "margin: 0;",
                {move || format!("Hello from a plugin: {}", title.get())}
            }
            p {
                style: "margin: 0; opacity: 0.7;",
                {format!("The {viewer_id} viewer runs in a WebAssembly sandbox and reads through the Pimble API. ")}
                {move || format!("It shows {} nodes.", rows.with(|r| r.len()))}
            }
            ul {
                class: "hello-viewer__forest",
                style: "margin: 0; padding: 0; list-style: none;",
                for row in rows.get() {
                    li {
                        key: format!("{}/{} {}", row.addr.store, row.addr.node, row.label),
                        class: "hello-viewer__row",
                        style: {
                            let addr = row.addr;
                            let depth = row.depth;
                            move || format!(
                                "padding: 2px 0 2px {}px; cursor: pointer;{}",
                                16 * depth.saturating_sub(1),
                                if chosen.get() == Some(addr) { " font-weight: 600;" } else { "" },
                            )
                        },
                        onclick: {
                            let addr = row.addr;
                            move || chosen.set(Some(addr))
                        },
                        {row.label.clone()}
                    }
                }
            }
            div {
                onkeydown: move |key: &KeyEventData| {
                    last_key.set(key.key.clone());
                    false
                },
                TextInput {
                    label: "New name",
                    placeholder: "A new name for the chosen row",
                    value_fn: move || typed.get(),
                    oninput: move |value: String| typed.set(value),
                }
            }
            div {
                style: "display: flex; gap: 8px;",
                Button {
                    onclick: move || rename_act("rename"),
                    "Rename"
                }
                Button {
                    onclick: move || top_act("top"),
                    "Move to top"
                }
                Button {
                    onclick: move || into_act("into"),
                    "Move into itself"
                }
            }
            p {
                class: "hello-viewer__status",
                style: "margin: 0;",
                {move || status.get()}
            }
            p {
                class: "hello-viewer__key",
                style: "margin: 0; opacity: 0.7;",
                {move || format!("Last key: {}", last_key.get())}
            }
        }
    }
}

/// Its tools, for the host's tests of tools run for agents
/// (docs/AGENT_GUIDES_CONTRACT.md section 8): one that answers, one that reads,
/// one that writes, and a field check.
fn tools(request: ToolRequest, api: Arc<Pimble>) -> ToolFuture {
    Box::pin(async move {
        let arg = |k: &str| request.args.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let addr = |k: &str| pimble_plugin_sdk::pimble_core::PimbleUrl::parse(&arg(k)).map(|u| Addr::new(u.store, u.node));
        match request.tool.as_str() {
            "echo" => ToolAnswer::ok(arg("text")),
            "count_children" => match addr("node") {
                None => ToolAnswer::no("node is not a pimble: link"),
                Some(a) => match api.children(a).await {
                    Ok(children) => ToolAnswer::ok(format!("{} children", children.len())),
                    Err(e) => ToolAnswer::no(e.sentence()),
                },
            },
            "rename" => match addr("node") {
                None => ToolAnswer::no("node is not a pimble: link"),
                Some(a) => match rename(&api, a, arg("title")).await {
                    Ok(()) => ToolAnswer::ok(format!("renamed to {}", arg("title"))),
                    Err(e) => ToolAnswer::no(e.sentence()),
                },
            },
            "check_field" if arg("value") == "bad" => ToolAnswer::no("a mood cannot be bad"),
            "check_field" => ToolAnswer::ok(format!("{} is a fine mood", arg("value"))),
            other => ToolAnswer::no(format!("hello has no tool {other}")),
        }
    })
}

pimble_plugin_sdk::plugin!(describe: describe, view: view, guide: include_bytes!("../guide.json"), tools: tools);
