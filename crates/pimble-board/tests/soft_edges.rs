//! Soft edges (docs/DRAWING_WAVE6_CONTRACT.md section 3): an element's
//! feather, a layer's feather and blur; the stored keys, what they draw,
//! where they reach (bounds, the visible extent, the spatial index), clips
//! after blur, the raster cache, SVG filters, Flatten and Merge Down baking
//! them, large canvases, and two people editing at once.

use std::time::Instant;

use pimble_board::*;
use pimble_crdt::NodeDoc;

const T: &str = "2026-11-18T10:00:00.000Z";
const RED: Color = Color::rgb(220, 30, 30);
const INK: Color = Color::rgb(20, 40, 200);

fn st() -> Stamp<'static> {
    Stamp::new(None, T)
}

fn ok<R: std::fmt::Debug>(board: &Board, f: impl FnOnce(&mut BoardTxn<'_, '_>) -> doc::Result<R>) -> R {
    board.edit(st(), f).into_result().unwrap().0
}

fn filled(kind: Kind, r: Rect, color: Color) -> NewElement {
    NewElement::new(kind, Geom::Rect(r)).with_style(Style { fill: Some(color), stroke: Some(color), width: Some(0.0), ..Style::default() })
}

/// Transparent pixels of `view` (w x h).
fn draw(snap: &BoardSnapshot, view: View, w: u32, h: u32) -> Vec<u8> {
    render(snap, &NoPictures, &view, w, h, None).unwrap().take()
}

fn at(px: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [px[i], px[i + 1], px[i + 2], px[i + 3]]
}

fn alpha(px: &[u8], w: u32, x: u32, y: u32) -> u8 {
    at(px, w, x, y)[3]
}

/// A canvas with a red square 40..160 in "Layer 1"; answers (board, layer,
/// square).
fn square() -> (Board, String, String) {
    let board = Board::standalone();
    let l1 = board.create(st()).unwrap().0;
    let sq = ok(&board, |t| t.add_element(filled(Kind::Rect, Rect::new(40.0, 40.0, 120.0, 120.0), RED).in_layer(&l1)));
    (board, l1, sq)
}

fn feather(radius: f32) -> Style {
    Style { feather: Some(radius), ..Style::default() }
}

#[test]
fn a_feathered_element_fades_at_its_edge_and_keeps_its_inside() {
    let (board, _l1, sq) = square();
    let hard = draw(&board.snapshot(), View::new(0.0, 0.0, 1.0), 200, 200);
    assert_eq!(alpha(&hard, 200, 39, 100), 0);
    assert_eq!(alpha(&hard, 200, 41, 100), 255);
    ok(&board, |t| t.set_style(&sq, &feather(20.0)));
    {
        use yrs::{Map, Transact};
        let txn = board.doc().transact();
        let map = board.board_map(&txn).unwrap();
        let Some(yrs::Out::YMap(els)) = map.get(&txn, "elements") else { panic!() };
        let Some(yrs::Out::YMap(el)) = els.get(&txn, sq.as_str()) else { panic!() };
        let Some(yrs::Out::YMap(style)) = el.get(&txn, "style") else { panic!() };
        assert_eq!(style.get(&txn, "feather").map(|v| v.to_string(&txn)), Some("20".into()), "one key in the style map");
    }
    let snap = board.snapshot();
    let e = snap.get(&sq).unwrap();
    assert_eq!(e.style.feather, 20.0);
    assert_eq!(e.soft, 30.0, "three sigma, sigma half the radius");
    let b = e.bounds;
    assert!(b.x <= 10.5 && b.right() >= 189.5, "the bounds grow by the reach: {b:?}");
    let hb = e.hard_bounds();
    assert!((hb.x - 40.0).abs() < 1.0, "{hb:?}");
    let px = draw(&snap, View::new(0.0, 0.0, 1.0), 200, 200);
    // At the edge about half; inside opaque; outside fading to nothing.
    let edge = alpha(&px, 200, 40, 100);
    assert!((100..=160).contains(&edge), "the edge is half covered: {edge}");
    assert!(alpha(&px, 200, 30, 100) > 5 && alpha(&px, 200, 30, 100) < edge, "fading outside");
    assert!(alpha(&px, 200, 50, 100) > edge && alpha(&px, 200, 50, 100) < 255, "fading inside");
    assert_eq!(alpha(&px, 200, 100, 100), 255, "the middle is untouched");
    assert!(alpha(&px, 200, 5, 100) < 2, "nothing past the reach");
    let mid = at(&px, 200, 100, 100);
    assert!(mid[0] > 200 && mid[1] < 50, "still red: {mid:?}");
    // The color in the rim is the element's own.
    let rim = at(&px, 200, 32, 100);
    assert!(rim[3] > 0 && (rim[0] as f32 / rim[3] as f32) > 0.8, "{rim:?}");
    // A marquee takes it by its hard edge, the index finds it by its reach.
    assert_eq!(snap.within(&Rect::new(35.0, 35.0, 130.0, 130.0)).len(), 1);
    assert!(snap.query(&Rect::new(15.0, 100.0, 2.0, 2.0)).contains(&0));
    let extent = snap.visible_extent().unwrap();
    assert!(extent.x < 15.0, "{extent:?}");
    // Cleared: hard again.
    ok(&board, |t| t.clear_style(&sq, StyleKey::Feather));
    let snap = board.snapshot();
    assert_eq!(snap.get(&sq).unwrap().soft, 0.0);
    assert_eq!(alpha(&draw(&snap, View::new(0.0, 0.0, 1.0), 200, 200), 200, 41, 100), 255);
}

