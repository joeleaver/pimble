//! The board view's raster cache (contract "Spikes before building", 1),
//! after rinch's `paint` example: one persistent raster of the board at the
//! current zoom, drawn into incrementally, and frames that are only a copy
//! (or, while a zoom has not settled, a resample) of it.
//!
//! - **The cache** covers the view plus a margin, at `cache_zoom` pixels per
//!   board unit, background filled. A remote edit or a selection change
//!   re-renders only the board rectangles it touched
//!   ([`BoardView::set_snapshot`] compares element fingerprints); panning
//!   past the margin shifts the cache and renders the strips uncovered.
//! - **A stroke being drawn** is stamped into the cache segment by segment
//!   ([`BoardView::extend_stroke`]), never a re-render per pointer move; once
//!   the stroke is in the document, its rectangle is re-rendered from it.
//! - **A zoom** scales the cache until [`BoardView::settle`] re-renders it
//!   at the new zoom (the app calls it when the wheel or pinch stops).
//! - **Frames** are exactly the requested physical size and opaque, the
//!   shape rinch's `RenderSurface::submit_frame` handles fastest (rinch
//!   #361): row copies, no premultiply. [`BoardView::frame`] answers `None`
//!   when nothing changed since the last frame, like paint's dirty flag.
//!
//! Nothing here knows rinch: the app hands `frame()` to a `SurfaceWriter`.

use std::collections::HashMap;

use tiny_skia::{BlendMode, FilterQuality, Pixmap, PixmapPaint, Transform as Affine};

use crate::model::*;
use crate::options::ViewOptions;
use crate::render::{draw_region, stroke_paint, stroke_path, tiny_color, PictureSource, View};
use crate::doc::{Board, Changes};
use crate::snapshot::{id_hash, BoardSnapshot};

/// The cache's margin around the view, as a fraction of the view's size.
const MARGIN: f32 = 0.25;
/// When the dirty area passes this share of the cache, re-render it whole.
const FULL_RERENDER_SHARE: f32 = 0.5;

struct Cache {
    pixmap: Pixmap,
    /// The board point at the cache's pixel (0, 0).
    origin: Point,
    zoom: f32,
}

impl Cache {
    fn board_rect(&self) -> Rect {
        Rect::new(self.origin.x, self.origin.y, self.pixmap.width() as f32 / self.zoom, self.pixmap.height() as f32 / self.zoom)
    }

    fn view(&self) -> View {
        View::new(self.origin.x, self.origin.y, self.zoom)
    }
}

/// A stroke being drawn by this person, not yet in the document.
struct LiveStroke {
    kind: Kind,
    style: ResolvedStyle,
    points: Vec<[f32; 3]>,
}

/// See the module doc.
pub struct BoardView {
    snapshot: BoardSnapshot,
    background: Color,
    width: u32,
    height: u32,
    /// The board point at the view's center.
    center: Point,
    /// Physical pixels per board unit.
    zoom: f32,
    cache: Option<Cache>,
    /// Board rectangles to re-render in the cache.
    dirty: Vec<Rect>,
    frame: Pixmap,
    frame_dirty: bool,
    live: Option<LiveStroke>,
    /// Per element (by a hash of its id): (fingerprint, bounds, global),
    /// from the last snapshot.
    seen: HashMap<u64, (u64, Rect, bool)>,
    /// Grid spacing in board units, when this viewer shows a grid.
    grid: Option<f32>,
}

impl BoardView {
    /// A view `width` x `height` physical pixels, showing board point (0, 0)
    /// at its center at 1:1, over `background`.
    pub fn new(width: u32, height: u32, background: Color) -> Self {
        let (width, height) = (width.max(1), height.max(1));
        BoardView {
            snapshot: BoardSnapshot::empty(),
            background: Color { a: 255, ..background },
            width,
            height,
            center: Point::default(),
            zoom: 1.0,
            cache: None,
            dirty: Vec::new(),
            frame: Pixmap::new(width, height).expect("a frame of at least one pixel"),
            frame_dirty: true,
            live: None,
            seen: HashMap::new(),
            grid: None,
        }
    }

