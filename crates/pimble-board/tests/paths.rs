//! Drawing wave 2's model (docs/DRAWING_CONTRACT.md section 2): the `path`
//! element, live shapes, stroke detail, fitting, SVG path data, and the
//! edits the vector tools make.

use pimble_board::path::{self, fit, parse_d, to_d};
use pimble_board::*;
use pimble_crdt::NodeDoc;

const T0: &str = "2026-11-16T10:00:00.000Z";

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

fn dist(a: Point, b: Point) -> f32 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

/// The largest distance from any of `points` to the fitted curve.
fn worst_point_to_curve(p: &PathGeom, points: &[Point]) -> f32 {
    points.iter().map(|q| p.distance_to(*q)).fold(0.0, f32::max)
}

/// Samples along every segment of `p`.
fn samples(p: &PathGeom, per_segment: usize) -> Vec<Point> {
    p.segments().flat_map(|(_, _, c)| (0..=per_segment).map(move |i| path::cubic_at(&c, i as f32 / per_segment as f32))).collect()
}

fn dist_to_polyline(q: Point, pts: &[Point]) -> f32 {
    pts.windows(2)
        .map(|w| {
            let (a, b) = (w[0], w[1]);
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let l2 = dx * dx + dy * dy;
            let t = if l2 > 0.0 { (((q.x - a.x) * dx + (q.y - a.y) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
            dist(q, Point::new(a.x + t * dx, a.y + t * dy))
        })
        .fold(f32::INFINITY, f32::min)
}

fn alpha(pixmap: &tiny_skia::Pixmap, x: u32, y: u32) -> u8 {
    pixmap.pixel(x, y).map_or(0, |p| p.alpha())
}

fn draw(board: &Board, w: u32, h: u32) -> tiny_skia::Pixmap {
    render(&board.snapshot(), &NoPictures, &View::new(0.0, 0.0, 1.0), w, h, None).unwrap()
}

// ── Fitting ─────────────────────────────────────────────────────────────────

#[test]
fn a_fitted_circle_stays_within_tolerance() {
    let pts: Vec<Point> = (0..=240)
        .map(|i| {
            let a = i as f32 / 240.0 * std::f32::consts::TAU;
            Point::new(200.0 + 100.0 * a.cos(), 200.0 + 100.0 * a.sin())
        })
        .collect();
    for tol in [0.5f32, 1.0, 3.0] {
        let p = fit(&pts, tol);
        assert_eq!(p.subpaths.len(), 1);
        assert!(!p.subpaths[0].closed, "a loop drawn closed stays open");
        assert!(worst_point_to_curve(&p, &pts) <= tol * 1.01, "tolerance {tol}: {}", worst_point_to_curve(&p, &pts));
        // And the curve stays near the circle between the points.
        for q in samples(&p, 20) {
            let r = dist(q, Point::new(200.0, 200.0));
            assert!((r - 100.0).abs() <= tol * 1.5, "tolerance {tol}: a curve point is {r} from the center");
        }
        assert!(p.subpaths[0].anchors.len() <= 16, "tolerance {tol}: {} anchors", p.subpaths[0].anchors.len());
        // Joins inside a smooth curve are smooth.
        let inner = &p.subpaths[0].anchors[1..p.subpaths[0].anchors.len() - 1];
        assert!(inner.iter().all(|a| a.mode != AnchorMode::Corner), "{inner:?}");
    }
}

#[test]
fn a_fitted_zigzag_keeps_its_corners_within_tolerance() {
    let corners: Vec<Point> = (0..=6).map(|i| Point::new(i as f32 * 50.0, if i % 2 == 0 { 0.0 } else { 60.0 })).collect();
    let mut pts = Vec::new();
    for w in corners.windows(2) {
        for k in 0..25 {
            let t = k as f32 / 25.0;
            pts.push(Point::new(w[0].x + (w[1].x - w[0].x) * t, w[0].y + (w[1].y - w[0].y) * t));
        }
    }
    pts.push(*corners.last().unwrap());
    let tol = 1.0;
    let p = fit(&pts, tol);
    assert!(worst_point_to_curve(&p, &pts) <= tol * 1.01);
    for q in samples(&p, 20) {
        assert!(dist_to_polyline(q, &pts) <= tol * 1.5, "{q:?} strays from the zigzag");
    }
    let anchors = &p.subpaths[0].anchors;
    for c in &corners[1..corners.len() - 1] {
        let near = anchors.iter().find(|a| dist(a.p, *c) <= tol).unwrap_or_else(|| panic!("no anchor at the corner {c:?}"));
        assert_eq!(near.mode, AnchorMode::Corner);
    }
    assert!(anchors.len() <= 12, "{} anchors", anchors.len());
}

#[test]
fn a_straight_stroke_fits_to_a_line_of_two_anchors() {
    let pts: Vec<Point> = (0..=100).map(|i| Point::new(i as f32 * 3.0, i as f32 * 1.5 + 10.0)).collect();
    let p = fit(&pts, 1.0);
    assert_eq!(p.subpaths[0].anchors.len(), 2);
    assert!(p.subpaths[0].is_line(0), "a straight piece has no handles");
    assert!(worst_point_to_curve(&p, &pts) <= 1e-3);
    // One point is a dot; nothing is nothing.
    assert_eq!(fit(&[Point::new(5.0, 5.0)], 1.0).subpaths[0].anchors.len(), 2);
    assert!(fit(&[], 1.0).is_empty());
}

// ── SVG path data ───────────────────────────────────────────────────────────

fn same_shape(a: &PathGeom, b: &PathGeom, eps: f32) {
    assert_eq!(a.subpaths.len(), b.subpaths.len());
    for (sa, sb) in a.subpaths.iter().zip(&b.subpaths) {
        assert_eq!(sa.closed, sb.closed);
        assert_eq!(sa.anchors.len(), sb.anchors.len(), "{sa:?} vs {sb:?}");
        for (x, y) in sa.anchors.iter().zip(&sb.anchors) {
            assert!(dist(x.p, y.p) <= eps && dist(x.h_in, y.h_in) <= eps && dist(x.h_out, y.h_out) <= eps, "{x:?} vs {y:?}");
        }
    }
}

#[test]
fn path_data_round_trips_through_every_command() {
    let cases = [
        "M10 10 L50 10 L50 50 Z",
        "M0 0 H100 V50 H0 Z M20 10 h20 v20 h-20 z",
        "M0 0 C10 -20 40 -20 50 0 S90 20 100 0",
        "m10 10 c10 -20 40 -20 50 0 s40 20 50 0 l0 30 z",
        "M0 0 Q50 -50 100 0 T200 0",
        "M0 50 A50 50 0 0 1 100 50 A50 50 0 0 1 0 50 Z",
        "M0 0 a25 10 30 1 0 50 20",
        "M1e1 1.5.5-2L4,5",
    ];
    for d in cases {
        let p = parse_d(d).unwrap_or_else(|e| panic!("{d}: {e}"));
        let again = parse_d(&to_d(&p)).unwrap();
        same_shape(&p, &again, 2e-3);
        assert_eq!(to_d(&again), to_d(&p), "{d}");
    }
}

#[test]
fn path_data_reads_what_each_command_means() {
    // Relative moves and lines, horizontal and vertical.
    let p = parse_d("m10 20 l5 0 h5 v5 L0 0 z").unwrap();
    let pts: Vec<Point> = p.subpaths[0].anchors.iter().map(|a| a.p).collect();
    assert_eq!(pts, vec![Point::new(10.0, 20.0), Point::new(15.0, 20.0), Point::new(20.0, 20.0), Point::new(20.0, 25.0), Point::new(0.0, 0.0)]);
    assert!(p.subpaths[0].closed);
    // After Z, a relative m starts from the subpath's start.
    let p = parse_d("M10 10 L20 10 Z m5 5 l1 0").unwrap();
    assert_eq!(p.subpaths[1].anchors[0].p, Point::new(15.0, 15.0));
    // A move's extra pairs are lines.
    assert_eq!(parse_d("M0 0 10 0 10 10").unwrap().subpaths[0].anchors.len(), 3);
    // S reflects the last control; the joint is smooth.
    let p = parse_d("M0 0 C0 -10 40 -10 50 0 S100 10 100 0").unwrap();
    let joint = p.subpaths[0].anchors[1];
    assert_eq!(joint.h_out, Point::new(10.0, 10.0));
    assert_eq!(joint.mode, AnchorMode::Symmetric);
    // A quadratic is the same curve as a cubic.
    let p = parse_d("M0 0 Q50 100 100 0").unwrap();
    let mid = path::cubic_at(&p.subpaths[0].segment(0).unwrap(), 0.5);
    assert!(dist(mid, Point::new(50.0, 50.0)) < 1e-3, "{mid:?}");
    // A half circle arc stays on its circle.
    let p = parse_d("M0 50 A50 50 0 0 1 100 50").unwrap();
    for q in samples(&p, 16) {
        assert!((dist(q, Point::new(50.0, 50.0)) - 50.0).abs() < 0.05, "{q:?}");
    }
    assert!(p.bounds().unwrap().y < 1.0, "the sweep goes up (y down): {:?}", p.bounds());
    // A radius too small grows to reach.
    let p = parse_d("M0 0 A1 1 0 0 0 100 0").unwrap();
    assert!((p.bounds().unwrap().h - 50.0).abs() < 0.1);
    // Refusals.
    assert!(matches!(parse_d("L1 2"), Err(PathError::Syntax { .. })));
    assert!(matches!(parse_d("M0 0 L"), Err(PathError::Syntax { .. })));
    assert!(matches!(parse_d("M0 0 X5 5"), Err(PathError::Syntax { .. })));
    assert!(matches!(parse_d(""), Err(PathError::Empty)));
    assert!(matches!(parse_d("M5 5"), Err(PathError::Empty)));
}

// ── The element ─────────────────────────────────────────────────────────────

fn square_with_hole() -> PathGeom {
    let mut p = parse_d("M0 0 H100 V100 H0 Z M30 30 H70 V70 H30 Z").unwrap();
    p.rule = PathRule::EvenOdd;
    p
}

#[test]
fn a_path_round_trips_through_the_node_document_and_merges_last_writer_wins() {
    let mut a = NodeDoc::new();
    a.init("board", "Canvas", None, T0).unwrap();
    let board = Board::of_node(&a);
    board.create(st()).unwrap();
    let geom = parse_d("M0 0 C10 -20 40 -20 50 0 L50 40 Z").unwrap();
    let style = Style {
        fill: Some(Color::rgb(10, 20, 30)),
        cap: Some(Cap::Square),
        join: Some(Join::Miter),
        miter: Some(8.0),
        dash_pattern: Some(vec![6.0, 3.0, 1.0, 3.0]),
        ..Style::default()
    };
    let id = add(&board, NewElement::path(geom.clone()).with_style(style.clone()));
    let raw = board.read().unwrap();
    let el = raw.elements.iter().find(|e| e.id == id).unwrap();
    assert_eq!(el.kind, Kind::Path);
    assert_eq!(el.geom, Geom::Path(geom.clone()));
    assert_eq!(el.style, style);

    // Two peers each drag a point of the same path: one path wins whole.
    let mut b = NodeDoc::load(&a.save()).unwrap();
    let (ba, bb) = (Board::of_node(&a), Board::of_node(&b));
    let mut ga = geom.clone();
    ga.move_anchor(AnchorRef::new(0, 0), Point::new(-10.0, 0.0));
    let mut gb = geom.clone();
    gb.move_anchor(AnchorRef::new(0, 2), Point::new(60.0, 60.0));
    ba.edit(st(), |t| t.set_geom(&id, &Geom::Path(ga.clone()))).into_result().unwrap();
    bb.edit(st(), |t| t.set_geom(&id, &Geom::Path(gb.clone()))).into_result().unwrap();
    let (asv, bsv) = (a.state_vector(), b.state_vector());
    let to_b = a.diff_since(&bsv).unwrap();
    let to_a = b.diff_since(&asv).unwrap();
    b.apply_update(&to_b).unwrap();
    a.apply_update(&to_a).unwrap();
    let ra = ba.snapshot();
    let rb = bb.snapshot();
    let (ea, eb) = (&ra.get(&id).unwrap().raw.geom, &rb.get(&id).unwrap().raw.geom);
    assert_eq!(ea, eb);
    assert!(*ea == Geom::Path(ga) || *ea == Geom::Path(gb), "one person's path, never a mix");
}

#[test]
fn a_path_is_refused_with_other_geometry_and_old_elements_read_as_before() {
    let b = board();
    let err = b.add_element(st(), NewElement::new(Kind::Path, Geom::Rect(Rect::new(0.0, 0.0, 1.0, 1.0)))).unwrap_err();
    assert!(matches!(err, BoardError::Invalid(_)));
    let err = b.add_element(st(), NewElement::new(Kind::Rect, Geom::Path(square_with_hole()))).unwrap_err();
    assert!(matches!(err, BoardError::Invalid(_)));
    // An element with none of the new style keys looks as it always did.
    let id = add(&b, NewElement::new(Kind::Rect, Geom::Rect(Rect::new(0.0, 0.0, 10.0, 10.0))));
    let snap = b.snapshot();
    let s = &snap.get(&id).unwrap().style;
    assert_eq!((s.cap, s.join, s.radius, s.dash_pattern.clone()), (Cap::Round, Join::Round, 0.0, None));
    let dashed = Style { dash: Some(Dash::Dashed), ..Style::default() }.resolved(&Kind::Line);
    assert_eq!(dashed.cap, Cap::Butt, "the dashed preset keeps its butt caps");
    // A kind this version does not know (what an older app sees of a path)
    // is kept and never drawn.
    let other = add(&b, NewElement::new(Kind::Other("blob".into()), Geom::None));
    let snap = b.snapshot();
    assert!(!snap.get(&other).unwrap().visible);
}

#[test]
fn a_path_draws_with_its_fill_rule_and_hits_on_its_stroke_and_fill() {
    let b = board();
    let id = add(
        &b,
        NewElement::path(square_with_hole()).with_style(Style { fill: Some(Color::rgb(0, 0, 255)), width: Some(2.0), ..Style::default() }),
    );
    let snap = b.snapshot();
    let e = snap.get(&id).unwrap();
    assert_eq!(e.pivot_bounds, Rect::new(0.0, 0.0, 100.0, 100.0));
    assert!(e.bounds.contains(Point::new(-0.9, -0.9)));
    assert!(e.hits(Point::new(15.0, 50.0), 0.0), "in the fill");
    assert!(!e.hits(Point::new(50.0, 50.0), 0.0), "in the hole (even-odd)");
    assert!(e.hits(Point::new(50.0, 31.0), 0.5), "on the hole's edge");
    assert!(!e.hits(Point::new(110.0, 50.0), 2.0));
    assert_eq!(snap.hit_precise(Point::new(15.0, 50.0), 1.0).map(|e| e.id()), Some(id.as_str()));
    assert_eq!(snap.query(&Rect::new(90.0, 90.0, 5.0, 5.0)), vec![0], "the spatial index holds it");
    let px = draw(&b, 120, 120);
    assert!(alpha(&px, 15, 50) > 200);
    assert_eq!(alpha(&px, 50, 50), 0, "the hole is empty");
    // The same path under nonzero (both squares wound alike) has no hole.
    let mut nz = square_with_hole();
    nz.rule = PathRule::NonZero;
    b.edit(st(), |t| t.set_geom(&id, &Geom::Path(nz))).into_result().unwrap();
    assert!(alpha(&draw(&b, 120, 120), 50, 50) > 200);
    // An unfilled open path hits only near its line.
    let line = add(&b, NewElement::path(parse_d("M200 0 C250 -50 300 50 350 0").unwrap()));
    let snap = b.snapshot();
    let l = snap.get(&line).unwrap();
    assert!(l.hits(Point::new(200.0, 0.0), 0.5));
    assert!(l.hits(Point::new(275.0, 0.0), 0.5), "it passes through its middle");
    assert!(!l.hits(Point::new(275.0, 30.0), 0.5), "an unfilled curve is only its line");
}

#[test]
fn svg_export_writes_paths_with_their_stroke_detail() {
    let b = board();
    add(
        &b,
        NewElement::path(square_with_hole()).with_style(Style {
            fill: Some(Color::rgb(0, 0, 255)),
            cap: Some(Cap::Square),
            join: Some(Join::Miter),
            miter: Some(10.0),
            dash_pattern: Some(vec![5.0, 2.0, 1.0]),
            ..Style::default()
        }),
    );
    let svg = render_svg(&b.snapshot(), Rect::new(-10.0, -10.0, 130.0, 130.0), None, &|_| None);
    assert!(svg.contains("fill-rule=\"evenodd\""), "{svg}");
    assert!(svg.contains("stroke-linecap=\"square\""), "{svg}");
    assert!(svg.contains("stroke-linejoin=\"miter\""), "{svg}");
    assert!(svg.contains("stroke-miterlimit=\"10\""), "{svg}");
    assert!(svg.contains("stroke-dasharray=\"5 2 1 5 2 1\""), "an odd list repeats: {svg}");
    assert!(svg.contains("<path d=\"M0 0L100 0L100 100L0 100"), "{svg}");
}

// ── Live shapes ─────────────────────────────────────────────────────────────

#[test]
fn live_shapes_draw_hit_and_fill_their_box() {
    let b = board();
    let fill = Some(Color::rgb(200, 0, 0));
    let rounded = add(
        &b,
        NewElement::new(Kind::Rect, Geom::Rect(Rect::new(0.0, 0.0, 100.0, 100.0))).with_style(Style { fill, width: Some(0.0), radius: Some(30.0), ..Style::default() }),
    );
    let hex = add(&b, NewElement::new(Kind::Polygon, Geom::Rect(Rect::new(200.0, 0.0, 100.0, 80.0))).with_style(Style { fill, sides: Some(6), ..Style::default() }));
    let star = add(
        &b,
        NewElement::new(Kind::Star, Geom::Rect(Rect::new(400.0, 0.0, 100.0, 100.0))).with_style(Style { points: Some(5), inner: Some(0.4), ..Style::default() }),
    );
    let snap = b.snapshot();
    let px = draw(&b, 520, 120);
    assert_eq!(alpha(&px, 2, 2), 0, "a rounded corner is cut");
    assert!(alpha(&px, 50, 2) > 200 && alpha(&px, 50, 50) > 200);
    assert!(!snap.get(&rounded).unwrap().hits(Point::new(1.0, 1.0), 0.5));
    // A polygon and a star are stretched to their box.
    let h = snap.get(&hex).unwrap();
    assert_eq!(h.pivot_bounds, Rect::new(200.0, 0.0, 100.0, 80.0));
    assert!(h.hits(Point::new(250.0, 40.0), 0.0), "filled: inside hits");
    let polygon = path::polygon(&Rect::new(200.0, 0.0, 100.0, 80.0), 6);
    let pb = polygon.bounds().unwrap();
    assert!((pb.x - 200.0).abs() < 1e-3 && (pb.w - 100.0).abs() < 1e-3 && (pb.h - 80.0).abs() < 1e-3, "{pb:?}");
    assert_eq!(polygon.subpaths[0].anchors.len(), 6);
    let s = snap.get(&star).unwrap();
    assert!(!s.hits(Point::new(450.0, 50.0), 1.0), "an unfilled star's middle is not it");
    assert!(s.hits(Point::new(450.0, 0.0), 1.0), "its top tip is");
    assert_eq!(path::star(&Rect::new(0.0, 0.0, 10.0, 10.0), 5, 0.4).subpaths[0].anchors.len(), 10);
    assert!(alpha(&px, 450, 2) > 0);
    // Sides change on their own, keeping the geometry (style keys merge).
    b.edit(st(), |t| t.set_style(&hex, &Style { sides: Some(3), ..Style::default() })).into_result().unwrap();
    assert_eq!(b.snapshot().get(&hex).unwrap().style.sides, 3);
    b.edit(st(), |t| t.clear_style(&hex, StyleKey::Sides)).into_result().unwrap();
    assert_eq!(b.snapshot().get(&hex).unwrap().style.sides, DEFAULT_SIDES);
}

#[test]
fn shapes_lines_and_pen_strokes_turn_into_paths_in_place() {
    let b = board();
    let style = Style { fill: Some(Color::rgb(0, 160, 0)), width: Some(3.0), radius: Some(12.0), ..Style::default() };
    let rect = add(&b, NewElement::new(Kind::Rect, Geom::Rect(Rect::new(10.0, 10.0, 100.0, 60.0))).with_style(style.clone()));
    let ellipse = add(
        &b,
        NewElement::new(Kind::Ellipse, Geom::Rect(Rect::new(150.0, 10.0, 80.0, 40.0)))
            .with_transform(Transform { tx: 5.0, ty: 3.0, rot: 30.0, sx: 1.2, sy: 0.8 }),
    );
    let pen = add(&b, NewElement::stroke(Kind::Pen, (0..30).map(|i| [10.0 + i as f32 * 4.0, 120.0 + (i as f32 * 0.3).sin() * 15.0, 0.5]).collect()).with_transform(Transform { rot: 20.0, ..Transform::IDENTITY }));
    let arrow = add(&b, NewElement::new(Kind::Arrow, Geom::Segment { a: Point::new(0.0, 0.0), b: Point::new(10.0, 10.0) }));
    let before = draw(&b, 300, 200);
    let snap0 = b.snapshot();
    let (r0, e0, p0) = (snap0.get(&rect).unwrap(), snap0.get(&ellipse).unwrap(), snap0.get(&pen).unwrap());
    let (order0, layer0, bounds_e0, bounds_p0) = (r0.raw.order.clone(), r0.layer.clone(), e0.bounds, p0.bounds);

    for id in [&rect, &ellipse, &pen] {
        b.edit(st(), |t| t.convert_to_path(id)).into_result().unwrap();
    }
    let snap = b.snapshot();
    let r = snap.get(&rect).unwrap();
    assert_eq!(r.raw.kind, Kind::Path);
    assert_eq!((r.raw.order.clone(), r.layer.clone()), (order0, layer0));
    assert_eq!(r.raw.style, style, "the style is kept");
    let Geom::Path(rp) = &r.raw.geom else { panic!() };
    assert_eq!(rp.subpaths[0].anchors.len(), 8, "a rounded rectangle's corners are arcs");
    let e = snap.get(&ellipse).unwrap();
    assert!((e.bounds.x - bounds_e0.x).abs() < 0.5 && (e.bounds.w - bounds_e0.w).abs() < 0.5, "a turned ellipse stays where it was: {:?} vs {bounds_e0:?}", e.bounds);
    let p = snap.get(&pen).unwrap();
    assert_eq!(p.raw.kind, Kind::Path);
    assert!((p.bounds.x - bounds_p0.x).abs() < 3.0 && (p.bounds.y - bounds_p0.y).abs() < 3.0, "{:?} vs {bounds_p0:?}", p.bounds);
    // It draws as it did (the pen's round ends included).
    let after = draw(&b, 300, 200);
    let differ = before.pixels().iter().zip(after.pixels()).filter(|(x, y)| (x.alpha() as i32 - y.alpha() as i32).abs() > 96).count();
    assert!(differ < 60, "{differ} pixels changed");
    // An arrow has no path; a path converts to itself; a locked shape stays.
    let err = b.edit(st(), |t| t.convert_to_path(&arrow)).into_result().unwrap_err();
    assert!(matches!(err, BoardError::Invalid(_)));
    let same = b.edit(st(), |t| t.convert_to_path(&rect)).into_result().unwrap().0;
    assert_eq!(&same, rp);
    let star = add(&b, NewElement::new(Kind::Star, Geom::Rect(Rect::new(0.0, 0.0, 10.0, 10.0))));
    b.edit(st(), |t| t.set_locked(&star, true)).into_result().unwrap();
    assert!(b.edit(st(), |t| t.convert_to_path(&star)).into_result().unwrap_err().is_lock());
}

#[test]
fn a_direct_select_edit_converts_and_moves_a_point_in_one_write() {
    let b = board().with_origin("sam");
    let id = add(&b, NewElement::new(Kind::Rect, Geom::Rect(Rect::new(0.0, 0.0, 100.0, 50.0))));
    let mut manager = b.undo_manager(yrs::undo::Options::default()).unwrap();
    b.edit(st(), |t| {
        let mut p = t.convert_to_path(&id)?;
        p.move_anchor(AnchorRef::new(0, 2), Point::new(120.0, 70.0));
        t.set_geom(&id, &Geom::Path(p))
    })
    .into_result()
    .unwrap();
    let snap = b.snapshot();
    let Geom::Path(p) = &snap.get(&id).unwrap().raw.geom else { panic!("a path") };
    assert_eq!(p.subpaths[0].anchors[2].p, Point::new(120.0, 70.0));
    // One undo takes it all back: a rectangle again.
    assert!(b.undo(&mut manager, false).is_some());
    let snap = b.snapshot();
    let e = snap.get(&id).unwrap();
    assert_eq!((e.raw.kind.clone(), e.raw.geom.clone()), (Kind::Rect, Geom::Rect(Rect::new(0.0, 0.0, 100.0, 50.0))));
}

// ── The Pencil ──────────────────────────────────────────────────────────────

#[test]
fn a_pencil_stroke_streams_as_a_pen_and_finishes_as_a_fitted_path() {
    let b = board();
    let pts: Vec<[f32; 3]> = (0..=60)
        .map(|i| {
            let a = i as f32 / 60.0 * std::f32::consts::PI;
            [100.0 + 80.0 * a.cos(), 100.0 + 80.0 * a.sin(), 0.5]
        })
        .collect();
    let style = Style { stroke: Some(Color::rgb(1, 2, 3)), width: Some(5.0), ..Style::default() };
    let id = add(&b, NewElement::stroke(Kind::Pen, pts[..10].to_vec()).with_style(style.clone()));
    b.append_points(st(), &id, &pts[10..]).unwrap();
    let order = b.snapshot().get(&id).unwrap().raw.order.clone();
    let fitted = b.edit(st(), |t| t.stroke_to_path(&id, &pts, 0.5)).into_result().unwrap().0;
    let snap = b.snapshot();
    let e = snap.get(&id).unwrap();
    assert_eq!(e.raw.kind, Kind::Path);
    assert_eq!(e.raw.order, order);
    assert_eq!(e.raw.style, style);
    assert!(e.raw.points.is_empty(), "the points went with the stroke's array");
    assert_eq!(e.raw.geom, Geom::Path(fitted.clone()));
    assert!(fitted.subpaths[0].anchors.len() <= 8);
    let as_points: Vec<Point> = pts.iter().map(|p| Point::new(p[0], p[1])).collect();
    assert!(worst_point_to_curve(&fitted, &as_points) <= 0.51);
    // A stroke that is no longer a pen is refused.
    assert!(matches!(b.edit(st(), |t| t.stroke_to_path(&id, &pts, 0.5)).into_result(), Err(BoardError::NotAStroke(_))));
    // Released before anything streamed: written once, as a path.
    let quick = b.edit(st(), |t| t.add_fitted_path(&pts, 1.0, NewElement::stroke(Kind::Pen, Vec::new()).with_style(style.clone()))).into_result().unwrap().0;
    assert_eq!(b.snapshot().get(&quick).unwrap().raw.kind, Kind::Path);
}

// ── Editing helpers ─────────────────────────────────────────────────────────

#[test]
fn anchors_and_handles_move_by_their_mode() {
    let mut p = parse_d("M0 0 C0 -30 70 -30 100 0 C130 30 170 30 200 0").unwrap();
    let mid = AnchorRef::new(0, 1);
    assert_eq!(p.anchor(mid).unwrap().mode, AnchorMode::Symmetric);
    // Symmetric: the other handle mirrors.
    p.move_handle(HandleRef { anchor: mid, side: HandleSide::Out }, Point::new(100.0, 40.0), false);
    let a = *p.anchor(mid).unwrap();
    assert_eq!(a.h_out, Point::new(0.0, 40.0));
    assert_eq!(a.h_in, Point::new(0.0, -40.0));
    // Smooth: the other turns to stay opposite, keeping its length.
    p.set_mode(mid, AnchorMode::Smooth);
    p.anchor_mut(mid).unwrap().h_in = Point::new(-10.0, 0.0);
    p.move_handle(HandleRef { anchor: mid, side: HandleSide::Out }, Point::new(100.0, 25.0), false);
    let a = *p.anchor(mid).unwrap();
    assert!(dist(a.h_in, Point::new(0.0, -10.0)) < 1e-4, "{:?}", a.h_in);
    // Corner: alone. Alt-drag breaks a smooth anchor into a corner.
    p.move_handle(HandleRef { anchor: mid, side: HandleSide::In }, Point::new(90.0, 5.0), true);
    let a = *p.anchor(mid).unwrap();
    assert_eq!(a.mode, AnchorMode::Corner);
    assert_eq!(a.h_in, Point::new(-10.0, 5.0));
    assert!(dist(a.h_out, Point::new(0.0, 25.0)) < 1e-4);
    // Moving an anchor brings its handles.
    p.move_anchors(&[mid, mid], 5.0, 5.0);
    assert_eq!(p.anchor(mid).unwrap().p, Point::new(105.0, 5.0));
    assert_eq!(p.anchor(mid).unwrap().in_point(), Point::new(95.0, 10.0));
}

#[test]
fn corners_switch_to_smooth_with_sensible_handles_and_back() {
    let mut p = PathGeom::polyline(&[Point::new(0.0, 0.0), Point::new(60.0, 30.0), Point::new(120.0, 0.0)], false);
    let mid = AnchorRef::new(0, 1);
    assert_eq!(p.toggle_smooth(mid), Some(AnchorMode::Smooth));
    let a = *p.anchor(mid).unwrap();
    assert!(a.h_in != Point::default() && a.h_out != Point::default(), "handles made from nothing");
    let cross = a.h_in.x * a.h_out.y - a.h_in.y * a.h_out.x;
    assert!(cross.abs() < 1e-3 && a.h_in.x * a.h_out.x + a.h_in.y * a.h_out.y < 0.0, "collinear and opposite: {a:?}");
    assert!((a.h_out.y).abs() < 1e-4, "along the line between the neighbors");
    assert_eq!(p.toggle_smooth(mid), Some(AnchorMode::Corner));
    assert_eq!(p.anchor(mid).unwrap().h_in, Point::default());
}

#[test]
fn hitting_anchors_handles_and_segments_names_them() {
    let p = parse_d("M0 0 C0 -30 70 -30 100 0 L100 100 M300 0 L400 0").unwrap();
    assert_eq!(p.hit_anchor(Point::new(99.0, 1.0), 3.0), Some(AnchorRef::new(0, 1)));
    assert_eq!(p.hit_anchor(Point::new(50.0, 50.0), 3.0), None);
    assert_eq!(p.hit_handle(Point::new(70.0, -29.0), 3.0, None), Some(HandleRef { anchor: AnchorRef::new(0, 1), side: HandleSide::In }));
    assert_eq!(p.hit_handle(Point::new(70.0, -29.0), 3.0, Some(&[AnchorRef::new(0, 0)])), None, "only the named anchors' handles");
    let hit = p.hit_segment(Point::new(350.0, 1.0), 2.0).unwrap();
    assert_eq!((hit.subpath, hit.segment), (1, 0));
    assert!((hit.t - 0.5).abs() < 1e-3 && hit.distance <= 1.0 + 1e-3);
    let hit = p.hit_segment(Point::new(100.5, 50.0), 2.0).unwrap();
    assert_eq!((hit.subpath, hit.segment), (0, 1));
    assert!(p.hit_segment(Point::new(200.0, 50.0), 2.0).is_none());
}

#[test]
fn deleting_anchors_drops_short_subpaths_and_says_when_nothing_is_left() {
    let mut p = parse_d("M0 0 L10 0 L20 0 M100 0 L110 0").unwrap();
    assert!(!p.delete_anchors(&[AnchorRef::new(1, 0)]), "something is left");
    assert_eq!(p.subpaths.len(), 1, "a subpath of one anchor goes");
    assert!(!p.delete_anchors(&[AnchorRef::new(0, 1)]));
    assert_eq!(p.subpaths[0].anchors.len(), 2);
    assert!(p.delete_anchors(&[AnchorRef::new(0, 0), AnchorRef::new(9, 9)]), "emptied: the caller deletes the element");
}

#[test]
fn inserting_on_a_line_keeps_it_a_line_and_geometry_maps() {
    let mut p = PathGeom::polyline(&[Point::new(0.0, 0.0), Point::new(100.0, 0.0), Point::new(100.0, 100.0)], true);
    let at = p.insert_anchor(0, 2, 0.5).unwrap();
    assert_eq!(at, AnchorRef::new(0, 3), "the closing segment's new anchor goes last");
    assert_eq!(p.anchor(at).unwrap().p, Point::new(50.0, 50.0));
    assert_eq!(p.anchor(at).unwrap().mode, AnchorMode::Corner);
    assert!(p.insert_anchor(0, 0, 0.0).is_none());
    let moved = Geom::Path(p.clone()).mapped(|q| Point::new(q.x * 2.0, q.y + 1.0)).unwrap();
    let Geom::Path(m) = moved else { panic!() };
    assert_eq!(m.bounds().unwrap(), Rect::new(0.0, 1.0, 200.0, 100.0));
    assert_eq!(p.rescaled(&Rect::new(0.0, 0.0, 100.0, 100.0), &Rect::new(10.0, 10.0, 50.0, 50.0)).bounds().unwrap(), Rect::new(10.0, 10.0, 50.0, 50.0));
}
