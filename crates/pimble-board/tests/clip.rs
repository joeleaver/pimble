//! Clip masks (docs/DRAWING_WAVE6_CONTRACT.md section 1): a layer or group
//! clipped to a closed element of the same canvas, the refusals, what reads
//! as no clip, nesting, rendering of vector and paint, the raster cache,
//! SVG export, hit testing, and two people editing at once.

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

fn refused<R: std::fmt::Debug>(board: &Board, f: impl FnOnce(&mut BoardTxn<'_, '_>) -> doc::Result<R>) -> BoardError {
    let edited = board.edit(st(), f);
    assert!(edited.update.is_none(), "a refused write writes nothing");
    edited.result.unwrap_err()
}

fn filled(kind: Kind, r: Rect, color: Color) -> NewElement {
    NewElement::new(kind, Geom::Rect(r)).with_style(Style { fill: Some(color), stroke: Some(color), width: Some(0.0), ..Style::default() })
}

fn pixels(snap: &BoardSnapshot, w: u32, h: u32) -> Vec<u8> {
    render(snap, &NoPictures, &View::new(0.0, 0.0, 1.0), w, h, Some(Color::WHITE)).unwrap().take()
}

fn at(px: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [px[i], px[i + 1], px[i + 2], px[i + 3]]
}

const WHITE: [u8; 4] = [255, 255, 255, 255];

fn is_red(p: [u8; 4]) -> bool {
    p[0] > 180 && p[1] < 80 && p[2] < 80
}

fn is_ink(p: [u8; 4]) -> bool {
    p[2] > 150 && p[0] < 120
}

/// A canvas 100 x 100: "Layer 1" with a red square over all of it, a hidden
/// "Clips" layer below it holding an ellipse over x 20..60, y 20..60.
/// Answers (board, layer 1, clips layer, the ellipse).
fn clipped_setup() -> (Board, String, String, String) {
    let board = Board::standalone();
    let l1 = board.create(st()).unwrap().0;
    let (clips, ellipse) = ok(&board, |t| {
        t.add_element(filled(Kind::Rect, Rect::new(0.0, 0.0, 100.0, 100.0), RED).in_layer(&l1))?;
        let clips = t.add_layer("Clips", Placement::Bottom)?;
        let ellipse = t.add_element(filled(Kind::Ellipse, Rect::new(20.0, 20.0, 40.0, 40.0), Color::BLACK).in_layer(&clips))?;
        t.set_layer_visible(&clips, false)?;
        Ok((clips, ellipse))
    });
    (board, l1, clips, ellipse)
}

#[test]
fn a_clipped_layer_draws_only_inside_its_clip() {
    let (board, l1, _clips, ellipse) = clipped_setup();
    assert!(is_red(at(&pixels(&board.snapshot(), 100, 100), 100, 5, 5)), "unclipped first");
    ok(&board, |t| t.set_layer_clip(&l1, Some(&ellipse)));
    {
        use yrs::{Map, Transact};
        let txn = board.doc().transact();
        let map = board.board_map(&txn).unwrap();
        let Some(yrs::Out::YMap(layers)) = map.get(&txn, "layers") else { panic!("layers") };
        let Some(yrs::Out::YMap(layer)) = layers.get(&txn, l1.as_str()) else { panic!("layer") };
        assert_eq!(layer.get(&txn, "clip").map(|v| v.to_string(&txn)), Some(ellipse.clone()), "one key on the layer's map");
    }
    let snap = board.snapshot();
    let layer = snap.layer(&l1).unwrap();
    assert_eq!((layer.clip.as_deref(), layer.clip_named.as_deref(), layer.isolated), (Some(ellipse.as_str()), Some(ellipse.as_str()), true));
    let px = pixels(&snap, 100, 100);
    assert!(is_red(at(&px, 100, 40, 40)), "inside the ellipse: {:?}", at(&px, 100, 40, 40));
    assert_eq!(at(&px, 100, 5, 5), WHITE, "outside it nothing");
    assert_eq!(at(&px, 100, 22, 22), WHITE, "the ellipse's corner, outside its outline");
    assert_eq!(at(&px, 100, 80, 40), WHITE);
    // A thumbnail draws what the layer holds, unclipped.
    let ids: std::collections::HashSet<String> = snap.elements.iter().filter(|e| e.layer == l1).map(|e| e.raw.id.clone()).collect();
    let thumb = render_area_unclipped(&snap, &NoPictures, Rect::new(0.0, 0.0, 100.0, 100.0), 1.0, Some(&ids)).unwrap();
    assert!(is_red(at(&thumb.image.data, thumb.image.width, 5, 5)), "unclipped");
    // The hidden clip draws nothing of its own; the visible extent is the clip's.
    let extent = snap.visible_extent().unwrap();
    assert!(extent.x >= 19.0 && extent.right() <= 61.0, "{extent:?}");
    // Released: whole again.
    ok(&board, |t| t.set_layer_clip(&l1, None));
    let snap = board.snapshot();
    assert_eq!(snap.layer(&l1).unwrap().clip, None);
    assert!(!snap.layer(&l1).unwrap().isolated);
    assert!(is_red(at(&pixels(&snap, 100, 100), 100, 5, 5)));
}

