//! Soft edges (docs/DRAWING_WAVE6_CONTRACT.md section 3): an element's or a
//! layer's **feather** (its edge fades out, what is inside kept sharp) and a
//! layer's **blur** (everything it draws softened), each a radius in board
//! units, worked as a Gaussian blur with sigma half the radius, so the edge
//! fades over about the radius on either side and nothing reaches further
//! than [`reach`] (three sigma) past it.
//!
//! - The blur is three box passes each way (separable, the cost per pixel
//!   independent of the radius), on premultiplied pixels with transparency
//!   around them.
//! - A blur that would be wider than [`WORK_SIGMA_PX`] on the target is
//!   worked at a lower density (a power of two at which sigma is 3 to 6
//!   pixels) and laid onto the target bilinear: a blurred picture holds no
//!   detail finer than its sigma, so this loses nothing visible. Worked that
//!   way, what an element or layer draws is blurred **whole** once, kept in
//!   a cache for the process ([`BUDGET`], least recently used out first)
//!   keyed by what it draws (an element's fingerprint, a layer's hash of
//!   everything inside it) and the density, so panning and zooming over a
//!   large feather or a hazy layer reuse it. What is too large for one entry
//!   ([`MAX_ENTRY_PX`]) is worked for the area drawn only.
//! - A feather keeps the sharp drawing inside and the blur outside:
//!   per pixel, with `M` the sharp alpha and `Mb` the blurred one,
//!   `out = (1 - M) · blurred + Mb · sharp` (premultiplied), alpha `Mb`.
//!   A flat color comes out exactly as its blurred self; a picture or text
//!   inside keeps its detail and only its rim fades. The SVG export writes
//!   the same sum with `feGaussianBlur` and `feComposite`.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

use tiny_skia::{BlendMode, FilterQuality, Pixmap, PixmapPaint, Transform as Affine};

use crate::model::*;
use crate::render::{PictureSource, View};
use crate::snapshot::{BoardSnapshot, Element, Fx};

/// The largest radius kept, board units (a larger one reads as this).
pub const MAX_RADIUS: f32 = 10_000.0;
/// The widest sigma, in pixels, blurred at the target's own density; wider
/// ones are worked at a lower density.
pub const WORK_SIGMA_PX: f32 = 6.0;
/// The most pixels one cached blur holds (2,048 × 2,048, 16 MB).
pub const MAX_ENTRY_PX: u64 = 2048 * 2048;
/// What the blur cache holds at most, bytes.
pub const BUDGET: usize = 160 << 20;

/// A radius as stored, read: finite, at least 0, at most [`MAX_RADIUS`]
/// (absent, negative or not a number: 0).
pub fn radius(stored: Option<f32>) -> f32 {
    stored.filter(|r| r.is_finite() && *r > 0.0).map_or(0.0, |r| r.min(MAX_RADIUS))
}

/// The Gaussian's sigma for a radius.
pub fn sigma(radius: f32) -> f32 {
    radius.max(0.0) * 0.5
}

/// How far past the hard edge a radius reaches, board units (three sigma).
pub fn reach(radius: f32) -> f32 {
    if radius > 0.0 {
        3.0 * sigma(radius)
    } else {
        0.0
    }
}

// ── The blur ────────────────────────────────────────────────────────────

/// The radii of three box blurs that together approximate a Gaussian of
/// `sigma` pixels (after Kutskir's boxes for a Gaussian).
fn boxes(sigma: f32) -> [usize; 3] {
    let n = 3.0f32;
    let ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
    let mut wl = ideal.floor() as i64;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wl = wl.max(1);
    let wu = wl + 2;
    let wlf = wl as f32;
    let m = ((12.0 * sigma * sigma - n * wlf * wlf - 4.0 * n * wlf - 3.0 * n) / (-4.0 * wlf - 4.0)).round() as i64;
    let mut out = [0usize; 3];
    for (i, o) in out.iter_mut().enumerate() {
        let w = if (i as i64) < m { wl } else { wu };
        *o = ((w - 1) / 2).max(0) as usize;
    }
    out
}

