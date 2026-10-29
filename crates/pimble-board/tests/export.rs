//! Hit testing against painted geometry, view options, the view's grid, and
//! export (a part of a board as pixels, JPEG, SVG).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use pimble_board::*;

const T0: &str = "2026-10-09T10:00:00.000Z";

fn sam() -> Author {
    Author::new("sam-sub", "Sam")
}

fn board() -> Board {
    let board = Board::standalone();
    board.create(Stamp::new(None, T0)).unwrap();
    board
}

fn add(board: &Board, el: NewElement) -> String {
    board.add_element(Stamp::new(None, T0), el).unwrap().0
}

fn filled(color: Color) -> Style {
    Style { fill: Some(color), ..Style::default() }
}

#[test]
fn a_diagonal_stroke_is_hit_on_its_line_not_in_its_empty_corner() {
    let b = board();
    let pts: Vec<[f32; 3]> = (0..=20).map(|i| [i as f32 * 10.0, i as f32 * 10.0, 0.5]).collect();
    let id = add(&b, NewElement::stroke(Kind::Pen, pts));
    let snap = b.snapshot();
    let e = snap.get(&id).unwrap();
    assert!(e.bounds.contains(Point::new(180.0, 20.0)), "the corner is in its bounding box");
    assert!(!e.hits(Point::new(180.0, 20.0), 2.0));
    assert!(e.hits(Point::new(100.0, 100.0), 0.0));
    assert!(e.hits(Point::new(103.0, 98.0), 2.0));
    assert!(snap.hit_precise(Point::new(180.0, 20.0), 2.0).is_none());
    assert_eq!(snap.hit_precise(Point::new(150.0, 151.0), 2.0).map(|e| e.id()), Some(id.as_str()));
}

#[test]
fn an_unfilled_shape_is_hit_on_its_outline_and_a_filled_one_inside() {
    let b = board();
    let open = add(&b, NewElement::new(Kind::Rect, Geom::Rect(Rect::new(0.0, 0.0, 100.0, 60.0))));
    let full = add(
        &b,
        NewElement::new(Kind::Rect, Geom::Rect(Rect::new(200.0, 0.0, 100.0, 60.0))).with_style(filled(Color::rgb(0, 128, 255))),
    );
    let ellipse = add(&b, NewElement::new(Kind::Ellipse, Geom::Rect(Rect::new(0.0, 200.0, 100.0, 60.0))));
    let labelled = add(&b, NewElement::new(Kind::Diamond, Geom::Rect(Rect::new(200.0, 200.0, 100.0, 60.0))).with_text("go"));
    let snap = b.snapshot();
    let (open, full, ellipse, labelled) =
        (snap.get(&open).unwrap(), snap.get(&full).unwrap(), snap.get(&ellipse).unwrap(), snap.get(&labelled).unwrap());
    assert!(!open.hits(Point::new(50.0, 30.0), 2.0), "an unfilled rectangle's middle is not it");
    assert!(open.hits(Point::new(0.0, 30.0), 0.0));
    assert!(open.hits(Point::new(101.0, 30.0), 1.0));
    assert!(full.hits(Point::new(250.0, 30.0), 0.0));
    assert!(!ellipse.hits(Point::new(50.0, 230.0), 2.0));
    assert!(ellipse.hits(Point::new(100.0, 230.0), 1.0));
    assert!(!ellipse.hits(Point::new(2.0, 202.0), 2.0), "an ellipse's bounding corner is not it");
    assert!(labelled.hits(Point::new(250.0, 230.0), 0.0), "a shape with text is hit inside");
    assert!(!labelled.hits(Point::new(203.0, 203.0), 1.0), "outside a diamond's edges");
}