    pub fn snapshot(&self) -> &BoardSnapshot {
        &self.snapshot
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The surface's physical size changed (rinch `layout_size`).
    pub fn resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) == (self.width, self.height) {
            return;
        }
        self.width = width;
        self.height = height;
        self.frame = Pixmap::new(width, height).expect("a frame of at least one pixel");
        self.cache = None;
        self.frame_dirty = true;
    }

    pub fn set_background(&mut self, background: Color) {
        self.background = Color { a: 255, ..background };
        self.invalidate_all();
    }

    /// Show a grid every `spacing` board units under the elements (this
    /// viewer's choice, not the document's), or none. Zoomed out so far that
    /// lines would be closer than 8 pixels, the spacing is multiplied by 5
    /// until they are not.
    pub fn set_grid(&mut self, spacing: Option<f32>) {
        let spacing = spacing.filter(|s| s.is_finite() && *s > 0.0);
        if spacing != self.grid {
            self.grid = spacing;
            self.invalidate_all();
        }
    }

    pub fn grid(&self) -> Option<f32> {
        self.grid
    }

    /// Draw the board with other view options ([`ViewOptions`]): the
    /// snapshot is read again from `board` with them, and what changed is
    /// re-rendered. [`BoardView::update`] keeps them.
    pub fn set_options(&mut self, options: ViewOptions, board: &Board) {
        let old = std::mem::replace(&mut self.snapshot, BoardSnapshot::empty());
        let global = old.options().color_by_author != options.color_by_author || old.options().show_all != options.show_all;
        let next = old.with_options(options, board);
        self.set_snapshot(next);
        if global {
            self.invalidate_all();
        }
    }

    /// The background of a cache region: the background color, and the grid
    /// when one shows.
    fn fill_background(&self, pixmap: &mut Pixmap, view: &View) {
        pixmap.fill(tiny_color(self.background, 1.0));
        if let Some(spacing) = self.grid {
            draw_grid(pixmap, view, spacing, grid_color(self.background));
        }
    }

    // ── The camera ───────────────────────────────────────────────────────

    pub fn center(&self) -> Point {
        self.center
    }

    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// What the frame shows, as a renderer view: its corner on a whole
    /// pixel at the current zoom, so a pan is a whole-pixel offset into the
    /// cache and the frame is exactly this view.
    pub fn view(&self) -> View {
        let z = self.zoom;
        let x = self.center.x - self.width as f32 / 2.0 / z;
        let y = self.center.y - self.height as f32 / 2.0 / z;
        View::new((x * z).round() / z, (y * z).round() / z, z)
    }

    pub fn set_camera(&mut self, center: Point, zoom: f32) {
        self.center = center;
        self.zoom = zoom.clamp(1e-3, 1e3);
        self.frame_dirty = true;
    }

    /// Pan by a drag of (`dx`, `dy`) physical pixels.
    pub fn pan_by(&mut self, dx: f32, dy: f32) {
        self.center = Point::new(self.center.x - dx / self.zoom, self.center.y - dy / self.zoom);
        self.frame_dirty = true;
    }

    /// Zoom by `factor` keeping the board point under physical pixel
    /// (`sx`, `sy`) where it is (wheel zoom about the cursor).
    pub fn zoom_at(&mut self, sx: f32, sy: f32, factor: f32) {
        let anchor = self.screen_to_board(sx, sy);
        let zoom = (self.zoom * factor).clamp(1e-3, 1e3);
        self.zoom = zoom;
        self.center = Point::new(
            anchor.x - (sx - self.width as f32 / 2.0) / zoom,
            anchor.y - (sy - self.height as f32 / 2.0) / zoom,
        );
        self.frame_dirty = true;
    }

    /// Fit `rect` with `margin` physical pixels clear on every side.
    pub fn fit(&mut self, rect: &Rect, margin: f32) {
        let v = View::fit(rect, self.width, self.height, margin);
        self.set_camera(rect.center(), v.scale);
    }

    pub fn screen_to_board(&self, sx: f32, sy: f32) -> Point {
        self.view().to_board(sx, sy)
    }

    pub fn board_to_screen(&self, p: Point) -> (f32, f32) {
        self.view().to_pixels(p)
    }

    /// Whether the cache is at another zoom than the view (a zoom has not
    /// settled).
    pub fn needs_settle(&self) -> bool {
        self.cache.as_ref().is_some_and(|c| c.zoom != self.zoom)
    }

    // ── The document ─────────────────────────────────────────────────────

    /// A new snapshot of the board (after a local or remote edit). Only
    /// what changed is re-rendered: the old and new bounds of every element
    /// whose fingerprint changed, appeared or went.
    pub fn set_snapshot(&mut self, snapshot: BoardSnapshot) {
        let mut next: HashMap<u64, (u64, Rect, bool)> = HashMap::with_capacity(snapshot.elements.len());
        let mut dirty: Vec<Rect> = Vec::new();
        let mut everything = snapshot.markup_visible != self.snapshot.markup_visible || snapshot.page != self.snapshot.page;
        for e in &snapshot.elements {
            let entry = (e.fingerprint, e.bounds, e.global);
            let key = id_hash(&e.raw.id);
            match self.seen.get(&key) {
                Some(old) if old.0 == entry.0 => {}
                Some(old) => {
                    everything |= old.2 || entry.2;
                    dirty.push(old.1);
                    dirty.push(entry.1);
                }
                None => {
                    everything |= entry.2;
                    dirty.push(entry.1);
                }
            }
            next.insert(key, entry);
        }
        for (key, old) in &self.seen {
            if !next.contains_key(key) {
                everything |= old.2;
                dirty.push(old.1);
            }
        }
        self.seen = next;
        self.snapshot = snapshot;
        if everything {
            self.invalidate_all();
        } else {
            for r in dirty {
                self.invalidate(r);
            }
        }
    }

    /// Bring the snapshot up to date with `board` from what a
    /// [`Board::watch`] saw change, and re-render what that changed. The
    /// cheap path for every edit: the elements that did not change move
    /// into the new snapshot instead of being read, laid out and dropped.
    pub fn update(&mut self, board: &Board, changes: &Changes) {
        if changes.is_empty() && self.snapshot.exists {
            return;
        }
        let old = std::mem::replace(&mut self.snapshot, BoardSnapshot::empty());
        let next = old.update(board, changes);
        self.set_snapshot(next);
    }

    /// Re-render `rect` (board units) at the next frame.
    pub fn invalidate(&mut self, rect: Rect) {
        if rect.is_finite() && rect.w >= 0.0 && rect.h >= 0.0 {
            self.dirty.push(rect.inflate(1.0));
            self.frame_dirty = true;
        }
    }

    /// Re-render everything at the next frame.
    pub fn invalidate_all(&mut self) {
        self.cache = None;
        self.dirty.clear();
        self.frame_dirty = true;
    }

    // ── A stroke being drawn ─────────────────────────────────────────────

    /// Start a pen or highlighter stroke at `point` (board units, with
    /// pressure).
    pub fn begin_stroke(&mut self, kind: Kind, style: &Style, point: [f32; 3]) {
        let style = style.resolved(&kind);
        self.live = Some(LiveStroke { kind, style, points: Vec::new() });
        self.extend_stroke(&[point]);
    }

    /// Add points to the stroke being drawn and stamp only the new segments
    /// into the cache.
    pub fn extend_stroke(&mut self, points: &[[f32; 3]]) {
        let Some(live) = self.live.as_mut() else { return };
        let start = live.points.len().saturating_sub(1);
        live.points.extend_from_slice(points);
        let segment = &live.points[start..];
        if let Some(cache) = self.cache.as_mut() {
            let view = cache.view();
            // The cache has the background baked in, so a highlighter's
            // multiply would darken empty space here: while drawing it is laid
            // over plainly, and the element's own render settles it.
            stamp(&mut cache.pixmap, &view, &live.kind, &live.style, segment, false);
        }
        self.frame_dirty = true;
    }

    /// The stroke ends: answers its points, for the element the app writes.
    /// Its stamps stay in the cache until the element's own rendering
    /// replaces them.
    pub fn end_stroke(&mut self) -> Vec<[f32; 3]> {
        self.live.take().map(|l| l.points).unwrap_or_default()
    }

    /// Drop the stroke being drawn and its stamps.
    pub fn cancel_stroke(&mut self) {
        if let Some(live) = self.live.take() {
            if let Some(path) = stroke_path(&live.points) {
                let b = path.bounds();
                self.invalidate(Rect::new(b.x(), b.y(), b.width(), b.height()).inflate(live.style.width));
            }
        }
    }

    // ── Rendering ────────────────────────────────────────────────────────

    /// Re-render the cache at the current zoom when a zoom has not settled.
    /// Answers whether it did anything.
    pub fn settle(&mut self, pictures: &dyn PictureSource) -> bool {
        if !self.needs_settle() {
            return false;
        }
        self.rebuild_cache(pictures);
        self.frame_dirty = true;
        true
    }

    /// The next frame, `width * height * 4` bytes of opaque RGBA, or `None`
    /// when nothing changed since the last one.
    pub fn frame(&mut self, pictures: &dyn PictureSource) -> Option<&[u8]> {
        if !self.frame_dirty {
            return None;
        }
        self.frame_dirty = false;
        self.compose(pictures);
        Some(self.frame.data())
    }

    /// The current frame's bytes, whether or not anything changed.
    pub fn current_frame(&mut self, pictures: &dyn PictureSource) -> &[u8] {
        if self.frame_dirty {
            self.frame_dirty = false;
            self.compose(pictures);
        }
        self.frame.data()
    }

    fn cache_rect_for_view(&self) -> (Point, u32, u32) {
        let mx = (self.width as f32 * MARGIN).ceil().max(64.0);
        let my = (self.height as f32 * MARGIN).ceil().max(64.0);
        let w = self.width + 2 * mx as u32;
        let h = self.height + 2 * my as u32;
        let v = self.view();
        // Snapped to whole cache pixels, so a pan is a whole-pixel offset.
        let ox = ((v.x * self.zoom).round() - mx) / self.zoom;
        let oy = ((v.y * self.zoom).round() - my) / self.zoom;
        (Point::new(ox, oy), w, h)
    }

    /// Render the board into `pixmap` (the background filled first). The
    /// elements are drawn on a transparent layer and laid over the
    /// background, so a highlighter multiplies only what is under it on the
    /// board and shows as its own color over empty space, never darkened by
    /// the view's background (a dark theme turned it black).
    fn render_region(&self, pixmap: &mut Pixmap, view: &View, pictures: &dyn PictureSource) {
        self.fill_background(pixmap, view);
        let Some(mut content) = Pixmap::new(pixmap.width(), pixmap.height()) else { return };
        draw_region(&mut content, &self.snapshot, pictures, view);
        if let Some(live) = &self.live {
            stamp(&mut content, view, &live.kind, &live.style, &live.points, true);
        }
        let over = PixmapPaint { opacity: 1.0, blend_mode: BlendMode::SourceOver, quality: FilterQuality::Nearest };
        pixmap.draw_pixmap(0, 0, content.as_ref(), &over, Affine::identity(), None);
    }

    fn rebuild_cache(&mut self, pictures: &dyn PictureSource) {
        let (origin, w, h) = self.cache_rect_for_view();
        let mut pixmap = Pixmap::new(w, h).expect("cache size");
        let view = View::new(origin.x, origin.y, self.zoom);
        self.render_region(&mut pixmap, &view, pictures);
        self.cache = Some(Cache { pixmap, origin, zoom: self.zoom });
        self.dirty.clear();
    }

    /// Re-render `rect` of the cache from the document.
    fn render_into_cache(&mut self, rect: Rect, pictures: &dyn PictureSource) {
        let Some(cache) = self.cache.as_ref() else { return };
        let Some(r) = rect.intersect(&cache.board_rect()) else { return };
        let z = cache.zoom;
        let x0 = ((r.x - cache.origin.x) * z).floor().max(0.0) as i32;
        let y0 = ((r.y - cache.origin.y) * z).floor().max(0.0) as i32;
        let x1 = (((r.right() - cache.origin.x) * z).ceil() as i32).min(cache.pixmap.width() as i32);
        let y1 = (((r.bottom() - cache.origin.y) * z).ceil() as i32).min(cache.pixmap.height() as i32);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
        let Some(mut tmp) = Pixmap::new(w, h) else { return };
        let view = View::new(cache.origin.x + x0 as f32 / z, cache.origin.y + y0 as f32 / z, z);
        self.render_region(&mut tmp, &view, pictures);
        let paint = PixmapPaint { opacity: 1.0, blend_mode: BlendMode::Source, quality: FilterQuality::Nearest };
        if let Some(cache) = self.cache.as_mut() {
            cache.pixmap.draw_pixmap(x0, y0, tmp.as_ref(), &paint, Affine::identity(), None);
        }
    }

    /// Move the cache so it covers the view again at the same zoom: copy
    /// what overlaps, render what is uncovered.
    fn shift_cache(&mut self, pictures: &dyn PictureSource) {
        let (origin, w, h) = self.cache_rect_for_view();
        let Some(old) = self.cache.take() else { return };
        let z = old.zoom;
        let dx = ((old.origin.x - origin.x) * z).round() as i32;
        let dy = ((old.origin.y - origin.y) * z).round() as i32;
        let mut pixmap = Pixmap::new(w, h).expect("cache size");
        pixmap.fill(tiny_color(self.background, 1.0));
        let paint = PixmapPaint { opacity: 1.0, blend_mode: BlendMode::Source, quality: FilterQuality::Nearest };
        pixmap.draw_pixmap(dx, dy, old.pixmap.as_ref(), &paint, Affine::identity(), None);
        let new = Cache { pixmap, origin, zoom: z };
        let kept = old.board_rect();
        let full = new.board_rect();
        self.cache = Some(new);
        // The strips of the new cache the old one did not cover.
        let mut strips = Vec::new();
        if kept.y > full.y {
            strips.push(Rect::new(full.x, full.y, full.w, kept.y - full.y));
        }
        if kept.bottom() < full.bottom() {
            strips.push(Rect::new(full.x, kept.bottom(), full.w, full.bottom() - kept.bottom()));
        }
        let (my0, my1) = (kept.y.max(full.y), kept.bottom().min(full.bottom()));
        if my1 > my0 {
            if kept.x > full.x {
                strips.push(Rect::new(full.x, my0, kept.x - full.x, my1 - my0));
            }
            if kept.right() < full.right() {
                strips.push(Rect::new(kept.right(), my0, full.right() - kept.right(), my1 - my0));
            }
        }
        for s in strips {
            self.render_into_cache(s.inflate(1.0 / z), pictures);
        }
    }

    fn compose(&mut self, pictures: &dyn PictureSource) {
        if self.cache.is_none() {
            self.rebuild_cache(pictures);
        }
        // Pending re-renders, at the cache's zoom.
        if !self.dirty.is_empty() {
            let cache_area = self.cache.as_ref().map(|c| c.board_rect().area()).unwrap_or(0.0);
            let dirty = merge(std::mem::take(&mut self.dirty));
            let area: f32 = dirty.iter().map(|r| r.area()).sum();
            let same_zoom = self.cache.as_ref().is_some_and(|c| c.zoom == self.zoom);
            if area > cache_area * FULL_RERENDER_SHARE && same_zoom {
                self.rebuild_cache(pictures);
            } else {
                for r in dirty {
                    self.render_into_cache(r, pictures);
                }
            }
        }
        let view = self.view();
        let cache = self.cache.as_ref().expect("cache built");
        if cache.zoom == self.zoom {
            let inside = {
                let c = cache.board_rect();
                let v = view.board_rect(self.width, self.height);
                v.x >= c.x - 0.5 / self.zoom
                    && v.y >= c.y - 0.5 / self.zoom
                    && v.right() <= c.right() + 0.5 / self.zoom
                    && v.bottom() <= c.bottom() + 0.5 / self.zoom
            };
            if !inside {
                self.shift_cache(pictures);
            }
            let cache = self.cache.as_ref().expect("cache built");
            // Whole-pixel offset: copy rows.
            let ox = ((view.x - cache.origin.x) * cache.zoom).round() as i64;
            let oy = ((view.y - cache.origin.y) * cache.zoom).round() as i64;
            copy_rows(&cache.pixmap, ox, oy, &mut self.frame, self.background);
        } else {
            // A zoom not settled: the cache, scaled (nearest, as paint's
            // view resample does).
            let k = cache.zoom / self.zoom;
            let sx0 = (view.x - cache.origin.x) * cache.zoom;
            let sy0 = (view.y - cache.origin.y) * cache.zoom;
            resample_nearest(&cache.pixmap, sx0, sy0, k, &mut self.frame, self.background);
        }
    }
}

