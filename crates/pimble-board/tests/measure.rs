//! Measurements on a canvas (docs/MEASURE_CONTRACT.md): the stored scale
//! (round trip, undo, two people), what elements measure after their
//! transforms, dimension elements and the `dims`/`area` labels in the
//! renderer and the SVG export, relabeling on a change of scale.

use pimble_board::measure::{self, parse_scale, Unit};
use pimble_board::*;
use pimble_crdt::NodeDoc;

const T: &str = "2026-11-22T10:00:00.000Z";

fn st() -> Stamp<'static> {
    Stamp::new(None, T)
}

fn ok<R: std::fmt::Debug>(board: &Board, f: impl FnOnce(&mut BoardTxn<'_, '_>) -> doc::Result<R>) -> R {
    board.edit(st(), f).into_result().unwrap().0
}

fn board() -> Board {
    let b = Board::standalone();
    b.create(st()).unwrap();
    b
}

fn feet() -> Scale {
    // 1 in = 10 ft: a foot is 9.6 board units.
    parse_scale("1 in = 10 ft", Unit::Ft).unwrap()
}

fn close(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

fn pixels(snap: &BoardSnapshot, w: u32, h: u32) -> Vec<u8> {
    render(snap, &NoPictures, &View::new(0.0, 0.0, 1.0), w, h, Some(Color::WHITE)).unwrap().take()
}

#[test]
fn the_scale_is_stored_read_back_undone_and_removed() {
    let b = board();
    assert_eq!(b.snapshot().scale, None, "no scale until one is set");
    let mut manager = b.undo_manager(yrs::undo::Options::default()).unwrap();
    let s = feet().with_precision(Some(2));
    ok(&b, |t| t.set_scale(Some(&s)));
    let snap = b.snapshot();
    assert_eq!(snap.scale.as_ref(), Some(&s));
    assert_eq!(b.read().unwrap().scale.as_ref(), Some(&s));
    assert_eq!(snap.scale.as_ref().unwrap().label.as_deref(), Some("1 in = 10 ft"));
    b.undo(&mut manager, false).expect("setting the scale is one undo step");
    assert_eq!(b.snapshot().scale, None);
    b.undo(&mut manager, true).expect("and redo");
    assert_eq!(b.snapshot().scale.as_ref(), Some(&s));
    ok(&b, |t| t.set_scale(None));
    assert_eq!(b.snapshot().scale, None);
    // A scale that is no scale is refused and writes nothing.
    let edited = b.edit(st(), |t| t.set_scale(Some(&Scale::new(Unit::M, 0.0))));
    assert!(edited.update.is_none() && edited.result.is_err());
}

#[test]
fn a_copy_keeps_the_scale() {
    let b = board();
    ok(&b, |t| t.set_scale(Some(&feet())));
    let raw = b.read().unwrap();
    let copy = Board::standalone();
    copy.edit(st(), |t| t.copy_from(&raw)).into_result().unwrap();
    assert_eq!(copy.snapshot().scale, Some(feet()));
}

#[test]
fn two_people_setting_scales_end_with_one() {
    let node = || {
        let mut d = NodeDoc::new();
        d.init("board", "Canvas", None, T).unwrap();
        d
    };
    let mut a = node();
    Board::of_node(&a).create(st()).unwrap();
    let mut b = node();
    let sync = |a: &mut NodeDoc, b: &mut NodeDoc| {
        let (asv, bsv) = (a.state_vector(), b.state_vector());
        let to_b = a.diff_since(&bsv).unwrap();
        let to_a = b.diff_since(&asv).unwrap();
        b.apply_update(&to_b).unwrap();
        a.apply_update(&to_a).unwrap();
    };
    sync(&mut a, &mut b);
    Board::of_node(&a).edit(st(), |t| t.set_scale(Some(&feet()))).into_result().unwrap();
    Board::of_node(&b).edit(st(), |t| t.set_scale(Some(&parse_scale("1:100", Unit::M).unwrap()))).into_result().unwrap();
    sync(&mut a, &mut b);
    let (sa, sb) = (Board::of_node(&a).snapshot().scale, Board::of_node(&b).snapshot().scale);
    assert!(sa.is_some() && sa == sb, "one whole scale, the same on both: {sa:?} {sb:?}");
}

#[test]
fn elements_measure_as_seen() {
    let b = board();
    let line = ok(&b, |t| t.add_element(NewElement::new(Kind::Line, Geom::Segment { a: Point::new(0.0, 0.0), b: Point::new(30.0, -40.0) })));
    let rect = ok(&b, |t| t.add_element(NewElement::new(Kind::Rect, Geom::Rect(Rect::new(100.0, 100.0, 96.0, 48.0)))));
    let ellipse = ok(&b, |t| t.add_element(NewElement::new(Kind::Ellipse, Geom::Rect(Rect::new(0.0, 200.0, 20.0, 10.0)))));
    let diamond = ok(&b, |t| t.add_element(NewElement::new(Kind::Diamond, Geom::Rect(Rect::new(300.0, 0.0, 40.0, 30.0)))));
    let pen = ok(&b, |t| t.add_element(NewElement::stroke(Kind::Pen, vec![[0.0, 0.0, 0.5], [3.0, 4.0, 0.5], [3.0, 14.0, 0.5]])));
    let path = ok(&b, |t| t.add_element(NewElement::path(path::PathGeom::polyline(&[Point::new(0.0, 0.0), Point::new(10.0, 0.0), Point::new(10.0, 10.0)], true))));
    // Turned a quarter and doubled across: a rotated, scaled box measures
    // as it is seen.
    ok(&b, |t| t.set_transform(&rect, &Transform { rot: 90.0, sx: 2.0, ..Transform::IDENTITY }));
    let snap = b.snapshot();
    let m = measure::of(snap.get(&line).unwrap());
    assert!(close(m.length.unwrap(), 50.0, 1e-3) && close(m.angle.unwrap(), 53.130, 1e-2), "{m:?}");
    let m = measure::of(snap.get(&rect).unwrap());
    assert!(close(m.width.unwrap(), 192.0, 1e-2) && close(m.height.unwrap(), 48.0, 1e-2), "{m:?}");
    assert!(close(m.area.unwrap(), 192.0 * 48.0, 0.5) && close(m.perimeter.unwrap(), 480.0, 0.05), "{m:?}");
    let m = measure::of(snap.get(&ellipse).unwrap());
    assert!(close(m.area.unwrap(), std::f32::consts::PI * 50.0, 1e-2), "{m:?}");
    assert!(close(m.perimeter.unwrap(), 48.442, 1e-2), "Ramanujan: {m:?}");
    let m = measure::of(snap.get(&diamond).unwrap());
    assert!(close(m.area.unwrap(), 600.0, 0.5) && close(m.perimeter.unwrap(), 100.0, 0.05), "{m:?}");
    let m = measure::of(snap.get(&pen).unwrap());
    assert!(close(m.length.unwrap(), 15.0, 1e-3) && m.area.is_none(), "{m:?}");
    let m = measure::of(snap.get(&path).unwrap());
    assert!(close(m.area.unwrap(), 50.0, 1e-3) && close(m.perimeter.unwrap(), 34.142, 1e-2), "a closed path: {m:?}");
}

fn dimension(b: &Board, a: Point, z: Point, offset: f32) -> String {
    ok(b, |t| {
        t.add_element(NewElement::new(Kind::Dimension, Geom::Segment { a, b: z }).with_style(Style { dim_offset: Some(offset), ..Style::default() }))
    })
}

#[test]
fn a_dimension_shows_its_length_in_the_canvas_units() {
    let b = board();
    let d = dimension(&b, Point::new(20.0, 100.0), Point::new(20.0 + 9.6 * 12.5, 100.0), 20.0);
    let snap = b.snapshot();
    let e = snap.get(&d).unwrap();
    assert_eq!(e.marks.as_ref().unwrap().texts, vec!["120".to_string()], "without a scale, board units");
    let before = e.fingerprint;
    ok(&b, |t| t.set_scale(Some(&feet())));
    let snap = b.snapshot();
    let e = snap.get(&d).unwrap();
    assert_eq!(e.marks.as_ref().unwrap().texts, vec!["12′ 6″".to_string()]);
    assert_ne!(e.fingerprint, before, "a new scale relabels: the raster cache redraws it");
    // The dimension line sits 20 above the points (a positive offset is
    // above a line drawn east), and the bounds hold it and the label.
    assert!(e.bounds.y < 100.0 - 20.0 - 6.0, "{:?}", e.bounds);
    // Hit on its dimension line, not far from it.
    assert!(e.hits(Point::new(60.0, 80.0), 1.0));
    assert!(!e.hits(Point::new(60.0, 140.0), 1.0));
    // Its text replaces the label, `{}` standing for the length.
    ok(&b, |t| t.set_text(&d, "{} clear"));
    assert_eq!(b.snapshot().get(&d).unwrap().marks.as_ref().unwrap().texts, vec!["12′ 6″ clear".to_string()]);
    // It draws: dark pixels where the line and label are.
    let px = pixels(&b.snapshot(), 200, 160);
    let dark = px.chunks_exact(4).filter(|p| p[0] < 120 && p[1] < 120 && p[2] < 120).count();
    assert!(dark > 50, "{dark}");
}

#[test]
fn dimensions_tick_in_imperial_and_use_arrows_in_metric() {
    let b = board();
    let d = dimension(&b, Point::new(0.0, 50.0), Point::new(100.0, 50.0), -10.0);
    ok(&b, |t| t.set_scale(Some(&feet())));
    assert!(b.snapshot().get(&d).unwrap().marks.as_ref().unwrap().heads.is_none(), "ticks");
    ok(&b, |t| t.set_scale(Some(&parse_scale("1:100", Unit::M).unwrap())));
    assert!(b.snapshot().get(&d).unwrap().marks.as_ref().unwrap().heads.is_some(), "arrows");
    ok(&b, |t| t.set_style(&d, &Style { arrow_heads: Some(ArrowHeads::None), ..Style::default() }));
    assert!(b.snapshot().get(&d).unwrap().marks.as_ref().unwrap().heads.is_none(), "arrow_heads none asks for ticks");
    // Degenerate dimensions draw without panicking.
    let zero = dimension(&b, Point::new(5.0, 5.0), Point::new(5.0, 5.0), 0.0);
    let snap = b.snapshot();
    assert!(snap.get(&zero).is_some());
    let _ = pixels(&snap, 120, 80);
    let _ = render_svg(&snap, Rect::new(-50.0, -50.0, 300.0, 200.0), None, &|_| None);
}

#[test]
fn dims_and_area_label_shapes_only_with_a_scale() {
    let b = board();
    let room = ok(&b, |t| {
        t.add_element(NewElement::new(Kind::Rect, Geom::Rect(Rect::new(50.0, 50.0, 9.6 * 12.0, 9.6 * 10.0))).with_style(Style { dims: Some(true), area: Some(true), ..Style::default() }))
    });
    let wall = ok(&b, |t| {
        t.add_element(NewElement::new(Kind::Line, Geom::Segment { a: Point::new(0.0, 300.0), b: Point::new(96.0, 300.0) }).with_style(Style { dims: Some(true), ..Style::default() }))
    });
    let hex = ok(&b, |t| {
        t.add_element(NewElement::new(Kind::Polygon, Geom::Rect(Rect::new(300.0, 300.0, 100.0, 100.0))).with_style(Style { dims: Some(true), sides: Some(6), ..Style::default() }))
    });
    let snap = b.snapshot();
    assert!(snap.get(&room).unwrap().marks.is_none() && snap.get(&wall).unwrap().marks.is_none(), "nothing without a scale");
    let plain = render_svg(&snap, Rect::new(0.0, 0.0, 500.0, 500.0), None, &|_| None);
    ok(&b, |t| t.set_scale(Some(&feet())));
    let snap = b.snapshot();
    assert_eq!(snap.get(&room).unwrap().marks.as_ref().unwrap().texts, vec!["12′".to_string(), "10′".to_string(), "120 sq ft".to_string()]);
    assert_eq!(snap.get(&wall).unwrap().marks.as_ref().unwrap().texts, vec!["10′".to_string()]);
    assert_eq!(snap.get(&hex).unwrap().marks.as_ref().unwrap().texts.len(), 6, "a label per side");
    // The SVG export carries the labels (as glyph outlines, haloed).
    let labeled = render_svg(&snap, Rect::new(0.0, 0.0, 500.0, 500.0), None, &|_| None);
    assert!(labeled.matches("<path").count() > plain.matches("<path").count() + 4, "labels in the SVG");
    assert!(labeled.contains("stroke=\"#ffffff\""), "the labels' halo");
    assert!(!labeled.contains("sq ft"), "no text nodes, glyphs");
    // Switching dims off takes the side labels away and leaves the area.
    ok(&b, |t| t.clear_style(&room, StyleKey::Dims));
    assert_eq!(b.snapshot().get(&room).unwrap().marks.as_ref().unwrap().texts, vec!["120 sq ft".to_string()]);
}

#[test]
fn measuring_off_keeps_the_scale_and_labels_nothing() {
    let b = board();
    let room = ok(&b, |t| {
        t.add_element(NewElement::new(Kind::Rect, Geom::Rect(Rect::new(0.0, 0.0, 96.0, 96.0))).with_style(Style { dims: Some(true), area: Some(true), ..Style::default() }))
    });
    let off = Scale { off: true, ..feet() };
    ok(&b, |t| t.set_scale(Some(&off)));
    let snap = b.snapshot();
    assert_eq!(snap.scale.as_ref(), Some(&off), "the scale is kept");
    assert!(snap.measuring().is_none());
    assert!(snap.get(&room).unwrap().marks.is_none(), "nothing labeled while off");
    ok(&b, |t| t.set_scale(Some(&feet())));
    let snap = b.snapshot();
    assert_eq!(snap.measuring(), Some(&feet()));
    assert!(snap.get(&room).unwrap().marks.is_some());
}

#[test]
fn labels_read_light_on_a_dark_canvas() {
    let b = board();
    let room = ok(&b, |t| {
        t.add_element(NewElement::new(Kind::Rect, Geom::Rect(Rect::new(0.0, 0.0, 96.0, 96.0))).with_style(Style { area: Some(true), ..Style::default() }))
    });
    ok(&b, |t| t.set_scale(Some(&feet())));
    let light = b.snapshot();
    let dark = BoardSnapshot::build_with(b.read(), pimble_board::ViewOptions { dark: true, ..Default::default() });
    let (l, d) = (light.get(&room).unwrap().marks.clone().unwrap(), dark.get(&room).unwrap().marks.clone().unwrap());
    assert_eq!(l.label_color, pimble_board::measure::LABEL_COLOR);
    assert_eq!(d.label_color, pimble_board::measure::LABEL_COLOR_DARK);
    assert_eq!(d.halo_color, pimble_board::measure::LABEL_HALO_DARK);
    assert_ne!(l.hash, d.hash, "the cache redraws when the theme changes");
}

#[test]
fn the_raster_cache_relabels_on_a_new_scale() {
    let b = board();
    ok(&b, |t| t.set_scale(Some(&feet())));
    ok(&b, |t| {
        t.add_element(NewElement::new(Kind::Rect, Geom::Rect(Rect::new(40.0, 40.0, 96.0, 96.0))).with_style(Style { area: Some(true), ..Style::default() }))
    });
    let full_before = pixels(&b.snapshot(), 200, 200);
    ok(&b, |t| t.set_scale(Some(&parse_scale("1:100", Unit::M).unwrap())));
    let full_after = pixels(&b.snapshot(), 200, 200);
    assert_ne!(full_before, full_after, "the label changed");
}
