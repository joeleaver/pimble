//! The pointer and the keyboard on a board (docs/IMAGE_MARKUP_CONTRACT.md
//! section 4): every tool, the selection (hit testing on the shapes' own
//! geometry, a marquee, shift to add), moving, resizing and rotating,
//! typing text into a real input over the surface, and arranging
//! (duplicate, order, group, lock, align and distribute).
//!
//! Everything a gesture writes is one `Board::edit` through
//! [`super::local_edit`], written about every 80 ms while the pointer moves
//! (so others watch it happen) and once more on release.

use pimble_board::{End, Side};

use super::*;

/// Two presses closer than this are a double-click.
const DOUBLE_CLICK_MS: f64 = 450.0;
/// How near (logical pixels) a press must be to a handle to take it.
const HANDLE_REACH: f32 = 7.0;
/// How far above the selection box the rotate handle sits, logical pixels.
pub(super) const ROTATE_OFFSET: f32 = 22.0;
/// A drag shorter than this (logical pixels) is a click.
const CLICK_SLOP: f32 = 3.0;
/// How near (logical pixels) the pointer must be to an element to hit it.
const HIT_SLOP: f32 = 5.0;
/// How far an arrow key moves the selection, board units (shift: ten times).
const NUDGE: f32 = 1.0;
/// Where a duplicate lands, board units from the original.
const DUPLICATE_OFFSET: f32 = 16.0;
/// A new sticky note's size, and a shape's when it is clicked rather than
/// dragged out, board units.
const STICKY_SIZE: f32 = 200.0;
const CLICKED_SHAPE: (f32, f32) = (160.0, 100.0);
/// A node card's size, board units.
const CARD_SIZE: (f32, f32) = (240.0, 56.0);

// ── Events ──────────────────────────────────────────────────────────────────

/// Which keys a focused board keeps from the rest of the app.
pub(super) fn claims_key(pane: PaneId, key: &SurfaceKeyData) -> bool {
    // While text is being typed every key is the input's (in the browser the
    // surface still hears the keys of the page it was last pressed in).
    if with_session(pane, |s| s.text.is_some()).unwrap_or(false) {
        return false;
    }
    let primary = key.ctrl || key.meta;
    let active = with_session(pane, |s| !s.selected.is_empty() || !matches!(s.gesture, Gesture::Idle)).unwrap_or(false);
    let selected = with_session(pane, |s| !s.selected.is_empty()).unwrap_or(false);
    match key.key.as_str() {
        " " | "Shift" => true,
        "Delete" | "Backspace" => selected,
        "Escape" => active,
        "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight" => selected,
        _ if primary => matches!(
            key.key.to_ascii_lowercase().as_str(),
            "z" | "y" | "d" | "g" | "a" | "c" | "]" | "[" | "l"
        ),
        _ if key.alt => false,
        k => Tool::from_key(k).is_some(),
    }
}

fn wheel_zooms_out(delta_y: f32) -> bool {
    // A wheel turned toward the person: negative on the desktop (winit's
    // sign), positive in the browser (`WheelEvent.deltaY`).
    if cfg!(feature = "native") {
        delta_y < 0.0
    } else {
        delta_y > 0.0
    }
}

pub(super) fn on_event(store: AppStore, pane: PaneId, event: SurfaceEvent) {
    if with_session(pane, |_| ()).is_none() {
        return;
    }
    let scale = scale_of(pane);
    let ui = store.pane(pane).board_ui;
    match event {
        SurfaceEvent::MouseDown { x, y, button } => {
            let (px, py) = (x * scale, y * scale);
            // A press on the board focuses its pane, as a press in an editor does.
            crate::pane_view::focus_pane(store, pane);
            crate::editor::give_keyboard_to(store, pane);
            // A press anywhere else ends the text being typed.
            end_text(store, pane);
            ui.tip.set(None);
            let now = now_ms();
            let (space, double) = with_session(pane, |s| {
                let (t, lx, ly) = s.last_press;
                let double = now - t < DOUBLE_CLICK_MS && (lx - px).hypot(ly - py) < 6.0 * scale;
                s.last_press = if double { (0.0, 0.0, 0.0) } else { (now, px, py) };
                s.pointer = (px, py);
                (s.space, double)
            })
            .unwrap_or((false, false));
            let tool = untracked(|| store.board_tool.get());
            match button {
                SurfaceMouseButton::Middle => begin_pan(pane, px, py),
                SurfaceMouseButton::Left if space || tool == Tool::Hand => begin_pan(pane, px, py),
                SurfaceMouseButton::Left => {
                    if double && double_click(store, pane, px, py) {
                        return;
                    }
                    press(store, pane, tool, px, py);
                }
                SurfaceMouseButton::Right => {}
            }
        }
        SurfaceEvent::MouseMove { x, y } => {
            ui.hover.set_if_changed(true);
            pointer_moved(store, pane, x * scale, y * scale);
        }
        SurfaceEvent::MouseUp { x, y, .. } => pointer_released(store, pane, x * scale, y * scale),
        SurfaceEvent::MouseWheel { x, y, delta_y, .. } => {
            if delta_y == 0.0 {
                return;
            }
            let factor = if wheel_zooms_out(delta_y) { 1.0 / 1.1 } else { 1.1 };
            let generation = with_session(pane, |s| {
                s.view.zoom_at(x * scale, y * scale, factor);
                s.settle_generation += 1;
                s.settle_generation
            });
            camera_moved(store, pane);
            if let Some(generation) = generation {
                let _ = set_timeout(SETTLE_MS, move || {
                    let settled = with_session(pane, |s| s.settle_generation == generation && s.view.settle(&picture_source()));
                    if settled == Some(true) {
                        present(pane);
                    }
                });
            }
        }
        SurfaceEvent::KeyDown(key) => key_down(store, pane, &key),
        SurfaceEvent::KeyUp(key) => {
            with_session(pane, |s| match key.key.as_str() {
                " " => s.space = false,
                "Shift" => s.shift = false,
                _ => {}
            });
        }
        SurfaceEvent::MouseEnter { .. } => ui.hover.set_if_changed(true),
        SurfaceEvent::MouseLeave => {
            with_session(pane, |s| s.space = false);
            ui.tip.set_if_changed(None);
            // A moment's grace, so the pointer can reach the markup control.
            let _ = set_timeout(600, move || ui.hover.set_if_changed(false));
        }
        SurfaceEvent::FocusLost => {
            with_session(pane, |s| {
                s.space = false;
                s.shift = false;
            });
        }
        SurfaceEvent::DragOver { x, y } | SurfaceEvent::DragEnter { x, y } => {
            with_session(pane, |s| s.pointer = (x * scale, y * scale));
        }
        SurfaceEvent::Drop { x, y } => {
            with_session(pane, |s| s.pointer = (x * scale, y * scale));
            drop_tree_row(store, pane);
        }
        _ => {}
    }
}

// ── Pressing ────────────────────────────────────────────────────────────────

fn begin_pan(pane: PaneId, px: f32, py: f32) {
    with_session(pane, |s| s.gesture = Gesture::Pan { last: (px, py) });
}

fn press(store: AppStore, pane: PaneId, tool: Tool, px: f32, py: f32) {
    let Some(p) = with_session(pane, |s| s.view.screen_to_board(px, py)) else { return };
    match tool {
        Tool::Select | Tool::Hand => begin_select(store, pane, px, py),
        Tool::Pen | Tool::Highlighter => begin_stroke(store, pane, tool.kind().unwrap_or(Kind::Pen), p),
        Tool::Line | Tool::Arrow | Tool::Rect | Tool::Ellipse | Tool::Diamond | Tool::Frame => {
            if !writable_or_say(store, pane) || target_layer(store, pane).is_none() {
                return;
            }
            let start = snap(store, p);
            new_undo_step(pane);
            with_session(pane, |s| {
                s.selected.clear();
                s.gesture = Gesture::Shape { kind: tool.kind().unwrap_or(Kind::Rect), start, last: start, id: None, written_at: 0.0 };
            });
            after_change(store, pane);
        }
        Tool::Connector => {
            if !writable_or_say(store, pane) || target_layer(store, pane).is_none() {
                return;
            }
            let from = with_session(pane, |s| end_at(s, p, None)).unwrap_or(End::Point(p));
            new_undo_step(pane);
            with_session(pane, |s| {
                s.selected.clear();
                s.gesture = Gesture::Connector { from, last: p, id: None, written_at: 0.0 };
            });
            after_change(store, pane);
        }
        Tool::Text => {
            // On a text that is there: edit it. Elsewhere: a new one.
            let existing = with_session(pane, |s| hit_at(s, p).filter(|e| e.raw.kind == Kind::Text).map(|e| e.raw.id.clone())).flatten();
            match existing {
                Some(id) => begin_text_existing(store, pane, &id),
                None => begin_text_new(store, pane, snap(store, p)),
            }
        }
        Tool::Step => {
            let Some(layer) = target_layer(store, pane) else { return };
            new_undo_step(pane);
            let style = new_style(store, &Kind::Step);
            let at = snap(store, p);
            if let Some(id) = local_edit(store, pane, |t| t.add_element(with_layer(NewElement::new(Kind::Step, Geom::Anchor(at)).with_style(style), &layer))) {
                select(store, pane, vec![id]);
            }
        }
        Tool::Sticky => {
            let Some(layer) = target_layer(store, pane) else { return };
            new_undo_step(pane);
            let style = new_style(store, &Kind::Sticky);
            let at = snap(store, Point::new(p.x - STICKY_SIZE / 2.0, p.y - STICKY_SIZE / 2.0));
            let rect = Rect::new(at.x, at.y, STICKY_SIZE, STICKY_SIZE);
            let element = with_layer(NewElement::new(Kind::Sticky, Geom::Rect(rect)).with_text("").with_style(style), &layer);
            if let Some(id) = local_edit(store, pane, |t| t.add_element(element)) {
                select(store, pane, vec![id.clone()]);
                begin_text_existing(store, pane, &id);
            }
        }
        Tool::Eraser => {
            if !writable_or_say(store, pane) {
                return;
            }
            new_undo_step(pane);
            with_session(pane, |s| {
                s.selected.clear();
                s.gesture = Gesture::Erase;
            });
            erase_at(store, pane, p);
        }
    }
}

/// The element a press at `p` hits, on its own geometry (contract section
/// 4: never just its bounds), skipping elements in locked layers (a locked
/// picture layer is the screenshot under the marks).
fn hit_at<'a>(s: &'a Session, p: Point) -> Option<&'a pimble_board::Element> {
    let snapshot = s.snapshot();
    let tolerance = HIT_SLOP * scale_hint(s) / s.view.zoom().max(1e-3);
    // The topmost hit that is not in a locked layer: look again below a
    // locked one.
    let mut hits: Vec<&pimble_board::Element> = snapshot
        .query(&Rect::new(p.x, p.y, 0.0, 0.0).inflate(tolerance))
        .into_iter()
        .map(|i| &snapshot.elements[i])
        .filter(|e| e.hits(p, tolerance))
        .collect();
    hits.reverse();
    hits.into_iter().find(|e| !snapshot.layer(&e.layer).is_some_and(|l| l.locked))
}

