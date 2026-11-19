//! Symbols and scatters (docs/DRAWING_WAVE6_CONTRACT.md section 2).
//!
//! A **symbol** is a few elements kept in the canvas as a definition, not
//! drawn by themselves: a key `symbol:<id>` of the board map (one map each,
//! never an entry of a container made on first use), holding its `name`,
//! `created_at`, `author` and an `elements` map of element maps in the same
//! shape the board's own elements take (no `layer`). Their coordinates are
//! the symbol's own: (0, 0) is where a placement puts it.
//!
//! A **scatter** is one element of kind `scatter`: its symbol's id in its
//! `symbol` key and its placements packed in its `geom`, an `Array` of
//! `Buffer` chunks appended as a stroke's points are. A placement is
//! [`PLACEMENT_BYTES`] (16) little-endian bytes: `x`, `y` as `f32` (the
//! scatter's own space), the scale as `u16` in 1/1024 steps (to 64), the
//! rotation as `u16` in 1/65,536 of a turn, and a tint as four bytes (red,
//! green, blue, and how far the symbol's colors move toward it). 25,000
//! placements are about 400 KB.
//!
//! Drawing a scatter draws its symbol through each placement's transform,
//! from a small picture of the symbol (a sprite) drawn once per size step
//! and tint and kept in a bounded cache ([`SPRITE_BUDGET`]); placements
//! larger than [`DIRECT_PX`] on the screen are drawn as vectors. Chunks
//! carry their bounds, so a view visits only the chunks it shows.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform as Affine};

use crate::doc::RawElement;
use crate::model::*;
use crate::render::{ts_scale, PictureSource};
use crate::snapshot::{map_rect, Element, Fx};

/// The board-map key prefix of a symbol: `symbol:<id>`.
pub const SYMBOL_PREFIX: &str = "symbol:";
/// Bytes per stored placement.
pub const PLACEMENT_BYTES: usize = 16;
/// Placements per stored chunk when many are written at once (each chunk
/// carries its own bounds when drawn, so a view skips the chunks it does
/// not show).
pub const CHUNK_PLACEMENTS: usize = 256;
/// The most placements one scatter takes (about 1.6 MB).
pub const MAX_PLACEMENTS: usize = 100_000;
/// A placement drawn larger than this many pixels across is drawn as
/// vectors rather than from a sprite.
pub const DIRECT_PX: f32 = 384.0;
/// The sprite cache's budget, bytes (shared by every canvas in the process).
pub const SPRITE_BUDGET: usize = 64 << 20;
/// The largest scale a placement stores.
pub const MAX_SCALE: f32 = 64.0;

/// Refusals, in words.
pub const NO_SYMBOL_ELEMENTS: &str = "A symbol is made of shapes, paths, strokes, text or a picture: select at least one.";
pub const SCATTER_IN_SYMBOL: &str = "A symbol cannot hold a scatter, a connector, a node card or a spotlight.";
pub const NEEDS_CLOSED_SHAPE: &str = "Scatter in Shape needs one closed shape selected: a rectangle, ellipse, polygon, star, frame or closed path.";
pub const NOTHING_FITS: &str = "No placement fits inside that shape at this spacing; try a smaller spacing.";
pub const TOO_MANY: &str = "That is more than 100,000 placements in one scatter; use a larger spacing or several scatters.";

/// The board-map key of symbol `id`.
pub fn symbol_key(id: &str) -> String {
    format!("{SYMBOL_PREFIX}{id}")
}

/// One placement of a symbol in a scatter: where its origin goes, its scale,
/// its rotation (degrees clockwise) and its tint (alpha is how far the
/// symbol's colors move toward the tint's color; transparent is none).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub x: f32,
    pub y: f32,
    pub scale: f32,
    pub rotation: f32,
    pub tint: Color,
}

impl Placement {
    pub fn at(x: f32, y: f32) -> Self {
        Self { x, y, scale: 1.0, rotation: 0.0, tint: Color::TRANSPARENT }
    }

    /// The placement as stored and read back (its scale and rotation
    /// rounded to what the packing keeps).
    pub fn stored(self) -> Self {
        let mut out = Vec::new();
        unpack(&pack(&[self]), &mut out);
        out[0]
    }

    /// Symbol space to the scatter's space.
    pub fn affine(&self) -> Affine {
        Affine::from_translate(self.x, self.y).pre_concat(Affine::from_rotate(self.rotation)).pre_concat(Affine::from_scale(self.scale, self.scale))
    }
}