#[test]
fn hit_testing_follows_rotation() {
    let b = board();
    let id = add(
        &b,
        NewElement::new(Kind::Rect, Geom::Rect(Rect::new(0.0, 0.0, 100.0, 20.0)))
            .with_style(filled(Color::rgb(10, 10, 10)))
            .with_transform(Transform { rot: 45.0, ..Transform::IDENTITY }),
    );
    let snap = b.snapshot();
    let e = snap.get(&id).unwrap();
    let c = Point::new(50.0, 10.0);
    assert!(e.hits(c, 0.0));
    // Along the rotated long axis (clockwise, y down).
    let d = 40.0 / 2f32.sqrt();
    assert!(e.hits(Point::new(c.x + d, c.y + d), 0.0));
    // Where the long axis was before rotating.
    assert!(!e.hits(Point::new(c.x + 40.0, c.y), 1.0));
    // A corner of its axis-aligned bounds.
    assert!(!e.hits(Point::new(e.bounds.x + 2.0, e.bounds.y + 2.0), 1.0));
}

#[test]
fn a_frame_is_hit_on_its_border_and_name_and_what_is_inside_stays_clickable() {
    let b = board();
    let frame = add(&b, NewElement::new(Kind::Frame, Geom::Rect(Rect::new(0.0, 0.0, 300.0, 200.0))).with_text("Plan"));
    let inner = add(
        &b,
        NewElement::new(Kind::Rect, Geom::Rect(Rect::new(100.0, 80.0, 50.0, 40.0)))
            .with_style(filled(Color::rgb(200, 0, 0)))
            .with_parent(frame.clone()),
    );
    let snap = b.snapshot();
    let f = snap.get(&frame).unwrap();
    assert!(!f.hits(Point::new(150.0, 100.0), 2.0));
    assert!(!f.hits(Point::new(50.0, 50.0), 2.0));
    assert!(f.hits(Point::new(0.0, 100.0), 0.0));
    assert!(f.hits(Point::new(300.0, 1.0), 1.0));
    let tb = f.text_box.unwrap();
    assert!(f.hits(tb.center(), 0.0), "its name label");
    assert_eq!(snap.hit_precise(Point::new(125.0, 100.0), 2.0).map(|e| e.id()), Some(inner.as_str()));
    assert!(snap.hit_precise(Point::new(50.0, 50.0), 2.0).is_none());
    assert_eq!(snap.hit_precise(Point::new(0.0, 50.0), 2.0).map(|e| e.id()), Some(frame.as_str()));
    assert_eq!(snap.frames().iter().map(|e| e.id()).collect::<Vec<_>>(), vec![frame.as_str()]);
}

#[test]
fn a_marquee_selects_what_lies_wholly_inside_it() {
    let b = board();
    let a = add(&b, NewElement::new(Kind::Rect, Geom::Rect(Rect::new(10.0, 10.0, 50.0, 50.0))));
    let c = add(&b, NewElement::new(Kind::Ellipse, Geom::Rect(Rect::new(80.0, 10.0, 50.0, 50.0))));
    let _far = add(&b, NewElement::new(Kind::Rect, Geom::Rect(Rect::new(150.0, 10.0, 100.0, 50.0))));
    let snap = b.snapshot();
    let got: Vec<&str> = snap.within(&Rect::new(0.0, 0.0, 200.0, 100.0)).iter().map(|e| e.id()).collect();
    assert_eq!(got, vec![a.as_str(), c.as_str()]);
    // Drawn from the other corner.
    let got: Vec<&str> = snap.within(&Rect::new(70.0, 0.0, -70.0, 100.0)).iter().map(|e| e.id()).collect();
    assert_eq!(got, vec![a.as_str()]);
}

