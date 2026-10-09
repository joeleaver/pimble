//! The renderer (contract section 8): a [`BoardSnapshot`], the pictures it
//! names and a view in, pixels out, on tiny-skia. The board view's raster
//! cache ([`crate::BoardView`]), the loader for pictures in text, export, the
//! clipboard and the MCP all draw through [`draw_region`], so every copy of a
//! board agrees with the live view by construction.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use tiny_skia::{
    BlendMode, FillRule, FilterQuality, LineCap, LineJoin, Paint, Path, PathBuilder, Pattern, Pixmap, PixmapPaint,
    SpreadMode, Stroke, StrokeDash, Transform as Affine,
};

use crate::model::*;
use crate::snapshot::{head_size, BoardSnapshot, Element, SHADOW_OFFSET, STEP_RADIUS};

/// Text smaller than this many pixels is drawn as bars.
const GREEK_BELOW_PX: f32 = 4.0;
/// The most pixels on a side [`render_extent`] renders.
pub const MAX_EXTENT_PX: f32 = 8192.0;

// ── Pictures ────────────────────────────────────────────────────────────

/// A decoded picture, kept premultiplied, with half-size copies made on
/// first need so a large picture drawn small is sampled from a near size
/// (quality, and far fewer pixels touched).
pub struct Picture {
    base: Pixmap,
    levels: OnceLock<Vec<Pixmap>>,
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
        for px in rgba.chunks_exact_mut(4) {
            let a = px[3] as u16;
            if a < 255 {
                px[0] = ((px[0] as u16 * a + 127) / 255) as u8;
                px[1] = ((px[1] as u16 * a + 127) / 255) as u8;
                px[2] = ((px[2] as u16 * a + 127) / 255) as u8;
            }
        }
        let base = Pixmap::from_vec(rgba, tiny_skia::IntSize::from_wh(width, height)?)?;
        Some(Picture { base, levels: OnceLock::new() })
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
    if let Some(bg) = background {
        pixmap.fill(tiny_color(bg, 1.0));
    }
    draw_region(&mut pixmap, snapshot, pictures, view);
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
    if rect.w <= 0.0 || rect.h <= 0.0 {
        return None;
    }
    let scale = scale.min(MAX_EXTENT_PX / rect.w.max(rect.h)).max(1e-4);
    let x0 = (rect.x * scale).floor();
    let y0 = (rect.y * scale).floor();
    let w = ((rect.right() * scale).ceil() - x0).max(1.0) as u32;
    let h = ((rect.bottom() * scale).ceil() - y0).max(1.0) as u32;
    let view = View::new(x0 / scale, y0 / scale, scale);
    let background = snapshot.page.as_ref().map(|p| p.fill.unwrap_or(Color::WHITE));
    let pixmap = render(snapshot, pictures, &view, w, h, background)?;
    if snapshot.page.is_some() {
        return Some(Rendered { rect: view.board_rect(w, h), image: RgbaImage::from_pixmap(pixmap) });
    }
    // Trim to the pixels that are not fully transparent.
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
    let trimmed = if (tw, th) == (w, h) {
        pixmap
    } else {
        pixmap.clone_rect(tiny_skia::IntRect::from_xywh(minx as i32, miny as i32, tw, th)?)?
    };
    let rect = Rect::new(view.x + minx as f32 / scale, view.y + miny as f32 / scale, tw as f32 / scale, th as f32 / scale);
    Some(Rendered { rect, image: RgbaImage::from_pixmap(trimmed) })
}

