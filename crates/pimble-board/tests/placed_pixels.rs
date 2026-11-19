//! Drawing wave 6, section 5 (docs/DRAWING_WAVE6_CONTRACT.md): pictures
//! placed in a paint layer, as `pixels` operations with a transform; large
//! pictures in pieces; the clip and Flatten over them; two devices.

use std::collections::HashMap;
use std::sync::Arc;

use pimble_board::*;
use pimble_crdt::NodeDoc;

const T: &str = "2026-11-18T10:00:00.000Z";
const SRC: &str = "pimble-blob:00000000-0000-0000-0000-000000000001/aaaaaaaaaaaaaaaaaaaaaaaaaa";

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

/// A picture `w` × `h`: its left half red, its right half blue.
fn halves(w: u32, h: u32) -> Picture {
    let rgba: Vec<u8> = (0..w * h).flat_map(|i| if i % w < w / 2 { [220, 30, 30, 255] } else { [30, 40, 220, 255] }).collect();
    Picture::from_rgba(w, h, rgba).unwrap()
}

fn pictures(src: &str, picture: Picture) -> HashMap<String, Arc<Picture>> {
    HashMap::from([(src.to_string(), Arc::new(picture))])
}

/// The color at board point (`x`, `y`) of a 1:1 render of 0..w, 0..h over white.
fn color(snap: &BoardSnapshot, pics: &dyn PictureSource, x: f32, y: f32) -> Color {
    sample_color(snap, pics, Point::new(x, y), None)
}

fn is_red(c: Color) -> bool {
    c.r > 180 && c.g < 90 && c.b < 90
}

fn is_blue(c: Color) -> bool {
    c.b > 180 && c.r < 90
}

/// A board with "Layer 1" and a paint layer above it.
fn painted() -> (Board, String) {
    let board = Board::standalone();
    board.create(st()).unwrap();
    let paint = ok(&board, |t| t.paint_layer_for(None));
    (board, paint)
}

#[test]
fn a_placed_picture_is_stored_with_its_transform_and_drawn_through_it() {
    let (board, paint) = painted();
    let rect = Rect::new(0.0, 0.0, 200.0, 100.0);
    // A quarter turn clockwise about the picture's center: its left (red)
    // half now on top, its right (blue) half below.
    let turn = PixelTransform::rotate(90.0, rect.center());
    let ids = ok(&board, |t| t.add_placed_pixels(&paint, &[(SRC.to_string(), rect)], Some(turn)));
    assert_eq!(ids.len(), 1);
    let snap = board.snapshot();
    let ops = snap.paint_ops(&paint);
    let PaintBody::Pixels(p) = &ops[0].raw.body else { panic!("a pixels op") };
    assert_eq!(p.rect, rect);
    let t = p.transform.expect("its transform is stored");
    assert!(t.to_array().iter().zip(turn.to_array()).all(|(a, b)| (a - b).abs() < 1e-5));
    let b = ops[0].bounds.unwrap();
    assert!((b.w - 100.0).abs() < 0.01 && (b.h - 200.0).abs() < 0.01, "its bounds are where it shows: {b:?}");

    let pics = pictures(SRC, halves(200, 100));
    assert!(is_red(color(&snap, &pics, 100.0, 0.0)), "the red half is on top: {:?}", color(&snap, &pics, 100.0, 0.0));
    assert!(is_blue(color(&snap, &pics, 100.0, 100.0)), "the blue half below: {:?}", color(&snap, &pics, 100.0, 100.0));
    assert!(color(&snap, &pics, 20.0, 50.0).a < 10, "nothing where it was before the turn");
    // No transform (or the identity): it lies as its rectangle says, and
    // nothing is stored for one.
    let (board2, paint2) = painted();
    ok(&board2, |t| t.add_placed_pixels(&paint2, &[(SRC.to_string(), rect)], Some(PixelTransform::IDENTITY)));
    let snap2 = board2.snapshot();
    assert!(matches!(&snap2.paint_ops(&paint2)[0].raw.body, PaintBody::Pixels(p) if p.transform.is_none()));
    assert!(is_red(color(&snap2, &pics, 20.0, 50.0)) && is_blue(color(&snap2, &pics, 180.0, 50.0)));
}

