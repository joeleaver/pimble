//! One Paste (docs/MENUS_CONTRACT.md section 3.1; Joe, 2026-11-04: "'copy'
//! copies everything, paste could choose what to paste"). What a row's Paste
//! can do is judged when it is chosen, from what is held then: the rows cut
//! (moved here, inside the row, last), the node copied (mounted here), the
//! picture on the system clipboard (a canvas here, desktop). One choice is done
//! at once; several are offered in a small chooser; none is said.

use rinch::prelude::*;

use crate::protocol::BackendCommand;
use crate::state::{parse_tree_value, AppStore};

/// Something Paste can do on a row.
#[derive(Debug, Clone, PartialEq)]
pub enum Choice {
    /// Move the cut rows inside it.
    Move { what: String },
    /// Mount the copied node inside it.
    Mount { what: String },
    /// The picture on the clipboard, as a canvas inside it.
    Picture,
}

impl Choice {
    /// The choice as the chooser names it.
    pub fn label(&self) -> String {
        match self {
            Choice::Move { what } => format!("Move {what} here"),
            Choice::Mount { what } => format!("Mount {what} here"),
            Choice::Picture => "The picture, as a canvas".to_string(),
        }
    }
}

/// Paste's choices on a row, while the chooser is open.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub row: String,
    pub choices: Vec<Choice>,
}

fn quoted(name: &str) -> String {
    format!("\u{201c}{name}\u{201d}")
}

/// What Paste can do on `row` now.
fn choices(store: AppStore, row: &str) -> Vec<Choice> {
    let mut choices = Vec::new();
    let cut = crate::tree_keys::cut_rows();
    match cut.as_slice() {
        [] => {}
        [one] => choices.push(Choice::Move { what: quoted(&crate::tree_keys::label_of(store, one)) }),
        many => choices.push(Choice::Move { what: format!("the {} cut items", many.len()) }),
    }
    // A mount: not in or from an encrypted store (the server has no tree
    // there), and not of the row onto itself.
    if let Some((source_store, source_node, title)) = untracked(|| store.mount_source.get()) {
        let target = parse_tree_value(row);
        let onto_itself = matches!(target, Some((s, Some(n))) if s == source_store && n == source_node);
        let vault = target.is_some_and(|(s, _)| store.is_vault(s)) || store.is_vault(source_store);
        if !onto_itself && !vault {
            choices.push(Choice::Mount { what: quoted(&title) });
        }
    }
    #[cfg(feature = "native")]
    if rinch::clipboard::paste_image().is_ok() {
        choices.push(Choice::Picture);
    }
    choices
}

/// A row menu's Paste on `row`.
pub(crate) fn paste(store: AppStore, row: &str) {
    let choices = choices(store, row);
    match choices.as_slice() {
        [] => {
            #[cfg(feature = "native")]
            let line = "There is nothing here to paste.";
            // The browser cannot read a picture from a menu: Ctrl+V on the
            // selected row takes one (`boards::install_browser_handlers`).
            #[cfg(not(feature = "native"))]
            let line = "There is nothing here to paste. To paste a picture, select the row and press Ctrl+V.";
            crate::events::show_notice(store, line.to_string());
        }
        [one] => choose(store, row, one),
        _ => store.paste_offer.set(Some(Offer { row: row.to_string(), choices })),
    }
}

/// Ctrl+V in the explorer with nothing cut (Joe, 2026-11-04: "ctrl+v paste
/// could always just be a straight paste"): no chooser; the picture on the
/// clipboard if there is one, else a mount of the copied node.
pub(crate) fn paste_straight(store: AppStore, row: &str) {
    let choices = choices(store, row);
    let pick = choices
        .iter()
        .find(|c| matches!(c, Choice::Move { .. }))
        .or_else(|| choices.iter().find(|c| matches!(c, Choice::Picture)))
        .or_else(|| choices.iter().find(|c| matches!(c, Choice::Mount { .. })));
    match pick {
        Some(choice) => choose(store, row, &choice.clone()),
        None => paste(store, row),
    }
}

/// Do `choice` on `row`.
pub(crate) fn choose(store: AppStore, row: &str, choice: &Choice) {
    store.paste_offer.set(None);
    match choice {
        Choice::Move { .. } => crate::tree_keys::move_cut_into(store, row),
        Choice::Mount { .. } => {
            let Some((source_store, source_node, title)) = untracked(|| store.mount_source.get()) else { return };
            // Under a mount it goes into the mount's source, like anything
            // made on a row, and only where this device may write.
            let Some((store_id, parent_id)) = crate::tree_view::create_parent_of_row(store, row) else { return };
            store.send(BackendCommand::CreateMount { store_id, parent_id, source_store_id: source_store, source_node_id: source_node, title: Some(title) });
        }
        Choice::Picture => {
            #[cfg(feature = "native")]
            if let Some((s, parent)) = crate::tree_view::create_parent_of_row(store, row) {
                crate::boards::paste_image(store, crate::boards::Dest::Under(s, parent));
            }
        }
    }
}

/// The chooser: one button per choice.
pub(crate) fn render_chooser(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    rsx! {
        Modal {
            opened_fn: move || store.paste_offer.with(|o| o.is_some()),
            onclose: move || store.paste_offer.set(None),
            title: "Paste",
            size: "sm",
            div {
                style: "display: flex; flex-direction: column; gap: 8px;",
                for choice in store.paste_offer.get().map(|o| o.choices).unwrap_or_default() {
                    Button {
                        key: choice.label(),
                        variant: "default",
                        size: "sm",
                        onclick: {
                            let choice = choice.clone();
                            move || {
                                if let Some(offer) = untracked(|| store.paste_offer.get()) {
                                    choose(store, &offer.row, &choice);
                                }
                            }
                        },
                        {choice.label()}
                    }
                }
            }
        }
    }
}