/// Placements packed as stored (see the module doc).
pub fn pack(placements: &[Placement]) -> Vec<u8> {
    let mut out = Vec::with_capacity(placements.len() * PLACEMENT_BYTES);
    for p in placements {
        out.extend_from_slice(&p.x.to_le_bytes());
        out.extend_from_slice(&p.y.to_le_bytes());
        let scale = (p.scale.clamp(0.0, MAX_SCALE) * 1024.0).round().clamp(1.0, 65535.0) as u16;
        out.extend_from_slice(&scale.to_le_bytes());
        let turn = p.rotation.rem_euclid(360.0) / 360.0;
        let rot = ((turn * 65536.0).round() as u32 & 0xffff) as u16;
        out.extend_from_slice(&rot.to_le_bytes());
        out.extend_from_slice(&[p.tint.r, p.tint.g, p.tint.b, p.tint.a]);
    }
    out
}

/// Placements read from one stored chunk; a placement that is not finite is
/// skipped, a short tail ignored.
pub fn unpack(bytes: &[u8], out: &mut Vec<Placement>) {
    for c in bytes.chunks_exact(PLACEMENT_BYTES) {
        let f = |i: usize| f32::from_le_bytes([c[i], c[i + 1], c[i + 2], c[i + 3]]);
        let (x, y) = (f(0), f(4));
        if !(x.is_finite() && y.is_finite()) {
            continue;
        }
        let scale = u16::from_le_bytes([c[8], c[9]]) as f32 / 1024.0;
        let rotation = u16::from_le_bytes([c[10], c[11]]) as f32 / 65536.0 * 360.0;
        out.push(Placement { x, y, scale, rotation, tint: Color::rgba(c[12], c[13], c[14], c[15]) });
    }
}

/// A scatter as stored: its symbol and its placements, with where each
/// stored chunk ends.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RawScatter {
    pub symbol: String,
    pub placements: Vec<Placement>,
    /// The end (exclusive) of each chunk in `placements`.
    pub chunk_ends: Vec<usize>,
}

impl RawScatter {
    /// Placements of `symbol`, in chunks of [`CHUNK_PLACEMENTS`].
    pub fn of(symbol: &str, placements: &[Placement]) -> RawScatter {
        let mut chunk_ends: Vec<usize> = (1..=placements.len().div_ceil(CHUNK_PLACEMENTS)).map(|k| (k * CHUNK_PLACEMENTS).min(placements.len())).collect();
        chunk_ends.dedup();
        RawScatter { symbol: symbol.to_string(), placements: placements.to_vec(), chunk_ends }
    }

    /// Each chunk's range in `placements`.
    pub fn chunks(&self) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
        let mut start = 0;
        self.chunk_ends.iter().map(move |end| {
            let r = start..*end;
            start = *end;
            r
        })
    }
}

/// A symbol as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct RawSymbol {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub author: Option<Author>,
    pub elements: Vec<RawElement>,
}

/// A symbol resolved for drawing: its elements in drawing order, in symbol
/// space, and their extent.
#[derive(Debug)]
pub struct SymbolDef {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub author: Option<Author>,
    /// Bottom to top, resolved in a layer of their own.
    pub elements: Vec<Element>,
    /// What its elements paint, symbol space. `None` when it paints nothing.
    pub bounds: Option<Rect>,
    /// Changes whenever anything it draws does.
    pub hash: u64,
    /// The pictures it shows (a sprite drawn while one is missing is not
    /// kept).
    pub pictures: Vec<String>,
}

impl SymbolDef {
    pub(crate) fn of(id: String, name: String, created_at: String, author: Option<Author>, elements: Vec<Element>) -> SymbolDef {
        let mut h = Fx::default();
        let mut bounds: Option<Rect> = None;
        let mut pictures = Vec::new();
        for e in &elements {
            e.fingerprint.hash(&mut h);
            if !e.visible || !e.has_extent || e.global {
                continue;
            }
            bounds = Some(bounds.map_or(e.bounds, |b| b.union(&e.bounds)));
            if let Geom::Picture(p) = &e.raw.geom {
                pictures.push(p.src.clone());
            }
        }
        SymbolDef { id, name, created_at, author, elements, bounds: bounds.filter(|b| b.is_finite()), hash: h.finish(), pictures }
    }

    /// The symbol as stored (what copying a scatter to another canvas
    /// brings along: [`crate::BoardTxn::adopt_symbol`]).
    pub fn to_raw(&self) -> RawSymbol {
        RawSymbol {
            id: self.id.clone(),
            name: self.name.clone(),
            created_at: self.created_at.clone(),
            author: self.author.clone(),
            elements: self
                .elements
                .iter()
                .map(|e| {
                    let mut raw = e.raw.clone();
                    raw.layer = String::new();
                    raw
                })
                .collect(),
        }
    }
}

