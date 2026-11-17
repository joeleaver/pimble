//! The paint spikes' numbers (docs/DRAWING_CONTRACT.md "Spikes before
//! building": brush engine speed, replay cost):
//!
//!     cargo run -p pimble-board --release --example paint_bench
//!
//! and for wasm (wasmtime runs the same code the browser would):
//!
//!     cargo build -p pimble-board --release --example paint_bench --target wasm32-wasip1
//!     wasmtime target/wasm32-wasip1/release/examples/paint_bench.wasm
//!
//! Measures: stamping dabs (per dab, by size); a long pressure stroke drawn
//! across a 4K view (3840 x 2160 pixels at 2 pixels per board unit), points
//! appended four at a time as a pen's pointer events arrive, each append
//! brought into the view (snapshot update, tiles, the view's re-render);
//! and a paint layer of 5,000 strokes opened cold, from the memory cache,
//! and (native) from a disk cache.

use std::time::Instant;

use pimble_board::*;

const T0: &str = "2026-11-17T10:00:00.000Z";

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }

    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next()
    }
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn st() -> Stamp<'static> {
    Stamp::new(None, T0)
}

fn paint_board() -> (Board, String) {
    let board = Board::standalone();
    board.create(st()).unwrap();
    let layer = board.edit(st(), |t| t.paint_layer_for(None)).into_result().unwrap().0;
    (board, layer)
}

/// A wandering pen stroke over `w` x `h` board units: a sweep left to right
/// and back, eight times down, pressure rising and falling.
fn long_stroke(n: usize, w: f32, h: f32) -> Vec<BrushPoint> {
    (0..n)
        .map(|i| {
            let t = i as f32 / n as f32;
            let row = (t * 8.0).floor();
            let along = (t * 8.0).fract();
            let x = if row as i32 % 2 == 0 { along * w } else { (1.0 - along) * w };
            let y = (row + 0.5) / 8.0 * h + (along * 25.0).sin() * 20.0;
            [x, y, 0.5 + 0.5 * (t * 40.0).sin(), 0.2]
        })
        .collect()
}