#[test]
fn view_options_color_by_author_show_everything_and_title_cards() {
    let b = board();
    let s = sam();
    let st = Stamp::new(Some(&s), T0);
    let mine = b
        .add_element(st, NewElement::new(Kind::Rect, Geom::Rect(Rect::new(0.0, 0.0, 50.0, 50.0))).with_style(filled(Color::WHITE)))
        .unwrap()
        .0;
    let anon = add(&b, NewElement::new(Kind::Sticky, Geom::Rect(Rect::new(100.0, 0.0, 100.0, 100.0))).with_text("hi"));
    let pic = add(
        &b,
        NewElement::new(
            Kind::Picture,
            Geom::Picture(PictureGeom { rect: Rect::new(0.0, 200.0, 10.0, 10.0), src: "pimble-blob:p".into(), px_w: 2, px_h: 2, crop: None }),
        ),
    );
    let card = add(
        &b,
        NewElement::new(Kind::Card, Geom::Card { rect: Rect::new(300.0, 0.0, 200.0, 50.0), link: "pimble://node/1".into() }),
    );
    let layer = b.snapshot().top_layer().unwrap().id.clone();
    b.edit(Stamp::new(None, T0), |t| t.set_layer_visible(&layer, false)).into_result().unwrap();

    let plain = b.snapshot();
    assert!(!plain.get(&mine).unwrap().visible);
    let mut labels = HashMap::new();
    labels.insert("pimble://node/1".to_string(), CardLabel { title: "Groceries".into(), color: Some(Color::rgb(0, 200, 0)) });
    let options = ViewOptions { color_by_author: true, show_all: true, card_labels: Arc::new(labels) };
    let seen = b.snapshot().with_options(options.clone(), &b);
    assert_eq!(seen.options(), &options);
    let m = seen.get(&mine).unwrap();
    assert!(m.visible, "show_all shows a hidden layer");
    assert_eq!(m.style.stroke, author_color("sam-sub"));
    assert_eq!(m.style.fill, Some(Color { a: 0x40, ..author_color("sam-sub") }));
    assert_ne!(m.fingerprint, plain.get(&mine).unwrap().fingerprint);
    let a = seen.get(&anon).unwrap();
    assert_eq!(a.style.stroke, UNAUTHORED_COLOR);
    assert_eq!(a.style.fill, Some(Color { a: 0x40, ..UNAUTHORED_COLOR }));
    let p = seen.get(&pic).unwrap();
    assert_eq!(p.style.stroke, plain.get(&pic).unwrap().style.stroke, "pictures keep their colors");
    let c = seen.get(&card).unwrap();
    assert_eq!(c.card.as_ref().map(|l| l.title.as_str()), Some("Groceries"));
    assert_ne!(c.layout, plain.get(&card).unwrap().layout, "the card shows the label's title");
    // The stored values are untouched, and an update keeps the options.
    assert!(!seen.layers[0].visible);
    let watch = b.watch().unwrap();
    b.edit(Stamp::new(None, T0), |t| t.set_text(&anon, "hello")).into_result().unwrap();
    let updated = seen.update(&b, &watch.take());
    assert_eq!(updated.options(), &options);
    assert!(updated.get(&mine).unwrap().visible);
    assert_eq!(author_color("sam-sub"), author_color("sam-sub"));
}

#[test]
fn set_options_re_renders_the_view() {
    let b = board();
    let s = sam();
    b.add_element(
        Stamp::new(Some(&s), T0),
        NewElement::new(Kind::Rect, Geom::Rect(Rect::new(-50.0, -50.0, 100.0, 100.0))).with_style(filled(Color::BLACK)),
    )
    .unwrap();
    let bg = Color::rgb(0xf8, 0xf9, 0xfa);
    let mut view = BoardView::new(200, 200, bg);
    view.set_snapshot(b.snapshot());
    let before = view.frame(&NoPictures).unwrap().to_vec();
    let at = |f: &[u8]| f[(100 * 200 + 100) * 4..(100 * 200 + 100) * 4 + 3].to_vec();
    assert_eq!(at(&before), vec![0, 0, 0]);
    view.set_options(ViewOptions { color_by_author: true, ..ViewOptions::default() }, &b);
    let after = view.frame(&NoPictures).unwrap().to_vec();
    assert_ne!(at(&after), vec![0, 0, 0]);
    // An update keeps them.
    let watch = b.watch().unwrap();
    b.add_element(Stamp::new(None, T0), NewElement::new(Kind::Rect, Geom::Rect(Rect::new(60.0, 60.0, 10.0, 10.0)))).unwrap();
    view.update(&b, &watch.take());
    assert!(view.snapshot().options().color_by_author);
}

