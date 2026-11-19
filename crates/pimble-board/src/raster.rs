//! Layers drawn as one picture for a `pixels` operation
//! (docs/DRAWING_CONTRACT.md sections 1 and 3, wave 5): **Flatten Layer**,
//! **Merge Down** onto a paint layer, and **Merge Group** with a paint layer
//! in it. This crate cannot store a blob, so each is two steps for the
//! caller:
//!
//! 1. [`rasterize`] a [`RasterJob`] from a snapshot and the pictures it
//!    shows: a [`LayerRaster`] holding a grid of tiles, each a PNG at most
//!    [`RASTER_TILE_PX`] on a side with where it lies (a layer of any size
//!    is drawn tile by tile, so memory stays bounded; empty tiles are left
//!    out);
//! 2. put each tile's PNG as a blob of the node's store (the app's
//!    `PutBlob`, the MCP's `put_blob`) and hand their `pimble-blob:` URLs,
//!    in tile order, to [`crate::BoardTxn::apply_raster`], which writes the
//!    merge or the flatten, one `pixels` operation per tile, in one
//!    transaction (one undo).
//!
//! Also here: [`fill_clip`], the area a fill may fill, worked out by the
//! filler's device.

use tiny_skia::{BlendMode, FilterQuality, Pixmap, PixmapPaint};

use crate::brush::PX_PER_UNIT;
use crate::doc::BoardError;
use crate::model::{Blend, Geom, Kind, LayerKind, Point, Rect};
use crate::paint_ops::PaintBody;
use crate::render::{draw_element, trim_transparent, PictureSource, RgbaImage, View};
use crate::retouch::PxBox;
use crate::snapshot::BoardSnapshot;

/// The most board units a fill covers on a side (4,096 paint pixels).
pub const MAX_FILL_UNITS: f32 = 2048.0;
/// How far past a paint layer's painting a fill may reach.
pub const FILL_MARGIN: f32 = 64.0;
/// The most pixels one tile of a layer drawn as pixels has on a side
/// (1,024 board units at the paint grid's 2 pixels per unit): 16 MB of
/// pixels while it is drawn, and a PNG under the 20 MiB a blob keeps as it
/// is.
pub const RASTER_TILE_PX: usize = 2048;
/// The most tiles one flatten or merge makes (at 2 pixels per unit, about
/// 16,000 units on a side).
pub const MAX_RASTER_TILES: usize = 256;
/// The least and the most pixels per board unit a layer is drawn as pixels
/// at ([`rasterize_at`]); [`rasterize`] draws at the paint grid's
/// [`PX_PER_UNIT`], the most a paint layer keeps.
pub const MIN_RASTER_SCALE: f32 = 0.125;
pub const MAX_RASTER_SCALE: f32 = PX_PER_UNIT;

/// The area a fill at `seed` in paint layer `layer` may fill, as the
/// filler's device works it out (docs/DRAWING_CONTRACT.md section 3: stored
/// in the operation, so every device fills the same area): what the filler
/// sees (`view`, the visible board rectangle; without one, the canvas's
/// visible extent grown by [`FILL_MARGIN`], or 1,024 units around the seed
/// on an empty canvas), together with the layer's painting grown by
/// [`FILL_MARGIN`] (so a shape partly off screen fills whole), at most
/// [`MAX_FILL_UNITS`] on a side around the seed, on the paint pixel grid.
pub fn fill_clip(snapshot: &BoardSnapshot, layer: &str, seed: Point, view: Option<Rect>) -> Rect {
    let mut r = match view.map(|v| v.normalized()).filter(|v| v.is_finite() && v.w > 0.0 && v.h > 0.0) {
        Some(v) => v,
        None => snapshot.visible_extent().map(|e| e.inflate(FILL_MARGIN)).unwrap_or(Rect::new(seed.x - 512.0, seed.y - 512.0, 1024.0, 1024.0)),
    };
    // The layer's painting (fills aside: each fill's area would grow the next).
    let painted = snapshot
        .paint_ops(layer)
        .iter()
        .filter(|o| !matches!(o.raw.body, PaintBody::Fill(_)))
        .filter_map(|o| o.bounds)
        .reduce(|a, b| a.union(&b));
    if let Some(p) = painted {
        r = r.union(&p.inflate(FILL_MARGIN));
    }
    r = r.union(&Rect::new(seed.x - 1.0, seed.y - 1.0, 2.0, 2.0));
    let fit = |start: f32, len: f32, at: f32| {
        if len <= MAX_FILL_UNITS {
            return (start, len);
        }
        let s = (at - MAX_FILL_UNITS / 2.0).clamp(start, start + len - MAX_FILL_UNITS);
        (s, MAX_FILL_UNITS)
    };
    let (x, w) = fit(r.x, r.w, seed.x);
    let (y, h) = fit(r.y, r.h, seed.y);
    // On whole paint pixels, inside the limit.
    let snap_in = |start: f32, len: f32| {
        let a = (start * PX_PER_UNIT).ceil() / PX_PER_UNIT;
        let b = ((start + len) * PX_PER_UNIT).floor() / PX_PER_UNIT;
        (a, (b - a).max(1.0 / PX_PER_UNIT))
    };
    let (x, w) = snap_in(x, w);
    let (y, h) = snap_in(y, h);
    Rect::new(x, y, w, h)
}