/// Blur premultiplied RGBA `data` (`w` × `h`) in place with a Gaussian of
/// `sigma` pixels, everything outside transparent.
pub fn gaussian(data: &mut [u8], w: usize, h: usize, sigma: f32) {
    if w == 0 || h == 0 || !(sigma.is_finite() && sigma >= 0.3) {
        return;
    }
    let mut tmp = vec![0u8; data.len()];
    for r in boxes(sigma) {
        if r == 0 {
            continue;
        }
        box_rows(data, &mut tmp, w, h, r);
        box_cols(&tmp, data, w, h, r);
    }
}

/// `1 / (2r + 1)` in 24-bit fixed point.
fn inverse(r: usize) -> u64 {
    let d = (2 * r + 1) as u64;
    ((1u64 << 24) + d / 2) / d
}

fn box_rows(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    let inv = inverse(r);
    let round = 1u64 << 23;
    for y in 0..h {
        let row = &src[y * w * 4..(y + 1) * w * 4];
        let out = &mut dst[y * w * 4..(y + 1) * w * 4];
        let mut acc = [0u32; 4];
        for x in 0..=r.min(w - 1) {
            for c in 0..4 {
                acc[c] += row[x * 4 + c] as u32;
            }
        }
        for x in 0..w {
            for c in 0..4 {
                out[x * 4 + c] = ((acc[c] as u64 * inv + round) >> 24) as u8;
            }
            let add = x + r + 1;
            if add < w {
                for c in 0..4 {
                    acc[c] += row[add * 4 + c] as u32;
                }
            }
            if x >= r {
                let sub = x - r;
                for c in 0..4 {
                    acc[c] -= row[sub * 4 + c] as u32;
                }
            }
        }
    }
}

fn box_cols(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    let inv = inverse(r);
    let round = 1u64 << 23;
    let stride = w * 4;
    let mut acc = vec![0u32; stride];
    for y in 0..=r.min(h - 1) {
        for (a, v) in acc.iter_mut().zip(&src[y * stride..(y + 1) * stride]) {
            *a += *v as u32;
        }
    }
    for y in 0..h {
        for (o, a) in dst[y * stride..(y + 1) * stride].iter_mut().zip(&acc) {
            *o = ((*a as u64 * inv + round) >> 24) as u8;
        }
        let add = y + r + 1;
        if add < h {
            for (a, v) in acc.iter_mut().zip(&src[add * stride..(add + 1) * stride]) {
                *a += *v as u32;
            }
        }
        if y >= r {
            let sub = y - r;
            for (a, v) in acc.iter_mut().zip(&src[sub * stride..(sub + 1) * stride]) {
                *a -= *v as u32;
            }
        }
    }
}

/// A feather: `sharp` keeps its inside and takes `soft`'s edge (see the
/// module doc). Both the same size, premultiplied.
pub fn feather_into(sharp: &mut [u8], soft: &[u8]) {
    for (s, b) in sharp.chunks_exact_mut(4).zip(soft.chunks_exact(4)) {
        let m = s[3] as u32;
        let mb = b[3] as u32;
        for c in 0..3 {
            s[c] = (((255 - m) * b[c] as u32 + mb * s[c] as u32 + 127) / 255) as u8;
        }
        s[3] = mb as u8;
    }
}

// ── What is blurred ─────────────────────────────────────────────────────

/// Something drawn and then blurred: an element, or what a layer holds.
pub(crate) trait Source {
    /// What it draws, for the cache (`None`: not kept, as while a picture
    /// it shows is still loading).
    fn key(&self) -> Option<u64>;
    /// Where it draws with hard edges, board units (`None`: nothing).
    fn content(&self) -> Option<Rect>;
    /// Draw it through `vt` (board to pixels: a scale and a shift) over
    /// what `pixmap` holds; `region` is the board area `pixmap` shows.
    fn draw(&self, pixmap: &mut Pixmap, vt: Affine, region: &Rect);
}

/// Whether `vt` is a uniform scale and a shift (what a view is).
pub(crate) fn upright(vt: &Affine) -> bool {
    vt.kx.abs() < 1e-6 && vt.ky.abs() < 1e-6 && vt.sx > 0.0 && (vt.sx - vt.sy).abs() <= 1e-4 * vt.sx
}

fn board_rect(vt: &Affine, w: u32, h: u32) -> Rect {
    let s = vt.sx;
    Rect::new(-vt.tx / s, -vt.ty / s, w as f32 / s, h as f32 / s)
}

