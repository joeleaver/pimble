//! The application's menus, as data, so both shells build the same ones.
//!
//! The desktop turns this into `rinch::menu::Menu`s for its native menu bar
//! ([`build_menus`]); the browser will hand the same spec to rinch-web's
//! DOM menu bar once `rinch::menu` compiles without the `desktop` feature.
//! Keeping the content here rather than inline in [`crate::app::run`] is what
//! stops the two drifting, and it is why the spec carries no rinch type: it has
//! to build on a target where `rinch::menu` does not exist yet.
//!
//! An item appears on a target only where it can work. The ones that open a
//! native file dialog or administer a server's stores are `native`; the ones
//! that talk to the accounts service are not. Nothing here is ever a label with
//! no action behind it — a menu that lists what it cannot do is the thing this
//! module exists to prevent.

use std::rc::Rc;

use crate::state::AppStore;

/// One line in a menu.
pub enum MenuEntry {
    Item {
        label: &'static str,
        /// An accelerator, as rinch spells them (`"Ctrl+K"`), or empty.
        shortcut: &'static str,
        /// What it does. Always something.
        action: Rc<dyn Fn()>,
    },
    Separator,
}

impl MenuEntry {
    fn item(label: &'static str, shortcut: &'static str, action: impl Fn() + 'static) -> Self {
        MenuEntry::Item { label, shortcut, action: Rc::new(action) }
    }
}

/// One top-level menu.
pub struct MenuSection {
    pub title: &'static str,
    pub entries: Vec<MenuEntry>,
}

/// Every menu this build offers.
pub fn menu_spec(store: AppStore) -> Vec<MenuSection> {
    let mut sections = vec![MenuSection { title: "File", entries: file_entries(store) }];
    // The desktop signs in through its own server's keystore
    // (docs/DESKTOP_ACCOUNT_CONTRACT.md); the browser has account pages of
    // its own, reached from the File menu above.
    #[cfg(feature = "native")]
    sections.push(MenuSection { title: "Account", entries: account_entries(store) });
    sections.push(MenuSection { title: "Edit", entries: edit_entries(store) });
    sections.push(MenuSection { title: "View", entries: view_entries(store) });
    sections
}

/// A native menu cannot re-label its items reactively and no label here may
/// be dead, so the one item is "Account..." whichever face the modal shows
/// (docs/DESKTOP_ACCOUNT_CONTRACT.md decision 1).
#[cfg(feature = "native")]
fn account_entries(store: AppStore) -> Vec<MenuEntry> {
    vec![
        MenuEntry::item("Account...", "", move || crate::app::open_account_modal(store, "")),
        MenuEntry::item("Add Hosted Store...", "", move || crate::app::open_hosted_modal(store)),
    ]
}

fn file_entries(store: AppStore) -> Vec<MenuEntry> {
    let mut entries = Vec::new();

    // "New Store..." exists on both, and means something different on each: a
    // directory the desktop picks a path for, a hosted store the browser asks
    // the accounts service for and mints a key for.
    #[cfg(feature = "native")]
    entries.push(MenuEntry::item("New Store...", "Ctrl+N", move || {
        crate::app::pick_and_create_store(store)
    }));
    #[cfg(not(feature = "native"))]
    entries.push(MenuEntry::item("New Store...", "Ctrl+N", move || {
        crate::app::open_new_store_modal(store)
    }));

    #[cfg(feature = "native")]
    {
        entries.push(MenuEntry::item("Open Store...", "Ctrl+O", move || {
            crate::app::pick_and_open_store(store)
        }));
        entries.push(MenuEntry::item("Add Remote Store...", "", move || {
            crate::app::open_connect_modal(store, None)
        }));
        entries.push(MenuEntry::Separator);
        entries.push(MenuEntry::item("Close Store", "", move || {
            if let Some((store_id, _)) = store.selected_store_and_node() {
                store.send(crate::protocol::BackendCommand::CloseStore { store_id });
            }
        }));
        entries.push(MenuEntry::Separator);
        entries.push(MenuEntry::item("Exit", "Alt+F4", || {
            rinch::prelude::close_current_window()
        }));
    }

    // A browser has an account behind it, and the way out of one.
    #[cfg(not(feature = "native"))]
    {
        entries.push(MenuEntry::Separator);
        entries.push(MenuEntry::item("Account", "", crate::app::run_account_action));
        entries.push(MenuEntry::item("Sign Out", "", crate::app::run_sign_out_action));
    }

    entries
}

fn edit_entries(store: AppStore) -> Vec<MenuEntry> {
    vec![MenuEntry::item("Delete", "", move || {
        // A native menu item cannot be greyed out reactively, so the check a
        // row's "Delete" makes at render time is made here when it runs: a
        // node shared with this device to read changes in no way
        // (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles"). An editor
        // deletes like anyone else.
        if let Some((store_id, node_id)) = store.selected_store_and_node() {
            if !store.node_access(store_id, node_id).allows_write() {
                tracing::info!("Delete refused: {:?}/{:?} is shared with us to read", store_id, node_id);
                return;
            }
            store.send(crate::protocol::BackendCommand::DeleteNode { store_id, node_id });
        }
    })]
}

fn view_entries(store: AppStore) -> Vec<MenuEntry> {
    vec![
        MenuEntry::item("Toggle Dark Mode", "", move || crate::app::toggle_dark_mode(store)),
        MenuEntry::Separator,
        MenuEntry::item("Focus Search", "Ctrl+K", crate::app::focus_search),
        MenuEntry::item("Rebuild Search Index", "", move || {
            crate::app::rebuild_search_indexes(store)
        }),
        MenuEntry::Separator,
        // docs/MOVE_CONTRACT.md "Seeing and undoing what was removed": the
        // store of the current selection, else the first open store.
        MenuEntry::item("Recently Deleted...", "", move || {
            crate::app::open_deleted_modal_for_selection(store)
        }),
    ]
}

/// The spec as rinch menus, for a shell that has a menu bar.
///
/// Both shells call this. `rinch::menu` — the types and the renderer, not the
/// `muda` builders — is platform-independent, so the desktop's native bar and
/// rinch-web's DOM bar are handed the very same values.
pub fn build_menus(store: AppStore) -> Vec<(&'static str, rinch::menu::Menu)> {
    use rinch::menu::{Menu, MenuItem};

    menu_spec(store)
        .into_iter()
        .map(|section| {
            let mut menu = Menu::new();
            for entry in section.entries {
                menu = match entry {
                    MenuEntry::Separator => menu.separator(),
                    MenuEntry::Item { label, shortcut, action } => {
                        let mut item = MenuItem::new(label);
                        if !shortcut.is_empty() {
                            item = item.shortcut(shortcut);
                        }
                        menu.item(item.on_click(move || action()))
                    }
                };
            }
            (section.title, menu)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every entry either separates or does something. The point of the spec is
    /// that a shell cannot render a label with nothing behind it.
    #[test]
    fn every_item_has_an_action() {
        let store = AppStore::new();
        let mut items = 0;
        for section in menu_spec(store) {
            assert!(!section.title.is_empty());
            for entry in section.entries {
                if let MenuEntry::Item { label, .. } = entry {
                    assert!(!label.is_empty());
                    items += 1;
                }
            }
        }
        assert!(items >= 5, "a menu bar with {items} items is not one");
    }
}
