//! The drawing contract's loose ends (docs/DRAWING_CONTRACT.md, "Loose ends
//! closed"): Outline Stroke, and the smudge that starts off the paint.

use pimble_board::combine::area;
use pimble_board::*;

const T0: &str = "2026-11-17T10:00:00.000Z";

fn st() -> Stamp<'static> {
    Stamp::new(None, T0)
}

fn board() -> Board {
    let board = Board::standalone();
    board.create(st()).unwrap();
    board
}

fn add(board: &Board, el: NewElement) -> String {
    board.add_element(st(), el).unwrap().0
}

fn close(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

fn pixel(p: &tiny_skia::Pixmap, x: u32, y: u32) -> (u8, u8, u8, u8) {
    let c = p.pixel(x, y).unwrap().demultiply();
    (c.red(), c.green(), c.blue(), c.alpha())
}

fn draw(board: &Board, w: u32, h: u32) -> tiny_skia::Pixmap {
    render(&board.snapshot(), &NoPictures, &View::new(0.0, 0.0, 1.0), w, h, None).unwrap()
}

const GREEN: Color = Color::rgb(0, 160, 0);

#[test]
fn a_line_becomes_the_filled_outline_of_its_stroke_in_one_undo() {
    let b = board().with_origin("ann");
    let line = add(
        &b,
        NewElement::new(Kind::Line, Geom::Segment { a: Point::new(0.0, 50.0), b: Point::new(200.0, 50.0) })
            .with_style(Style { stroke: Some(GREEN), width: Some(10.0), cap: Some(Cap::Butt), arrow_heads: Some(ArrowHeads::None), ..Style::default() }),
    );
    let before = draw(&b, 220, 100);
    let mut manager = b.undo_manager(yrs::undo::Options::default()).unwrap();
    let (out, _) = b.edit(st(), |t| t.outline_stroke(&[line.clone()])).into_result().unwrap();
    assert_eq!(out, vec![line.clone()], "a line with nothing else to show becomes its outline");
    let snap = b.snapshot();
    assert_eq!(snap.elements.len(), 1);
    let e = snap.get(&line).unwrap();
    assert_eq!(e.raw.kind, Kind::Path);
    assert_eq!(e.raw.style.fill, Some(GREEN), "filled with the stroke's color");
    assert_eq!(e.style.width, 0.0, "and no stroke");
    assert_eq!(e.raw.style.cap, None, "the stroke's detail goes");
    let Geom::Path(p) = &e.raw.geom else { panic!("a path") };
    assert!(close(area(p), 2000.0, 2.0), "{}", area(p));
    let world = e.bounds;
    assert!(close(world.x, 0.0, 0.1) && close(world.y, 45.0, 0.1) && close(world.w, 200.0, 0.1) && close(world.h, 10.0, 0.1), "{world:?}");
    // It looks the same.
    let after = draw(&b, 220, 100);
    assert_eq!(pixel(&after, 100, 50), pixel(&before, 100, 50));
    assert_eq!(pixel(&after, 100, 46), pixel(&before, 100, 46));
    assert_eq!(pixel(&after, 100, 60).3, 0);
    assert!(b.undo(&mut manager, false).is_some());
    assert_eq!(b.snapshot().get(&line).unwrap().raw.kind, Kind::Line, "one undo puts the line back");
}

#[test]
fn a_filled_shape_keeps_its_fill_and_gets_its_outline_above_it() {
    let b = board().with_origin("ann");
    let red = Color::rgb(220, 0, 0);
    let rect = add(
        &b,
        NewElement::new(Kind::Rect, Geom::Rect(Rect::new(20.0, 20.0, 100.0, 60.0)))
            .with_style(Style { stroke: Some(GREEN), fill: Some(red), width: Some(8.0), join: Some(Join::Miter), ..Style::default() })
            .with_transform(Transform { rot: 30.0, ..Transform::IDENTITY }),
    );
    let before = draw(&b, 160, 120);
    let (out, _) = b.edit(st(), |t| t.outline_stroke(&[rect.clone()])).into_result().unwrap();
    let snap = b.snapshot();
    assert_eq!(snap.elements.len(), 2);
    let outline = snap.get(&out[0]).unwrap();
    let kept = snap.get(&rect).unwrap();
    assert_eq!(kept.raw.kind, Kind::Rect);
    assert_eq!((kept.style.fill, kept.style.width), (Some(red), 0.0), "the fill stays, its stroke goes");
    assert_eq!(outline.raw.kind, Kind::Path);
    let at = |id: &str| snap.elements.iter().position(|e| e.raw.id == id).unwrap();
    assert_eq!(at(&out[0]), at(&rect) + 1, "right above it");
    assert_eq!(outline.raw.transform.rot, 30.0, "turned as the stroke was");
    let after = draw(&b, 160, 120);
    let (cx, cy) = (70, 50);
    assert_eq!(pixel(&after, cx, cy), pixel(&before, cx, cy), "the fill");
    // Pixels differing at all are along anti-aliased edges only.
    let differing = (0..160).flat_map(|x| (0..120).map(move |y| (x, y))).filter(|&(x, y)| {
        let (a, c) = (pixel(&after, x, y), pixel(&before, x, y));
        (a.0 as i32 - c.0 as i32).abs() > 40 || (a.1 as i32 - c.1 as i32).abs() > 40 || (a.3 as i32 - c.3 as i32).abs() > 40
    });
    let differing = differing.count();
    assert!(differing < 40, "it looks the same: {differing} pixels differ");
}

#[test]
fn dashes_outline_as_separate_pieces_and_refusals_are_sentences() {
    let b = board();
    let line = add(
        &b,
        NewElement::new(Kind::Line, Geom::Segment { a: Point::new(0.0, 0.0), b: Point::new(100.0, 0.0) })
            .with_style(Style { width: Some(4.0), dash_pattern: Some(vec![10.0, 10.0]), cap: Some(Cap::Butt), arrow_heads: Some(ArrowHeads::None), ..Style::default() }),
    );
    let (out, _) = b.edit(st(), |t| t.outline_stroke(&[line.clone()])).into_result().unwrap();
    let snap = b.snapshot();
    let Geom::Path(p) = &snap.get(&out[0]).unwrap().raw.geom else { panic!("a path") };
    assert_eq!(p.subpaths.len(), 5, "five dashes");
    assert!(close(area(p), 200.0, 1.0), "{}", area(p));

    let text = add(&b, NewElement::new(Kind::Text, Geom::Rect(Rect::new(0.0, 50.0, 80.0, 20.0))).with_text("hi"));
    let err = b.edit(st(), |t| t.outline_stroke(&[text.clone()])).into_result().unwrap_err().to_string();
    assert!(err.contains("Only shapes, lines without arrow heads, pen strokes and paths have a stroke to outline: a text does not."), "{err}");
    let bare = add(&b, NewElement::new(Kind::Rect, Geom::Rect(Rect::new(0.0, 100.0, 50.0, 50.0))).with_style(Style { width: Some(0.0), fill: Some(GREEN), ..Style::default() }));
    let err = b.edit(st(), |t| t.outline_stroke(&[bare])).into_result().unwrap_err().to_string();
    assert!(err.contains("has no stroke to outline"), "{err}");
}

#[test]
fn quadratic_and_closing_segments_read_back_as_anchors() {
    let mut pb = tiny_skia::PathBuilder::new();
    pb.move_to(0.0, 0.0);
    pb.quad_to(50.0, 50.0, 100.0, 0.0);
    pb.line_to(0.0, 0.0);
    pb.close();
    let g = PathGeom::from_tiny(&pb.finish().unwrap());
    assert_eq!(g.subpaths.len(), 1);
    assert!(g.subpaths[0].closed);
    assert_eq!(g.subpaths[0].anchors.len(), 2, "the closing point is not doubled");
    let a = &g.subpaths[0].anchors[0];
    assert!(close(a.h_out.x, 100.0 / 3.0, 1e-3) && close(a.h_out.y, 100.0 / 3.0, 1e-3), "{a:?}");
}
