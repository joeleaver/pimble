//! Paint layers drawn as tiles, and the cache that keeps them
//! (docs/DRAWING_CONTRACT.md section 3, "Rendering and its cache").
//!
//! - A paint layer is drawn into **tiles of [`TILE_PX`] × [`TILE_PX`]
//!   pixels** at [`PX_PER_UNIT`] pixels per board unit: tile (tx, ty) covers
//!   board units `tx × 128 .. (tx + 1) × 128` across and the same down.
//! - A tile's pixels are a replay of the layer's operations that reach it,
//!   in order ([`crate::brush`] for strokes, a picture's pixels for `pixels`).
//!   The **memory cache** ([`PaintCache`]) keeps each tile keyed by its layer,
//!   its place and the list of (operation, fingerprint) that touch it; a
//!   tile whose list is unchanged is a copy, never a replay.
//! - **An appended stroke redraws only the tiles under its new points**, and
//!   only the new dabs in them: a tile whose topmost operation is one of the
//!   layer's last few strokes keeps that stroke's coverage mask and the
//!   pixels under it, so new points are stamped into the mask and only the
//!   pixels they touch are laid down again. Every other change replays the
//!   tile.
//! - **A disk cache** is a [`TileStore`] the app hands the cache (the desktop
//!   one is [`DirTileStore`]; the browser's IndexedDB is the app's): a tile
//!   replayed whole is saved under a key that names its layer, place and
//!   operations, so a stale tile can never be found, and a tile not in
//!   memory is looked for there before it is replayed.
//! - A tile that needed a picture not yet loaded (a `pixels` operation) is
//!   drawn without it and not kept, so it is drawn again once the picture is
//!   there.
//! - **Operations that read the pixels below them** (fill, smudge, blur,
//!   move: wave 5) cross tiles, so each is worked out once over its whole
//!   area from the layer's tiles as they are just below it (kept apart from
//!   the layer's own tiles, keyed by the operation) and the result is laid
//!   into each tile it touches: a fill's coverage, a smudge's or blur's
//!   pixels, a move's lifted pixels. Such an operation's effective
//!   fingerprint ([`PaintOp::effective`]) folds in the operations below it
//!   that reach where it reads, so a change there replays every tile it
//!   touches, here and on disk.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform as Affine};

use crate::brush::{self, Dab, DabWalker, Lay, PxRect, StrokeBlend, PX_PER_UNIT};
use crate::model::{Blend, Rect};
use crate::paint_ops::{settings_hash, PaintBody, PaintOp, StrokeOp};
use crate::retouch::{self, PxBox};
use crate::render::{draw_picture_rect, draw_placed_picture, Picture, PictureSource, View};
use crate::snapshot::{id_hash, BoardSnapshot, Fx};

/// A tile's side in pixels.
pub const TILE_PX: u32 = 256;
/// A tile's side in board units.
pub const TILE_UNITS: f32 = TILE_PX as f32 / PX_PER_UNIT;
/// How many of a layer's last operations keep their mask in the tiles they
/// top, so appending to them stamps only the new dabs.
const KEEP_OPEN: usize = 4;
/// How many tiles one draw lays down between checks of the budget.
const EVICT_EVERY: usize = 32;
/// The memory cache's budget when none is given.
pub const DEFAULT_BUDGET: usize = 256 << 20;
const TILE_BYTES: usize = (TILE_PX * TILE_PX * 4) as usize;
const MASK_LEN: usize = (TILE_PX * TILE_PX) as usize;

/// The board rectangle of tile (`tx`, `ty`).
pub fn tile_rect(tx: i32, ty: i32) -> Rect {
    Rect::new(tx as f32 * TILE_UNITS, ty as f32 * TILE_UNITS, TILE_UNITS, TILE_UNITS)
}

/// The tiles a board rectangle meets, as (tx0..=tx1, ty0..=ty1).
pub fn tiles_of(r: &Rect) -> Option<(i32, i32, i32, i32)> {
    let r = r.normalized();
    if !r.is_finite() || r.w < 0.0 || r.h < 0.0 {
        return None;
    }
    let f = |v: f32| (v / TILE_UNITS).floor().clamp(-1e7, 1e7) as i32;
    // A rectangle ending exactly on a tile edge does not reach the next one.
    let g = |v: f32| ((v / TILE_UNITS).ceil() - 1.0).clamp(-1e7, 1e7) as i32;
    let (x0, y0) = (f(r.x), f(r.y));
    Some((x0, y0, g(r.right()).max(x0), g(r.bottom()).max(y0)))
}

/// The coarsest level of the tile pyramid: a level-`k` tile covers
/// 2^k × 2^k tiles of level 0 at the same [`TILE_PX`] pixels, so a view
/// zoomed far out reads a few coarse tiles instead of every full one.
pub const MAX_LEVEL: u8 = 12;

/// The pyramid level a view at `scale` (screen pixels per board unit)
/// reads: the coarsest whose density is still at least the screen's.
pub fn level_for_scale(scale: f32) -> u8 {
    if !(scale > 0.0) || scale >= PX_PER_UNIT {
        return 0;
    }
    ((PX_PER_UNIT / scale).log2().floor().max(0.0) as u8).min(MAX_LEVEL)
}

/// The board rectangle of tile (`tx`, `ty`) of pyramid level `k`.
pub fn tile_rect_at(k: u8, tx: i32, ty: i32) -> Rect {
    let side = TILE_UNITS * (1u32 << k) as f32;
    Rect::new(tx as f32 * side, ty as f32 * side, side, side)
}

/// The tiles of level `k` a board rectangle meets.
fn tiles_of_at(r: &Rect, k: u8) -> Option<(i32, i32, i32, i32)> {
    let r = r.normalized();
    if !r.is_finite() || r.w < 0.0 || r.h < 0.0 {
        return None;
    }
    let side = TILE_UNITS * (1u32 << k) as f32;
    let f = |v: f32| (v / side).floor().clamp(-1e7, 1e7) as i32;
    let g = |v: f32| ((v / side).ceil() - 1.0).clamp(-1e7, 1e7) as i32;
    let (x0, y0) = (f(r.x), f(r.y));
    Some((x0, y0, g(r.right()).max(x0), g(r.bottom()).max(y0)))
}

