//! The navigator (docs/IMAGE_MARKUP_CONTRACT.md section 8): a second, small
//! surface in the board's side panel showing the whole board (its visible
//! extent and wherever the view is), with the view's rectangle over it. A
//! press or a drag on it moves the view there. It is re-rendered a moment
//! after changes stop, never per pointer move.

use super::*;

/// How long after the last change the navigator is drawn again.
const NAV_DEBOUNCE_MS: u32 = 250;

thread_local! {
    static NAV_SURFACES: RefCell<[Option<RenderSurfaceHandle>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
    /// The view each navigator was last drawn with (to map presses back).
    static NAV_VIEWS: RefCell<[Option<pimble_board::View>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
    /// A press on the navigator is held.
    static NAV_DRAG: RefCell<[bool; MAX_PANES]> = const { RefCell::new([false; MAX_PANES]) };
}

/// Pane `pane`'s navigator surface, made once.
pub(super) fn surface(store: AppStore, pane: PaneId) -> RenderSurfaceHandle {
    if let Some(surface) = NAV_SURFACES.with(|s| s.borrow()[pane.index()].clone()) {
        return surface;
    }
    let surface = create_render_surface();
    surface.set_render_callback(move |_writer, _w, _h| render(store, pane));
    surface.set_event_handler(move |event| on_event(store, pane, event));
    NAV_SURFACES.with(|s| s.borrow_mut()[pane.index()] = Some(surface.clone()));
    surface
}

/// The board or the view changed: the view's rectangle follows at once, the
/// picture a moment later.
pub(super) fn changed(store: AppStore, pane: PaneId) {
    place_view_rect(store, pane);
    let Some(generation) = with_session(pane, |s| {
        s.nav_generation += 1;
        s.nav_generation
    }) else {
        return;
    };
    let _ = set_timeout(NAV_DEBOUNCE_MS, move || {
        if with_session(pane, |s| s.nav_generation == generation) == Some(true) {
            render(store, pane);
        }
    });
}

/// What the navigator shows: the visible extent and the view's rectangle.
fn shown_rect(s: &Session) -> Rect {
    let (w, h) = s.view.size();
    let view = s.view.view().board_rect(w, h);
    match s.snapshot().render_rect() {
        Some(extent) => extent.union(&view),
        None => view,
    }
}

fn render(store: AppStore, pane: PaneId) {
    let Some(surface) = NAV_SURFACES.with(|s| s.borrow()[pane.index()].clone()) else { return };
    let (width, height) = surface.layout_size();
    if width == 0 || height == 0 || !untracked(|| store.board_style.panel.get()) {
        return;
    }
    let scale = scale_of(pane);
    let dark = untracked(|| store.dark_mode.get());
    let writer = surface.writer();
    let drawn = with_session(pane, |s| {
        let rect = shown_rect(s);
        let view = pimble_board::View::fit(&rect, width, height, 6.0 * scale);
        let pixmap = pimble_board::render(s.snapshot(), &picture_source(), &view, width, height, Some(background(dark)))?;
        writer.submit_frame(pixmap.data(), width, height);
        Some(view)
    })
    .flatten();
    if let Some(view) = drawn {
        NAV_VIEWS.with(|v| v.borrow_mut()[pane.index()] = Some(view));
        place_view_rect(store, pane);
    }
}

/// The main view's rectangle over the navigator, logical pixels.
fn place_view_rect(store: AppStore, pane: PaneId) {
    let Some(nav) = NAV_VIEWS.with(|v| v.borrow()[pane.index()]) else { return };
    let scale = scale_of(pane);
    let rect = with_session(pane, |s| {
        let (w, h) = s.view.size();
        let r = s.view.view().board_rect(w, h);
        let (x0, y0) = nav.to_pixels(Point::new(r.x, r.y));
        let (x1, y1) = nav.to_pixels(Point::new(r.right(), r.bottom()));
        (x0 / scale, y0 / scale, (x1 - x0) / scale, (y1 - y0) / scale)
    });
    store.pane(pane).board_ui.nav_view.set_if_changed(rect);
}

fn on_event(store: AppStore, pane: PaneId, event: SurfaceEvent) {
    let scale = scale_of(pane);
    let go = |x: f32, y: f32| {
        let Some(nav) = NAV_VIEWS.with(|v| v.borrow()[pane.index()]) else { return };
        let p = nav.to_board(x * scale, y * scale);
        with_session(pane, |s| {
            let zoom = s.view.zoom();
            s.view.set_camera(p, zoom);
        });
        camera_moved(store, pane);
    };
    match event {
        SurfaceEvent::MouseDown { x, y, button: SurfaceMouseButton::Left } => {
            crate::pane_view::focus_pane(store, pane);
            NAV_DRAG.with(|d| d.borrow_mut()[pane.index()] = true);
            go(x, y);
        }
        SurfaceEvent::MouseMove { x, y } => {
            if NAV_DRAG.with(|d| d.borrow()[pane.index()]) {
                go(x, y);
            }
        }
        SurfaceEvent::MouseUp { .. } | SurfaceEvent::MouseLeave => {
            NAV_DRAG.with(|d| d.borrow_mut()[pane.index()] = false);
        }
        _ => {}
    }
}
