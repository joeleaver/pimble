//! One divider slot: the line between two panes, and dragging it.

use rinch::prelude::*;

use crate::panes::Direction;
use crate::state::AppStore;

use super::PRESS_HEARD;

/// One divider slot: the line of split `index`, when the tiling has that
/// many splits.
///
/// The element that hears the press covers the whole space its split
/// divides and lets every press through but the ones on the line inside it,
/// so the press's element bounds are that space in pixels on both backends:
/// the drag reads its ratio straight off them, and the area's size (for the
/// least a pane may be) follows from the space's share of it.
pub(super) fn render_divider(__scope: &mut RenderScope, store: AppStore, index: usize) -> NodeHandle {
    let divider = move || store.tiling.with(|tiling| tiling.dividers().into_iter().find(|d| d.index == index));
    let ratio = move |direction: Direction, space: crate::panes::Rect, line: crate::panes::Rect| match direction {
        Direction::Right => (line.x - space.x) / space.w,
        Direction::Down => (line.y - space.y) / space.h,
    };
    rsx! {
        div {
            class: "pimble-pane-split",
            style: {move || match divider() {
                Some(d) => format!(
                    "left: {:.4}%; top: {:.4}%; width: {:.4}%; height: {:.4}%;",
                    d.space.x * 100.0,
                    d.space.y * 100.0,
                    d.space.w * 100.0,
                    d.space.h * 100.0,
                ),
                None => "display: none;".to_string(),
            }},
            onmousedown: move || {
                PRESS_HEARD.with(|heard| heard.set(true));
                let Some(d) = untracked(divider) else { return };
                let ctx = rinch::core::get_click_context();
                if ctx.element_width <= 0.0 || ctx.element_height <= 0.0 {
                    return;
                }
                let (area_width, area_height) = (ctx.element_width / d.space.w, ctx.element_height / d.space.h);
                // Where on the line it was grabbed: the line does not jump
                // to the pointer.
                let start = ratio(d.direction, d.space, d.rect);
                let grabbed = match d.direction {
                    Direction::Right => (ctx.mouse_x - ctx.element_x) / ctx.element_width,
                    Direction::Down => (ctx.mouse_y - ctx.element_y) / ctx.element_height,
                } - start;
                let direction = d.direction;
                let drag_to = move |px: f32, py: f32| {
                    let at = match direction {
                        Direction::Right => px,
                        Direction::Down => py,
                    } - grabbed;
                    let mut tiling = untracked(|| store.tiling.get());
                    if tiling.drag_ratio(index, at, area_width, area_height) && tiling != untracked(|| store.tiling.get()) {
                        store.tiling.set(tiling);
                    }
                };
                // The layout is saved when the drag ends, not on every move
                // (docs/SPLIT_VIEW_CONTRACT.md "Persistence").
                store.pane_dragging.set(true);
                rinch::core::Drag::percent()
                    .on_move(drag_to)
                    // The last move left the line where it was let go; the
                    // release itself says nothing more (rinch hands `on_end`
                    // of a percent drag the raw pointer position, not a
                    // fraction: rinch-core/src/events/drag.rs `finish_drag`).
                    .on_end(move |_, _| store.pane_dragging.set(false))
                    .on_cancel(move |_, _| store.pane_dragging.set(false))
                    .start();
            },
            div {
                class: {move || match divider().map(|d| d.direction) {
                    Some(Direction::Down) => "pimble-pane-divider pimble-pane-divider--down",
                    _ => "pimble-pane-divider pimble-pane-divider--right",
                }},
                style: {move || match divider() {
                    Some(d) => {
                        let at = ratio(d.direction, d.space, d.rect) * 100.0;
                        match d.direction {
                            Direction::Right => format!("left: {at:.4}%;"),
                            Direction::Down => format!("top: {at:.4}%;"),
                        }
                    }
                    None => String::new(),
                }},
            }
        }
    }
}