/// A scatter resolved for drawing: its symbol and the bounds of each chunk
/// of placements, in the scatter's own space.
#[derive(Debug)]
pub struct ResolvedScatter {
    pub def: Option<Arc<SymbolDef>>,
    /// Per stored chunk: its range in the placements, and what it covers.
    pub chunks: Vec<(std::ops::Range<usize>, Rect)>,
    /// A hash of everything but the placements (the symbol, the tint, the
    /// element's place and look): unchanged while placements are only
    /// appended.
    pub look: u64,
}

impl ResolvedScatter {
    pub(crate) fn resolve(raw: &RawScatter, def: Option<Arc<SymbolDef>>) -> ResolvedScatter {
        let mut chunks = Vec::with_capacity(raw.chunk_ends.len());
        if let Some(b) = def.as_ref().and_then(|d| d.bounds) {
            for range in raw.chunks() {
                let mut acc: Option<Rect> = None;
                for p in &raw.placements[range.clone()] {
                    let r = placed_box(p, &b);
                    acc = Some(acc.map_or(r, |a| a.union(&r)));
                }
                if let Some(r) = acc {
                    chunks.push((range, r));
                }
            }
        }
        ResolvedScatter { def, chunks, look: 0 }
    }

    /// What every placement covers, the scatter's space.
    pub fn extent(&self) -> Option<Rect> {
        self.chunks.iter().map(|(_, r)| *r).reduce(|a, b| a.union(&b))
    }

    /// What the placements from index `from` on cover, the scatter's space.
    pub fn tail_extent(&self, raw: &RawScatter, from: usize) -> Option<Rect> {
        let b = self.def.as_ref()?.bounds?;
        raw.placements.get(from..)?.iter().map(|p| placed_box(p, &b)).reduce(|a, b| a.union(&b))
    }
}

/// The box a placement of a symbol of extent `b` covers.
pub fn placed_box(p: &Placement, b: &Rect) -> Rect {
    map_rect(&p.affine(), b)
}

/// The tint as drawn: quantized, so placements whose tints differ by less
/// than a step share a sprite.
fn tint_q(c: Color) -> u32 {
    if c.a < 8 {
        return 0;
    }
    ((c.r as u32 >> 3) << 19) | ((c.g as u32 >> 3) << 14) | ((c.b as u32 >> 3) << 9) | ((c.a as u32 >> 4) << 5) | 1
}

fn tint_of_q(q: u32) -> Color {
    if q == 0 {
        return Color::TRANSPARENT;
    }
    let ch = |shift: u32| (((q >> shift) & 31) as u8) << 3 | 4;
    Color::rgba(ch(19), ch(14), ch(9), ((((q >> 5) & 15) as u8) << 4) | 8)
}

/// A color moved toward `tint` by its alpha.
pub fn tinted(c: Color, tint: Color) -> Color {
    if tint.a == 0 {
        return c;
    }
    let t = tint.a as f32 / 255.0;
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8;
    Color::rgba(lerp(c.r, tint.r), lerp(c.g, tint.g), lerp(c.b, tint.b), c.a)
}

/// The tints a placement is drawn with, as drawn (quantized): the scatter's
/// and then its own.
pub fn tints_as_drawn(scatter: Color, placement: Color) -> (Color, Color) {
    (tint_of_q(tint_q(scatter)), tint_of_q(tint_q(placement)))
}

/// Tint premultiplied pixels: each color moved toward the tint, coverage
/// kept.
fn tint_pixels(pixmap: &mut Pixmap, tints: &[Color]) {
    for tint in tints.iter().filter(|t| t.a > 0) {
        let t = tint.a as f32 / 255.0;
        for px in pixmap.pixels_mut() {
            let a = px.alpha();
            if a == 0 {
                continue;
            }
            let lerp = |c: u8, to: u8| {
                let target = to as f32 * a as f32 / 255.0;
                (c as f32 + (target - c as f32) * t).round().clamp(0.0, a as f32) as u8
            };
            if let Some(p) = tiny_skia::PremultipliedColorU8::from_rgba(lerp(px.red(), tint.r), lerp(px.green(), tint.g), lerp(px.blue(), tint.b), a) {
                *px = p;
            }
        }
    }
}

// ── The sprite cache ────────────────────────────────────────────────────

#[derive(Default)]
struct Sprites {
    map: HashMap<(u64, i32, u64), (Arc<Pixmap>, u64)>,
    bytes: usize,
    clock: u64,
}