#[test]
fn what_cannot_be_placed_is_refused_and_writes_nothing() {
    let (board, paint) = painted();
    let r = Rect::new(0.0, 0.0, 10.0, 10.0);
    let flat = PixelTransform { a: 1.0, b: 0.0, c: 1.0, d: 0.0, e: 0.0, f: 0.0 };
    let e = refused(&board, |t| t.add_placed_pixels(&paint, &[(SRC.to_string(), r)], Some(flat)));
    assert_eq!(e.to_string(), paint_ops::PIXELS_FLAT);
    refused(&board, |t| t.add_placed_pixels(&paint, &[], None));
    refused(&board, |t| t.add_placed_pixels(&paint, &[(SRC.to_string(), Rect::new(0.0, 0.0, 0.0, 10.0))], None));
    // A vector layer takes no pixels; a locked paint layer neither.
    let vector = board.snapshot().layers[0].id.clone();
    refused(&board, |t| t.add_placed_pixels(&vector, &[(SRC.to_string(), r)], None));
    ok(&board, |t| t.set_layer_locked(&paint, true));
    assert!(refused(&board, |t| t.add_placed_pixels(&paint, &[(SRC.to_string(), r)], None)).is_lock());
    // A refused piece among good ones writes none of them.
    ok(&board, |t| t.set_layer_locked(&paint, false));
    refused(&board, |t| t.add_placed_pixels(&paint, &[(SRC.to_string(), r), (String::new(), r)], None));
    assert!(board.snapshot().paint_ops(&paint).is_empty());
}

#[test]
fn a_large_picture_is_cut_into_pieces_that_meet_without_a_seam() {
    // 2,500 × 300 pixels: two pieces across (2,048 and 452), one down.
    assert!(needs_pieces(2500, 300) && !needs_pieces(2048, 2048));
    let big = halves(2500, 300);
    let rect = Rect::new(10.0, 20.0, 1250.0, 150.0);
    let pieces = picture_pieces(&big, rect).unwrap();
    assert_eq!(pieces.len(), 2);
    assert_eq!(pieces[0].pixel_size, (2048, 300));
    assert_eq!(pieces[1].pixel_size, (452, 300));
    assert_eq!(pieces[0].rect.right(), pieces[1].rect.x, "the pieces meet exactly");
    assert_eq!((pieces[0].rect.x, pieces[1].rect.right(), pieces[0].rect.h), (10.0, 1260.0, 150.0));
    assert!(pieces.iter().all(|p| p.bytes.len() < 20 * 1024 * 1024));
    // Opaque pieces are JPEG; one with transparency PNG.
    assert_eq!(&pieces[0].bytes[..2], &[0xff, 0xd8]);
    let clear = Picture::from_rgba(2100, 10, vec![0u8; 2100 * 10 * 4]).unwrap();
    assert_eq!(&picture_pieces(&clear, rect).unwrap()[0].bytes[1..4], b"PNG");

    // Laid down in a turn: every piece through the one transform, no seam
    // where they meet.
    let (board, paint) = painted();
    let srcs = ["pimble-blob:00000000-0000-0000-0000-000000000001/bbbbbbbbbbbbbbbbbbbbbbbbbb", "pimble-blob:00000000-0000-0000-0000-000000000001/cccccccccccccccccccccccccc"];
    let turn = PixelTransform::rotate(30.0, rect.center());
    let laid: Vec<(String, Rect)> = srcs.iter().zip(&pieces).map(|(s, p)| (s.to_string(), p.rect)).collect();
    ok(&board, |t| t.add_placed_pixels(&paint, &laid, Some(turn)));
    let pics: HashMap<String, Arc<Picture>> = srcs.iter().zip(&pieces).map(|(s, p)| (s.to_string(), Arc::new(Picture::decode(&p.bytes).unwrap()))).collect();
    let snap = board.snapshot();
    // Along the seam (x = 10 + 1024 in the picture's own frame), turned.
    let seam_x = pieces[0].rect.right();
    for k in [-40.0, -10.0, 0.0, 10.0, 40.0] {
        let p = turn.apply(Point::new(seam_x, rect.center().y + k));
        let c = color(&snap, &pics, p.x, p.y);
        assert!(is_blue(c) && c.a > 245, "no seam at {p:?}: {c:?}");
    }
    let red = turn.apply(Point::new(rect.x + 100.0, rect.center().y));
    assert!(is_red(color(&snap, &pics, red.x, red.y)));
}