/// The physical pixels per logical pixel the session last drew at (hit
/// slop is in logical pixels).
fn scale_hint(s: &Session) -> f32 {
    let _ = s;
    1.0
}

/// What clicking `e` selects: the outermost group it is in (a group is
/// picked as one), never a frame.
fn select_target(snapshot: &BoardSnapshot, e: &pimble_board::Element) -> String {
    let mut id = e.raw.id.clone();
    let mut at = e.parent.clone();
    let mut steps = 0;
    while let Some(parent) = at {
        let Some(p) = snapshot.get(&parent) else { break };
        if p.raw.kind != Kind::Group || steps > 64 {
            break;
        }
        id = parent;
        at = p.parent.clone();
        steps += 1;
    }
    id
}

/// Which handle of the selection box is at physical pixel (`px`, `py`).
fn handle_at(s: &Session, px: f32, py: f32, scale: f32) -> Option<Handle> {
    if !s.selected.iter().any(|id| s.editable(id)) {
        return None;
    }
    let b = s.selection_bounds()?;
    let (x, y, w, h) = s.to_screen(&b);
    let reach = HANDLE_REACH * scale;
    let near = |hx: f32, hy: f32| (px - hx).abs() <= reach && (py - hy).abs() <= reach;
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    if near(cx, y - ROTATE_OFFSET * scale) {
        return Some(Handle::Rotate);
    }
    let handles = [
        (Handle::NW, x, y),
        (Handle::NE, x + w, y),
        (Handle::SE, x + w, y + h),
        (Handle::SW, x, y + h),
        (Handle::N, cx, y),
        (Handle::E, x + w, cy),
        (Handle::S, cx, y + h),
        (Handle::W, x, cy),
    ];
    handles.into_iter().find(|(_, hx, hy)| near(*hx, *hy)).map(|(h, ..)| h)
}

fn begin_select(store: AppStore, pane: PaneId, px: f32, py: f32) {
    let scale = scale_of(pane);
    let writable = with_session(pane, |s| store.node_access(s.store_id, s.node_id).allows_write()).unwrap_or(false);
    with_session(pane, |s| {
        let p = s.view.screen_to_board(px, py);
        if writable {
            if let Some(handle) = handle_at(s, px, py, scale) {
                let bounds = s.selection_bounds().unwrap_or_default();
                let bases = bases_of(s, &s.selected.clone());
                s.gesture = match handle {
                    Handle::Rotate => {
                        let c = bounds.center();
                        let a = (p.y - c.y).atan2(p.x - c.x);
                        Gesture::Rotate { center: c, start: a, last: a, bases, written_at: 0.0 }
                    }
                    _ => Gesture::Resize { handle, bounds, last: p, bases, written_at: 0.0 },
                };
                if let Some(undo) = s.undo.as_mut() {
                    undo.reset();
                }
                return;
            }
        }
        let hit = hit_at(s, p).map(|e| select_target(s.snapshot(), e));
        match hit {
            Some(id) => {
                if s.shift {
                    // Shift adds to the selection, or takes it back out.
                    if let Some(i) = s.selected.iter().position(|x| *x == id) {
                        s.selected.remove(i);
                        s.gesture = Gesture::Idle;
                        return;
                    }
                    s.selected.push(id.clone());
                } else if !s.selected.contains(&id) {
                    s.selected = vec![id.clone()];
                }
                let bases = bases_of(s, &s.selected.clone());
                s.gesture = Gesture::Move { start: p, last: p, bases, written_at: 0.0, clicked: Some(id) };
                if let Some(undo) = s.undo.as_mut() {
                    undo.reset();
                }
            }
            None => {
                let base = if s.shift { s.selected.clone() } else { Vec::new() };
                s.selected = base.clone();
                s.gesture = Gesture::Marquee { start: (px, py), now: (px, py), base };
            }
        }
    });
    load_selection_style(store, pane);
    after_change(store, pane);
}

fn begin_stroke(store: AppStore, pane: PaneId, kind: Kind, p: Point) {
    if !writable_or_say(store, pane) {
        return;
    }
    let Some(layer) = target_layer(store, pane) else { return };
    let style = new_style(store, &kind);
    let point = [p.x, p.y, 0.5];
    with_session(pane, |s| s.view.begin_stroke(kind.clone(), &style, point));
    // A new stroke is a new undo step, however soon after the last.
    new_undo_step(pane);
    let element = with_layer(NewElement::stroke(kind, vec![point]).with_style(style), &layer);
    let id = local_edit(store, pane, |t| t.add_element(element));
    with_session(pane, |s| match id {
        Some(id) => {
            s.selected.clear();
            s.gesture = Gesture::Stroke { id, pending: Vec::new(), appended_at: now_ms() };
        }
        None => s.view.cancel_stroke(),
    });
    after_change(store, pane);
}

/// A double-click: a node card opens its node, an element with text is typed
/// into. Answers whether it did something.
fn double_click(store: AppStore, pane: PaneId, px: f32, py: f32) -> bool {
    let target = with_session(pane, |s| {
        let p = s.view.screen_to_board(px, py);
        hit_at(s, p).map(|e| (e.raw.id.clone(), e.raw.kind.clone(), e.raw.geom.clone()))
    })
    .flatten();
    let Some((id, kind, geom)) = target else { return false };
    match (kind, geom) {
        (Kind::Card, Geom::Card { link, .. }) => {
            // Opens in the focused pane, as a followed link does.
            crate::links::follow_href(store, &link);
            true
        }
        (Kind::Text | Kind::Sticky | Kind::Rect | Kind::Ellipse | Kind::Diamond | Kind::Frame | Kind::Connector | Kind::Callout, _) => {
            if with_session(pane, |s| s.editable(&id)) != Some(true) {
                crate::events::show_notice(store, LOCKED.to_string());
                return true;
            }
            select(store, pane, vec![id.clone()]);
            begin_text_existing(store, pane, &id);
            true
        }
        _ => false,
    }
}

// ── Moving the pointer ──────────────────────────────────────────────────────

/// What a pointer move asks of the document, worked out under the session's
/// borrow and written after it.
enum Write {
    Nothing,
    Redraw,
    Append(String, Vec<[f32; 3]>),
    /// Element writes: geometry and transform per element.
    Elements(Vec<(String, Option<Geom>, Option<Transform>)>),
    MakeShape(Kind, Point, Point),
    ShapeGeom(String, Geom),
    MakeConnector(End, Point),
    Erase(Point),
}

fn pointer_moved(store: AppStore, pane: PaneId, px: f32, py: f32) {
    let writable = with_session(pane, |s| store.node_access(s.store_id, s.node_id).allows_write()).unwrap_or(false);
    let snap_on = untracked(|| store.board_style.snap.get());
    let scale = scale_of(pane);
    let next = with_session(pane, |s| {
        s.pointer = (px, py);
        let p = s.view.screen_to_board(px, py);
        let shift = s.shift;
        let zoom = s.view.zoom().max(1e-3);
        let due = |written_at: &mut f64| {
            if now_ms() - *written_at < DRAG_WRITE_EVERY_MS {
                return false;
            }
            *written_at = now_ms();
            true
        };
        match &mut s.gesture {
            Gesture::Idle => Write::Nothing,
            Gesture::Pan { last } => {
                let (dx, dy) = (px - last.0, py - last.1);
                *last = (px, py);
                s.view.pan_by(dx, dy);
                Write::Redraw
            }
            Gesture::Stroke { id, pending, appended_at } => {
                let point = [p.x, p.y, 0.5];
                pending.push(point);
                let id = id.clone();
                let batch = if now_ms() - *appended_at >= APPEND_EVERY_MS {
                    *appended_at = now_ms();
                    Some(std::mem::take(pending))
                } else {
                    None
                };
                s.view.extend_stroke(&[point]);
                match batch {
                    Some(points) => Write::Append(id, points),
                    None => Write::Redraw,
                }
            }
            Gesture::Shape { kind, start, last, id, written_at } => {
                let p = constrain(kind, *start, snap_point(snap_on, p), shift);
                *last = p;
                let far = (p.x - start.x).hypot(p.y - start.y) * zoom > CLICK_SLOP * scale;
                match id {
                    None if far && writable => Write::MakeShape(kind.clone(), *start, p),
                    Some(id) if due(written_at) => Write::ShapeGeom(id.clone(), shape_geom(kind, *start, p)),
                    _ => Write::Redraw,
                }
            }
            Gesture::Connector { from, last, id, written_at } => {
                let p = constrain(&Kind::Connector, from.last_point(), p, shift);
                *last = p;
                let far = (p.x - from.last_point().x).hypot(p.y - from.last_point().y) * zoom > CLICK_SLOP * scale;
                match id {
                    None if far && writable => Write::MakeConnector(from.clone(), p),
                    Some(id) if due(written_at) => Write::ShapeGeom(id.clone(), Geom::Connector { from: from.clone(), to: End::Point(p) }),
                    _ => Write::Redraw,
                }
            }
            Gesture::Move { start, last, bases, written_at, .. } => {
                if !writable {
                    return Write::Nothing;
                }
                *last = p;
                if !due(written_at) {
                    return Write::Nothing;
                }
                let (dx, dy) = move_delta(bases, *start, p, snap_on, shift);
                Write::Elements(bases.iter().filter_map(|b| translated(b, dx, dy)).collect())
            }
            Gesture::Resize { handle, bounds, last, bases, written_at } => {
                *last = p;
                if !writable || !due(written_at) {
                    return Write::Nothing;
                }
                let to = resized_bounds(*handle, bounds, snap_point(snap_on, p), shift);
                Write::Elements(bases.iter().filter_map(|b| resized(b, bounds, &to)).collect())
            }
            Gesture::Rotate { center, start, last, bases, written_at } => {
                let a = (p.y - center.y).atan2(p.x - center.x);
                *last = a;
                if !writable || !due(written_at) {
                    return Write::Nothing;
                }
                let degrees = rotation_degrees(*start, a, shift);
                Write::Elements(bases.iter().filter_map(|b| rotated(b, *center, degrees)).collect())
            }
            Gesture::Marquee { start, now, base } => {
                *now = (px, py);
                let a = s.view.screen_to_board(start.0.min(px), start.1.min(py));
                let b = s.view.screen_to_board(start.0.max(px), start.1.max(py));
                let rect = Rect::new(a.x, a.y, b.x - a.x, b.y - a.y);
                let snapshot = s.view.snapshot();
                let mut chosen = base.clone();
                for e in snapshot.within(&rect) {
                    if snapshot.layer(&e.layer).is_some_and(|l| l.locked) {
                        continue;
                    }
                    let id = select_target(snapshot, e);
                    if !chosen.contains(&id) {
                        chosen.push(id);
                    }
                }
                s.selected = chosen;
                Write::Redraw
            }
            Gesture::Erase => Write::Erase(p),
        }
    });
    let Some(next) = next else { return };
    match next {
        Write::Nothing => {
            hover_attribution(store, pane, px, py);
        }
        Write::Redraw => after_change(store, pane),
        Write::Append(id, points) => {
            local_edit(store, pane, |t| t.append_points(&id, &points));
        }
        Write::Elements(writes) => write_elements(store, pane, &writes),
        Write::MakeShape(kind, start, p) => {
            let Some(layer) = target_layer(store, pane) else { return };
            let style = new_style(store, &kind);
            let mut element = with_layer(NewElement::new(kind.clone(), shape_geom(&kind, start, p)).with_style(style), &layer);
            if kind == Kind::Frame {
                // A frame sits below what it holds.
                element.placement = Some(Placement::Bottom);
                let n = with_session(pane, |s| s.snapshot().frames().len()).unwrap_or(0) + 1;
                element.text = Some(format!("Frame {n}"));
            }
            let id = local_edit(store, pane, |t| t.add_element(element));
            with_session(pane, |s| {
                if let Gesture::Shape { id: slot, written_at, .. } = &mut s.gesture {
                    *slot = id;
                    *written_at = now_ms();
                }
            });
        }
        Write::ShapeGeom(id, geom) => {
            local_edit(store, pane, |t| t.set_geom(&id, &geom));
        }
        Write::MakeConnector(from, p) => {
            let Some(layer) = target_layer(store, pane) else { return };
            let style = new_style(store, &Kind::Connector);
            let element = with_layer(NewElement::new(Kind::Connector, Geom::Connector { from, to: End::Point(p) }).with_style(style), &layer);
            let id = local_edit(store, pane, |t| t.add_element(element));
            with_session(pane, |s| {
                if let Gesture::Connector { id: slot, written_at, .. } = &mut s.gesture {
                    *slot = id;
                    *written_at = now_ms();
                }
            });
        }
        Write::Erase(p) => erase_at(store, pane, p),
    }
}