/// What to draw as pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RasterJob {
    /// Flatten Layer: a paint layer's whole history as one picture.
    Flatten(String),
    /// Merge Down of a layer (vector or paint) onto the paint layer below.
    MergeDown(String),
    /// Merge Group of a group holding a paint layer.
    MergeGroup(String),
}

/// One tile of a layer drawn as pixels: one blob, one `pixels` operation.
#[derive(Debug, Clone, PartialEq)]
pub struct RasterTile {
    /// The picture as PNG (straight alpha).
    pub png: Vec<u8>,
    /// Where it lies, board units, on the pixel grid it was drawn at (at
    /// [`rasterize`]'s density its pixels are exactly [`PX_PER_UNIT`] per
    /// unit).
    pub rect: Rect,
    /// Its size in pixels.
    pub pixel_size: (u32, u32),
}

/// A layer (or group) drawn as pixels, ready for its blobs
/// ([`crate::BoardTxn::apply_raster`]).
#[derive(Debug, Clone, PartialEq)]
pub struct LayerRaster {
    pub job: RasterJob,
    /// The tiles that show anything, row by row; none when nothing shows
    /// (the merge or flatten then writes no `pixels` operation).
    pub tiles: Vec<RasterTile>,
    /// How the `pixels` operation lays it down: Merge Down keeps the merged
    /// layer's blend here (its opacity is in the picture's alpha).
    pub blend: Blend,
    /// Flatten: the operations it replaces (those painted after it was
    /// drawn stay).
    pub ops: Vec<String>,
    /// Merge Down: the paint layer below, as it was when drawn.
    pub onto: Option<String>,
    /// Flatten: the layer's blur and feather radii drawn into the pixels
    /// (docs/DRAWING_WAVE6_CONTRACT.md section 3), which the flatten then
    /// takes off the layer; (0, 0) when it had none.
    pub baked: (f32, f32),
}

impl LayerRaster {
    /// The area every tile together covers.
    pub fn rect(&self) -> Option<Rect> {
        self.tiles.iter().map(|t| t.rect).reduce(|a, b| a.union(&b))
    }

    /// The bytes of every tile's PNG.
    pub fn png_bytes(&self) -> usize {
        self.tiles.iter().map(|t| t.png.len()).sum()
    }
}

/// Whether merging `layer` (Merge Down of a layer, Merge Group of a group)
/// draws it as pixels: a layer onto a paint layer, a group holding one.
pub fn merge_needs_pixels(snapshot: &BoardSnapshot, layer: &str) -> bool {
    let Some(at) = snapshot.layers.iter().position(|l| l.id == layer) else { return false };
    let tree = snapshot.layer_tree();
    if tree.entries[at].kind.is_group() {
        return tree.descendants(at).iter().any(|d| tree.entries[*d].kind == LayerKind::Paint);
    }
    let siblings = tree.siblings(at);
    let pos = siblings.iter().position(|s| *s == at).unwrap_or(0);
    pos > 0 && tree.entries[siblings[pos - 1]].kind == LayerKind::Paint
}