#[test]
fn a_clipped_paint_layer_clips_a_placed_picture_and_flatten_folds_it_in() {
    let (board, paint) = painted();
    let rect = Rect::new(0.0, 0.0, 200.0, 100.0);
    ok(&board, |t| t.add_placed_pixels(&paint, &[(SRC.to_string(), rect)], Some(PixelTransform::translate(10.0, 0.0))));
    // A stroke painted over it, in the same layer.
    let line: Vec<BrushPoint> = (0..20).map(|i| [20.0 + 160.0 * i as f32 / 19.0, 50.0, 1.0, 0.0]).collect();
    ok(&board, |t| t.add_stroke(NewStroke::new(paint.clone(), &brush::ROUND, Color::rgb(20, 200, 40)).with_size(12.0).with_points(line)));
    let pics = pictures(SRC, halves(200, 100));
    let snap = board.snapshot();
    assert!(color(&snap, &pics, 60.0, 50.0).g > 150, "the stroke is above the picture");
    assert!(is_red(color(&snap, &pics, 60.0, 20.0)));
    assert!(color(&snap, &pics, 5.0, 20.0).a < 10, "moved 10 right");

    // Clipped to a rectangle over its left part.
    let clips = ok(&board, |t| t.add_layer("Clips", Placement::Bottom));
    let clip = ok(&board, |t| t.add_element(NewElement::new(Kind::Rect, Geom::Rect(Rect::new(0.0, 0.0, 100.0, 100.0))).in_layer(&clips)));
    ok(&board, |t| t.set_layer_visible(&clips, false));
    ok(&board, |t| t.set_layer_clip(&paint, Some(&clip)));
    let snap = board.snapshot();
    assert!(is_red(color(&snap, &pics, 60.0, 20.0)), "inside the clip");
    assert!(color(&snap, &pics, 160.0, 20.0).a < 10, "outside the clip the picture does not show");

    // Flatten folds the picture and the stroke into pixels that look the same.
    ok(&board, |t| t.set_layer_clip(&paint, None));
    let snap = board.snapshot();
    let raster = rasterize(&snap, &pics, RasterJob::Flatten(paint.clone())).unwrap();
    let mut flat = pics.clone();
    let mut srcs = Vec::new();
    for (i, tile) in raster.tiles.iter().enumerate() {
        let src = format!("pimble-blob:00000000-0000-0000-0000-000000000001/{}", char::from(b'd' + i as u8).to_string().repeat(26));
        flat.insert(src.clone(), Arc::new(Picture::decode(&tile.png).unwrap()));
        srcs.push(src);
    }
    ok(&board, |t| t.apply_raster(&raster, &srcs));
    let after = board.snapshot();
    assert!(after.paint_ops(&paint).iter().all(|op| matches!(&op.raw.body, PaintBody::Pixels(p) if p.transform.is_none())));
    for (x, y) in [(60.0, 20.0), (160.0, 80.0), (60.0, 50.0)] {
        let (a, b) = (color(&snap, &pics, x, y), color(&after, &flat, x, y));
        assert!(a.r.abs_diff(b.r) <= 6 && a.g.abs_diff(b.g) <= 6 && a.b.abs_diff(b.b) <= 6, "looks the same at {x},{y}: {a:?} {b:?}");
    }
}

fn node() -> NodeDoc {
    let mut doc = NodeDoc::new();
    doc.init("board", "Map", None, T).unwrap();
    doc
}

#[test]
fn two_devices_read_the_same_placed_picture() {
    let a = node();
    let board = Board::of_node(&a);
    board.create(st()).unwrap();
    let paint = ok(&board, |t| t.paint_layer_for(None));
    let rect = Rect::new(-50.0, -25.0, 100.0, 50.0);
    let t = PixelTransform::rotate(45.0, rect.center()).then(&PixelTransform::translate(5.0, 7.0));
    ok(&board, |tx| tx.add_placed_pixels(&paint, &[(SRC.to_string(), rect)], Some(t)));
    let mut b = NodeDoc::new();
    b.apply_update(&a.diff_since(&b.state_vector()).unwrap()).unwrap();
    let (sa, sb) = (Board::of_node(&a).snapshot(), Board::of_node(&b).snapshot());
    let (pa, pb) = (&sa.paint_ops(&paint)[0], &sb.paint_ops(&paint)[0]);
    assert_eq!(pa.raw.body, pb.raw.body);
    assert_eq!(pa.effective, pb.effective, "the same fingerprint, so the same tiles");
    let pics = pictures(SRC, halves(100, 50));
    let p = t.apply(Point::new(-25.0, 0.0));
    assert_eq!(color(&sa, &pics, p.x, p.y), color(&sb, &pics, p.x, p.y));
    assert!(is_red(color(&sb, &pics, p.x, p.y)));
}