/// The density a blur of `sigma` board units is worked at when the target
/// shows `scale` pixels per unit: the target's own when sigma is at most
/// [`WORK_SIGMA_PX`] there, else the power of two at which it is half that
/// to that.
pub fn work_scale(sigma: f32, scale: f32) -> f32 {
    if sigma * scale <= WORK_SIGMA_PX {
        return scale;
    }
    let s = WORK_SIGMA_PX / sigma;
    2f32.powi(s.log2().floor() as i32).min(scale)
}

/// `src` blurred with `sigma` board units as `vt` shows it, drawn into
/// `out` (cleared by the caller: what it holds is drawn over).
pub(crate) fn blurred(out: &mut Pixmap, vt: Affine, sigma: f32, src: &dyn Source) {
    let (w, h) = (out.width(), out.height());
    if !upright(&vt) {
        src.draw(out, vt, &board_rect(&vt, w, h));
        return;
    }
    let s = vt.sx;
    let sp = sigma * s;
    if sp < 0.3 {
        src.draw(out, vt, &board_rect(&vt, w, h));
        return;
    }
    let ws = work_scale(sigma, s);
    if ws >= s {
        // At the target's own density, with room for what blurs in.
        let m = (3.0 * sp).ceil() as u32 + 1;
        let Some(mut big) = Pixmap::new(w + 2 * m, h + 2 * m) else { return };
        let bvt = vt.post_translate(m as f32, m as f32);
        let area = board_rect(&bvt, w + 2 * m, h + 2 * m);
        src.draw(&mut big, bvt, &area);
        let (bw, bh) = (big.width() as usize, big.height() as usize);
        gaussian(big.data_mut(), bw, bh, sp);
        let paint = PixmapPaint { opacity: 1.0, blend_mode: BlendMode::SourceOver, quality: FilterQuality::Nearest };
        out.draw_pixmap(-(m as i32), -(m as i32), big.as_ref(), &paint, Affine::identity(), None);
        return;
    }
    // Lower: the whole of it once, kept, when that is small enough.
    let grow = 3.0 * sigma + 2.0 / ws;
    if let (Some(key), Some(content)) = (src.key(), src.content()) {
        let area = content.inflate(grow);
        if let Some(px) = PxArea::of(&area, ws).filter(|p| p.pixels() <= MAX_ENTRY_PX) {
            let ck = (key, ws.to_bits(), sigma.to_bits());
            let cached = cache().lock().ok().and_then(|mut c| c.get(&ck));
            let image = match cached {
                Some(image) => image,
                None => {
                    let Some(image) = px.blur(src, ws, sigma) else { return };
                    let image = Arc::new(image);
                    if let Ok(mut c) = cache().lock() {
                        c.put(ck, image.clone());
                    }
                    image
                }
            };
            px.lay(out, &image, vt, ws);
            return;
        }
    }
    // Too large to keep (or not to be kept): the area drawn, at the lower
    // density.
    let area = board_rect(&vt, w, h).inflate(grow);
    let Some(px) = PxArea::of(&area, ws) else { return };
    if let Some(image) = px.blur(src, ws, sigma) {
        px.lay(out, &image, vt, ws);
    }
}

/// An area on the pixel grid of density `scale`.
struct PxArea {
    x0: i64,
    y0: i64,
    w: u32,
    h: u32,
}

impl PxArea {
    fn of(r: &Rect, scale: f32) -> Option<PxArea> {
        if !r.is_finite() {
            return None;
        }
        let x0 = (r.x * scale).floor() as i64;
        let y0 = (r.y * scale).floor() as i64;
        let x1 = (r.right() * scale).ceil() as i64;
        let y1 = (r.bottom() * scale).ceil() as i64;
        let (w, h) = (x1 - x0, y1 - y0);
        (w > 0 && h > 0 && w < 1 << 20 && h < 1 << 20).then_some(PxArea { x0, y0, w: w as u32, h: h as u32 })
    }

    fn pixels(&self) -> u64 {
        self.w as u64 * self.h as u64
    }

    /// `src` drawn over this area at `scale` and blurred.
    fn blur(&self, src: &dyn Source, scale: f32, sigma: f32) -> Option<Pixmap> {
        let mut image = Pixmap::new(self.w, self.h)?;
        let ivt = Affine::from_row(scale, 0.0, 0.0, scale, -(self.x0 as f32), -(self.y0 as f32));
        src.draw(&mut image, ivt, &board_rect(&ivt, self.w, self.h));
        let (w, h) = (self.w as usize, self.h as usize);
        gaussian(image.data_mut(), w, h, sigma * scale);
        Some(image)
    }