#[test]
fn a_grid_draws_lines_under_the_elements() {
    for bg in [Color::rgb(0xf8, 0xf9, 0xfa), Color::rgb(0x1f, 0x20, 0x24)] {
        let gc = grid_color(bg);
        assert_ne!(gc, bg);
        let mut view = BoardView::new(200, 120, bg);
        view.set_snapshot(BoardSnapshot::empty());
        view.set_grid(Some(50.0));
        let frame = view.frame(&NoPictures).unwrap().to_vec();
        let px = |x: f32, y: f32| {
            let (sx, sy) = view.board_to_screen(Point::new(x, y));
            let i = ((sy.floor() as usize) * 200 + sx.floor() as usize) * 4;
            Color::rgb(frame[i], frame[i + 1], frame[i + 2])
        };
        assert_eq!(px(0.0, 10.0), gc, "a vertical line at x = 0");
        assert_eq!(px(50.0, 10.0), gc);
        assert_eq!(px(10.0, -50.0), gc, "a horizontal line at y = -50");
        assert_eq!(px(25.0, 10.0), bg, "background between lines");
        // Panning far enough to shift the cache keeps the lines in place.
        view.pan_by(-137.0, 0.0);
        let frame = view.frame(&NoPictures).unwrap().to_vec();
        let (sx, _) = view.board_to_screen(Point::new(200.0, 0.0));
        let i = (10 * 200 + sx.floor() as usize) * 4;
        assert_eq!(Color::rgb(frame[i], frame[i + 1], frame[i + 2]), gc);
        // Zoomed far out, the lines thin out instead of filling the view.
        view.set_camera(Point::default(), 0.05);
        view.settle(&NoPictures);
        let frame = view.current_frame(&NoPictures).to_vec();
        let grid_px = frame.chunks_exact(4).filter(|p| Color::rgb(p[0], p[1], p[2]) == gc).count();
        assert!(grid_px < 200 * 120 / 4, "{grid_px} grid pixels");
    }
}

#[test]
fn render_area_draws_only_the_chosen_elements_and_their_members() {
    let b = board();
    let frame = add(&b, NewElement::new(Kind::Frame, Geom::Rect(Rect::new(0.0, 0.0, 100.0, 100.0))));
    let member = add(
        &b,
        NewElement::new(Kind::Rect, Geom::Rect(Rect::new(20.0, 20.0, 30.0, 30.0)))
            .with_style(filled(Color::rgb(255, 0, 0)))
            .with_parent(frame.clone()),
    );
    let other = add(&b, NewElement::new(Kind::Rect, Geom::Rect(Rect::new(200.0, 0.0, 50.0, 50.0))).with_style(filled(Color::rgb(0, 0, 255))));
    let snap = b.snapshot();
    let all = Rect::new(-10.0, -10.0, 300.0, 200.0);
    let only: HashSet<String> = [frame.clone()].into_iter().collect();
    let r = render_area(&snap, &NoPictures, all, 1.0, None, Some(&only), false).unwrap();
    assert_eq!((r.image.width, r.image.height), (300, 200));
    let px = |img: &RgbaImage, rect: Rect, x: f32, y: f32| {
        let (ix, iy) = (((x - rect.x) * img.width as f32 / rect.w) as usize, ((y - rect.y) * img.height as f32 / rect.h) as usize);
        let i = (iy * img.width as usize + ix) * 4;
        [img.data[i], img.data[i + 1], img.data[i + 2], img.data[i + 3]]
    };
    assert_eq!(px(&r.image, r.rect, 35.0, 35.0), [255, 0, 0, 255], "the frame's member is drawn");
    assert_eq!(px(&r.image, r.rect, 225.0, 25.0)[3], 0, "the other element is not");
    // Trimmed, on a background nothing trims; trimmed on transparency, to the frame.
    let t = render_area(&snap, &NoPictures, all, 1.0, None, Some(&only), true).unwrap();
    assert!(t.rect.w < 110.0 && t.rect.h < 110.0, "{:?}", t.rect);
    let bgd = render_area(&snap, &NoPictures, all, 1.0, Some(Color::WHITE), None, true).unwrap();
    assert_eq!((bgd.image.width, bgd.image.height), (300, 200));
    assert_eq!(px(&bgd.image, bgd.rect, 225.0, 25.0), [0, 0, 255, 255]);
    // The selection's bounds.
    let sel = selection_bounds(&snap, &[member.clone(), other.clone()]).unwrap();
    assert!(sel.x <= 20.0 && sel.right() >= 250.0);
    assert!(selection_bounds(&snap, &["nothing".to_string()]).is_none());
    // A huge scale is capped.
    let big = render_area(&snap, &NoPictures, all, 1000.0, None, None, false).unwrap();
    assert!(big.image.width as f32 <= MAX_EXTENT_PX + 1.0);
    // JPEG, flattened.
    let jpeg = t.image.encode_jpeg(85, Color::WHITE).unwrap();
    assert_eq!(&jpeg[..3], &[0xff, 0xd8, 0xff]);
    // render_extent still works.
    assert!(render_extent(&snap, &NoPictures, 1.0).is_some());
}