/// Draw what `job` lays down as pixels, from `snapshot` and the `pictures`
/// it shows (every picture it draws must be loaded:
/// [`BoardError::PicturesNotLoaded`] otherwise), at the paint grid's
/// [`PX_PER_UNIT`]. Refusals are sentences: a paint layer onto a vector
/// layer, a hidden layer merged (it would merge as nothing), an area of
/// more than [`MAX_RASTER_TILES`] tiles.
pub fn rasterize(snapshot: &BoardSnapshot, pictures: &dyn PictureSource, job: RasterJob) -> Result<LayerRaster, BoardError> {
    rasterize_at(snapshot, pictures, job, PX_PER_UNIT)
}

/// [`rasterize`] at `scale` pixels per board unit (from
/// [`MIN_RASTER_SCALE`] to [`MAX_RASTER_SCALE`]): fewer pixels for a very
/// large layer, drawn stretched over the same area (softer). Tile by tile:
/// at most one tile's pixels (and its PNG) are held at a time.
pub fn rasterize_at(snapshot: &BoardSnapshot, pictures: &dyn PictureSource, job: RasterJob, scale: f32) -> Result<LayerRaster, BoardError> {
    if !(scale.is_finite() && (MIN_RASTER_SCALE..=MAX_RASTER_SCALE).contains(&scale)) {
        return Err(BoardError::Invalid(format!("the density is from {MIN_RASTER_SCALE} to {MAX_RASTER_SCALE} pixels per unit")));
    }
    let id = match &job {
        RasterJob::Flatten(id) | RasterJob::MergeDown(id) | RasterJob::MergeGroup(id) => id.clone(),
    };
    let at = snapshot.layers.iter().position(|l| l.id == id).ok_or_else(|| BoardError::NoLayer(id.clone()))?;
    let tree = snapshot.layer_tree();
    let layer = &snapshot.layers[at];
    let mut out = LayerRaster { job: job.clone(), tiles: Vec::new(), blend: Blend::Normal, ops: Vec::new(), onto: None, baked: (0.0, 0.0) };
    // The entries drawn, and how.
    let (drawn, alone): (Vec<usize>, bool) = match &job {
        RasterJob::Flatten(_) => {
            if layer.kind != LayerKind::Paint {
                return Err(BoardError::NotAPaintLayer { id, kind: layer.kind.clone() });
            }
            out.ops = snapshot.paint_ops(&layer.id).iter().map(|o| o.raw.id.clone()).collect();
            out.baked = (layer.blur, layer.feather);
            (vec![at], true)
        }
        RasterJob::MergeDown(_) => {
            if layer.kind.is_group() {
                return Err(BoardError::Invalid("That is a group. Merge it with Merge Group.".into()));
            }
            let siblings = tree.siblings(at);
            let pos = siblings.iter().position(|s| *s == at).unwrap_or(0);
            let below = pos.checked_sub(1).map(|p| siblings[p]).ok_or_else(|| BoardError::Invalid("the bottom layer has nothing below it".into()))?;
            match (&layer.kind, &tree.entries[below].kind) {
                (_, LayerKind::Paint) => {}
                (LayerKind::Paint, _) => return Err(BoardError::Invalid(crate::doc::PAINT_ONTO_VECTOR.into())),
                (_, k) if k.is_group() => {
                    return Err(BoardError::Invalid("The layer below is a group. Move this layer into it, or merge the group first.".into()))
                }
                _ => return Err(BoardError::Invalid("Two vector layers merge without pixels.".into())),
            }
            if !layer.visible {
                return Err(BoardError::Invalid("Show the layer before merging it down: hidden, it would merge as nothing.".into()));
            }
            out.onto = Some(snapshot.layers[below].id.clone());
            out.blend = if layer.blend == Blend::PassThrough { Blend::Normal } else { layer.blend };
            (vec![at], true)
        }
        RasterJob::MergeGroup(_) => {
            if !layer.kind.is_group() {
                return Err(BoardError::Invalid("That layer is not a group.".into()));
            }
            let inside = tree.descendants(at);
            if !inside.iter().any(|d| tree.entries[*d].kind == LayerKind::Paint) {
                return Err(BoardError::Invalid("That group holds no paint layer: it merges without pixels.".into()));
            }
            if inside.iter().any(|d| !snapshot.layers[*d].visible) {
                return Err(BoardError::Invalid("Show or delete the hidden layers in the group before merging it: hidden, they would merge as nothing.".into()));
            }
            (tree.entries[at].children.clone(), false)
        }
    };
    // Everything drawn, at any depth.
    let mut every: Vec<usize> = Vec::new();
    for d in &drawn {
        every.push(*d);
        every.extend(tree.descendants(*d));
    }
    // Its pictures must be here.
    let mut missing = 0;
    let mut bounds: Option<Rect> = None;
    let mut grow = |r: Rect| {
        if r.is_finite() && r.w >= 0.0 && r.h >= 0.0 {
            bounds = Some(bounds.map_or(r, |b| b.union(&r)));
        }
    };
    for &e in &every {
        let l = &snapshot.layers[e];
        match l.kind {
            LayerKind::Paint => {
                for op in snapshot.paint_ops(&l.id) {
                    if let PaintBody::Pixels(p) = &op.raw.body {
                        if pictures.picture(&p.src).is_none() {
                            missing += 1;
                        }
                    }
                }
                // What a blur or feather spreads is drawn too.
                if let Some(b) = snapshot.paint_reach(e) {
                    grow(b);
                }
            }
            LayerKind::Vector => {
                for el in snapshot.elements.iter().filter(|el| el.layer_index == Some(e) && el.has_extent) {
                    if let Geom::Picture(p) = &el.raw.geom {
                        if pictures.picture(&p.src).is_none() {
                            missing += 1;
                        }
                    }
                    if !matches!(el.raw.kind, Kind::Group) {
                        grow(el.bounds);
                    }
                }
            }
            _ => {}
        }
    }
    if missing > 0 {
        return Err(BoardError::PicturesNotLoaded(missing));
    }
    let Some(bounds) = bounds.filter(|b| b.is_finite()) else { return Ok(out) };
    // The area in pixels at `scale` (at the paint grid's density exactly the
    // paint pixels it touches), cut into tiles.
    let area = if scale == PX_PER_UNIT {
        let Some(px) = PxBox::of_rect(&bounds) else { return Ok(out) };
        (px.x0, px.y0, px.x1(), px.y1())
    } else {
        let clamp = |v: f32| v.clamp(-1e9, 1e9) as i64;
        let (x0, y0) = (clamp((bounds.x * scale).floor()), clamp((bounds.y * scale).floor()));
        (x0, y0, clamp((bounds.right() * scale).ceil()).max(x0 + 1), clamp((bounds.bottom() * scale).ceil()).max(y0 + 1))
    };
    let t = RASTER_TILE_PX as i64;
    let (cols, rows) = (((area.2 - area.0) + t - 1) / t, ((area.3 - area.1) + t - 1) / t);
    if (cols * rows) as usize > MAX_RASTER_TILES {
        return Err(BoardError::Invalid(format!(
            "That reaches {} by {} units, more than one flatten or merge draws ({MAX_RASTER_TILES} tiles of {} units at {} pixels per unit); \
             draw it at fewer pixels per unit, or split it into several layers",
            (bounds.w).round(),
            (bounds.h).round(),
            RASTER_TILE_PX as f32 / scale,
            scale
        )));
    }
    let opacity = if matches!(job, RasterJob::Flatten(_)) || !alone { 1.0 } else { layer.opacity };
    for row in 0..rows {
        for col in 0..cols {
            let (px0, py0) = (area.0 + col * t, area.1 + row * t);
            let (w, h) = ((area.2 - px0).min(t) as u32, (area.3 - py0).min(t) as u32);
            let view = View::new(px0 as f32 / scale, py0 as f32 / scale, scale);
            let region = view.board_rect(w, h);
            let mut pixmap = Pixmap::new(w, h).ok_or_else(|| BoardError::Invalid("too large to draw".into()))?;
            let mut complete = true;
            if alone {
                // One layer: its own pixels, its opacity baked in for a merge
                // (its blend goes on the operation), none for a flatten (the
                // layer keeps its opacity).
                // A merge takes the layer's clip into the pixels (the layer
                // goes); a flatten keeps the layer, and its clip with it.
                // Its blur and feather are drawn in either way (a merge's
                // layer goes; a flatten takes them off the layer).
                let clip = if matches!(job, RasterJob::Flatten(_)) { None } else { snapshot.clip_of(at) };
                let soft = layer.blur > 0.0 || layer.feather > 0.0;
                if opacity >= 1.0 && clip.is_none() && !soft {
                    draw_entry(&mut pixmap, snapshot, pictures, at, &view, &region, &mut complete);
                } else {
                    let mut buffer = Pixmap::new(w, h).ok_or_else(|| BoardError::Invalid("too large to draw".into()))?;
                    draw_entry(&mut buffer, snapshot, pictures, at, &view, &region, &mut complete);
                    if soft {
                        crate::soft::soften_layer(&mut buffer, snapshot, pictures, at, view.transform(), None, true);
                    }
                    let paint = PixmapPaint { opacity, blend_mode: BlendMode::SourceOver, quality: FilterQuality::Nearest };
                    crate::render::blend_clipped(&mut pixmap, &buffer, &paint, clip, view.transform());
                }
            } else {
                for child in &drawn {
                    draw_composited(&mut pixmap, snapshot, pictures, *child, &view, &region, &mut complete);
                }
            }
            if !complete {
                return Err(BoardError::PicturesNotLoaded(1));
            }
            let Some((pixmap, (minx, miny))) = trim_transparent(pixmap) else { continue };
            let rect = Rect::new(
                (px0 + minx as i64) as f32 / scale,
                (py0 + miny as i64) as f32 / scale,
                pixmap.width() as f32 / scale,
                pixmap.height() as f32 / scale,
            );
            let pixel_size = (pixmap.width(), pixmap.height());
            let png = RgbaImage::from_pixmap(pixmap).encode_png().map_err(|e| BoardError::Invalid(format!("the pixels could not be encoded: {e}")))?;
            out.tiles.push(RasterTile { png, rect, pixel_size });
        }
    }
    Ok(out)
}