    /// `image` (this area at `scale`) laid onto `out` as `vt` shows it.
    fn lay(&self, out: &mut Pixmap, image: &Pixmap, vt: Affine, scale: f32) {
        let s = vt.sx;
        let k = s / scale;
        let t = Affine::from_row(k, 0.0, 0.0, k, self.x0 as f32 * k + vt.tx, self.y0 as f32 * k + vt.ty);
        let paint = PixmapPaint { opacity: 1.0, blend_mode: BlendMode::SourceOver, quality: FilterQuality::Bilinear };
        out.draw_pixmap(0, 0, image.as_ref(), &paint, t, None);
    }
}

// ── The cache ───────────────────────────────────────────────────────────

type Key = (u64, u32, u32);

#[derive(Default)]
struct Cache {
    map: HashMap<Key, (Arc<Pixmap>, u64)>,
    bytes: usize,
    clock: u64,
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

impl Cache {
    fn get(&mut self, key: &Key) -> Option<Arc<Pixmap>> {
        self.clock += 1;
        let clock = self.clock;
        self.map.get_mut(key).map(|(p, used)| {
            *used = clock;
            p.clone()
        })
    }

    fn put(&mut self, key: Key, image: Arc<Pixmap>) {
        self.clock += 1;
        self.bytes += image.data().len();
        if let Some((old, _)) = self.map.insert(key, (image, self.clock)) {
            self.bytes -= old.data().len();
        }
        if self.bytes > BUDGET {
            let mut by_use: Vec<(Key, u64, usize)> = self.map.iter().map(|(k, (p, u))| (*k, *u, p.data().len())).collect();
            by_use.sort_by_key(|(_, u, _)| *u);
            for (k, _, size) in by_use {
                if self.bytes <= BUDGET * 3 / 4 {
                    break;
                }
                self.map.remove(&k);
                self.bytes -= size;
            }
        }
    }
}

/// How many bytes of blurs are kept (for tests and measurements).
pub fn cached_bytes() -> usize {
    cache().lock().map(|c| c.bytes).unwrap_or(0)
}

/// Drop every kept blur (for measurements: a cold render).
pub fn clear_cache() {
    if let Ok(mut c) = cache().lock() {
        c.map.clear();
        c.bytes = 0;
    }
}

// ── Elements ────────────────────────────────────────────────────────────

/// An element drawn with hard edges, to be feathered.
pub(crate) struct ElementSource<'a> {
    pub e: &'a Element,
    pub pictures: &'a dyn PictureSource,
}

impl Source for ElementSource<'_> {
    fn key(&self) -> Option<u64> {
        if !element_ready(self.e, self.pictures) {
            return None;
        }
        let mut h = Fx::default();
        1u8.hash(&mut h);
        self.e.fingerprint.hash(&mut h);
        Some(h.finish())
    }

    fn content(&self) -> Option<Rect> {
        Some(self.e.hard_bounds()).filter(|b| b.is_finite())
    }

    fn draw(&self, pixmap: &mut Pixmap, vt: Affine, region: &Rect) {
        crate::render::draw_element_hard(pixmap, self.e, self.pictures, vt, region);
    }
}

/// Whether every picture `e` shows is loaded.
fn element_ready(e: &Element, pictures: &dyn PictureSource) -> bool {
    match &e.raw.geom {
        Geom::Picture(p) => pictures.picture(&p.src).is_some(),
        _ => match e.scatter.as_ref().and_then(|s| s.def.as_ref()) {
            Some(def) => def.pictures.iter().all(|src| pictures.picture(src).is_some()),
            None => true,
        },
    }
}

