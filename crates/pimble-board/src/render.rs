//! The renderer (contract section 8): a [`BoardSnapshot`], the pictures it
//! names and a view in, pixels out, on tiny-skia. The board view's raster
//! cache ([`crate::BoardView`]), the loader for pictures in text, export, the
//! clipboard and the MCP all draw through [`draw_region`], so every copy of a
//! board agrees with the live view by construction.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use tiny_skia::{BlendMode, FillRule, FilterQuality, Paint, Pattern, Pixmap, PixmapPaint, SpreadMode, Stroke, Transform as Affine};

use crate::model::*;
pub(crate) use crate::paint::stroke_path;
use crate::paint::{element_ops, rect_path, Op, Space, StrokeStyle};
use crate::snapshot::{BoardSnapshot, Element, SHADOW_OFFSET};
/// The most pixels on a side [`render_extent`] renders.
pub const MAX_EXTENT_PX: f32 = 8192.0;

// ── Pictures ────────────────────────────────────────────────────────────

/// A decoded picture, kept premultiplied, with half-size copies made on
/// first need so a large picture drawn small is sampled from a near size
/// (quality, and far fewer pixels touched).
pub struct Picture {
    base: Pixmap,
    levels: OnceLock<Vec<Pixmap>>,
    /// Every pixel opaque: drawn 1:1 it is a row copy.
    opaque: bool,
}

impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Picture({}x{})", self.base.width(), self.base.height())
    }
}

impl Picture {
    /// From straight-alpha RGBA bytes.
    pub fn from_rgba(width: u32, height: u32, mut rgba: Vec<u8>) -> Option<Picture> {
        if rgba.len() != (width as usize) * (height as usize) * 4 {
            return None;
        }
        let mut opaque = true;
        for px in rgba.chunks_exact_mut(4) {
            let a = px[3] as u16;
            if a < 255 {
                opaque = false;
                px[0] = ((px[0] as u16 * a + 127) / 255) as u8;
                px[1] = ((px[1] as u16 * a + 127) / 255) as u8;
                px[2] = ((px[2] as u16 * a + 127) / 255) as u8;
            }
        }
        let base = Pixmap::from_vec(rgba, tiny_skia::IntSize::from_wh(width, height)?)?;
        Some(Picture { base, levels: OnceLock::new(), opaque })
    }

    /// Decode PNG, JPEG, GIF or WebP bytes (a blob's).
    pub fn decode(bytes: &[u8]) -> Result<Picture, image::ImageError> {
        let img = image::load_from_memory(bytes)?.into_rgba8();
        let (w, h) = img.dimensions();
        Picture::from_rgba(w, h, img.into_raw()).ok_or_else(|| {
            image::ImageError::Limits(image::error::LimitError::from_kind(image::error::LimitErrorKind::DimensionError))
        })
    }

    pub fn width(&self) -> u32 {
        self.base.width()
    }

    pub fn height(&self) -> u32 {
        self.base.height()
    }

    /// The copy to sample when one picture pixel lands on `scale` output
    /// pixels, and how many base pixels one of its pixels spans.
    fn level_for(&self, scale: f32) -> (&Pixmap, f32) {
        if scale >= 0.5 {
            return (&self.base, 1.0);
        }
        let levels = self.levels.get_or_init(|| {
            let mut out: Vec<Pixmap> = Vec::new();
            loop {
                let prev = out.last().unwrap_or(&self.base);
                if prev.width() < 32 || prev.height() < 32 {
                    break;
                }
                match halve(prev) {
                    Some(next) => out.push(next),
                    None => break,
                }
            }
            out
        });
        let mut factor = 1.0;
        let mut chosen = &self.base;
        for level in levels {
            if scale * factor * 2.0 > 1.0 {
                break;
            }
            factor *= 2.0;
            chosen = level;
        }
        (chosen, factor)
    }
}

/// A 2x2 box-filtered half-size copy.
fn halve(src: &Pixmap) -> Option<Pixmap> {
    let (w, h) = (src.width() / 2, src.height() / 2);
    let mut out = Pixmap::new(w.max(1), h.max(1))?;
    let s = src.data();
    let sw = src.width() as usize;
    let d = out.data_mut();
    for y in 0..h as usize {
        for x in 0..w as usize {
            let i0 = ((2 * y) * sw + 2 * x) * 4;
            let i1 = i0 + sw * 4;
            let o = (y * w as usize + x) * 4;
            for c in 0..4 {
                let sum = s[i0 + c] as u16 + s[i0 + 4 + c] as u16 + s[i1 + c] as u16 + s[i1 + 4 + c] as u16;
                d[o + c] = ((sum + 2) / 4) as u8;
            }
        }
    }
    Some(out)
}