/// Entry `at` drawn with its own opacity and blend, as the renderer
/// composites it (a group's children through it).
fn draw_composited(pixmap: &mut Pixmap, snapshot: &BoardSnapshot, pictures: &dyn PictureSource, at: usize, view: &View, region: &Rect, complete: &mut bool) {
    let l = &snapshot.layers[at];
    if !l.visible {
        return;
    }
    let isolated = snapshot.layer_tree().entries[at].isolated;
    if !isolated {
        draw_entry(pixmap, snapshot, pictures, at, view, region, complete);
        return;
    }
    let Some(mut buffer) = Pixmap::new(pixmap.width(), pixmap.height()) else { return };
    draw_entry(&mut buffer, snapshot, pictures, at, view, region, complete);
    if l.blur > 0.0 || l.feather > 0.0 {
        crate::soft::soften_layer(&mut buffer, snapshot, pictures, at, view.transform(), None, true);
    }
    let paint = PixmapPaint { opacity: l.opacity, blend_mode: l.blend.to_tiny(), quality: FilterQuality::Nearest };
    crate::render::blend_clipped(pixmap, &buffer, &paint, snapshot.clip_of(at), view.transform());
}

/// What entry `at` holds, drawn straight into `pixmap`: a vector layer's
/// elements, a paint layer's tiles, a group's children composited.
fn draw_entry(pixmap: &mut Pixmap, snapshot: &BoardSnapshot, pictures: &dyn PictureSource, at: usize, view: &View, region: &Rect, complete: &mut bool) {
    let tree = snapshot.layer_tree();
    match tree.entries[at].kind {
        LayerKind::Paint => *complete &= snapshot.paint_cache().draw_layer_checked(pixmap, snapshot, at, view, region, pictures),
        LayerKind::Vector => {
            let vt = view.transform();
            for e in snapshot.elements.iter().filter(|e| e.layer_index == Some(at) && e.has_extent && !matches!(e.raw.kind, Kind::Group)) {
                draw_element(pixmap, e, pictures, vt, region);
            }
        }
        LayerKind::Group => {
            for child in tree.entries[at].children.clone() {
                draw_composited(pixmap, snapshot, pictures, child, view, region, complete);
            }
        }
        LayerKind::Other(_) => {}
    }
}