/// Lay `src` (a whole tile) at half size into quadrant (`dx`, `dy`) of
/// `out`: a 2×2 box filter over premultiplied pixels.
fn halve_into(out: &mut [u8], src: &[u8], dx: usize, dy: usize) {
    let side = TILE_PX as usize;
    let half = side / 2;
    for y in 0..half {
        let (r0, r1) = (2 * y * side * 4, (2 * y + 1) * side * 4);
        let o = ((dy * half + y) * side + dx * half) * 4;
        for x in 0..half {
            let (a, b) = (r0 + 8 * x, r1 + 8 * x);
            for c in 0..4 {
                let sum = src[a + c] as u16 + src[a + 4 + c] as u16 + src[b + c] as u16 + src[b + 4 + c] as u16;
                out[o + 4 * x + c] = ((sum + 2) / 4) as u8;
            }
        }
    }
}

/// Where a tile is kept on disk: a hash of its layer, its place and the
/// operations that touch it (each with its fingerprint).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileKey(pub u128);

impl TileKey {
    /// 32 hex digits, for a file name or an IndexedDB key.
    pub fn hex(&self) -> String {
        format!("{:032x}", self.0)
    }

    fn of(layer: &str, tx: i32, ty: i32, ops: &[(u64, u64)]) -> TileKey {
        let hash = |salt: u64| {
            let mut h = Fx::default();
            salt.hash(&mut h);
            layer.hash(&mut h);
            (tx, ty).hash(&mut h);
            ops.hash(&mut h);
            h.finish()
        };
        TileKey(((hash(0x7069_6d62) as u128) << 64) | hash(0x6c65_7469_6c65) as u128)
    }
}

/// A disk cache of tiles (contract: "the user's cache directory on the
/// desktop and IndexedDB in the browser"). Pixels are [`TILE_PX`]² × 4
/// premultiplied RGBA bytes. Both calls may fail silently: the cache is
/// derived and disposable.
pub trait TileStore: Send + Sync {
    fn load(&self, key: &TileKey) -> Option<Vec<u8>>;
    fn save(&self, key: &TileKey, pixels: &[u8]);
}

/// A [`TileStore`] in a directory (the desktop's: `<cache dir>/pimble/tiles`),
/// a PNG per tile (losslessly compressed premultiplied pixels).
#[cfg(not(target_arch = "wasm32"))]
pub struct DirTileStore {
    dir: std::path::PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl DirTileStore {
    /// Tiles in `dir`, made when the first tile is saved.
    pub fn new(dir: impl Into<std::path::PathBuf>) -> DirTileStore {
        DirTileStore { dir: dir.into() }
    }

    fn path(&self, key: &TileKey) -> std::path::PathBuf {
        let hex = key.hex();
        self.dir.join(&hex[..2]).join(format!("{hex}.png"))
    }

    /// Remove the least recently written tiles until the directory holds at
    /// most `max_bytes`. Answers how many it removed.
    pub fn prune(&self, max_bytes: u64) -> usize {
        let mut files: Vec<(std::time::SystemTime, u64, std::path::PathBuf)> = Vec::new();
        let Ok(subdirs) = std::fs::read_dir(&self.dir) else { return 0 };
        for sub in subdirs.flatten() {
            let Ok(entries) = std::fs::read_dir(sub.path()) else { continue };
            for f in entries.flatten() {
                if let Ok(meta) = f.metadata() {
                    files.push((meta.modified().unwrap_or(std::time::UNIX_EPOCH), meta.len(), f.path()));
                }
            }
        }
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        let mut removed = 0;
        for (_, len, path) in files {
            if total <= max_bytes {
                break;
            }
            if std::fs::remove_file(path).is_ok() {
                total = total.saturating_sub(len);
                removed += 1;
            }
        }
        removed
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl TileStore for DirTileStore {
    fn load(&self, key: &TileKey) -> Option<Vec<u8>> {
        let bytes = std::fs::read(self.path(key)).ok()?;
        let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).ok()?.into_rgba8();
        (img.dimensions() == (TILE_PX, TILE_PX)).then(|| img.into_raw())
    }

    fn save(&self, key: &TileKey, pixels: &[u8]) {
        use image::ImageEncoder;
        let path = self.path(key);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut out = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new_with_quality(
            &mut out,
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::Sub,
        );
        if encoder.write_image(pixels, TILE_PX, TILE_PX, image::ExtendedColorType::Rgba8).is_ok() {
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, &out).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }
}

/// What the cache has done, for tests and the spike's numbers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Tiles drawn from scratch.
    pub replayed: u64,
    /// Tiles brought up to date by stamping a stroke's new dabs.
    pub appended: u64,
    /// Tiles a stroke was laid onto without replaying what was under it.
    pub stacked: u64,
    /// Tiles read from the disk cache.
    pub loaded: u64,
    /// Tiles answered from memory unchanged.
    pub hits: u64,
    /// Coarse tiles (pyramid levels above 0) made from the four below.
    pub merged: u64,
}

/// The tile cache: memory first, then a [`TileStore`]. Shared by snapshots
/// of one board (a snapshot brought up to date keeps its cache), and safe to
/// use from the thread that renders pictures in text.
pub struct PaintCache {
    inner: Mutex<Inner>,
    store: Option<Arc<dyn TileStore>>,
    budget: usize,
}

impl std::fmt::Debug for PaintCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.lock().unwrap();
        write!(f, "PaintCache({} tiles, {} strokes' dabs)", inner.tiles.len(), inner.dabs.len())
    }
}

impl Default for PaintCache {
    fn default() -> Self {
        PaintCache { inner: Mutex::new(Inner::default()), store: None, budget: DEFAULT_BUDGET }
    }
}

impl PaintCache {
    /// A memory cache of [`DEFAULT_BUDGET`] bytes.
    pub fn new() -> Arc<PaintCache> {
        Arc::new(PaintCache::default())
    }

    /// A cache of `budget` bytes in memory, backed by `store`.
    pub fn with_store(store: Option<Arc<dyn TileStore>>, budget: usize) -> Arc<PaintCache> {
        Arc::new(PaintCache { inner: Mutex::new(Inner::default()), store, budget: budget.max(TILE_BYTES * 4) })
    }