/// Where the renderer gets a picture's pixels, by the element's `src` (a
/// `pimble-blob:` URL). `None` draws a placeholder.
pub trait PictureSource {
    fn picture(&self, src: &str) -> Option<Arc<Picture>>;
}

impl<F: Fn(&str) -> Option<Arc<Picture>>> PictureSource for F {
    fn picture(&self, src: &str) -> Option<Arc<Picture>> {
        self(src)
    }
}

impl PictureSource for HashMap<String, Arc<Picture>> {
    fn picture(&self, src: &str) -> Option<Arc<Picture>> {
        self.get(src).cloned()
    }
}

/// No pictures: every picture element draws its placeholder.
pub struct NoPictures;

impl PictureSource for NoPictures {
    fn picture(&self, _src: &str) -> Option<Arc<Picture>> {
        None
    }
}

// ── Views and output ────────────────────────────────────────────────────

/// What part of the board a pixel buffer shows: the board point at pixel
/// (0, 0), and pixels per board unit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub x: f32,
    pub y: f32,
    pub scale: f32,
}

impl View {
    pub fn new(x: f32, y: f32, scale: f32) -> Self {
        Self { x, y, scale }
    }

    /// `rect` fitted into `width` x `height` pixels, centered, with
    /// `margin` pixels kept clear on every side.
    pub fn fit(rect: &Rect, width: u32, height: u32, margin: f32) -> View {
        let aw = (width as f32 - 2.0 * margin).max(1.0);
        let ah = (height as f32 - 2.0 * margin).max(1.0);
        let scale = (aw / rect.w.max(1e-3)).min(ah / rect.h.max(1e-3));
        let c = rect.center();
        View { x: c.x - width as f32 / 2.0 / scale, y: c.y - height as f32 / 2.0 / scale, scale }
    }

    /// The board rectangle `width` x `height` pixels show.
    pub fn board_rect(&self, width: u32, height: u32) -> Rect {
        Rect::new(self.x, self.y, width as f32 / self.scale, height as f32 / self.scale)
    }

    /// Board units to pixels.
    pub fn transform(&self) -> Affine {
        Affine::from_row(self.scale, 0.0, 0.0, self.scale, -self.x * self.scale, -self.y * self.scale)
    }

    pub fn to_board(&self, px: f32, py: f32) -> Point {
        Point::new(self.x + px / self.scale, self.y + py / self.scale)
    }

    pub fn to_pixels(&self, p: Point) -> (f32, f32) {
        ((p.x - self.x) * self.scale, (p.y - self.y) * self.scale)
    }
}

/// Straight-alpha RGBA pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl RgbaImage {
    /// From a tiny-skia pixmap (premultiplied), demultiplied.
    pub fn from_pixmap(pixmap: Pixmap) -> RgbaImage {
        let (width, height) = (pixmap.width(), pixmap.height());
        let mut data = pixmap.take();
        for px in data.chunks_exact_mut(4) {
            let a = px[3] as u32;
            if a != 0 && a != 255 {
                for c in &mut px[..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        RgbaImage { width, height, data }
    }

    /// JPEG bytes at `quality` (1 to 100), flattened onto `background`
    /// (JPEG has no transparency).
    pub fn encode_jpeg(&self, quality: u8, background: Color) -> Result<Vec<u8>, image::ImageError> {
        let mut rgb = Vec::with_capacity((self.width * self.height * 3) as usize);
        for px in self.data.chunks_exact(4) {
            let a = px[3] as u32;
            for (c, b) in px[..3].iter().zip([background.r, background.g, background.b]) {
                rgb.push(((*c as u32 * a + b as u32 * (255 - a) + 127) / 255) as u8);
            }
        }
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100)).encode(
            &rgb,
            self.width,
            self.height,
            image::ExtendedColorType::Rgb8,
        )?;
        Ok(out)
    }

    /// PNG bytes (Copy Picture, Save Picture As...).
    pub fn encode_png(&self) -> Result<Vec<u8>, image::ImageError> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::write_buffer_with_format(&mut out, &self.data, self.width, self.height, image::ExtendedColorType::Rgba8, image::ImageFormat::Png)?;
        Ok(out.into_inner())
    }
}