fn sprites() -> &'static Mutex<Sprites> {
    static SPRITES: OnceLock<Mutex<Sprites>> = OnceLock::new();
    SPRITES.get_or_init(Mutex::default)
}

impl Sprites {
    fn get(&mut self, key: &(u64, i32, u64)) -> Option<Arc<Pixmap>> {
        self.clock += 1;
        let clock = self.clock;
        self.map.get_mut(key).map(|(p, used)| {
            *used = clock;
            p.clone()
        })
    }

    fn put(&mut self, key: (u64, i32, u64), sprite: Arc<Pixmap>) {
        self.clock += 1;
        self.bytes += sprite.data().len();
        if let Some((old, _)) = self.map.insert(key, (sprite, self.clock)) {
            self.bytes -= old.data().len();
        }
        if self.bytes > SPRITE_BUDGET {
            // The least recently used go until three quarters are left.
            let mut by_use: Vec<((u64, i32, u64), u64, usize)> = self.map.iter().map(|(k, (p, u))| (*k, *u, p.data().len())).collect();
            by_use.sort_by_key(|(_, u, _)| *u);
            for (k, _, size) in by_use {
                if self.bytes <= SPRITE_BUDGET * 3 / 4 {
                    break;
                }
                self.map.remove(&k);
                self.bytes -= size;
            }
        }
    }
}

/// How many bytes of sprites are held (for tests and measurements).
pub fn sprite_bytes() -> usize {
    sprites().lock().map(|s| s.bytes).unwrap_or(0)
}

/// Pixels of padding around a sprite.
const PAD: f32 = 1.0;

/// The symbol drawn at `k` pixels per symbol unit with `tints`, its top-left
/// at (`b.x - PAD / k`, `b.y - PAD / k`).
fn sprite(def: &SymbolDef, b: &Rect, k: f32, tints: &[Color], pictures: &dyn PictureSource) -> Option<Pixmap> {
    let w = (b.w * k + 2.0 * PAD).ceil().max(1.0) as u32;
    let h = (b.h * k + 2.0 * PAD).ceil().max(1.0) as u32;
    let mut pixmap = Pixmap::new(w, h)?;
    let ts = Affine::from_translate(PAD, PAD).pre_scale(k, k).pre_translate(-b.x, -b.y);
    for e in &def.elements {
        crate::render::draw_element(&mut pixmap, e, pictures, ts, b);
    }
    tint_pixels(&mut pixmap, tints);
    Some(pixmap)
}

/// Draw one placed symbol through `t` (symbol space to pixels).
#[allow(clippy::too_many_arguments)]
fn draw_instance(pixmap: &mut Pixmap, def: &SymbolDef, b: &Rect, t: Affine, tints: (Color, Color), opacity: f32, pictures: &dyn PictureSource, cacheable: bool) {
    let k = ts_scale(&t);
    if !(k.is_finite() && k > 0.0) {
        return;
    }
    let across = k * b.w.max(b.h);
    let tint_list = [tints.0, tints.1];
    if across > DIRECT_PX {
        // Big on the screen: drawn as vectors, into a buffer when it needs
        // tinting or fading.
        let device = map_rect(&t, b).inflate(1.0);
        let screen = Rect::new(0.0, 0.0, pixmap.width() as f32, pixmap.height() as f32);
        let Some(area) = device.intersect(&screen) else { return };
        if tints.0.a == 0 && tints.1.a == 0 && opacity >= 1.0 {
            for e in &def.elements {
                crate::render::draw_element(pixmap, e, pictures, t, b);
            }
            return;
        }
        let (x0, y0) = (area.x.floor(), area.y.floor());
        let Some(mut buffer) = Pixmap::new((area.right().ceil() - x0).max(1.0) as u32, (area.bottom().ceil() - y0).max(1.0) as u32) else { return };
        let bt = Affine::from_translate(-x0, -y0).pre_concat(t);
        for e in &def.elements {
            crate::render::draw_element(&mut buffer, e, pictures, bt, b);
        }
        tint_pixels(&mut buffer, &tint_list);
        let paint = PixmapPaint { opacity, ..PixmapPaint::default() };
        pixmap.draw_pixmap(x0 as i32, y0 as i32, buffer.as_ref(), &paint, Affine::identity(), None);
        return;
    }
    // Half-octave steps, at least as fine as the placement shows.
    let level = (k.log2() * 2.0).ceil().clamp(-40.0, 40.0) as i32;
    let kl = 2f32.powf(level as f32 / 2.0);
    let key = (def.hash, level, ((tint_q(tints.0) as u64) << 32) | tint_q(tints.1) as u64);
    let cached = if cacheable { sprites().lock().ok().and_then(|mut s| s.get(&key)) } else { None };
    let sprite = match cached {
        Some(s) => s,
        None => {
            let Some(made) = sprite(def, b, kl, &tint_list, pictures) else { return };
            let made = Arc::new(made);
            if cacheable {
                if let Ok(mut s) = sprites().lock() {
                    s.put(key, made.clone());
                }
            }
            made
        }
    };
    let paint = PixmapPaint { opacity, quality: FilterQuality::Bilinear, ..PixmapPaint::default() };
    let st = t.pre_translate(b.x, b.y).pre_scale(1.0 / kl, 1.0 / kl).pre_translate(-PAD, -PAD);
    pixmap.draw_pixmap(0, 0, sprite.as_ref().as_ref(), &paint, st, None);
}