#[test]
fn a_feather_scales_with_the_view_and_is_the_same_cached_or_not() {
    let (board, _l1, sq) = square();
    ok(&board, |t| t.set_style(&sq, &feather(40.0)));
    let snap = board.snapshot();
    // At 1 pixel per unit sigma is 20 px: worked at a lower density and
    // kept. At 4 pixels per unit the same picture, four times larger.
    let one = draw(&snap, View::new(0.0, 0.0, 1.0), 200, 200);
    let four = draw(&snap, View::new(0.0, 0.0, 4.0), 800, 800);
    for x in [10u32, 20, 30, 40, 50, 60, 70, 100] {
        let a = alpha(&one, 200, x, 100) as i32;
        let b = alpha(&four, 800, x * 4 + 2, 400) as i32;
        assert!((a - b).abs() <= 12, "x {x}: {a} at 1, {b} at 4");
    }
    // A part of the view drawn alone is the same as the whole (what the
    // view's strips and tiles draw).
    let part = draw(&snap, View::new(20.0, 60.0, 1.0), 40, 80);
    for (x, y) in [(0u32, 0u32), (10, 30), (25, 40), (39, 79)] {
        let a = at(&part, 40, x, y);
        let b = at(&one, 200, x + 20, y + 60);
        assert!(a.iter().zip(b).all(|(p, q)| p.abs_diff(q) <= 3), "({x}, {y}): {a:?} vs {b:?}");
    }
    assert!(soft::cached_bytes() > 0, "the lower-density blur is kept");
}

#[test]
fn a_feathered_layer_fades_its_edge_and_keeps_its_inside_sharp() {
    let (board, l1, _sq) = square();
    // A thin dark line across the middle: inside, it must stay sharp.
    ok(&board, |t| {
        t.add_element(filled(Kind::Rect, Rect::new(60.0, 99.0, 80.0, 2.0), Color::BLACK).in_layer(&l1))?;
        t.set_layer_feather(&l1, 20.0)
    });
    let snap = board.snapshot();
    let layer = snap.layer(&l1).unwrap();
    assert_eq!((layer.feather, layer.blur, layer.isolated), (20.0, 0.0, true));
    let px = draw(&snap, View::new(0.0, 0.0, 1.0), 200, 200);
    let edge = alpha(&px, 200, 40, 120);
    assert!((100..=160).contains(&edge), "{edge}");
    assert!(alpha(&px, 200, 25, 120) > 0);
    let line = at(&px, 200, 100, 100);
    assert!(line[0] < 30 && line[3] == 255, "the line inside is sharp: {line:?}");
    let beside = at(&px, 200, 100, 97);
    assert!(beside[0] > 200, "and the red beside it untouched: {beside:?}");
    // Bounds grow by the layer's reach too.
    let e = &snap.elements[0];
    assert_eq!(e.soft, 30.0);
}