#[test]
fn a_visible_clip_draws_where_it_is() {
    let (board, l1, clips, ellipse) = clipped_setup();
    ok(&board, |t| {
        t.set_layer_visible(&clips, true)?;
        t.set_layer_clip(&l1, Some(&ellipse))
    });
    let px = pixels(&board.snapshot(), 100, 100);
    // The red is clipped to the ellipse and drawn over it.
    assert!(is_red(at(&px, 100, 40, 40)));
    assert_eq!(at(&px, 100, 5, 5), WHITE);
}

#[test]
fn moving_or_reshaping_the_clip_moves_the_edge() {
    let (board, l1, _clips, ellipse) = clipped_setup();
    ok(&board, |t| t.set_layer_clip(&l1, Some(&ellipse)));
    let e = board.snapshot().get(board.snapshot().elements.iter().find(|e| e.layer == l1).unwrap().id()).unwrap().fingerprint;
    let red_id = board.snapshot().elements.iter().find(|e| e.layer == l1).unwrap().id().to_string();
    ok(&board, |t| t.set_transform(&ellipse, &Transform { tx: 40.0, ..Transform::IDENTITY }));
    let snap = board.snapshot();
    assert_ne!(snap.get(&red_id).unwrap().fingerprint, e, "the clipped element redraws when its clip moves");
    let px = pixels(&snap, 100, 100);
    assert_eq!(at(&px, 100, 30, 40), WHITE, "where the clip was");
    assert!(is_red(at(&px, 100, 80, 40)), "where it is now");
    // Reshaped: a rectangle over the whole width, a strip.
    ok(&board, |t| {
        t.set_transform(&ellipse, &Transform::IDENTITY)?;
        t.set_geom(&ellipse, &Geom::Rect(Rect::new(0.0, 40.0, 100.0, 10.0)))
    });
    let px = pixels(&board.snapshot(), 100, 100);
    assert!(is_red(at(&px, 100, 5, 45)));
    assert_eq!(at(&px, 100, 5, 30), WHITE);
}

#[test]
fn paint_is_kept_whole_and_clipped_when_it_is_composited() {
    let (board, _l1, _clips, ellipse) = clipped_setup();
    let paint = ok(&board, |t| t.add_layer_in("Paint", LayerKind::Paint, None, Placement::Top));
    let line: Vec<BrushPoint> = (0..20).map(|i| [5.0 + 90.0 * i as f32 / 19.0, 40.0, 1.0, 0.0]).collect();
    ok(&board, |t| t.add_stroke(NewStroke::new(paint.clone(), &brush::ROUND, INK).with_size(10.0).with_points(line)));
    ok(&board, |t| t.set_layer_clip(&paint, Some(&ellipse)));
    let px = pixels(&board.snapshot(), 100, 100);
    assert!(is_ink(at(&px, 100, 40, 40)), "inside: {:?}", at(&px, 100, 40, 40));
    assert!(!is_ink(at(&px, 100, 10, 40)), "outside the clip the paint does not show");
    assert!(!is_ink(at(&px, 100, 85, 40)));
    // The operation is untouched, so moving the clip shows paint that was hidden.
    assert_eq!(board.snapshot().paint_ops(&paint).len(), 1);
    ok(&board, |t| t.set_transform(&ellipse, &Transform { tx: 35.0, ..Transform::IDENTITY }));
    let px = pixels(&board.snapshot(), 100, 100);
    assert!(is_ink(at(&px, 100, 85, 40)), "moved: {:?}", at(&px, 100, 85, 40));
    assert!(!is_ink(at(&px, 100, 30, 40)));
}

