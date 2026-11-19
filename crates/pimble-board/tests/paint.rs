//! Paint layers (docs/DRAWING_CONTRACT.md section 3, wave 4): operations as
//! stored, the brush strokes they replay into, the tile cache and its
//! incremental path, the layer rules for paint tools, the highlighter on a
//! picture, the eyedropper, undo, and export.

use std::collections::HashMap;
use std::sync::Arc;

use pimble_board::*;

const T: &str = "2026-11-17T10:00:00.000Z";
const INK: Color = Color::rgb(20, 40, 200);

fn st() -> Stamp<'static> {
    Stamp::new(None, T)
}

fn ok<R: std::fmt::Debug>(board: &Board, f: impl FnOnce(&mut BoardTxn<'_, '_>) -> doc::Result<R>) -> R {
    board.edit(st(), f).into_result().unwrap().0
}

fn refused<R: std::fmt::Debug>(board: &Board, f: impl FnOnce(&mut BoardTxn<'_, '_>) -> doc::Result<R>) -> BoardError {
    let edited = board.edit(st(), f);
    assert!(edited.update.is_none(), "a refused write writes nothing");
    edited.result.unwrap_err()
}

/// A board with "Layer 1" and a paint layer above it.
fn painted() -> (Board, String, String) {
    let board = Board::standalone();
    let l1 = board.create(st()).unwrap().0;
    let paint = ok(&board, |t| t.paint_layer_for(None));
    (board, l1, paint)
}

fn line(x0: f32, x1: f32, y: f32, n: usize) -> Vec<BrushPoint> {
    (0..n).map(|i| [x0 + (x1 - x0) * i as f32 / (n - 1) as f32, y, 1.0, 0.0]).collect()
}

fn round(layer: &str) -> NewStroke {
    NewStroke::new(layer, &brush::ROUND, INK).with_size(10.0)
}

fn pixels(snap: &BoardSnapshot, view: View, w: u32, h: u32) -> Vec<u8> {
    render(snap, &NoPictures, &view, w, h, Some(Color::WHITE)).unwrap().take()
}

fn at(px: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [px[i], px[i + 1], px[i + 2], px[i + 3]]
}

fn inked(p: [u8; 4]) -> bool {
    p[2] > 150 && p[0] < 120
}

#[test]
fn a_stroke_is_stored_as_its_own_key_with_its_brush_and_points() {
    let (board, _, paint) = painted();
    let id = ok(&board, |t| t.begin_stroke(round(&paint).with_points(line(0.0, 10.0, 5.0, 3))));
    board.append_stroke(st(), &id, &line(12.0, 30.0, 5.0, 4)).unwrap();
    {
        use yrs::{Map, Transact};
        let txn = board.doc().transact();
        let map = board.board_map(&txn).unwrap();
        let keys: Vec<String> = map.keys(&txn).map(|k| k.to_string()).collect();
        assert!(keys.contains(&format!("paint:{id}")), "{keys:?}");
        assert!(!keys.contains(&"paint".to_string()), "no container made on first use");
    }
    let raw = board.read().unwrap();
    let op = raw.paint.iter().find(|o| o.id == id).unwrap();
    assert_eq!((op.layer.as_str(), op.kind()), (paint.as_str(), "stroke"));
    let PaintBody::Stroke(s) = &op.body else { panic!("a stroke") };
    assert_eq!(s.points.len(), 7);
    assert_eq!((s.preset.as_str(), s.color, s.blend), ("round", INK, StrokeBlend::Normal));
    assert_eq!(s.settings, brush::ROUND.settings.with_size(10.0));
    // An empty stroke ended is removed; one with points stays.
    let empty = ok(&board, |t| t.begin_stroke(round(&paint)));
    assert!(!ok(&board, |t| t.end_stroke(&empty)));
    assert!(ok(&board, |t| t.end_stroke(&id)));
    assert_eq!(board.snapshot().paint_ops(&paint).len(), 1);
    assert_eq!(board.snapshot().layer(&paint).unwrap().count, 1);
}