    /// What it has done so far.
    pub fn stats(&self) -> CacheStats {
        self.inner.lock().unwrap().stats
    }

    /// Bytes held in memory (tiles, kept masks, dabs).
    pub fn memory_bytes(&self) -> usize {
        self.inner.lock().unwrap().bytes()
    }

    /// Forget every tile in memory (the disk cache stays).
    pub fn clear(&self) {
        *self.inner.lock().unwrap() = Inner::default();
    }
    /// Draw paint layer `layer` (an index into the snapshot's layers) into
    /// `target` over `region` of `view`.
    pub(crate) fn draw_layer(&self, target: &mut Pixmap, snapshot: &BoardSnapshot, layer: usize, view: &View, region: &Rect, pictures: &dyn PictureSource) {
        self.draw_layer_checked(target, snapshot, layer, view, region, pictures);
    }

    /// [`PaintCache::draw_layer`], answering whether everything it drew was
    /// complete (every picture it needed was there).
    pub(crate) fn draw_layer_checked(&self, target: &mut Pixmap, snapshot: &BoardSnapshot, layer: usize, view: &View, region: &Rect, pictures: &dyn PictureSource) -> bool {
        let Some(l) = snapshot.layers.get(layer) else { return true };
        let ops = snapshot.paint_ops(&l.id);
        let Some(bounds) = snapshot.paint_bounds(&l.id) else { return true };
        let Some(area) = region.intersect(&bounds) else { return true };
        // Zoomed out, the pyramid level whose density is the screen's: a
        // county-sized layer fitted to a window is about 130 tiles of level
        // 3, not 8,000 of level 0 (2 GB, past any budget, so every step
        // replayed or reloaded them all).
        let k = level_for_scale(view.scale);
        let Some((x0, y0, x1, y1)) = tiles_of_at(&area, k) else { return true };
        let vt = view.transform();
        let recent_from = ops.len().saturating_sub(KEEP_OPEN);
        let ctx = Ctx { layer: &l.id, ops, store: self.store.as_deref(), pictures };
        let mut inner = self.inner.lock().unwrap();
        inner.clock += 1;
        let mut complete = true;
        let mut touching: Vec<(usize, bool)> = Vec::new();
        let mut drawn = 0usize;
        for ty in y0..=y1 {
            for tx in x0..=x1 {
                let rect = tile_rect_at(k, tx, ty);
                if k > 0 {
                    let budget = self.budget;
                    let Some(entry) = inner.coarse(&ctx, k, tx, ty, budget) else { continue };
                    complete &= entry.complete;
                    draw_picture_rect(target, &entry.picture, rect, None, vt, 1.0, false, false);
                    drawn += 1;
                    if drawn % EVICT_EVERY == 0 {
                        inner.clock += 1;
                        inner.evict(self.budget);
                    }
                    continue;
                }
                touching.clear();
                touching.extend(ops.iter().enumerate().filter(|(_, o)| o.touches(&rect)).map(|(i, _)| (i, i >= recent_from)));
                if touching.is_empty() {
                    continue;
                }
                let entry = inner.tile(&ctx, None, tx, ty, &touching);
                complete &= entry.complete;
                draw_picture_rect(target, &entry.picture, rect, None, vt, 1.0, false, view.scale >= PX_PER_UNIT);
                // A draw over more tiles than the budget holds (a large
                // layer zoomed out, its thumbnail, a whole-canvas render)
                // keeps to the budget as it goes: the tiles already drawn
                // into `target` may go, instead of every tile of the area
                // being held until the end (docs/DRAWING_CONTRACT.md "Large
                // canvases": 25,000 dabs over 12 km held 2.5 GB).
                drawn += 1;
                if drawn % EVICT_EVERY == 0 {
                    inner.clock += 1;
                    inner.evict(self.budget);
                }
            }
        }
        inner.evict(self.budget);
        complete
    }

    /// The pixels of tile (`tx`, `ty`) of paint layer `layer`, as
    /// premultiplied RGBA bytes ([`TILE_PX`] square), replayed when needed.
    /// `None` when nothing of the layer reaches it.
    pub fn tile_pixels(&self, snapshot: &BoardSnapshot, layer: &str, tx: i32, ty: i32, pictures: &dyn PictureSource) -> Option<Vec<u8>> {
        let ops = snapshot.paint_ops(layer);
        let rect = tile_rect(tx, ty);
        let recent_from = ops.len().saturating_sub(KEEP_OPEN);
        let touching: Vec<(usize, bool)> = ops.iter().enumerate().filter(|(_, o)| o.touches(&rect)).map(|(i, _)| (i, i >= recent_from)).collect();
        if touching.is_empty() {
            return None;
        }
        let ctx = Ctx { layer, ops, store: self.store.as_deref(), pictures };
        let mut inner = self.inner.lock().unwrap();
        inner.clock += 1;
        let out = inner.tile(&ctx, None, tx, ty, &touching).picture.pixmap().data().to_vec();
        inner.evict(self.budget);
        Some(out)
    }
}

/// One layer's operations being drawn, and where from.
struct Ctx<'a> {
    layer: &'a str,
    /// Its operations, bottom to top.
    ops: &'a [PaintOp],
    store: Option<&'a dyn TileStore>,
    pictures: &'a dyn PictureSource,
}

struct TileEntry {
    /// (op id hash, effective fingerprint) of the operations it was drawn
    /// from.
    ops: Vec<(u64, u64)>,
    picture: Picture,
    open: Option<Box<Open>>,
    /// Every picture it needed was there.
    complete: bool,
    used: u64,
}

/// The topmost stroke of a tile, kept so new dabs stamp onto it.
struct Open {
    op: u64,
    lineage: u64,
    dabs_done: usize,
    /// The tile's pixels before the stroke.
    base: Vec<u8>,
    mask: Vec<f32>,
}

/// A stroke's dabs, kept so appended points only walk on from where it
/// stopped.
struct DabEntry {
    stroke: Arc<StrokeOp>,
    look: u64,
    walker: DabWalker,
    dabs: Vec<Dab>,
    /// Changes when the dabs are made again from scratch.
    lineage: u64,
    used: u64,
}

