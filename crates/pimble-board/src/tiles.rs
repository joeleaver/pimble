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

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use tiny_skia::{Pixmap, Transform as Affine};

use crate::brush::{self, Dab, DabWalker, Lay, PxRect, PX_PER_UNIT};
use crate::model::Rect;
use crate::paint_ops::{settings_hash, PaintBody, PaintOp, StrokeOp};
use crate::render::{draw_picture_rect, Picture, PictureSource, View};
use crate::snapshot::{id_hash, BoardSnapshot, Fx};

/// A tile's side in pixels.
pub const TILE_PX: u32 = 256;
/// A tile's side in board units.
pub const TILE_UNITS: f32 = TILE_PX as f32 / PX_PER_UNIT;
/// How many of a layer's last operations keep their mask in the tiles they
/// top, so appending to them stamps only the new dabs.
const KEEP_OPEN: usize = 4;
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
        let Some(l) = snapshot.layers.get(layer) else { return };
        let ops = snapshot.paint_ops(&l.id);
        let Some(bounds) = snapshot.paint_bounds(&l.id) else { return };
        let Some(area) = region.intersect(&bounds) else { return };
        let Some((x0, y0, x1, y1)) = tiles_of(&area) else { return };
        let vt = view.transform();
        let recent_from = ops.len().saturating_sub(KEEP_OPEN);
        let mut inner = self.inner.lock().unwrap();
        inner.clock += 1;
        let mut touching: Vec<(&PaintOp, bool)> = Vec::new();
        for ty in y0..=y1 {
            for tx in x0..=x1 {
                let rect = tile_rect(tx, ty);
                touching.clear();
                touching.extend(
                    ops.iter()
                        .enumerate()
                        .filter(|(_, o)| o.bounds.is_some_and(|b| b.intersects(&rect)) && !matches!(o.raw.body, PaintBody::Other(_)))
                        .map(|(i, o)| (o, i >= recent_from)),
                );
                if touching.is_empty() {
                    continue;
                }
                let picture = inner.tile(self.store.as_deref(), &l.id, tx, ty, &touching, pictures);
                draw_picture_rect(target, picture, rect, None, vt, 1.0, false, view.scale >= PX_PER_UNIT);
            }
        }
        inner.evict(self.budget);
    }

    /// The pixels of tile (`tx`, `ty`) of paint layer `layer`, as
    /// premultiplied RGBA bytes ([`TILE_PX`] square), replayed when needed.
    /// `None` when nothing of the layer reaches it.
    pub fn tile_pixels(&self, snapshot: &BoardSnapshot, layer: &str, tx: i32, ty: i32, pictures: &dyn PictureSource) -> Option<Vec<u8>> {
        let ops = snapshot.paint_ops(layer);
        let rect = tile_rect(tx, ty);
        let recent_from = ops.len().saturating_sub(KEEP_OPEN);
        let touching: Vec<(&PaintOp, bool)> = ops
            .iter()
            .enumerate()
            .filter(|(_, o)| o.bounds.is_some_and(|b| b.intersects(&rect)) && !matches!(o.raw.body, PaintBody::Other(_)))
            .map(|(i, o)| (o, i >= recent_from))
            .collect();
        if touching.is_empty() {
            return None;
        }
        let mut inner = self.inner.lock().unwrap();
        inner.clock += 1;
        let out = inner.tile(self.store.as_deref(), layer, tx, ty, &touching, pictures).pixmap().data().to_vec();
        inner.evict(self.budget);
        Some(out)
    }
}

