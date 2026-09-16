//! Two things a browser needs that a window does not.
//!
//! **The native context menu.** rinch-web already suppresses it, but only when
//! the right-click lands inside an element carrying `data-oncontextmenu`
//! (`crates/rinch-web/src/event_delegation.rs`, the `contextmenu` listener:
//! `prevent_default()` sits inside the `if let` that looks the handler up). A
//! right-click anywhere else — including on `ContextMenu`'s own portalled
//! overlay while a menu is open — leaves the browser's menu to appear over
//! ours. Pimble wants its own menu everywhere, so this cancels the default for
//! the whole page.
//!
//! The listener is deliberately bubble-phase and never stops propagation:
//! rinch's own listener is also on `document` and also bubble-phase, and
//! starving it would mean no rinch menu at all. Either listener calling
//! `preventDefault` is enough, and the order between them does not matter.
//!
//! **Accelerators.** The search box's placeholder promises Ctrl+K. On the
//! desktop the View menu carries that accelerator; a browser has no menu bar,
//! so the shortcut is bound here. A control that advertises a shortcut and does
//! nothing is worse than one that advertises none.

use wasm_bindgen::prelude::Closure;
use wasm_bindgen::JsCast;

/// Install both, once, for the lifetime of the page.
pub fn install() {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };

    let contextmenu = Closure::wrap(Box::new(|event: web_sys::Event| {
        event.prevent_default();
    }) as Box<dyn FnMut(_)>);
    let _ = document
        .add_event_listener_with_callback("contextmenu", contextmenu.as_ref().unchecked_ref());
    contextmenu.forget();

    let keydown = Closure::wrap(Box::new(|event: web_sys::KeyboardEvent| {
        // Ctrl+K, and Cmd+K where that is what people reach for.
        if (event.ctrl_key() || event.meta_key())
            && !event.alt_key()
            && event.key().eq_ignore_ascii_case("k")
        {
            event.prevent_default();
            pimble_app::app::focus_search();
        }
    }) as Box<dyn FnMut(_)>);
    let _ = document.add_event_listener_with_callback("keydown", keydown.as_ref().unchecked_ref());
    keydown.forget();
}
