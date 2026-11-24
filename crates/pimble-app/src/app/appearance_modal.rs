//! The "Appearance..." modal: a node's icon and colors, and the way to its
//! document theme.

use rinch::prelude::*;

use crate::protocol::BackendCommand;
use crate::appearance::{icons_matching, IconGlyph, COLOR_CHOICES, ROW_COLOR_CHOICES};
use crate::state::AppStore;

/// Open the "Appearance..." picker for `(store_id, node_id)`, its icon
/// search cleared. Tags are the Properties panel's.
pub(super) fn open_appearance_modal(store: AppStore, store_id: pimble_core::StoreId, node_id: pimble_core::NodeId) {
    store.appearance_icon_query.set(String::new());
    store.appearance_modal_node.set(Some((store_id, node_id)));
}

/// The "Appearance..." modal.
pub(super) fn render(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    // ── "Appearance..." modal (node context menu) ───────────────────
    // Picks a colour and an icon for one node; every click applies at
    // once through `SetNodeAppearance`, and the selection shown follows
    // the node's own signal, so a change from another window shows too.
    let appearance_node_color = move || -> Option<String> {
        let (s_id, n_id) = store.appearance_modal_node.get()?;
        store.get_node_signal(s_id, n_id)?.with(|n| n.metadata.color().map(String::from))
    };
    let appearance_node_icon = move || -> Option<String> {
        let (s_id, n_id) = store.appearance_modal_node.get()?;
        store.get_node_signal(s_id, n_id)?.with(|n| n.metadata.icon().map(String::from))
    };
    let appearance_node_background = move || -> Option<String> {
        let (s_id, n_id) = store.appearance_modal_node.get()?;
        store.get_node_signal(s_id, n_id)?.with(|n| n.metadata.background().map(String::from))
    };
    // The document theme in force on the node and where it comes from
    // (docs/THEMES_CONTRACT.md section 9); "Theme..." opens its picker.
    let theme_line = move || -> String {
        let Some((s_id, n_id)) = store.appearance_modal_node.get() else { return String::new() };
        let (in_force, walk) = crate::themes::resolve(store, s_id, n_id);
        crate::themes::in_force_sentence(&in_force, &walk)
    };
    let open_theme = move || {
        if let Some((store_id, node_id)) = untracked(|| store.appearance_modal_node.get()) {
            store.appearance_modal_node.set(None);
            super::theme_picker::open_theme_picker(store, store_id, node_id);
        }
    };
    let send_appearance = move |icon: Option<Option<String>>, color: Option<Option<String>>, background: Option<Option<String>>| {
        if let Some((store_id, node_id)) = untracked(|| store.appearance_modal_node.get()) {
            store.send(BackendCommand::SetNodeAppearance { store_id, node_id, icon, color, background, tags: None, theme: None });
        }
    };
    let swatches: Vec<NodeHandle> = COLOR_CHOICES
        .iter()
        .map(|&(name, hex)| {
            rsx! {
                div {
                    class: {move || {
                        if appearance_node_color().as_deref() == Some(hex) { "pimble-swatch pimble-swatch--active" } else { "pimble-swatch" }
                    }},
                    style: format!("background: {hex};"),
                    title: name,
                    onclick: move || send_appearance(None, Some(Some(hex.to_string())), None),
                }
            }
        })
        .collect();
    let row_swatches: Vec<NodeHandle> = ROW_COLOR_CHOICES
        .iter()
        .map(|&(name, hex)| {
            rsx! {
                div {
                    class: {move || {
                        if appearance_node_background().as_deref() == Some(hex) { "pimble-swatch pimble-swatch--active" } else { "pimble-swatch" }
                    }},
                    style: format!("background: {hex};"),
                    title: name,
                    onclick: move || send_appearance(None, None, Some(Some(hex.to_string()))),
                }
            }
        })
        .collect();
    rsx! {
        Modal {
            opened_fn: move || store.appearance_modal_node.get().is_some(),
            onclose: move || store.appearance_modal_node.set(None),
            title: "Appearance",
            size: "md",

            div {
                div { class: "pimble-appearance__label", "Text" }
                div {
                    class: "pimble-appearance__row",
                    div {
                        class: {move || {
                            if appearance_node_color().is_none() { "pimble-swatch pimble-swatch--none pimble-swatch--active" } else { "pimble-swatch pimble-swatch--none" }
                        }},
                        onclick: move || send_appearance(None, Some(None), None),
                        "None"
                    }
                    {swatches}
                }
                // The whole row tinted, as Scrivener's full-width binder
                // labels are; independent of the text color above.
                div { class: "pimble-appearance__label", "Background" }
                div {
                    class: "pimble-appearance__row",
                    div {
                        class: {move || {
                            if appearance_node_background().is_none() { "pimble-swatch pimble-swatch--none pimble-swatch--active" } else { "pimble-swatch pimble-swatch--none" }
                        }},
                        onclick: move || send_appearance(None, None, Some(None)),
                        "None"
                    }
                    {row_swatches}
                }
                div { class: "pimble-appearance__label", "Theme" }
                div {
                    class: "pimble-appearance__row",
                    div { class: "pimble-appearance__theme", {move || theme_line()} }
                    Button {
                        variant: "default",
                        size: "xs",
                        onclick: open_theme,
                        "Theme..."
                    }
                }
                div { class: "pimble-appearance__label", "Icon" }
                TextInput {
                    placeholder: "Search all icons...",
                    value_fn: move || store.appearance_icon_query.get(),
                    oninput: move |val: String| store.appearance_icon_query.set(val),
                }
                div {
                    class: "pimble-appearance__row",
                    style: "margin-top: 6px;",
                    div {
                        class: {move || {
                            if appearance_node_icon().is_none() { "pimble-swatch pimble-swatch--none pimble-swatch--active" } else { "pimble-swatch pimble-swatch--none" }
                        }},
                        onclick: move || send_appearance(Some(None), None, None),
                        "Default"
                    }
                    // The curated set, or every Tabler icon matching the
                    // search: reactive on the query, one component per icon.
                    for icon in icons_matching(&store.appearance_icon_query.get()) {
                        div {
                            key: icon.name(),
                            class: {move || {
                                if appearance_node_icon().as_deref() == Some(icon.name()) { "pimble-icon-choice pimble-icon-choice--active" } else { "pimble-icon-choice" }
                            }},
                            title: icon.name(),
                            onclick: move || send_appearance(Some(Some(icon.name().to_string())), None, None),
                            IconGlyph { name: icon.name() }
                        }
                    }
                }
                div {
                    style: "margin-top: 14px; display: flex; justify-content: flex-end;",
                    Button {
                        variant: "filled",
                        onclick: move || store.appearance_modal_node.set(None),
                        "Done"
                    }
                }
            }
        }
    }
}