/// The grid's line color over `background`: a slightly darker shade of a
/// light background, a slightly lighter one of a dark background.
pub fn grid_color(background: Color) -> Color {
    let luma = 0.299 * background.r as f32 + 0.587 * background.g as f32 + 0.114 * background.b as f32;
    let shift = |c: u8| -> u8 {
        if luma > 128.0 {
            (c as f32 * 0.9).round() as u8
        } else {
            (c as f32 + (255.0 - c as f32) * 0.1).round() as u8
        }
    };
    Color::rgb(shift(background.r), shift(background.g), shift(background.b))
}

/// The grid spacing actually drawn at `zoom`: `spacing` times 5 until lines
/// are at least 8 pixels apart.
pub(crate) fn grid_step(spacing: f32, zoom: f32) -> f32 {
    let mut s = spacing;
    while s * zoom < 8.0 && s.is_finite() {
        s *= 5.0;
    }
    s
}

/// One-pixel grid lines over `pixmap` seen through `view`, at board
/// multiples of the spacing, so every region of one cache lines up.
fn draw_grid(pixmap: &mut Pixmap, view: &View, spacing: f32, color: Color) {
    let step = grid_step(spacing, view.scale);
    if !(step.is_finite() && step > 0.0) {
        return;
    }
    let (w, h) = (pixmap.width(), pixmap.height());
    let r = view.board_rect(w, h);
    let mut paint = tiny_skia::Paint::default();
    paint.set_color(tiny_color(color, 1.0));
    paint.anti_alias = false;
    // A hair over the line's exact pixel so float error in a region's
    // origin never moves a line by one pixel between regions.
    let px = |b: f32, o: f32| ((b - o) * view.scale + 1e-3).floor();
    let mut k = (r.x / step).floor();
    while k * step <= r.right() {
        let x = px(k * step, view.x);
        if x >= 0.0 && x < w as f32 {
            if let Some(rect) = tiny_skia::Rect::from_xywh(x, 0.0, 1.0, h as f32) {
                pixmap.fill_rect(rect, &paint, Affine::identity(), None);
            }
        }
        k += 1.0;
    }
    let mut k = (r.y / step).floor();
    while k * step <= r.bottom() {
        let y = px(k * step, view.y);
        if y >= 0.0 && y < h as f32 {
            if let Some(rect) = tiny_skia::Rect::from_xywh(0.0, y, w as f32, 1.0) {
                pixmap.fill_rect(rect, &paint, Affine::identity(), None);
            }
        }
        k += 1.0;
    }
}