fn main() {
    let only_replay = std::env::var("ONLY_REPLAY").is_ok();
    // ── Per dab ──────────────────────────────────────────────────────────
    if !only_replay {
    println!("== Stamping dabs (one 256 px tile, f32 mask) ==");
    for (preset, size) in [("pencil", 2.0), ("round", 8.0), ("round", 24.0), ("airbrush", 60.0), ("airbrush", 120.0)] {
        let p = brush::preset(preset).unwrap();
        let settings = p.settings.with_size(size);
        let (board, layer) = paint_board();
        // A stroke inside one tile, many dabs.
        let pts: Vec<BrushPoint> = (0..400).map(|i| [64.0 + (i as f32 * 0.37).sin() * 30.0, 64.0 + (i as f32 * 0.23).cos() * 30.0, 1.0, 0.0]).collect();
        let id = board
            .edit(st(), |t| t.add_stroke(NewStroke::new(layer.clone(), p, Color::BLACK).with_settings(settings).with_points(pts.clone())))
            .into_result()
            .unwrap()
            .0;
        let n = brush::dabs(&settings, brush::seed_of(&id), &pts).len();
        let snap = board.snapshot();
        let t = Instant::now();
        let runs = 5;
        for _ in 0..runs {
            snap.paint_cache().clear();
            snap.paint_cache().tile_pixels(&snap, &layer, 0, 0, &NoPictures).unwrap();
        }
        let per = ms(t) / runs as f64;
        println!("  {preset:>9} {size:>5} units ({:>4} px wide): {n:>5} dabs in {per:>7.2} ms = {:>6.2} us per dab", size * 2.0, per * 1000.0 / n as f64);
    }

    // ── A long stroke across a 4K view ───────────────────────────────────
    println!("== A long pressure stroke across a 4K view (1920 x 1080 units, 3840 x 2160 px) ==");
    for (preset, size) in [("round", 24.0), ("airbrush", 80.0), ("charcoal", 16.0)] {
        let p = brush::preset(preset).unwrap();
        let (board, layer) = paint_board();
        let pts = long_stroke(4000, 1900.0, 1060.0);
        let mut view = BoardView::new(3840, 2160, Color::WHITE);
        view.set_camera(Point::new(960.0, 540.0), 2.0);
        view.update(&board, &Changes { all: true, ..Changes::default() });
        view.frame(&NoPictures);
        let watch = board.watch().unwrap();
        let id = board.edit(st(), |t| t.begin_stroke(NewStroke::new(layer.clone(), p, Color::rgb(30, 60, 200)).with_size(size))).into_result().unwrap().0;
        view.update(&board, &watch.take());
        view.frame(&NoPictures);
        let mut times = Vec::new();
        let t_all = Instant::now();
        for chunk in pts.chunks(4) {
            let t = Instant::now();
            board.append_stroke(st(), &id, chunk).unwrap();
            view.update(&board, &watch.take());
            view.frame(&NoPictures);
            times.push(ms(t));
        }
        let total = ms(t_all);
        if std::env::var("SLOW").is_ok() {
            let mut idx: Vec<usize> = (0..times.len()).collect();
            idx.sort_by(|a, b| times[*b].partial_cmp(&times[*a]).unwrap());
            println!("slowest appends: {:?}", idx[..8].iter().map(|i| (*i, times[*i] as i64)).collect::<Vec<_>>());
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let snap = view.snapshot();
        let dabs = match &snap.paint[0].raw.body {
            PaintBody::Stroke(s) => brush::dabs(&s.settings, brush::seed_of(&id), &s.points).len(),
            _ => 0,
        };
        let stats = snap.paint_cache().stats();
        println!(
            "  {preset:>9} {size:>4} units: {} appends of 4 points, {dabs} dabs; per append median {:.2} ms, p95 {:.2} ms, max {:.2} ms; whole stroke {:.0} ms ({stats:?})",
            times.len(),
            times[times.len() / 2],
            times[times.len() * 95 / 100],
            times[times.len() - 1],
            total
        );
        // The same stroke replayed whole, cold, for comparison.
        let snap = board.snapshot();
        let t = Instant::now();
        render_paint_layer(&snap, &layer, &NoPictures, 2.0, None).unwrap();
        println!("            the finished stroke replayed cold: {:.0} ms", ms(t));
    }

    }
    // ── 5,000 strokes ────────────────────────────────────────────────────
    println!("== A paint layer of 5,000 strokes over 1920 x 1080 units ==");
    let (board, layer) = paint_board();
    let mut rng = Rng(0x5eed);
    let t = Instant::now();
    board
        .edit(st(), |tx| {
            for i in 0..5000 {
                let p = &PRESETS[i % 7];
                let (x, y) = (rng.range(0.0, 1900.0), rng.range(0.0, 1060.0));
                let a = rng.range(0.0, 6.28);
                let len = rng.range(20.0, 160.0);
                let pts: Vec<BrushPoint> = (0..30)
                    .map(|k| {
                        let s = k as f32 / 29.0;
                        [x + a.cos() * len * s + (s * 9.0).sin() * 6.0, y + a.sin() * len * s, 0.3 + 0.7 * s, 0.1]
                    })
                    .collect();
                let color = Color::rgb((rng.next() * 255.0) as u8, (rng.next() * 255.0) as u8, (rng.next() * 255.0) as u8);
                tx.add_stroke(NewStroke::new(layer.clone(), p, color).with_size(rng.range(4.0, 30.0)).with_points(pts))?;
            }
            Ok(())
        })
        .into_result()
        .unwrap();
    println!("  writing them: {:.0} ms", ms(t));
    let t = Instant::now();
    let snap = board.snapshot();
    println!("  reading the board (snapshot): {:.0} ms", ms(t));
    let t = Instant::now();
    let cold = render_paint_layer(&snap, &layer, &NoPictures, 2.0, None).unwrap();
    let cold_ms = ms(t);
    let stats = snap.paint_cache().stats();
    println!("  cold: every tile replayed ({} tiles) at 2 px/unit: {:.0} ms", stats.replayed, cold_ms);
    let t = Instant::now();
    render_paint_layer(&snap, &layer, &NoPictures, 2.0, None).unwrap();
    println!("  again, tiles from memory: {:.0} ms", ms(t));
    let t = Instant::now();
    render(&snap, &NoPictures, &View::new(0.0, 0.0, 1.0), 1920, 1080, Some(Color::WHITE)).unwrap();
    println!("  the whole view at 100% (1920 x 1080 px) from memory: {:.0} ms", ms(t));
    let fresh = board.snapshot();
    let t = Instant::now();
    render(&fresh, &NoPictures, &View::new(0.0, 0.0, 1.0), 1920, 1080, Some(Color::WHITE)).unwrap();
    println!("  the whole view at 100% cold: {:.0} ms", ms(t));
    #[cfg(not(target_arch = "wasm32"))]
    {
        let dir = std::env::temp_dir().join(format!("pimble-paint-bench-{}", std::process::id()));
        let store: std::sync::Arc<dyn TileStore> = std::sync::Arc::new(DirTileStore::new(&dir));
        let snap = board.snapshot().with_paint_cache(PaintCache::with_store(Some(store.clone()), 256 << 20));
        let t = Instant::now();
        render_paint_layer(&snap, &layer, &NoPictures, 2.0, None).unwrap();
        println!("  cold, saving every tile to disk: {:.0} ms", ms(t));
        let size: u64 = walk(&dir);
        let snap = board.snapshot().with_paint_cache(PaintCache::with_store(Some(store), 256 << 20));
        let t = Instant::now();
        let warm = render_paint_layer(&snap, &layer, &NoPictures, 2.0, None).unwrap();
        println!("  a new memory cache, tiles from disk ({} loaded, {:.1} MB on disk): {:.0} ms", snap.paint_cache().stats().loaded, size as f64 / 1e6, ms(t));
        assert_eq!(warm.image, cold.image, "tiles from disk are the tiles replayed");
        let _ = std::fs::remove_dir_all(&dir);
    }
    let _ = cold;
}

#[cfg(not(target_arch = "wasm32"))]
fn walk(dir: &std::path::Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                total += walk(&path);
            } else if let Ok(m) = e.metadata() {
                total += m.len();
            }
        }
    }
    total
}
