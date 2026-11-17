//! The pixel work of the operations that read the pixels below them
//! (docs/DRAWING_CONTRACT.md section 3, wave 5): a fill's flood, a smudge's
//! drag, a blur's softening. Pure functions over premultiplied RGBA at
//! [`PX_PER_UNIT`] pixels per board unit, deterministic on one device; the
//! tile cache ([`crate::tiles`]) feeds them the layer's pixels below the
//! operation and keeps what they answer.

use std::collections::HashMap;

use crate::brush::{self, PX_PER_UNIT};
use crate::model::Rect;
use crate::paint_ops::{Retouch, RetouchOp, BLUR_MAX_KERNEL_PX};
use crate::tiles::TILE_PX;

const T: i64 = TILE_PX as i64;
const TILE_LEN: usize = (TILE_PX * TILE_PX) as usize;

/// A box of paint pixels: `x0..x0 + w` by `y0..y0 + h`, in the board's
/// pixel grid (pixel (0, 0) covers board units 0..0.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PxBox {
    pub x0: i64,
    pub y0: i64,
    pub w: usize,
    pub h: usize,
}

impl PxBox {
    /// The pixels a board rectangle touches.
    pub fn of_rect(r: &Rect) -> Option<PxBox> {
        let r = r.normalized();
        if !r.is_finite() {
            return None;
        }
        let clamp = |v: f32| v.clamp(-1e9, 1e9) as i64;
        let x0 = clamp((r.x * PX_PER_UNIT).floor());
        let y0 = clamp((r.y * PX_PER_UNIT).floor());
        let x1 = clamp((r.right() * PX_PER_UNIT).ceil()).max(x0 + 1);
        let y1 = clamp((r.bottom() * PX_PER_UNIT).ceil()).max(y0 + 1);
        Some(PxBox { x0, y0, w: (x1 - x0) as usize, h: (y1 - y0) as usize })
    }

    pub fn x1(&self) -> i64 {
        self.x0 + self.w as i64
    }

    pub fn y1(&self) -> i64 {
        self.y0 + self.h as i64
    }

    pub fn grown(&self, by: i64) -> PxBox {
        PxBox { x0: self.x0 - by, y0: self.y0 - by, w: self.w + 2 * by as usize, h: self.h + 2 * by as usize }
    }

    /// The tiles it meets, (tx0, ty0, tx1, ty1) inclusive.
    pub fn tiles(&self) -> (i32, i32, i32, i32) {
        (
            self.x0.div_euclid(T) as i32,
            self.y0.div_euclid(T) as i32,
            (self.x1() - 1).div_euclid(T) as i32,
            (self.y1() - 1).div_euclid(T) as i32,
        )
    }

    /// The part of it inside tile (`tx`, `ty`), in the tile's own pixels:
    /// (x0, y0, x1, y1), and the same part's offset in this box.
    pub fn in_tile(&self, tx: i32, ty: i32) -> Option<((usize, usize, usize, usize), (usize, usize))> {
        let (ox, oy) = (tx as i64 * T, ty as i64 * T);
        let x0 = self.x0.max(ox);
        let y0 = self.y0.max(oy);
        let x1 = self.x1().min(ox + T);
        let y1 = self.y1().min(oy + T);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some((((x0 - ox) as usize, (y0 - oy) as usize, (x1 - ox) as usize, (y1 - oy) as usize), ((x0 - self.x0) as usize, (y0 - self.y0) as usize)))
    }
}

// ── Fill ────────────────────────────────────────────────────────────────

/// Pixels read through tiles fetched on first need (a fill over a large
/// clip reads only the tiles its flood reaches).
struct Lazy<'f> {
    tiles: Vec<Option<Vec<u8>>>,
    index: HashMap<(i32, i32), usize>,
    last: ((i32, i32), usize),
    fetch: &'f mut dyn FnMut(i32, i32) -> Option<Vec<u8>>,
}

impl Lazy<'_> {
    fn get(&mut self, x: i64, y: i64) -> [u8; 4] {
        let key = (x.div_euclid(T) as i32, y.div_euclid(T) as i32);
        let at = if self.last.0 == key && !self.tiles.is_empty() {
            self.last.1
        } else {
            let at = match self.index.get(&key) {
                Some(at) => *at,
                None => {
                    let px = (self.fetch)(key.0, key.1).filter(|p| p.len() == TILE_LEN * 4);
                    self.tiles.push(px);
                    self.index.insert(key, self.tiles.len() - 1);
                    self.tiles.len() - 1
                }
            };
            self.last = (key, at);
            at
        };
        match &self.tiles[at] {
            Some(px) => {
                let i = ((y.rem_euclid(T) * T + x.rem_euclid(T)) * 4) as usize;
                [px[i], px[i + 1], px[i + 2], px[i + 3]]
            }
            None => [0; 4],
        }
    }
}