#[test]
fn a_clipped_layer_in_a_clipped_group_shows_where_both_reach() {
    let board = Board::standalone();
    let l1 = board.create(st()).unwrap().0;
    let (g, inner, left, top) = ok(&board, |t| {
        let clips = t.add_layer("Clips", Placement::Top)?;
        let left = t.add_element(filled(Kind::Rect, Rect::new(0.0, 0.0, 60.0, 100.0), Color::BLACK).in_layer(&clips))?;
        let top = t.add_element(filled(Kind::Rect, Rect::new(0.0, 0.0, 100.0, 60.0), Color::BLACK).in_layer(&clips))?;
        t.set_layer_visible(&clips, false)?;
        let g = t.add_group("G", None, Placement::Top)?;
        let inner = t.add_layer_in("Inner", LayerKind::Vector, Some(&g), Placement::Top)?;
        t.add_element(filled(Kind::Rect, Rect::new(0.0, 0.0, 100.0, 100.0), RED).in_layer(&inner))?;
        Ok((g, inner, left, top))
    });
    let _ = l1;
    ok(&board, |t| {
        t.set_layer_clip(&g, Some(&left))?;
        t.set_layer_clip(&inner, Some(&top))
    });
    let snap = board.snapshot();
    assert!(snap.layer(&g).unwrap().isolated, "a clipped group gets a buffer");
    let px = pixels(&snap, 100, 100);
    assert!(is_red(at(&px, 100, 30, 30)), "inside both");
    assert_eq!(at(&px, 100, 80, 30), WHITE, "outside the group's");
    assert_eq!(at(&px, 100, 30, 80), WHITE, "outside the layer's");
    assert_eq!(at(&px, 100, 80, 80), WHITE);
    // Hit testing follows the clips.
    let red = snap.elements.iter().find(|e| e.layer == inner).unwrap();
    assert!(red.hits(Point::new(30.0, 30.0), 0.5));
    assert!(!red.hits(Point::new(80.0, 30.0), 0.5));
    assert!(!red.hits(Point::new(30.0, 80.0), 0.5));
    assert_eq!(snap.hit_precise(Point::new(80.0, 80.0), 0.5).map(|e| e.id().to_string()), None);
    assert_eq!(snap.hit_precise(Point::new(30.0, 30.0), 0.5).map(|e| e.id().to_string()), Some(red.id().to_string()));
}