pub(crate) fn tiny_color(c: Color, opacity: f32) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c.r, c.g, c.b, (c.a as f32 * opacity.clamp(0.0, 1.0)).round() as u8)
}

/// Render `view` of the board into a new `width` x `height` pixmap filled
/// with `background` (transparent when `None`).
pub fn render(
    snapshot: &BoardSnapshot,
    pictures: &dyn PictureSource,
    view: &View,
    width: u32,
    height: u32,
    background: Option<Color>,
) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new(width.max(1), height.max(1))?;
    draw_region(&mut pixmap, snapshot, pictures, view);
    // The elements are drawn on a transparent layer and laid over the
    // background, so a highlighter multiplies only what is under it on the
    // board, never the background (which would turn it black on a dark one).
    if let Some(bg) = background {
        let mut out = Pixmap::new(pixmap.width(), pixmap.height())?;
        out.fill(tiny_color(bg, 1.0));
        let over = PixmapPaint { opacity: 1.0, blend_mode: BlendMode::SourceOver, quality: FilterQuality::Nearest };
        out.draw_pixmap(0, 0, pixmap.as_ref(), &over, Affine::identity(), None);
        return Some(out);
    }
    Some(pixmap)
}

/// A board rendered as a picture: what it shows (its page, or its visible
/// extent trimmed to the pixels that are not fully transparent) and where
/// that is on the board.
#[derive(Debug, Clone)]
pub struct Rendered {
    /// The board rectangle the image shows.
    pub rect: Rect,
    pub image: RgbaImage,
}

/// Render the board as a picture at `scale` pixels per board unit
/// (contract section 1, "The visible extent"): a board with a page renders
/// its page on the page's fill; otherwise the visible extent, on
/// transparency, trimmed to the pixels that are not fully transparent. The
/// scale is lowered so no side passes [`MAX_EXTENT_PX`]. `None` when nothing
/// shows.
pub fn render_extent(snapshot: &BoardSnapshot, pictures: &dyn PictureSource, scale: f32) -> Option<Rendered> {
    let rect = snapshot.render_rect()?;
    let background = snapshot.page.as_ref().map(|p| p.fill.unwrap_or(Color::WHITE));
    render_area(snapshot, pictures, rect, scale, background, None, snapshot.page.is_none())
}

/// Render `rect` of the board (export, a selection, a frame) at `scale`
/// pixels per board unit, lowered so no side passes [`MAX_EXTENT_PX`], over
/// `background` (transparent when `None`). `only` draws just those elements
/// and their members (everything whose chain of parents reaches one of
/// them). `trim` cuts the image down to the pixels that are not fully
/// transparent. `None` when the rectangle is empty, or trimming leaves
/// nothing.
pub fn render_area(
    snapshot: &BoardSnapshot,
    pictures: &dyn PictureSource,
    rect: Rect,
    scale: f32,
    background: Option<Color>,
    only: Option<&HashSet<String>>,
    trim: bool,
) -> Option<Rendered> {
    let rect = rect.normalized();
    if !(rect.w > 0.0 && rect.h > 0.0 && rect.is_finite()) {
        return None;
    }
    let scale = scale.min(MAX_EXTENT_PX / rect.w.max(rect.h)).max(1e-4);
    let x0 = (rect.x * scale).floor();
    let y0 = (rect.y * scale).floor();
    let w = ((rect.right() * scale).ceil() - x0).max(1.0) as u32;
    let h = ((rect.bottom() * scale).ceil() - y0).max(1.0) as u32;
    let view = View::new(x0 / scale, y0 / scale, scale);
    let mut pixmap = Pixmap::new(w, h)?;
    if let Some(bg) = background {
        pixmap.fill(tiny_color(bg, 1.0));
    }
    let mask = only.map(|ids| included(snapshot, ids));
    draw_region_masked(&mut pixmap, snapshot, pictures, &view, mask.as_deref());
    if !trim {
        return Some(Rendered { rect: view.board_rect(w, h), image: RgbaImage::from_pixmap(pixmap) });
    }
    let (pixmap, (minx, miny)) = trim_transparent(pixmap)?;
    let rect = Rect::new(
        view.x + minx as f32 / scale,
        view.y + miny as f32 / scale,
        pixmap.width() as f32 / scale,
        pixmap.height() as f32 / scale,
    );
    Some(Rendered { rect, image: RgbaImage::from_pixmap(pixmap) })
}