fn pointer_released(store: AppStore, pane: PaneId, px: f32, py: f32) {
    let snap_on = untracked(|| store.board_style.snap.get());
    let gesture = with_session(pane, |s| std::mem::replace(&mut s.gesture, Gesture::Idle));
    let Some(gesture) = gesture else { return };
    let shift = with_session(pane, |s| s.shift).unwrap_or(false);
    let p = with_session(pane, |s| s.view.screen_to_board(px, py)).unwrap_or_default();
    match gesture {
        Gesture::Stroke { id, mut pending, .. } => {
            let (points, min_gap) = with_session(pane, |s| {
                pending.push([p.x, p.y, 0.5]);
                s.view.extend_stroke(&pending[pending.len() - 1..]);
                let mut points = s.view.end_stroke();
                points.push([p.x, p.y, 0.5]);
                (points, 0.75 / s.view.zoom().max(1e-3))
            })
            .unwrap_or_default();
            // Smoothed once, on release, and never edited after.
            let smoothed = smooth_stroke(&points, min_gap);
            local_edit(store, pane, |t| t.replace_points(&id, &smoothed));
        }
        Gesture::Shape { kind, start, last, id, .. } => match id {
            Some(id) => {
                let geom = shape_geom(&kind, start, last);
                local_edit(store, pane, |t| t.set_geom(&id, &geom));
                if kind == Kind::Frame {
                    adopt_into_frame(store, pane, &id);
                }
                select(store, pane, vec![id]);
            }
            None if matches!(kind, Kind::Rect | Kind::Ellipse | Kind::Diamond) => {
                // A click: a shape of the usual size, centered there.
                let Some(layer) = target_layer(store, pane) else { return };
                let (w, h) = CLICKED_SHAPE;
                let rect = Rect::new(start.x - w / 2.0, start.y - h / 2.0, w, h);
                let element = with_layer(NewElement::new(kind.clone(), Geom::Rect(rect)).with_style(new_style(store, &kind)), &layer);
                if let Some(id) = local_edit(store, pane, |t| t.add_element(element)) {
                    select(store, pane, vec![id]);
                }
            }
            None => after_change(store, pane),
        },
        Gesture::Connector { from, last, id, .. } => {
            if let Some(id) = id {
                let to = with_session(pane, |s| end_at(s, last, Some(&id))).unwrap_or(End::Point(last));
                // A connector joining an element to itself is a point.
                let to = match (&from, &to) {
                    (End::Element { id: a, .. }, End::Element { id: b, .. }) if a == b => End::Point(last),
                    _ => to,
                };
                local_edit(store, pane, |t| t.set_geom(&id, &Geom::Connector { from, to }));
                select(store, pane, vec![id]);
            } else {
                after_change(store, pane);
            }
        }
        Gesture::Move { start, last, bases, clicked, .. } => {
            let moved = (last.x - start.x).abs() > f32::EPSILON || (last.y - start.y).abs() > f32::EPSILON;
            if moved {
                let (dx, dy) = move_delta(&bases, start, last, snap_on, shift);
                let writes: Vec<_> = bases.iter().filter_map(|b| translated(b, dx, dy)).collect();
                write_elements(store, pane, &writes);
                let ids: Vec<String> = bases.iter().map(|b| b.id.clone()).collect();
                frame_membership(store, pane, &ids);
            } else if let Some(id) = clicked {
                // A click on one of several selected: just that one.
                if !shift {
                    with_session(pane, |s| s.selected = vec![id]);
                }
            }
            load_selection_style(store, pane);
            after_change(store, pane);
        }
        Gesture::Resize { handle, bounds, last, bases, .. } => {
            let to = resized_bounds(handle, &bounds, snap_point(snap_on, last), shift);
            let writes: Vec<_> = bases.iter().filter_map(|b| resized(b, &bounds, &to)).collect();
            write_elements(store, pane, &writes);
            after_change(store, pane);
        }
        Gesture::Rotate { center, start, last, bases, .. } => {
            let degrees = rotation_degrees(start, last, shift);
            let writes: Vec<_> = bases.iter().filter_map(|b| rotated(b, center, degrees)).collect();
            write_elements(store, pane, &writes);
            after_change(store, pane);
        }
        Gesture::Marquee { .. } => {
            load_selection_style(store, pane);
            after_change(store, pane);
        }
        _ => after_change(store, pane),
    }
}

/// Write each element's new geometry and transform in one edit.
fn write_elements(store: AppStore, pane: PaneId, writes: &[(String, Option<Geom>, Option<Transform>)]) {
    if writes.is_empty() {
        return;
    }
    local_edit(store, pane, |t| {
        for (id, geom, transform) in writes {
            if let Some(geom) = geom {
                t.set_geom(id, geom)?;
            }
            if let Some(transform) = transform {
                t.set_transform(id, transform)?;
            }
        }
        Ok(())
    });
}

// ── Geometry ────────────────────────────────────────────────────────────────

/// The elements a move, resize or rotate acts on, as they are now: the
/// editable ones, leaving out any whose frame or group is also selected (it
/// moves with it) and connectors whose both ends are elements (they follow).
fn bases_of(s: &Session, ids: &[String]) -> Vec<Base> {
    let snapshot = s.snapshot();
    let selected: HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
    let ancestor_selected = |e: &pimble_board::Element| {
        let mut at = e.parent.clone();
        let mut steps = 0;
        while let Some(p) = at {
            if selected.contains(p.as_str()) {
                return true;
            }
            at = snapshot.get(&p).and_then(|x| x.parent.clone());
            steps += 1;
            if steps > 64 {
                break;
            }
        }
        false
    };
    ids.iter()
        .filter(|id| s.editable(id))
        .filter_map(|id| snapshot.get(id))
        .filter(|e| !ancestor_selected(e))
        .filter(|e| !matches!(&e.raw.geom, Geom::Connector { from: End::Element { .. }, to: End::Element { .. } }))
        .map(|e| base_of(snapshot, e))
        .collect()
}

fn base_of(snapshot: &BoardSnapshot, e: &pimble_board::Element) -> Base {
    let parent_offset = e.parent.as_ref().and_then(|p| snapshot.get(p)).map(|p| (p.world.tx, p.world.ty)).unwrap_or((0.0, 0.0));
    let w = e.world;
    let plain = (w.sx - 1.0).abs() < 1e-5 && (w.sy - 1.0).abs() < 1e-5 && w.kx.abs() < 1e-5 && w.ky.abs() < 1e-5;
    Base {
        id: e.raw.id.clone(),
        kind: e.raw.kind.clone(),
        geom: e.raw.geom.clone(),
        transform: e.raw.transform,
        pivot: e.pivot_bounds,
        parent_offset,
        plain,
        bounds: e.bounds,
    }
}

/// Snap a board point to the grid when snapping is on.
fn snap_point(on: bool, p: Point) -> Point {
    if on {
        Point::new((p.x / GRID).round() * GRID, (p.y / GRID).round() * GRID)
    } else {
        p
    }
}

fn snap(store: AppStore, p: Point) -> Point {
    snap_point(untracked(|| store.board_style.snap.get()), p)
}

/// Shift's constraint (contract section 4): lines at multiples of 45°,
/// squares and circles.
fn constrain(kind: &Kind, start: Point, p: Point, shift: bool) -> Point {
    if !shift {
        return p;
    }
    let (dx, dy) = (p.x - start.x, p.y - start.y);
    match kind {
        Kind::Line | Kind::Arrow | Kind::Connector => {
            let len = dx.hypot(dy);
            let step = std::f32::consts::FRAC_PI_4;
            let a = (dy.atan2(dx) / step).round() * step;
            Point::new(start.x + len * a.cos(), start.y + len * a.sin())
        }
        Kind::Rect | Kind::Ellipse | Kind::Diamond | Kind::Frame => {
            let d = dx.abs().max(dy.abs());
            Point::new(start.x + d * dx.signum(), start.y + d * dy.signum())
        }
        _ => p,
    }
}

/// A line or arrow from `a` to `b`, or a shape spanning them.
fn shape_geom(kind: &Kind, a: Point, b: Point) -> Geom {
    match kind {
        Kind::Line | Kind::Arrow => Geom::Segment { a, b },
        _ => Geom::Rect(Rect::from_points(a, b)),
    }
}

/// A move's offset: the pointer's, snapped so the selection's corner lands
/// on the grid, and along one axis with shift.
fn move_delta(bases: &[Base], start: Point, p: Point, snap_on: bool, shift: bool) -> (f32, f32) {
    let (mut dx, mut dy) = (p.x - start.x, p.y - start.y);
    if shift {
        if dx.abs() > dy.abs() {
            dy = 0.0;
        } else {
            dx = 0.0;
        }
    }
    if snap_on {
        if let Some(b) = bases.iter().map(|b| b.bounds).reduce(|a, b| a.union(&b)) {
            let x = ((b.x + dx) / GRID).round() * GRID;
            let y = ((b.y + dy) / GRID).round() * GRID;
            if !(shift && dx == 0.0) {
                dx = x - b.x;
            }
            if !(shift && dy == 0.0) {
                dy = y - b.y;
            }
        }
    }
    (dx, dy)
}