/// One piece of a picture placed in a paint layer
/// (docs/DRAWING_WAVE6_CONTRACT.md section 5): its own blob, its own
/// `pixels` operation, all the pieces sharing the placement's transform.
#[derive(Debug, Clone, PartialEq)]
pub struct PicturePiece {
    /// The piece encoded: PNG when it has any transparency, else JPEG at
    /// [`PIECE_JPEG_QUALITY`].
    pub bytes: Vec<u8>,
    /// Where it lies before the placement's transform, board units: its
    /// share of the whole picture's rectangle.
    pub rect: Rect,
    /// Its size in pixels.
    pub pixel_size: (u32, u32),
}

/// The quality an opaque piece of a large placed picture is written at.
pub const PIECE_JPEG_QUALITY: u8 = 90;

/// Whether a picture `width` × `height` pixels is placed in pieces: more
/// than [`RASTER_TILE_PX`] on a side, as Flatten tiles a layer.
pub fn needs_pieces(width: u32, height: u32) -> bool {
    width as usize > RASTER_TILE_PX || height as usize > RASTER_TILE_PX
}

/// A large picture placed over `rect` cut into a grid of pieces of at most
/// [`RASTER_TILE_PX`] on a side (row by row), so no blob holds more than
/// one tile's pixels and a view decodes only the pieces it shows, as
/// Flatten's tiles. Refused past [`MAX_RASTER_TILES`] pieces, and for a
/// rectangle with no area.
pub fn picture_pieces(picture: &crate::render::Picture, rect: Rect) -> Result<Vec<PicturePiece>, BoardError> {
    let rect = rect.normalized();
    let (pw, ph) = (picture.width(), picture.height());
    if !(rect.w > 0.0 && rect.h > 0.0 && rect.is_finite()) || pw == 0 || ph == 0 {
        return Err(BoardError::Invalid("pixels need a rectangle with a size".into()));
    }
    let t = RASTER_TILE_PX as u32;
    let (cols, rows) = (pw.div_ceil(t), ph.div_ceil(t));
    if (cols * rows) as usize > MAX_RASTER_TILES {
        return Err(BoardError::Invalid(format!(
            "This picture is {pw} × {ph} pixels, more than {MAX_RASTER_TILES} pieces of {RASTER_TILE_PX} pixels; scale it down first."
        )));
    }
    let (kx, ky) = (rect.w / pw as f32, rect.h / ph as f32);
    let mut out = Vec::with_capacity((cols * rows) as usize);
    for row in 0..rows {
        for col in 0..cols {
            let (x0, y0) = (col * t, row * t);
            let (w, h) = ((pw - x0).min(t), (ph - y0).min(t));
            let area = tiny_skia::IntRect::from_xywh(x0 as i32, y0 as i32, w, h).ok_or_else(|| BoardError::Invalid("a piece with no area".into()))?;
            let piece = picture.pixmap().clone_rect(area).ok_or_else(|| BoardError::Invalid("a piece with no area".into()))?;
            let opaque = piece.data().chunks_exact(4).all(|px| px[3] == 255);
            let image = RgbaImage::from_pixmap(piece);
            let encoded = if opaque { image.encode_jpeg(PIECE_JPEG_QUALITY, crate::model::Color::WHITE) } else { image.encode_png() };
            let bytes = encoded.map_err(|e| BoardError::Invalid(format!("the picture could not be encoded: {e}")))?;
            // The edges are worked out from the whole picture's, so the
            // pieces meet exactly.
            let (left, top) = (rect.x + x0 as f32 * kx, rect.y + y0 as f32 * ky);
            let right = if x0 + w == pw { rect.right() } else { rect.x + (x0 + w) as f32 * kx };
            let bottom = if y0 + h == ph { rect.bottom() } else { rect.y + (y0 + h) as f32 * ky };
            out.push(PicturePiece { bytes, rect: Rect::new(left, top, right - left, bottom - top), pixel_size: (w, h) });
        }
    }
    Ok(out)
}
