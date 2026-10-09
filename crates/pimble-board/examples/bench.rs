//! Spike 3's numbers (docs/IMAGE_MARKUP_CONTRACT.md "Spikes before building"):
//!
//!     cargo run -p pimble-board --release --example bench [-- --png <dir>]
//!
//! and for wasm (wasmtime runs the same code the browser would):
//!
//!     cargo build -p pimble-board --release --example bench --target wasm32-wasip1
//!     wasmtime target/wasm32-wasip1/release/examples/bench.wasm
//!
//! Measures: a full render of a 4000 x 3000 picture with 200 elements on
//! it; stamping a 2000-point stroke incrementally into the raster cache; and
//! a 5000-element whiteboard's view after a zoom settles. `--png <dir>`
//! writes the renders out to look at.

use std::sync::Arc;
use std::time::Instant;

use pimble_board::*;

const T0: &str = "2026-10-09T10:00:00.000Z";
const BG: Color = Color::rgb(245, 245, 245);

/// A tiny deterministic generator, so runs compare.
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

fn median_ms(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    samples[samples.len() / 2]
}

fn time<R>(runs: usize, mut f: impl FnMut() -> R) -> (f64, R) {
    let mut samples = Vec::with_capacity(runs);
    let mut last = None;
    for _ in 0..runs {
        let t = Instant::now();
        let r = f();
        samples.push(t.elapsed().as_secs_f64() * 1000.0);
        last = Some(r);
    }
    (median_ms(samples), last.unwrap())
}

fn screenshot(w: u32, h: u32) -> Picture {
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let band = ((x / 200 + y / 150) % 2) as u8;
            rgba[i] = (x * 255 / w) as u8;
            rgba[i + 1] = (y * 255 / h) as u8;
            rgba[i + 2] = 120 + band * 80;
            rgba[i + 3] = 255;
        }
    }
    Picture::from_rgba(w, h, rgba).unwrap()
}

fn stroke(rng: &mut Rng, x: f32, y: f32, n: usize, step: f32) -> Vec<[f32; 3]> {
    let mut pts = Vec::with_capacity(n);
    let (mut px, mut py, mut a) = (x, y, rng.range(0.0, 6.28));
    for _ in 0..n {
        a += rng.range(-0.3, 0.3);
        px += a.cos() * step;
        py += a.sin() * step;
        pts.push([px, py, 0.5]);
    }
    pts
}

