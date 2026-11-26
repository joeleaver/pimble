//! The paint tile pyramid (docs/DRAWING_CONTRACT.md "Large canvases"): a
//! view zoomed far out reads coarse tiles, each its four children at half
//! size, instead of every full tile. A county-sized paint layer (one
//! picture stretched over 11,800 units, as the Braynard map had) took 6-9 s
//! a zoom step before, every step replaying or reloading 8,000 tiles.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pimble_board::*;

const T: &str = "2026-11-19T10:00:00.000Z";
const SRC: &str = "pimble-blob:00000000-0000-0000-0000-000000000001/aaaaaaaaaaaaaaaaaaaaaaaaaa";

fn st() -> Stamp<'static> {
    Stamp::new(None, T)
}

fn ok<R: std::fmt::Debug>(board: &Board, f: impl FnOnce(&mut BoardTxn<'_, '_>) -> doc::Result<R>) -> R {
    board.edit(st(), f).into_result().unwrap().0
}

/// A picture `w` × `h`: its left half red, its right half blue.
fn halves(w: u32, h: u32) -> Picture {
    let rgba: Vec<u8> = (0..w * h).flat_map(|i| if i % w < w / 2 { [220, 30, 30, 255] } else { [30, 40, 220, 255] }).collect();
    Picture::from_rgba(w, h, rgba).unwrap()
}

/// A board whose paint layer holds one picture stretched over `rect`.
fn stretched(rect: Rect) -> (Board, String) {
    let board = Board::standalone();
    board.create(st()).unwrap();
    let paint = ok(&board, |t| t.paint_layer_for(None));
    ok(&board, |t| t.add_placed_pixels(&paint, &[(SRC.to_string(), rect)], None));
    (board, paint)
}

/// The CPU time this thread has used: what a render costs, whatever else the
/// machine is doing (a render is one thread's work; a wall clock also counts
/// the time the scheduler gave other programs, and a full suite at load
/// average 90 made a 500 ms bound on it fail at 900 ms, 2026-11-26).
#[cfg(unix)]
fn thread_cpu() -> Duration {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `t` is a valid, writable timespec for the call's duration.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t) };
    assert_eq!(rc, 0, "the thread's CPU clock reads");
    Duration::new(t.tv_sec as u64, t.tv_nsec as u32)
}

fn px(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [img[i], img[i + 1], img[i + 2], img[i + 3]]
}

#[test]
fn levels_follow_the_view() {
    assert_eq!(tiles::level_for_scale(4.0), 0);
    assert_eq!(tiles::level_for_scale(2.0), 0);
    assert_eq!(tiles::level_for_scale(1.5), 0);
    assert_eq!(tiles::level_for_scale(1.0), 1);
    assert_eq!(tiles::level_for_scale(0.25), 3);
    assert_eq!(tiles::level_for_scale(0.16), 3);
    assert_eq!(tiles::level_for_scale(1e-6), tiles::MAX_LEVEL);
    let r = tiles::tile_rect_at(3, -1, 2);
    assert_eq!((r.x, r.y, r.w), (-1024.0, 2048.0, 1024.0));
}

#[test]
fn a_county_sized_layer_fitted_reads_coarse_tiles_and_matches_the_full_ones() {
    let area = Rect::new(0.0, 0.0, 11_800.0, 11_000.0);
    let (board, _) = stretched(area);
    let pics: HashMap<String, Arc<Picture>> = HashMap::from([(SRC.to_string(), Arc::new(halves(2400, 2200)))]);
    let cache = PaintCache::new();
    let snap = board.snapshot().with_paint_cache(cache.clone());
    let (w, h) = (1920u32, 1080u32);
    let view = View::fit(&area, w, h, 0.0);

    let t0 = Instant::now();
    let cold = render(&snap, &pics, &view, w, h, None).unwrap().take();
    let cold_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let first = cache.stats();
    let t1 = Instant::now();
    #[cfg(unix)]
    let cpu0 = thread_cpu();
    let warm = render(&snap, &pics, &view, w, h, None).unwrap().take();
    #[cfg(unix)]
    let warm_cpu = thread_cpu() - cpu0;
    let warm_ms = t1.elapsed().as_secs_f64() * 1000.0;
    let second = cache.stats();
    eprintln!("fitted county: cold {cold_ms:.0} ms ({} replayed, {} merged), warm {warm_ms:.1} ms ({} tiles read); {} MB held", first.replayed, first.merged, second.hits - first.hits, cache.memory_bytes() >> 20);
    assert_eq!(cold, warm);
    assert!(first.merged > 0, "zoomed out, it reads coarse tiles");
    assert_eq!(second.replayed, first.replayed, "the second step replays nothing");
    assert_eq!(second.merged, first.merged, "and makes no coarse tile again");
    assert_eq!(second.loaded, first.loaded, "and reads nothing from disk");
    // What a warm step does is read the tiles of the view's own level from
    // memory, about one a tile of the screen: the 8,000 full ones under them
    // would be thousands.
    let k = tiles::level_for_scale(view.scale);
    let side = tiles::tile_rect_at(k, 0, 0).w * view.scale;
    let on_screen = ((w as f32 / side).ceil() as u64 + 1) * ((h as f32 / side).ceil() as u64 + 1);
    let read = second.hits - first.hits;
    assert!(read > 0 && read <= on_screen, "a warm fitted step reads the view's {on_screen} tiles at level {k} at most, not {read}");
    // And it is quick, by the thread's own clock.
    #[cfg(unix)]
    assert!(warm_cpu < Duration::from_millis(500), "a warm fitted step is quick: {warm_cpu:?} of CPU ({warm_ms:.0} ms by the wall)");
    assert!(cache.memory_bytes() <= tiles::DEFAULT_BUDGET, "it kept to its budget");

    // The same picture as a full-density render shows: red left, blue
    // right, edges where the picture's are.
    let mid = h / 2;
    let left = px(&cold, w, view.to_pixels(Point::new(2000.0, 5500.0)).0 as u32, mid);
    let right = px(&cold, w, view.to_pixels(Point::new(9800.0, 5500.0)).0 as u32, mid);
    assert!(left[0] > 180 && left[2] < 90, "red on the left: {left:?}");
    assert!(right[2] > 180 && right[0] < 90, "blue on the right: {right:?}");
}