/// An element moved by (`dx`, `dy`): its transform, or a free connector's
/// points.
fn translated(b: &Base, dx: f32, dy: f32) -> Option<(String, Option<Geom>, Option<Transform>)> {
    if let Geom::Connector { from, to } = &b.geom {
        let shift = |end: &End| match end {
            End::Point(p) => End::Point(Point::new(p.x + dx, p.y + dy)),
            other => other.clone(),
        };
        return Some((b.id.clone(), Some(Geom::Connector { from: shift(from), to: shift(to) }), None));
    }
    let t = Transform { tx: b.transform.tx + dx, ty: b.transform.ty + dy, ..b.transform };
    Some((b.id.clone(), None, Some(t)))
}

/// The selection box after dragging `handle` to `p` (shift on a corner keeps
/// the proportions).
fn resized_bounds(handle: Handle, b: &Rect, p: Point, shift: bool) -> Rect {
    let (mut x0, mut y0, mut x1, mut y1) = (b.x, b.y, b.right(), b.bottom());
    match handle {
        Handle::N => y0 = p.y,
        Handle::S => y1 = p.y,
        Handle::E => x1 = p.x,
        Handle::W => x0 = p.x,
        Handle::NE => {
            y0 = p.y;
            x1 = p.x;
        }
        Handle::NW => {
            y0 = p.y;
            x0 = p.x;
        }
        Handle::SE => {
            y1 = p.y;
            x1 = p.x;
        }
        Handle::SW => {
            y1 = p.y;
            x0 = p.x;
        }
        Handle::Rotate => {}
    }
    let corner = matches!(handle, Handle::NE | Handle::NW | Handle::SE | Handle::SW);
    if shift && corner && b.w > 0.0 && b.h > 0.0 {
        let k = ((x1 - x0).abs() / b.w).max((y1 - y0).abs() / b.h);
        let (w, h) = (b.w * k, b.h * k);
        match handle {
            Handle::SE => {
                x1 = x0 + w;
                y1 = y0 + h;
            }
            Handle::NE => {
                x1 = x0 + w;
                y0 = y1 - h;
            }
            Handle::SW => {
                x0 = x1 - w;
                y1 = y0 + h;
            }
            _ => {
                x0 = x1 - w;
                y0 = y1 - h;
            }
        }
    }
    let r = Rect::from_points(Point::new(x0, y0), Point::new(x1, y1));
    Rect::new(r.x, r.y, r.w.max(1.0), r.h.max(1.0))
}

/// An element after the selection box went from `from` to `to`: its
/// geometry edited where it is a plain shape (so a rectangle's outline keeps
/// its width), else its transform scaled about the box.
fn resized(b: &Base, from: &Rect, to: &Rect) -> Option<(String, Option<Geom>, Option<Transform>)> {
    let kx = to.w / from.w.max(1e-3);
    let ky = to.h / from.h.max(1e-3);
    let map = |p: Point| Point::new(to.x + (p.x - from.x) * kx, to.y + (p.y - from.y) * ky);
    let t = b.transform;
    let geometric = b.plain
        && t.rot.abs() < 1e-3
        && (t.sx - 1.0).abs() < 1e-5
        && (t.sy - 1.0).abs() < 1e-5
        && !matches!(b.kind, Kind::Text | Kind::Pen | Kind::Highlighter | Kind::Group | Kind::Step);
    if geometric {
        let off = (b.parent_offset.0 + t.tx, b.parent_offset.1 + t.ty);
        let mp = |p: &Point| {
            let q = map(Point::new(p.x + off.0, p.y + off.1));
            Point::new(q.x - off.0, q.y - off.1)
        };
        let mr = |r: &Rect| Rect::from_points(mp(&Point::new(r.x, r.y)), mp(&Point::new(r.right(), r.bottom())));
        let geom = match &b.geom {
            Geom::Rect(r) => Some(Geom::Rect(mr(r))),
            Geom::Picture(p) => Some(Geom::Picture(PictureGeom { rect: mr(&p.rect), ..p.clone() })),
            Geom::Card { rect, link } => Some(Geom::Card { rect: mr(rect), link: link.clone() }),
            Geom::Segment { a, b } => Some(Geom::Segment { a: mp(a), b: mp(b) }),
            Geom::Callout { rect, tip } => Some(Geom::Callout { rect: mr(rect), tip: mp(tip) }),
            Geom::Connector { from, to } => {
                let end = |e: &End| match e {
                    End::Point(p) => End::Point(mp(p)),
                    other => other.clone(),
                };
                Some(Geom::Connector { from: end(from), to: end(to) })
            }
            _ => None,
        };
        if let Some(geom) = geom {
            return Some((b.id.clone(), Some(geom), None));
        }
    }
    if matches!(b.geom, Geom::Connector { .. }) {
        return None;
    }
    // Scaled about the box: uniformly for a rotated element (a non-uniform
    // scale of a rotated shape would shear it).
    let (sx, sy) = if t.rot.abs() > 1e-3 {
        let k = (kx * ky).abs().sqrt();
        (k, k)
    } else {
        (kx, ky)
    };
    let c = b.pivot.center();
    let cw = Point::new(b.parent_offset.0 + c.x + t.tx, b.parent_offset.1 + c.y + t.ty);
    let ncw = map(cw);
    let transform = Transform {
        tx: ncw.x - c.x - b.parent_offset.0,
        ty: ncw.y - c.y - b.parent_offset.1,
        rot: t.rot,
        sx: t.sx * sx,
        sy: t.sy * sy,
    };
    Some((b.id.clone(), None, Some(transform)))
}

/// The angle a rotate drag has turned, degrees clockwise; shift snaps to 15°.
fn rotation_degrees(start: f32, now: f32, shift: bool) -> f32 {
    let d = (now - start).to_degrees();
    if shift {
        (d / 15.0).round() * 15.0
    } else {
        d
    }
}

/// An element turned by `degrees` about board point `center`: its own
/// rotation grows, and its center goes round the selection's.
fn rotated(b: &Base, center: Point, degrees: f32) -> Option<(String, Option<Geom>, Option<Transform>)> {
    if matches!(b.geom, Geom::Connector { .. }) {
        return None;
    }
    let t = b.transform;
    let c = b.pivot.center();
    let cw = Point::new(b.parent_offset.0 + c.x + t.tx, b.parent_offset.1 + c.y + t.ty);
    let (sin, cos) = degrees.to_radians().sin_cos();
    let (rx, ry) = (cw.x - center.x, cw.y - center.y);
    let ncw = Point::new(center.x + rx * cos - ry * sin, center.y + rx * sin + ry * cos);
    let mut rot = t.rot + degrees;
    while rot > 180.0 {
        rot -= 360.0;
    }
    while rot <= -180.0 {
        rot += 360.0;
    }
    let transform = Transform { tx: t.tx + ncw.x - cw.x, ty: t.ty + ncw.y - cw.y, rot, ..t };
    Some((b.id.clone(), None, Some(transform)))
}

/// Where a connector end at `p` attaches: the element under it (not
/// `except`, not another connector), on the side whose middle it is near,
/// or a free point.
fn end_at(s: &Session, p: Point, except: Option<&str>) -> End {
    let snapshot = s.snapshot();
    let tolerance = HIT_SLOP / s.view.zoom().max(1e-3);
    let target = snapshot
        .query(&Rect::new(p.x, p.y, 0.0, 0.0).inflate(tolerance))
        .into_iter()
        .rev()
        .map(|i| &snapshot.elements[i])
        .find(|e| {
            Some(e.raw.id.as_str()) != except
                && e.raw.kind != Kind::Connector
                && !e.global
                && e.visible
                && (e.hits(p, tolerance) || (e.raw.kind != Kind::Frame && e.bounds.contains(p)))
        });
    match target {
        Some(e) => {
            let b = e.bounds;
            let c = b.center();
            let reach = (12.0 / s.view.zoom().max(1e-3)).max(b.w.min(b.h) * 0.15);
            let sides = [
                (Side::Top, Point::new(c.x, b.y)),
                (Side::Right, Point::new(b.right(), c.y)),
                (Side::Bottom, Point::new(c.x, b.bottom())),
                (Side::Left, Point::new(b.x, c.y)),
            ];
            let side = sides.iter().find(|(_, q)| (q.x - p.x).hypot(q.y - p.y) <= reach).map(|(side, _)| *side);
            End::Element { id: e.raw.id.clone(), side, last: p }
        }
        None => End::Point(p),
    }
}

// ── Frames and groups ───────────────────────────────────────────────────────

/// A frame's own rectangle on the board (without its name label).
fn frame_rect(e: &pimble_board::Element) -> Rect {
    let r = e.pivot_bounds;
    Rect::new(r.x + e.world.tx, r.y + e.world.ty, r.w * e.world.sx, r.h * e.world.sy)
}

/// After elements moved: each one whose center is now in a frame belongs to
/// it, and one that left its frame belongs to none (contract section 3,
/// `parent`: "moving a frame moves what is in it"). Members of groups are
/// left to their group.
fn frame_membership(store: AppStore, pane: PaneId, ids: &[String]) {
    let changes: Vec<(String, Option<String>, Transform)> = with_session(pane, |s| {
        let snapshot = s.snapshot();
        let frames: Vec<&pimble_board::Element> = snapshot.frames();
        let mut out = Vec::new();
        for id in ids {
            let Some(e) = snapshot.get(id) else { continue };
            if matches!(e.raw.kind, Kind::Frame | Kind::Connector) {
                continue;
            }
            let current = e.parent.clone();
            if current.as_ref().and_then(|p| snapshot.get(p)).is_some_and(|p| p.raw.kind == Kind::Group) {
                continue;
            }
            let c = e.bounds.center();
            let next = frames.iter().rev().find(|f| f.raw.id != *id && frame_rect(f).contains(c)).map(|f| f.raw.id.clone());
            if next == current {
                continue;
            }
            let old = current.as_ref().and_then(|p| snapshot.get(p)).map(|p| (p.world.tx, p.world.ty)).unwrap_or((0.0, 0.0));
            let new = next.as_ref().and_then(|p| snapshot.get(p)).map(|p| (p.world.tx, p.world.ty)).unwrap_or((0.0, 0.0));
            let t = e.raw.transform;
            out.push((id.clone(), next, Transform { tx: t.tx + old.0 - new.0, ty: t.ty + old.1 - new.1, ..t }));
        }
        out
    })
    .unwrap_or_default();
    if changes.is_empty() {
        return;
    }
    local_edit(store, pane, |t| {
        for (id, parent, transform) in &changes {
            t.set_parent(id, parent.as_deref())?;
            t.set_transform(id, transform)?;
        }
        Ok(())
    });
}

