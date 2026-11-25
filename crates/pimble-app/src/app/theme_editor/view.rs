//! The theme editor's two columns (docs/THEMES_CONTRACT.md section 9): on
//! the left the base, "Show only what this theme changes" and the tokens in
//! their groups (`model`), each with a control fitting its type and a reset
//! when the theme sets it; on the right the sample text drawn in the theme
//! as it stands, with the scheme switch.

use rinch::prelude::*;
use serde_json::{Map, Value as Json};

use pimble_themes::tokens::token_of;
use pimble_themes::{BASE, COPIED_FROM, DEFAULT};

use super::model::{self, Control, Group, Line};
use super::{held, stored, write};
use crate::panes::PaneId;
use crate::state::AppStore;
use crate::themes;

/// The theme editor of pane `pane`, shown while it holds a theme node.
pub(crate) fn render_theme_editor(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let state = store.pane(pane);
    let ui = state.theme_ui;
    // What the theme stores, tracked; empty until its store's themes are listed.
    let own = move || held(store, pane).and_then(|(s, n)| stored(store, s, n)).unwrap_or_default();
    let groups = rinch::core::reactive::Memo::new(move || {
        let Some((s, n)) = held(store, pane) else { return Vec::new() };
        model::groups(&n.to_string(), &themes::of_store(store, s), &own(), ui.only_changed.get(), &SCHEME_VARS.with(Clone::clone))
    });
    let bases = move || {
        let Some((s, n)) = held(store, pane) else { return Vec::new() };
        model::bases(&n.to_string(), &themes::of_store(store, s)).into_iter().map(|(id, name)| SelectOption::new(id, name)).collect::<Vec<_>>()
    };
    let base = move || own().get(BASE).and_then(Json::as_str).unwrap_or(DEFAULT).to_string();
    let set_base = move |id: String| {
        let Some((s, n)) = untracked(|| held(store, pane)) else { return };
        if pimble_themes::edit::would_cycle(&n.to_string(), &id, &pimble_themes::WithStore(&untracked(|| themes::of_store(store, s)))) {
            crate::events::show_notice(store, "A theme cannot be based on itself, or on a theme based on it.".to_string());
            return;
        }
        let value = if id == DEFAULT { None } else { Some(Json::from(id)) };
        write(store, pane, BASE, value);
    };
    let copied = move || held(store, pane).and_then(|(_, _)| copied_line(store, &own()));
    let writable = move || held(store, pane).is_some_and(|(s, n)| store.node_access_tracked(s, n).allows_write());
    let drawn = rinch::core::reactive::Memo::new(move || {
        let Some((s, n)) = held(store, pane) else { return Default::default() };
        themes::drawn_for(&themes::of_store(store, s), &n.to_string(), ui.dark.get())
    });
    let scheme_class = move |dark: bool| {
        if ui.dark.get() == dark { "pimble-theme-editor__scheme pimble-theme-editor__scheme--on" } else { "pimble-theme-editor__scheme" }
    };
    let sample = themes::preview::sample();

    rsx! {
        div {
            class: "pimble-theme-editor",
            style: {move || if state.show_theme.get() { "" } else { "display: none;" }},
            div {
                class: "pimble-theme-editor__tokens",
                div {
                    class: "pimble-theme-picker__note",
                    style: {move || if writable() { "display: none;" } else { "" }},
                    {pimble_core::StoreAccess::READ_ONLY_REFUSAL}
                }
                div {
                    class: "pimble-theme-editor__copied",
                    style: {move || if copied().is_some() { "" } else { "display: none;" }},
                    {move || copied().unwrap_or_default()}
                }
                div {
                    class: "pimble-theme-editor__head",
                    Select {
                        label: "Based on",
                        size: "xs",
                        value_fn: move || base(),
                        onchange: set_base,
                        data: {move || bases()},
                    }
                    Checkbox {
                        label: "Show only what this theme changes",
                        checked_fn: move || ui.only_changed.get(),
                        onchange: move || ui.only_changed.set(!untracked(|| ui.only_changed.get())),
                    }
                }
                div {
                    class: "pimble-theme-editor__empty",
                    style: {move || if ui.only_changed.get() && groups.get().is_empty() { "" } else { "display: none;" }},
                    "This theme changes nothing yet: everything comes from its base."
                }
                for group in groups.get() {
                    div {
                        key: group_key(&group),
                        {group_view(__scope, store, pane, group.clone())}
                    }
                }
            }
            div {
                class: "pimble-theme-editor__preview-column",
                div {
                    class: "pimble-theme-editor__schemes",
                    span { class: {move || scheme_class(false)}, onclick: move || ui.dark.set(false), "Light" }
                    span { class: {move || scheme_class(true)}, onclick: move || ui.dark.set(true), "Dark" }
                }
                div {
                    class: {move || format!("{} pimble-theme-picker__preview", themes::wrap_classes(&drawn.get(), ui.dark.get()))},
                    style: {move || format!("{}{}", scheme_vars(ui.dark.get()), drawn.get().style)},
                    {super::super::theme_preview::page_in(__scope, ui.dark, &sample)}
                }
            }
        }
    }
}