/// Draw scatter `e` through `vt` (board to pixels), only the placements
/// that reach `region` (board units).
pub(crate) fn draw(pixmap: &mut Pixmap, e: &Element, pictures: &dyn PictureSource, vt: Affine, region: &Rect) {
    let (Some(sc), Some(raw)) = (&e.scatter, &e.raw.scatter) else { return };
    let Some(def) = &sc.def else { return };
    let Some(b) = def.bounds else { return };
    if !e.visible || e.opacity <= 0.0 {
        return;
    }
    let local = match e.world.invert() {
        Some(inv) => map_rect(&inv, region),
        None => return,
    };
    let world = vt.pre_concat(e.world);
    // A symbol with a picture that has not arrived is drawn with its
    // placeholder and not kept.
    let cacheable = def.pictures.iter().all(|src| pictures.picture(src).is_some());
    for (range, cb) in &sc.chunks {
        if !cb.intersects(&local) {
            continue;
        }
        for p in &raw.placements[range.clone()] {
            if !placed_box(p, &b).intersects(&local) {
                continue;
            }
            let tints = (e.style.tint, p.tint);
            draw_instance(pixmap, def, &b, world.pre_concat(p.affine()), tints, e.opacity, pictures, cacheable);
        }
    }
}

/// Whether board point `q` (the scatter's own space) is on one of its
/// placements' boxes, within `tol`.
pub(crate) fn hits(e: &Element, q: Point, tol: f32) -> bool {
    let (Some(sc), Some(raw)) = (&e.scatter, &e.raw.scatter) else { return false };
    let Some(b) = sc.def.as_ref().and_then(|d| d.bounds) else { return false };
    for (range, cb) in &sc.chunks {
        if !cb.inflate(tol).contains(q) {
            continue;
        }
        for p in &raw.placements[range.clone()] {
            let Some(inv) = p.affine().invert() else { continue };
            let mut pt = [tiny_skia::Point::from_xy(q.x, q.y)];
            inv.map_points(&mut pt);
            if b.inflate(tol / p.scale.max(1e-3)).contains(Point::new(pt[0].x, pt[0].y)) {
                return true;
            }
        }
    }
    false
}

// ── Making placements: the stamp brush and the region fill ────────────────

/// A seeded generator (SplitMix64), fixed forever: a stored scatter does not
/// depend on it, but a stamp replayed from the same pointer path gives the
/// same placements.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    /// The generator for item `index` of `seed`: independent of how many
    /// came before.
    pub fn at(seed: u64, index: u64) -> Self {
        let mut r = Rng(seed ^ index.wrapping_mul(0xD6E8_FEB8_6659_FD93));
        r.next();
        r
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// 0 to 1.
    pub fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// -1 to 1.
    pub fn signed(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }
}

/// The seed a scatter's jitter is drawn from: its element id (FNV-1a, as a
/// brush stroke's is its op id).
pub fn seed_of(id: &str) -> u64 {
    crate::brush::seed_of(id)
}

/// How each placement varies (the stamp brush and the region fill share it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Jitter {
    /// The scale every placement starts from.
    pub size: f32,
    /// 0 to 1: the scale varies by up to this fraction either way.
    pub size_jitter: f32,
    /// Degrees either way (0: upright; 180: any angle).
    pub rotation_jitter: f32,
    /// The tint every placement takes (alpha the amount; transparent:
    /// none).
    pub tint: Color,
    /// 0 to 1: how much each placement's color varies (lighter and darker
    /// without a tint; around the tint's color with one).
    pub tint_jitter: f32,
}

impl Default for Jitter {
    fn default() -> Self {
        Jitter { size: 1.0, size_jitter: 0.0, rotation_jitter: 0.0, tint: Color::TRANSPARENT, tint_jitter: 0.0 }
    }
}