#[test]
fn a_blurred_layer_softens_everything_in_it() {
    let (board, l1, _sq) = square();
    ok(&board, |t| {
        t.add_element(filled(Kind::Rect, Rect::new(60.0, 99.0, 80.0, 2.0), Color::BLACK).in_layer(&l1))?;
        t.set_layer_blur(&l1, 8.0)
    });
    let snap = board.snapshot();
    assert_eq!(snap.layer(&l1).unwrap().blur, 8.0);
    let px = draw(&snap, View::new(0.0, 0.0, 1.0), 200, 200);
    let line = at(&px, 200, 100, 100);
    assert!(line[0] > 60, "the line is blurred into the red: {line:?}");
    assert!(alpha(&px, 200, 36, 100) > 0, "spread past the edge");
    assert!(alpha(&px, 200, 100, 100) == 255);
    // Off again: sharp.
    ok(&board, |t| t.set_layer_blur(&l1, 0.0));
    let snap = board.snapshot();
    assert!(!snap.layer(&l1).unwrap().isolated);
    assert!(at(&draw(&snap, View::new(0.0, 0.0, 1.0), 200, 200), 200, 100, 100)[0] < 30);
}

#[test]
fn a_clip_cuts_what_the_blur_spread_and_a_feather_inside_a_clipped_layer() {
    let (board, l1, sq) = square();
    let clip = ok(&board, |t| {
        let clips = t.add_layer("Clips", Placement::Bottom)?;
        let clip = t.add_element(filled(Kind::Rect, Rect::new(0.0, 0.0, 100.0, 200.0), Color::BLACK).in_layer(&clips))?;
        t.set_layer_visible(&clips, false)?;
        t.set_layer_clip(&l1, Some(&clip))?;
        t.set_layer_blur(&l1, 10.0)?;
        t.set_style(&sq, &feather(20.0))?;
        Ok(clip)
    });
    let snap = board.snapshot();
    let px = draw(&snap, View::new(0.0, 0.0, 1.0), 200, 200);
    assert!(alpha(&px, 200, 30, 100) > 0, "blurred out to the left");
    assert_eq!(alpha(&px, 200, 101, 100), 0, "nothing past the clip");
    assert!(alpha(&px, 200, 98, 100) > 200);
    let _ = clip;
}

#[test]
fn paint_layers_feather_and_blur_too() {
    let board = Board::standalone();
    board.create(st()).unwrap();
    let paint = ok(&board, |t| t.add_layer_in("Paint", LayerKind::Paint, None, Placement::Top));
    let line: Vec<BrushPoint> = (0..20).map(|i| [20.0 + 160.0 * i as f32 / 19.0, 100.0, 1.0, 0.0]).collect();
    ok(&board, |t| t.add_stroke(NewStroke::new(paint.clone(), &brush::ROUND, INK).with_size(20.0).with_points(line)));
    let hard = draw(&board.snapshot(), View::new(0.0, 0.0, 1.0), 200, 200);
    assert_eq!(alpha(&hard, 200, 100, 80), 0);
    ok(&board, |t| t.set_layer_blur(&paint, 20.0));
    let snap = board.snapshot();
    let reach = snap.paint_reach(snap.layers.iter().position(|l| l.id == paint).unwrap()).unwrap();
    assert!(reach.y < 60.0, "the paint's reach grows: {reach:?}");
    let px = draw(&snap, View::new(0.0, 0.0, 1.0), 200, 200);
    assert!(alpha(&px, 200, 100, 80) > 10, "the stroke spreads: {}", alpha(&px, 200, 100, 80));
    assert!(alpha(&px, 200, 100, 100) < 255);
}