/// What an operation that reads the pixels below it made of them, worked
/// out once over its whole area (it crosses tiles) and laid into each tile
/// it touches.
enum Derived {
    /// A fill's coverage per tile ([`retouch::flood`]).
    Fill(HashMap<(i32, i32), Vec<u8>>),
    /// A smudge's or blur's pixels over its area: every pixel of the layer
    /// there once it is done (premultiplied).
    Patch { area: PxBox, data: Vec<u8> },
    /// A move's lifted pixels: the layer under its selection's box, with
    /// what is outside the selection cleared (`None`: nothing there).
    Lifted { area: PxBox, pixmap: Option<Pixmap> },
    /// Nothing to lay down (an area too large to work over).
    Nothing,
}

impl Derived {
    fn bytes(&self) -> usize {
        match self {
            Derived::Fill(m) => m.len() * MASK_LEN,
            Derived::Patch { data, .. } => data.len(),
            Derived::Lifted { pixmap, .. } => pixmap.as_ref().map_or(0, |p| p.data().len()),
            Derived::Nothing => 0,
        }
    }
}

struct DerivedEntry {
    value: Arc<Derived>,
    complete: bool,
    used: u64,
}

/// The most pixels a smudge, blur or move works over at once (4096²): one
/// reaching further draws nothing (docs/DRAWING_CONTRACT.md, "Built in
/// wave 5 (model)").
const MAX_WORK_PX: usize = 4096 * 4096;

#[derive(Default)]
struct Inner {
    tiles: HashMap<(String, i32, i32), TileEntry>,
    dabs: HashMap<String, DabEntry>,
    derived: HashMap<u64, DerivedEntry>,
    clock: u64,
    lineage: u64,
    scratch: Vec<f32>,
    stats: CacheStats,
}

impl Inner {
    fn bytes(&self) -> usize {
        let tiles: usize = self.tiles.values().map(|t| TILE_BYTES + t.open.as_ref().map_or(0, |_| TILE_BYTES + MASK_LEN * 4)).sum();
        let dabs: usize = self.dabs.values().map(|d| d.dabs.len() * std::mem::size_of::<Dab>()).sum();
        let derived: usize = self.derived.values().map(|d| d.value.bytes()).sum();
        tiles + dabs + derived
    }

    /// Bring stroke `id`'s dabs up to date with `stroke`.
    fn update_dabs(&mut self, id: &str, stroke: &Arc<StrokeOp>) {
        let look = settings_hash(&stroke.settings, stroke.color, stroke.blend);
        let clock = self.clock;
        if let Some(d) = self.dabs.get_mut(id) {
            d.used = clock;
            if Arc::ptr_eq(&d.stroke, stroke) {
                return;
            }
            let walked = d.walker.fed();
            if d.look == look && stroke.points.len() >= walked && stroke.points[..walked] == d.stroke.points[..walked] {
                d.walker.feed(&stroke.points[walked..], &mut d.dabs);
                d.stroke = stroke.clone();
                return;
            }
        }
        self.lineage += 1;
        let mut walker = DabWalker::new(&stroke.settings, brush::seed_of(id));
        let mut dabs = Vec::new();
        walker.feed(&stroke.points, &mut dabs);
        self.dabs.insert(id.to_string(), DabEntry { stroke: stroke.clone(), look, walker, dabs, lineage: self.lineage, used: clock });
    }

    /// Tile (`tx`, `ty`) of the layer drawn from `touching` (indices into
    /// `ctx.ops` of the operations that reach it, with whether each is one
    /// of the layer's last few), as the layer is (`upto` `None`) or as it is
    /// just below operation `upto` (what a fill, smudge, blur or move reads).
    fn tile(&mut self, ctx: &Ctx<'_>, upto: Option<usize>, tx: i32, ty: i32, touching: &[(usize, bool)]) -> &TileEntry {
        let ops: Vec<(&PaintOp, bool)> = touching.iter().map(|(i, r)| (&ctx.ops[*i], *r)).collect();
        let want: Vec<(u64, u64)> = ops.iter().map(|(o, _)| (id_hash(&o.raw.id), o.effective)).collect();
        for (o, _) in &ops {
            if let PaintBody::Stroke(s) = &o.raw.body {
                self.update_dabs(&o.raw.id, s);
            }
        }
        let key_layer = match upto {
            None => ctx.layer.to_string(),
            // Below an operation: kept apart from the layer's own tiles.
            Some(k) => format!("{}\u{1}{}", ctx.layer, ctx.ops[k].raw.id),
        };
        let store = if upto.is_none() { ctx.store } else { None };
        let key = (key_layer, tx, ty);
        let clock = self.clock;
        let (ox, oy) = (tx as i64 * TILE_PX as i64, ty as i64 * TILE_PX as i64);
        let rect = tile_rect(tx, ty);
        // In memory: unchanged, or a stroke grew or was laid on top.
        let mut fresh = true;
        {
            let Inner { tiles, dabs, stats, .. } = &mut *self;
            if let Some(entry) = tiles.get_mut(&key) {
                entry.used = clock;
                if entry.ops == want {
                    stats.hits += 1;
                    fresh = false;
                } else if let Some(done) = advance(entry, &want, &ops, dabs, ox, oy, &rect) {
                    match done {
                        Advanced::Appended => stats.appended += 1,
                        Advanced::Stacked => stats.stacked += 1,
                    }
                    entry.ops = want.clone();
                    fresh = false;
                }
            }
        }
        if !fresh {
            return &self.tiles[&key];
        }
        let disk_key = TileKey::of(&key.0, tx, ty, &want);
        if let Some(px) = store.and_then(|s| s.load(&disk_key)).filter(|px| px.len() == TILE_BYTES) {
            self.stats.loaded += 1;
            let picture = Picture::from_premultiplied(TILE_PX, TILE_PX, px).expect("a tile's size");
            self.tiles.insert(key.clone(), TileEntry { ops: want, picture, open: None, complete: true, used: clock });
            return &self.tiles[&key];
        }
        // Replay: first what the operations that read the pixels below them
        // made of those pixels.
        let mut derived: Vec<Option<Arc<Derived>>> = Vec::with_capacity(ops.len());
        let mut complete = true;
        for (i, _) in touching {
            if ctx.ops[*i].raw.body.is_local() {
                derived.push(None);
            } else {
                let (d, c) = self.ensure_derived(ctx, *i);
                complete &= c;
                derived.push(Some(d));
            }
        }
        self.stats.replayed += 1;
        let Inner { tiles, dabs, scratch, .. } = &mut *self;
        let (mut entry, drawn) = replay(&want, &ops, &derived, dabs, scratch, tx, ty, &rect, ctx.pictures, clock);
        complete &= drawn;
        if complete {
            if let Some(s) = store {
                s.save(&disk_key, entry.picture.pixmap().data());
            }
        } else {
            // Drawn without a picture that is not here yet: shown now,
            // drawn again next time.
            entry.ops.push((0, 0));
            entry.complete = false;
        }
        tiles.insert(key.clone(), entry);
        &tiles[&key]
    }