/// A frame just drawn takes in every element wholly inside it that belongs
/// to no frame or group yet.
fn adopt_into_frame(store: AppStore, pane: PaneId, frame: &str) {
    let members: Vec<(String, Transform)> = with_session(pane, |s| {
        let snapshot = s.snapshot();
        let Some(f) = snapshot.get(frame) else { return Vec::new() };
        let rect = frame_rect(f);
        let offset = (f.world.tx, f.world.ty);
        snapshot
            .within(&rect)
            .into_iter()
            .filter(|e| e.raw.id != frame && e.parent.is_none() && e.raw.kind != Kind::Frame && e.layer == f.layer)
            .map(|e| {
                let t = e.raw.transform;
                (e.raw.id.clone(), Transform { tx: t.tx - offset.0, ty: t.ty - offset.1, ..t })
            })
            .collect()
    })
    .unwrap_or_default();
    if members.is_empty() {
        return;
    }
    local_edit(store, pane, |t| {
        for (id, transform) in &members {
            t.set_parent(id, Some(frame))?;
            t.set_transform(id, transform)?;
        }
        Ok(())
    });
}

/// Ctrl+G: the selection becomes one group (a `group` element they all name
/// as their parent), picked and moved as one.
pub(super) fn group(store: AppStore, pane: PaneId) {
    let plan = with_session(pane, |s| {
        let bases = bases_of(s, &s.selected.clone());
        if bases.len() < 2 {
            return None;
        }
        let snapshot = s.snapshot();
        let parents: HashSet<Option<String>> = bases.iter().map(|b| snapshot.get(&b.id).and_then(|e| e.parent.clone())).collect();
        let common = if parents.len() == 1 { parents.into_iter().next().flatten() } else { None };
        let offset = common.as_ref().and_then(|p| snapshot.get(p)).map(|p| (p.world.tx, p.world.ty)).unwrap_or((0.0, 0.0));
        let layer = snapshot.get(&bases[0].id).map(|e| e.layer.clone())?;
        let members: Vec<(String, Transform)> = bases
            .iter()
            .map(|b| {
                let t = b.transform;
                (b.id.clone(), Transform { tx: t.tx + b.parent_offset.0 - offset.0, ty: t.ty + b.parent_offset.1 - offset.1, ..t })
            })
            .collect();
        Some((common, layer, members))
    })
    .flatten();
    let Some((common, layer, members)) = plan else { return };
    new_undo_step(pane);
    let group = local_edit(store, pane, |t| {
        let mut element = NewElement::new(Kind::Group, Geom::None).in_layer(layer.clone());
        element.parent = common.clone();
        let g = t.add_element(element)?;
        for (id, transform) in &members {
            t.set_parent(id, Some(&g))?;
            t.set_transform(id, transform)?;
        }
        Ok(g)
    });
    if let Some(g) = group {
        select(store, pane, vec![g]);
    }
}

/// Ctrl+Shift+G: every selected group lets go of its members, which keep
/// where they are.
pub(super) fn ungroup(store: AppStore, pane: PaneId) {
    let plan: Vec<(String, Option<String>, Vec<(String, Transform)>)> = with_session(pane, |s| {
        let snapshot = s.snapshot();
        s.selected
            .iter()
            .filter(|id| s.editable(id))
            .filter_map(|id| snapshot.get(id))
            .filter(|g| g.raw.kind == Kind::Group)
            .map(|g| {
                let gt = g.raw.transform;
                let members = snapshot
                    .elements
                    .iter()
                    .filter(|e| e.parent.as_deref() == Some(g.raw.id.as_str()))
                    .map(|e| {
                        let t = e.raw.transform;
                        // The group's own move goes to each member (a turned
                        // or scaled group keeps its members' offsets only).
                        (e.raw.id.clone(), Transform { tx: t.tx + gt.tx, ty: t.ty + gt.ty, ..t })
                    })
                    .collect();
                (g.raw.id.clone(), g.parent.clone(), members)
            })
            .collect()
    })
    .unwrap_or_default();
    if plan.is_empty() {
        return;
    }
    new_undo_step(pane);
    let freed: Vec<String> = plan.iter().flat_map(|(_, _, m)| m.iter().map(|(id, _)| id.clone())).collect();
    local_edit(store, pane, |t| {
        for (group, parent, members) in &plan {
            for (id, transform) in members {
                t.set_parent(id, parent.as_deref())?;
                t.set_transform(id, transform)?;
            }
            t.delete_element(group)?;
        }
        Ok(())
    });
    select(store, pane, freed);
}

// ── Arranging ───────────────────────────────────────────────────────────────

/// Delete the selection (a group with its members); locked elements stay.
pub(crate) fn delete_selected(store: AppStore, pane: PaneId) {
    let ids: Vec<String> = with_session(pane, |s| {
        let snapshot = s.snapshot();
        let mut out: Vec<String> = Vec::new();
        for id in s.selected.iter().filter(|id| s.editable(id)) {
            collect_with_members(snapshot, id, &mut out);
        }
        out
    })
    .unwrap_or_default();
    if ids.is_empty() {
        if with_session(pane, |s| !s.selected.is_empty()) == Some(true) {
            crate::events::show_notice(store, LOCKED.to_string());
        }
        return;
    }
    new_undo_step(pane);
    local_edit(store, pane, |t| {
        for id in &ids {
            // A member may have gone with its group already.
            let _ = t.delete_element(id);
        }
        Ok(())
    });
}

/// `id` and, for a group, everything in it.
fn collect_with_members(snapshot: &BoardSnapshot, id: &str, out: &mut Vec<String>) {
    if out.iter().any(|x| x == id) {
        return;
    }
    out.push(id.to_string());
    if snapshot.get(id).is_some_and(|e| e.raw.kind == Kind::Group) {
        for e in &snapshot.elements {
            if e.parent.as_deref() == Some(id) {
                collect_with_members(snapshot, &e.raw.id, out);
            }
        }
    }
}

/// Ctrl+D: a copy of the selection (a group with its members), a little
/// below and to the right, selected.
pub(super) fn duplicate(store: AppStore, pane: PaneId) {
    let plan: Vec<(pimble_board::RawElement, Vec<pimble_board::RawElement>)> = with_session(pane, |s| {
        let snapshot = s.snapshot();
        s.selected
            .iter()
            .filter_map(|id| snapshot.get(id))
            .map(|e| {
                let mut members = Vec::new();
                let mut ids = Vec::new();
                collect_with_members(snapshot, &e.raw.id, &mut ids);
                for id in ids.iter().skip(1) {
                    if let Some(m) = snapshot.get(id) {
                        members.push(m.raw.clone());
                    }
                }
                (e.raw.clone(), members)
            })
            .collect()
    })
    .unwrap_or_default();
    if plan.is_empty() {
        return;
    }
    if !writable_or_say(store, pane) {
        return;
    }
    new_undo_step(pane);
    let copies = local_edit(store, pane, |t| {
        let mut out = Vec::new();
        for (top, members) in &plan {
            let mut map: HashMap<String, String> = HashMap::new();
            let first = copy_element(t, top, top.parent.clone(), DUPLICATE_OFFSET)?;
            map.insert(top.id.clone(), first.clone());
            // Members after their parents (collected parents first).
            for m in members {
                let parent = m.parent.as_ref().and_then(|p| map.get(p)).cloned();
                let id = copy_element(t, m, parent, 0.0)?;
                map.insert(m.id.clone(), id);
            }
            out.push(first);
        }
        Ok(out)
    });
    if let Some(copies) = copies {
        select(store, pane, copies);
    }
}

/// One element written again as a new one, `offset` board units down and
/// right.
fn copy_element(t: &mut BoardTxn<'_, '_>, e: &pimble_board::RawElement, parent: Option<String>, offset: f32) -> pimble_board::doc::Result<String> {
    let mut element = NewElement::new(e.kind.clone(), e.geom.clone()).in_layer(e.layer.clone()).with_style(e.style.clone());
    element.parent = parent;
    element.text = e.text.clone();
    element.points = e.points.clone();
    let mut transform = e.transform;
    if let Geom::Connector { from, to } = &e.geom {
        let shift = |end: &End| match end {
            End::Point(p) => End::Point(Point::new(p.x + offset, p.y + offset)),
            other => other.clone(),
        };
        element.geom = Geom::Connector { from: shift(from), to: shift(to) };
    } else {
        transform.tx += offset;
        transform.ty += offset;
    }
    element.transform = Some(transform);
    t.add_element(element)
}

/// Which way an element goes in its layer's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Order {
    Forward,
    Backward,
    Front,
    Back,
}

/// Bring forward / send back (writes `order`, contract section 4), within
/// each element's layer.
pub(super) fn reorder(store: AppStore, pane: PaneId, order: Order) {
    let plan: Vec<(String, Placement)> = with_session(pane, |s| {
        let snapshot = s.snapshot();
        let mut ids: Vec<&String> = s.selected.iter().filter(|id| s.editable(id)).collect();
        // Forward from the top down, backward from the bottom up, so
        // neighbours in the selection do not trade places.
        let z = |id: &String| snapshot.elements.iter().position(|e| &e.raw.id == id).unwrap_or(0);
        ids.sort_by_key(|id| z(id));
        if matches!(order, Order::Forward | Order::Front) {
            ids.reverse();
        }
        ids.into_iter()
            .filter_map(|id| {
                let e = snapshot.get(id)?;
                let same: Vec<&pimble_board::Element> = snapshot.elements.iter().filter(|x| x.layer == e.layer).collect();
                let i = same.iter().position(|x| x.raw.id == *id)?;
                let placement = match order {
                    Order::Front => Placement::Top,
                    Order::Back => Placement::Bottom,
                    Order::Forward => Placement::Above(same.get(i + 1)?.raw.id.clone()),
                    Order::Backward => Placement::Below(same.get(i.checked_sub(1)?)?.raw.id.clone()),
                };
                Some((id.clone(), placement))
            })
            .collect()
    })
    .unwrap_or_default();
    if plan.is_empty() {
        return;
    }
    new_undo_step(pane);
    local_edit(store, pane, |t| {
        for (id, placement) in &plan {
            t.move_element(id, placement.clone())?;
        }
        Ok(())
    });
}

/// Lock or unlock the selection (a guard against accidents, for everyone).
pub(super) fn toggle_lock(store: AppStore, pane: PaneId) {
    let plan = with_session(pane, |s| {
        let snapshot = s.snapshot();
        let any_unlocked = s.selected.iter().filter_map(|id| snapshot.get(id)).any(|e| !e.raw.locked);
        (s.selected.clone(), any_unlocked)
    });
    let Some((ids, lock)) = plan else { return };
    if ids.is_empty() {
        return;
    }
    new_undo_step(pane);
    local_edit(store, pane, |t| {
        for id in &ids {
            t.set_locked(id, lock)?;
        }
        Ok(())
    });
}

