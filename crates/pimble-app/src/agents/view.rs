//! Settings > AI Agents: the list, making or changing an agent, and a new
//! agent's key with its setup.

use rinch::prelude::*;

use super::picker::{chevron, from_above, role_buttons, role_style, role_tip, rows, row_label, set_role, toggle_row};
use super::{choice_of, grants_of, last_used, listed_rows, remove, set_up, snippet, summary, Choice, ClientApp, Editing};
use crate::protocol::BackendCommand;
use crate::state::AppStore;

pub(crate) fn render_page(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let ui = store.agents_ui;
    let listing = move || ui.editing.get().is_none() && ui.created.get().is_none();
    let save = move || {
        let Some(e) = untracked(|| ui.editing.get()) else { return };
        if e.name.trim().is_empty() {
            ui.error.set("Name the agent: the app it is for, or what it does.".into());
            return;
        }
        if e.grants.is_empty() {
            ui.error.set("Choose what it may read or edit: nothing is chosen yet.".into());
            return;
        }
        ui.error.set(String::new());
        ui.busy.set(true);
        let grants = grants_of(&e.grants);
        store.send(match e.id {
            Some(id) => BackendCommand::UpdateAgent { id, name: e.name.trim().to_string(), grants },
            None => BackendCommand::CreateAgent { name: e.name.trim().to_string(), grants },
        });
    };

    rsx! {
            div {
                class: "pimble-agents",

                // ── The list ──
                div {
                    style: {move || if listing() { "display: flex; flex-direction: column; gap: 10px;" } else { "display: none;" }},
                    div {
                        class: "pimble-agents__intro",
                        "Apps like Claude connect to Pimble as agents. Each one reads or edits only the parts of your notes you choose, on this computer."
                    }
                    for (agent, placed) in listed_rows(store) {
                        div {
                            key: format!("{}:{placed}", agent.id),
                            class: "pimble-agents__row",
                            div {
                                style: "flex: 1; min-width: 0;",
                                div { class: "pimble-agents__name", {agent.name.clone()} }
                                div { class: "pimble-agents__reach", {summary(store, &agent.grants)} }
                                div {
                                    class: "pimble-agents__reach",
                                    {last_used(&agent)}
                                }
                                div {
                                    class: "pimble-agents__reach",
                                    {placed}
                                }
                            }
                            Button {
                                size: "xs",
                                variant: "light",
                                onclick: {
                                    let agent = agent.clone();
                                    move || {
                                        ui.error.set(String::new());
                                        ui.editing.set(Some(Editing {
                                            id: Some(agent.id.clone()),
                                            name: agent.name.clone(),
                                            app: ClientApp::Other,
                                            grants: choice_of(&agent.grants, store),
                                        }));
                                    }
                                },
                                "Edit"
                            }
                            Button {
                                size: "xs",
                                variant: "subtle",
                                color: "red",
                                onclick: {
                                    let id = agent.id.clone();
                                    move || {
                                        remove(store, &id);
                                    }
                                },
                                "Remove"
                            }
                        }
                    }
                    div {
                        style: {move || if ui.agents.get().is_empty() && !ui.busy.get() { "font-size: 12px; color: var(--rinch-color-dimmed);" } else { "display: none;" }},
                        "No agents yet."
                    }
                    div {
                        style: {move || if ui.notice.get().is_empty() { "display: none;" } else { "font-size: 13px;" }},
                        {move || ui.notice.get()}
                    }
                    Button {
                        size: "sm",
                        variant: "filled",
                        onclick: move || {
                            ui.error.set(String::new());
                            ui.editing.set(Some(Editing { id: None, name: ClientApp::Claude.name().to_string(), app: ClientApp::Claude, grants: Choice::new() }));
                        },
                        "New Agent"
                    }
                }

                // ── Making or changing one ──
                div {
                    style: {move || if ui.editing.get().is_some() { "display: flex; flex-direction: column; gap: 10px;" } else { "display: none;" }},
                    TextInput {
                        label: "Name",
                        placeholder: "Claude",
                        value_fn: move || ui.editing.get().map(|e| e.name).unwrap_or_default(),
                        oninput: move |val: String| ui.editing.update(|e| {
                            if let Some(e) = e {
                                e.name = val;
                            }
                        }),
                    }
                    div {
                        style: {move || if ui.editing.get().is_some_and(|e| e.id.is_none()) { "" } else { "display: none;" }},
                        Select {
                            label: "For",
                            value_fn: move || ui.editing.get().map(|e| e.app.value().to_string()).unwrap_or_default(),
                            onchange: move |val: String| ui.editing.update(|e| {
                                if let Some(e) = e {
                                    e.app = ClientApp::from_value(&val);
                                    if e.name.is_empty() || ClientApp::ALL.iter().any(|a| a.name() == e.name) {
                                        e.name = e.app.name().to_string();
                                    }
                                }
                            }),
                            data: {|| ClientApp::ALL.iter().map(|a| SelectOption::new(a.value(), a.name())).collect::<Vec<_>>()},
                        }
                    }
                    div { class: "pimble-agents__label", "What it can reach" }
                    div {
                        class: "pimble-agents__hint",
                        "A choice covers everything inside; a row inside can only add to it. Nothing is chosen to start with."
                    }
                    div {
                        class: "pimble-agents__picker",
                        for row in rows(store, &ui.expanded.get()) {
                            div {
                                key: format!("{}:{:?}", row.store, row.node),
                                class: "pimble-agents__pick",
                                style: format!("padding-left: {}px;", 4 + row.depth * 16),
                                div {
                                    class: "pimble-agents__chevron",
                                    onclick: move || toggle_row(store, row.store, row.node),
                                    {chevron(row)}
                                }
                                div {
                                    style: "flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                    {row_label(store, row)}
                                }
                                div {
                                    class: "pimble-agents__from",
                                    {move || from_above(store, row.store, row.node)}
                                }
                                for (label, role, rs, rn) in role_buttons(row.store, row.node) {
                                    div {
                                        key: label,
                                        title: role_tip(role),
                                        style: move || role_style(store, rs, rn, role),
                                        onclick: move || set_role(store, rs, rn, role),
                                        {label}
                                    }
                                }
                            }
                        }
                    }
                    div {
                        style: "display: flex; gap: 8px; justify-content: flex-end;",
                        Button {
                            size: "sm",
                            variant: "subtle",
                            onclick: move || {
                                ui.error.set(String::new());
                                ui.editing.set(None);
                            },
                            "Cancel"
                        }
                        Button {
                            size: "sm",
                            variant: "filled",
                            loading: {|| ui.busy.get()},
                            onclick: save,
                            {move || if ui.editing.get().is_some_and(|e| e.id.is_some()) { "Save" } else { "Make Agent" }}
                        }
                    }
                }

                // ── A new agent, set up ──
                div {
                    style: {move || if ui.created.get().is_some() { "display: flex; flex-direction: column; gap: 10px;" } else { "display: none;" }},
                    div {
                        class: "pimble-agents__name",
                        {move || ui.created.get().map(|(a, _, _)| format!("{} is ready.", a.name)).unwrap_or_default()}
                    }
                    div {
                        class: "pimble-agents__hint",
                        "Its key is shown this once. Pimble keeps only a fingerprint of it; if it is lost, remove the agent and make another."
                    }
                    div {
                        style: {move || match ui.created.get() {
                            Some((_, _, app)) if app != ClientApp::Other && cfg!(feature = "native") => "",
                            _ => "display: none;",
                        }},
                        Button {
                            size: "sm",
                            variant: "filled",
                            onclick: move || set_up(store),
                            {move || ui.created.get().map(|(_, _, app)| format!("Add to {}", app.name())).unwrap_or_default()}
                        }
                        div {
                            class: "pimble-agents__hint",
                            style: "margin-top: 6px;",
                            {move || ui.created.get().map(|(_, _, app)| app.add_hint()).unwrap_or_default()}
                        }
                    }
                    div {
                        style: {move || if ui.setup.get().is_empty() { "display: none;" } else { "font-size: 13px;" }},
                        {move || ui.setup.get()}
                    }
                    div { class: "pimble-agents__label", "Or add it to the app's MCP settings yourself:" }
                    pre {
                        class: "pimble-agents__snippet",
                        {move || ui.created.get().map(|(a, key, app)| snippet(app, &a.name, &key)).unwrap_or_default()}
                    }
                    div {
                        style: "display: flex; gap: 8px; justify-content: flex-end;",
                        Button {
                            size: "sm",
                            variant: "light",
                            onclick: move || {
                                if let Some((a, key, app)) = untracked(|| ui.created.get()) {
                                    let _ = crate::links::copy_text(&snippet(app, &a.name, &key));
                                    ui.setup.set("Copied.".into());
                                }
                            },
                            "Copy Setup"
                        }
                        Button {
                            size: "sm",
                            variant: "filled",
                            onclick: move || {
                                ui.created.set(None);
                                ui.setup.set(String::new());
                            },
                            "Done"
                        }
                    }
                }

                div {
                    style: {move || if ui.error.get().is_empty() { "display: none;" } else { "color: var(--rinch-color-red-6); font-size: 12px; margin-top: 8px;" }},
                    {move || ui.error.get()}
                }
            }
    }
}