/// Stamp a run of stroke points (a segment or the whole stroke) into
/// `pixmap` seen through `view`.
/// `multiply` false lays a highlighter over plainly (stamping into a cache
/// whose background is baked in).
fn stamp(pixmap: &mut Pixmap, view: &View, kind: &Kind, style: &ResolvedStyle, points: &[[f32; 3]], multiply: bool) {
    let Some(path) = stroke_path(points) else { return };
    let mut paint = stroke_paint(kind, style.stroke, style.opacity);
    if !multiply {
        paint.blend_mode = BlendMode::SourceOver;
    }
    let stroke = tiny_skia::Stroke {
        width: style.width.max(0.5),
        line_cap: tiny_skia::LineCap::Round,
        line_join: tiny_skia::LineJoin::Round,
        ..Default::default()
    };
    pixmap.stroke_path(&path, &paint, &stroke, view.transform(), None);
}

/// Fill `dst` from `src` scaled: `dst` pixel (x, y) shows `src` pixel
/// (`sx0` + x * `k`, `sy0` + y * `k`), nearest; what `src` does not cover is
/// `background`. A column table and whole-pixel copies: no blending, no
/// filtering, a frame in a few milliseconds even at 4K.
fn resample_nearest(src: &Pixmap, sx0: f32, sy0: f32, k: f32, dst: &mut Pixmap, background: Color) {
    let (sw, sh) = (src.width() as i64, src.height() as i64);
    let (dw, dh) = (dst.width() as usize, dst.height() as usize);
    let bg = [background.r, background.g, background.b, 255u8];
    let columns: Vec<i64> = (0..dw).map(|x| (sx0 + (x as f32 + 0.5) * k).floor() as i64).collect();
    let s = src.data();
    let d = dst.data_mut();
    for y in 0..dh {
        let sy = (sy0 + (y as f32 + 0.5) * k).floor() as i64;
        let row = &mut d[y * dw * 4..(y + 1) * dw * 4];
        if sy < 0 || sy >= sh {
            for px in row.chunks_exact_mut(4) {
                px.copy_from_slice(&bg);
            }
            continue;
        }
        let base = (sy * sw * 4) as usize;
        for (px, sx) in row.chunks_exact_mut(4).zip(columns.iter()) {
            if *sx < 0 || *sx >= sw {
                px.copy_from_slice(&bg);
            } else {
                let i = base + (*sx as usize) * 4;
                px.copy_from_slice(&s[i..i + 4]);
            }
        }
    }
}