/// Draw every visible element `view` touches over what `pixmap` holds. The
/// one drawing path: everything else calls this.
pub fn draw_region(pixmap: &mut Pixmap, snapshot: &BoardSnapshot, pictures: &dyn PictureSource, view: &View) {
    let region = view.board_rect(pixmap.width(), pixmap.height());
    let vt = view.transform();
    for i in snapshot.query(&region) {
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
    let mut s = Stroke { width: width.max(0.01), line_cap: LineCap::Round, line_join: LineJoin::Round, ..Stroke::default() };
    match dash {
        Dash::Solid => {}
        Dash::Dashed => {
            s.line_cap = LineCap::Butt;
            s.dash = StrokeDash::new(vec![width * 3.0, width * 2.0], 0.0);
        }
        Dash::Dotted => s.dash = StrokeDash::new(vec![0.01, width * 2.0], 0.0),
    }
    s
}

fn rect_path(r: &Rect) -> Option<Path> {
    tiny_skia::Rect::from_xywh(r.x, r.y, r.w.max(0.01), r.h.max(0.01)).map(PathBuilder::from_rect)
}

fn shape_path(kind: &Kind, r: &Rect) -> Option<Path> {
    match kind {
        Kind::Ellipse => tiny_skia::Rect::from_xywh(r.x, r.y, r.w.max(0.01), r.h.max(0.01)).and_then(PathBuilder::from_oval),
        Kind::Diamond => {
            let c = r.center();
            let mut pb = PathBuilder::new();
            pb.move_to(c.x, r.y);
            pb.line_to(r.right(), c.y);
            pb.line_to(c.x, r.bottom());
            pb.line_to(r.x, c.y);
            pb.close();
            pb.finish()
        }
        _ => rect_path(r),
    }
}

fn draw_shadow(pixmap: &mut Pixmap, path: &Path, ts: Affine, opacity: f32) {
    let paint = paint_of(Color::rgba(0, 0, 0, 56), opacity);
    pixmap.fill_path(path, &paint, FillRule::Winding, ts.pre_translate(SHADOW_OFFSET, SHADOW_OFFSET), None);
}

/// Draw `path` filled and/or stroked per the element's style.
fn fill_and_stroke(pixmap: &mut Pixmap, e: &Element, path: &Path, ts: Affine) {
    let s = &e.style;
    if s.shadow {
        draw_shadow(pixmap, path, ts, e.opacity);
    }
    if let Some(fill) = s.fill {
        pixmap.fill_path(path, &paint_of(fill, e.opacity), FillRule::Winding, ts, None);
    }
    if s.width > 0.0 {
        pixmap.stroke_path(path, &paint_of(s.stroke, e.opacity), &stroke_of(s.width, s.dash), ts, None);
    }
}

fn draw_text(pixmap: &mut Pixmap, e: &Element, ts: Affine, color: Color) {
    let (Some(layout), Some(tb)) = (&e.layout, e.text_box) else { return };
    if layout.is_empty() {
        return;
    }
    let px = layout.font_size * ts_scale(&ts);
    if px < GREEK_BELOW_PX {
        let paint = paint_of(color, e.opacity * 0.35);
        for r in layout.line_boxes(Point::new(tb.x, tb.y), tb.w, e.style.align) {
            if let Some(path) = rect_path(&r) {
                pixmap.fill_path(&path, &paint, FillRule::Winding, ts, None);
            }
        }
        return;
    }
    if let Some(path) = e.text_path() {
        pixmap.fill_path(path, &paint_of(color, e.opacity), FillRule::Winding, ts, None);
    }
}

/// The scale a transform applies (its area factor's square root).
fn ts_scale(ts: &Affine) -> f32 {
    (ts.sx * ts.sy - ts.kx * ts.ky).abs().sqrt()
}

fn arrow_head(tip: Point, from: Point, width: f32) -> Option<(Path, Point)> {
    let (dx, dy) = (tip.x - from.x, tip.y - from.y);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-3 {
        return None;
    }
    let (ux, uy) = (dx / len, dy / len);
    let size = head_size(width).min(len * 0.9);
    let half = size * 0.5;
    let base = Point::new(tip.x - ux * size, tip.y - uy * size);
    let mut pb = PathBuilder::new();
    pb.move_to(tip.x, tip.y);
    pb.line_to(base.x - uy * half, base.y + ux * half);
    pb.line_to(base.x + uy * half, base.y - ux * half);
    pb.close();
    // The line stops inside the head so its round cap does not show.
    let stop = Point::new(tip.x - ux * size * 0.7, tip.y - uy * size * 0.7);
    Some((pb.finish()?, stop))
}

fn draw_segment(pixmap: &mut Pixmap, e: &Element, a: Point, b: Point, ts: Affine) {
    let s = &e.style;
    let paint = paint_of(s.stroke, e.opacity);
    let (mut a2, mut b2) = (a, b);
    if s.arrow_heads.end() {
        if let Some((head, stop)) = arrow_head(b, a, s.width) {
            pixmap.fill_path(&head, &paint, FillRule::Winding, ts, None);
            b2 = stop;
        }
    }
    if s.arrow_heads.start() {
        if let Some((head, stop)) = arrow_head(a, b, s.width) {
            pixmap.fill_path(&head, &paint, FillRule::Winding, ts, None);
            a2 = stop;
        }
    }
    let mut pb = PathBuilder::new();
    pb.move_to(a2.x, a2.y);
    pb.line_to(b2.x, b2.y);
    if let Some(path) = pb.finish() {
        pixmap.stroke_path(&path, &paint, &stroke_of(s.width, s.dash), ts, None);
    }
}

/// A stroke's points as one path (a straight polyline: smoothing happens
/// once, when the drawer releases, contract section 3).
pub(crate) fn stroke_path(points: &[[f32; 3]]) -> Option<Path> {
    let mut it = points.iter().filter(|p| p[0].is_finite() && p[1].is_finite());
    let first = it.next()?;
    let mut pb = PathBuilder::new();
    pb.move_to(first[0], first[1]);
    let mut any = false;
    for p in it {
        pb.line_to(p[0], p[1]);
        any = true;
    }
    if !any {
        // A dot: a zero-length segment, which a round cap draws as a disc.
        pb.line_to(first[0] + 0.01, first[1]);
    }
    pb.finish()
}

pub(crate) fn stroke_paint(kind: &Kind, color: Color, opacity: f32) -> Paint<'static> {
    let mut paint = paint_of(color, opacity);
    if matches!(kind, Kind::Highlighter) {
        paint.blend_mode = BlendMode::Multiply;
    }
    paint
}

