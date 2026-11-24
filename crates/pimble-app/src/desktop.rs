//! The desktop window's life: opening it, bringing it to the front, and
//! closing it, into the tray (`crate::tray`) or out of the app. Desktop
//! only; what the window shows is `crate::app::build_view`'s, shared with
//! the browser.

use rinch::prelude::*;

use crate::events::EVENT_PROCESSOR;
use crate::state::AppStore;

/// A link from outside reached this window, or the tray or a notification
/// asked for it: show it and bring it to the front. On Wayland a window may
/// only take the focus with the activation token the click that started
/// `pimble <link>` (or a second plain `pimble`) gave it (rinch #2880), which
/// that program handed over; without one (X11, Windows, the tray, a launch
/// with none) the window asks for the focus itself.
pub(crate) fn bring_to_front(activation_token: Option<&str>) {
    show_current_window();
    match activation_token {
        Some(token) => activate_current_window_with_token(token),
        None => focus_current_window(),
    }
}

/// Open the desktop window: the same UI [`crate::app::build_view`] builds,
/// wrapped in a native menu bar, the borderless window and the saved theme,
/// with the tray icon beside it when this process owns the tray.
/// `renderer` is the GPU with a fall back to software unless the command
/// line chose (`pimble --cpu`); `RINCH_RENDERER` overrides either.
/// `activation_token` is the one this program was started with, if any,
/// which the first window takes the focus with (rinch #2880).
pub fn run(renderer: Renderer, activation_token: Option<String>) {
    use std::sync::Arc;

    // The zoom this device saved, before the window is made, so the first
    // frame is drawn at it; and the zoom keys the menu's chords do not cover
    // (docs/ZOOM_CONTRACT.md).
    crate::zoom::restore();
    let (store, build_component) = crate::app::build_view();
    // Reachable from the start for what runs outside the view (the tray's
    // takeover, Quit), not only once a note is opened in an editor.
    crate::links::set_app_store(store);
    crate::zoom::listen(store);
    crate::update::start(store);
    crate::diagnostics_ui::notice_once(store);

    // One spec, two shells: `crates/pimble-app/src/menus.rs` says what the
    // menus hold and which target each item belongs on, so the browser's menu
    // bar cannot drift from this one.
    let menus = crate::menus::build_menus(store);

    let theme = crate::app::theme_props(untracked(|| store.dark_mode.get()));

    // The tray icon, when this process owns the tray and the setting is on
    // (docs/NOTIFICATIONS_CONTRACT.md "Built in wave 4"), put up once the
    // window is there: making it asks the desktop's tray host, which a hung
    // one makes wait up to five seconds (rinch's bound), and the first window
    // must not wait on that. A window process that does not own the tray
    // starts waiting to take it over only then too (`crate::tray::window_up`):
    // a takeover needs the window and the app store, so one that came during
    // startup would otherwise lose the notices held for it. Every window
    // follows the setting as stored, which another window's Settings can
    // change.
    let app_component = move |__scope: &mut RenderScope| -> NodeHandle {
        // Armed as the window's view is first built, outside its ownership,
        // so it runs on the next turn of the event loop, the window made.
        rinch::core::reactive::unowned(|| {
            let _ = set_timeout(0, crate::tray::window_up);
        });
        build_component(__scope)
    };
    crate::tray::follow_setting();

    // A close by the system (Alt+F4, the taskbar); the window's own close
    // button goes through `close_window`. Both hide into the tray or let the
    // app quit (`crate::tray::close_requested`), writing first what this
    // device remembers of how things looked. It runs on the main thread, and
    // reads only thread-locals there, so it captures nothing.
    let on_close: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(crate::tray::close_requested);

    // The size and place it closed at on this device.
    let saved_window = crate::persistence::load_window();
    let (width, height, place) = saved_window.as_ref().and_then(|w| w.sane()).unwrap_or((1200, 800, None));
    let props = WindowProps {
        title: "Pimble".into(),
        width,
        height,
        x: place.map(|p| p.0),
        y: place.map(|p| p.1),
        maximized: saved_window.as_ref().is_some_and(|w| w.maximized),
        // The monitor the place is on, only with a place to put there.
        monitor: saved_window.and_then(|w| w.monitor).filter(|_| place.is_some()),
        borderless: true,
        transparent: true,
        resizable: true,
        menu_in_titlebar: true,
        icon: Some(include_bytes!("../../../assets/brand/png/pimble-256.png")),
        on_close_requested: Some(on_close),
        ..Default::default()
    };

    App::new(app_component)
        .window_props(props)
        .theme(theme)
        .menu(menus)
        .renderer(renderer)
        .activation_token(activation_token)
        .run();

    EVENT_PROCESSOR.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

/// What this device remembers of how things looked (`views.rs`), written now
/// with every pane's place and board camera as they are: the window is
/// closing, hiding into the tray, or the app is quitting.
pub(crate) fn remember_views(store: Option<AppStore>) {
    if let Some(store) = store.or_else(|| crate::links::with_app_store(|store| store)) {
        crate::places::remember_all(store);
    }
    crate::boards::remember_all();
    crate::views::save_now();
    // The window's own size and place (as it is when not maximized, where
    // the platform says), and whether it is maximized.
    if let Some(window) = rinch::prelude::current_window_restore() {
        crate::persistence::save_window(&crate::persistence::WindowGeometry {
            width: window.size.0,
            height: window.size.1,
            x: window.position.map(|p| p.0),
            y: window.position.map(|p| p.1),
            maximized: window.maximized,
            monitor: window.monitor,
        });
    }
}

/// The window's own close button: into the tray, or the app quits, as a
/// close by the system does. Rinch's own close control exits without asking
/// `on_close_requested`, so the question is asked here.
pub(crate) fn close_window() {
    if crate::tray::close_requested() {
        close_current_window();
    }
}