impl Jitter {
    /// A placement at (`x`, `y`) varied by `rng`.
    pub fn place(&self, x: f32, y: f32, rng: &mut Rng) -> Placement {
        let size = self.size.clamp(1.0 / 1024.0, MAX_SCALE);
        let scale = (size * (1.0 + rng.signed() * self.size_jitter.clamp(0.0, 0.95))).clamp(1.0 / 1024.0, MAX_SCALE);
        let rotation = rng.signed() * self.rotation_jitter.clamp(0.0, 180.0);
        let (u, v, w, z) = (rng.signed(), rng.signed(), rng.signed(), rng.unit());
        let j = self.tint_jitter.clamp(0.0, 1.0);
        let tint = if j <= 0.0 {
            self.tint
        } else if self.tint.a == 0 {
            // Lighter or darker, by up to 40% at full jitter.
            let amount = (z * j * 0.4 * 255.0).round() as u8;
            if u < 0.0 {
                Color::rgba(0, 0, 0, amount)
            } else {
                Color::rgba(255, 255, 255, amount)
            }
        } else {
            let ch = |c: u8, d: f32| (c as f32 + d * j * 64.0).round().clamp(0.0, 255.0) as u8;
            // One shift for lightness, a smaller one per channel for hue.
            Color::rgba(ch(self.tint.r, u * 0.7 + v * 0.3), ch(self.tint.g, u * 0.7 + w * 0.3), ch(self.tint.b, u * 0.7 - v * 0.3), self.tint.a)
        };
        Placement { x, y, scale, rotation, tint }
    }
}

/// The stamp brush's settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stamp {
    /// Board units between placements along the pointer's path.
    pub spacing: f32,
    /// How far a placement strays from the path, board units (any
    /// direction, up to this).
    pub scatter: f32,
    pub jitter: Jitter,
}

impl Default for Stamp {
    fn default() -> Self {
        Stamp { spacing: 40.0, scatter: 0.0, jitter: Jitter::default() }
    }
}

/// Places stamps along a pointer's path: the first at the first point, then
/// one every `spacing` along the path. Placement `n` is varied by
/// `Rng::at(seed, n)`, so the same path and seed make the same placements
/// however the path arrives.
#[derive(Debug, Clone)]
pub struct StampWalker {
    stamp: Stamp,
    seed: u64,
    count: u64,
    last: Option<Point>,
    /// Distance walked since the last placement.
    walked: f32,
}

impl StampWalker {
    pub fn new(stamp: Stamp, seed: u64) -> Self {
        StampWalker { stamp, seed, count: 0, last: None, walked: 0.0 }
    }

    /// How many placements so far.
    pub fn count(&self) -> u64 {
        self.count
    }

    fn put(&mut self, at: Point) -> Placement {
        let mut rng = Rng::at(self.seed, self.count);
        self.count += 1;
        let (a, r) = (rng.unit() * std::f32::consts::TAU, rng.unit().sqrt() * self.stamp.scatter.max(0.0));
        self.stamp.jitter.place(at.x + a.cos() * r, at.y + a.sin() * r, &mut rng)
    }

    /// The placements the path to `to` makes.
    pub fn walk(&mut self, to: Point) -> Vec<Placement> {
        let mut out = Vec::new();
        if !(to.x.is_finite() && to.y.is_finite()) {
            return out;
        }
        let Some(from) = self.last else {
            self.last = Some(to);
            out.push(self.put(to));
            return out;
        };
        let spacing = self.stamp.spacing.max(0.5);
        let (dx, dy) = (to.x - from.x, to.y - from.y);
        let len = (dx * dx + dy * dy).sqrt();
        if len <= 0.0 {
            return out;
        }
        let mut at = spacing - self.walked;
        while at <= len {
            let t = at / len;
            out.push(self.put(Point::new(from.x + dx * t, from.y + dy * t)));
            at += spacing;
        }
        // The distance from the last placement to the path's end.
        self.walked = len - (at - spacing);
        self.last = Some(to);
        out
    }
}

/// How a region is filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FillMethod {
    /// Poisson-disk: no two placements closer than the spacing, otherwise
    /// random (Bridson's algorithm).
    #[default]
    Poisson,
    /// A grid at the spacing, each point moved by up to half the spacing
    /// times the jitter.
    Grid,
}

impl FillMethod {
    pub fn parse(s: &str) -> Option<FillMethod> {
        match s {
            "poisson" => Some(FillMethod::Poisson),
            "grid" => Some(FillMethod::Grid),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            FillMethod::Poisson => "poisson",
            FillMethod::Grid => "grid",
        }
    }
}

