//! Pen pressure (contract section 3: points are x, y, pressure): a pen's
//! width follows the pressure of each point; a stroke at one pressure (a
//! mouse's 0.5) draws exactly as strokes always have, and the live stamps
//! agree with the element's own rendering.

use pimble_board::tiny_skia;
use pimble_board::*;

const T0: &str = "2026-10-09T10:00:00.000Z";
const BG: Color = Color::rgb(250, 250, 250);
const INK: Color = Color::rgb(20, 20, 160);

fn style(width: f32) -> Style {
    Style { stroke: Some(INK), width: Some(width), ..Style::default() }
}

fn board_with(kind: Kind, points: Vec<[f32; 3]>, width: f32) -> Board {
    let board = Board::standalone();
    let st = Stamp::new(None, T0);
    board.create(st).unwrap();
    board.add_element(st, NewElement::stroke(kind, points).with_style(style(width))).unwrap();
    board
}

fn rendered(board: &Board) -> Vec<u8> {
    render(&board.snapshot(), &NoPictures, &View::new(-100.0, -100.0, 1.0), 200, 200, Some(BG)).unwrap().take()
}

/// Pixels that differ by more than a rounding step.
fn differing(a: &[u8], b: &[u8]) -> usize {
    a.chunks_exact(4).zip(b.chunks_exact(4)).filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > 2)).count()
}

fn inked(px: &[u8], x: usize, y: usize) -> bool {
    let i = (y * 200 + x) * 4;
    px[i] < 150 && px[i + 2] > 100
}

/// How many inked pixels column `x` has.
fn thickness(px: &[u8], x: usize) -> usize {
    (0..200).filter(|y| inked(px, x, *y)).count()
}

fn wave(pressure: impl Fn(usize) -> f32) -> Vec<[f32; 3]> {
    (0..=16).map(|i| [-80.0 + i as f32 * 10.0, (i as f32 * 0.6).sin() * 30.0, pressure(i)]).collect()
}

#[test]
fn a_stroke_at_one_pressure_draws_as_strokes_always_have() {
    let points = wave(|_| 0.5);
    let got = rendered(&board_with(Kind::Pen, points.clone(), 6.0));
    // The polyline stroked at the style's width, with round caps and joins:
    // what a pen drew before pressure was drawn.
    let mut want = tiny_skia::Pixmap::new(200, 200).unwrap();
    want.fill(tiny_skia::Color::from_rgba8(BG.r, BG.g, BG.b, 255));
    let mut pb = tiny_skia::PathBuilder::new();
    pb.move_to(points[0][0], points[0][1]);
    for p in &points[1..] {
        pb.line_to(p[0], p[1]);
    }
    let path = pb.finish().unwrap();
    let mut paint = tiny_skia::Paint::default();
    paint.set_color(tiny_skia::Color::from_rgba8(INK.r, INK.g, INK.b, 255));
    paint.anti_alias = true;
    let stroke = tiny_skia::Stroke {
        width: 6.0,
        line_cap: tiny_skia::LineCap::Round,
        line_join: tiny_skia::LineJoin::Round,
        ..Default::default()
    };
    want.stroke_path(&path, &paint, &stroke, tiny_skia::Transform::from_translate(100.0, 100.0), None);
    assert_eq!(differing(&got, want.data()), 0);
    // And as SVG it is still one stroked path at the style's width.
    let board = board_with(Kind::Pen, points, 6.0);
    let svg = render_svg(&board.snapshot(), Rect::new(-100.0, -100.0, 200.0, 200.0), None, &|_: &str| None);
    assert!(svg.contains("stroke-width=\"6\""), "{svg}");
}