#[test]
fn strokes_draw_in_their_layer_and_the_eraser_takes_them_away() {
    let (board, _, paint) = painted();
    ok(&board, |t| t.add_stroke(round(&paint).with_points(line(10.0, 90.0, 50.0, 20))));
    let view = View::new(0.0, 0.0, 1.0);
    let px = pixels(&board.snapshot(), view, 100, 100);
    assert!(inked(at(&px, 100, 50, 50)), "{:?}", at(&px, 100, 50, 50));
    assert_eq!(at(&px, 100, 50, 20), [255, 255, 255, 255]);
    // Erase the middle.
    ok(&board, |t| t.add_stroke(NewStroke::new(paint.clone(), &brush::ROUND, Color::BLACK).with_size(20.0).erasing().with_points(line(40.0, 60.0, 50.0, 5))));
    let px = pixels(&board.snapshot(), view, 100, 100);
    assert_eq!(at(&px, 100, 50, 50), [255, 255, 255, 255], "erased");
    assert!(inked(at(&px, 100, 20, 50)), "the rest stays");
    // Hidden layer, hidden markup: nothing; show everything: it shows.
    ok(&board, |t| t.set_layer_visible(&paint, false));
    assert_eq!(at(&pixels(&board.snapshot(), view, 100, 100), 100, 20, 50), [255, 255, 255, 255]);
    ok(&board, |t| t.set_layer_visible(&paint, true));
    ok(&board, |t| t.set_markup_visible(false));
    let snap = board.snapshot();
    assert_eq!(at(&pixels(&snap, view, 100, 100), 100, 20, 50), [255, 255, 255, 255]);
    assert!(snap.visible_extent().is_none());
    let all = BoardSnapshot::build_with(board.read(), ViewOptions { show_all: true, ..ViewOptions::default() });
    assert!(inked(at(&pixels(&all, view, 100, 100), 100, 20, 50)));
}

#[test]
fn paint_layers_composite_in_layer_order_with_their_blend() {
    let (board, l1, paint) = painted();
    // A red rectangle in the vector layer below, a vector layer above with
    // a green one: the paint sits between them.
    let red = NewElement::new(Kind::Rect, Geom::Rect(Rect::new(0.0, 0.0, 100.0, 100.0)))
        .with_style(Style { fill: Some(Color::rgb(220, 30, 30)), width: Some(0.0), ..Style::default() })
        .in_layer(l1.clone());
    ok(&board, |t| t.add_element(red));
    let top = ok(&board, |t| t.add_layer("Top", Placement::Top));
    let green = NewElement::new(Kind::Rect, Geom::Rect(Rect::new(60.0, 0.0, 40.0, 100.0)))
        .with_style(Style { fill: Some(Color::rgb(30, 200, 30)), width: Some(0.0), ..Style::default() })
        .in_layer(top);
    ok(&board, |t| t.add_element(green));
    ok(&board, |t| t.add_stroke(NewStroke::new(paint.clone(), &brush::MARKER, Color::rgb(255, 235, 59)).with_settings(brush::HIGHLIGHTER.settings).with_points(line(5.0, 95.0, 50.0, 10))));
    let view = View::new(0.0, 0.0, 1.0);
    let px = pixels(&board.snapshot(), view, 100, 100);
    assert_eq!(at(&px, 100, 30, 50)[..3], [255, 235, 59], "normal: the paint covers the red");
    assert_eq!(at(&px, 100, 80, 50)[..3], [30, 200, 30], "the vector layer above covers the paint");
    ok(&board, |t| t.set_layer_blend(&paint, Blend::Multiply));
    let px = pixels(&board.snapshot(), view, 100, 100);
    let p = at(&px, 100, 30, 50);
    assert!(p[0] > 200 && p[1] < 40 && p[2] < 20, "multiply over red: {p:?}");
}

#[test]
fn an_appended_stroke_redraws_only_its_new_dabs_and_matches_a_replay() {
    let (board, _, paint) = painted();
    // Something under it, so the stroke is laid over pixels.
    ok(&board, |t| t.add_stroke(NewStroke::new(paint.clone(), &brush::AIRBRUSH, Color::rgb(250, 120, 0)).with_size(80.0).with_points(line(0.0, 300.0, 100.0, 8))));
    let mut view = BoardView::new(320, 220, Color::WHITE);
    view.set_camera(Point::new(150.0, 100.0), 1.0);
    view.update(&board, &Changes { all: true, ..Changes::default() });
    view.frame(&NoPictures);
    let watch = board.watch().unwrap();
    let id = ok(&board, |t| t.begin_stroke(NewStroke::new(paint.clone(), &brush::WATERCOLOR, INK).with_size(24.0)));
    let points: Vec<BrushPoint> = (0..120).map(|i| [10.0 + i as f32 * 2.3, 40.0 + (i as f32 * 0.2).sin() * 60.0, 0.3 + (i % 10) as f32 / 14.0, 0.0]).collect();
    for chunk in points.chunks(3) {
        board.append_stroke(st(), &id, chunk).unwrap();
        view.update(&board, &watch.take());
        view.frame(&NoPictures);
    }
    let live = view.current_frame(&NoPictures).to_vec();
    let stats = view.snapshot().paint_cache().stats();
    assert!(stats.appended > 20, "new dabs stamped, not tiles replayed: {stats:?}");
    // The same board opened cold draws exactly the same pixels.
    let mut cold = BoardView::new(320, 220, Color::WHITE);
    cold.set_camera(Point::new(150.0, 100.0), 1.0);
    cold.update(&board, &Changes { all: true, ..Changes::default() });
    assert_eq!(cold.current_frame(&NoPictures), &live[..]);
    assert_eq!(cold.snapshot().paint_cache().stats().appended, 0);
}

