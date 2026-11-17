//! Drawing wave 3's model (docs/DRAWING_CONTRACT.md section 2): combining
//! paths, gradients in fills and strokes, flipping, SVG in.

use pimble_board::combine::{area, CombineOp};
use pimble_board::path;
use pimble_board::*;
use pimble_crdt::NodeDoc;

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

fn rect_el(x: f32, y: f32, w: f32, h: f32) -> NewElement {
    NewElement::new(Kind::Rect, Geom::Rect(Rect::new(x, y, w, h)))
}

fn world_point(e: &Element, p: Point) -> Point {
    let mut q = [tiny_skia::Point::from_xy(p.x, p.y)];
    e.world.map_points(&mut q);
    Point::new(q[0].x, q[0].y)
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

// ── Combining ───────────────────────────────────────────────────────────────

#[test]
fn union_through_transforms_keeps_the_top_operand_in_one_undo() {
    let b = board().with_origin("ann");
    let bottom = add(&b, rect_el(0.0, 0.0, 100.0, 100.0).with_style(Style { fill: Some(Color::rgb(255, 0, 0)), ..Style::default() }));
    // A 100 square drawn at the origin and moved 50 right by its transform.
    let top = add(
        &b,
        rect_el(0.0, 0.0, 100.0, 100.0)
            .with_style(Style { fill: Some(Color::rgb(0, 0, 255)), width: Some(2.0), ..Style::default() })
            .with_transform(Transform::translate(50.0, 0.0)),
    );
    b.edit(st(), |t| t.set_note(&top, "the keeper")).into_result().unwrap();
    let mut manager = b.undo_manager(yrs::undo::Options::default()).unwrap();
    let (kept, _) = b.edit(st(), |t| t.combine(&[bottom.clone(), top.clone()], CombineOp::Union)).into_result().unwrap();
    assert_eq!(kept, top, "the top operand's place");
    let snap = b.snapshot();
    assert!(snap.get(&bottom).is_none(), "the other operand is gone");
    let e = snap.get(&top).unwrap();
    assert_eq!(e.raw.kind, Kind::Path);
    assert!(e.raw.transform.is_identity());
    assert_eq!(e.raw.style.fill, Some(Color::rgb(0, 0, 255)), "and its style");
    assert_eq!(e.raw.note.as_deref(), Some("the keeper"));
    let Geom::Path(p) = &e.raw.geom else { panic!("a path") };
    assert!(close(area(p), 15000.0, 0.5), "{}", area(p));
    assert_eq!(p.bounds().unwrap(), Rect::new(0.0, 0.0, 150.0, 100.0));
    // One undo: both rectangles back as they were.
    assert!(b.undo(&mut manager, false).is_some());
    let snap = b.snapshot();
    assert_eq!(snap.get(&bottom).unwrap().raw.kind, Kind::Rect);
    let back = snap.get(&top).unwrap();
    assert_eq!((back.raw.kind.clone(), back.raw.transform), (Kind::Rect, Transform::translate(50.0, 0.0)));
}

#[test]
fn subtract_is_the_back_minus_the_front_and_keeps_the_back() {
    let b = board();
    let back = add(&b, NewElement::new(Kind::Ellipse, Geom::Rect(Rect::new(0.0, 0.0, 100.0, 100.0))).with_style(Style { fill: Some(Color::rgb(1, 2, 3)), ..Style::default() }));
    let front = add(&b, rect_el(50.0, -10.0, 100.0, 120.0));
    let (kept, _) = b.edit(st(), |t| t.combine(&[front.clone(), back.clone()], CombineOp::Subtract)).into_result().unwrap();
    assert_eq!(kept, back);
    let snap = b.snapshot();
    assert!(snap.get(&front).is_none());
    let e = snap.get(&back).unwrap();
    assert_eq!(e.raw.style.fill, Some(Color::rgb(1, 2, 3)));
    let Geom::Path(p) = &e.raw.geom else { panic!() };
    let half_disc = std::f32::consts::PI * 2500.0 / 2.0;
    assert!(close(area(p), half_disc, half_disc * 0.005), "{}", area(p));
    assert!(p.bounds().unwrap().right() <= 50.01);
}

#[test]
fn combining_refuses_what_has_no_outline_locks_and_nothing_left() {
    let b = board();
    let a = add(&b, rect_el(0.0, 0.0, 10.0, 10.0));
    let far = add(&b, rect_el(100.0, 0.0, 10.0, 10.0));
    let text = add(&b, NewElement::new(Kind::Text, Geom::Rect(Rect::new(0.0, 0.0, 0.0, 0.0))).with_text("hi"));
    let sticky = add(&b, NewElement::new(Kind::Sticky, Geom::Rect(Rect::new(0.0, 0.0, 50.0, 50.0))));
    let err = b.edit(st(), |t| t.combine(&[a.clone(), text.clone(), sticky.clone()], CombineOp::Union)).into_result().unwrap_err();
    assert_eq!(err.to_string(), "Only shapes, lines without arrow heads, pen strokes and paths combine: a text and a sticky note do not.");
    let err = b.edit(st(), |t| t.combine(&[a.clone(), far.clone()], CombineOp::Intersect)).into_result().unwrap_err();
    assert!(err.to_string().contains("do not overlap"), "{err}");
    let err = b.edit(st(), |t| t.combine(&[a.clone()], CombineOp::Union)).into_result().unwrap_err();
    assert!(err.to_string().contains("two or more"));
    b.edit(st(), |t| t.set_locked(&far, true)).into_result().unwrap();
    assert!(b.edit(st(), |t| t.combine(&[a.clone(), far.clone()], CombineOp::Union)).into_result().unwrap_err().is_lock());
    // Nothing was written by any refusal.
    let snap = b.snapshot();
    assert_eq!(snap.get(&a).unwrap().raw.kind, Kind::Rect);
    assert_eq!(snap.elements.len(), 4);
}

#[test]
fn a_combined_group_member_stays_in_its_group_where_it_was() {
    let b = board();
    let a = add(&b, rect_el(0.0, 0.0, 10.0, 10.0));
    let c = add(&b, rect_el(5.0, 0.0, 10.0, 10.0));
    let g = add(&b, NewElement::new(Kind::Group, Geom::None));
    b.edit(st(), |t| {
        t.set_parent(&a, Some(&g))?;
        t.set_parent(&c, Some(&g))?;
        t.set_transform(&g, &Transform::translate(100.0, 100.0))
    })
    .into_result()
    .unwrap();
    let (kept, _) = b.edit(st(), |t| t.combine(&[a.clone(), c.clone()], CombineOp::Union)).into_result().unwrap();
    let snap = b.snapshot();
    let e = snap.get(&kept).unwrap();
    assert_eq!(e.parent.as_deref(), Some(g.as_str()));
    let Geom::Path(p) = &e.raw.geom else { panic!() };
    // In the group's space, and on the board where the squares were.
    assert_eq!(p.bounds().unwrap(), Rect::new(0.0, 0.0, 15.0, 10.0));
    assert!(close(e.bounds.x, 100.0, 3.0) && close(e.bounds.y, 100.0, 3.0), "{:?}", e.bounds);
}

#[test]
fn pen_strokes_lines_and_paths_with_holes_combine() {
    let b = board();
    let pen = add(&b, NewElement::stroke(Kind::Pen, vec![[0.0, 0.0, 0.5], [100.0, 0.0, 0.5], [100.0, 100.0, 0.5], [0.0, 100.0, 0.5], [0.0, 0.0, 0.5]]).with_style(Style { fill: Some(Color::BLACK), ..Style::default() }));
    let mut holed = path::rect(&Rect::new(50.0, 25.0, 100.0, 50.0), 0.0);
    holed.subpaths.extend(path::rect(&Rect::new(75.0, 40.0, 50.0, 20.0), 0.0).subpaths);
    holed.rule = PathRule::EvenOdd;
    let p = add(&b, NewElement::path(holed));
    let (kept, _) = b.edit(st(), |t| t.combine(&[pen, p], CombineOp::Exclude)).into_result().unwrap();
    let snap = b.snapshot();
    let Geom::Path(r) = &snap.get(&kept).unwrap().raw.geom else { panic!() };
    // 10000 + 4000 (the holed band) - 2 * its overlap with the square
    // (50x50 minus the hole's 25x20 part).
    let overlap = 2500.0 - 500.0;
    assert!(close(area(r), 10000.0 + 4000.0 - 2.0 * overlap, 2.0), "{}", area(r));
}

// ── Gradients ───────────────────────────────────────────────────────────────

fn red_to_blue() -> Gradient {
    Gradient::linear(Point::new(0.0, 0.5), Point::new(1.0, 0.5), vec![GradientStop::new(0.0, Color::rgb(255, 0, 0)), GradientStop::new(1.0, Color::rgb(0, 0, 255))])
}

#[test]
fn a_gradient_is_stored_beside_its_solid_fallback_and_merges_as_one_value() {
    let mut doc = NodeDoc::new();
    doc.init("board", "Canvas", None, T0).unwrap();
    let b = Board::of_node(&doc);
    b.create(st()).unwrap();
    let id = add(&b, rect_el(0.0, 0.0, 100.0, 50.0));
    b.edit(st(), |t| t.set_fill_paint(&id, &Paint::Gradient(red_to_blue()))).into_result().unwrap();
    let raw = b.read().unwrap();
    let el = raw.elements.iter().find(|e| e.id == id).unwrap();
    assert_eq!(el.style.fill_gradient, Some(red_to_blue()));
    // What an older app reads: the solid `fill` key, the first stop's color.
    assert_eq!(el.style.fill, Some(Color::rgb(255, 0, 0)));
    let resolved = el.style.resolved(&Kind::Rect);
    assert_eq!(resolved.fill_gradient, Some(red_to_blue()));
    // A solid color picked after it replaces it.
    b.edit(st(), |t| t.set_fill_paint(&id, &Paint::Solid(Color::rgb(0, 255, 0)))).into_result().unwrap();
    let snap = b.snapshot();
    let e = snap.get(&id).unwrap();
    assert_eq!((e.raw.style.fill, e.raw.style.fill_gradient.clone()), (Some(Color::rgb(0, 255, 0)), None));
    // "No fill" leaves no gradient behind, by either way of saying it.
    b.edit(st(), |t| t.set_fill_paint(&id, &Paint::Gradient(red_to_blue()))).into_result().unwrap();
    b.edit(st(), |t| t.clear_style(&id, StyleKey::Fill)).into_result().unwrap();
    let snap = b.snapshot();
    assert_eq!(snap.get(&id).unwrap().style.fill, None);
    assert_eq!(snap.get(&id).unwrap().style.fill_gradient, None);
    // A stroke is never none.
    assert!(b.edit(st(), |t| t.set_stroke_paint(&id, &Paint::None)).into_result().is_err());
    // An element with no gradient keys reads as it always did.
    let plain = add(&b, rect_el(0.0, 0.0, 10.0, 10.0).with_style(Style { fill: Some(Color::rgb(9, 9, 9)), ..Style::default() }));
    let snap = b.snapshot();
    let s = &snap.get(&plain).unwrap().style;
    assert_eq!((s.fill, s.fill_gradient.clone(), s.stroke_gradient.clone()), (Some(Color::rgb(9, 9, 9)), None, None));
}

#[test]
fn a_linear_gradient_draws_across_the_element_and_moves_with_it() {
    let b = board();
    let id = add(&b, rect_el(0.0, 0.0, 100.0, 40.0).with_style(Style { width: Some(0.0), ..Style::default() }));
    b.edit(st(), |t| t.set_fill_paint(&id, &Paint::Gradient(red_to_blue()))).into_result().unwrap();
    let px = draw(&b, 200, 60);
    let (l, r) = (pixel(&px, 3, 20), pixel(&px, 96, 20));
    assert!(l.0 > 230 && l.2 < 25, "red at the left: {l:?}");
    assert!(r.2 > 230 && r.0 < 25, "blue at the right: {r:?}");
    // Moved 100 right: the gradient goes with it.
    b.edit(st(), |t| t.set_transform(&id, &Transform::translate(100.0, 0.0))).into_result().unwrap();
    let px = draw(&b, 200, 60);
    let (l, r) = (pixel(&px, 103, 20), pixel(&px, 196, 20));
    assert!(l.0 > 230 && r.2 > 230, "{l:?} {r:?}");
    // Resized geometrically: it stretches with the box.
    b.edit(st(), |t| {
        t.set_transform(&id, &Transform::IDENTITY)?;
        t.set_geom(&id, &Geom::Rect(Rect::new(0.0, 0.0, 200.0, 40.0)))
    })
    .into_result()
    .unwrap();
    let px = draw(&b, 200, 60);
    let mid = pixel(&px, 100, 20);
    assert!(close(mid.0 as f32, mid.2 as f32, 20.0), "the middle is half way: {mid:?}");
}

#[test]
fn a_radial_gradient_and_a_gradient_stroke_draw_and_export() {
    let b = board();
    let radial = Gradient::radial(Point::new(0.5, 0.5), Point::new(1.0, 0.5), vec![GradientStop::new(0.0, Color::WHITE), GradientStop::new(1.0, Color::BLACK)]);
    let id = add(&b, NewElement::new(Kind::Ellipse, Geom::Rect(Rect::new(0.0, 0.0, 100.0, 100.0))).with_style(Style { width: Some(0.0), fill_gradient: Some(radial), ..Style::default() }));
    let px = draw(&b, 120, 120);
    let (c, edge) = (pixel(&px, 50, 50), pixel(&px, 95, 50));
    assert!(c.0 > 240, "white in the middle: {c:?}");
    assert!(edge.0 < 40, "dark at the edge: {edge:?}");
    let line = add(
        &b,
        NewElement::new(Kind::Line, Geom::Segment { a: Point::new(0.0, 110.0), b: Point::new(100.0, 110.0) })
            .with_style(Style { width: Some(6.0), ..Style::default() }),
    );
    b.edit(st(), |t| t.set_stroke_paint(&line, &Paint::Gradient(red_to_blue()))).into_result().unwrap();
    let snap = b.snapshot();
    // A line's flat box gets room: its stroke still runs red to blue.
    let svg = render_svg(&snap, Rect::new(0.0, 0.0, 120.0, 120.0), None, &|_| None);
    assert!(svg.contains("<radialGradient id=\"pimble-gradient-1\" gradientUnits=\"userSpaceOnUse\" cx=\"50\" cy=\"50\" r=\"50\">"), "{svg}");
    assert!(svg.contains("fill=\"url(#pimble-gradient-1)\""));
    assert!(svg.contains("<linearGradient id=\"pimble-gradient-2\""), "{svg}");
    assert!(svg.contains("stroke=\"url(#pimble-gradient-2)\""));
    assert!(svg.contains("<stop offset=\"1\" stop-color=\"#0000ff\"/>"));
    let px = draw(&b, 120, 120);
    let (l, r) = (pixel(&px, 4, 110), pixel(&px, 96, 110));
    assert!(l.0 > 200 && r.2 > 200, "{l:?} {r:?}");
    let _ = id;
}

#[test]
fn gradient_defaults_and_colors_along_them() {
    let g = Gradient::default_linear(Color::rgb(10, 20, 30));
    assert_eq!(g.stops.len(), 2);
    assert_eq!(g.stops[1].color, Color::rgba(10, 20, 30, 0));
    assert_eq!((g.from, g.to), (Point::new(0.0, 0.5), Point::new(1.0, 0.5)));
    assert_eq!(g.color_at(0.5), Color::rgba(10, 20, 30, 128));
    let r = Gradient::default_radial(Color::WHITE);
    assert_eq!(r.kind, GradientKind::Radial);
    assert!(close(r.radius(), 0.5, 1e-6));
    assert_eq!(Gradient::box_for(Rect::new(0.0, 10.0, 100.0, 0.0)), Rect::new(0.0, -40.0, 100.0, 100.0));
}

// ── Flipping ────────────────────────────────────────────────────────────────

#[test]
fn a_path_flips_in_place_and_back_through_its_transform() {
    let b = board();
    let tri = path::parse_d("M0 0 L100 0 L0 50 Z").unwrap();
    let id = add(&b, NewElement::path(tri).with_transform(Transform { tx: 10.0, ty: 5.0, rot: 30.0, sx: 1.0, sy: 2.0 }));
    let before: Vec<Point> = {
        let snap = b.snapshot();
        let e = snap.get(&id).unwrap();
        let Geom::Path(p) = &e.raw.geom else { panic!() };
        p.subpaths[0].anchors.iter().map(|a| world_point(e, a.p)).collect()
    };
    let bounds = b.snapshot().get(&id).unwrap().bounds;
    let c = bounds.center();
    b.edit(st(), |t| t.flip(std::slice::from_ref(&id), FlipAxis::Horizontal)).into_result().unwrap();
    let snap = b.snapshot();
    let e = snap.get(&id).unwrap();
    let Geom::Path(p) = &e.raw.geom else { panic!() };
    for (a, was) in p.subpaths[0].anchors.iter().zip(&before) {
        let now = world_point(e, a.p);
        assert!(close(now.x, 2.0 * c.x - was.x, 0.01) && close(now.y, was.y, 0.01), "{now:?} vs {was:?}");
    }
    // Flipped back: the same transform it had.
    b.edit(st(), |t| t.flip(std::slice::from_ref(&id), FlipAxis::Horizontal)).into_result().unwrap();
    let t = b.snapshot().get(&id).unwrap().raw.transform;
    assert!(close(t.tx, 10.0, 1e-3) && close(t.ty, 5.0, 1e-3) && close(t.rot, 30.0, 1e-3) && close(t.sx, 1.0, 1e-6) && close(t.sy, 2.0, 1e-6), "{t:?}");
}

#[test]
fn text_moves_to_its_mirrored_place_without_mirroring_and_shapes_mirror_about_the_selection() {
    let b = board();
    let text = add(&b, NewElement::new(Kind::Text, Geom::Rect(Rect::new(0.0, 0.0, 0.0, 0.0))).with_text("Label"));
    let pen = add(&b, NewElement::stroke(Kind::Pen, vec![[200.0, 0.0, 0.5], [300.0, 100.0, 0.5]]));
    let snap = b.snapshot();
    let (tb, pb) = (snap.get(&text).unwrap().bounds, snap.get(&pen).unwrap().bounds);
    let all = tb.union(&pb);
    b.edit(st(), |t| t.flip(&[text.clone(), pen.clone()], FlipAxis::Vertical)).into_result().unwrap();
    let snap = b.snapshot();
    let te = snap.get(&text).unwrap();
    assert_eq!((te.raw.transform.sx, te.raw.transform.sy, te.raw.transform.rot), (1.0, 1.0, 0.0), "glyphs are not mirrored");
    let mirrored_center_y = 2.0 * all.center().y - tb.center().y;
    assert!(close(te.bounds.center().y, mirrored_center_y, 0.01), "{:?}", te.bounds);
    let pe = snap.get(&pen).unwrap();
    // The stroke went from top-left to bottom-right; now bottom-left to top-right.
    let (a, z) = (world_point(pe, Point::new(200.0, 0.0)), world_point(pe, Point::new(300.0, 100.0)));
    assert!(close(a.y, 2.0 * all.center().y, 0.5) && close(z.y, 2.0 * all.center().y - 100.0, 0.5), "{a:?} {z:?}");
}

#[test]
fn a_group_flips_its_members_and_a_lock_refuses_the_flip() {
    let b = board();
    let a = add(&b, rect_el(0.0, 0.0, 10.0, 10.0));
    let c = add(&b, NewElement::new(Kind::Star, Geom::Rect(Rect::new(90.0, 0.0, 10.0, 10.0))));
    let g = add(&b, NewElement::new(Kind::Group, Geom::None));
    b.edit(st(), |t| {
        t.set_parent(&a, Some(&g))?;
        t.set_parent(&c, Some(&g))
    })
    .into_result()
    .unwrap();
    b.edit(st(), |t| t.flip(std::slice::from_ref(&g), FlipAxis::Horizontal)).into_result().unwrap();
    let snap = b.snapshot();
    assert!(close(snap.get(&a).unwrap().bounds.center().x, 95.0, 0.01));
    assert!(close(snap.get(&c).unwrap().bounds.center().x, 5.0, 0.01));
    assert_eq!(snap.get(&c).unwrap().raw.transform.sx, -1.0, "the star is mirrored");
    assert!(snap.get(&g).unwrap().raw.transform.is_identity(), "the group itself is not written");
    b.edit(st(), |t| t.set_locked(&a, true)).into_result().unwrap();
    assert!(b.edit(st(), |t| t.flip(std::slice::from_ref(&g), FlipAxis::Vertical)).into_result().unwrap_err().is_lock());
}

// ── SVG in ──────────────────────────────────────────────────────────────────

#[test]
fn svg_markup_goes_in_as_one_edit_with_its_groups() {
    let b = board().with_origin("ann");
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <defs><linearGradient id="g"><stop offset="0" stop-color="#ff0000"/><stop offset="1" stop-color="#0000ff"/></linearGradient></defs>
        <g><rect width="50" height="50" fill="url(#g)"/><circle cx="75" cy="75" r="25" fill="#00ff00"/></g>
        <text x="10" y="90" font-size="12">Hi</text>
    </svg>"##;
    let import = svg_in::parse(svg, &SvgPlacement { at: Point::new(500.0, 500.0), ..SvgPlacement::default() }).unwrap();
    let mut manager = b.undo_manager(yrs::undo::Options::default()).unwrap();
    let (ids, _) = b.edit(st(), |t| t.import_svg(&import, None)).into_result().unwrap();
    assert_eq!(ids.len(), 4, "a group, two paths, a text");
    let snap = b.snapshot();
    let group = snap.get(&ids[0]).unwrap();
    assert_eq!(group.raw.kind, Kind::Group);
    assert_eq!(snap.get(&ids[1]).unwrap().parent.as_deref(), Some(ids[0].as_str()));
    assert!(snap.get(&ids[1]).unwrap().style.fill_gradient.is_some());
    let t = snap.get(&ids[3]).unwrap();
    assert_eq!((t.raw.kind.clone(), t.raw.text.as_deref()), (Kind::Text, Some("Hi")));
    assert!(close(group.bounds.x, 500.0, 0.5) && close(group.bounds.y, 500.0, 0.5), "{:?}", group.bounds);
    // One undo takes the whole drawing back.
    assert!(b.undo(&mut manager, false).is_some());
    assert_eq!(b.snapshot().elements.len(), 0);
}

#[test]
fn svg_in_needs_a_vector_layer_like_any_new_element() {
    let b = board();
    let first = b.snapshot().layers[0].id.clone();
    b.edit(st(), |t| {
        t.add_layer_in("Paint 1", LayerKind::Paint, None, Placement::Top)?;
        t.delete_layer(&first)
    })
    .into_result()
    .unwrap();
    let import = svg_in::parse(r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10"/></svg>"#, &SvgPlacement::default()).unwrap();
    assert_eq!(b.edit(st(), |t| t.import_svg(&import, None)).into_result().unwrap_err(), BoardError::NoVectorLayer);
}