#[test]
fn pressure_widens_a_pen_where_it_was_pressed_harder() {
    // Light at the start, hard at the end, along a straight line.
    let points: Vec<[f32; 3]> = (0..=16).map(|i| [-80.0 + i as f32 * 10.0, 0.0, 0.05 + i as f32 * 0.058]).collect();
    let board = board_with(Kind::Pen, points.clone(), 8.0);
    let px = rendered(&board);
    let (light, hard) = (thickness(&px, 30), thickness(&px, 170));
    assert!(light >= 1 && hard > light * 3, "light {light}, hard {hard}");
    assert!((hard as f32 - 8.0 * pressure_width(0.92)).abs() <= 2.0, "hard {hard}");
    // Not what the same points at one pressure draw.
    let flat: Vec<[f32; 3]> = points.iter().map(|p| [p[0], p[1], 0.5]).collect();
    assert!(differing(&px, &rendered(&board_with(Kind::Pen, flat, 8.0))) > 200);
    // Its bounds hold all of it, and a press on its thick end hits it.
    let snap = board.snapshot();
    let e = &snap.elements[0];
    assert!(e.bounds.h >= 8.0 * pressure_width(0.92) - 0.5, "{:?}", e.bounds);
    assert!(e.hits(Point::new(70.0, 6.0), 0.5));
    assert!(!e.hits(Point::new(-70.0, 6.0), 0.5));
    // As SVG it is a filled outline, not a stroke.
    let svg = render_svg(&snap, Rect::new(-100.0, -100.0, 200.0, 200.0), None, &|_: &str| None);
    assert!(!svg.contains("stroke-width"), "{svg}");
}

#[test]
fn a_varying_stroke_has_no_holes_where_it_turns_back() {
    // A tight zigzag whose pressure changes at every point: every point on
    // the line is ink (the parts' union, nothing cancelled out).
    let points: Vec<[f32; 3]> =
        (0..=12).map(|i| [-60.0 + i as f32 * 10.0, if i % 2 == 0 { -30.0 } else { 30.0 }, if i % 2 == 0 { 0.2 } else { 0.9 }]).collect();
    let px = rendered(&board_with(Kind::Pen, points.clone(), 6.0));
    for pair in points.windows(2) {
        for t in 0..=10 {
            let t = t as f32 / 10.0;
            let x = pair[0][0] + (pair[1][0] - pair[0][0]) * t + 100.0;
            let y = pair[0][1] + (pair[1][1] - pair[0][1]) * t + 100.0;
            assert!(inked(&px, x.round() as usize, y.round() as usize), "a hole at {x}, {y}");
        }
    }
}

#[test]
fn a_highlighter_keeps_one_width_whatever_the_pressure() {
    let varying = rendered(&board_with(Kind::Highlighter, wave(|i| 0.1 + i as f32 * 0.05), 12.0));
    let flat = rendered(&board_with(Kind::Highlighter, wave(|_| 0.5), 12.0));
    assert_eq!(differing(&varying, &flat), 0);
}

#[test]
fn live_stamps_of_a_varying_stroke_match_its_rendering() {
    let points = wave(|i| 0.15 + (i % 5) as f32 * 0.18);
    let empty = Board::standalone();
    empty.create(Stamp::new(None, T0)).unwrap();
    let mut view = BoardView::new(200, 200, BG);
    view.set_snapshot(empty.snapshot());
    view.set_camera(Point::new(0.0, 0.0), 1.0);
    view.frame(&NoPictures);
    view.begin_stroke(Kind::Pen, &style(6.0), points[0]);
    for p in &points[1..] {
        view.extend_stroke(&[*p]);
        view.frame(&NoPictures);
    }
    let live = view.frame(&NoPictures).map(|f| f.to_vec()).unwrap_or_default();
    let settled = render(&board_with(Kind::Pen, points, 6.0).snapshot(), &NoPictures, &view.view(), 200, 200, Some(BG)).unwrap().take();
    let d = differing(&live, &settled);
    // Stamped a segment at a time, anti-aliased edges where the parts
    // overlap blend twice: a few pixels, within what a re-rendered rectangle
    // is allowed (tests/view.rs).
    assert!(d < 40, "{d} pixels differ from the settled rendering");
}