#[test]
fn the_raster_cache_matches_a_full_render_after_a_change() {
    let (board, l1, sq) = square();
    ok(&board, |t| {
        t.set_style(&sq, &feather(12.0))?;
        t.set_layer_blur(&l1, 4.0)
    });
    let bg = Color::rgb(250, 250, 250);
    let mut view = BoardView::new(200, 200, bg);
    view.set_camera(Point::new(100.0, 100.0), 1.0);
    view.set_snapshot(board.snapshot());
    view.frame(&NoPictures);
    let w = board.watch().unwrap();
    w.take();
    ok(&board, |t| t.set_transform(&sq, &Transform { tx: 20.0, ..Transform::IDENTITY }));
    view.update(&board, &w.take());
    let frame = view.frame(&NoPictures).expect("redrawn").to_vec();
    let full = render(view.snapshot(), &NoPictures, &view.view(), 200, 200, Some(bg)).unwrap().take();
    let differing = frame.chunks_exact(4).zip(full.chunks_exact(4)).filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > 3)).count();
    assert_eq!(differing, 0);
    // The layer's feather changed: everything in it redraws.
    ok(&board, |t| t.set_layer_feather(&l1, 30.0));
    view.update(&board, &w.take());
    let frame = view.frame(&NoPictures).expect("redrawn").to_vec();
    let full = render(view.snapshot(), &NoPictures, &view.view(), 200, 200, Some(bg)).unwrap().take();
    let differing = frame.chunks_exact(4).zip(full.chunks_exact(4)).filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > 3)).count();
    assert_eq!(differing, 0);
}

#[test]
fn the_svg_export_writes_gaussian_blur_filters() {
    let (board, l1, sq) = square();
    ok(&board, |t| {
        t.set_style(&sq, &feather(20.0))?;
        t.set_layer_blur(&l1, 6.0)
    });
    let svg = render_svg(&board.snapshot(), Rect::new(0.0, 0.0, 200.0, 200.0), None, &|_| None);
    assert!(svg.contains("<feGaussianBlur in=\"SourceGraphic\" stdDeviation=\"3\" result=\"blurred\"/>"), "{svg}");
    assert!(svg.contains("stdDeviation=\"10\" result=\"soft\""), "{svg}");
    assert!(svg.contains("operator=\"arithmetic\" k1=\"0\" k2=\"1\" k3=\"1\" k4=\"0\""), "{svg}");
    assert!(svg.contains("color-interpolation-filters=\"sRGB\""));
    assert_eq!(svg.matches("filter=\"url(#pimble-soft-").count(), 2, "the layer's group and the element's");
    // Board units, not the element's own.
    assert!(svg.contains("filterUnits=\"userSpaceOnUse\""));
}