    /// Tile (`tx`, `ty`) of pyramid level `k` of the layer (level 0 is
    /// [`Inner::tile`]), or `None` when nothing of the layer reaches it. A
    /// coarse tile is its four children at half size, keyed like a full one
    /// by the operations that touch it (every child's are among them, so an
    /// unchanged list is an unchanged tile), in memory and on disk.
    fn coarse(&mut self, ctx: &Ctx<'_>, k: u8, tx: i32, ty: i32, budget: usize) -> Option<&TileEntry> {
        let rect = tile_rect_at(k, tx, ty);
        let recent_from = ctx.ops.len().saturating_sub(KEEP_OPEN);
        let touching: Vec<(usize, bool)> = ctx.ops.iter().enumerate().filter(|(_, o)| o.touches(&rect)).map(|(i, _)| (i, i >= recent_from)).collect();
        if touching.is_empty() {
            return None;
        }
        if k == 0 {
            return Some(self.tile(ctx, None, tx, ty, &touching));
        }
        let want: Vec<(u64, u64)> = touching.iter().map(|(i, _)| (id_hash(&ctx.ops[*i].raw.id), ctx.ops[*i].effective)).collect();
        let key = (format!("{}\u{2}{}", ctx.layer, k), tx, ty);
        let clock = self.clock;
        let mut fresh = true;
        if let Some(entry) = self.tiles.get_mut(&key) {
            entry.used = clock;
            if entry.ops == want {
                self.stats.hits += 1;
                fresh = false;
            }
        }
        if !fresh {
            return self.tiles.get(&key);
        }
        let disk_key = TileKey::of(&key.0, tx, ty, &want);
        if let Some(px) = ctx.store.and_then(|s| s.load(&disk_key)).filter(|px| px.len() == TILE_BYTES) {
            self.stats.loaded += 1;
            let picture = Picture::from_premultiplied(TILE_PX, TILE_PX, px).expect("a tile's size");
            self.tiles.insert(key.clone(), TileEntry { ops: want, picture, open: None, complete: true, used: clock });
            return self.tiles.get(&key);
        }
        // Only pictures here (a placed picture, Flatten's pieces): drawn
        // straight at this level's scale, without the 4^k full tiles under
        // it (a county-sized picture fitted: 130 draws, not 8,000 replays).
        if touching.iter().all(|(i, _)| matches!(ctx.ops[*i].raw.body, PaintBody::Pixels(_))) {
            let k_scale = PX_PER_UNIT / (1u32 << k) as f32;
            let to_tile = Affine::from_row(k_scale, 0.0, 0.0, k_scale, -rect.x * k_scale, -rect.y * k_scale);
            let mut pixmap = Pixmap::new(TILE_PX, TILE_PX).expect("a tile's size");
            let mut complete = true;
            for (i, _) in &touching {
                let PaintBody::Pixels(p) = &ctx.ops[*i].raw.body else { continue };
                match ctx.pictures.picture(&p.src) {
                    Some(pic) => draw_pixels_op(&mut pixmap, p, &pic, to_tile, false),
                    None => complete = false,
                }
            }
            self.stats.merged += 1;
            return Some(self.keep_coarse(ctx, key, disk_key, want, pixmap.take(), complete));
        }
        let mut px = vec![0u8; TILE_BYTES];
        let mut complete = true;
        for (dx, dy) in [(0usize, 0usize), (1, 0), (0, 1), (1, 1)] {
            let (cx, cy) = (tx * 2 + dx as i32, ty * 2 + dy as i32);
            let Some(child) = self.coarse(ctx, k - 1, cx, cy, budget) else { continue };
            complete &= child.complete;
            halve_into(&mut px, child.picture.pixmap().data(), dx, dy);
            // Material for this tile, not what is shown: the first to go,
            // before the coarse tiles drawn earlier in the same step (by
            // age alone, a fitted step evicted its own level and replayed
            // all 8,000 tiles again the next time).
            let child_key = if k == 1 { (ctx.layer.to_string(), cx, cy) } else { (format!("{}\u{2}{}", ctx.layer, k - 1), cx, cy) };
            if let Some(e) = self.tiles.get_mut(&child_key) {
                e.used = 0;
            }
        }
        self.stats.merged += 1;
        // Made from many tiles below (a thumbnail's level 8 is 65,536 of
        // level 0 at most): keep to the budget while going, sparing this
        // tile, which is put in after.
        self.clock += 1;
        self.evict(budget);
        Some(self.keep_coarse(ctx, key, disk_key, want, px, complete))
    }

    /// Keep a coarse tile just made: in memory, and on disk when complete
    /// (one that lacked a picture is drawn again next time).
    fn keep_coarse(&mut self, ctx: &Ctx<'_>, key: (String, i32, i32), disk_key: TileKey, want: Vec<(u64, u64)>, px: Vec<u8>, complete: bool) -> &TileEntry {
        let mut entry = TileEntry { ops: want, picture: Picture::from_premultiplied(TILE_PX, TILE_PX, px).expect("a tile's size"), open: None, complete, used: self.clock };
        if complete {
            if let Some(s) = ctx.store {
                s.save(&disk_key, entry.picture.pixmap().data());
            }
        } else {
            entry.ops.push((0, 0));
        }
        self.tiles.insert(key.clone(), entry);
        &self.tiles[&key]
    }