/// A fill's coverage, per tile (`TILE_PX`² bytes, 255 filled), only for
/// tiles it fills anything in: the pixels inside `clip` within `tolerance`
/// of the seed pixel's color (connected to it 4-wise when `contiguous`),
/// grown by one pixel so the fill reaches under a soft edge. `fetch` gives
/// the layer's pixels below the fill, a tile at a time (`None`: empty).
pub(crate) fn flood(clip: PxBox, seed: (i64, i64), tolerance: f32, contiguous: bool, fetch: &mut dyn FnMut(i32, i32) -> Option<Vec<u8>>) -> HashMap<(i32, i32), Vec<u8>> {
    let mut out = HashMap::new();
    let (sx, sy) = seed;
    if sx < clip.x0 || sy < clip.y0 || sx >= clip.x1() || sy >= clip.y1() {
        return out;
    }
    let mut px = Lazy { tiles: Vec::new(), index: HashMap::new(), last: ((0, 0), 0), fetch };
    let at = px.get(sx, sy);
    let limit = (tolerance.clamp(0.0, 1.0) * 255.0).round() as i32;
    let close = |p: [u8; 4]| (0..4).all(|c| (p[c] as i32 - at[c] as i32).abs() <= limit);
    let (w, h) = (clip.w, clip.h);
    let mut mask = vec![0u8; w * h];
    let idx = |x: i64, y: i64| (y - clip.y0) as usize * w + (x - clip.x0) as usize;
    if contiguous {
        let mut stack = vec![(sx, sy)];
        while let Some((x, y)) = stack.pop() {
            if mask[idx(x, y)] != 0 || !close(px.get(x, y)) {
                continue;
            }
            let mut l = x;
            while l > clip.x0 && mask[idx(l - 1, y)] == 0 && close(px.get(l - 1, y)) {
                l -= 1;
            }
            let mut r = x;
            while r + 1 < clip.x1() && mask[idx(r + 1, y)] == 0 && close(px.get(r + 1, y)) {
                r += 1;
            }
            mask[idx(l, y)..=idx(r, y)].fill(255);
            for ny in [y - 1, y + 1] {
                if ny < clip.y0 || ny >= clip.y1() {
                    continue;
                }
                let mut run = false;
                for i in l..=r {
                    let open = mask[idx(i, ny)] == 0 && close(px.get(i, ny));
                    if open && !run {
                        stack.push((i, ny));
                    }
                    run = open;
                }
            }
        }
    } else {
        for y in clip.y0..clip.y1() {
            for x in clip.x0..clip.x1() {
                if close(px.get(x, y)) {
                    mask[idx(x, y)] = 255;
                }
            }
        }
    }
    let mask = dilate(&mask, w, h);
    let (tx0, ty0, tx1, ty1) = clip.tiles();
    for ty in ty0..=ty1 {
        for tx in tx0..=tx1 {
            let Some(((x0, y0, x1, y1), (bx, by))) = clip.in_tile(tx, ty) else { continue };
            let mut tile = vec![0u8; TILE_LEN];
            let mut any = false;
            for row in 0..(y1 - y0) {
                let src = &mask[(by + row) * w + bx..(by + row) * w + bx + (x1 - x0)];
                let dst = (y0 + row) * TILE_PX as usize + x0;
                tile[dst..dst + (x1 - x0)].copy_from_slice(src);
                any |= src.iter().any(|v| *v != 0);
            }
            if any {
                out.insert((tx, ty), tile);
            }
        }
    }
    out
}

/// Grow a coverage mask by one pixel (its 3 × 3 neighbourhood's maximum).
fn dilate(mask: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut across = vec![0u8; mask.len()];
    for y in 0..h {
        let row = &mask[y * w..(y + 1) * w];
        for x in 0..w {
            let l = x.saturating_sub(1);
            let r = (x + 1).min(w - 1);
            across[y * w + x] = row[l].max(row[x]).max(row[r]);
        }
    }
    let mut out = vec![0u8; mask.len()];
    for y in 0..h {
        let u = y.saturating_sub(1);
        let d = (y + 1).min(h - 1);
        for x in 0..w {
            out[y * w + x] = across[u * w + x].max(across[y * w + x]).max(across[d * w + x]);
        }
    }
    out
}