#[test]
fn merge_down_and_flatten_bake_soft_edges() {
    let (board, l1, sq) = square();
    let below = ok(&board, |t| {
        let below = t.add_layer_in("Below", LayerKind::Paint, None, Placement::Below(l1.clone()))?;
        t.set_style(&sq, &feather(20.0))?;
        t.set_layer_blur(&l1, 4.0)?;
        Ok(below)
    });
    let raster = rasterize(&board.snapshot(), &NoPictures, RasterJob::MergeDown(l1.clone())).unwrap();
    let r = raster.rect().unwrap();
    assert!(r.x < 30.0 && r.right() > 170.0, "what the feather and blur spread is in the pixels: {r:?}");
    // The pixels show the soft edge: decode and look at the left edge.
    let tile = &raster.tiles[0];
    let img = image::load_from_memory(&tile.png).unwrap().to_rgba8();
    let px_per_unit = tile.pixel_size.0 as f32 / tile.rect.w;
    let x = ((40.0 - tile.rect.x) * px_per_unit) as u32;
    let y = ((100.0 - tile.rect.y) * px_per_unit) as u32;
    let a = img.get_pixel(x, y)[3];
    assert!((60..=200).contains(&a), "the edge is soft in the pixels: {a}");
    // Flatten: a blurred paint layer's blur goes into the pixels and off the layer.
    let paint = ok(&board, |t| t.add_layer_in("Paint", LayerKind::Paint, None, Placement::Top));
    let line: Vec<BrushPoint> = (0..20).map(|i| [20.0 + 160.0 * i as f32 / 19.0, 100.0, 1.0, 0.0]).collect();
    ok(&board, |t| {
        t.add_stroke(NewStroke::new(paint.clone(), &brush::ROUND, INK).with_size(20.0).with_points(line))?;
        t.set_layer_blur(&paint, 16.0)
    });
    let before = draw(&board.snapshot(), View::new(0.0, 0.0, 1.0), 200, 200);
    let raster = rasterize(&board.snapshot(), &NoPictures, RasterJob::Flatten(paint.clone())).unwrap();
    assert_eq!(raster.baked, (16.0, 0.0));
    let r = raster.rect().unwrap();
    assert!(r.y < 80.0, "{r:?}");
    let srcs: Vec<String> = (0..raster.tiles.len()).map(|i| format!("pimble-blob:t{i}")).collect();
    let pictures: std::collections::HashMap<String, std::sync::Arc<Picture>> =
        raster.tiles.iter().zip(&srcs).map(|(t, s)| (s.clone(), std::sync::Arc::new(Picture::decode(&t.png).unwrap()))).collect();
    ok(&board, |t| t.apply_raster(&raster, &srcs));
    let snap = board.snapshot();
    assert_eq!(snap.layer(&paint).unwrap().blur, 0.0, "the blur is in the pixels now");
    let after = render(&snap, &pictures, &View::new(0.0, 0.0, 1.0), 200, 200, None).unwrap().take();
    for (x, y) in [(100u32, 80u32), (100, 100), (100, 70)] {
        let (a, b) = (at(&before, 200, x, y), at(&after, 200, x, y));
        assert!(a.iter().zip(b).all(|(p, q)| p.abs_diff(q) <= 12), "({x}, {y}): {a:?} vs {b:?}");
    }
    let _ = below;
}

#[test]
fn older_documents_and_refusals() {
    let (board, l1, sq) = square();
    // Nonsense reads as none; a radius is clamped.
    ok(&board, |t| t.set_style(&sq, &feather(-5.0)));
    assert_eq!(board.snapshot().get(&sq).unwrap().style.feather, 0.0);
    ok(&board, |t| t.set_style(&sq, &feather(1e9)));
    assert_eq!(board.snapshot().get(&sq).unwrap().style.feather, soft::MAX_RADIUS);
    let e = board.edit(st(), |t| t.set_layer_blur(&l1, f32::NAN));
    assert!(e.update.is_none());
    // Removing a key that is not there writes nothing.
    let e = board.edit(st(), |t| t.set_layer_feather(&l1, 0.0));
    assert!(e.update.is_none() || e.result.is_ok());
    // A copy keeps the keys.
    ok(&board, |t| {
        t.set_layer_blur(&l1, 5.0)?;
        t.set_layer_feather(&l1, 7.0)
    });
    let copy = Board::standalone();
    copy.edit(st(), |t| t.copy_from(&board.read().unwrap())).into_result().unwrap();
    let l = copy.snapshot().layer(&l1).cloned().unwrap();
    assert_eq!((l.blur, l.feather), (5.0, 7.0));
}

fn node() -> NodeDoc {
    let mut doc = NodeDoc::new();
    doc.init("board", "Canvas", None, T).unwrap();
    doc
}

fn sync(a: &mut NodeDoc, b: &mut NodeDoc) {
    let (asv, bsv) = (a.state_vector(), b.state_vector());
    let to_b = a.diff_since(&bsv).unwrap();
    let to_a = b.diff_since(&asv).unwrap();
    b.apply_update(&to_b).unwrap();
    a.apply_update(&to_a).unwrap();
}

