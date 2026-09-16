//! The browser's own context menu, turned off.
//!
//! A right-click used to show Pimble's menu with the browser's on top of it.
//! rinch-web suppresses the native menu only where a right-click lands inside
//! an element carrying `data-oncontextmenu`, which leaves the tree's padding,
//! the editor, and `ContextMenu`'s own portalled overlay to the browser.
//! `set_suppress_native_context_menu(true)` moves that decision into the
//! delegation itself, for the whole page, and is a flag rather than a widening
//! because a rinch island hydrated into somebody else's page must not take
//! right-click-copy away from the rest of it.
//!
//! Accelerators need nothing here. `rinch_web::mount_into_with_menu_bar` arms
//! every menu item's `shortcut` against the document, so Ctrl+K reaches "Focus
//! Search" through the same declaration the desktop's menu bar uses
//! (`pimble_app::menus`) rather than through a binding this crate keeps in step
//! by hand.

/// Turn the browser's context menu off for this page, once.
pub fn install() {
    rinch_web::set_suppress_native_context_menu(true);
}