// ── Smudge and blur ─────────────────────────────────────────────────────

/// A dab's coverage at distance² `d2` from its center: the brush's
/// falloff (as [`brush`] stamps it) times its alpha.
fn falloff(d2: f32, r: f32, hardness: f32, alpha: f32) -> f32 {
    let r2 = r * r;
    if d2 >= r2 {
        return 0.0;
    }
    let inner = (hardness.clamp(0.0, 1.0) * r).min(r - 1.0).max(0.0);
    if d2 <= inner * inner {
        return alpha;
    }
    let soft = (r - inner).max(1e-3);
    let t = (d2.sqrt() - inner) / soft;
    alpha * (1.0 - t * t * (3.0 - 2.0 * t))
}

/// How much more than its own box a retouch reads, in pixels.
pub(crate) fn read_margin(effect: Retouch) -> i64 {
    match effect {
        Retouch::Smudge => 0,
        Retouch::Blur => BLUR_MAX_KERNEL_PX,
    }
}

/// Smudge or blur `buf` (premultiplied RGBA as f32, 0 to 255, `w` × `h`,
/// its pixel (0, 0) the board's pixel (`ox`, `oy`)) with `op`'s dabs,
/// seeded from its id (`seed`).
pub(crate) fn retouch(buf: &mut [f32], w: usize, h: usize, ox: i64, oy: i64, op: &RetouchOp, seed: u64) {
    let dabs = brush::dabs(&op.settings, seed, &op.points);
    let strength = op.strength.clamp(0.0, 1.0);
    if dabs.is_empty() || strength <= 0.0 {
        return;
    }
    let hardness = op.settings.hardness;
    match op.effect {
        Retouch::Smudge => smudge(buf, w, h, ox, oy, &dabs, hardness, strength),
        Retouch::Blur => blur(buf, w, h, ox, oy, &dabs, hardness, strength),
    }
}

/// How opaque (0 to 255) a pixel must be for a spot of the smudge brush to
/// load from it.
const LOAD_ALPHA: f32 = 8.0;