/// How a selection lines up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Align2 {
    Left,
    Center,
    Right,
    Top,
    Middle,
    Bottom,
    /// Equal gaps across, or down.
    SpreadAcross,
    SpreadDown,
}

/// Align or distribute the selection (contract section 4, "Arranging").
pub(super) fn align(store: AppStore, pane: PaneId, how: Align2) {
    let writes = with_session(pane, |s| {
        let bases = bases_of(s, &s.selected.clone());
        if bases.len() < 2 {
            return Vec::new();
        }
        let all = bases.iter().map(|b| b.bounds).reduce(|a, b| a.union(&b)).unwrap_or_default();
        let mut deltas: Vec<(usize, f32, f32)> = Vec::new();
        match how {
            Align2::SpreadAcross | Align2::SpreadDown => {
                if bases.len() < 3 {
                    return Vec::new();
                }
                let across = how == Align2::SpreadAcross;
                let mut order: Vec<usize> = (0..bases.len()).collect();
                let key = |i: &usize| if across { bases[*i].bounds.center().x } else { bases[*i].bounds.center().y };
                order.sort_by(|a, b| key(a).partial_cmp(&key(b)).unwrap_or(std::cmp::Ordering::Equal));
                let size: f32 = bases.iter().map(|b| if across { b.bounds.w } else { b.bounds.h }).sum();
                let span = if across { all.w } else { all.h };
                let gap = (span - size) / (bases.len() - 1) as f32;
                let mut at = if across { all.x } else { all.y };
                for i in order {
                    let b = bases[i].bounds;
                    if across {
                        deltas.push((i, at - b.x, 0.0));
                        at += b.w + gap;
                    } else {
                        deltas.push((i, 0.0, at - b.y));
                        at += b.h + gap;
                    }
                }
            }
            _ => {
                for (i, b) in bases.iter().enumerate() {
                    let r = b.bounds;
                    let (dx, dy) = match how {
                        Align2::Left => (all.x - r.x, 0.0),
                        Align2::Center => (all.center().x - r.center().x, 0.0),
                        Align2::Right => (all.right() - r.right(), 0.0),
                        Align2::Top => (0.0, all.y - r.y),
                        Align2::Middle => (0.0, all.center().y - r.center().y),
                        Align2::Bottom => (0.0, all.bottom() - r.bottom()),
                        _ => (0.0, 0.0),
                    };
                    deltas.push((i, dx, dy));
                }
            }
        }
        deltas.into_iter().filter(|(_, dx, dy)| dx.abs() > 1e-4 || dy.abs() > 1e-4).filter_map(|(i, dx, dy)| translated(&bases[i], dx, dy)).collect()
    })
    .unwrap_or_default();
    if writes.is_empty() {
        return;
    }
    new_undo_step(pane);
    write_elements(store, pane, &writes);
}

/// Arrow keys: the selection moves by a unit (shift: ten; a grid step while
/// snapping).
fn nudge(store: AppStore, pane: PaneId, dx: f32, dy: f32) {
    let writes = with_session(pane, |s| {
        let bases = bases_of(s, &s.selected.clone());
        bases.iter().filter_map(|b| translated(b, dx, dy)).collect::<Vec<_>>()
    })
    .unwrap_or_default();
    write_elements(store, pane, &writes);
}

/// Ctrl+A: every element that can be picked.
fn select_all(store: AppStore, pane: PaneId) {
    let ids = with_session(pane, |s| {
        let snapshot = s.snapshot();
        let mut out: Vec<String> = Vec::new();
        for e in &snapshot.elements {
            if !e.visible || snapshot.layer(&e.layer).is_some_and(|l| l.locked) {
                continue;
            }
            let id = select_target(snapshot, e);
            if !out.contains(&id) {
                out.push(id);
            }
        }
        out
    })
    .unwrap_or_default();
    select(store, pane, ids);
}

/// Replace the selection.
pub(super) fn select(store: AppStore, pane: PaneId, ids: Vec<String>) {
    with_session(pane, |s| s.selected = ids);
    load_selection_style(store, pane);
    after_change(store, pane);
}

// ── The eraser ──────────────────────────────────────────────────────────────

/// The eraser removes whole elements (contract section 3: "there are no
/// pixels"), never locked ones.
fn erase_at(store: AppStore, pane: PaneId, p: Point) {
    let target = with_session(pane, |s| {
        let e = hit_at(s, p)?;
        let id = e.raw.id.clone();
        s.editable(&id).then_some(id)
    })
    .flatten();
    if let Some(id) = target {
        local_edit(store, pane, |t| t.delete_element(&id));
    }
}

// ── Style ───────────────────────────────────────────────────────────────────

/// The layer new elements go into: the one picked in the layers panel, else
/// the topmost that is not locked. `None` (with a sentence) when that layer
/// is locked.
fn target_layer(store: AppStore, pane: PaneId) -> Option<String> {
    let chosen = untracked(|| store.pane(pane).board_ui.layer.get());
    let found = with_session(pane, |s| {
        let snapshot = s.snapshot();
        match chosen.as_ref().and_then(|id| snapshot.layer(id)) {
            Some(l) => (!l.locked).then(|| l.id.clone()).ok_or(()),
            None => snapshot.layers.iter().rev().find(|l| !l.locked).map(|l| l.id.clone()).ok_or(()),
        }
    });
    match found {
        Some(Ok(id)) => Some(id),
        Some(Err(())) => {
            crate::events::show_notice(store, "That layer is locked. Unlock it, or pick another layer to draw in.".to_string());
            None
        }
        None => None,
    }
}

fn with_layer(element: NewElement, layer: &str) -> NewElement {
    element.in_layer(layer.to_string())
}

/// The style a new element of `kind` takes from the style panel.
pub(super) fn new_style(store: AppStore, kind: &Kind) -> Style {
    let st = store.board_style;
    let (stroke, fill, width, opacity, dash, font_size, align, heads) = untracked(|| {
        (
            Color::parse(&st.stroke.get()),
            st.fill.get().and_then(|c| Color::parse(&c)),
            st.width.get(),
            st.opacity.get(),
            st.dash.get(),
            st.font_size.get(),
            st.align.get(),
            st.heads.get(),
        )
    });
    let opacity = (opacity < 0.999).then_some(opacity);
    let dash = (dash != Dash::Solid).then_some(dash);
    match kind {
        Kind::Pen => Style { stroke, width: Some(width), opacity, dash, ..Style::default() },
        // The marks' red multiplied over a picture reads as a dark smear: a
        // highlighter left on the first color is the highlighter's yellow.
        Kind::Highlighter => {
            let stroke = stroke.filter(|c| c.to_hex() != DEFAULT_COLOR);
            Style { stroke, width: Some(width * 5.0), opacity, ..Style::default() }
        }
        Kind::Line => Style { stroke, width: Some(width), opacity, dash, arrow_heads: Some(ArrowHeads::None), ..Style::default() },
        Kind::Arrow => Style { stroke, width: Some(width), opacity, dash, arrow_heads: Some(heads), ..Style::default() },
        Kind::Connector => Style { stroke, width: Some((width / 2.0).max(1.0)), opacity, dash, arrow_heads: Some(heads), ..Style::default() },
        Kind::Rect | Kind::Ellipse | Kind::Diamond => {
            Style { stroke, fill, width: Some(width), opacity, dash, font_size: Some(font_size), align: Some(align), ..Style::default() }
        }
        Kind::Text => Style { stroke, fill, opacity, font_size: Some(font_size), align: Some(align), ..Style::default() },
        Kind::Sticky => Style { fill, opacity, font_size: Some(font_size * 0.85), align: Some(align), ..Style::default() },
        Kind::Step => Style { stroke, opacity, font_size: Some(font_size), ..Style::default() },
        Kind::Frame => Style::default(),
        _ => Style { stroke, opacity, ..Style::default() },
    }
}

/// The style panel shows the first selected element's style (and new
/// elements take it, as in most drawing tools).
fn load_selection_style(store: AppStore, pane: PaneId) {
    let style = with_session(pane, |s| {
        let snapshot = s.snapshot();
        let e = s.selected.first().and_then(|id| snapshot.get(id))?;
        (!matches!(e.raw.kind, Kind::Picture | Kind::Group | Kind::Card)).then(|| (e.raw.kind.clone(), e.style.clone()))
    })
    .flatten();
    let Some((kind, s)) = style else { return };
    let st = store.board_style;
    match kind {
        // A sticky note's color is its own: shapes drawn next do not take it.
        Kind::Sticky => {}
        Kind::Highlighter => {
            st.stroke.set_if_changed(s.stroke.to_hex());
            st.width.set_if_changed((s.width / 5.0).max(1.0));
        }
        _ => {
            st.stroke.set_if_changed(Color { a: 255, ..s.stroke }.to_hex());
            if matches!(kind, Kind::Rect | Kind::Ellipse | Kind::Diamond | Kind::Text) {
                st.fill.set_if_changed(s.fill.map(|c| c.to_hex()));
            }
            if s.width > 0.0 {
                st.width.set_if_changed(s.width);
            }
            if matches!(kind, Kind::Arrow | Kind::Connector) {
                st.heads.set_if_changed(s.arrow_heads);
            }
        }
    }
    st.opacity.set_if_changed(s.opacity);
    st.dash.set_if_changed(s.dash);
    // A sticky note's text is drawn smaller than the size picked (`new_style`).
    st.font_size.set_if_changed(if kind == Kind::Sticky { (s.font_size / 0.85).round() } else { s.font_size });
    st.align.set_if_changed(s.align);
}

/// One style key changed in the panel: written to every selected element
/// that has it (contract section 4, "Style").
pub(super) fn apply_style(store: AppStore, pane: PaneId, style: Style) {
    let ids = with_session(pane, |s| s.selected.iter().filter(|id| s.editable(id)).cloned().collect::<Vec<_>>()).unwrap_or_default();
    if ids.is_empty() {
        return;
    }
    let kinds: HashMap<String, Kind> =
        with_session(pane, |s| ids.iter().filter_map(|id| s.snapshot().get(id).map(|e| (id.clone(), e.raw.kind.clone()))).collect())
            .unwrap_or_default();
    new_undo_step(pane);
    local_edit(store, pane, |t| {
        for id in &ids {
            let Some(kind) = kinds.get(id) else { continue };
            if matches!(kind, Kind::Picture | Kind::Group) {
                continue;
            }
            let mut s = style.clone();
            if *kind == Kind::Highlighter {
                s.width = s.width.map(|w| w * 5.0);
            }
            if !matches!(kind, Kind::Line | Kind::Arrow | Kind::Connector) {
                s.arrow_heads = None;
            }
            if *kind == Kind::Sticky {
                // A sticky note's color is its fill; its text stays dark.
                if let Some(c) = s.stroke.take() {
                    s.fill = Some(c);
                }
            }
            t.set_style(id, &s)?;
            if style.fill.is_none() && style.stroke.is_none() && style.width.is_none() && style.opacity.is_none() && style.dash.is_none()
                && style.font_size.is_none() && style.align.is_none() && style.arrow_heads.is_none()
            {
                // "No fill" is clearing the key.
                t.clear_style(id, pimble_board::StyleKey::Fill)?;
            }
        }
        Ok(())
    });
}