#[test]
fn what_cannot_clip_is_refused_with_a_sentence() {
    let (board, l1, clips, ellipse) = clipped_setup();
    let (line, open, closed, inside) = ok(&board, |t| {
        let line = t.add_element(NewElement::new(Kind::Line, Geom::Segment { a: Point::new(0.0, 0.0), b: Point::new(10.0, 10.0) }).in_layer(&clips))?;
        let open = t.add_element(
            NewElement::new(Kind::Path, Geom::Path(PathGeom::polyline(&[Point::new(0.0, 0.0), Point::new(10.0, 0.0), Point::new(10.0, 10.0)], false)))
                .in_layer(&clips),
        )?;
        let closed = t.add_element(
            NewElement::new(Kind::Path, Geom::Path(PathGeom::polyline(&[Point::new(0.0, 0.0), Point::new(10.0, 0.0), Point::new(10.0, 10.0)], true)))
                .in_layer(&clips),
        )?;
        let inside = t.add_element(filled(Kind::Star, Rect::new(0.0, 0.0, 10.0, 10.0), Color::BLACK).in_layer(&l1))?;
        Ok((line, open, closed, inside))
    });
    let sentence = |e: BoardError| e.to_string();
    assert_eq!(sentence(refused(&board, |t| t.set_layer_clip(&l1, Some(&line)))), clip::NOT_CLOSED);
    assert_eq!(sentence(refused(&board, |t| t.set_layer_clip(&l1, Some(&open)))), clip::NOT_CLOSED);
    assert_eq!(sentence(refused(&board, |t| t.set_layer_clip(&l1, Some(&inside)))), clip::CLIP_CYCLE);
    assert!(sentence(refused(&board, |t| t.set_layer_clip(&l1, Some("nothing")))).contains("no element nothing"));
    ok(&board, |t| t.set_layer_clip(&l1, Some(&closed)));
    // A group clipped to an element of a layer inside it is a cycle too.
    let g = ok(&board, |t| t.group_layers(&[l1.clone()], "G"));
    assert_eq!(sentence(refused(&board, |t| t.set_layer_clip(&g, Some(&inside)))), clip::CLIP_CYCLE);
    // Locked: refused, naming the lock.
    ok(&board, |t| t.set_layer_locked(&g, true));
    let e = refused(&board, |t| t.set_layer_clip(&l1, Some(&ellipse)));
    assert!(matches!(e, BoardError::LayerLocked(ref l) if l.id == g), "{e:?}");
    assert!(e.is_lock());
}

#[test]
fn a_clip_that_cannot_apply_reads_as_none_everywhere() {
    let (board, l1, _clips, ellipse) = clipped_setup();
    ok(&board, |t| t.set_layer_clip(&l1, Some(&ellipse)));
    // The clip element deleted: unclipped (and undo brings it back).
    ok(&board, |t| t.delete_element(&ellipse));
    let snap = board.snapshot();
    let layer = snap.layer(&l1).unwrap();
    assert_eq!((layer.clip.as_deref(), layer.clip_named.as_deref()), (None, Some(ellipse.as_str())));
    assert!(!layer.isolated);
    assert!(is_red(at(&pixels(&snap, 100, 100), 100, 5, 5)));
    // Someone moved the clip element into the layer it clips (written
    // directly, as a concurrent move can leave it): a cycle, no clip.
    let (board, l1, _clips, ellipse) = clipped_setup();
    ok(&board, |t| t.set_layer_clip(&l1, Some(&ellipse)));
    ok(&board, |t| t.move_to_layer(&[ellipse.clone()], &l1));
    let snap = board.snapshot();
    assert_eq!(snap.layer(&l1).unwrap().clip, None);
    assert!(is_red(at(&pixels(&snap, 100, 100), 100, 5, 5)));
}

#[test]
fn the_svg_export_clips_with_a_clip_path() {
    let (board, l1, _clips, ellipse) = clipped_setup();
    ok(&board, |t| t.set_layer_clip(&l1, Some(&ellipse)));
    let svg = render_svg(&board.snapshot(), Rect::new(0.0, 0.0, 100.0, 100.0), None, &|_| None);
    assert!(svg.contains("<clipPath id=\"pimble-clip-1\" clipPathUnits=\"userSpaceOnUse\"><path d=\"M"), "{svg}");
    assert!(svg.contains("<g clip-path=\"url(#pimble-clip-1)\">"), "{svg}");
}

#[test]
fn the_raster_cache_redraws_when_the_clip_moves() {
    let (board, l1, _clips, ellipse) = clipped_setup();
    ok(&board, |t| t.set_layer_clip(&l1, Some(&ellipse)));
    let bg = Color::rgb(250, 250, 250);
    let mut view = BoardView::new(100, 100, bg);
    view.set_camera(Point::new(50.0, 50.0), 1.0);
    view.set_snapshot(board.snapshot());
    view.frame(&NoPictures);
    let w = board.watch().unwrap();
    w.take();
    ok(&board, |t| t.set_transform(&ellipse, &Transform { tx: 40.0, ..Transform::IDENTITY }));
    view.update(&board, &w.take());
    let frame = view.frame(&NoPictures).expect("redrawn").to_vec();
    let full = render(view.snapshot(), &NoPictures, &view.view(), 100, 100, Some(bg)).unwrap().take();
    let differing = frame.chunks_exact(4).zip(full.chunks_exact(4)).filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > 2)).count();
    assert_eq!(differing, 0);
    assert!(is_red(at(&frame, 100, 80, 40)));
    assert!(!is_red(at(&frame, 100, 30, 40)));
}