#[allow(clippy::too_many_arguments)]
fn smudge(buf: &mut [f32], w: usize, h: usize, ox: i64, oy: i64, dabs: &[brush::Dab], hardness: f32, strength: f32) {
    let reach = dabs.iter().map(|d| d.radius * PX_PER_UNIT).fold(0.0f32, f32::max).ceil() as i64 + 1;
    let side = (2 * reach + 1) as usize;
    // The paint the brush carries, by offset from the dab's pixel.
    let mut carried = vec![0.0f32; side * side * 4];
    let pick = 1.0 - strength;
    // Each spot of the brush is loaded with the paint under it the first
    // time it passes over some (at least `LOAD_ALPHA` opaque) and lays
    // nothing down before: a smudge started off the paint does nothing
    // until it reaches some, and never drags in the transparency it
    // started on.
    let mut loaded = vec![false; side * side];
    for dab in dabs {
        let cx = dab.x * PX_PER_UNIT - ox as f32;
        let cy = dab.y * PX_PER_UNIT - oy as f32;
        let r = dab.radius * PX_PER_UNIT;
        if !(cx.is_finite() && cy.is_finite() && r > 0.0) {
            continue;
        }
        let (ix, iy) = (cx.floor() as i64, cy.floor() as i64);
        for v in -reach..=reach {
            let y = iy + v;
            for u in -reach..=reach {
                let x = ix + u;
                let ci = (((v + reach) as usize) * side + (u + reach) as usize) * 4;
                let inside = x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h;
                let pi = if inside { (y as usize * w + x as usize) * 4 } else { 0 };
                if !inside {
                    continue;
                }
                let spot = ci / 4;
                if !loaded[spot] {
                    if buf[pi + 3] < LOAD_ALPHA {
                        continue;
                    }
                    // This spot of the brush takes up what is under it.
                    carried[ci..ci + 4].copy_from_slice(&buf[pi..pi + 4]);
                    loaded[spot] = true;
                    continue;
                }
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                let m = falloff(dx * dx + dy * dy, r, hardness, dab.alpha);
                if m <= 0.0 {
                    continue;
                }
                let k = strength * m;
                let p = m * pick;
                for c in 0..4 {
                    let old = buf[pi + c];
                    buf[pi + c] = old + (carried[ci + c] - old) * k;
                    carried[ci + c] += (old - carried[ci + c]) * p;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn blur(buf: &mut [f32], w: usize, h: usize, ox: i64, oy: i64, dabs: &[brush::Dab], hardness: f32, strength: f32) {
    let mut src: Vec<f32> = Vec::new();
    let mut tmp: Vec<f32> = Vec::new();
    for dab in dabs {
        let cx = dab.x * PX_PER_UNIT - ox as f32;
        let cy = dab.y * PX_PER_UNIT - oy as f32;
        let r = dab.radius * PX_PER_UNIT;
        if !(cx.is_finite() && cy.is_finite() && r > 0.0) {
            continue;
        }
        let kr = ((r * 0.25).round() as i64).clamp(1, BLUR_MAX_KERNEL_PX);
        // The dab's box, and the box it reads (kr more each way), in the
        // buffer.
        let x0 = ((cx - r).floor() as i64).max(0);
        let y0 = ((cy - r).floor() as i64).max(0);
        let x1 = ((cx + r).ceil() as i64).min(w as i64);
        let y1 = ((cy + r).ceil() as i64).min(h as i64);
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let rx0 = (x0 - kr).max(0);
        let ry0 = (y0 - kr).max(0);
        let rx1 = (x1 + kr).min(w as i64);
        let ry1 = (y1 + kr).min(h as i64);
        let (rw, rh) = ((rx1 - rx0) as usize, (ry1 - ry0) as usize);
        src.clear();
        src.reserve(rw * rh * 4);
        for y in ry0..ry1 {
            let row = (y as usize * w + rx0 as usize) * 4;
            src.extend_from_slice(&buf[row..row + rw * 4]);
        }
        // Box blur across, then down (edges read as far as the buffer goes).
        tmp.clear();
        tmp.resize(rw * rh * 4, 0.0);
        for y in 0..rh {
            for x in 0..rw {
                let a = x.saturating_sub(kr as usize);
                let b = (x + kr as usize).min(rw - 1);
                let n = (b - a + 1) as f32;
                for c in 0..4 {
                    let mut sum = 0.0;
                    for i in a..=b {
                        sum += src[(y * rw + i) * 4 + c];
                    }
                    tmp[(y * rw + x) * 4 + c] = sum / n;
                }
            }
        }
        for y in (y0 - ry0) as usize..(y1 - ry0) as usize {
            let a = y.saturating_sub(kr as usize);
            let b = (y + kr as usize).min(rh - 1);
            let n = (b - a + 1) as f32;
            let by = ry0 as usize + y;
            let dy = by as f32 + 0.5 - cy;
            for x in (x0 - rx0) as usize..(x1 - rx0) as usize {
                let bx = rx0 as usize + x;
                let dx = bx as f32 + 0.5 - cx;
                let m = falloff(dx * dx + dy * dy, r, hardness, dab.alpha);
                if m <= 0.0 {
                    continue;
                }
                let k = strength * m;
                let pi = (by * w + bx) * 4;
                for c in 0..4 {
                    let mut sum = 0.0;
                    for j in a..=b {
                        sum += tmp[(j * rw + x) * 4 + c];
                    }
                    let blurred = sum / n;
                    buf[pi + c] += (blurred - buf[pi + c]) * k;
                }
            }
        }
    }
}

/// f32 premultiplied pixels back to bytes, kept premultiplied (no color
/// above its alpha).
pub(crate) fn to_bytes(buf: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(buf.len());
    for px in buf.chunks_exact(4) {
        let a = (px[3] + 0.5).clamp(0.0, 255.0) as u8;
        for c in &px[..3] {
            out.push(((c + 0.5).clamp(0.0, 255.0) as u8).min(a));
        }
        out.push(a);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fill_stays_inside_a_ring_and_grows_a_pixel() {
        // A ring of opaque pixels around (20..40, 20..40) in tile (0, 0).
        let mut tile = vec![0u8; TILE_LEN * 4];
        for y in 18..42 {
            for x in 18..42 {
                let edge = !(20..40).contains(&x) || !(20..40).contains(&y);
                if edge {
                    tile[(y * 256 + x) * 4..(y * 256 + x) * 4 + 4].copy_from_slice(&[0, 0, 0, 255]);
                }
            }
        }
        let clip = PxBox { x0: 0, y0: 0, w: 100, h: 100 };
        let mut fetch = |tx: i32, ty: i32| (tx == 0 && ty == 0).then(|| tile.clone());
        let tiles = flood(clip, (30, 30), 0.1, true, &mut fetch);
        let m = &tiles[&(0, 0)];
        assert_eq!(m[30 * 256 + 30], 255);
        assert_eq!(m[20 * 256 + 20], 255, "the inside corner");
        assert_eq!(m[19 * 256 + 30], 255, "grown a pixel into the ring");
        assert_eq!(m[18 * 256 + 30], 0, "not through it");
        assert_eq!(m[10 * 256 + 10], 0, "outside stays");
        // Outside the ring: everything else in the clip, not the inside.
        let tiles = flood(clip, (5, 5), 0.1, true, &mut fetch);
        let m = &tiles[&(0, 0)];
        assert_eq!(m[5 * 256 + 99], 255);
        assert_eq!(m[5 * 256 + 100], 0, "the clip ends it");
        assert_eq!(m[30 * 256 + 30], 0);
        // Not contiguous: both.
        let tiles = flood(clip, (5, 5), 0.1, false, &mut fetch);
        assert_eq!(tiles[&(0, 0)][30 * 256 + 30], 255);
    }

    #[test]
    fn a_fill_crosses_tiles_and_reads_only_what_it_reaches() {
        let clip = PxBox { x0: -300, y0: -10, w: 600, h: 20 };
        let mut asked = Vec::new();
        let mut fetch = |tx: i32, ty: i32| {
            asked.push((tx, ty));
            None
        };
        let tiles = flood(clip, (0, 0), 0.0, true, &mut fetch);
        // x -300..300 meets tiles -2..=1, y -10..10 tiles -1..=0.
        assert_eq!(tiles.len(), 8, "{:?}", tiles.keys());
        assert_eq!(asked.len(), 8, "each tile read once");
    }

    #[test]
    fn blur_softens_an_edge_and_smudge_drags_color() {
        let (w, h) = (40usize, 20usize);
        let mut buf = vec![0.0f32; w * h * 4];
        for y in 0..h {
            for x in 0..20 {
                buf[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[255.0, 0.0, 0.0, 255.0]);
            }
        }
        let before = buf.clone();
        let op = RetouchOp {
            effect: Retouch::Blur,
            preset: "round".into(),
            settings: brush::ROUND.settings.with_size(8.0),
            strength: 1.0,
            points: vec![[10.0, 0.0, 1.0, 0.0], [10.0, 10.0, 1.0, 0.0]],
        };
        retouch(&mut buf, w, h, 0, 0, &op, 7);
        let a = |b: &[f32], x: usize, y: usize| b[(y * w + x) * 4 + 3];
        assert!(a(&buf, 19, 10) < 250.0 && a(&buf, 20, 10) > 5.0, "{} {}", a(&buf, 19, 10), a(&buf, 20, 10));
        assert_eq!(a(&buf, 2, 2), 255.0, "far from the stroke");
        let mut again = before.clone();
        retouch(&mut again, w, h, 0, 0, &op, 7);
        assert_eq!(again, buf, "deterministic");
        // Smudge from the red into the empty half.
        let mut buf = before.clone();
        let op = RetouchOp { effect: Retouch::Smudge, points: vec![[8.0, 5.0, 1.0, 0.0], [16.0, 5.0, 1.0, 0.0]], ..op };
        retouch(&mut buf, w, h, 0, 0, &op, 7);
        assert!(a(&buf, 25, 10) > 20.0, "{}", a(&buf, 25, 10));
    }

    #[test]
    fn a_smudge_started_off_the_paint_loads_only_once_it_reaches_some() {
        let (w, h) = (60usize, 20usize);
        let mut buf = vec![0.0f32; w * h * 4];
        // Red on the right half (board x 15 to 30 at 2 pixels a unit).
        for y in 0..h {
            for x in 30..w {
                buf[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[255.0, 0.0, 0.0, 255.0]);
            }
        }
        let before = buf.clone();
        let a = |b: &[f32], x: usize, y: usize| b[(y * w + x) * 4 + 3];
        // From the empty left into the red and on through it, at full
        // strength.
        let op = RetouchOp {
            effect: Retouch::Smudge,
            preset: "round".into(),
            settings: brush::ROUND.settings.with_size(6.0),
            strength: 1.0,
            points: vec![[2.0, 5.0, 1.0, 0.0], [26.0, 5.0, 1.0, 0.0]],
        };
        retouch(&mut buf, w, h, 0, 0, &op, 7);
        assert_eq!(a(&buf, 4, 10), 0.0, "nothing happens off the paint");
        // Inside the red, nothing transparent was dragged in.
        for x in 36..52 {
            assert!(a(&buf, x, 10) > 250.0, "x {x}: {}", a(&buf, x, 10));
        }
        let mut again = before.clone();
        retouch(&mut again, w, h, 0, 0, &op, 7);
        assert_eq!(again, buf, "deterministic");
    }
}