thread_local! {
    /// The app's color variables for the light and the dark scheme, read
    /// once from the app's theme sheet.
    static SCHEME_VARS: [String; 2] = [model::root_vars(&crate::app::theme_css(false)), model::root_vars(&crate::app::theme_css(true))];
}

/// The app's color variables in the scheme `dark` says, as inline style.
fn scheme_vars(dark: bool) -> String {
    SCHEME_VARS.with(|vars| vars[usize::from(dark)].clone())
}

/// What keys a group in its list: everything it shows, so a group whose
/// lines changed is drawn again and one that did not is kept.
fn group_key(group: &Group) -> String {
    format!("{group:?}")
}

/// "Copied from Reading in Family": a copy's original by its name now, when
/// its store is open and still holds it, else as it was copied.
fn copied_line(store: AppStore, own: &Map<String, Json>) -> Option<String> {
    let from = own.get(COPIED_FROM)?;
    let store_id = from.get("store").and_then(Json::as_str).and_then(|s| pimble_core::StoreId::parse(s).ok())?;
    let node = from.get("node").and_then(Json::as_str).unwrap_or_default().to_string();
    let store_name = store.get_store_signal(store_id).map(|s| s.with(|s| s.name.clone()));
    let name = store.store_themes.with(|all| all.get(&store_id)?.iter().find(|t| t.node.to_string() == node).map(|t| t.name.clone()));
    Some(match (name, store_name) {
        (Some(name), Some(store_name)) => format!("Copied from {name} in {store_name}."),
        (None, Some(store_name)) => format!("Copied from a theme of {store_name}."),
        _ => "Copied from a theme of a store not open here.".to_string(),
    })
}

fn group_view(__scope: &mut RenderScope, store: AppStore, pane: PaneId, group: Group) -> NodeHandle {
    let el = rsx! {
        div {
            class: "pimble-theme-editor__group",
            div { class: "pimble-appearance__label", {group.title.clone()} }
        }
    };
    for line in group.lines {
        let child = line_view(__scope, store, pane, line);
        el.append_child(&child);
    }
    el
}

/// One token: its label, its control, where an inherited value comes from,
/// a reset when the theme sets it, and a refused value's sentence.
fn line_view(__scope: &mut RenderScope, store: AppStore, pane: PaneId, line: Line) -> NodeHandle {
    let key = line.key;
    let from_text = match (&line.from, line.value.is_empty()) {
        (Some(from), false) => format!("{}, from {from}", line.value),
        (Some(from), true) => format!("not set, from {from}"),
        (None, _) => String::new(),
    };
    let class = if line.own { "pimble-theme-editor__line pimble-theme-editor__line--own" } else { "pimble-theme-editor__line" };
    let reset_style = if line.own { "" } else { "display: none;" };
    let from_style = if from_text.is_empty() { "display: none;" } else { "" };
    let refusal = line.refusal.clone().unwrap_or_default();
    let refusal_style = if refusal.is_empty() { "display: none;" } else { "" };
    let control = control_view(__scope, store, pane, &line);
    let cell = rsx! { div { class: "pimble-theme-editor__control" } };
    cell.append_child(&control);
    rsx! {
        div {
            class: class,
            div { class: "pimble-theme-editor__label", {line.label.clone()} }
            {cell}
            div { class: "pimble-theme-editor__from", style: from_style, {from_text} }
            span {
                class: "pimble-theme-editor__reset",
                style: reset_style,
                title: "Back to the inherited value",
                onclick: move || write(store, pane, key, None),
                "Reset"
            }
            div { class: "pimble-theme-editor__refusal", style: refusal_style, {refusal} }
        }
    }
}