/// Element `e` drawn feathered through `vt` into `pixmap`. False when it
/// was not (no feather, a turned or skewed target): draw it as it is.
pub(crate) fn draw_feathered(pixmap: &mut Pixmap, e: &Element, pictures: &dyn PictureSource, vt: Affine, region: &Rect) -> bool {
    let radius = e.style.feather;
    if radius <= 0.0 || e.global || !upright(&vt) {
        return false;
    }
    // The pixels it can touch.
    let b = crate::snapshot::map_rect(&vt, &e.bounds);
    let x0 = b.x.floor().max(0.0);
    let y0 = b.y.floor().max(0.0);
    let x1 = b.right().ceil().min(pixmap.width() as f32);
    let y1 = b.bottom().ceil().min(pixmap.height() as f32);
    if !(x1 > x0 && y1 > y0) {
        return true;
    }
    let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
    let sub = vt.post_translate(-x0, -y0);
    let (Some(mut sharp), Some(mut soft)) = (Pixmap::new(w, h), Pixmap::new(w, h)) else { return true };
    let area = board_rect(&sub, w, h);
    let region = area.intersect(region).unwrap_or(area);
    crate::render::draw_element_hard(&mut sharp, e, pictures, sub, &region);
    blurred(&mut soft, sub, sigma(radius), &ElementSource { e, pictures });
    feather_into(sharp.data_mut(), soft.data());
    let paint = PixmapPaint { opacity: 1.0, blend_mode: BlendMode::SourceOver, quality: FilterQuality::Nearest };
    pixmap.draw_pixmap(x0 as i32, y0 as i32, sharp.as_ref(), &paint, Affine::identity(), None);
    true
}

// ── Layers ──────────────────────────────────────────────────────────────

/// What a layer (with its groups' contents) draws, worked out once per
/// snapshot when it is first drawn soft.
#[derive(Debug, Clone)]
pub(crate) struct LayerInfo {
    /// A hash of everything inside it.
    pub rev: u64,
    /// Where it draws with its own soft edges left out, board units.
    pub content: Option<Rect>,
    /// The pictures it shows.
    pub pictures: Vec<String>,
}

pub(crate) fn layer_info(snapshot: &BoardSnapshot, at: usize) -> LayerInfo {
    let tree = snapshot.layer_tree();
    let own = snapshot.soft_reach(at);
    let mut h = Fx::default();
    2u8.hash(&mut h);
    at.hash(&mut h);
    let mut content: Option<Rect> = None;
    let mut grow = |r: Rect| {
        if r.is_finite() {
            content = Some(content.map_or(r, |c| c.union(&r)));
        }
    };
    let mut pictures = Vec::new();
    for e in &snapshot.elements {
        let Some(l) = e.layer_index else { continue };
        if !tree.is_within(l, at) || !e.visible || !e.has_extent || matches!(e.raw.kind, Kind::Group) {
            continue;
        }
        e.fingerprint.hash(&mut h);
        grow(e.bounds.inflate(-own));
        match &e.raw.geom {
            Geom::Picture(p) => pictures.push(p.src.clone()),
            _ => {
                if let Some(def) = e.scatter.as_ref().and_then(|s| s.def.as_ref()) {
                    pictures.extend(def.pictures.iter().cloned());
                }
            }
        }
    }
    for p in 0..snapshot.layers.len() {
        if !tree.is_within(p, at) || !snapshot.paint_shown(p) {
            continue;
        }
        snapshot.composite.get(p).hash(&mut h);
        for op in snapshot.paint_ops(&snapshot.layers[p].id) {
            op.effective.hash(&mut h);
            if let crate::paint_ops::PaintBody::Pixels(px) = &op.raw.body {
                pictures.push(px.src.clone());
            }
        }
        if let Some(r) = snapshot.paint_reach(p) {
            grow(r.inflate(-own));
        }
    }
    LayerInfo { rev: h.finish(), content, pictures }
}

/// What layer `at` holds, drawn without its own opacity, blend, clip, blur
/// and feather.
pub(crate) struct LayerSource<'a> {
    pub snapshot: &'a BoardSnapshot,
    pub pictures: &'a dyn PictureSource,
    pub at: usize,
    pub mask: Option<&'a [bool]>,
    pub clipped: bool,
}

impl LayerSource<'_> {
    fn info(&self) -> &LayerInfo {
        self.snapshot.soft_info[self.at].get_or_init(|| layer_info(self.snapshot, self.at))
    }
}