/// Copy `src` into `dst` with `src` pixel (`ox`, `oy`) at `dst` (0, 0),
/// filling what `src` does not cover with `background`.
fn copy_rows(src: &Pixmap, ox: i64, oy: i64, dst: &mut Pixmap, background: Color) {
    let (sw, sh) = (src.width() as i64, src.height() as i64);
    let (dw, dh) = (dst.width() as i64, dst.height() as i64);
    let covers = ox >= 0 && oy >= 0 && ox + dw <= sw && oy + dh <= sh;
    if !covers {
        dst.fill(tiny_color(background, 1.0));
    }
    let x0 = ox.max(0);
    let x1 = (ox + dw).min(sw);
    if x1 <= x0 {
        return;
    }
    let s = src.data();
    let d = dst.data_mut();
    for dy in 0..dh {
        let sy = oy + dy;
        if sy < 0 || sy >= sh {
            continue;
        }
        let sstart = ((sy * sw + x0) * 4) as usize;
        let send = ((sy * sw + x1) * 4) as usize;
        let dstart = ((dy * dw + (x0 - ox)) * 4) as usize;
        d[dstart..dstart + (send - sstart)].copy_from_slice(&s[sstart..send]);
    }
}

/// Merge overlapping rectangles (a small list: quadratic is fine).
fn merge(mut rects: Vec<Rect>) -> Vec<Rect> {
    let mut changed = true;
    while changed {
        changed = false;
        let mut out: Vec<Rect> = Vec::with_capacity(rects.len());
        'next: for r in rects {
            for o in out.iter_mut() {
                if o.intersects(&r) {
                    *o = o.union(&r);
                    changed = true;
                    continue 'next;
                }
            }
            out.push(r);
        }
        rects = out;
    }
    rects
}