/// Typed text for `key`, checked and stored, or the reason it is not.
fn typed(store: AppStore, pane: PaneId, key: &'static str, text: &str) {
    let Some(token) = token_of(key) else { return };
    if text.trim().is_empty() {
        return;
    }
    match pimble_themes::edit::typed(token, text) {
        Ok(json) => write(store, pane, key, Some(json)),
        Err(refusal) => crate::events::show_notice(store, refusal.reason),
    }
}

/// A pair's half typed: the pair stored, the other half kept or offered.
fn pair_typed(store: AppStore, pane: PaneId, key: &'static str, light: &str, dark: &str, typed_dark: bool, text: &str) {
    match model::pair_with(light, dark, typed_dark, text) {
        Some(json) => write(store, pane, key, Some(json)),
        None if text.trim().is_empty() => {}
        None => crate::events::show_notice(store, format!("{:?} is not a color: give it as #rrggbb.", text.trim())),
    }
}

fn control_view(__scope: &mut RenderScope, store: AppStore, pane: PaneId, line: &Line) -> NodeHandle {
    let key = line.key;
    match line.control.clone() {
        Control::Text => {
            let value = line.value.clone();
            rsx! {
                input {
                    class: "pimble-properties__input",
                    value: value,
                    onchange: move |text: String| typed(store, pane, key, &text),
                }
            }
        }
        Control::Choice { options, current } => {
            let data = Signal::new(options.iter().map(|(json, name)| SelectOption::new(json_text(json), name.clone())).collect::<Vec<_>>());
            let current = Signal::new(current.as_ref().map(json_text).unwrap_or_default());
            rsx! {
                Select {
                    size: "xs",
                    value_fn: move || current.get(),
                    onchange: move |value: String| write(store, pane, key, Some(Json::from(value))),
                    data: {move || data.get()},
                }
            }
        }
        Control::Toggle(on) => rsx! {
            Checkbox {
                label: "",
                checked_fn: move || on,
                onchange: move || write(store, pane, key, Some(Json::from(!on))),
            }
        },
        Control::Pair { light, dark } => pair_view(__scope, store, pane, key, light, dark),
        Control::ColorRef { slot, light, dark } => {
            let mut options: Vec<SelectOption> = vec![SelectOption::new("", "Own colors")];
            options.extend(model::slot_names().into_iter().map(|s| SelectOption::new(s, s)));
            let data = Signal::new(options);
            let current = Signal::new(slot.unwrap_or_default());
            // "Own colors" keeps the colors the slot draws with as a pair of
            // the block's own; with no slot, the pair is typed below.
            let own_pair = (!light.is_empty() && !dark.is_empty()).then(|| serde_json::json!({ "light": light.clone(), "dark": dark.clone() }));
            let own_pair = Signal::new(own_pair);
            let pair = pair_view(__scope, store, pane, key, light, dark);
            let el = rsx! {
                div {
                    class: "pimble-theme-editor__color-ref",
                    Select {
                        size: "xs",
                        value_fn: move || current.get(),
                        onchange: move |value: String| {
                            if !value.is_empty() {
                                write(store, pane, key, Some(Json::from(value)));
                            } else if let Some(pair) = untracked(|| own_pair.get()) {
                                write(store, pane, key, Some(pair));
                            }
                        },
                        data: {move || data.get()},
                    }
                }
            };
            el.append_child(&pair);
            el
        }
    }
}

/// Two `#rrggbb` fields, light then dark.
fn pair_view(__scope: &mut RenderScope, store: AppStore, pane: PaneId, key: &'static str, light: String, dark: String) -> NodeHandle {
    let held = Signal::new((light.clone(), dark.clone()));
    rsx! {
        div {
            class: "pimble-theme-editor__pair",
            input {
                class: "pimble-properties__input",
                placeholder: "light",
                value: light,
                onchange: move |text: String| {
                    let (l, d) = held.get();
                    pair_typed(store, pane, key, &l, &d, false, &text);
                },
            }
            input {
                class: "pimble-properties__input",
                placeholder: "dark",
                value: dark,
                onchange: move |text: String| {
                    let (l, d) = held.get();
                    pair_typed(store, pane, key, &l, &d, true, &text);
                },
            }
        }
    }
}

/// A list's value as its text: a string as it is, anything else as JSON.
fn json_text(json: &Json) -> String {
    json.as_str().map(str::to_string).unwrap_or_else(|| json.to_string())
}