// ── Text ────────────────────────────────────────────────────────────────────

/// [`begin_text_new`], for tests.
#[cfg(test)]
pub(super) fn begin_text_new_for_test(store: AppStore, pane: PaneId, at: Point) {
    begin_text_new(store, pane, at)
}

/// Start typing a new text element at `at` (created by the first character).
fn begin_text_new(store: AppStore, pane: PaneId, at: Point) {
    if !writable_or_say(store, pane) || target_layer(store, pane).is_none() {
        return;
    }
    new_undo_step(pane);
    with_session(pane, |s| {
        s.selected.clear();
        s.text = Some(TextEdit { id: None, kind: Kind::Text, at, last: String::new() });
    });
    open_text_input(store, pane, String::new());
}

/// Start typing into element `id`'s text (a text, a sticky note, the text
/// inside a shape, a frame's name, a connector's label).
pub(super) fn begin_text_existing(store: AppStore, pane: PaneId, id: &str) {
    if !writable_or_say(store, pane) {
        return;
    }
    let found = with_session(pane, |s| {
        let e = s.snapshot().get(id)?;
        let text = e.raw.text.clone().unwrap_or_default();
        s.text = Some(TextEdit { id: Some(id.to_string()), kind: e.raw.kind.clone(), at: Point::new(e.bounds.x, e.bounds.y), last: text.clone() });
        Some(text)
    })
    .flatten();
    let Some(text) = found else { return };
    new_undo_step(pane);
    open_text_input(store, pane, text);
}

fn open_text_input(store: AppStore, pane: PaneId, text: String) {
    let ui = store.pane(pane).board_ui;
    ui.text_value.update(|(n, v)| {
        *n += 1;
        *v = text;
    });
    place_text_box(store, pane);
    // Escape ends the typing; everything else is the input's.
    rinch::core::set_keyboard_interceptor(move |data| {
        if data.key == "Escape" {
            run_on_main_thread(move || end_text(store, pane));
            return true;
        }
        false
    });
    // Focus once the input is shown.
    let _ = set_timeout(0, move || ui::focus_text_input(pane));
}

/// A keystroke in the text input: the element's yrs Text gets the smallest
/// edit from what it holds (so someone typing elsewhere in it keeps theirs);
/// a new text is made by its first character.
pub(super) fn text_input(store: AppStore, pane: PaneId, value: String) {
    let Some((id, at)) = with_session(pane, |s| s.text.as_ref().map(|t| (t.id.clone(), t.at))).flatten() else { return };
    match id {
        None => {
            if value.is_empty() {
                return;
            }
            let Some(layer) = target_layer(store, pane) else { return };
            let style = new_style(store, &Kind::Text);
            let element = with_layer(NewElement::new(Kind::Text, Geom::Rect(Rect::new(at.x, at.y, 0.0, 0.0))).with_text(value.clone()).with_style(style), &layer);
            let made = local_edit(store, pane, |t| t.add_element(element));
            with_session(pane, |s| {
                if let (Some(edit), Some(id)) = (s.text.as_mut(), made) {
                    edit.id = Some(id.clone());
                    edit.last = value.clone();
                    s.selected = vec![id];
                }
            });
        }
        Some(id) => {
            with_session(pane, |s| {
                if let Some(edit) = s.text.as_mut() {
                    edit.last = value.clone();
                }
            });
            local_edit(store, pane, |t| t.set_text(&id, &value));
        }
    }
    place_text_box(store, pane);
}

/// Stop typing: an empty text element goes, the input hides.
pub(crate) fn end_text(store: AppStore, pane: PaneId) {
    let Some(edit) = with_session(pane, |s| s.text.take()).flatten() else { return };
    let ui = store.pane(pane).board_ui;
    ui.text_box.set(None);
    rinch::core::clear_keyboard_interceptor();
    ui::release_text_input(pane);
    if let Some(id) = edit.id {
        let empty = with_session(pane, |s| s.snapshot().get(&id).map(|e| e.raw.text.as_deref().unwrap_or("").trim().is_empty())).flatten();
        if empty == Some(true) && edit.kind == Kind::Text {
            local_edit(store, pane, |t| t.delete_element(&id));
        }
    }
    after_change(store, pane);
}

/// The element being typed into changed under the input (a collaborator
/// typed in it too): the input shows the merged text.
pub(super) fn text_changed_elsewhere(store: AppStore, pane: PaneId) {
    let merged = with_session(pane, |s| {
        let edit = s.text.as_ref()?;
        let id = edit.id.as_ref()?;
        let e = s.snapshot().get(id);
        match e {
            None => Some(None),
            Some(e) => {
                let now = e.raw.text.clone().unwrap_or_default();
                (now != edit.last).then_some(Some(now))
            }
        }
    })
    .flatten();
    match merged {
        // The element went (deleted by someone else).
        Some(None) => end_text(store, pane),
        Some(Some(text)) => {
            with_session(pane, |s| {
                if let Some(edit) = s.text.as_mut() {
                    edit.last = text.clone();
                }
            });
            store.pane(pane).board_ui.text_value.update(|(n, v)| {
                *n += 1;
                *v = text;
            });
        }
        None => {}
    }
}

/// Where the text input sits over the board: on the text being typed, at
/// its size on screen.
pub(super) fn place_text_box(store: AppStore, pane: PaneId) {
    let scale = scale_of(pane);
    let dark = untracked(|| store.dark_mode.get());
    let st = store.board_style;
    let (stroke, font_size, align) = untracked(|| (st.stroke.get(), st.font_size.get(), st.align.get()));
    let placed = with_session(pane, |s| {
        let edit = s.text.as_ref()?;
        let zoom = s.view.zoom();
        let bg = background(dark);
        let board_bg = format!("rgba({}, {}, {}, 0.92)", bg.r, bg.g, bg.b);
        let (rect, font, color, background_css, align) = match edit.id.as_ref().and_then(|id| s.snapshot().get(id)) {
            None => {
                let w = (font_size * 6.0).max(120.0 / zoom * scale);
                (Rect::new(edit.at.x, edit.at.y, w, font_size * 1.3), font_size, stroke.clone(), board_bg, align)
            }
            Some(e) => {
                let k = (e.world.sx * e.world.sy - e.world.kx * e.world.ky).abs().sqrt().max(1e-3);
                let font = e.style.font_size * k;
                let world_rect = |r: &Rect| Rect::new(r.x * e.world.sx + e.world.tx, r.y * e.world.sy + e.world.ty, r.w * e.world.sx, r.h * e.world.sy);
                let pivot = world_rect(&e.pivot_bounds);
                let color = Color { a: 255, ..e.style.stroke }.to_hex();
                match e.raw.kind {
                    Kind::Sticky => {
                        let fill = e.style.fill.unwrap_or(pimble_board::STICKY_YELLOW);
                        let tb = e.text_box.map(|r| world_rect(&r)).unwrap_or(pivot);
                        let h = tb.h.max(pivot.h - (tb.y - pivot.y) * 2.0);
                        (Rect::new(tb.x, tb.y, tb.w, h), font, color, fill.to_hex(), e.style.align)
                    }
                    Kind::Rect | Kind::Ellipse | Kind::Diamond | Kind::Callout => {
                        let pad = font * 0.4;
                        let r = Rect::new(pivot.x + pad, pivot.center().y - font * 0.75, (pivot.w - 2.0 * pad).max(font * 4.0), font * 1.5);
                        (r, font, color, board_bg, e.style.align)
                    }
                    Kind::Frame => {
                        let r = Rect::new(pivot.x, pivot.y - font * 1.5, pivot.w.max(font * 8.0), font * 1.35);
                        (r, font, "#616161".to_string(), board_bg, Align::Left)
                    }
                    Kind::Connector => {
                        let (a, b) = e.route.unwrap_or_default();
                        let mid = Point::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
                        (Rect::new(mid.x - font * 4.0, mid.y - font * 0.7, font * 8.0, font * 1.4), font, color, board_bg, Align::Center)
                    }
                    _ => {
                        let tb = e.text_box.map(|r| world_rect(&r)).unwrap_or(pivot);
                        let w = tb.w.max(font * 6.0);
                        (Rect::new(tb.x, tb.y, w, tb.h.max(font * 1.3)), font, color, board_bg, e.style.align)
                    }
                }
            }
        };
        let (x, y, w, h) = s.to_screen(&rect);
        Some(TextBox {
            x: x / scale,
            y: y / scale,
            w: (w / scale).max(40.0),
            h: (h / scale).max(18.0),
            font_px: (font * zoom / scale).max(6.0),
            color,
            background: background_css,
            align,
        })
    })
    .flatten();
    store.pane(pane).board_ui.text_box.set_if_changed(placed);
}

// ── Node cards ──────────────────────────────────────────────────────────────

/// A tree row dropped on a board becomes a node card where it was dropped
/// (contract section 4): a `pimble:` link to the row's node, which shows its
/// icon, color and title live.
pub(super) fn drop_tree_row(store: AppStore, pane: PaneId) {
    let Some(ctx) = TREE_DRAG.with(Cell::get) else { return };
    let Some(value) = ctx.take() else { return };
    store.drop_target.set(None);
    let Some((store_id, Some(node_id))) = crate::state::parse_tree_value(&value) else { return };
    let Some(layer) = target_layer(store, pane) else { return };
    let title = store
        .get_node_signal(store_id, node_id)
        .map(|sig| untracked(|| sig.with(|n| n.metadata.title.clone())))
        .unwrap_or_default();
    let link = pimble_core::PimbleUrl { store: store_id, node: node_id, anchor: None }.to_string();
    let Some(at) = with_session(pane, |s| s.view.screen_to_board(s.pointer.0, s.pointer.1)) else { return };
    let (w, h) = CARD_SIZE;
    let at = snap(store, Point::new(at.x - w / 2.0, at.y - h / 2.0));
    let rect = Rect::new(at.x, at.y, w, h);
    new_undo_step(pane);
    let mut element = with_layer(NewElement::new(Kind::Card, Geom::Card { rect, link }), &layer);
    // The title when it was dropped: what shows where the node cannot be
    // read (the live title wins wherever it can).
    element.text = Some(title);
    if let Some(id) = local_edit(store, pane, |t| t.add_element(element)) {
        select(store, pane, vec![id]);
    }
}

// ── Attribution ─────────────────────────────────────────────────────────────