/// `pixmap` cut down to the pixels that are not fully transparent, and where
/// the cut starts in it. `None` when every pixel is transparent.
fn trim_transparent(pixmap: Pixmap) -> Option<(Pixmap, (u32, u32))> {
    let (w, h) = (pixmap.width(), pixmap.height());
    let data = pixmap.data();
    let (mut minx, mut miny, mut maxx, mut maxy) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for y in 0..h {
        let row = &data[(y * w * 4) as usize..((y + 1) * w * 4) as usize];
        let mut first = None;
        let mut last = None;
        for x in 0..w {
            if row[(x * 4 + 3) as usize] != 0 {
                first.get_or_insert(x);
                last = Some(x);
            }
        }
        if let (Some(f), Some(l)) = (first, last) {
            minx = minx.min(f);
            maxx = maxx.max(l);
            miny = miny.min(y);
            maxy = maxy.max(y);
        }
    }
    if minx == u32::MAX {
        return None;
    }
    let (tw, th) = (maxx - minx + 1, maxy - miny + 1);
    if (tw, th) == (w, h) {
        return Some((pixmap, (0, 0)));
    }
    let cut = pixmap.clone_rect(tiny_skia::IntRect::from_xywh(minx as i32, miny as i32, tw, th)?)?;
    Some((cut, (minx, miny)))
}

/// Per element (z-order), whether it is one of `ids` or a member, at any
/// depth, of one of them.
pub(crate) fn included(snapshot: &BoardSnapshot, ids: &HashSet<String>) -> Vec<bool> {
    snapshot
        .elements
        .iter()
        .map(|e| {
            let mut at = Some(e);
            // Parents never make a cycle (the snapshot cut them); the bound
            // is a guard.
            for _ in 0..=snapshot.elements.len() {
                let Some(cur) = at else { break };
                if ids.contains(cur.id()) {
                    return true;
                }
                at = cur.parent.as_deref().and_then(|p| snapshot.get(p));
            }
            false
        })
        .collect()
}

/// The union of the bounds of the elements `ids` names (that have anything
/// to show); `None` when none do.
pub fn selection_bounds(snapshot: &BoardSnapshot, ids: &[String]) -> Option<Rect> {
    ids.iter()
        .filter_map(|id| snapshot.get(id))
        .filter(|e| e.has_extent && e.bounds.is_finite())
        .map(|e| e.bounds)
        .reduce(|a, b| a.union(&b))
}

/// Draw every visible element `view` touches over what `pixmap` holds. The
/// one drawing path: everything else calls this.
pub fn draw_region(pixmap: &mut Pixmap, snapshot: &BoardSnapshot, pictures: &dyn PictureSource, view: &View) {
    draw_region_masked(pixmap, snapshot, pictures, view, None);
}

/// [`draw_region`], drawing only the elements `mask` (by z-order index)
/// allows when given.
fn draw_region_masked(pixmap: &mut Pixmap, snapshot: &BoardSnapshot, pictures: &dyn PictureSource, view: &View, mask: Option<&[bool]>) {
    let region = view.board_rect(pixmap.width(), pixmap.height());
    let vt = view.transform();
    for i in snapshot.query(&region) {
        if mask.is_some_and(|m| !m[i]) {
            continue;
        }
        draw_element(pixmap, &snapshot.elements[i], pictures, vt, &region);
    }
}

fn paint_of(color: Color, opacity: f32) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color(tiny_color(color, opacity));
    paint.anti_alias = true;
    paint
}

fn stroke_of(width: f32, dash: Dash) -> Stroke {
    StrokeStyle { width, dash }.tiny()
}

/// The scale a transform applies (its area factor's square root).
pub(crate) fn ts_scale(ts: &Affine) -> f32 {
    (ts.sx * ts.sy - ts.kx * ts.ky).abs().sqrt()
}

pub(crate) fn stroke_paint(kind: &Kind, color: Color, opacity: f32) -> Paint<'static> {
    let mut paint = paint_of(color, opacity);
    if matches!(kind, Kind::Highlighter) {
        paint.blend_mode = BlendMode::Multiply;
    }
    paint
}