    /// The tiles of the layer below operation `k` that `touching` reach,
    /// or `None` when nothing reaches tile (`tx`, `ty`).
    fn below_tile(&mut self, ctx: &Ctx<'_>, k: usize, tx: i32, ty: i32) -> Option<&TileEntry> {
        let rect = tile_rect(tx, ty);
        let touching: Vec<(usize, bool)> = ctx.ops[..k].iter().enumerate().filter(|(_, o)| o.touches(&rect)).map(|(i, _)| (i, false)).collect();
        if touching.is_empty() {
            return None;
        }
        Some(self.tile(ctx, Some(k), tx, ty, &touching))
    }

    /// The layer's pixels below operation `k` over `area`, premultiplied,
    /// and whether they are complete.
    fn region(&mut self, ctx: &Ctx<'_>, k: usize, area: PxBox) -> (Vec<u8>, bool) {
        let mut out = vec![0u8; area.w * area.h * 4];
        let mut complete = true;
        let (tx0, ty0, tx1, ty1) = area.tiles();
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                let Some(((x0, y0, x1, y1), (bx, by))) = area.in_tile(tx, ty) else { continue };
                let Some(entry) = self.below_tile(ctx, k, tx, ty) else { continue };
                complete &= entry.complete;
                let src = entry.picture.pixmap().data();
                let n = (x1 - x0) * 4;
                for row in 0..(y1 - y0) {
                    let s = ((y0 + row) * TILE_PX as usize + x0) * 4;
                    let d = ((by + row) * area.w + bx) * 4;
                    out[d..d + n].copy_from_slice(&src[s..s + n]);
                }
            }
        }
        (out, complete)
    }

    /// What operation `k` (a fill, smudge, blur or move) makes of the
    /// pixels below it, worked out once per [`PaintOp::derived`] key, and
    /// whether it is complete.
    fn ensure_derived(&mut self, ctx: &Ctx<'_>, k: usize) -> (Arc<Derived>, bool) {
        let op = &ctx.ops[k];
        let clock = self.clock;
        if let Some(e) = self.derived.get_mut(&op.derived) {
            e.used = clock;
            if e.complete {
                return (e.value.clone(), true);
            }
        }
        let mut complete = true;
        let value = match &op.raw.body {
            PaintBody::Fill(f) => match PxBox::of_rect(&f.clip) {
                Some(clip) => {
                    let seed = ((f.seed.x * PX_PER_UNIT).floor() as i64, (f.seed.y * PX_PER_UNIT).floor() as i64);
                    let mut fetch = |tx: i32, ty: i32| match self.below_tile(ctx, k, tx, ty) {
                        Some(entry) => {
                            complete &= entry.complete;
                            Some(entry.picture.pixmap().data().to_vec())
                        }
                        None => None,
                    };
                    Derived::Fill(retouch::flood(clip, seed, f.tolerance, f.contiguous, &mut fetch))
                }
                None => Derived::Nothing,
            },
            PaintBody::Retouch(r) => match op.bounds.and_then(|b| PxBox::of_rect(&b)) {
                Some(area) if area.w * area.h <= MAX_WORK_PX => {
                    let margin = retouch::read_margin(r.effect);
                    let read = area.grown(margin);
                    let (bytes, c) = self.region(ctx, k, read);
                    complete &= c;
                    let mut buf: Vec<f32> = bytes.iter().map(|b| *b as f32).collect();
                    retouch::retouch(&mut buf, read.w, read.h, read.x0, read.y0, r, brush::seed_of(&op.raw.id));
                    let m = margin as usize;
                    let mut cut = Vec::with_capacity(area.w * area.h * 4);
                    for row in 0..area.h {
                        let s = ((row + m) * read.w + m) * 4;
                        cut.extend_from_slice(&buf[s..s + area.w * 4]);
                    }
                    Derived::Patch { area, data: retouch::to_bytes(&cut) }
                }
                _ => Derived::Nothing,
            },
            PaintBody::Move(mv) => match mv.selection.bounds().and_then(|b| PxBox::of_rect(&b)) {
                Some(area) if area.w * area.h <= MAX_WORK_PX => {
                    let (mut bytes, c) = self.region(ctx, k, area);
                    complete &= c;
                    let mut any = false;
                    for y in 0..area.h {
                        let by = ((area.y0 + y as i64) as f32 + 0.5) / PX_PER_UNIT;
                        for x in 0..area.w {
                            let i = (y * area.w + x) * 4;
                            if bytes[i + 3] == 0 && bytes[i..i + 3] == [0, 0, 0] {
                                continue;
                            }
                            let bx = ((area.x0 + x as i64) as f32 + 0.5) / PX_PER_UNIT;
                            if mv.selection.contains(crate::model::Point::new(bx, by)) {
                                any = true;
                            } else {
                                bytes[i..i + 4].fill(0);
                            }
                        }
                    }
                    let pixmap = if any { tiny_skia::IntSize::from_wh(area.w as u32, area.h as u32).and_then(|s| Pixmap::from_vec(bytes, s)) } else { None };
                    Derived::Lifted { area, pixmap }
                }
                _ => Derived::Nothing,
            },
            _ => Derived::Nothing,
        };
        let value = Arc::new(value);
        self.derived.insert(op.derived, DerivedEntry { value: value.clone(), complete, used: clock });
        (value, complete)
    }

    /// Drop the least recently used tiles, dabs and derived results until
    /// under `budget`.
    fn evict(&mut self, budget: usize) {
        let mut bytes = self.bytes();
        if bytes <= budget {
            return;
        }
        let target = budget / 10 * 8;
        // (used, what: 0 tile, 1 dabs, 2 derived, id, x, y, derived key)
        let mut by_age: Vec<(u64, u8, String, i32, i32, u64)> = self
            .tiles
            .iter()
            .map(|((l, x, y), t)| (t.used, 0, l.clone(), *x, *y, 0))
            .chain(self.dabs.iter().map(|(id, d)| (d.used, 1, id.clone(), 0, 0, 0)))
            .chain(self.derived.iter().map(|(k, d)| (d.used, 2, String::new(), 0, 0, *k)))
            .collect();
        by_age.sort_by_key(|a| a.0);
        for (used, what, id, x, y, dk) in by_age {
            if bytes <= target || used == self.clock {
                break;
            }
            match what {
                0 => {
                    if let Some(t) = self.tiles.remove(&(id, x, y)) {
                        bytes = bytes.saturating_sub(TILE_BYTES + t.open.as_ref().map_or(0, |_| TILE_BYTES + MASK_LEN * 4));
                    }
                }
                1 => {
                    if let Some(d) = self.dabs.remove(&id) {
                        bytes = bytes.saturating_sub(d.dabs.len() * std::mem::size_of::<Dab>());
                    }
                }
                _ => {
                    if let Some(d) = self.derived.remove(&dk) {
                        bytes = bytes.saturating_sub(d.value.bytes());
                    }
                }
            }
        }
    }
}

