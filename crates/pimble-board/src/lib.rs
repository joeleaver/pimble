//! Boards (docs/IMAGE_MARKUP_CONTRACT.md): a co-edited vector canvas of
//! elements in layers, each stamped with who made it, living in a node
//! document's `data` root. One crate for the three homes a board has (an
//! image node, a whiteboard, the markup on a picture in text), native and
//! wasm32, no UI:
//!
//! - [`Board`] and [`BoardTxn`]: the board document over yrs (contract
//!   section 3). Every write is one transaction and answers an ordinary yrs
//!   update of the node's document, which goes through `applyEdit` like any
//!   other edit.
//! - [`BoardSnapshot`]: the document read once and resolved for drawing
//!   (z-order, members moved with their frames and groups, routed
//!   connectors, derived step numbers, bounds, the visible extent, a spatial
//!   index).
//! - [`render`], [`render_extent`], [`draw_region`]: the renderer, on
//!   tiny-skia, with one bundled font (Noto Sans, SIL OFL 1.1,
//!   `fonts/OFL.txt`).
//! - [`BoardView`]: the raster cache the board view paints from, after
//!   rinch's `paint` example (contract spike 1), with a per-viewer grid.
//! - [`Element::hits`], [`BoardSnapshot::hit_precise`],
//!   [`BoardSnapshot::within`]: hit testing against painted geometry.
//! - [`ViewOptions`]: a viewer's choices (color by author, show everything,
//!   card titles), carried by the snapshot.
//! - [`render_area`], [`render_svg`]: export of a part of a board, as pixels
//!   (PNG, JPEG) or as SVG.

pub mod doc;
mod hit;
pub mod model;
pub mod options;
pub mod order;
mod paint;
pub mod render;
pub mod snapshot;
mod spatial;
pub mod svg;
pub mod text;
pub mod view;

pub use doc::{
    Board, BoardError, BoardTxn, BoardWatch, ChangedElements, Changes, Edited, RawBoard, RawElement, RawLayer, BOARDS_KEY,
    BOARD_KEY,
};
pub use model::*;
pub use paint::pressure_width;
pub use options::{author_color, CardLabel, ViewOptions, UNAUTHORED_COLOR};
pub use render::{
    draw_region, render, render_area, render_extent, selection_bounds, NoPictures, Picture, PictureSource, Rendered,
    RgbaImage, View, MAX_EXTENT_PX,
};
pub use snapshot::{BoardSnapshot, Element, Layer};
pub use svg::render_svg;
pub use view::{grid_color, BoardView};

/// The renderer's pixel types, for callers that keep a pixmap.
pub use tiny_skia;

/// Smooth a stroke's points once, on release (contract section 3): points
/// closer than `min_gap` board units to the last kept one are dropped, then
/// two passes of a three-point average move each inner point toward its
/// neighbours. The ends stay where they are.
pub fn smooth_stroke(points: &[[f32; 3]], min_gap: f32) -> Vec<[f32; 3]> {
    let mut kept: Vec<[f32; 3]> = Vec::with_capacity(points.len());
    for (i, p) in points.iter().enumerate() {
        if !(p[0].is_finite() && p[1].is_finite()) {
            continue;
        }
        let last = i + 1 == points.len();
        match kept.last() {
            Some(q) if !last && ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt() < min_gap => {}
            _ => kept.push(*p),
        }
    }
    for _ in 0..2 {
        if kept.len() < 3 {
            break;
        }
        let prev = kept.clone();
        for i in 1..prev.len() - 1 {
            for c in 0..3 {
                kept[i][c] = prev[i - 1][c] * 0.25 + prev[i][c] * 0.5 + prev[i + 1][c] * 0.25;
            }
        }
    }
    kept
}
