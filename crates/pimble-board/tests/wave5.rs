//! Wave 5 of the drawing contract (docs/DRAWING_CONTRACT.md section 3):
//! fill, smudge, blur, moves of paint selections, Flatten Layer, and Merge
//! Down and Merge Group across kinds, with the tile cache following
//! changes below an operation that reads the pixels below it.

use std::collections::HashMap;
use std::sync::Arc;

use pimble_board::*;

const T: &str = "2026-11-17T10:00:00.000Z";
const RED: Color = Color::rgb(220, 30, 30);
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

fn ring(cx: f32, cy: f32, r: f32) -> Vec<BrushPoint> {
    (0..=72).map(|i| {
        let a = i as f32 / 72.0 * std::f32::consts::TAU;
        [cx + r * a.cos(), cy + r * a.sin(), 1.0, 0.0]
    })
    .collect()
}

fn hard(layer: &str, color: Color, size: f32) -> NewStroke {
    NewStroke::new(layer, &brush::ROUND, color).with_settings(BrushSettings { hardness: 1.0, opacity: 1.0, flow: 1.0, ..brush::ROUND.settings.with_size(size) })
}

fn pixels(snap: &BoardSnapshot, pictures: &dyn PictureSource, view: View, w: u32, h: u32) -> Vec<u8> {
    render(snap, pictures, &view, w, h, Some(Color::WHITE)).unwrap().take()
}

fn px1(snap: &BoardSnapshot, w: u32, h: u32) -> Vec<u8> {
    pixels(snap, &NoPictures, View::new(0.0, 0.0, 1.0), w, h)
}

fn at(px: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [px[i], px[i + 1], px[i + 2], px[i + 3]]
}

fn is_red(p: [u8; 4]) -> bool {
    p[0] > 190 && p[1] < 70 && p[2] < 70
}

const WHITE: [u8; 4] = [255, 255, 255, 255];

fn max_diff(a: &[u8], b: &[u8]) -> u8 {
    a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0)
}

/// A filled square on an empty paint layer: a fill whose clip is the square.
fn square(board: &Board, layer: &str, r: Rect, color: Color) -> String {
    let seed = r.center();
    ok(board, |t| t.add_fill(NewFill::new(layer, seed, color, r)))
}

#[test]
fn a_fill_is_stored_with_its_seed_clip_and_tolerance() {
    let (board, _, paint) = painted();
    let clip = Rect::new(0.0, 0.0, 200.0, 100.0);
    let id = ok(&board, |t| t.add_fill(NewFill::new(&paint, Point::new(50.0, 50.0), RED, clip).with_tolerance(0.3).everywhere()));
    let raw = board.read().unwrap();
    let op = raw.paint.iter().find(|o| o.id == id).unwrap();
    assert_eq!(op.kind(), "fill");
    let PaintBody::Fill(f) = &op.body else { panic!("a fill") };
    assert_eq!((f.seed, f.color, f.clip, f.contiguous), (Point::new(50.0, 50.0), RED, clip, false));
    assert!((f.tolerance - 0.3).abs() < 1e-6);
    // Refusals write nothing.
    assert!(matches!(refused(&board, |t| t.add_fill(NewFill::new(&paint, Point::new(500.0, 50.0), RED, clip))), BoardError::Invalid(_)));
    let huge = Rect::new(0.0, 0.0, 5000.0, 10.0);
    assert!(matches!(refused(&board, |t| t.add_fill(NewFill::new(&paint, Point::new(5.0, 5.0), RED, huge))), BoardError::Invalid(_)));
    let l1 = board.snapshot().layers.iter().find(|l| l.kind == LayerKind::Vector).unwrap().id.clone();
    assert!(matches!(refused(&board, |t| t.add_fill(NewFill::new(&l1, Point::new(5.0, 5.0), RED, clip))), BoardError::NotAPaintLayer { .. }));
}