fn draw_element(pixmap: &mut Pixmap, e: &Element, pictures: &dyn PictureSource, vt: Affine, region: &Rect) {
    let ts = vt.pre_concat(e.world);
    for op in element_ops(e, region, Some(ts_scale(&ts))) {
        match op {
            Op::Fill { path, color, opacity, rule, space, shadow, multiply } => {
                let mut t = if space == Space::Element { ts } else { vt };
                if shadow {
                    t = t.pre_translate(SHADOW_OFFSET, SHADOW_OFFSET);
                }
                let mut paint = paint_of(color, opacity);
                if multiply {
                    paint.blend_mode = BlendMode::Multiply;
                }
                pixmap.fill_path(path.get(), &paint, rule, t, None);
            }
            Op::Stroke { path, color, opacity, stroke, space, multiply } => {
                let t = if space == Space::Element { ts } else { vt };
                let mut paint = paint_of(color, opacity);
                if multiply {
                    paint.blend_mode = BlendMode::Multiply;
                }
                pixmap.stroke_path(path.get(), &paint, &stroke.tiny(), t, None);
            }
            Op::Picture(p) => draw_picture(pixmap, e, p, pictures, ts),
        }
    }
}

fn draw_picture(pixmap: &mut Pixmap, e: &Element, p: &PictureGeom, pictures: &dyn PictureSource, ts: Affine) {
    let r = p.rect.normalized();
    let Some(rect_path) = rect_path(&r) else { return };
    let Some(picture) = pictures.picture(&p.src) else {
        // Not here yet (or not decodable): a placeholder.
        pixmap.fill_path(&rect_path, &paint_of(Color::rgb(0xe0, 0xe0, 0xe0), e.opacity), FillRule::Winding, ts, None);
        pixmap.stroke_path(&rect_path, &paint_of(Color::rgb(0x9e, 0x9e, 0x9e), e.opacity), &stroke_of(1.0 / ts_scale(&ts).max(1e-3), Dash::Solid), ts, None);
        return;
    };
    let crop = p
        .crop
        .map(|c| c.normalized())
        .filter(|c| c.w > 0.0 && c.h > 0.0)
        .unwrap_or(Rect::new(0.0, 0.0, picture.width() as f32, picture.height() as f32));
    // Picture pixels to element space.
    let (kx, ky) = (r.w / crop.w, r.h / crop.h);
    let out_scale = ts_scale(&ts) * (kx * ky).abs().sqrt();
    let (level, factor) = picture.level_for(out_scale);
    let image_ts = Affine::from_row(kx * factor, 0.0, 0.0, ky * factor, r.x - crop.x * kx, r.y - crop.y * ky);
    let full = ts.pre_concat(image_ts);
    // Exactly one pixel per pixel, unrotated, on whole pixels: no sampling.
    let exact = (full.sx - 1.0).abs() < 1e-4
        && (full.sy - 1.0).abs() < 1e-4
        && full.kx == 0.0
        && full.ky == 0.0
        && (full.tx - full.tx.round()).abs() < 1e-3
        && (full.ty - full.ty.round()).abs() < 1e-3;
    if exact && p.crop.is_none() && e.opacity >= 1.0 {
        if picture.opaque && factor == 1.0 {
            copy_opaque(pixmap, level, full.tx.round() as i64, full.ty.round() as i64);
            return;
        }
        let paint = PixmapPaint { opacity: 1.0, blend_mode: BlendMode::SourceOver, quality: FilterQuality::Nearest };
        pixmap.draw_pixmap(full.tx.round() as i32, full.ty.round() as i32, level.as_ref(), &paint, Affine::identity(), None);
        return;
    }
    let quality = if exact { FilterQuality::Nearest } else { FilterQuality::Bilinear };
    let shader = Pattern::new(level.as_ref(), SpreadMode::Pad, quality, e.opacity, image_ts);
    let paint = Paint { shader, anti_alias: true, ..Paint::default() };
    pixmap.fill_path(&rect_path, &paint, FillRule::Winding, ts, None);
}

/// An opaque picture drawn one pixel per pixel at (`x`, `y`): rows copied,
/// clipped to `dst`.
fn copy_opaque(dst: &mut Pixmap, src: &Pixmap, x: i64, y: i64) {
    let (dw, dh) = (dst.width() as i64, dst.height() as i64);
    let (sw, sh) = (src.width() as i64, src.height() as i64);
    let x0 = x.max(0);
    let x1 = (x + sw).min(dw);
    if x1 <= x0 {
        return;
    }
    let s = src.data();
    let d = dst.data_mut();
    for dy in y.max(0)..(y + sh).min(dh) {
        let sy = dy - y;
        let si = ((sy * sw + (x0 - x)) * 4) as usize;
        let di = ((dy * dw + x0) * 4) as usize;
        let n = ((x1 - x0) * 4) as usize;
        d[di..di + n].copy_from_slice(&s[si..si + n]);
    }
}