#[test]
fn a_tile_unchanged_is_never_replayed_and_a_change_under_it_replays_it() {
    let (board, _, paint) = painted();
    let a = ok(&board, |t| t.add_stroke(round(&paint).with_points(line(10.0, 60.0, 20.0, 6))));
    let snap = board.snapshot();
    let cache = snap.paint_cache().clone();
    let first = cache.tile_pixels(&snap, &paint, 0, 0, &NoPictures).unwrap();
    let again = cache.tile_pixels(&snap, &paint, 0, 0, &NoPictures).unwrap();
    assert_eq!(first, again);
    let s = cache.stats();
    assert_eq!((s.replayed, s.hits), (1, 1));
    // A stroke on top is laid over without a replay.
    ok(&board, |t| t.add_stroke(round(&paint).with_points(line(10.0, 60.0, 40.0, 6))));
    let snap = board.snapshot().with_paint_cache(cache.clone());
    cache.tile_pixels(&snap, &paint, 0, 0, &NoPictures).unwrap();
    assert_eq!(cache.stats().stacked, 1);
    // The bottom stroke removed: replayed.
    ok(&board, |t| t.delete_paint_op(&a));
    let snap = board.snapshot().with_paint_cache(cache.clone());
    let after = cache.tile_pixels(&snap, &paint, 0, 0, &NoPictures).unwrap();
    assert_eq!(cache.stats().replayed, 2);
    assert_eq!(after, PaintCache::new().tile_pixels(&snap, &paint, 0, 0, &NoPictures).unwrap());
}

#[test]
fn tiles_meet_without_a_seam_at_any_zoom() {
    let (board, _, paint) = painted();
    // A wide marker band across the tile edge at x = 128.
    ok(&board, |t| t.add_stroke(NewStroke::new(paint.clone(), &brush::MARKER, INK).with_size(40.0).with_settings(BrushSettings { opacity: 1.0, ..brush::MARKER.settings.with_size(40.0) }).with_points(line(60.0, 200.0, 64.0, 30))));
    for scale in [0.37, 1.0, 1.3, 2.0, 5.5] {
        let view = View::new(100.0, 54.0, scale);
        let w = (60.0 * scale) as u32;
        let h = (20.0 * scale) as u32;
        let px = pixels(&board.snapshot(), view, w.max(4), h.max(4));
        for x in 0..w.max(4) {
            let p = at(&px, w.max(4), x, h.max(4) / 2);
            assert!(p[0] < 60 && p[2] > 150, "scale {scale}, column {x}: {p:?}");
        }
    }
}

#[test]
fn two_people_painting_at_once_both_appear_in_order() {
    let a = Board::standalone();
    a.create(st()).unwrap();
    let paint = ok(&a, |t| t.paint_layer_for(None));
    let b = {
        use yrs::{updates::decoder::Decode, ReadTxn, Transact, Update};
        let b = Board::standalone();
        let state = a.doc().transact().encode_state_as_update_v1(&yrs::StateVector::default());
        b.doc().transact_mut().apply_update(Update::decode_v1(&state).unwrap()).unwrap();
        let _ = b.doc().transact().state_vector();
        b
    };
    let (ia, ua) = a.edit(st(), |t| t.add_stroke(round(&paint).with_points(line(10.0, 90.0, 30.0, 8)))).into_result().unwrap();
    let (ib, ub) = b.edit(st(), |t| t.add_stroke(NewStroke::new(paint.clone(), &brush::ROUND, Color::rgb(200, 20, 20)).with_size(10.0).with_points(line(10.0, 90.0, 60.0, 8)))).into_result().unwrap();
    for (board, update) in [(&a, ub.unwrap()), (&b, ua.unwrap())] {
        use yrs::{updates::decoder::Decode, Transact, Update};
        board.doc().transact_mut().apply_update(Update::decode_v1(&update).unwrap()).unwrap();
    }
    let (sa, sb) = (a.snapshot(), b.snapshot());
    let ids = |s: &BoardSnapshot| s.paint_ops(&paint).iter().map(|o| o.raw.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&sa), ids(&sb), "every device replays the same order");
    assert!(ids(&sa).contains(&ia) && ids(&sa).contains(&ib));
    let view = View::new(0.0, 0.0, 1.0);
    assert_eq!(pixels(&sa, view, 100, 100), pixels(&sb, view, 100, 100));
}