#[test]
fn a_fill_fills_inside_a_ring_and_follows_a_change_below_it() {
    let (board, _, paint) = painted();
    let wall = ok(&board, |t| t.add_stroke(hard(&paint, INK, 6.0).with_points(ring(100.0, 100.0, 40.0))));
    let snap = board.snapshot();
    let clip = fill_clip(&snap, &paint, Point::new(100.0, 100.0), Some(Rect::new(0.0, 0.0, 200.0, 200.0)));
    assert!(clip.contains(Point::new(100.0, 100.0)) && clip.x <= 0.0 && clip.right() >= 200.0);
    ok(&board, |t| t.add_fill(NewFill::new(&paint, Point::new(100.0, 100.0), RED, clip)));
    let cache = PaintCache::new();
    let mut view = BoardView::new(200, 200, Color::WHITE);
    view.set_paint_cache(cache.clone());
    view.set_camera(Point::new(100.0, 100.0), 1.0);
    view.update(&board, &Changes { all: true, ..Changes::default() });
    let frame = view.current_frame(&NoPictures).to_vec();
    assert!(is_red(at(&frame, 200, 100, 100)), "{:?}", at(&frame, 200, 100, 100));
    assert!(is_red(at(&frame, 200, 100, 64)), "up to the wall: {:?}", at(&frame, 200, 100, 64));
    assert_eq!(at(&frame, 200, 10, 10), WHITE, "outside the ring");
    // The wall taken away: the same fill now reaches the whole clip, and
    // the view redraws where the wall never was.
    let watch = board.watch().unwrap();
    ok(&board, |t| t.delete_paint_op(&wall));
    view.update(&board, &watch.take());
    let frame = view.current_frame(&NoPictures).to_vec();
    assert!(is_red(at(&frame, 200, 10, 10)), "{:?}", at(&frame, 200, 10, 10));
    // A cold render agrees.
    assert_eq!(frame, px1(&board.snapshot(), 200, 200));
}