fn draw_element(pixmap: &mut Pixmap, e: &Element, pictures: &dyn PictureSource, vt: Affine, region: &Rect) {
    if !e.visible || e.opacity <= 0.0 {
        return;
    }
    let ts = vt.pre_concat(e.world);
    let s = &e.style;
    match (&e.raw.kind, &e.raw.geom) {
        (Kind::Pen | Kind::Highlighter, _) => {
            if let Some(path) = stroke_path(&e.raw.points) {
                let paint = stroke_paint(&e.raw.kind, s.stroke, e.opacity);
                pixmap.stroke_path(&path, &paint, &stroke_of(s.width.max(0.5), s.dash), ts, None);
            }
        }
        (Kind::Picture, Geom::Picture(p)) => draw_picture(pixmap, e, p, pictures, ts),
        (Kind::Line | Kind::Arrow, Geom::Segment { a, b }) => draw_segment(pixmap, e, *a, *b, ts),
        (Kind::Rect | Kind::Ellipse | Kind::Diamond, Geom::Rect(r)) => {
            if let Some(path) = shape_path(&e.raw.kind, &r.normalized()) {
                fill_and_stroke(pixmap, e, &path, ts);
            }
            draw_text(pixmap, e, ts, s.stroke);
        }
        (Kind::Text, Geom::Rect(_)) => {
            if let Some(fill) = s.fill {
                if let Some(path) = e.text_box.and_then(|tb| rect_path(&tb.inflate(s.font_size * 0.2))) {
                    pixmap.fill_path(&path, &paint_of(fill, e.opacity), FillRule::Winding, ts, None);
                }
            }
            draw_text(pixmap, e, ts, s.stroke);
        }
        (Kind::Sticky, Geom::Rect(r)) => {
            let g = e.grown.unwrap_or(*r).normalized();
            if let Some(path) = rect_path(&g) {
                if s.shadow {
                    draw_shadow(pixmap, &path, ts, e.opacity);
                }
                let fill = s.fill.unwrap_or(STICKY_YELLOW);
                pixmap.fill_path(&path, &paint_of(fill, e.opacity), FillRule::Winding, ts, None);
                if s.width > 0.0 {
                    pixmap.stroke_path(&path, &paint_of(s.stroke, e.opacity), &stroke_of(s.width, s.dash), ts, None);
                }
            }
            draw_text(pixmap, e, ts, s.stroke);
        }
        (Kind::Step, Geom::Anchor(c)) => {
            let rad = s.font_size * STEP_RADIUS;
            if let Some(path) = PathBuilder::from_circle(c.x, c.y, rad) {
                if s.shadow {
                    draw_shadow(pixmap, &path, ts, e.opacity);
                }
                pixmap.fill_path(&path, &paint_of(s.stroke, e.opacity), FillRule::Winding, ts, None);
            }
            draw_text(pixmap, e, ts, s.fill.unwrap_or(Color::WHITE));
        }
        (Kind::Callout, Geom::Callout { rect, tip }) => {
            let r = rect.normalized();
            // The pointer leaves from the box's center toward the tip.
            let c = r.center();
            let (dx, dy) = (tip.x - c.x, tip.y - c.y);
            let len = (dx * dx + dy * dy).sqrt().max(1e-3);
            let half = (r.w.min(r.h) * 0.2).max(4.0);
            let mut pb = PathBuilder::new();
            pb.push_rect(tiny_skia::Rect::from_xywh(r.x, r.y, r.w.max(0.01), r.h.max(0.01)).unwrap_or(tiny_skia::Rect::from_xywh(0.0, 0.0, 1.0, 1.0).unwrap()));
            pb.move_to(c.x - dy / len * half, c.y + dx / len * half);
            pb.line_to(tip.x, tip.y);
            pb.line_to(c.x + dy / len * half, c.y - dx / len * half);
            pb.close();
            if let Some(path) = pb.finish() {
                if s.shadow {
                    draw_shadow(pixmap, &path, ts, e.opacity);
                }
                // Stroke first, fill over it: the outline shows only outside
                // the union of the box and the pointer.
                if s.width > 0.0 {
                    pixmap.stroke_path(&path, &paint_of(s.stroke, e.opacity), &stroke_of(s.width * 2.0, Dash::Solid), ts, None);
                }
                if let Some(fill) = s.fill {
                    pixmap.fill_path(&path, &paint_of(fill, 1.0_f32.min(e.opacity)), FillRule::Winding, ts, None);
                }
            }
            draw_text(pixmap, e, ts, Color::rgb(0x21, 0x21, 0x21));
        }
        (Kind::Connector, _) => {
            if let Some((a, b)) = e.route {
                draw_segment(pixmap, e, a, b, vt);
                if let (Some(layout), Some(tb)) = (&e.layout, e.text_box) {
                    if !layout.is_empty() {
                        if let Some(bg) = rect_path(&tb.inflate(s.font_size * 0.25)) {
                            pixmap.fill_path(&bg, &paint_of(Color::WHITE, e.opacity), FillRule::Winding, vt, None);
                        }
                        draw_text(pixmap, e, vt, s.stroke);
                    }
                }
            }
        }
        (Kind::Frame, Geom::Rect(r)) => {
            if let Some(path) = rect_path(&r.normalized()) {
                if let Some(fill) = s.fill {
                    pixmap.fill_path(&path, &paint_of(fill, e.opacity), FillRule::Winding, ts, None);
                }
                pixmap.stroke_path(&path, &paint_of(s.stroke, e.opacity), &stroke_of(s.width.max(0.5), s.dash), ts, None);
            }
            draw_text(pixmap, e, ts, Color::rgb(0x61, 0x61, 0x61));
        }
        (Kind::Card, Geom::Card { rect, .. }) => {
            let r = rect.normalized();
            if let Some(path) = rect_path(&r) {
                fill_and_stroke(pixmap, e, &path, ts);
            }
            // A small square where the node's icon goes.
            let pad = s.font_size * 0.6;
            let icon = s.font_size * 1.2;
            if let Some(path) = rect_path(&Rect::new(r.x + pad, r.y + (r.h - icon) / 2.0, icon, icon)) {
                pixmap.fill_path(&path, &paint_of(s.stroke, e.opacity * 0.5), FillRule::Winding, ts, None);
            }
            draw_text(pixmap, e, ts, Color::rgb(0x21, 0x21, 0x21));
        }
        (Kind::Spotlight, Geom::Rect(r)) => {
            // Everything outside the rectangle is dimmed, across the region.
            let mut pb = PathBuilder::new();
            if let Some(outer) = tiny_skia::Rect::from_xywh(region.x - 1.0, region.y - 1.0, region.w + 2.0, region.h + 2.0) {
                pb.push_rect(outer);
            }
            if let Some(inner) = rect_path(&r.normalized()).and_then(|p| p.transform(e.world)) {
                pb.push_path(&inner);
            }
            if let Some(path) = pb.finish() {
                let fill = s.fill.unwrap_or(Color::rgba(0, 0, 0, 0x99));
                pixmap.fill_path(&path, &paint_of(fill, e.opacity), FillRule::EvenOdd, vt, None);
            }
        }
        _ => {}
    }
}

fn draw_picture(pixmap: &mut Pixmap, e: &Element, p: &PictureGeom, pictures: &dyn PictureSource, ts: Affine) {
    let r = p.rect.normalized();
    let Some(rect_path) = rect_path(&r) else { return };
    if e.style.shadow {
        draw_shadow(pixmap, &rect_path, ts, e.opacity);
    }
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
        let paint = PixmapPaint { opacity: 1.0, blend_mode: BlendMode::SourceOver, quality: FilterQuality::Nearest };
        pixmap.draw_pixmap(full.tx.round() as i32, full.ty.round() as i32, level.as_ref(), &paint, Affine::identity(), None);
        return;
    }
    let quality = if exact { FilterQuality::Nearest } else { FilterQuality::Bilinear };
    let shader = Pattern::new(level.as_ref(), SpreadMode::Pad, quality, e.opacity, image_ts);
    let paint = Paint { shader, anti_alias: true, ..Paint::default() };
    pixmap.fill_path(&rect_path, &paint, FillRule::Winding, ts, None);
}