#[test]
fn undo_takes_back_my_last_stroke_and_never_yours() {
    let mine = Board::standalone().with_origin("me");
    mine.create(st()).unwrap();
    let theirs = Board::new(mine.doc().clone(), mine.doc().get_or_insert_map(pimble_crdt::ROOT_DATA), &[BOARD_KEY]).with_origin("them");
    let paint = ok(&mine, |t| t.paint_layer_for(None));
    let mut manager = mine.undo_manager(yrs::undo::Options::default()).unwrap();
    manager.reset();
    let id = ok(&mine, |t| t.begin_stroke(round(&paint)));
    for chunk in line(0.0, 100.0, 10.0, 12).chunks(3) {
        mine.append_stroke(st(), &id, chunk).unwrap();
    }
    ok(&mine, |t| t.end_stroke(&id));
    let other = ok(&theirs, |t| t.add_stroke(round(&paint).with_points(line(0.0, 100.0, 50.0, 4))));
    assert_eq!(mine.snapshot().paint_ops(&paint).len(), 2);
    mine.undo(&mut manager, false).expect("an undo");
    let left: Vec<String> = mine.snapshot().paint_ops(&paint).iter().map(|o| o.raw.id.clone()).collect();
    assert_eq!(left, vec![other]);
}

#[test]
fn paint_tools_pick_their_layer_by_the_contract() {
    let board = Board::standalone();
    let l1 = board.create(st()).unwrap().0;
    // No paint layer: "Paint 1" above the picked layer, in one write with
    // the stroke.
    let (p1, _) = ok(&board, |t| {
        let layer = t.paint_layer_for(Some(&l1))?;
        let id = t.add_stroke(round(&layer).with_points(line(0.0, 10.0, 0.0, 2)))?;
        Ok((layer, id))
    });
    let snap = board.snapshot();
    let names: Vec<&str> = snap.layers.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["Layer 1", "Paint 1"]);
    assert_eq!(snap.layer(&p1).unwrap().kind, LayerKind::Paint);
    // Picked vector layer below a paint layer: that paint layer.
    assert_eq!(ok(&board, |t| t.paint_layer_for(Some(&l1))), p1);
    // A locked paint layer is passed over: a new one.
    ok(&board, |t| t.set_layer_locked(&p1, true));
    let p2 = ok(&board, |t| t.paint_layer_for(Some(&p1)));
    assert_ne!(p2, p1);
    assert_eq!(board.snapshot().layer(&p2).unwrap().name, "Paint 2");
    // Nothing picked: the topmost paint layer that takes paint.
    assert_eq!(ok(&board, |t| t.paint_layer_for(None)), p2);
    // Refusals.
    assert!(matches!(refused(&board, |t| t.begin_stroke(round(&l1))), BoardError::NotAPaintLayer { kind: LayerKind::Vector, .. }));
    assert!(matches!(refused(&board, |t| t.begin_stroke(round(&p1))), BoardError::LayerLocked(_)));
    // Deleting a layer takes its paint with it.
    ok(&board, |t| t.set_layer_locked(&p1, false));
    ok(&board, |t| t.delete_layer(&p1));
    assert!(board.read().unwrap().paint.is_empty());
}

fn picture_canvas() -> (Board, String, String, String) {
    let board = Board::standalone();
    let made = ok(&board, |t| t.start_canvas(Some(recipe::StartingPicture { src: "pimble-blob:s/b", px_w: 40, px_h: 40 })));
    let (layer, picture) = made.picture.unwrap();
    (board, made.draw_layer, layer, picture)
}