/// Tags open and close in order (a well-formedness check without an XML
/// parser).
fn balanced(svg: &str) -> bool {
    let mut stack: Vec<String> = Vec::new();
    let mut rest = svg;
    while let Some(start) = rest.find('<') {
        let Some(end) = rest[start..].find('>') else { return false };
        let tag = &rest[start + 1..start + end];
        rest = &rest[start + end + 1..];
        if tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        if let Some(name) = tag.strip_prefix('/') {
            if stack.pop().as_deref() != Some(name.trim()) {
                return false;
            }
        } else if !tag.ends_with('/') {
            stack.push(tag.split_whitespace().next().unwrap_or("").to_string());
        }
    }
    stack.is_empty()
}

#[test]
fn an_svg_holds_every_element_as_paths_and_pictures_as_data_uris() {
    let b = board();
    let png = RgbaImage { width: 2, height: 2, data: vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255] }
        .encode_png()
        .unwrap();
    add(
        &b,
        NewElement::new(
            Kind::Picture,
            Geom::Picture(PictureGeom {
                rect: Rect::new(0.0, 0.0, 200.0, 200.0),
                src: "pimble-blob:pic".into(),
                px_w: 2,
                px_h: 2,
                crop: Some(Rect::new(0.0, 0.0, 1.0, 2.0)),
            }),
        ),
    );
    add(&b, NewElement::new(Kind::Rect, Geom::Rect(Rect::new(10.0, 10.0, 80.0, 40.0))).with_style(Style { dash: Some(Dash::Dashed), ..Style::default() }));
    add(&b, NewElement::new(Kind::Arrow, Geom::Segment { a: Point::new(0.0, 100.0), b: Point::new(150.0, 150.0) }));
    add(&b, NewElement::new(Kind::Text, Geom::Rect(Rect::new(20.0, 160.0, 0.0, 0.0))).with_text("<script>\"&'"));
    add(&b, NewElement::stroke(Kind::Highlighter, vec![[0.0, 0.0, 0.5], [50.0, 10.0, 0.5]]));
    add(&b, NewElement::new(Kind::Rect, Geom::Rect(Rect::new(400.0, 400.0, 10.0, 10.0))).with_transform(Transform { rot: 30.0, ..Transform::IDENTITY }));
    add(
        &b,
        NewElement::new(
            Kind::Picture,
            Geom::Picture(PictureGeom { rect: Rect::new(300.0, 0.0, 50.0, 50.0), src: "pimble-blob:missing".into(), px_w: 4, px_h: 4, crop: None }),
        ),
    );
    let snap = b.snapshot();
    let bytes = png.clone();
    let svg = render_svg(&snap, Rect::new(0.0, 0.0, 500.0, 500.0), None, &move |src: &str| (src == "pimble-blob:pic").then(|| bytes.clone()));
    assert!(svg.starts_with("<svg"));
    assert!(svg.contains("viewBox=\"0 0 500 500\""));
    assert!(svg.contains("width=\"500\""));
    assert!(svg.contains("<image"));
    assert!(svg.contains("href=\"data:image/png;base64,iVBOR"), "a base64 PNG data URI");
    assert!(svg.contains("viewBox=\"0 0 1 2\""), "the crop");
    assert!(svg.contains("stroke-dasharray"));
    assert!(svg.contains("mix-blend-mode:multiply"));
    assert!(svg.contains("matrix("), "a rotated element carries its transform");
    assert!(svg.contains("fill=\"#e0e0e0\""), "a missing picture's placeholder");
    assert!(!svg.contains("script"), "text is outlines, never text");
    // A path for the rect, the arrow (line and head), the text, the
    // highlighter and the rotated rect.
    assert!(svg.matches("<path ").count() >= 6, "{svg}");
    assert!(balanced(&svg), "{svg}");
    // `only` leaves the others out.
    let id = snap.elements.iter().find(|e| e.kind() == &Kind::Arrow).unwrap().id().to_string();
    let only: HashSet<String> = [id].into_iter().collect();
    let svg = render_svg(&snap, Rect::new(0.0, 0.0, 500.0, 500.0), Some(&only), &|_: &str| None);
    assert!(!svg.contains("<image") && svg.matches("<path ").count() == 2, "{svg}");
    assert!(balanced(&svg));
}