/// What an element is called in its attribution (contract section 6).
fn kind_label(e: &pimble_board::Element) -> String {
    match &e.raw.kind {
        Kind::Picture => "Picture".to_string(),
        Kind::Pen => "Drawing".to_string(),
        Kind::Highlighter => "Highlight".to_string(),
        Kind::Line => "Line".to_string(),
        Kind::Arrow => "Arrow".to_string(),
        Kind::Rect => "Rectangle".to_string(),
        Kind::Ellipse => "Ellipse".to_string(),
        Kind::Diamond => "Diamond".to_string(),
        Kind::Text => "Text".to_string(),
        Kind::Sticky => "Sticky note".to_string(),
        Kind::Step => format!("Step {}", e.step_number.unwrap_or(0)),
        Kind::Callout => "Callout".to_string(),
        Kind::Connector => "Connector".to_string(),
        Kind::Frame => "Frame".to_string(),
        Kind::Group => "Group".to_string(),
        Kind::Card => "Node card".to_string(),
        Kind::Spotlight => "Spotlight".to_string(),
        Kind::Other(k) => k.clone(),
    }
}

/// "2 hours ago", from an RFC 3339 stamp.
fn ago(stamp: &str) -> String {
    let Ok(t) = chrono::DateTime::parse_from_rfc3339(stamp) else { return String::new() };
    let secs = (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds().max(0);
    let plural = |n: i64, unit: &str| if n == 1 { format!("1 {unit} ago") } else { format!("{n} {unit}s ago") };
    match secs {
        0..=44 => "just now".to_string(),
        45..=3599 => plural((secs + 30) / 60, "minute"),
        3600..=86_399 => plural(secs / 3600, "hour"),
        86_400..=2_591_999 => plural(secs / 86_400, "day"),
        _ => t.format("%Y-%m-%d").to_string(),
    }
}

/// "Arrow, by Sam, 2 hours ago (changed by Joe, 5 minutes ago)".
pub(super) fn attribution(e: &pimble_board::Element) -> String {
    let by = match &e.raw.author {
        Some(a) => format!("by {}", a.name),
        None => "before sign-in".to_string(),
    };
    let mut out = format!("{}, {by}, {}", kind_label(e), ago(&e.raw.created_at));
    if let (Some(who), Some(when)) = (&e.raw.edited_by, &e.raw.edited_at) {
        let same = e.raw.author.as_ref().is_some_and(|a| a.id == who.id);
        if *when != e.raw.created_at && !(same && ago(when) == ago(&e.raw.created_at)) {
            out.push_str(&format!(" (changed by {}, {})", who.name, ago(when)));
        }
    }
    out
}

/// The pointer rests over the board: on a board with two or more authors,
/// the element under it says who made it (contract section 6: attribution
/// shows only when it says something).
fn hover_attribution(store: AppStore, pane: PaneId, px: f32, py: f32) {
    let scale = scale_of(pane);
    let ui = store.pane(pane).board_ui;
    let tip = with_session(pane, |s| {
        if s.snapshot().authors().len() < 2 || s.text.is_some() {
            return None;
        }
        let p = s.view.screen_to_board(px, py);
        let e = hit_at(s, p)?;
        Some((px / scale + 14.0, py / scale + 16.0, attribution(e)))
    })
    .flatten();
    ui.tip.set_if_changed(tip);
}

// ── Keys ────────────────────────────────────────────────────────────────────

fn key_down(store: AppStore, pane: PaneId, key: &SurfaceKeyData) {
    if with_session(pane, |s| s.text.is_some()).unwrap_or(false) {
        return;
    }
    let primary = key.ctrl || key.meta;
    let lower = key.key.to_ascii_lowercase();
    match key.key.as_str() {
        " " => {
            with_session(pane, |s| s.space = true);
        }
        "Shift" => {
            with_session(pane, |s| s.shift = true);
        }
        "Delete" | "Backspace" => delete_selected(store, pane),
        "Escape" => {
            with_session(pane, |s| {
                if matches!(s.gesture, Gesture::Stroke { .. }) {
                    s.view.cancel_stroke();
                }
                s.gesture = Gesture::Idle;
                s.selected.clear();
            });
            after_change(store, pane);
        }
        "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight" => {
            let snap_on = untracked(|| store.board_style.snap.get());
            let step = if snap_on {
                GRID
            } else if key.shift {
                NUDGE * 10.0
            } else {
                NUDGE
            };
            let (dx, dy) = match key.key.as_str() {
                "ArrowUp" => (0.0, -step),
                "ArrowDown" => (0.0, step),
                "ArrowLeft" => (-step, 0.0),
                _ => (step, 0.0),
            };
            nudge(store, pane, dx, dy);
        }
        _ if primary => match lower.as_str() {
            "z" => undo(store, pane, key.shift),
            "y" => undo(store, pane, true),
            "d" => duplicate(store, pane),
            "g" if key.shift => ungroup(store, pane),
            "g" => group(store, pane),
            "a" => select_all(store, pane),
            "c" => export::copy_picture(store, pane),
            "l" => toggle_lock(store, pane),
            "]" => reorder(store, pane, if key.shift { Order::Front } else { Order::Forward }),
            "[" => reorder(store, pane, if key.shift { Order::Back } else { Order::Backward }),
            _ => {}
        },
        k if !key.alt => {
            if let Some(tool) = Tool::from_key(k) {
                store.board_tool.set(tool);
            }
        }
        _ => {}
    }
}

/// Undo (or redo) this person's last edit of pane `pane`'s board, never a
/// collaborator's (contract section 4).
pub(crate) fn undo(store: AppStore, pane: PaneId, redo: bool) {
    let Some(held) = with_session(pane, |s| (s.store_id, s.node_id)) else { return };
    if !store.node_access(held.0, held.1).allows_write() {
        return;
    }
    let update = with_session(pane, |s| {
        let update = s.undo.as_mut().and_then(|undo| s.board.undo(undo, redo));
        s.sync_view();
        update
    })
    .flatten();
    if let Some(bytes) = update {
        send_update(store, pane, held.0, held.1, bytes);
    }
    after_change(store, pane);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(kind: Kind, geom: Geom) -> Base {
        Base {
            id: "e".into(),
            kind,
            geom,
            transform: Transform::IDENTITY,
            pivot: Rect::new(0.0, 0.0, 100.0, 50.0),
            parent_offset: (0.0, 0.0),
            plain: true,
            bounds: Rect::new(0.0, 0.0, 100.0, 50.0),
        }
    }

    #[test]
    fn shift_constrains_lines_to_45_degrees_and_shapes_to_squares() {
        let p = constrain(&Kind::Line, Point::new(0.0, 0.0), Point::new(100.0, 10.0), true);
        assert!(p.y.abs() < 1e-3 && (p.x - 100.5).abs() < 1.0, "{p:?}");
        let p = constrain(&Kind::Arrow, Point::new(0.0, 0.0), Point::new(100.0, 90.0), true);
        assert!((p.x - p.y).abs() < 1e-3, "{p:?}");
        let p = constrain(&Kind::Rect, Point::new(0.0, 0.0), Point::new(100.0, -40.0), true);
        assert_eq!(p, Point::new(100.0, -100.0));
        assert_eq!(constrain(&Kind::Rect, Point::new(0.0, 0.0), Point::new(100.0, -40.0), false), Point::new(100.0, -40.0));
    }

    #[test]
    fn resizing_a_rectangle_edits_its_geometry_and_a_stroke_its_transform() {
        let from = Rect::new(0.0, 0.0, 100.0, 50.0);
        let to = Rect::new(0.0, 0.0, 200.0, 100.0);
        let (_, geom, t) = resized(&base(Kind::Rect, Geom::Rect(from)), &from, &to).unwrap();
        assert_eq!(geom, Some(Geom::Rect(to)));
        assert!(t.is_none());
        let (_, geom, t) = resized(&base(Kind::Pen, Geom::Stroke), &from, &to).unwrap();
        assert!(geom.is_none());
        let t = t.unwrap();
        assert_eq!((t.sx, t.sy), (2.0, 2.0));
        // The pivot's center (50, 25) lands on the new box's (100, 50).
        assert_eq!((t.tx, t.ty), (50.0, 25.0));
    }

    #[test]
    fn a_handle_drag_with_shift_keeps_proportions() {
        let b = Rect::new(0.0, 0.0, 100.0, 50.0);
        let r = resized_bounds(Handle::SE, &b, Point::new(300.0, 60.0), true);
        assert_eq!(r, Rect::new(0.0, 0.0, 300.0, 150.0));
        let r = resized_bounds(Handle::E, &b, Point::new(300.0, 60.0), false);
        assert_eq!(r, Rect::new(0.0, 0.0, 300.0, 50.0));
    }

    #[test]
    fn rotating_turns_about_the_selection_center() {
        let b = base(Kind::Rect, Geom::Rect(Rect::new(0.0, 0.0, 100.0, 50.0)));
        // About its own center: only the rotation changes.
        let (_, _, t) = rotated(&b, Point::new(50.0, 25.0), 90.0).unwrap();
        let t = t.unwrap();
        assert_eq!(t.rot, 90.0);
        assert!(t.tx.abs() < 1e-3 && t.ty.abs() < 1e-3);
        // About a point to its left: its center goes round it.
        let (_, _, t) = rotated(&b, Point::new(0.0, 25.0), 180.0).unwrap();
        let t = t.unwrap();
        assert!((t.tx + 100.0).abs() < 1e-3 && t.ty.abs() < 1e-3, "{t:?}");
        assert_eq!(rotation_degrees(0.0, 0.3, true), 15.0);
    }

    #[test]
    fn moves_snap_the_selection_corner_to_the_grid() {
        let mut b = base(Kind::Rect, Geom::Rect(Rect::new(3.0, 7.0, 10.0, 10.0)));
        b.bounds = Rect::new(3.0, 7.0, 10.0, 10.0);
        let (dx, dy) = move_delta(&[b], Point::new(0.0, 0.0), Point::new(25.0, 9.0), true, false);
        assert_eq!((3.0 + dx, 7.0 + dy), (20.0, 20.0));
    }

    #[test]
    fn attribution_names_the_author_and_the_last_change() {
        let mut board = pimble_board::Board::standalone();
        board = board.with_origin("t");
        let sam = Author::new("s", "sam");
        let joe = Author::new("j", "joe");
        let then = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let later = (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        board.create(Stamp::new(Some(&sam), &then)).unwrap();
        let (id, _) = board
            .add_element(Stamp::new(Some(&sam), &then), NewElement::new(Kind::Arrow, Geom::Segment { a: Point::new(0.0, 0.0), b: Point::new(9.0, 9.0) }))
            .unwrap();
        board.edit(Stamp::new(Some(&joe), &later), |t| t.set_transform(&id, &Transform::translate(5.0, 0.0))).into_result().unwrap();
        let snapshot = board.snapshot();
        let e = snapshot.get(&id).unwrap();
        assert_eq!(attribution(e), "Arrow, by sam, 2 hours ago (changed by joe, 5 minutes ago)");
    }
}