#[test]
fn the_highlighter_on_bare_canvas_paints_a_translucent_normal_layer() {
    let board = Board::standalone();
    ok(&board, |t| t.start_canvas(None));
    let h = ok(&board, |t| t.highlights_layer(None));
    let snap = board.snapshot();
    let layer = snap.layer(&h).unwrap();
    assert_eq!((layer.kind.clone(), layer.blend, layer.name.as_str()), (LayerKind::Paint, Blend::Normal, "Highlights"));
    assert!((layer.opacity - pimble_board::BARE_HIGHLIGHTS_OPACITY).abs() < 1e-6, "{}", layer.opacity);
    assert_eq!(ok(&board, |t| t.highlights_layer(None)), h, "reused");
}

#[test]
fn the_highlighter_on_a_picture_paints_a_multiply_layer_above_it() {
    let (board, draw, picture_layer, picture) = picture_canvas();
    let h = ok(&board, |t| t.highlights_layer(Some(&picture)));
    let snap = board.snapshot();
    let names: Vec<&str> = snap.layers.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["Picture", "Highlights", "Layer 1"]);
    let layer = snap.layer(&h).unwrap();
    assert_eq!((layer.kind.clone(), layer.blend), (LayerKind::Paint, Blend::Multiply));
    assert_eq!(ok(&board, |t| t.highlights_layer(Some(&picture))), h, "reused");
    let _ = (draw, picture_layer);
    ok(&board, |t| t.add_stroke(NewStroke::new(h.clone(), &brush::HIGHLIGHTER, brush::HIGHLIGHTER.color).with_points(line(2.0, 38.0, 20.0, 6))));
    let red = Arc::new(Picture::from_rgba(40, 40, [200u8, 50, 50, 255].repeat(1600)).unwrap());
    let pictures: HashMap<String, Arc<Picture>> = [("pimble-blob:s/b".to_string(), red)].into();
    let px = render(&board.snapshot(), &pictures, &View::new(0.0, 0.0, 1.0), 40, 40, None).unwrap().take();
    let p = at(&px, 40, 20, 20);
    // Multiply: 200 x 255 / 255, 50 x 235 / 255, 50 x 59 / 255.
    assert!((p[0] as i32 - 200).abs() <= 2 && (p[1] as i32 - 46).abs() <= 2 && (p[2] as i32 - 12).abs() <= 2, "{p:?}");
    assert_eq!(at(&px, 40, 20, 2)[..3], [200, 50, 50], "outside the stroke: the picture");
    // The eyedropper reads the composite.
    let c = sample_color(&board.snapshot(), &pictures, Point::new(20.0, 20.0), None);
    assert!((c.g as i32 - 46).abs() <= 2, "{c:?}");
    assert_eq!(sample_color(&board.snapshot(), &NoPictures, Point::new(-50.0, -50.0), None), Color::TRANSPARENT);
    assert_eq!(sample_color(&board.snapshot(), &NoPictures, Point::new(-50.0, -50.0), Some(Color::WHITE)), Color::WHITE);
}

#[test]
fn pixels_draw_their_picture_and_wait_for_it() {
    let (board, _, paint) = painted();
    ok(&board, |t| t.add_pixels(&paint, "pimble-blob:s/p", Rect::new(10.0, 10.0, 20.0, 20.0)));
    let snap = board.snapshot();
    assert_eq!(snap.picture_srcs(), vec!["pimble-blob:s/p".to_string()]);
    // Not here yet: nothing drawn, and nothing kept.
    let px = render(&snap, &NoPictures, &View::new(0.0, 0.0, 1.0), 40, 40, None).unwrap().take();
    assert_eq!(at(&px, 40, 20, 20)[3], 0);
    let green = Arc::new(Picture::from_rgba(2, 2, [0u8, 180, 0, 255].repeat(4)).unwrap());
    let pictures: HashMap<String, Arc<Picture>> = [("pimble-blob:s/p".to_string(), green)].into();
    let px = render(&snap, &pictures, &View::new(0.0, 0.0, 1.0), 40, 40, None).unwrap().take();
    assert_eq!(at(&px, 40, 20, 20), [0, 180, 0, 255]);
    assert_eq!(at(&px, 40, 5, 5)[3], 0);
    // At scale 1 it is a level-1 tile (docs/DRAWING_CONTRACT.md "Large
    // canvases": the pyramid), made from the picture straight.
    let stats = snap.paint_cache().stats();
    assert_eq!(stats.replayed + stats.merged, 2, "the tile drawn without its picture was drawn again: {stats:?}");
}