/// A text's descenders hang below its line box; a render cut to what the
/// canvas shows keeps them (found sketching through the MCP, 2026-10-29: "the
/// logo" lost the bottom of its g).
#[test]
fn a_render_keeps_descenders_below_a_text_box() {
    let b = board();
    let id = add(&b, NewElement::new(Kind::Text, Geom::Rect(Rect::new(0.0, 0.0, 0.0, 0.0))).with_text("gyp"));
    let snap = b.snapshot();
    let e = snap.get(&id).unwrap();
    let tb = e.text_box.unwrap();
    assert!(e.bounds.bottom() > tb.bottom() + 1.0, "bounds {:?} box {:?}", e.bounds, tb);
    let rendered = render_extent(&snap, &NoPictures, 2.0).unwrap();
    assert!(rendered.rect.bottom() > tb.bottom() + 1.0, "the render reaches below the box: {:?}", rendered.rect);
}

/// A callout's pointer and box fill as one, whichever way the pointer points
/// (before 2026-10-29 the two were wound apart and the pointer's outline
/// showed through the box), and a pen with a fill is a filled shape.
#[test]
fn a_callout_box_covers_its_pointer_and_a_filled_pen_is_filled() {
    for tip in [Point::new(150.0, -100.0), Point::new(-100.0, 300.0)] {
        let b = board();
        let style = Style { fill: Some(Color::WHITE), stroke: Some(Color::BLACK), width: Some(3.0), ..Style::default() };
        add(&b, NewElement::new(Kind::Callout, Geom::Callout { rect: Rect::new(0.0, 0.0, 300.0, 100.0), tip }).with_style(style));
        let snap = b.snapshot();
        let r = render_area(&snap, &NoPictures, Rect::new(-120.0, -120.0, 540.0, 440.0), 1.0, Some(Color::WHITE), None, false).unwrap();
        // Inside the box, away from its edge, everything is the box's white.
        let px = |x: f32, y: f32| {
            let (ix, iy) = ((x + 120.0) as u32, (y + 120.0) as u32);
            let i = ((iy * r.image.width + ix) * 4) as usize;
            [r.image.data[i], r.image.data[i + 1], r.image.data[i + 2]]
        };
        for (x, y) in [(150.0, 50.0), (140.0, 40.0), (160.0, 60.0), (120.0, 50.0), (180.0, 50.0)] {
            assert_eq!(px(x, y), [255, 255, 255], "inside the box at {x},{y} for tip {tip:?}");
        }
    }
    let b = board();
    let pts = vec![[0.0, 0.0, 0.5], [100.0, 0.0, 0.5], [100.0, 100.0, 0.5], [0.0, 100.0, 0.5], [0.0, 0.0, 0.5]];
    let style = Style { fill: Some(Color::rgb(0, 200, 0)), ..Style::default() };
    let id = add(&b, NewElement::stroke(Kind::Pen, pts).with_style(style));
    let snap = b.snapshot();
    let r = render_area(&snap, &NoPictures, Rect::new(0.0, 0.0, 100.0, 100.0), 1.0, Some(Color::WHITE), None, false).unwrap();
    let i = ((50 * r.image.width + 50) * 4) as usize;
    assert_eq!(&r.image.data[i..i + 3], &[0, 200, 0], "the middle is filled");
    assert!(snap.get(&id).unwrap().hits(Point::new(50.0, 50.0), 0.0), "and hit inside");
}
