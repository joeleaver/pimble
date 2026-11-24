//! The tray (docs/NOTIFICATIONS_CONTRACT.md section 4, "Built in wave 4"):
//! closing the window leaves Pimble running in the system tray, so
//! notifications keep arriving. Desktop only.
//!
//! rinch draws the icon (`rinch::tray`, its `system-tray` feature: a
//! StatusNotifierItem on Linux, the notification area on Windows) and keeps
//! the event loop running while the window is hidden. The icon is up while
//! this process is the primary window process (`crate::instance`) and the
//! setting is on. A window closed then is hidden, not closed; one closed
//! with no icon up quits, as before.

use std::cell::{Cell, RefCell};

use rinch::menu::{Menu, MenuItem};
use rinch::prelude::*;
use rinch::tray::{TrayError, TrayIcon, TrayIconBuilder};

/// The Settings switch's label.
pub const SETTING_LABEL: &str = "Keep running in the tray when the window closes";

/// What this device keeps of the tray, in `state.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraySettings {
    /// Closing the window leaves Pimble in the tray (on by default; Joe,
    /// 2026-11-23).
    pub keep_running: bool,
    /// The one notification saying so has been shown.
    pub notice_shown: bool,
}

impl Default for TraySettings {
    fn default() -> Self {
        TraySettings { keep_running: true, notice_shown: false }
    }
}

const KEEP_RUNNING_KEY: &str = "tray_keep_running";
const NOTICE_SHOWN_KEY: &str = "tray_notice_shown";

impl TraySettings {
    /// Read from the state file's JSON; anything missing is the default.
    pub fn from_state(state: &serde_json::Value) -> TraySettings {
        let default = TraySettings::default();
        TraySettings {
            keep_running: state[KEEP_RUNNING_KEY].as_bool().unwrap_or(default.keep_running),
            notice_shown: state[NOTICE_SHOWN_KEY].as_bool().unwrap_or(default.notice_shown),
        }
    }

    /// Write into the state file's JSON, keeping everything else in it.
    pub fn write_into(self, state: &mut serde_json::Value) {
        if !state.is_object() {
            *state = serde_json::json!({});
        }
        state[KEEP_RUNNING_KEY] = serde_json::json!(self.keep_running);
        state[NOTICE_SHOWN_KEY] = serde_json::json!(self.notice_shown);
    }
}

/// What a close of the window does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnClose {
    /// The app quits.
    Quit,
    /// The window hides and Pimble stays in the tray; `tell` is the first
    /// time, when one notification says so.
    Hide { tell: bool },
}

/// The window is closing: with the setting on and the icon up it hides into
/// the tray, saying so the first time; otherwise the app quits. No icon up
/// (the setting off, a window process that is not the primary, a desktop
/// that shows no tray icons) always quits, so a window never disappears
/// with nothing left to bring it back.
pub fn on_close(settings: TraySettings, icon_up: bool) -> OnClose {
    if settings.keep_running && icon_up {
        OnClose::Hide { tell: !settings.notice_shown }
    } else {
        OnClose::Quit
    }
}

/// Where the tray stands, for the Settings page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayStatus {
    /// The icon is up.
    Up,
    /// The setting is off.
    Off,
    /// Another Pimble window owns the tray.
    OtherWindow,
    /// This desktop shows no tray icons (Linux: no StatusNotifierWatcher).
    NoHost,
    /// The icon could not be made.
    Failed,
}

/// The sentence Settings says under the switch.
pub fn status_line(status: TrayStatus) -> &'static str {
    match status {
        TrayStatus::Up => "Closing the window keeps Pimble in the tray, where notifications still arrive. Quit from the tray icon or File > Quit.",
        TrayStatus::Off => "Closing the window quits Pimble; notifications arrive only while it is open.",
        TrayStatus::OtherWindow => "Another Pimble window owns the tray icon; closing this window closes only this one.",
        TrayStatus::NoHost => "This desktop shows no tray icons, so closing the window quits Pimble.",
        TrayStatus::Failed => "The tray icon could not be made, so closing the window quits Pimble.",
    }
}

thread_local! {
    /// The icon, on the main thread (a `TrayIcon` is `!Send`).
    static TRAY: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
    /// Where it stands, for Settings to follow.
    static STATUS: Cell<Option<Signal<TrayStatus>>> = const { Cell::new(None) };
}

/// The status signal, made once with app lifetime.
pub fn status() -> Signal<TrayStatus> {
    STATUS.with(|s| match s.get() {
        Some(signal) => signal,
        None => {
            let signal = rinch::core::reactive::unowned(|| Signal::new(TrayStatus::Off));
            s.set(Some(signal));
            signal
        }
    })
}

fn icon_up() -> bool {
    TRAY.with(|t| t.borrow().is_some())
}