impl Source for LayerSource<'_> {
    fn key(&self) -> Option<u64> {
        if self.mask.is_some() {
            return None;
        }
        let info = self.info();
        if !info.pictures.iter().all(|src| self.pictures.picture(src).is_some()) {
            return None;
        }
        let mut h = Fx::default();
        info.rev.hash(&mut h);
        self.clipped.hash(&mut h);
        Some(h.finish())
    }

    fn content(&self) -> Option<Rect> {
        self.info().content
    }

    fn draw(&self, pixmap: &mut Pixmap, vt: Affine, _region: &Rect) {
        let s = vt.sx;
        let view = View::new(-vt.tx / s, -vt.ty / s, s);
        crate::render::draw_layer_inside(pixmap, self.snapshot, self.pictures, &view, self.mask, self.clipped, self.at);
    }
}

/// Layer `at`'s blur and feather worked on `buffer` (what it drew, as `vt`
/// shows it), before it is blended down with its opacity, blend and clip.
pub(crate) fn soften_layer(buffer: &mut Pixmap, snapshot: &BoardSnapshot, pictures: &dyn PictureSource, at: usize, vt: Affine, mask: Option<&[bool]>, clipped: bool) {
    let Some(l) = snapshot.layers.get(at) else { return };
    let (b, f) = (sigma(l.blur), sigma(l.feather));
    if b <= 0.0 && f <= 0.0 {
        return;
    }
    let src = LayerSource { snapshot, pictures, at, mask, clipped };
    let (w, h) = (buffer.width(), buffer.height());
    if b > 0.0 {
        let Some(mut out) = Pixmap::new(w, h) else { return };
        blurred(&mut out, vt, b, &src);
        *buffer = out;
    }
    if f > 0.0 {
        // The feather's mask is the edge of what shows: the blurred
        // drawing's, when it is blurred too (two Gaussians make one).
        let Some(mut soft) = Pixmap::new(w, h) else { return };
        blurred(&mut soft, vt, (b * b + f * f).sqrt(), &src);
        feather_into(buffer.data_mut(), soft.data());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_boxes_approximate_the_gaussian() {
        // A single opaque column blurred: its spread matches sigma.
        for sigma in [1.0f32, 3.0, 8.0, 20.0] {
            let w = (sigma * 12.0) as usize + 1;
            let c = w / 2;
            // A full-height column, so the vertical passes keep it; read
            // the middle row.
            let h = w;
            let mut img = vec![0u8; w * h * 4];
            for y in 0..h {
                img[(y * w + c) * 4 + 3] = 255;
            }
            gaussian(&mut img, w, h, sigma);
            let big = &img[(h / 2) * w * 4..(h / 2 + 1) * w * 4];
            let total: f32 = big.chunks(4).map(|p| p[3] as f32).sum();
            assert!(total > 0.0);
            let var: f32 = big.chunks(4).enumerate().map(|(x, p)| p[3] as f32 * ((x as f32 - c as f32).powi(2))).sum::<f32>() / total;
            let got = var.sqrt();
            assert!((got - sigma).abs() < sigma * 0.25 + 0.6, "sigma {sigma}: spread {got}");
        }
    }

    #[test]
    fn a_blur_keeps_premultiplied_pixels_valid() {
        let (w, h) = (40, 30);
        let mut data = vec![0u8; w * h * 4];
        for y in 10..20 {
            for x in 10..30 {
                let i = (y * w + x) * 4;
                data[i..i + 4].copy_from_slice(&[200, 100, 30, 200]);
            }
        }
        gaussian(&mut data, w, h, 4.0);
        for p in data.chunks(4) {
            assert!(p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]);
        }
        let mut sharp = vec![0u8; w * h * 4];
        sharp[(15 * w + 15) * 4..(15 * w + 15) * 4 + 4].copy_from_slice(&[255, 0, 0, 255]);
        feather_into(&mut sharp, &data);
        for p in sharp.chunks(4) {
            assert!(p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]);
        }
    }

    #[test]
    fn the_work_density_keeps_sigma_small() {
        assert_eq!(work_scale(2.0, 2.0), 2.0);
        let s = work_scale(100.0, 2.0);
        assert!(s * 100.0 > WORK_SIGMA_PX / 2.0 - 1e-3 && s * 100.0 <= WORK_SIGMA_PX + 1e-3, "{s}");
        assert_eq!(radius(Some(-3.0)), 0.0);
        assert_eq!(radius(Some(f32::NAN)), 0.0);
        assert_eq!(radius(Some(1e9)), MAX_RADIUS);
        assert_eq!(reach(10.0), 15.0);
    }
}
