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
//! - [`path`]: the `path` element's geometry ([`PathGeom`]) and the pure
//!   helpers the vector tools edit it with (hit an anchor, a handle or a
//!   segment; insert, delete, move; corner and smooth), Bézier fitting for
//!   the Pencil, SVG path data in and out, live shapes as paths.
//! - [`Element::hits`], [`BoardSnapshot::hit_precise`],
//!   [`BoardSnapshot::within`]: hit testing against painted geometry.
//! - [`layer_tree`]: the layers read as a tree (groups, inherited
//!   visibility and locks, blending, draw order), the one resolution
//!   everything else reads.
//! - [`locks`]: what a lock guards, judged by [`BoardTxn`] for every
//!   caller, and the same rules read over a snapshot (which layer new
//!   elements go into, what can be edited).
//! - [`recipe`]: how a new canvas starts, its icons, and the sizes a sticky
//!   note and a node card take when placed with a click.
//! - [`ViewOptions`]: a viewer's choices (color by author, show everything,
//!   card titles), carried by the snapshot.
//! - [`render_area`], [`render_svg`]: export of a part of a board, as pixels
//!   (PNG, JPEG) or as SVG (gradients as `<linearGradient>` and
//!   `<radialGradient>`).
//! - [`combine`]: union, subtract, intersect and exclude over paths
//!   ([`BoardTxn::combine`] is the edit); [`BoardTxn::flip`] mirrors.
//! - [`svg_in`]: SVG markup read into elements and groups
//!   ([`BoardTxn::import_svg`] puts them on the board).
//! - [`Gradient`], [`Paint`]: linear and radial gradients in fills and
//!   strokes ([`BoardTxn::set_fill_paint`], [`BoardTxn::set_stroke_paint`]).
//! - Paint layers (docs/DRAWING_CONTRACT.md section 3): [`paint_ops`] (the
//!   operations as stored), [`brush`] (presets, dabs, the engine), [`tiles`]
//!   (the tile renderer and its cache, [`PaintCache`], a disk hook
//!   [`TileStore`]); [`BoardTxn::begin_stroke`] and friends write them,
//!   [`sample_color`] is the eyedropper. Wave 5: fills, smudges, blurs and
//!   moves of paint selections ([`BoardTxn::add_fill`],
//!   [`BoardTxn::begin_retouch`], [`BoardTxn::add_move`]), and [`raster`]:
//!   Flatten Layer and merges onto paint layers drawn as one picture.

pub mod along;
pub mod brush;
pub mod clip;
pub mod combine;
pub mod doc;
pub mod paint_ops;
pub mod raster;
pub mod scatter;
pub mod soft;
mod retouch;
pub mod tiles;
mod hit;
pub mod layer_tree;
pub mod locks;
pub mod measure;
pub mod model;
pub mod options;
pub mod recipe;
pub use pimble_crdt::order;
mod paint;
pub mod path;
pub mod render;
pub mod snapshot;
mod spatial;
pub mod svg;
pub mod svg_in;
pub mod text;
pub mod view;

pub use doc::{
    PAINT_ONTO_VECTOR, MadeSymbol, Board, BoardError, BoardTxn, BoardWatch, ChangedElements, Changes, Edited, FlipAxis, LockedLayer, RawBoard, RawElement, RawLayer, repivot, text_board_key,
    free_layer_name, HIGHLIGHTS_LAYER_NAME, BARE_HIGHLIGHTS_OPACITY, TEXT_BOARD_PREFIX, BOARD_KEY,
};
pub use brush::{BrushPoint, BrushSettings, Curve, Jitter, Preset, StrokeBlend, PRESETS, PX_PER_UNIT};
pub use paint_ops::{
    FillOp, MoveOp, NewFill, NewRetouch, NewStroke, PaintBody, PaintOp, PixelTransform, PixelsOp, RawPaintOp, Retouch, RetouchOp, Selection, StrokeOp,
};
pub use raster::{fill_clip, merge_needs_pixels, needs_pieces, picture_pieces, rasterize, rasterize_at, LayerRaster, PicturePiece, RasterJob, RasterTile};
pub use tiles::{CacheStats, PaintCache, TileKey, TileStore, TILE_PX, TILE_UNITS};
#[cfg(not(target_arch = "wasm32"))]
pub use tiles::DirTileStore;
pub use layer_tree::{LayerTree, TreeEntry};
pub use locks::Deleted;
pub use recipe::{NewCanvas, StartingPicture};
pub use model::*;
pub use paint::pressure_width;
pub use path::{Anchor, AnchorMode, AnchorRef, HandleRef, HandleSide, PathError, PathGeom, PathRule, SegmentHit, Subpath};
pub use options::{author_color, author_color_index, CardLabel, ViewOptions, UNAUTHORED_COLOR};
pub use render::{
    draw_region, render, render_area, render_area_unclipped, render_extent, render_paint_layer, sample_color, selection_bounds, NoPictures, Picture, PictureSource,
    Rendered, RgbaImage, View, MAX_EXTENT_PX,
};
pub use snapshot::{BoardSnapshot, Element, Layer, PaintMark};
pub use combine::CombineOp;
pub use measure::{Measures, Scale, Unit};
pub use svg::render_svg;
pub use svg_in::{SvgError, SvgImport, SvgItem, SvgPlacement};
pub use view::{grid_color, BoardView, EditOverlay, OverlayPicture};

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