struct TileEntry {
    /// (op id hash, fingerprint) of the operations it was drawn from.
    ops: Vec<(u64, u64)>,
    picture: Picture,
    open: Option<Box<Open>>,
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

#[derive(Default)]
struct Inner {
    tiles: HashMap<(String, i32, i32), TileEntry>,
    dabs: HashMap<String, DabEntry>,
    clock: u64,
    lineage: u64,
    scratch: Vec<f32>,
    stats: CacheStats,
}

impl Inner {
    fn bytes(&self) -> usize {
        let tiles: usize = self.tiles.values().map(|t| TILE_BYTES + t.open.as_ref().map_or(0, |_| TILE_BYTES + MASK_LEN * 4)).sum();
        let dabs: usize = self.dabs.values().map(|d| d.dabs.len() * std::mem::size_of::<Dab>()).sum();
        tiles + dabs
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

    fn tile(&mut self, store: Option<&dyn TileStore>, layer: &str, tx: i32, ty: i32, ops: &[(&PaintOp, bool)], pictures: &dyn PictureSource) -> &Picture {
        let want: Vec<(u64, u64)> = ops.iter().map(|(o, _)| (id_hash(&o.raw.id), o.fingerprint)).collect();
        for (o, _) in ops {
            if let PaintBody::Stroke(s) = &o.raw.body {
                self.update_dabs(&o.raw.id, s);
            }
        }
        let key = (layer.to_string(), tx, ty);
        let clock = self.clock;
        let (ox, oy) = (tx as i64 * TILE_PX as i64, ty as i64 * TILE_PX as i64);
        let rect = tile_rect(tx, ty);
        let Inner { tiles, dabs, stats, .. } = self;
        // In memory: unchanged, or a stroke grew or was laid on top.
        let mut fresh = true;
        if let Some(entry) = tiles.get_mut(&key) {
            entry.used = clock;
            if entry.ops == want {
                stats.hits += 1;
                fresh = false;
            } else if let Some(done) = advance(entry, &want, ops, dabs, ox, oy, &rect) {
                match done {
                    Advanced::Appended => stats.appended += 1,
                    Advanced::Stacked => stats.stacked += 1,
                }
                entry.ops = want.clone();
                fresh = false;
            }
        }
        if fresh {
            let disk_key = TileKey::of(layer, tx, ty, &want);
            let loaded = store.and_then(|s| s.load(&disk_key)).filter(|px| px.len() == TILE_BYTES);
            let entry = match loaded {
                Some(px) => {
                    stats.loaded += 1;
                    let picture = Picture::from_premultiplied(TILE_PX, TILE_PX, px).expect("a tile's size");
                    TileEntry { ops: want.clone(), picture, open: None, used: clock }
                }
                None => {
                    stats.replayed += 1;
                    let (entry, complete) = replay(&want, ops, dabs, &mut self.scratch, ox, oy, &rect, pictures, clock);
                    if complete {
                        if let Some(s) = store {
                            s.save(&disk_key, entry.picture.pixmap().data());
                        }
                    } else {
                        // Drawn without a picture that is not here yet: shown
                        // now, drawn again next time.
                        let mut entry = entry;
                        entry.ops.push((0, 0));
                        tiles.insert(key.clone(), entry);
                        return &tiles[&key].picture;
                    }
                    entry
                }
            };
            tiles.insert(key.clone(), entry);
        }
        &tiles[&key].picture
    }

    /// Drop the least recently used tiles and dabs until under `budget`.
    fn evict(&mut self, budget: usize) {
        let mut bytes = self.bytes();
        if bytes <= budget {
            return;
        }
        let target = budget / 10 * 8;
        let mut by_age: Vec<(u64, bool, String, i32, i32)> = self
            .tiles
            .iter()
            .map(|((l, x, y), t)| (t.used, true, l.clone(), *x, *y))
            .chain(self.dabs.iter().map(|(id, d)| (d.used, false, id.clone(), 0, 0)))
            .collect();
        by_age.sort_by_key(|a| a.0);
        for (used, tile, id, x, y) in by_age {
            if bytes <= target || used == self.clock {
                break;
            }
            if tile {
                if let Some(t) = self.tiles.remove(&(id, x, y)) {
                    bytes = bytes.saturating_sub(TILE_BYTES + t.open.as_ref().map_or(0, |_| TILE_BYTES + MASK_LEN * 4));
                }
            } else if let Some(d) = self.dabs.remove(&id) {
                bytes = bytes.saturating_sub(d.dabs.len() * std::mem::size_of::<Dab>());
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
    if n == m + 1 && recent && entry.ops[..] == want[..m] {
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

/// Replay a tile from its operations. Answers it and whether it is
/// complete (every picture it needed was there).
#[allow(clippy::too_many_arguments)]
fn replay(
    want: &[(u64, u64)],
    ops: &[(&PaintOp, bool)],
    dabs: &HashMap<String, DabEntry>,
    scratch: &mut Vec<f32>,
    ox: i64,
    oy: i64,
    rect: &Rect,
    pictures: &dyn PictureSource,
    clock: u64,
) -> (TileEntry, bool) {
    let mut picture = Picture::from_premultiplied(TILE_PX, TILE_PX, vec![0u8; TILE_BYTES]).expect("a tile's size");
    let mut open = None;
    let mut complete = true;
    if scratch.len() != MASK_LEN {
        *scratch = vec![0.0; MASK_LEN];
    }
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
                Some(pic) => {
                    // Board units to tile pixels.
                    let ts = Affine::from_row(PX_PER_UNIT, 0.0, 0.0, PX_PER_UNIT, -rect.x * PX_PER_UNIT, -rect.y * PX_PER_UNIT);
                    draw_picture_rect(picture.pixmap_mut(), &pic, p.rect.normalized(), None, ts, 1.0, true, false);
                }
                None => complete = false,
            },
            PaintBody::Other(_) => {}
        }
    }
    (TileEntry { ops: want.to_vec(), picture, open, used: clock }, complete)
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