/// A region fill's settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RegionFill {
    /// Board units between placements (Poisson: the least distance; grid:
    /// the cell).
    pub spacing: f32,
    /// The grid's jitter, 0 to 1 (Poisson ignores it).
    pub position_jitter: f32,
    pub method: FillMethod,
    pub seed: u64,
    pub jitter: Jitter,
}

impl Default for RegionFill {
    fn default() -> Self {
        RegionFill { spacing: 40.0, position_jitter: 0.8, method: FillMethod::Poisson, seed: 1, jitter: Jitter::default() }
    }
}

/// Placements filling the inside of `region` (board units), sorted into
/// 512-unit cells so each stored chunk covers a small area. Refused with a
/// sentence when nothing fits or too much would.
pub fn fill_region(region: &crate::clip::Clip, fill: &RegionFill) -> Result<Vec<Placement>, String> {
    let b = region.bounds;
    let spacing = fill.spacing.max(0.01);
    let estimate = (b.w as f64 / spacing as f64 + 1.0) * (b.h as f64 / spacing as f64 + 1.0);
    if !estimate.is_finite() || estimate > MAX_PLACEMENTS as f64 * 4.0 {
        return Err(TOO_MANY.to_string());
    }
    let mut points: Vec<Point> = match fill.method {
        FillMethod::Grid => {
            let mut out = Vec::new();
            let (cols, rows) = ((b.w / spacing).ceil() as i64 + 1, (b.h / spacing).ceil() as i64 + 1);
            // Centered on the region's box.
            let (x0, y0) = (b.center().x - (cols - 1) as f32 * spacing / 2.0, b.center().y - (rows - 1) as f32 * spacing / 2.0);
            let jit = fill.position_jitter.clamp(0.0, 1.0) * spacing / 2.0;
            for r in 0..rows {
                for c in 0..cols {
                    let mut rng = Rng::at(fill.seed ^ 0x6772_6964, (r as u64) << 32 | c as u64);
                    let p = Point::new(x0 + c as f32 * spacing + rng.signed() * jit, y0 + r as f32 * spacing + rng.signed() * jit);
                    if region.contains(p) {
                        out.push(p);
                    }
                }
            }
            out
        }
        FillMethod::Poisson => poisson(region, spacing, fill.seed),
    };
    if points.is_empty() {
        return Err(NOTHING_FITS.to_string());
    }
    if points.len() > MAX_PLACEMENTS {
        return Err(TOO_MANY.to_string());
    }
    // Sorted into cells, rows of cells top to bottom, so chunks are compact;
    // within a cell top to bottom, so placements lower down draw over the
    // ones behind them.
    let cell = |p: &Point| ((p.y / 512.0).floor() as i64, (p.x / 512.0).floor() as i64);
    points.sort_by(|a, c| cell(a).cmp(&cell(c)).then(a.y.total_cmp(&c.y)).then(a.x.total_cmp(&c.x)));
    Ok(points
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mut rng = Rng::at(fill.seed, i as u64);
            fill.jitter.place(p.x, p.y, &mut rng)
        })
        .collect())
}

