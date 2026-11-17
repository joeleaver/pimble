//! Layers drawn as one picture for a `pixels` operation
//! (docs/DRAWING_CONTRACT.md sections 1 and 3, wave 5): **Flatten Layer**,
//! **Merge Down** onto a paint layer, and **Merge Group** with a paint layer
//! in it. This crate cannot store a blob, so each is two steps for the
//! caller:
//!
//! 1. [`rasterize`] a [`RasterJob`] from a snapshot and the pictures it
//!    shows: a [`LayerRaster`] with the PNG (when anything shows) and where
//!    it lies;
//! 2. put the PNG as a blob of the node's store (the app's `PutBlob`, the
//!    MCP's `put_blob`) and hand its `pimble-blob:` URL to
//!    [`crate::BoardTxn::apply_raster`], which writes the merge or the
//!    flatten in one transaction (one undo).
//!
//! Also here: [`fill_clip`], the area a fill may fill, worked out by the
//! filler's device.

use tiny_skia::{BlendMode, FilterQuality, Pixmap, PixmapPaint, Transform as Affine};

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
/// The most paint pixels a layer drawn as pixels has on a side (4,096
/// board units).
pub const MAX_RASTER_PX: usize = 8192;

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

/// A layer (or group) drawn as one picture, ready for its blob
/// ([`crate::BoardTxn::apply_raster`]).
#[derive(Debug, Clone, PartialEq)]
pub struct LayerRaster {
    pub job: RasterJob,
    /// The picture as PNG (straight alpha); `None` when nothing shows (the
    /// merge or flatten then writes no `pixels` operation).
    pub png: Option<Vec<u8>>,
    /// Where it lies, board units, on the paint pixel grid (its pixels are
    /// exactly [`PX_PER_UNIT`] per unit).
    pub rect: Rect,
    /// Its size in pixels.
    pub pixel_size: (u32, u32),
    /// How the `pixels` operation lays it down: Merge Down keeps the merged
    /// layer's blend here (its opacity is in the picture's alpha).
    pub blend: Blend,
    /// Flatten: the operations it replaces (those painted after it was
    /// drawn stay).
    pub ops: Vec<String>,
    /// Merge Down: the paint layer below, as it was when drawn.
    pub onto: Option<String>,
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

/// Draw what `job` lays down as a picture, from `snapshot` and the
/// `pictures` it shows (every picture it draws must be loaded:
/// [`BoardError::PicturesNotLoaded`] otherwise). Refusals are sentences: a
/// paint layer onto a vector layer, a hidden layer merged (it would merge as
/// nothing), an area larger than [`MAX_RASTER_PX`] pixels on a side.
pub fn rasterize(snapshot: &BoardSnapshot, pictures: &dyn PictureSource, job: RasterJob) -> Result<LayerRaster, BoardError> {
    let id = match &job {
        RasterJob::Flatten(id) | RasterJob::MergeDown(id) | RasterJob::MergeGroup(id) => id.clone(),
    };
    let at = snapshot.layers.iter().position(|l| l.id == id).ok_or_else(|| BoardError::NoLayer(id.clone()))?;
    let tree = snapshot.layer_tree();
    let layer = &snapshot.layers[at];
    let mut out = LayerRaster { job: job.clone(), png: None, rect: Rect::default(), pixel_size: (0, 0), blend: Blend::Normal, ops: Vec::new(), onto: None };
    // The entries drawn, and how.
    let (drawn, alone): (Vec<usize>, bool) = match &job {
        RasterJob::Flatten(_) => {
            if layer.kind != LayerKind::Paint {
                return Err(BoardError::NotAPaintLayer { id, kind: layer.kind.clone() });
            }
            out.ops = snapshot.paint_ops(&layer.id).iter().map(|o| o.raw.id.clone()).collect();
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
                if let Some(b) = snapshot.paint_bounds(&l.id) {
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
    let Some(area) = bounds.and_then(|b| PxBox::of_rect(&b)) else { return Ok(out) };
    if area.w > MAX_RASTER_PX || area.h > MAX_RASTER_PX {
        return Err(BoardError::Invalid(format!(
            "That reaches more than {} units across, too large to draw as pixels.",
            MAX_RASTER_PX as f32 / PX_PER_UNIT
        )));
    }
    let view = View::new(area.x0 as f32 / PX_PER_UNIT, area.y0 as f32 / PX_PER_UNIT, PX_PER_UNIT);
    let mut pixmap = Pixmap::new(area.w as u32, area.h as u32).ok_or_else(|| BoardError::Invalid("too large to draw".into()))?;
    let region = view.board_rect(area.w as u32, area.h as u32);
    let mut complete = true;
    if alone {
        // One layer: its own pixels, its opacity baked in for a merge (its
        // blend goes on the operation), none for a flatten (the layer keeps
        // its opacity).
        let mut buffer = Pixmap::new(area.w as u32, area.h as u32).ok_or_else(|| BoardError::Invalid("too large to draw".into()))?;
        draw_entry(&mut buffer, snapshot, pictures, at, &view, &region, &mut complete);
        let opacity = if matches!(job, RasterJob::Flatten(_)) { 1.0 } else { layer.opacity };
        let paint = PixmapPaint { opacity, blend_mode: BlendMode::SourceOver, quality: FilterQuality::Nearest };
        pixmap.draw_pixmap(0, 0, buffer.as_ref(), &paint, Affine::identity(), None);
    } else {
        for child in &drawn {
            draw_composited(&mut pixmap, snapshot, pictures, *child, &view, &region, &mut complete);
        }
    }
    if !complete {
        return Err(BoardError::PicturesNotLoaded(1));
    }
    let Some((pixmap, (minx, miny))) = trim_transparent(pixmap) else { return Ok(out) };
    out.rect = Rect::new(
        (area.x0 + minx as i64) as f32 / PX_PER_UNIT,
        (area.y0 + miny as i64) as f32 / PX_PER_UNIT,
        pixmap.width() as f32 / PX_PER_UNIT,
        pixmap.height() as f32 / PX_PER_UNIT,
    );
    out.pixel_size = (pixmap.width(), pixmap.height());
    let png = RgbaImage::from_pixmap(pixmap).encode_png().map_err(|e| BoardError::Invalid(format!("the pixels could not be encoded: {e}")))?;
    out.png = Some(png);
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
    let paint = PixmapPaint { opacity: l.opacity, blend_mode: l.blend.to_tiny(), quality: FilterQuality::Nearest };
    pixmap.draw_pixmap(0, 0, buffer.as_ref(), &paint, Affine::identity(), None);
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
