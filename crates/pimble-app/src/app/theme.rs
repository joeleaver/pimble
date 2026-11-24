//! The app's theme: rinch's props and Pimble's variables for a scheme, and
//! switching between dark and light.

use rinch::prelude::*;

use crate::persistence::save_dark_mode;
use crate::state::AppStore;

/// rinch's theme for the app: one function so the startup theme and a runtime
/// switch (View > "Toggle Dark Mode", `rinch::update_theme`) agree on everything
/// but the scheme. The editor's own stylesheet follows the same flag through
/// `EditorHandle::set_dark_mode`, and colours the app derives follow
/// `AppStore::dark_mode` reactively.
pub fn theme_props(dark_mode: bool) -> ThemeProviderProps {
    ThemeProviderProps {
        primary_color: Some("blue".into()),
        dark_mode,
        default_radius: Some("sm".into()),
        ..Default::default()
    }
}

/// The theme stylesheet: rinch's own for the scheme, then Pimble's variables for
/// it (`styles::LIGHT_THEME_VARS`, `styles::DARK_THEME_VARS`). They ride in the
/// theme sheet because it is the one sheet rinch replaces when the scheme
/// changes; an app sheet is parsed once.
pub fn theme_css(dark_mode: bool) -> String {
    let mut css = rinch::generate_theme_css_string(&theme_props(dark_mode));
    css.push_str(if dark_mode { crate::styles::DARK_THEME_VARS } else { crate::styles::LIGHT_THEME_VARS });
    css
}

/// Put [`theme_css`] in force, at start and on every switch.
pub fn apply_theme(dark_mode: bool) {
    rinch::core::set_current_theme_css(Some(theme_css(dark_mode)));
}

/// Switch between the dark and light themes: the theme CSS, the editor's
/// stylesheet, every colour the app derives (they read `dark_mode`), and the
/// saved preference.
pub fn toggle_dark_mode(store: AppStore) {
    let dark = !untracked(|| store.dark_mode.get());
    store.dark_mode.set(dark);
    apply_theme(dark);
    crate::editor::set_dark_mode(dark);
    save_dark_mode(dark);
}