#[test]
fn a_coarse_render_matches_a_full_one_and_follows_an_edit() {
    let area = Rect::new(0.0, 0.0, 2048.0, 1024.0);
    let (board, paint) = stretched(area);
    let pics: HashMap<String, Arc<Picture>> = HashMap::from([(SRC.to_string(), Arc::new(halves(400, 200)))]);
    let (w, h) = (256u32, 128u32);
    // Scale 0.125: level 4.
    let view = View::new(0.0, 0.0, 0.125);
    let coarse = render(&board.snapshot().with_paint_cache(PaintCache::new()), &pics, &view, w, h, None).unwrap().take();
    // The full tiles (level 0, scale 2) at 16 times the size, boxed down.
    let full = render(&board.snapshot().with_paint_cache(PaintCache::new()), &pics, &View::new(0.0, 0.0, 2.0), w * 16, h * 16, None).unwrap().take();
    let mut direct = vec![0u8; coarse.len()];
    for y in 0..h as usize {
        for x in 0..w as usize {
            for c in 0..4 {
                let mut sum = 0u32;
                for sy in 0..16 {
                    for sx in 0..16 {
                        sum += full[(((y * 16 + sy) * w as usize * 16) + x * 16 + sx) * 4 + c] as u32;
                    }
                }
                direct[(y * w as usize + x) * 4 + c] = (sum / 256) as u8;
            }
        }
    }
    let worst = coarse.iter().zip(&direct).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
    let off = coarse.iter().zip(&direct).filter(|(a, b)| (**a as i32 - **b as i32).abs() > 8).count();
    assert!(off * 200 < coarse.len(), "a coarse render is the full one boxed down: {off} channels off, worst {worst}");

    // A stroke painted over it reaches the coarse tiles through the same
    // keys: the cache shown again draws it.
    let cache = PaintCache::new();
    let before = render(&board.snapshot().with_paint_cache(cache.clone()), &pics, &view, w, h, None).unwrap().take();
    let points: Vec<BrushPoint> = (0..8).map(|i| [1024.0 + 10.0 * i as f32, 512.0, 1.0, 0.0]).collect();
    ok(&board, |t| t.add_stroke(NewStroke::new(paint.clone(), &brush::ROUND, Color::rgb(0, 200, 0)).with_size(200.0).with_points(points)));
    let after = render(&board.snapshot().with_paint_cache(cache.clone()), &pics, &view, w, h, None).unwrap().take();
    assert_ne!(before, after, "the stroke shows zoomed out");
    let g = px(&after, w, 132, 64);
    assert!(g[1] > 150 && g[0] < 100, "green where it was painted: {g:?}");
}

#[test]
fn a_thin_stroke_zoomed_out_is_as_smooth_as_the_full_tiles_boxed_down() {
    let board = Board::standalone();
    board.create(st()).unwrap();
    let paint = ok(&board, |t| t.paint_layer_for(None));
    let points: Vec<BrushPoint> = (0..200).map(|i| [i as f32 * 20.0, i as f32 * 10.0, 1.0, 0.0]).collect();
    ok(&board, |t| t.add_stroke(NewStroke::new(paint.clone(), &brush::ROUND, Color::rgb(220, 30, 30)).with_size(40.0).with_points(points)));
    let (w, h) = (256u32, 128u32);
    for (scale, n) in [(0.125f32, 16usize), (0.0625, 32)] {
        let coarse = render(&board.snapshot().with_paint_cache(PaintCache::new()), &NoPictures, &View::new(0.0, 0.0, scale), w, h, None).unwrap().take();
        let full = render(&board.snapshot().with_paint_cache(PaintCache::new()), &NoPictures, &View::new(0.0, 0.0, 2.0), w * n as u32, h * n as u32, None).unwrap().take();
        let fw = w as usize * n;
        let mut worst = 0;
        let mut off = 0;
        for y in 0..h as usize {
            for x in 0..w as usize {
                let mut sum = 0u32;
                for sy in 0..n {
                    for sx in 0..n {
                        sum += full[((y * n + sy) * fw + x * n + sx) * 4 + 3] as u32;
                    }
                }
                let want = (sum / (n * n) as u32) as i32;
                let got = coarse[(y * w as usize + x) * 4 + 3] as i32;
                worst = worst.max((want - got).abs());
                off += ((want - got).abs() > 12) as usize;
            }
        }
        eprintln!("scale {scale}: worst alpha difference {worst}, {off} pixels past 12");
        assert!(worst <= 40 && off * 100 < (w * h) as usize, "scale {scale}: worst {worst}, {off} off");
    }
}
