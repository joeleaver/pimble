//! Zooming the whole window (docs/ZOOM_CONTRACT.md): View > Zoom In, Zoom
//! Out and Actual Size, in the steps a browser takes, remembered per device.
//!
//! On the desktop the zoom is rinch's (`rinch::window::set_zoom`): the
//! display's scale times the zoom, so everything is laid out and drawn larger
//! and stays sharp, and every pointer position the app reads is already in
//! the zoomed CSS pixels. In the browser rinch applies none: the browser's own
//! zoom (Ctrl and + or -) is the page's, so the menu items say so instead.

use crate::state::AppStore;

/// The zoom steps, as browsers take them.
pub const STEPS: [f64; 13] = [0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

/// The least and most zoom Pimble keeps: the first and last step.
pub const MIN: f64 = STEPS[0];
pub const MAX: f64 = STEPS[STEPS.len() - 1];

/// The keys, as rinch spells them. Ctrl and + (Shift and =) and the keypad's
/// + and - work too ([`alias`]).
pub const ZOOM_IN_KEY: &str = "Ctrl+=";
pub const ZOOM_OUT_KEY: &str = "Ctrl+-";
pub const ACTUAL_SIZE_KEY: &str = "Ctrl+0";

/// The step after `zoom`: the first step larger than it (a zoom between
/// steps, from an edited state file, goes to the next one up), or the
/// largest.
pub fn step_in(zoom: f64) -> f64 {
    STEPS.iter().copied().find(|s| *s > zoom + 1e-6).unwrap_or(MAX)
}

/// The step before `zoom`: the last step smaller than it, or the smallest.
pub fn step_out(zoom: f64) -> f64 {
    STEPS.iter().rev().copied().find(|s| *s < zoom - 1e-6).unwrap_or(MIN)
}

/// A saved zoom made sane: a number inside the steps' range, else actual
/// size.
pub fn sane(zoom: f64) -> f64 {
    if zoom.is_finite() && zoom > 0.0 { zoom.clamp(MIN, MAX) } else { 1.0 }
}

/// The zoom as the status bar says it: "125%".
pub fn label(zoom: f64) -> String {
    format!("{}%", (zoom * 100.0).round() as i64)
}

/// Whether `zoom` is actual size.
pub fn is_actual(zoom: f64) -> bool {
    (zoom - 1.0).abs() < 1e-6
}

/// What a key the menu chords did not take asks of the zoom: Ctrl and +
/// (on a US layout Shift and =, which the menu's Ctrl+= does not match) and
/// the keypad's + and - and 0. `None` for anything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alias {
    In,
    Out,
    Actual,
}

/// See [`Alias`].
pub fn alias(key: &str, code: &str, ctrl: bool, alt: bool) -> Option<Alias> {
    if !ctrl || alt {
        return None;
    }
    // The menu's chords name physical keys and come first, so a "-" that
    // reaches here is the keypad's (or another layout's); `code` is empty
    // where the caller does not know it (the editor's key hook).
    match (key, code) {
        ("+", _) | (_, "NumpadAdd") => Some(Alias::In),
        ("-", _) | (_, "NumpadSubtract") => Some(Alias::Out),
        (_, "Numpad0") => Some(Alias::Actual),
        _ => None,
    }
}

/// The zoom now.
pub fn current() -> f64 {
    rinch::window::zoom()
}

/// Zoom the window to `zoom` and remember it on this device. In the
/// browser, say where the zoom is instead.
pub fn set(store: AppStore, zoom: f64) {
    if !cfg!(feature = "native") {
        crate::events::show_notice(
            store,
            "Your browser zooms Pimble: Ctrl and + or -, and Ctrl and 0 for actual size.".to_string(),
        );
        return;
    }
    let applied = rinch::window::set_zoom(sane(zoom));
    crate::persistence::save_zoom(applied);
}

pub fn zoom_in(store: AppStore) {
    set(store, step_in(current()));
}

pub fn zoom_out(store: AppStore) {
    set(store, step_out(current()));
}

pub fn actual_size(store: AppStore) {
    set(store, 1.0);
}

/// The zoom this device saved, applied before the first window is made so
/// its first frame is drawn at it (desktop only).
pub fn restore() {
    let saved = sane(crate::persistence::load_zoom());
    if !is_actual(saved) {
        rinch::window::set_zoom(saved);
    }
}

/// Act on a key [`alias`] names. Answers whether it was one.
pub fn on_alias(store: AppStore, key: &str, code: &str, ctrl: bool, alt: bool) -> bool {
    if !cfg!(feature = "native") {
        return false;
    }
    match alias(key, code, ctrl, alt) {
        Some(Alias::In) => zoom_in(store),
        Some(Alias::Out) => zoom_out(store),
        Some(Alias::Actual) => actual_size(store),
        None => return false,
    }
    true
}

/// Listen for the [`alias`] keys everywhere the menu's chords and the
/// editor did not take them (the editor offers them through its own key
/// hook, `editor.rs`).
pub fn listen(store: AppStore) {
    if !cfg!(feature = "native") {
        return;
    }
    rinch::core::events::on_app_key(move |data| {
        data.kind == rinch::core::KeyEventKind::Down && on_alias(store, &data.key, &data.code, data.ctrl || data.meta, data.alt)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_go_up_and_down_as_a_browser_does() {
        assert_eq!(step_in(1.0), 1.1);
        assert_eq!(step_in(1.1), 1.25);
        assert_eq!(step_out(1.0), 0.9);
        assert_eq!(step_out(0.67), 0.5);
        assert_eq!(step_in(3.0), 3.0, "the largest stays");
        assert_eq!(step_out(0.5), 0.5, "the smallest stays");
        assert_eq!(step_in(1.3), 1.5, "between steps, to the next one up");
        assert_eq!(step_out(1.3), 1.25, "and down");
        let mut z = 0.5;
        let mut seen = vec![z];
        while z < MAX {
            z = step_in(z);
            seen.push(z);
        }
        assert_eq!(seen, STEPS, "every step, in order");
    }

    #[test]
    fn a_saved_zoom_is_made_sane() {
        assert_eq!(sane(1.25), 1.25);
        assert_eq!(sane(9.0), 3.0);
        assert_eq!(sane(0.1), 0.5);
        assert_eq!(sane(f64::NAN), 1.0);
        assert_eq!(sane(-1.0), 1.0);
    }

    #[test]
    fn labels_and_aliases() {
        assert_eq!(label(1.25), "125%");
        assert_eq!(label(0.67), "67%");
        assert!(is_actual(1.0) && !is_actual(1.1));
        assert_eq!(alias("+", "Equal", true, false), Some(Alias::In));
        assert_eq!(alias("+", "NumpadAdd", true, false), Some(Alias::In));
        assert_eq!(alias("-", "NumpadSubtract", true, false), Some(Alias::Out));
        assert_eq!(alias("-", "", true, false), Some(Alias::Out), "the editor knows no code");
        assert_eq!(alias("0", "Numpad0", true, false), Some(Alias::Actual));
        assert_eq!(alias("+", "Equal", false, false), None, "a plus typed is a plus");
        assert_eq!(alias("+", "Equal", true, true), None, "AltGr text is text");
        assert_eq!(alias("=", "Equal", true, false), None, "the menu's own chord");
    }
}