fn save(dir: &Option<String>, name: &str, pixmap: &tiny_skia::Pixmap) {
    if let Some(dir) = dir {
        let img = RgbaImage::from_pixmap(pixmap.clone());
        let path = format!("{dir}/{name}.png");
        std::fs::write(&path, img.encode_png().unwrap()).unwrap();
        println!("  wrote {path}");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let png = args.iter().position(|a| a == "--png").and_then(|i| args.get(i + 1).cloned());
    println!("pimble-board bench ({})", if cfg!(target_family = "wasm") { "wasm" } else { "native" });

    // ── 1. A 4000 x 3000 picture with 200 elements on it ────────────────
    let (ms, picture) = time(1, || screenshot(4000, 3000));
    println!("\n1. 4000x3000 picture + 200 elements");
    println!("  make the picture (not part of a render): {ms:.1} ms");
    let picture = Arc::new(picture);
    let pictures = move |src: &str| (src == "pimble-blob:shot").then(|| picture.clone());
    let board = Board::standalone();
    let author = Author::new("sub", "joe");
    let st = Stamp::new(Some(&author), T0);
    board.create(st).unwrap();
    let mut rng = Rng(0x9e3779b97f4a7c15);
    board
        .edit(st, |t| {
            let geom = PictureGeom { rect: Rect::new(0.0, 0.0, 4000.0, 3000.0), src: "pimble-blob:shot".into(), px_w: 4000, px_h: 3000, crop: None };
            t.add_element(NewElement::new(Kind::Picture, Geom::Picture(geom)))?;
            for i in 0..200 {
                let (x, y) = (rng.range(0.0, 3800.0), rng.range(0.0, 2800.0));
                let el = match i % 8 {
                    0 | 1 => NewElement::stroke(Kind::Pen, stroke(&mut rng, x, y, 60, 4.0)),
                    2 => NewElement::stroke(Kind::Highlighter, stroke(&mut rng, x, y, 40, 6.0)),
                    3 => NewElement::new(Kind::Arrow, Geom::Segment { a: Point::new(x, y), b: Point::new(x + rng.range(-300.0, 300.0), y + rng.range(-300.0, 300.0)) }),
                    4 => NewElement::new(Kind::Rect, Geom::Rect(Rect::new(x, y, rng.range(40.0, 400.0), rng.range(40.0, 300.0)))),
                    5 => NewElement::new(Kind::Ellipse, Geom::Rect(Rect::new(x, y, rng.range(40.0, 400.0), rng.range(40.0, 300.0)))),
                    6 => NewElement::new(Kind::Text, Geom::Rect(Rect::new(x, y, 0.0, 0.0))).with_text("This button is broken"),
                    _ => NewElement::new(Kind::Step, Geom::Anchor(Point::new(x, y))),
                };
                t.add_element(el)?;
            }
            Ok(())
        })
        .into_result()
        .unwrap();
    let (ms, snap) = time(5, || board.snapshot());
    println!("  snapshot (read + resolve 201 elements): {ms:.2} ms");
    let extent = snap.visible_extent().unwrap();
    let (ms, px) = time(5, || render(&snap, &pictures, &View::new(extent.x.floor(), extent.y.floor(), 1.0), 4000, 3000, Some(BG)).unwrap());
    println!("  full render at 1:1 (4000x3000 out): {ms:.1} ms");
    save(&png, "1-full-1to1", &px);
    let (ms, _) = time(3, || render_extent(&snap, &pictures, 1.0).unwrap());
    println!("  render_extent at 1:1 (render + trim, transparent): {ms:.1} ms");
    for (w, h) in [(1920u32, 1080u32), (3840, 2160)] {
        let view = View::fit(&extent, w, h, 0.0);
        let (ms, px) = time(5, || render(&snap, &pictures, &view, w, h, Some(BG)).unwrap());
        println!("  fit into {w}x{h} (scale {:.3}): {ms:.1} ms", view.scale);
        save(&png, &format!("1-fit-{w}"), &px);
    }

    // ── 2. A 2000-point stroke, stamped incrementally ───────────────────
    println!("\n2. 2000-point stroke stamped into the raster cache (1920x1080 view)");
    for zoom_label in ["fit", "1:1"] {
        let mut view = BoardView::new(1920, 1080, BG);
        view.set_snapshot(board.snapshot());
        if zoom_label == "fit" {
            view.fit(&extent, 0.0);
        } else {
            view.set_camera(Point::new(2000.0, 1500.0), 1.0);
        }
        let t = Instant::now();
        view.frame(&pictures);
        let first = t.elapsed().as_secs_f64() * 1000.0;
        let pts = stroke(&mut rng, 2000.0, 1500.0, 2000, 1.5);
        let style = Style { width: Some(4.0), ..Style::default() };
        view.begin_stroke(Kind::Pen, &style, pts[0]);
        let (mut stamp_ms, mut frame_ms) = (Vec::new(), Vec::new());
        for p in &pts[1..] {
            let t = Instant::now();
            view.extend_stroke(&[*p]);
            stamp_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            let t = Instant::now();
            let _ = view.frame(&pictures);
            frame_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let total_stamp: f64 = stamp_ms.iter().sum();
        let worst = stamp_ms.iter().cloned().fold(0.0, f64::max);
        println!(
            "  {zoom_label}: first frame (cache build) {first:.1} ms; per segment stamp median {:.4} ms, worst {worst:.3} ms, all 1999 {total_stamp:.1} ms; per frame (row copy) median {:.2} ms",
            median_ms(stamp_ms.clone()),
            median_ms(frame_ms.clone())
        );
        // The stroke lands in the document: what the re-render of its
        // rectangle costs, and the appends that carried it.
        let ended = view.end_stroke();
        let t = Instant::now();
        let mut id = String::new();
        for (k, chunk) in ended.chunks(10).enumerate() {
            if k == 0 {
                id = board.add_element(st, NewElement::stroke(Kind::Pen, chunk.to_vec()).with_style(style.clone())).unwrap().0;
            } else {
                board.append_points(st, &id, chunk).unwrap();
            }
        }
        let append_ms = t.elapsed().as_secs_f64() * 1000.0;
        let t = Instant::now();
        view.set_snapshot(board.snapshot());
        view.frame(&pictures);
        let settle_ms = t.elapsed().as_secs_f64() * 1000.0;
        println!("        200 appends of 10 points {append_ms:.1} ms; snapshot + re-render of the stroke's rectangle {settle_ms:.1} ms");
        if zoom_label == "fit" {
            let tmp = render(view.snapshot(), &pictures, &view.view(), 1920, 1080, Some(BG)).unwrap();
            save(&png, "2-stroke", &tmp);
        }
        board.delete_element(st, &id).unwrap();
    }

    // ── 3. A 5000-element whiteboard ────────────────────────────────────
    println!("\n3. 5000-element whiteboard (1920x1080 view)");
    let wb = Board::standalone();
    wb.create(st).unwrap();
    let mut rng = Rng(42);
    let t = Instant::now();
    wb.edit(st, |t| {
        for i in 0..5000 {
            let (x, y) = (rng.range(0.0, 20000.0), rng.range(0.0, 15000.0));
            let el = match i % 5 {
                0 | 1 => NewElement::new(Kind::Sticky, Geom::Rect(Rect::new(x, y, 200.0, 200.0))).with_text("Retro: what went well, and what we would change"),
                2 => NewElement::new(Kind::Rect, Geom::Rect(Rect::new(x, y, 240.0, 120.0))).with_text("A shape"),
                3 => NewElement::new(Kind::Arrow, Geom::Segment { a: Point::new(x, y), b: Point::new(x + 200.0, y + 80.0) }),
                _ => NewElement::stroke(Kind::Pen, stroke(&mut rng, x, y, 40, 5.0)),
            };
            t.add_element(el)?;
        }
        Ok(())
    })
    .into_result()
    .unwrap();
    println!("  write 5000 elements (one transaction): {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
    let (read_ms, raw) = time(3, || wb.read());
    let (ms, snap) = time(3, || BoardSnapshot::build(raw.clone()));
    println!("  snapshot: read the document {read_ms:.0} ms, resolve + lay out text + index {ms:.0} ms");
    let all = snap.visible_extent().unwrap();
    let mut view = BoardView::new(1920, 1080, BG);
    view.set_snapshot(wb.snapshot());
    for (label, rect) in [
        ("everything (zoomed out)", all),
        ("a quarter", Rect::new(all.x, all.y, all.w / 2.0, all.h / 2.0)),
        ("about 1:1", Rect::new(9000.0, 7000.0, 1920.0, 1080.0)),
    ] {
        view.frame(&pictures);
        view.fit(&rect, 0.0);
        let visible = snap.query(&view.view().board_rect(1920, 1080)).len();
        let t = Instant::now();
        let _ = view.frame(&pictures); // the scaled cache, before settling
        let scaled = t.elapsed().as_secs_f64() * 1000.0;
        let t = Instant::now();
        view.settle(&pictures);
        let settle = t.elapsed().as_secs_f64() * 1000.0;
        let t = Instant::now();
        view.frame(&pictures);
        let frame = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "  {label}: {visible} elements in view, zoom {:.3}: scaled frame {scaled:.1} ms, settle (re-render cache) {settle:.0} ms, frame {frame:.1} ms",
            view.zoom()
        );
        if let Some(dir) = &png {
            let px = render(&snap, &pictures, &view.view(), 1920, 1080, Some(BG)).unwrap();
            save(&Some(dir.clone()), &format!("3-{}", label.split(' ').next().unwrap()), &px);
        }
    }
    // An edit of one element while viewing about 1:1 (a remote append
    // every ~100 ms looks like this): the watch names it, the snapshot
    // re-reads it alone, the view re-renders its rectangle.
    let watch = wb.watch().unwrap();
    let id = snap.query(&view.view().board_rect(1920, 1080)).first().map(|i| snap.elements[*i].id().to_string());
    if let Some(id) = id {
        let mut times = (Vec::new(), Vec::new(), Vec::new());
        for k in 0..10 {
            watch.take();
            wb.edit(st, |t| t.set_transform(&id, &Transform::translate(3.0 * k as f32, 3.0))).into_result().unwrap();
            let t = Instant::now();
            view.update(&wb, &watch.take());
            times.0.push(t.elapsed().as_secs_f64() * 1000.0);
            let t = Instant::now();
            view.frame(&pictures);
            times.1.push(t.elapsed().as_secs_f64() * 1000.0);
            let t = Instant::now();
            let _ = BoardSnapshot::build(wb.read());
            times.2.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        println!(
            "  one element moved: view.update (re-read it, resolve, diff) {:.1} ms, then the frame (re-render its rectangle + copy) {:.1} ms; a full re-read and resolve would be {:.0} ms",
            median_ms(times.0),
            median_ms(times.1),
            median_ms(times.2)
        );
    }
}