/// Put the icon up if it should be: at start, when this process becomes the
/// primary, and when the setting is turned on.
pub fn start() {
    if icon_up() {
        return;
    }
    if !crate::instance::is_primary() {
        status().set(TrayStatus::OtherWindow);
        return;
    }
    if !crate::persistence::load_tray_settings().keep_running {
        status().set(TrayStatus::Off);
        return;
    }
    let menu = Menu::new()
        .item(MenuItem::new("Open Pimble").on_click(open_window))
        .separator()
        .item(MenuItem::new("Quit").on_click(quit));
    let built = TrayIconBuilder::new()
        .with_id("pimble")
        .with_title("Pimble")
        .with_tooltip("Pimble")
        .with_icon_png(include_bytes!("../../../assets/brand/png/pimble-64.png"))
        .map(|b| b.with_menu(menu).on_activate(open_window).wait_for_host(false).build());
    match built {
        Ok(Ok(tray)) => {
            TRAY.with(|t| *t.borrow_mut() = Some(tray));
            status().set(TrayStatus::Up);
        }
        Ok(Err(TrayError::NoHost(why))) => {
            tracing::info!("No tray on this desktop ({why}); closing the window quits");
            status().set(TrayStatus::NoHost);
        }
        Ok(Err(e)) | Err(e) => {
            tracing::warn!("The tray icon could not be made ({e}); closing the window quits");
            status().set(TrayStatus::Failed);
        }
    }
}

/// Take the icon down (the setting turned off).
fn stop() {
    // Dropped outside the borrow: dropping it releases its menu callbacks.
    let tray = TRAY.with(|t| t.borrow_mut().take());
    drop(tray);
    status().set(TrayStatus::Off);
}

/// The window process that owned the tray quit and this one is the primary
/// now (`crate::instance::wait_to_become_primary`).
pub fn became_primary() {
    start();
}

/// Settings turned the switch.
pub fn set_keep_running(on: bool) {
    let mut settings = crate::persistence::load_tray_settings();
    settings.keep_running = on;
    crate::persistence::save_tray_settings(settings);
    if on {
        start();
    } else {
        stop();
    }
}

/// "Open Pimble" (and a click on the icon): the window, shown and in front.
fn open_window() {
    crate::desktop::bring_to_front(None);
}

/// "Quit" in the tray and File > Quit: what this device remembers of how
/// things looked is written, and the app exits, whether the window is shown
/// or hidden.
pub fn quit() {
    crate::desktop::remember_views(None);
    rinch::prelude::exit();
}

/// The window is closing (its close button, or the system's: Alt+F4, the
/// taskbar). What it does is `on_close`'s; whether the system should go on
/// closing it is the answer.
pub fn close_requested() -> bool {
    let settings = crate::persistence::load_tray_settings();
    crate::desktop::remember_views(None);
    match on_close(settings, icon_up()) {
        OnClose::Quit => true,
        OnClose::Hide { tell } => {
            hide_current_window();
            if tell {
                crate::persistence::save_tray_settings(TraySettings { notice_shown: true, ..settings });
                tell_once();
            }
            false
        }
    }
}

/// The one notification that says Pimble is still running.
fn tell_once() {
    let shown = rinch::notification::Notification::new("Pimble is still running")
        .body("It stays in the tray so notifications reach you. Quit from the tray icon or File > Quit; Settings > General turns this off.")
        .icon_png(include_bytes!("../../../assets/brand/png/pimble-256.png"))
        .on_click(open_window)
        .on_error(|e| tracing::info!("The tray notice was not shown by the system: {e:?}"))
        .show();
    if let Err(e) = shown {
        tracing::info!("The tray notice was not shown by the system: {e:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_by_default_and_the_notice_not_yet_shown() {
        assert_eq!(TraySettings::default(), TraySettings { keep_running: true, notice_shown: false });
        assert_eq!(TraySettings::from_state(&serde_json::json!({})), TraySettings::default());
        assert_eq!(TraySettings::from_state(&serde_json::json!(null)), TraySettings::default());
    }

    #[test]
    fn the_settings_round_trip_and_keep_the_rest_of_the_state() {
        let mut state = serde_json::json!({ "dark_mode": false, "open_stores": ["/a"] });
        let settings = TraySettings { keep_running: false, notice_shown: true };
        settings.write_into(&mut state);
        assert_eq!(TraySettings::from_state(&state), settings);
        assert_eq!(state["dark_mode"], serde_json::json!(false));
        assert_eq!(state["open_stores"], serde_json::json!(["/a"]));
    }

    #[test]
    fn closing_with_the_icon_up_hides_and_tells_only_the_first_time() {
        let first = TraySettings::default();
        assert_eq!(on_close(first, true), OnClose::Hide { tell: true });
        let told = TraySettings { notice_shown: true, ..first };
        assert_eq!(on_close(told, true), OnClose::Hide { tell: false });
    }

    #[test]
    fn closing_with_the_setting_off_quits() {
        let off = TraySettings { keep_running: false, notice_shown: false };
        assert_eq!(on_close(off, true), OnClose::Quit);
        assert_eq!(on_close(off, false), OnClose::Quit);
    }

    /// No icon (another window owns the tray, the desktop has none, it
    /// failed): the window never hides into nothing.
    #[test]
    fn closing_with_no_icon_up_quits() {
        assert_eq!(on_close(TraySettings::default(), false), OnClose::Quit);
    }

    #[test]
    fn every_status_has_a_sentence() {
        for s in [TrayStatus::Up, TrayStatus::Off, TrayStatus::OtherWindow, TrayStatus::NoHost, TrayStatus::Failed] {
            assert!(status_line(s).ends_with('.'));
        }
    }
}