enum Advanced {
    Appended,
    Stacked,
}

/// Bring a tile in memory up to date without replaying it, when the change
/// is a stroke on top that grew (its new dabs stamped) or a new stroke laid
/// on top of what the tile held. `None` when the tile must be replayed.
fn advance(entry: &mut TileEntry, want: &[(u64, u64)], ops: &[(&PaintOp, bool)], dabs: &HashMap<String, DabEntry>, ox: i64, oy: i64, rect: &Rect) -> Option<Advanced> {
    let (n, m) = (want.len(), entry.ops.len());
    let (top, recent) = *ops.last()?;
    let PaintBody::Stroke(stroke) = &top.raw.body else { return None };
    let d = dabs.get(&top.raw.id)?;
    let how = lay_of(stroke);
    let s = &stroke.settings;
    if n == m && n > 0 && entry.ops[..n - 1] == want[..n - 1] && entry.ops[n - 1].0 == want[n - 1].0 {
        let open = entry.open.as_mut()?;
        if open.op != want[n - 1].0 || open.lineage != d.lineage || d.dabs.len() < open.dabs_done {
            return None;
        }
        let mut dirty = PxRect::EMPTY;
        for dab in &d.dabs[open.dabs_done..] {
            if dab_meets(dab, rect) {
                dirty.union(brush::stamp(&mut open.mask, TILE_PX, ox, oy, dab, s.hardness, s.flow));
            }
        }
        open.dabs_done = d.dabs.len();
        let pixels = entry.picture.pixmap_mut().data_mut();
        brush::lay(pixels, Some(&open.base), &open.mask, TILE_PX, ox, oy, dirty, &how);
        return Some(Advanced::Appended);
    }
    if n == m + 1 && recent && entry.complete && entry.ops[..] == want[..m] {
        let base = entry.picture.pixmap().data().to_vec();
        let mut mask = vec![0.0f32; MASK_LEN];
        let mut dirty = PxRect::EMPTY;
        for dab in &d.dabs {
            if dab_meets(dab, rect) {
                dirty.union(brush::stamp(&mut mask, TILE_PX, ox, oy, dab, s.hardness, s.flow));
            }
        }
        let pixels = entry.picture.pixmap_mut().data_mut();
        brush::lay(pixels, Some(&base), &mask, TILE_PX, ox, oy, dirty, &how);
        entry.open = Some(Box::new(Open { op: want[n - 1].0, lineage: d.lineage, dabs_done: d.dabs.len(), base, mask }));
        return Some(Advanced::Stacked);
    }
    None
}