/// Bridson's Poisson-disk sampling inside `region`.
fn poisson(region: &crate::clip::Clip, r: f32, seed: u64) -> Vec<Point> {
    let b = region.bounds;
    let cell = r / std::f32::consts::SQRT_2;
    let (cols, rows) = ((b.w / cell).ceil().max(1.0) as usize, (b.h / cell).ceil().max(1.0) as usize);
    let mut grid: Vec<u32> = vec![u32::MAX; cols * rows];
    let mut out: Vec<Point> = Vec::new();
    let mut active: Vec<usize> = Vec::new();
    let mut rng = Rng::new(seed ^ 0x706f_6973_736f_6e);
    let at = |p: Point| (((p.x - b.x) / cell) as usize).min(cols - 1) + (((p.y - b.y) / cell) as usize).min(rows - 1) * cols;
    let fits = |p: Point, out: &Vec<Point>, grid: &Vec<u32>| {
        if !(p.x >= b.x && p.y >= b.y && p.x <= b.right() && p.y <= b.bottom()) || !region.contains(p) {
            return false;
        }
        let (cx, cy) = (((p.x - b.x) / cell) as i64, ((p.y - b.y) / cell) as i64);
        for y in (cy - 2).max(0)..=(cy + 2).min(rows as i64 - 1) {
            for x in (cx - 2).max(0)..=(cx + 2).min(cols as i64 - 1) {
                let i = grid[x as usize + y as usize * cols];
                if i != u32::MAX {
                    let q = out[i as usize];
                    if (q.x - p.x).powi(2) + (q.y - p.y).powi(2) < r * r {
                        return false;
                    }
                }
            }
        }
        true
    };
    // Seeds: a coarse scan finds a first point in every part of the region
    // (a shape of several pieces is filled in each).
    let step = (r * 4.0).max(b.w.max(b.h) / 256.0);
    let mut y = b.y + step / 2.0;
    while y < b.bottom() {
        let mut x = b.x + step / 2.0;
        while x < b.right() {
            let p = Point::new(x + rng.signed() * step * 0.25, y + rng.signed() * step * 0.25);
            if fits(p, &out, &grid) {
                grid[at(p)] = out.len() as u32;
                active.push(out.len());
                out.push(p);
                // Grow from it before the next seed, so seeds land only where
                // growth has not reached.
                while let Some(&i) = active.last() {
                    let base = out[i];
                    let mut placed = false;
                    for _ in 0..30 {
                        let a = rng.unit() * std::f32::consts::TAU;
                        let d = r * (1.0 + rng.unit());
                        let q = Point::new(base.x + a.cos() * d, base.y + a.sin() * d);
                        if fits(q, &out, &grid) {
                            grid[at(q)] = out.len() as u32;
                            active.push(out.len());
                            out.push(q);
                            placed = true;
                            break;
                        }
                    }
                    if !placed {
                        active.pop();
                    }
                    if out.len() > MAX_PLACEMENTS {
                        return out;
                    }
                }
            }
            x += step;
        }
        y += step;
    }
    out
}

/// A raw scatter's placements, rechunked by `CHUNK_PLACEMENTS`.
pub fn chunked(placements: &[Placement]) -> impl Iterator<Item = &[Placement]> {
    placements.chunks(CHUNK_PLACEMENTS)
}

/// How a scatter reads in words (the MCP and the UI): "1,204 trees".
pub fn describe(e: &Element) -> String {
    let Some(raw) = &e.raw.scatter else { return String::new() };
    let name = e.scatter.as_ref().and_then(|s| s.def.as_ref()).map(|d| d.name.clone());
    let n = raw.placements.len();
    match name {
        Some(name) => format!("{} of symbol \"{}\"", count_text(n, "placement"), name),
        None => format!("{} of a symbol this canvas does not have ({})", count_text(n, "placement"), raw.symbol),
    }
}

fn count_text(n: usize, word: &str) -> String {
    let digits = n.to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("{grouped} {word}{}", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_placement_is_sixteen_bytes_and_reads_back() {
        let p = Placement { x: 12.5, y: -3.25, scale: 1.5, rotation: 90.0, tint: Color::rgba(10, 20, 30, 128) };
        let bytes = pack(&[p, p]);
        assert_eq!(bytes.len(), 2 * PLACEMENT_BYTES);
        let mut out = Vec::new();
        unpack(&bytes, &mut out);
        assert_eq!(out, vec![p, p]);
        let odd = Placement { scale: 0.3337, rotation: -30.0, ..p }.stored();
        assert!((odd.scale - 0.3337).abs() < 1.0 / 1024.0);
        assert!((odd.rotation - 330.0).abs() < 0.01);
    }

    #[test]
    fn stamps_are_the_same_whatever_the_pointer_path_is_cut_into() {
        let stamp = Stamp { spacing: 10.0, scatter: 5.0, jitter: Jitter { size_jitter: 0.3, rotation_jitter: 45.0, tint_jitter: 0.5, ..Jitter::default() } };
        let mut a = StampWalker::new(stamp, 7);
        let mut one = a.walk(Point::new(0.0, 0.0));
        one.extend(a.walk(Point::new(100.0, 0.0)));
        let mut b = StampWalker::new(stamp, 7);
        let mut two = b.walk(Point::new(0.0, 0.0));
        for x in [13.0, 27.5, 60.0, 61.0, 100.0] {
            two.extend(b.walk(Point::new(x, 0.0)));
        }
        assert_eq!(one.len(), 11);
        assert_eq!(one.len(), two.len());
        for (p, q) in one.iter().zip(&two) {
            assert!((p.x - q.x).abs() < 1e-3 && (p.y - q.y).abs() < 1e-3, "{p:?} {q:?}");
            assert_eq!((p.scale, p.rotation, p.tint), (q.scale, q.rotation, q.tint));
        }
        let mut c = StampWalker::new(stamp, 8);
        let mut other = c.walk(Point::new(0.0, 0.0));
        other.extend(c.walk(Point::new(100.0, 0.0)));
        assert_ne!(one, other, "another seed varies them otherwise");
    }
}
