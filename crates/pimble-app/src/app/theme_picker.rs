//! The Theme picker (docs/THEMES_CONTRACT.md section 9): Format > Theme...,
//! a row's "Theme..." and the Appearance picker's Theme row open it for one
//! node. It says which theme is in force and where it comes from, lists the
//! built-ins with a sample of each, and "Use the parent's" clears the node's
//! own. Every click applies at once through `SetNodeAppearance`, which
//! writes `custom["theme"]` in the node's document, so it is shared with
//! everyone who reads the tree. This store's themes and other open stores'
//! come with theme nodes (wave 2).

use rinch::prelude::*;

use pimble_core::{NodeId, StoreAccess, StoreId};

use crate::protocol::BackendCommand;
use crate::state::AppStore;
use crate::themes;

/// The words every sample shows: a heading and two paragraphs, so a theme's
/// heading style, body font and paragraph spacing all show.
const SAMPLE_HEADING: &str = "The Harbor";
const SAMPLE_FIRST: &str = "The boats came in after noon, low with the morning's catch.";
const SAMPLE_SECOND: &str = "By evening the quay was quiet again.";

/// Open the picker for `(store_id, node_id)`.
pub(crate) fn open_theme_picker(store: AppStore, store_id: StoreId, node_id: NodeId) {
    store.theme_picker_node.set(Some((store_id, node_id)));
}

/// Format > Theme...: the picker for the focused pane's node, or a sentence
/// saying there is none.
pub(crate) fn open_theme_picker_for_focused(store: AppStore) {
    let pane = store.focused();
    match untracked(|| crate::pane_view::pane_document(store, pane)) {
        Some((store_id, node_id)) => open_theme_picker(store, store_id, node_id),
        None => crate::events::show_notice(store, "Open a note to choose its theme.".to_string()),
    }
}

/// The picker.
pub(super) fn render(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let target = move || store.theme_picker_node.get();
    // The theme the node itself names, tracked.
    let own = move || -> Option<String> {
        let (s_id, n_id) = target()?;
        store.get_node_signal(s_id, n_id)?.with(|n| n.metadata.theme().map(String::from))
    };
    let writable = move || target().is_some_and(|(s_id, n_id)| store.node_access_tracked(s_id, n_id).allows_write());
    let in_force_line = move || match target() {
        Some((s_id, n_id)) => {
            let (in_force, walk) = themes::resolve(store, s_id, n_id);
            themes::in_force_sentence(&in_force, &walk)
        }
        None => String::new(),
    };
    let unresolved_lines = move || match target() {
        Some((s_id, n_id)) => {
            let (in_force, walk) = themes::resolve(store, s_id, n_id);
            themes::unresolved_sentences(&in_force, &walk).join(" ")
        }
        None => String::new(),
    };
    let send = move |theme: Option<String>| {
        let Some((store_id, node_id)) = untracked(target) else { return };
        // A native control cannot always be greyed out in time, so the
        // check the rows draw with is made again here.
        if !untracked(|| store.node_access(store_id, node_id).allows_write()) {
            crate::events::show_notice(store, StoreAccess::READ_ONLY_REFUSAL.to_string());
            return;
        }
        store.send(BackendCommand::SetNodeAppearance { store_id, node_id, icon: None, color: None, background: None, tags: None, theme: Some(theme) });
    };
    let row_class = move |active: bool| {
        let mut class = String::from("pimble-theme-choice");
        if active {
            class.push_str(" pimble-theme-choice--active");
        }
        if !writable() {
            class.push_str(" pimble-theme-choice--disabled");
        }
        class
    };

    let builtin_rows: Vec<NodeHandle> = pimble_themes::builtins()
        .map(|theme| {
            let id = theme.id.clone();
            let name = theme.name.clone();
            let active_id = id.clone();
            let class_id = id.clone();
            let style_id = id.clone();
            let click_id = id.clone();
            let active = rinch::core::reactive::Memo::new(move || own().as_deref() == Some(active_id.as_str()));
            rsx! {
                div {
                    class: {move || row_class(active.get())},
                    onclick: move || send(Some(click_id.clone())),
                    div { class: "pimble-theme-choice__name", {name.clone()} }
                    div {
                        class: {move || themes::classes("pimble-theme-sample", &themes::drawn_for(&class_id, store.dark_mode.get()))},
                        style: {move || themes::drawn_for(&style_id, store.dark_mode.get()).style},
                        div { class: "pimble-theme-sample__heading", {SAMPLE_HEADING} }
                        p { {SAMPLE_FIRST} }
                        p { {SAMPLE_SECOND} }
                    }
                }
            }
        })
        .collect();

    rsx! {
        Modal {
            opened_fn: move || target().is_some(),
            onclose: move || store.theme_picker_node.set(None),
            title: "Theme",
            size: "md",

            div {
                div { class: "pimble-theme-picker__in-force", {move || in_force_line()} }
                div {
                    class: "pimble-theme-picker__note",
                    style: {move || if unresolved_lines().is_empty() { "display: none;" } else { "" }},
                    {move || unresolved_lines()}
                }
                div {
                    class: "pimble-theme-picker__note",
                    style: {move || if writable() { "display: none;" } else { "" }},
                    {StoreAccess::READ_ONLY_REFUSAL}
                }
                div {
                    class: {move || row_class(own().is_none())},
                    onclick: move || send(None),
                    div { class: "pimble-theme-choice__name", "Use the parent's" }
                    div { class: "pimble-theme-choice__hint", "Clears this node's own theme: it reads in the one above it, or Default." }
                }
                div { class: "pimble-appearance__label", "Built-in" }
                div { class: "pimble-theme-picker__list", {builtin_rows} }
                div {
                    style: "margin-top: 14px; display: flex; justify-content: flex-end;",
                    Button {
                        variant: "filled",
                        onclick: move || store.theme_picker_node.set(None),
                        "Done"
                    }
                }
            }
        }
    }
}