#[test]
fn kinds_this_version_does_not_know_are_skipped() {
    let (board, _, paint) = painted();
    ok(&board, |t| t.add_stroke(round(&paint).with_points(line(10.0, 90.0, 50.0, 10))));
    {
        use yrs::{Map, MapPrelim, Transact};
        let mut txn = board.doc().transact_mut();
        let map = board.board_map(&txn).unwrap();
        let op: yrs::MapRef = map.insert(&mut txn, "paint:later", MapPrelim::default());
        op.insert(&mut txn, "layer", paint.clone());
        op.insert(&mut txn, "order", "zz");
        op.insert(&mut txn, "kind", "sparkle");
    }
    let snap = board.snapshot();
    assert_eq!(snap.paint_ops(&paint).len(), 2);
    assert!(matches!(snap.paint_op("later").unwrap().raw.body, PaintBody::Other(ref k) if k == "sparkle"));
    assert!(inked(at(&pixels(&snap, View::new(0.0, 0.0, 1.0), 100, 100), 100, 50, 50)));
}

#[test]
fn export_and_copies_carry_paint() {
    let (board, _, paint) = painted();
    ok(&board, |t| t.add_stroke(round(&paint).with_points(line(10.0, 90.0, 50.0, 10))));
    let snap = board.snapshot();
    let extent = snap.visible_extent().unwrap();
    assert!(extent.x < 10.0 && extent.right() > 90.0);
    let rendered = render_extent(&snap, &NoPictures, 1.0).unwrap();
    assert!(rendered.image.data.chunks_exact(4).any(|p| p[3] == 255));
    let svg = render_svg(&snap, Rect::new(0.0, 0.0, 100.0, 100.0), None, &|_| None);
    assert!(svg.contains("<image") && svg.contains("data:image/png;base64,"), "{svg}");
    let thumb = render_paint_layer(&snap, &paint, &NoPictures, 0.5, None).unwrap();
    assert!(thumb.image.width < 60);
    // Convert to Node copies the operations.
    let copy = Board::standalone();
    copy.edit(st(), |t| t.copy_from(&board.read().unwrap())).into_result().unwrap();
    assert_eq!(copy.read().unwrap().paint, board.read().unwrap().paint);
}

#[test]
fn tiles_saved_to_disk_come_back_the_same() {
    let (board, _, paint) = painted();
    ok(&board, |t| t.add_stroke(NewStroke::new(paint.clone(), &brush::CHARCOAL, INK).with_points(line(-40.0, 300.0, 60.0, 30))));
    let dir = std::env::temp_dir().join(format!("pimble-tiles-test-{}", std::process::id()));
    let store: Arc<dyn TileStore> = Arc::new(DirTileStore::new(&dir));
    let first = board.snapshot().with_paint_cache(PaintCache::with_store(Some(store.clone()), 64 << 20));
    let a = render_paint_layer(&first, &paint, &NoPictures, 2.0, None).unwrap();
    let second = board.snapshot().with_paint_cache(PaintCache::with_store(Some(store), 64 << 20));
    let b = render_paint_layer(&second, &paint, &NoPictures, 2.0, None).unwrap();
    assert!(second.paint_cache().stats().loaded >= 3, "{:?}", second.paint_cache().stats());
    assert_eq!(second.paint_cache().stats().replayed, 0);
    assert_eq!(a.image, b.image);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_snapshot_brought_up_to_date_keeps_its_tiles() {
    let (board, _, paint) = painted();
    ok(&board, |t| t.add_stroke(round(&paint).with_points(line(10.0, 90.0, 50.0, 10))));
    let watch = board.watch().unwrap();
    let snap = board.snapshot();
    let cache = snap.paint_cache().clone();
    render_paint_layer(&snap, &paint, &NoPictures, 2.0, None).unwrap();
    let id = ok(&board, |t| t.begin_stroke(round(&paint)));
    board.append_stroke(st(), &id, &line(10.0, 50.0, 20.0, 3)).unwrap();
    let changes = watch.take();
    assert!(changes.paint.contains(&id) && changes.elements.is_empty() && !changes.board, "{changes:?}");
    let snap = snap.update(&board, &changes);
    assert!(Arc::ptr_eq(snap.paint_cache(), &cache));
    assert_eq!(snap.paint_ops(&paint).len(), 2);
    assert_eq!(snap.layer(&paint).unwrap().count, 2);
}
