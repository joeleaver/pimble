//! The Theme picker (docs/THEMES_CONTRACT.md section 9): a row's "Theme...",
//! the Properties panel's Theme line and the Appearance picker's Theme row
//! open it for one node. It says which theme is in force and where it comes
//! from, and has two columns (Joe, 2026-11-24): the themes listed on the left ("Use the
//! parent's", this store's themes, the built-ins, other open stores'
//! themes), and on the right a preview of the node's own text in the theme
//! under the pointer, or, before the pointer has been over one, the theme
//! in force. The preview is drawn by the same rules as the note
//! (`theme_preview.rs`). A click applies at once through
//! `SetNodeAppearance`, which writes `custom["theme"]` in the node's
//! document, so it is shared with everyone who reads the tree; another
//! store's theme is copied in first (`theme_picker_list.rs`).
//! "Customize..." makes a new theme based on the one previewed and opens it
//! in the theme editor.

use rinch::prelude::*;

use pimble_core::{NodeId, StoreAccess, StoreId};
use pimble_themes::Origin;

use super::theme_picker_list::{self as list, Choice, Row};
use crate::state::AppStore;
use crate::themes;

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
    // It lists this store's themes and every other open store's.
    for open in untracked(|| store.store_ids.get()) {
        themes::want_listed(store, open);
    }
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
    let in_force_line = move || target().map(|(s_id, n_id)| themes::sentence(store, s_id, n_id)).unwrap_or_default();
    let unresolved_lines = move || match target() {
        Some((s_id, n_id)) => {
            let (in_force, walk) = themes::resolve(store, s_id, n_id);
            themes::unresolved_sentences(&in_force, &walk).join(" ")
        }
        None => String::new(),
    };
    // What "Use the parent's" reads in: the theme above, else the note's
    // paragraphs fallback (Document for a note spaced before themes).
    let parent_hint = move || match target() {
        Some((s_id, n_id)) => {
            let id = themes::in_force_without_own(store, s_id, n_id).id;
            format!("Reads in {} here.", themes::theme_name(&themes::of_store(store, s_id), &id))
        }
        None => String::new(),
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
    // The theme the preview draws, tracked: the row previewed, "Use the
    // parent's" as the theme above, and before any row the theme in force.
    let shown = move || -> Option<Choice> {
        let (s_id, n_id) = target()?;
        Some(match previewed.get() {
            Some(Choice::Parent) => Choice::Theme(themes::in_force_without_own(store, s_id, n_id).id),
            Some(choice) => choice,
            None => Choice::Theme(themes::resolve(store, s_id, n_id).0.id),
        })
    };
    let drawn = rinch::core::reactive::Memo::new(move || {
        let Some((s_id, _)) = target() else { return Default::default() };
        shown().map(|choice| list::drawn_of(store, s_id, &choice, store.dark_mode.get())).unwrap_or_default()
    });
    let shown_name = move || match (target(), shown()) {
        (Some((s_id, _)), Some(choice)) => list::name_of(store, s_id, &choice),
        _ => String::new(),
    };
    let caption = move || {
        let name = shown_name();
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

    let choose = move |choice: Choice| {
        if let Some(target) = untracked(target) {
            list::choose(store, target, &choice);
        }
    };
    let builtin_rows: Vec<NodeHandle> = pimble_themes::builtins()
        .map(|theme| {
            let row = Row { key: theme.id.clone(), choice: Choice::Theme(theme.id.clone()), name: theme.name.clone(), hint: None, node: None };
            theme_row(__scope, store, row, own, row_class, previewed, choose)
        })
        .collect();
    let store_rows = move || target().map(|(s_id, _)| list::store_rows(store, s_id)).unwrap_or_default();
    let other_rows = move || target().map(|(s_id, _)| list::other_rows(store, s_id)).unwrap_or_default();

    // "Customize...": the previewed theme (or the theme in force) as the
    // base of a new one applied here; and, when the theme in force comes
    // from an ancestor, in its place there.
    let customizable = move || shown().is_some_and(|choice| !matches!(choice, Choice::Parent));
    let customize_label = move || format!("Customize {}...", shown_name());
    let customize_here = move || {
        if let (Some((s_id, n_id)), Some(choice)) = (untracked(target), untracked(shown)) {
            list::customize(store, s_id, n_id, &choice);
        }
    };
    // The ancestor naming the theme in force, while that is what is previewed.
    let named_above = move || -> Option<(NodeId, String)> {
        let (s_id, n_id) = target()?;
        if previewed.get().is_some() {
            return None;
        }
        let (in_force, walk) = themes::resolve(store, s_id, n_id);
        match in_force.origin {
            Origin::Named { depth } if depth > 0 => walk.steps.get(depth).map(|step| (step.node_id, step.label.clone())),
            _ => None,
        }
    };
    let customize_above = move || {
        if let (Some((s_id, _)), Some((above, _)), Some(choice)) = (untracked(target), untracked(named_above), untracked(shown)) {
            list::customize(store, s_id, above, &choice);
        }
    };

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
                            onclick: move || choose(Choice::Parent),
                            div { "Use the parent's" }
                            div { class: "pimble-theme-choice__hint", {move || parent_hint()} }
                        }
                        div {
                            class: "pimble-appearance__label",
                            style: {move || if store_rows().is_empty() { "display: none;" } else { "" }},
                            "This store's themes"
                        }
                        for row in store_rows() {
                            div {
                                key: row.key.clone(),
                                {theme_row(__scope, store, row.clone(), own, row_class, previewed, choose)}
                            }
                        }
                        div { class: "pimble-appearance__label", "Built-in" }
                        {builtin_rows}
                        div {
                            class: "pimble-appearance__label",
                            style: {move || if other_rows().is_empty() { "display: none;" } else { "" }},
                            "From other open stores"
                        }
                        for row in other_rows() {
                            div {
                                key: row.key.clone(),
                                {theme_row(__scope, store, row.clone(), own, row_class, previewed, choose)}
                            }
                        }
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
                    style: "margin-top: 14px; display: flex; gap: 8px; justify-content: flex-end;",
                    div {
                        style: {move || if customizable() && writable() { "" } else { "display: none;" }},
                        Button {
                            variant: "default",
                            onclick: move || customize_here(),
                            {move || customize_label()}
                        }
                    }
                    div {
                        style: {move || if named_above().is_some() && writable() { "" } else { "display: none;" }},
                        Button {
                            variant: "default",
                            onclick: move || customize_above(),
                            {move || named_above().map(|(_, label)| format!("Customize at '{label}'...")).unwrap_or_default()}
                        }
                    }
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

/// One theme row: its name, a hint under it, and "Edit" for a theme of this
/// store. Hovering previews it; a click on the name applies it.
#[allow(clippy::too_many_arguments)]
fn theme_row(
    __scope: &mut RenderScope,
    store: AppStore,
    row: Row,
    own: impl Fn() -> Option<String> + Copy + 'static,
    row_class: impl Fn(Choice, bool) -> String + Copy + 'static,
    previewed: Signal<Option<Choice>>,
    choose: impl Fn(Choice) + Copy + 'static,
) -> NodeHandle {
    // Copy handles, since every closure below may run many times.
    let choice = Signal::new(row.choice.clone());
    let hint = Signal::new(row.hint.clone().unwrap_or_default());
    let name = Signal::new(row.name.clone());
    let hint_style = if row.hint.is_some() { "" } else { "display: none;" };
    let edit_style = if row.node.is_some() { "" } else { "display: none;" };
    let node = row.node;
    rsx! {
        div {
            class: {move || {
                let choice = choice.get();
                let active = matches!(&choice, Choice::Theme(id) if own().as_deref() == Some(id.as_str()));
                format!("{} pimble-theme-choice--row", row_class(choice, active))
            }},
            onmouseenter: move || previewed.set(Some(choice.get())),
            div {
                class: "pimble-theme-choice__name",
                onclick: move || choose(choice.get()),
                div { {move || name.get()} }
                div { class: "pimble-theme-choice__hint", style: hint_style, {move || hint.get()} }
            }
            div {
                class: "pimble-theme-choice__edit",
                style: edit_style,
                title: "Edit Theme...",
                onclick: move || {
                    if let Some(node) = node {
                        list::edit(store, node);
                    }
                },
                "Edit"
            }
        }
    }
}
