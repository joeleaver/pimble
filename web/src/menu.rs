//! The browser's half of the menu bar.
//!
//! The menus themselves are `pimble_app::menus`, shared with the desktop so the
//! two cannot drift. Two of the browser's items do something no shared crate
//! can do on its own — reach the accounts service and this page's key store —
//! so they are registered here as hooks before the view is built, exactly as
//! the sidebar's account button is.
//!
//! **What is still missing.** rinch already has a DOM-rendered menu bar
//! (`crates/rinch/src/menu/app_menu_bar.rs`, used on Linux) driven by the same
//! `rinch::menu` API the desktop uses, but `rinch::menu` is behind the `desktop`
//! feature and rinch-web has no entry point that takes menus. Once the upstream
//! branch lands, `pimble_app::menus::build_menus` loses its `cfg` and
//! `crate::route::mount_app` becomes one call:
//!
//! ```ignore
//! rinch_web::mount_with_menu_bar(theme, pimble_app::menus::build_menus(store), build);
//! ```
//!
//! Until then the items reachable another way are: "New Store..." through the
//! explorer's "+", "Account" through the sidebar's account button, "Focus
//! Search" through Ctrl+K (`crate::shortcuts`), and "Toggle Dark Mode" through
//! the browser's own colour scheme. "Sign Out" and "Rebuild Search Index" have
//! no other route yet, which is the gap the menu bar closes.

use wasm_bindgen_futures::spawn_local;

use crate::route::{self, Route};
use crate::{accounts, session};

/// Register what the browser's menu items do. Called before `build_view`.
pub fn install_hooks() {
    // The sidebar's account button and the menu's "Account" are the same move.
    pimble_app::app::set_account_action(|| route::go(Route::Account));

    pimble_app::app::set_sign_out_action(|| {
        spawn_local(async move {
            // Drop the keys first: whatever the network does next, this page
            // stops holding them.
            session::clear();
            let _ = accounts::logout().await;
            route::replace_with(Route::Login);
        });
    });
}

#[cfg(test)]
mod tests {
    /// The spec builds on this target and every item it offers has an action.
    ///
    /// `pimble_app::menus` has its own version of this; the point of repeating
    /// it here is that it runs against the *web* configuration of the spec,
    /// where a different set of items is compiled in.
    #[test]
    fn the_web_menus_are_all_actionable() {
        use pimble_app::menus::{menu_spec, MenuEntry};

        let store = pimble_app::state::AppStore::new();
        let mut labels = Vec::new();
        for section in menu_spec(store) {
            for entry in section.entries {
                if let MenuEntry::Item { label, .. } = entry {
                    labels.push(label);
                }
            }
        }

        // The items that only make sense with an account behind the app.
        assert!(labels.contains(&"Account"), "{labels:?}");
        assert!(labels.contains(&"Sign Out"), "{labels:?}");
        assert!(labels.contains(&"New Store..."), "{labels:?}");
        // And none that would need a file dialog or a service principal.
        assert!(!labels.contains(&"Open Store..."), "{labels:?}");
        assert!(!labels.contains(&"Add Remote Store..."), "{labels:?}");
        assert!(!labels.contains(&"Close Store"), "{labels:?}");
        assert!(!labels.contains(&"Exit"), "{labels:?}");
    }
}