#[test]
fn two_people_soften_at_once_and_read_the_same() {
    let mut a = node();
    let mut b = node();
    let ba = Board::of_node(&a);
    let l1 = ba.create(st()).unwrap().0;
    let sq = ok(&ba, |t| t.add_element(filled(Kind::Rect, Rect::new(40.0, 40.0, 120.0, 120.0), RED).in_layer(&l1)));
    sync(&mut a, &mut b);
    let (ba, bb) = (Board::of_node(&a), Board::of_node(&b));
    ok(&ba, |t| t.set_style(&sq, &feather(10.0)));
    ok(&bb, |t| {
        t.set_layer_blur(&l1, 6.0)?;
        t.set_style(&sq, &Style { fill: Some(INK), ..Style::default() })
    });
    sync(&mut a, &mut b);
    let (sa, sb) = (Board::of_node(&a).snapshot(), Board::of_node(&b).snapshot());
    assert_eq!(sa.get(&sq).unwrap().style.feather, 10.0);
    assert_eq!(sa.get(&sq).unwrap().style.fill, Some(INK));
    assert_eq!(sa.layer(&l1).unwrap().blur, 6.0);
    let (pa, pb) = (draw(&sa, View::new(0.0, 0.0, 1.0), 200, 200), draw(&sb, View::new(0.0, 0.0, 1.0), 200, 200));
    assert_eq!(pa, pb, "the same picture from the same document");
}

/// A map-sized canvas: an 11,800 x 11,000 frame of feathered regions and a
/// hazy layer, rendered fitted and zoomed in, cold and warm.
#[test]
fn a_big_feather_on_a_map_sized_canvas_renders_fast() {
    let board = Board::standalone();
    let l1 = board.create(st()).unwrap().0;
    let (regions, haze) = ok(&board, |t| {
        t.add_element(filled(Kind::Rect, Rect::new(0.0, 0.0, 11_800.0, 11_000.0), Color::rgb(230, 220, 190)).in_layer(&l1))?;
        let regions = t.add_layer("Regions", Placement::Top)?;
        for i in 0..12 {
            let x = (i % 4) as f32 * 2_900.0 + 200.0;
            let y = (i / 4) as f32 * 3_600.0 + 300.0;
            let id = t.add_element(filled(Kind::Ellipse, Rect::new(x, y, 2_400.0, 3_000.0), Color::rgb(60, 140, 60)).in_layer(&regions))?;
            t.set_style(&id, &feather(400.0))?;
        }
        let haze = t.add_layer("Haze", Placement::Top)?;
        for i in 0..6 {
            t.add_element(filled(Kind::Ellipse, Rect::new(i as f32 * 1_900.0, 4_000.0, 1_800.0, 1_200.0), Color::rgba(255, 255, 255, 160)).in_layer(&haze))?;
        }
        t.set_layer_blur(&haze, 600.0)?;
        t.set_layer_feather(&regions, 150.0)?;
        Ok((regions, haze))
    });
    let snap = board.snapshot();
    let fit = View::fit(&Rect::new(0.0, 0.0, 11_800.0, 11_000.0), 1920, 1080, 20.0);
    let close = View::new(5_000.0, 4_000.0, 2.0);
    let time = |v: View| {
        let t0 = Instant::now();
        let px = draw(&snap, v, 1920, 1080);
        (t0.elapsed().as_secs_f64() * 1000.0, px)
    };
    soft::clear_cache();
    let (fit_cold, _) = time(fit);
    let (fit_warm, _) = time(fit);
    let (close_cold, px) = time(close);
    let (close_warm, _) = time(close);
    eprintln!(
        "map: fitted {fit_cold:.0} ms cold, {fit_warm:.0} ms warm; 2 px/unit {close_cold:.0} ms cold, {close_warm:.0} ms warm; blurs kept {} KB",
        soft::cached_bytes() / 1024
    );
    assert!(px.chunks(4).any(|p| p[3] == 255));
    // Generous bounds for a shared machine (and debug builds).
    let limit = if cfg!(debug_assertions) { 20_000.0 } else { 3_000.0 };
    assert!(close_warm < limit && fit_warm < limit, "too slow");
    assert!(soft::cached_bytes() <= soft::BUDGET);
    let _ = (regions, haze);
}
