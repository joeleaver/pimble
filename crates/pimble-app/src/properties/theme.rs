//! The panel's "Theme" section (docs/THEMES_CONTRACT.md section 9): the
//! document theme the node reads in and where it comes from, and a line that
//! opens the Theme picker for it. A theme is how a node and its subtree are
//! drawn, not a format of its text, so it lives with the node's properties
//! and its row menu (Joe, 2026-11-24), never in a menu of the menu bar.

use rinch::prelude::*;

use super::shown;
use crate::state::AppStore;

/// The panel's "Theme" section.
pub(super) fn render_theme(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let in_force = move || match shown(store) {
        Some((s, n)) => crate::themes::sentence(store, s, n),
        None => String::new(),
    };
    rsx! {
        div {
            class: "pimble-properties__section",
            div { class: "pimble-properties__heading", "Theme" }
            div { class: "pimble-properties__theme", {move || in_force()} }
            div {
                class: "pimble-properties__add",
                onclick: move || {
                    if let Some((s, n)) = untracked(|| shown(store)) {
                        crate::app::open_theme_picker(store, s, n);
                    }
                },
                "Choose theme..."
            }
        }
    }
}
