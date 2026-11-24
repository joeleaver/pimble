//! The Theme picker (docs/THEMES_CONTRACT.md section 9): a row's "Theme...",
//! the Properties panel's Theme line and the Appearance picker's Theme row
//! open it for one node. It says which theme is in force and where it comes
//! from, and has two columns (Joe, 2026-11-24): the themes listed on the left ("Use the
//! parent's", then the built-ins), and on the right a preview of the node's
//! own text in the theme under the pointer, or, before the pointer has
//! been over one, the theme in force. The preview is drawn by the same
//! rules as the note (`theme_preview.rs`). A click applies at once through
//! `SetNodeAppearance`, which writes `custom["theme"]` in the node's
//! document, so it is shared with everyone who reads the tree. This store's
//! themes and other open stores' come with theme nodes (wave 2).

use rinch::prelude::*;

use pimble_core::{NodeId, StoreAccess, StoreId};

use crate::protocol::BackendCommand;
use crate::state::AppStore;
use crate::themes;

/// A row of the list.
#[derive(Debug, Clone, PartialEq)]
enum Choice {
    /// "Use the parent's": no theme of the node's own.
    Parent,
    /// A theme by id.
    Theme(String),
}

/// What the preview column draws: the blocks read when the picker opened,
/// keyed by the opening so a new one draws afresh.
#[derive(Debug, Clone, PartialEq)]
struct Page {
    opening: u64,
    blocks: Vec<themes::preview::Block>,
    /// The blocks are the note's own (an editor holds it), not the sample.
    own: bool,
}

/// Open the picker for `(store_id, node_id)`.
pub(crate) fn open_theme_picker(store: AppStore, store_id: StoreId, node_id: NodeId) {
    store.theme_picker_node.set(Some((store_id, node_id)));
}

/// The node "Theme..." acts on for a row's `(store, node)`: the node, but a
/// mount row's source node. A mounted subtree reads as it does in its own
/// store (decision 5), and a mount node has no content to draw.
pub(crate) fn theme_target(store: AppStore, store_id: StoreId, node_id: NodeId) -> Option<(StoreId, NodeId)> {
    if store.is_mount(store_id, node_id) {
        use pimble_core::plan::TreeView;
        store.view().mount_source(store_id, node_id)
    } else {
        Some((store_id, node_id))
    }
}

/// Node > "Theme...": the picker for the explorer's selected row (a store
/// row's root), as that row's own "Theme..." opens it.
pub(crate) fn open_theme_picker_for_selected(store: AppStore) {
    match untracked(|| store.selected_row_node()).and_then(|(s, n)| theme_target(store, s, n)) {
        Some((store_id, node_id)) => open_theme_picker(store, store_id, node_id),
        None => crate::events::show_notice(store, "Select a node to choose its theme.".to_string()),
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

    // The row last under the pointer: it stays previewed while the pointer
    // goes over to the preview to read it. None until the pointer has been
    // over a row, which previews the theme in force.
    let previewed: Signal<Option<Choice>> = Signal::new(None);
    // The page, read once per opening from the editor holding the node.
    let page: Signal<Vec<Page>> = Signal::new(Vec::new());
    let openings = std::cell::Cell::new(0u64);
    __scope.create_effect(move || {
        let Some((store_id, node_id)) = target() else { return };
        untracked(|| {
            openings.set(openings.get() + 1);
            let (blocks, own) = themes::preview_blocks(store, store_id, node_id);
            previewed.set(None);
            page.set(vec![Page { opening: openings.get(), blocks, own }]);
        });
    });
    // The theme the preview draws, tracked.
    let shown = move || -> Option<String> {
        let (s_id, n_id) = target()?;
        Some(match previewed.get() {
            Some(Choice::Theme(id)) => id,
            Some(Choice::Parent) => themes::in_force_without_own(store, s_id, n_id).id,
            None => themes::resolve(store, s_id, n_id).0.id,
        })
    };
    let drawn = rinch::core::reactive::Memo::new(move || shown().map(|id| themes::drawn_for(&id, store.dark_mode.get())).unwrap_or_default());
    let caption = move || {
        let name = shown().map(|id| themes::theme_name(&id)).unwrap_or_default();
        if page.with(|p| p.first().is_some_and(|p| p.own)) {
            format!("This note in {name}")
        } else {
            format!("A sample in {name}: open the note to see its own text")
        }
    };

    let row_class = move |choice: Choice, active: bool| {
        let mut class = String::from("pimble-theme-choice");
        if active {
            class.push_str(" pimble-theme-choice--active");
        }
        if previewed.get().as_ref() == Some(&choice) {
            class.push_str(" pimble-theme-choice--previewed");
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
            let class_id = id.clone();
            let hover_id = id.clone();
            let click_id = id.clone();
            rsx! {
                div {
                    class: {move || {
                        let active = own().as_deref() == Some(class_id.as_str());
                        row_class(Choice::Theme(class_id.clone()), active)
                    }},
                    onmouseenter: move || previewed.set(Some(Choice::Theme(hover_id.clone()))),
                    onclick: move || send(Some(click_id.clone())),
                    {name.clone()}
                }
            }
        })
        .collect();

    rsx! {
        Modal {
            opened_fn: move || target().is_some(),
            onclose: move || store.theme_picker_node.set(None),
            title: "Theme",
            size: "xl",

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
                    class: "pimble-theme-picker__columns",
                    div {
                        class: "pimble-theme-picker__list",
                        div {
                            class: {move || row_class(Choice::Parent, own().is_none())},
                            onmouseenter: move || previewed.set(Some(Choice::Parent)),
                            onclick: move || send(None),
                            div { "Use the parent's" }
                            div { class: "pimble-theme-choice__hint", "Reads in the theme above it, or Default." }
                        }
                        div { class: "pimble-appearance__label", "Built-in" }
                        {builtin_rows}
                    }
                    div {
                        class: "pimble-theme-picker__preview-column",
                        div { class: "pimble-theme-picker__caption", {move || caption()} }
                        for shown_page in page.get() {
                            div {
                                key: shown_page.opening,
                                class: {move || format!("{} pimble-theme-picker__preview", themes::wrap_classes(&drawn.get(), store.dark_mode.get()))},
                                style: {move || drawn.get().style},
                                {super::theme_preview::page(__scope, store, &shown_page.blocks)}
                            }
                        }
                    }
                }
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