#[test]
fn a_flatten_keeps_the_clip_a_merge_takes_it_and_a_copy_keeps_the_key() {
    let (board, l1, _clips, ellipse) = clipped_setup();
    let below = ok(&board, |t| {
        let below = t.add_layer_in("Below", LayerKind::Paint, None, Placement::Below(l1.clone()))?;
        t.set_layer_clip(&l1, Some(&ellipse))?;
        Ok(below)
    });
    // Merge Down onto the paint layer below: only what showed, the ellipse.
    let raster = rasterize(&board.snapshot(), &NoPictures, RasterJob::MergeDown(l1.clone())).unwrap();
    let r = raster.rect().unwrap();
    assert!(r.x >= 19.0 && r.y >= 19.0 && r.right() <= 61.0 && r.bottom() <= 61.0, "{r:?}");
    // A flatten keeps the layer, so its clip stays a key: the pixels are whole.
    let paint = ok(&board, |t| t.add_layer_in("Paint", LayerKind::Paint, None, Placement::Top));
    let line: Vec<BrushPoint> = (0..20).map(|i| [5.0 + 90.0 * i as f32 / 19.0, 40.0, 1.0, 0.0]).collect();
    ok(&board, |t| {
        t.add_stroke(NewStroke::new(paint.clone(), &brush::ROUND, INK).with_size(10.0).with_points(line))?;
        t.set_layer_clip(&paint, Some(&ellipse))
    });
    let raster = rasterize(&board.snapshot(), &NoPictures, RasterJob::Flatten(paint.clone())).unwrap();
    let r = raster.rect().unwrap();
    assert!(r.x < 10.0 && r.right() > 90.0, "{r:?}");
    let _ = below;
    let copy = Board::standalone();
    copy.edit(st(), |t| t.copy_from(&board.read().unwrap())).into_result().unwrap();
    assert_eq!(copy.snapshot().layer(&l1).unwrap().clip.as_deref(), Some(ellipse.as_str()));
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
fn two_people_clipping_at_once_read_the_same_canvas() {
    let mut a = node();
    let (l1, clips, star, square) = {
        let board = Board::of_node(&a);
        let l1 = board.create(st()).unwrap().0;
        let (clips, star, square) = ok(&board, |t| {
            t.add_element(filled(Kind::Rect, Rect::new(0.0, 0.0, 100.0, 100.0), RED).in_layer(&l1))?;
            let clips = t.add_layer("Clips", Placement::Top)?;
            let star = t.add_element(filled(Kind::Star, Rect::new(0.0, 0.0, 50.0, 50.0), Color::BLACK).in_layer(&clips))?;
            let square = t.add_element(filled(Kind::Rect, Rect::new(50.0, 50.0, 50.0, 50.0), Color::BLACK).in_layer(&clips))?;
            Ok((clips, star, square))
        });
        (l1, clips, star, square)
    };
    let mut b = node();
    sync(&mut a, &mut b);
    // One clips Layer 1 to the star, the other to the square; the other
    // moves the star into Layer 1 at the same time.
    ok(&Board::of_node(&a), |t| t.set_layer_clip(&l1, Some(&star)));
    ok(&Board::of_node(&b), |t| {
        t.set_layer_clip(&l1, Some(&square))?;
        t.move_to_layer(&[star.clone()], &l1)
    });
    sync(&mut a, &mut b);
    let (sa, sb) = (Board::of_node(&a).snapshot(), Board::of_node(&b).snapshot());
    let (la, lb) = (sa.layer(&l1).unwrap(), sb.layer(&l1).unwrap());
    assert_eq!(la.clip_named, lb.clip_named, "one value wins on both");
    assert_eq!(la.clip, lb.clip);
    assert_eq!(pixels(&sa, 100, 100), pixels(&sb, 100, 100), "the same picture on both devices");
    let _ = clips;
}