#[test]
fn a_fill_is_the_same_on_every_device_and_after_a_disk_round_trip() {
    let (a, _, paint) = painted();
    ok(&a, |t| t.add_stroke(hard(&paint, INK, 6.0).with_points(ring(60.0, 60.0, 30.0))));
    let b = {
        use yrs::{updates::decoder::Decode, ReadTxn, Transact, Update};
        let b = Board::standalone();
        let state = a.doc().transact().encode_state_as_update_v1(&yrs::StateVector::default());
        b.doc().transact_mut().apply_update(Update::decode_v1(&state).unwrap()).unwrap();
        b
    };
    let clip = Rect::new(0.0, 0.0, 120.0, 120.0);
    let (_, ua) = a.edit(st(), |t| t.add_fill(NewFill::new(&paint, Point::new(60.0, 60.0), RED, clip))).into_result().unwrap();
    // Meanwhile on b, a stroke across the ring's inside.
    let (_, ub) = b.edit(st(), |t| t.add_stroke(hard(&paint, Color::rgb(30, 160, 30), 4.0).with_points(line(40.0, 80.0, 60.0, 6)))).into_result().unwrap();
    for (board, update) in [(&a, ub.unwrap()), (&b, ua.unwrap())] {
        use yrs::{updates::decoder::Decode, Transact, Update};
        board.doc().transact_mut().apply_update(Update::decode_v1(&update).unwrap()).unwrap();
    }
    let (pa, pb) = (px1(&a.snapshot(), 120, 120), px1(&b.snapshot(), 120, 120));
    assert_eq!(pa, pb, "every device replays the same pixels");
    // Tiles saved to disk come back the same.
    let dir = std::env::temp_dir().join(format!("pimble-wave5-tiles-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store: Arc<dyn TileStore> = Arc::new(DirTileStore::new(&dir));
    let first = PaintCache::with_store(Some(store.clone()), 64 << 20);
    let saved = px1(&a.snapshot().with_paint_cache(first), 120, 120);
    let second = PaintCache::with_store(Some(store), 64 << 20);
    let loaded = px1(&a.snapshot().with_paint_cache(second.clone()), 120, 120);
    assert_eq!(saved, loaded);
    assert!(second.stats().loaded > 0 && second.stats().replayed == 0, "{:?}", second.stats());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_smudge_drags_color_and_streams_like_a_stroke() {
    let (board, _, paint) = painted();
    ok(&board, |t| t.add_stroke(hard(&paint, RED, 20.0).with_points(line(20.0, 100.0, 100.0, 12))));
    let mut view = BoardView::new(200, 200, Color::WHITE);
    view.set_camera(Point::new(100.0, 100.0), 1.0);
    view.update(&board, &Changes { all: true, ..Changes::default() });
    view.frame(&NoPictures);
    assert_eq!(at(view.current_frame(&NoPictures), 200, 130, 100), WHITE);
    let watch = board.watch().unwrap();
    let smudge = NewRetouch::new(&paint, Retouch::Smudge, &brush::ROUND).with_size(16.0).with_strength(0.9);
    let id = ok(&board, |t| t.begin_retouch(smudge.with_points(line(85.0, 88.0, 100.0, 2))));
    for chunk in line(90.0, 150.0, 100.0, 24).chunks(4) {
        board.append_stroke(st(), &id, chunk).unwrap();
        view.update(&board, &watch.take());
        view.frame(&NoPictures);
    }
    assert!(ok(&board, |t| t.end_stroke(&id)));
    let live = view.current_frame(&NoPictures).to_vec();
    let p = at(&live, 200, 125, 100);
    assert!(p[0] > p[1] + 40 && p != WHITE, "red dragged along: {p:?}");
    assert_eq!(board.read().unwrap().paint.iter().find(|o| o.id == id).unwrap().kind(), "smudge");
    // Opened cold, the same pixels.
    let mut cold = BoardView::new(200, 200, Color::WHITE);
    cold.set_camera(Point::new(100.0, 100.0), 1.0);
    cold.update(&board, &Changes { all: true, ..Changes::default() });
    assert_eq!(cold.current_frame(&NoPictures), &live[..]);
}

#[test]
fn a_blur_softens_an_edge_where_it_passes() {
    let (board, _, paint) = painted();
    square(&board, &paint, Rect::new(0.0, 0.0, 100.0, 200.0), RED);
    let before = px1(&board.snapshot(), 200, 200);
    assert!(is_red(at(&before, 200, 99, 100)) && at(&before, 200, 101, 100) == WHITE);
    ok(&board, |t| t.add_retouch(NewRetouch::new(&paint, Retouch::Blur, &brush::ROUND).with_size(16.0).with_strength(1.0).with_points(line(100.0, 100.0, 60.0, 2).into_iter().chain(line(100.0, 100.0, 140.0, 2)).collect())));
    let after = px1(&board.snapshot(), 200, 200);
    let (inside, outside) = (at(&after, 200, 99, 100), at(&after, 200, 101, 100));
    assert!(inside[1] > 40, "the red edge softened: {inside:?}");
    assert!(outside != WHITE && outside[1] < 250, "red spilled over: {outside:?}");
    assert_eq!(at(&after, 200, 99, 20), at(&before, 200, 99, 20), "away from the stroke, unchanged");
    assert_eq!(at(&after, 200, 30, 100), at(&before, 200, 30, 100));
}

#[test]
fn a_selection_moves_turns_and_deletes() {
    let (board, _, paint) = painted();
    square(&board, &paint, Rect::new(20.0, 20.0, 40.0, 40.0), RED);
    let sel = Selection::Rect(Rect::new(10.0, 10.0, 60.0, 60.0));
    let id = ok(&board, |t| t.add_move(&paint, sel.clone(), Some(PixelTransform::translate(100.0, 0.0))));
    let px = px1(&board.snapshot(), 200, 100);
    assert_eq!(at(&px, 200, 40, 40), WHITE, "lifted");
    assert!(is_red(at(&px, 200, 140, 40)), "put down");
    assert_eq!(at(&px, 200, 165, 40), WHITE);
    // Dragged on: one value.
    ok(&board, |t| t.set_move_transform(&id, Some(PixelTransform::translate(100.0, 20.0))));
    let px = px1(&board.snapshot(), 200, 100);
    assert!(is_red(at(&px, 200, 140, 75)) && at(&px, 200, 140, 30) == WHITE);
    // Delete: nothing put down.
    ok(&board, |t| t.set_move_transform(&id, None));
    let px = px1(&board.snapshot(), 200, 100);
    assert_eq!(at(&px, 200, 140, 75), WHITE);
    assert_eq!(at(&px, 200, 40, 40), WHITE);
    ok(&board, |t| t.delete_paint_op(&id));
    // A lasso keeps what is outside it; turned 90 degrees about its center.
    let lasso = Selection::Lasso(vec![Point::new(20.0, 20.0), Point::new(60.0, 20.0), Point::new(20.0, 60.0)]);
    assert!(lasso.contains(Point::new(25.0, 25.0)) && !lasso.contains(Point::new(55.0, 55.0)));
    ok(&board, |t| t.add_move(&paint, lasso, Some(PixelTransform::rotate(90.0, Point::new(40.0, 40.0)))));
    // The triangle above the diagonal x + y = 80 turns onto the one above
    // y = x: the wedge left of both is cleared, the rest stays red.
    let px = px1(&board.snapshot(), 200, 100);
    assert!(is_red(at(&px, 200, 55, 55)), "outside the lasso stays");
    assert!(is_red(at(&px, 200, 40, 55)), "outside the lasso stays");
    assert!(is_red(at(&px, 200, 50, 25)), "the turned triangle");
    assert_eq!(at(&px, 200, 25, 40), WHITE, "lifted and not put back");
    // Refusals.
    assert!(matches!(refused(&board, |t| t.add_move(&paint, Selection::Lasso(vec![Point::new(0.0, 0.0), Point::new(1.0, 1.0)]), None)), BoardError::Invalid(_)));
    assert!(matches!(refused(&board, |t| t.add_move(&paint, sel.clone(), Some(PixelTransform::scale(0.0, 1.0, Point::default())))), BoardError::Invalid(_)));
}

#[test]
fn pixel_transforms_compose_and_invert() {
    let c = Point::new(10.0, 20.0);
    let t = PixelTransform::rotate(90.0, c).then(&PixelTransform::translate(5.0, 0.0)).then(&PixelTransform::scale(2.0, 2.0, Point::default()));
    let p = t.apply(Point::new(11.0, 20.0));
    // (11, 20) turned about (10, 20) clockwise is (10, 21); then +5 x; then doubled.
    assert!((p.x - 30.0).abs() < 1e-4 && (p.y - 42.0).abs() < 1e-4, "{p:?}");
    let back = t.inverse().unwrap().apply(p);
    assert!((back.x - 11.0).abs() < 1e-3 && (back.y - 20.0).abs() < 1e-3);
    assert!(PixelTransform::scale(0.0, 1.0, c).inverse().is_none());
    let r = PixelTransform::rotate(45.0, Point::default()).map_rect(&Rect::new(0.0, 0.0, 10.0, 10.0));
    assert!((r.w - 14.142).abs() < 0.01);
    let sel = Selection::Rect(Rect::new(0.0, 0.0, 4.0, 2.0));
    assert_eq!(sel.bounds(), Some(Rect::new(0.0, 0.0, 4.0, 2.0)));
    assert_eq!(sel.outline_through(&PixelTransform::translate(1.0, 1.0))[2], Point::new(5.0, 3.0));
}

/// The pictures a raster becomes, under `src`.
fn decoded(raster: &LayerRaster, src: &str) -> HashMap<String, Arc<Picture>> {
    let png = raster.png.as_ref().expect("a picture");
    let pic = Picture::decode(png).unwrap();
    assert_eq!((pic.width(), pic.height()), raster.pixel_size);
    [(src.to_string(), Arc::new(pic))].into()
}

#[test]
fn flatten_replaces_a_history_with_one_picture_in_one_undo() {
    let board = Board::standalone().with_origin("me");
    board.create(st()).unwrap();
    let paint = ok(&board, |t| t.paint_layer_for(None));
    let mut manager = board.undo_manager(yrs::undo::Options::default()).unwrap();
    manager.reset();
    ok(&board, |t| t.add_stroke(hard(&paint, INK, 6.0).with_points(ring(60.0, 60.0, 30.0))));
    let clip = Rect::new(0.0, 0.0, 120.0, 120.0);
    ok(&board, |t| t.add_fill(NewFill::new(&paint, Point::new(60.0, 60.0), RED, clip)));
    ok(&board, |t| t.add_stroke(NewStroke::new(&paint, &brush::AIRBRUSH, Color::rgb(30, 160, 30)).with_size(30.0).with_points(line(10.0, 110.0, 20.0, 8))));
    let view = View::new(0.0, 0.0, 2.0);
    let snap = board.snapshot();
    let before = pixels(&snap, &NoPictures, view, 240, 240);
    let raster = rasterize(&snap, &NoPictures, RasterJob::Flatten(paint.clone())).unwrap();
    assert_eq!(raster.ops.len(), 3);
    let src = "pimble-blob:s/flat";
    let pictures = decoded(&raster, src);
    manager.reset();
    ok(&board, |t| t.apply_raster(&raster, Some(src)));
    let snap = board.snapshot();
    let ops = snap.paint_ops(&paint);
    assert_eq!(ops.len(), 1);
    assert!(matches!(&ops[0].raw.body, PaintBody::Pixels(p) if p.src == src && p.rect == raster.rect));
    let after = pixels(&snap, &pictures, view, 240, 240);
    // Straight-alpha PNG and back rounds faint pixels by a few levels.
    assert!(max_diff(&before, &after) <= 6, "the same picture: {}", max_diff(&before, &after));
    // One undo brings the history back.
    board.undo(&mut manager, false).expect("an undo");
    assert_eq!(board.snapshot().paint_ops(&paint).len(), 3);
    // A picture it needs not here: refused.
    ok(&board, |t| t.add_pixels(&paint, "pimble-blob:s/elsewhere", Rect::new(0.0, 0.0, 4.0, 4.0)));
    assert!(matches!(rasterize(&board.snapshot(), &NoPictures, RasterJob::Flatten(paint.clone())), Err(BoardError::PicturesNotLoaded(1))));
}

#[test]
fn flatten_keeps_what_was_painted_while_its_picture_was_stored() {
    let (board, _, paint) = painted();
    ok(&board, |t| t.add_stroke(hard(&paint, INK, 6.0).with_points(line(10.0, 90.0, 30.0, 6))));
    let raster = rasterize(&board.snapshot(), &NoPictures, RasterJob::Flatten(paint.clone())).unwrap();
    let late = ok(&board, |t| t.add_stroke(hard(&paint, RED, 6.0).with_points(line(10.0, 90.0, 60.0, 6))));
    let id = ok(&board, |t| t.apply_raster(&raster, Some("pimble-blob:s/f"))).unwrap();
    let ids: Vec<String> = board.snapshot().paint_ops(&paint).iter().map(|o| o.raw.id.clone()).collect();
    assert_eq!(ids, vec![id, late], "the picture under the stroke painted since");
}

#[test]
fn a_vector_layer_merges_down_onto_a_paint_layer_as_pixels() {
    let (board, l1, paint) = painted();
    square(&board, &paint, Rect::new(0.0, 0.0, 60.0, 100.0), Color::rgb(250, 230, 40));
    let top = ok(&board, |t| t.add_layer("Shapes", Placement::Top));
    let rect = NewElement::new(Kind::Rect, Geom::Rect(Rect::new(30.0, 20.0, 60.0, 40.0)))
        .with_style(Style { fill: Some(Color::rgb(40, 120, 220)), width: Some(0.0), ..Style::default() })
        .in_layer(top.clone());
    ok(&board, |t| t.add_element(rect));
    ok(&board, |t| t.set_layer_opacity(&top, 0.6));
    ok(&board, |t| t.set_layer_blend(&top, Blend::Multiply));
    let snap = board.snapshot();
    assert!(merge_needs_pixels(&snap, &top) && !merge_needs_pixels(&snap, &paint));
    assert!(matches!(refused(&board, |t| t.merge_layer_down(&top)), BoardError::NeedsPixels(_)));
    let view = View::new(0.0, 0.0, 2.0);
    let before = pixels(&snap, &NoPictures, view, 200, 200);
    let raster = rasterize(&snap, &NoPictures, RasterJob::MergeDown(top.clone())).unwrap();
    assert_eq!((raster.onto.as_deref(), raster.blend), (Some(paint.as_str()), Blend::Multiply));
    let src = "pimble-blob:s/merged";
    let pictures = decoded(&raster, src);
    ok(&board, |t| t.apply_raster(&raster, Some(src)));
    let snap = board.snapshot();
    assert!(snap.layer(&top).is_none(), "the merged layer goes");
    assert!(snap.elements.is_empty(), "with its elements");
    let ops = snap.paint_ops(&paint);
    assert!(matches!(&ops.last().unwrap().raw.body, PaintBody::Pixels(p) if p.blend == Blend::Multiply));
    let after = pixels(&snap, &pictures, view, 200, 200);
    assert!(max_diff(&before, &after) <= 3, "{}", max_diff(&before, &after));
    // The other way round is refused with a sentence.
    let _ = l1;
    let (board2, _, paint2) = painted();
    let err = rasterize(&board2.snapshot(), &NoPictures, RasterJob::MergeDown(paint2.clone())).unwrap_err();
    assert_eq!(err.to_string(), PAINT_ONTO_VECTOR);
    assert_eq!(refused(&board2, |t| t.merge_layer_down(&paint2)).to_string(), PAINT_ONTO_VECTOR);
    assert!(!merge_needs_pixels(&board2.snapshot(), &paint2));
}

#[test]
fn merging_refuses_hidden_layers_and_a_changed_layer_below() {
    let (board, _, paint) = painted();
    let top = ok(&board, |t| t.add_layer("Shapes", Placement::Top));
    ok(&board, |t| t.set_layer_visible(&top, false));
    assert!(matches!(rasterize(&board.snapshot(), &NoPictures, RasterJob::MergeDown(top.clone())), Err(BoardError::Invalid(_))));
    ok(&board, |t| t.set_layer_visible(&top, true));
    // Drawn for one layer below, then another paint layer slipped between.
    let raster = rasterize(&board.snapshot(), &NoPictures, RasterJob::MergeDown(top.clone())).unwrap();
    assert!(raster.png.is_none(), "an empty layer draws nothing");
    ok(&board, |t| t.add_layer_in("Paint 9", LayerKind::Paint, None, Placement::Below(top.clone())));
    assert!(matches!(refused(&board, |t| t.apply_raster(&raster, None)), BoardError::Invalid(_)));
    let _ = paint;
}

#[test]
fn a_group_with_a_paint_layer_merges_into_one_paint_layer() {
    let (board, _, _) = painted();
    let group = ok(&board, |t| t.add_layer_in("Group", LayerKind::Group, None, Placement::Top));
    let p = ok(&board, |t| t.add_layer_in("Paint A", LayerKind::Paint, Some(&group), Placement::Top));
    let v = ok(&board, |t| t.add_layer_in("Vector A", LayerKind::Vector, Some(&group), Placement::Top));
    square(&board, &p, Rect::new(10.0, 10.0, 50.0, 50.0), RED);
    let dot = NewElement::new(Kind::Ellipse, Geom::Rect(Rect::new(40.0, 40.0, 40.0, 40.0)))
        .with_style(Style { fill: Some(Color::rgb(30, 160, 30)), width: Some(0.0), ..Style::default() })
        .in_layer(v.clone());
    ok(&board, |t| t.add_element(dot));
    ok(&board, |t| t.set_layer_opacity(&v, 0.5));
    ok(&board, |t| t.set_layer_opacity(&group, 0.8));
    let snap = board.snapshot();
    assert!(merge_needs_pixels(&snap, &group));
    assert!(matches!(refused(&board, |t| t.merge_group(&group)), BoardError::NeedsPixels(_)));
    let view = View::new(0.0, 0.0, 2.0);
    let before = pixels(&snap, &NoPictures, view, 200, 200);
    let raster = rasterize(&snap, &NoPictures, RasterJob::MergeGroup(group.clone())).unwrap();
    let src = "pimble-blob:s/group";
    let pictures = decoded(&raster, src);
    ok(&board, |t| t.apply_raster(&raster, Some(src)));
    let snap = board.snapshot();
    let merged = snap.layer(&group).unwrap();
    assert_eq!((merged.kind.clone(), merged.opacity, merged.name.as_str()), (LayerKind::Paint, 0.8, "Group"));
    assert!(snap.layer(&p).is_none() && snap.layer(&v).is_none() && snap.elements.is_empty());
    assert_eq!(snap.paint_ops(&group).len(), 1);
    let after = pixels(&snap, &pictures, view, 200, 200);
    assert!(max_diff(&before, &after) <= 3, "{}", max_diff(&before, &after));
}

#[test]
fn an_operation_missing_what_it_needs_is_skipped() {
    let (board, _, paint) = painted();
    ok(&board, |t| t.add_stroke(hard(&paint, INK, 6.0).with_points(line(10.0, 90.0, 50.0, 10))));
    {
        use yrs::{Map, MapPrelim, Transact};
        let mut txn = board.doc().transact_mut();
        let map = board.board_map(&txn).unwrap();
        // A fill missing its seed reads as a kind this version cannot draw.
        let op: yrs::MapRef = map.insert(&mut txn, "paint:broken", MapPrelim::default());
        op.insert(&mut txn, "layer", paint.clone());
        op.insert(&mut txn, "order", "zz");
        op.insert(&mut txn, "kind", "fill");
    }
    let snap = board.snapshot();
    assert!(matches!(snap.paint_op("broken").unwrap().raw.body, PaintBody::Other(ref k) if k == "fill"));
    assert!(px1(&snap, 100, 100)[(50 * 100 + 50) * 4 + 2] > 150, "the rest draws");
}