/// Replay a tile from its operations (`derived` beside them: what each
/// that reads the pixels below it made of them). Answers it and whether it
/// is complete (every picture it needed was there).
#[allow(clippy::too_many_arguments)]
fn replay(
    want: &[(u64, u64)],
    ops: &[(&PaintOp, bool)],
    derived: &[Option<Arc<Derived>>],
    dabs: &HashMap<String, DabEntry>,
    scratch: &mut Vec<f32>,
    tx: i32,
    ty: i32,
    rect: &Rect,
    pictures: &dyn PictureSource,
    clock: u64,
) -> (TileEntry, bool) {
    let (ox, oy) = (tx as i64 * TILE_PX as i64, ty as i64 * TILE_PX as i64);
    let mut picture = Picture::from_premultiplied(TILE_PX, TILE_PX, vec![0u8; TILE_BYTES]).expect("a tile's size");
    let mut open = None;
    let mut complete = true;
    if scratch.len() != MASK_LEN {
        *scratch = vec![0.0; MASK_LEN];
    }
    // Board units to tile pixels.
    let to_tile = Affine::from_row(PX_PER_UNIT, 0.0, 0.0, PX_PER_UNIT, -rect.x * PX_PER_UNIT, -rect.y * PX_PER_UNIT);
    let last = ops.len().saturating_sub(1);
    for (i, (op, recent)) in ops.iter().enumerate() {
        match &op.raw.body {
            PaintBody::Stroke(stroke) => {
                let Some(d) = dabs.get(&op.raw.id) else { continue };
                let s = &stroke.settings;
                let how = lay_of(stroke);
                if i == last && *recent {
                    let base = picture.pixmap().data().to_vec();
                    let mut mask = vec![0.0f32; MASK_LEN];
                    let mut dirty = PxRect::EMPTY;
                    for dab in d.dabs.iter().filter(|dab| dab_meets(dab, rect)) {
                        dirty.union(brush::stamp(&mut mask, TILE_PX, ox, oy, dab, s.hardness, s.flow));
                    }
                    brush::lay(picture.pixmap_mut().data_mut(), Some(&base), &mask, TILE_PX, ox, oy, dirty, &how);
                    open = Some(Box::new(Open { op: want[i].0, lineage: d.lineage, dabs_done: d.dabs.len(), base, mask }));
                } else {
                    let mut dirty = PxRect::EMPTY;
                    for dab in d.dabs.iter().filter(|dab| dab_meets(dab, rect)) {
                        dirty.union(brush::stamp(scratch, TILE_PX, ox, oy, dab, s.hardness, s.flow));
                    }
                    brush::lay(picture.pixmap_mut().data_mut(), None, scratch, TILE_PX, ox, oy, dirty, &how);
                    clear(scratch, dirty);
                }
            }
            PaintBody::Pixels(p) => match pictures.picture(&p.src) {
                Some(pic) => draw_pixels_op(picture.pixmap_mut(), p, &pic, to_tile, true),
                None => complete = false,
            },
            PaintBody::Fill(f) => {
                let Some(Derived::Fill(masks)) = derived[i].as_deref() else { continue };
                let Some(mask) = masks.get(&(tx, ty)) else { continue };
                for (m, v) in scratch.iter_mut().zip(mask) {
                    *m = *v as f32 / 255.0;
                }
                let all = PxRect { x0: 0, y0: 0, x1: TILE_PX, y1: TILE_PX };
                let how = Lay { color: f.color, blend: StrokeBlend::Normal, opacity: 1.0, grain: 0.0, wet_edge: 0.0 };
                brush::lay(picture.pixmap_mut().data_mut(), None, scratch, TILE_PX, ox, oy, all, &how);
                scratch.fill(0.0);
            }
            PaintBody::Retouch(_) => {
                let Some(Derived::Patch { area, data }) = derived[i].as_deref() else { continue };
                let Some(((x0, y0, x1, y1), (bx, by))) = area.in_tile(tx, ty) else { continue };
                let dst = picture.pixmap_mut().data_mut();
                let n = (x1 - x0) * 4;
                for row in 0..(y1 - y0) {
                    let s = ((by + row) * area.w + bx) * 4;
                    let d = ((y0 + row) * TILE_PX as usize + x0) * 4;
                    dst[d..d + n].copy_from_slice(&data[s..s + n]);
                }
            }
            PaintBody::Move(mv) => {
                let Some(Derived::Lifted { area, pixmap }) = derived[i].as_deref() else { continue };
                // Lift: the selection's pixels go.
                if let Some(((x0, y0, x1, y1), _)) = area.in_tile(tx, ty) {
                    let dst = picture.pixmap_mut().data_mut();
                    for y in y0..y1 {
                        let by = ((oy + y as i64) as f32 + 0.5) / PX_PER_UNIT;
                        for x in x0..x1 {
                            let bx = ((ox + x as i64) as f32 + 0.5) / PX_PER_UNIT;
                            if mv.selection.contains(crate::model::Point::new(bx, by)) {
                                let i = (y * TILE_PX as usize + x) * 4;
                                dst[i..i + 4].fill(0);
                            }
                        }
                    }
                }
                // Put down, through the transform.
                let (Some(t), Some(lifted)) = (mv.transform, pixmap) else { continue };
                let from_lifted = Affine::from_row(1.0 / PX_PER_UNIT, 0.0, 0.0, 1.0 / PX_PER_UNIT, area.x0 as f32 / PX_PER_UNIT, area.y0 as f32 / PX_PER_UNIT);
                let move_ts = Affine::from_row(t.a, t.b, t.c, t.d, t.e, t.f);
                let ts = to_tile.pre_concat(move_ts).pre_concat(from_lifted);
                let quality = if t.whole_pixel_shift().is_some() { FilterQuality::Nearest } else { FilterQuality::Bilinear };
                let paint = PixmapPaint { opacity: 1.0, blend_mode: tiny_skia::BlendMode::SourceOver, quality };
                picture.pixmap_mut().draw_pixmap(0, 0, lifted.as_ref(), &paint, ts, None);
            }
            PaintBody::Other(_) => {}
        }
    }
    (TileEntry { ops: want.to_vec(), picture, open, complete, used: clock }, complete)
}

/// Whether a rectangle's edges lie on whole paint pixels (what Flatten
/// writes).
/// A `pixels` operation drawn into a tile through `to_tile` (board units
/// to the tile's pixels). A placed picture (wave 6) goes through its
/// transform; Flatten's pieces lie on the grid as they are, with crisp
/// edges so neighbours neither overlap nor part (`full`: at level 0's own
/// density, where they fall on whole pixels).
fn draw_pixels_op(target: &mut Pixmap, p: &crate::paint_ops::PixelsOp, pic: &Picture, to_tile: Affine, full: bool) {
    let placed = p.transform.is_some() || !on_pixel_grid(&p.rect);
    let ts = match p.transform {
        Some(t) => to_tile.pre_concat(Affine::from_row(t.a, t.b, t.c, t.d, t.e, t.f)),
        None => to_tile,
    };
    let draw = |into: &mut Pixmap| {
        if placed {
            draw_placed_picture(into, pic, p.rect.normalized(), ts);
        } else {
            draw_picture_rect(into, pic, p.rect.normalized(), None, ts, 1.0, full, false);
        }
    };
    if p.blend == Blend::Normal {
        draw(target);
    } else {
        // Drawn on its own, then blended onto what is there.
        let mut alone = Pixmap::new(TILE_PX, TILE_PX).expect("a tile's size");
        draw(&mut alone);
        let paint = PixmapPaint { opacity: 1.0, blend_mode: p.blend.to_tiny(), quality: FilterQuality::Nearest };
        target.draw_pixmap(0, 0, alone.as_ref(), &paint, Affine::identity(), None);
    }
}

fn on_pixel_grid(r: &Rect) -> bool {
    let r = r.normalized();
    [r.x, r.y, r.right(), r.bottom()].iter().all(|v| {
        let px = v * PX_PER_UNIT;
        (px - px.round()).abs() < 1e-3
    })
}

fn clear(mask: &mut [f32], r: PxRect) {
    if r.is_empty() {
        return;
    }
    for y in r.y0..r.y1 {
        let row = (y * TILE_PX) as usize;
        mask[row + r.x0 as usize..row + r.x1 as usize].fill(0.0);
    }
}

fn dab_meets(dab: &Dab, rect: &Rect) -> bool {
    dab.x + dab.radius >= rect.x && dab.x - dab.radius <= rect.right() && dab.y + dab.radius >= rect.y && dab.y - dab.radius <= rect.bottom()
}

fn lay_of(stroke: &StrokeOp) -> Lay {
    Lay { color: stroke.color, blend: stroke.blend, opacity: stroke.settings.opacity, grain: stroke.settings.grain, wet_edge: stroke.settings.wet_edge }
}
