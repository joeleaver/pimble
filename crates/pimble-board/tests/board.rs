//! The board document against the contract (docs/IMAGE_MARKUP_CONTRACT.md
//! sections 3, 5, 6 and 1's visible extent), through `NodeDoc` the way the
//! app and the server hold it: every write is an update of the node's
//! document that a peer merges with `NodeDoc::apply_update`.

use std::collections::HashMap;
use std::sync::Arc;

use pimble_board::*;
use pimble_crdt::NodeDoc;

const T0: &str = "2026-10-09T10:00:00.000Z";
const T1: &str = "2026-10-09T10:00:01.000Z";
const T2: &str = "2026-10-09T10:00:02.000Z";

fn sam() -> Author {
    Author::new("sub-sam", "sam")
}

fn joe() -> Author {
    Author::new("sub-joe", "joe")
}

fn node() -> NodeDoc {
    let mut doc = NodeDoc::new();
    doc.init("board", "Whiteboard", None, T0).unwrap();
    doc
}

/// A peer holding the same document.
fn peer(of: &NodeDoc) -> NodeDoc {
    NodeDoc::load(&of.save()).unwrap()
}

/// Exchange everything both ways.
fn sync(a: &mut NodeDoc, b: &mut NodeDoc) {
    let (asv, bsv) = (a.state_vector(), b.state_vector());
    let to_b = a.diff_since(&bsv).unwrap();
    let to_a = b.diff_since(&asv).unwrap();
    b.apply_update(&to_b).unwrap();
    a.apply_update(&to_a).unwrap();
}

fn rect_el(x: f32, y: f32, w: f32, h: f32) -> NewElement {
    NewElement::new(Kind::Rect, Geom::Rect(Rect::new(x, y, w, h)))
}

#[test]
fn a_new_board_has_one_layer_and_shows_its_markup() {
    let doc = node();
    let board = Board::of_node(&doc);
    assert!(!board.exists());
    assert!(!board.snapshot().exists);
    let (layer, update) = board.create(Stamp::new(Some(&sam()), T0)).unwrap();
    assert!(update.is_some());
    let snap = board.snapshot();
    assert!(snap.exists && snap.markup_visible && snap.page.is_none());
    assert_eq!(snap.layers.len(), 1);
    assert_eq!(snap.layers[0].id, layer);
    assert_eq!(snap.layers[0].name, "Layer 1");
    assert_eq!(snap.edited_by, Some(sam()));
    // Creating again writes nothing.
    let (again, update) = board.create(Stamp::new(None, T1)).unwrap();
    assert_eq!(again, layer);
    assert!(update.is_none());
    // It reports as a data change to a peer, as `applyEdit` sees it.
    let mut b = NodeDoc::new();
    let effect = b.apply_update(&doc.save()).unwrap();
    assert!(effect.data && effect.structure);
}

#[test]
fn every_kind_round_trips_through_the_node_document() {
    let doc = node();
    let board = Board::of_node(&doc);
    let author = sam();
    let stamp = Stamp::new(Some(&author), T0);
    board.create(stamp).unwrap();
    let picture = PictureGeom {
        rect: Rect::new(0.0, 0.0, 400.0, 300.0),
        src: "pimble-blob:abc".into(),
        px_w: 400,
        px_h: 300,
        crop: Some(Rect::new(10.0, 10.0, 380.0, 280.0)),
    };
    let edited = board.edit(stamp, |t| {
        let p = t.add_element(NewElement::new(Kind::Picture, Geom::Picture(picture.clone())))?;
        let pen = t.add_element(NewElement::stroke(Kind::Pen, vec![[1.0, 2.0, 0.5], [3.0, 4.0, 0.6]]))?;
        let arrow = t.add_element(
            NewElement::new(Kind::Arrow, Geom::Segment { a: Point::new(0.0, 0.0), b: Point::new(50.0, 60.0) })
                .with_style(Style { stroke: Some(Color::rgb(1, 2, 3)), width: Some(6.0), arrow_heads: Some(ArrowHeads::Both), ..Style::default() }),
        )?;
        let sticky = t.add_element(NewElement::new(Kind::Sticky, Geom::Rect(Rect::new(500.0, 0.0, 200.0, 200.0))).with_text("buy milk"))?;
        let step = t.add_element(NewElement::new(Kind::Step, Geom::Anchor(Point::new(30.0, 30.0))))?;
        let conn = t.add_element(
            NewElement::new(
                Kind::Connector,
                Geom::Connector {
                    from: End::Element { id: sticky.clone(), side: Some(Side::Left), last: Point::new(500.0, 100.0) },
                    to: End::Point(Point::new(300.0, 100.0)),
                },
            )
            .with_text("label"),
        )?;
        let card = t.add_element(NewElement::new(
            Kind::Card,
            Geom::Card { rect: Rect::new(0.0, 400.0, 240.0, 60.0), link: "pimble://store/node".into() },
        ))?;
        let callout = t.add_element(NewElement::new(
            Kind::Callout,
            Geom::Callout { rect: Rect::new(0.0, 500.0, 100.0, 50.0), tip: Point::new(150.0, 600.0) },
        ))?;
        t.set_transform(&sticky, &Transform { tx: 10.0, ty: 20.0, rot: 15.0, sx: 1.5, sy: 1.0 })?;
        Ok((p, pen, arrow, sticky, step, conn, card, callout))
    });
    let (p, pen, arrow, sticky, step, conn, card, callout) = edited.result.unwrap();
    let update = edited.update.unwrap();

    // A peer that only ever saw the update reads the same board.
    let mut other = NodeDoc::new();
    other.apply_update(&doc.save()).unwrap();
    assert!(other.apply_update(&update).unwrap() == pimble_crdt::NodeUpdateEffect::default(), "already merged");
    let snap = Board::of_node(&other).snapshot();
    assert_eq!(snap.elements.len(), 8);
    let get = |id: &str| snap.get(id).unwrap();
    assert_eq!(get(&p).raw.geom, Geom::Picture(picture.clone()));
    assert_eq!(get(&pen).raw.points, vec![[1.0, 2.0, 0.5], [3.0, 4.0, 0.6]]);
    assert_eq!(get(&arrow).raw.style.width, Some(6.0));
    assert_eq!(get(&arrow).style.arrow_heads, ArrowHeads::Both);
    assert_eq!(get(&arrow).style.stroke, Color::rgb(1, 2, 3));
    assert_eq!(get(&sticky).raw.text.as_deref(), Some("buy milk"));
    assert_eq!(get(&sticky).raw.transform, Transform { tx: 10.0, ty: 20.0, rot: 15.0, sx: 1.5, sy: 1.0 });
    assert_eq!(get(&step).step_number, Some(1));
    assert_eq!(get(&card).raw.kind, Kind::Card);
    assert_eq!(get(&callout).raw.kind, Kind::Callout);
    let route = get(&conn).route.unwrap();
    assert_eq!(route.1, Point::new(300.0, 100.0));
    // The sticky's left side, after its transform.
    assert!((route.0.x - get(&sticky).bounds.x).abs() < 1.0, "{route:?} {:?}", get(&sticky).bounds);
    for e in &snap.elements {
        assert_eq!(e.raw.author, Some(sam()));
        assert_eq!(e.raw.created_at, T0);
        assert!(e.visible, "{}", e.raw.kind);
    }
    // In z-order: the order they were added.
    let ids: Vec<&str> = snap.elements.iter().map(|e| e.id()).collect();
    assert_eq!(ids, vec![&p, &pen, &arrow, &sticky, &step, &conn, &card, &callout]);
    // The text on it, for search, from both sides.
    let texts: Vec<String> = snap.texts().into_iter().map(|t| t.2).collect();
    assert_eq!(texts, vec!["buy milk".to_string(), "label".to_string()]);
    let units = other.board_units();
    assert_eq!(units.len(), 2);
    assert!(units.iter().any(|u| u.text == "buy milk" && u.path == format!("f:board/{sticky}")));
}

#[test]
fn writes_stamp_who_and_when_only_when_someone_is_signed_in() {
    let doc = node();
    let board = Board::of_node(&doc);
    board.create(Stamp::new(None, T0)).unwrap();
    let (id, _) = board.add_element(Stamp::new(None, T0), rect_el(0.0, 0.0, 10.0, 10.0)).unwrap();
    let e = board.snapshot();
    let e = e.get(&id).unwrap();
    assert_eq!(e.raw.author, None);
    assert_eq!(e.raw.created_at, T0);
    assert_eq!(e.raw.edited_by, None);

    let j = joe();
    board.edit(Stamp::new(Some(&j), T1), |t| t.set_style(&id, &Style { fill: Some(Color::WHITE), ..Style::default() })).into_result().unwrap();
    let snap = board.snapshot();
    let e = snap.get(&id).unwrap();
    assert_eq!(e.raw.author, None, "signing in later does not rewrite who made it");
    assert_eq!(e.raw.edited_by, Some(joe()));
    assert_eq!(e.raw.edited_at.as_deref(), Some(T1));
    assert_eq!(snap.authors(), Vec::<Author>::new());
}

#[test]
fn different_properties_of_one_element_merge_and_the_same_property_converges() {
    let mut a = node();
    let s = sam();
    let j = joe();
    let (id, layer) = {
        let board = Board::of_node(&a);
        let (layer, _) = board.create(Stamp::new(Some(&s), T0)).unwrap();
        let (id, _) = board.add_element(Stamp::new(Some(&s), T0), rect_el(0.0, 0.0, 10.0, 10.0).with_text("hello world")).unwrap();
        (id, layer)
    };
    let mut b = peer(&a);

    // Different properties: color here, position there, text at both ends.
    Board::of_node(&a)
        .edit(Stamp::new(Some(&s), T1), |t| {
            t.set_style(&id, &Style { stroke: Some(Color::rgb(0, 0, 255)), ..Style::default() })?;
            t.set_text(&id, "oh hello world")
        })
        .into_result()
        .unwrap();
    Board::of_node(&b)
        .edit(Stamp::new(Some(&j), T1), |t| {
            t.set_style(&id, &Style { width: Some(9.0), ..Style::default() })?;
            t.set_transform(&id, &Transform::translate(100.0, 50.0))?;
            t.set_text(&id, "hello world!")
        })
        .into_result()
        .unwrap();
    sync(&mut a, &mut b);
    for doc in [&a, &b] {
        let snap = Board::of_node(doc).snapshot();
        let e = snap.get(&id).unwrap();
        assert_eq!(e.raw.style.stroke, Some(Color::rgb(0, 0, 255)));
        assert_eq!(e.raw.style.width, Some(9.0));
        assert_eq!(e.raw.transform, Transform::translate(100.0, 50.0));
        assert_eq!(e.raw.text.as_deref(), Some("oh hello world!"), "two people fixing one note keep both fixes");
        assert_eq!(e.layer, layer);
    }

    // The same property: two people dragging one rectangle end with one of
    // the two positions, never a mix, and both sides agree.
    Board::of_node(&a).edit(Stamp::new(Some(&s), T2), |t| t.set_transform(&id, &Transform::translate(1.0, 2.0))).into_result().unwrap();
    Board::of_node(&b).edit(Stamp::new(Some(&j), T2), |t| t.set_transform(&id, &Transform::translate(30.0, 40.0))).into_result().unwrap();
    sync(&mut a, &mut b);
    let ta = Board::of_node(&a).snapshot().get(&id).unwrap().raw.transform;
    let tb = Board::of_node(&b).snapshot().get(&id).unwrap().raw.transform;
    assert_eq!(ta, tb);
    assert!(ta == Transform::translate(1.0, 2.0) || ta == Transform::translate(30.0, 40.0), "{ta:?}");
    assert_eq!(Board::of_node(&a).snapshot().elements.len(), 1);
}

#[test]
fn concurrent_reorders_of_one_layer_never_duplicate_it() {
    let mut a = node();
    let s = sam();
    let board = Board::of_node(&a);
    let (l1, _) = board.create(Stamp::new(Some(&s), T0)).unwrap();
    let (l2, l3) = board
        .edit(Stamp::new(Some(&s), T0), |t| Ok((t.add_layer("Issues", Placement::Top)?, t.add_layer("Notes", Placement::Top)?)))
        .into_result()
        .unwrap()
        .0;
    let mut b = peer(&a);
    Board::of_node(&a).edit(Stamp::new(Some(&s), T1), |t| t.move_layer(&l3, Placement::Bottom)).into_result().unwrap();
    Board::of_node(&b).edit(Stamp::new(Some(&joe()), T1), |t| t.move_layer(&l3, Placement::Below(l2.clone()))).into_result().unwrap();
    // And the same layer moved concurrently with another layer's move.
    Board::of_node(&b).edit(Stamp::new(Some(&joe()), T1), |t| t.move_layer(&l1, Placement::Top)).into_result().unwrap();
    sync(&mut a, &mut b);
    let la: Vec<String> = Board::of_node(&a).snapshot().layers.iter().map(|l| l.id.clone()).collect();
    let lb: Vec<String> = Board::of_node(&b).snapshot().layers.iter().map(|l| l.id.clone()).collect();
    assert_eq!(la, lb);
    assert_eq!(la.len(), 3);
    let mut sorted = la.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), 3, "{la:?}");
    assert_eq!(la.last(), Some(&l1));
}

#[test]
fn two_people_adding_step_three_at_once_get_three_and_four() {
    let mut a = node();
    let s = sam();
    let board = Board::of_node(&a);
    board.create(Stamp::new(Some(&s), T0)).unwrap();
    for i in 0..2 {
        board.add_element(Stamp::new(Some(&s), T0), NewElement::new(Kind::Step, Geom::Anchor(Point::new(i as f32 * 50.0, 0.0)))).unwrap();
    }
    let mut b = peer(&a);
    let (sa, _) = Board::of_node(&a).add_element(Stamp::new(Some(&s), T1), NewElement::new(Kind::Step, Geom::Anchor(Point::new(200.0, 0.0)))).unwrap();
    let (sb, _) = Board::of_node(&b).add_element(Stamp::new(Some(&joe()), T1), NewElement::new(Kind::Step, Geom::Anchor(Point::new(300.0, 0.0)))).unwrap();
    // Before they meet, each sees its own as 3.
    assert_eq!(Board::of_node(&a).snapshot().get(&sa).unwrap().step_number, Some(3));
    assert_eq!(Board::of_node(&b).snapshot().get(&sb).unwrap().step_number, Some(3));
    sync(&mut a, &mut b);
    for doc in [&a, &b] {
        let snap = Board::of_node(doc).snapshot();
        let mut numbers: Vec<u32> = snap.elements.iter().filter_map(|e| e.step_number).collect();
        numbers.sort();
        assert_eq!(numbers, vec![1, 2, 3, 4]);
        let (na, nb) = (snap.get(&sa).unwrap().step_number.unwrap(), snap.get(&sb).unwrap().step_number.unwrap());
        assert_eq!(na.min(nb), 3);
        assert_eq!(na.max(nb), 4);
        // Ties on `created_at` break by id, the same way on both sides.
        assert_eq!(na < nb, sa < sb);
    }
    // Steps number per layer.
    let board = Board::of_node(&a);
    let layer = board.edit(Stamp::new(None, T2), |t| t.add_layer("Second", Placement::Top)).into_result().unwrap().0;
    let (other, _) = board.add_element(Stamp::new(None, T2), NewElement::new(Kind::Step, Geom::Anchor(Point::default())).in_layer(&layer)).unwrap();
    assert_eq!(board.snapshot().get(&other).unwrap().step_number, Some(1));
}

#[test]
fn a_stroke_grows_in_chunks_that_a_peer_sees_as_they_arrive() {
    let a = node();
    let board = Board::of_node(&a);
    board.create(Stamp::new(Some(&sam()), T0)).unwrap();
    let mut b = peer(&a);
    let (id, first) = board.add_element(Stamp::new(Some(&sam()), T0), NewElement::stroke(Kind::Highlighter, vec![[0.0, 0.0, 1.0]])).unwrap();
    b.apply_update(&first.unwrap()).unwrap();
    for k in 1..=5 {
        let pts: Vec<[f32; 3]> = (0..10).map(|i| [(k * 10 + i) as f32, 5.0, 0.5]).collect();
        let update = board.append_points(Stamp::new(Some(&sam()), T1), &id, &pts).unwrap().unwrap();
        b.apply_update(&update).unwrap();
        let seen = Board::of_node(&b).snapshot();
        assert_eq!(seen.get(&id).unwrap().raw.points.len(), 1 + k * 10);
    }
    let snap = Board::of_node(&b).snapshot();
    let e = snap.get(&id).unwrap();
    assert_eq!(e.raw.edited_by, None, "appending is drawing, not editing");
    // Bounds include half the width (highlighter default 24).
    assert_eq!(e.bounds.x, -12.0);
    // Smoothing on release replaces the chunks with one.
    let smooth = smooth_stroke(&e.raw.points, 2.0);
    board.edit(Stamp::new(Some(&sam()), T2), |t| t.replace_points(&id, &smooth)).into_result().unwrap();
    assert_eq!(board.snapshot().get(&id).unwrap().raw.points, smooth);
    assert!(board.edit(Stamp::new(None, T2), |t| t.append_points("nope", &[])).result.is_err());
    let (rect, _) = board.add_element(Stamp::new(None, T2), rect_el(0.0, 0.0, 1.0, 1.0)).unwrap();
    assert_eq!(board.edit(Stamp::new(None, T2), |t| t.append_points(&rect, &[[0.0, 0.0, 0.0]])).result, Err(BoardError::NotAStroke(rect)));
}

#[test]
fn the_visible_extent_follows_what_the_document_shows() {
    let doc = node();
    let board = Board::of_node(&doc);
    let (layer1, _) = board.create(Stamp::new(None, T0)).unwrap();
    assert_eq!(board.snapshot().visible_extent(), None);
    let picture = PictureGeom { rect: Rect::new(0.0, 0.0, 100.0, 80.0), src: "pimble-blob:p".into(), px_w: 100, px_h: 80, crop: None };
    let st = Stamp::new(None, T0);
    let (_pic, _) = board.add_element(st, NewElement::new(Kind::Picture, Geom::Picture(picture))).unwrap();
    assert_eq!(board.snapshot().visible_extent(), Some(Rect::new(0.0, 0.0, 100.0, 80.0)));
    // A line drawn in from beside the picture widens it, by half its width.
    let line = NewElement::new(Kind::Line, Geom::Segment { a: Point::new(-50.0, 40.0), b: Point::new(10.0, 40.0) })
        .with_style(Style { width: Some(10.0), ..Style::default() });
    board.add_element(st, line).unwrap();
    assert_eq!(board.snapshot().visible_extent(), Some(Rect::new(-55.0, 0.0, 155.0, 80.0)));
    // Hidden layers do not widen it.
    let hidden = board.edit(st, |t| t.add_layer("Hidden", Placement::Top)).into_result().unwrap().0;
    board.add_element(st, rect_el(1000.0, 1000.0, 10.0, 10.0).in_layer(&hidden)).unwrap();
    assert!(board.snapshot().visible_extent().unwrap().right() > 1000.0);
    board.edit(st, |t| t.set_layer_visible(&hidden, false)).into_result().unwrap();
    assert_eq!(board.snapshot().visible_extent(), Some(Rect::new(-55.0, 0.0, 155.0, 80.0)));
    // Markup hidden: only pictures.
    board.edit(st, |t| t.set_markup_visible(false)).into_result().unwrap();
    let snap = board.snapshot();
    assert_eq!(snap.visible_extent(), Some(Rect::new(0.0, 0.0, 100.0, 80.0)));
    assert_eq!(snap.markup_count(), 2);
    // A page wins over the extent.
    board.edit(st, |t| t.set_page(Some(&Page { rect: Rect::new(0.0, 0.0, 50.0, 50.0), fill: Some(Color::WHITE) }))).into_result().unwrap();
    assert_eq!(board.snapshot().render_rect(), Some(Rect::new(0.0, 0.0, 50.0, 50.0)));
    board.edit(st, |t| t.set_page(None)).into_result().unwrap();
    assert_eq!(board.snapshot().page, None);
    let _ = layer1;
}

#[test]
fn render_extent_trims_to_the_visible_pixels() {
    let doc = node();
    let board = Board::of_node(&doc);
    let st = Stamp::new(None, T0);
    board.create(st).unwrap();
    // A filled circle: its bounds include the stroke, its pixels are a disc.
    board
        .add_element(st, NewElement::new(Kind::Ellipse, Geom::Rect(Rect::new(10.0, 20.0, 100.0, 50.0))).with_style(Style {
            fill: Some(Color::rgb(0, 128, 0)),
            width: Some(0.0),
            ..Style::default()
        }))
        .unwrap();
    // A text element: bounds are the line box, pixels are the glyphs.
    board
        .add_element(st, NewElement::new(Kind::Text, Geom::Rect(Rect::new(10.0, 100.0, 0.0, 0.0))).with_text("Hi"))
        .unwrap();
    let snap = board.snapshot();
    let extent = snap.visible_extent().unwrap();
    let rendered = render_extent(&snap, &NoPictures, 1.0).unwrap();
    assert!(rendered.rect.x >= extent.x && rendered.rect.bottom() <= extent.bottom() + 1.0);
    assert!(rendered.rect.h < extent.h, "the text's empty line space is trimmed: {:?} vs {extent:?}", rendered.rect);
    let img = &rendered.image;
    assert_eq!((img.width as f32, img.height as f32), (rendered.rect.w, rendered.rect.h));
    // Every edge of the trimmed image has a visible pixel.
    let alpha = |x: u32, y: u32| img.data[((y * img.width + x) * 4 + 3) as usize];
    assert!((0..img.width).any(|x| alpha(x, 0) > 0));
    assert!((0..img.width).any(|x| alpha(x, img.height - 1) > 0));
    assert!((0..img.height).any(|y| alpha(0, y) > 0));
    assert!((0..img.height).any(|y| alpha(img.width - 1, y) > 0));
    assert!(img.encode_png().unwrap().starts_with(b"\x89PNG"));
    // Nothing visible renders as nothing.
    board.edit(st, |t| t.set_markup_visible(false)).into_result().unwrap();
    assert!(render_extent(&board.snapshot(), &NoPictures, 1.0).is_none());
}

#[test]
fn pictures_render_from_their_source_and_highlighter_multiplies() {
    let doc = node();
    let board = Board::of_node(&doc);
    let st = Stamp::new(None, T0);
    board.create(st).unwrap();
    let pic = Picture::from_rgba(4, 4, [200u8, 100, 50, 255].repeat(16)).unwrap();
    let mut pictures: HashMap<String, Arc<Picture>> = HashMap::new();
    pictures.insert("pimble-blob:x".into(), Arc::new(pic));
    let geom = PictureGeom { rect: Rect::new(0.0, 0.0, 40.0, 40.0), src: "pimble-blob:x".into(), px_w: 4, px_h: 4, crop: None };
    board.add_element(st, NewElement::new(Kind::Picture, Geom::Picture(geom))).unwrap();
    board
        .add_element(st, NewElement::stroke(Kind::Highlighter, vec![[0.0, 20.0, 1.0], [40.0, 20.0, 1.0]]).with_style(Style {
            stroke: Some(Color::rgb(255, 255, 0)),
            width: Some(10.0),
            ..Style::default()
        }))
        .unwrap();
    let snap = board.snapshot();
    let px = render(&snap, &pictures, &View::new(0.0, 0.0, 1.0), 40, 40, Some(Color::WHITE)).unwrap();
    let at = |x: u32, y: u32| {
        let p = px.pixel(x, y).unwrap();
        (p.red(), p.green(), p.blue())
    };
    assert_eq!(at(20, 5), (200, 100, 50), "the picture, scaled up");
    // Multiply: yellow keeps red and green, takes blue out.
    let (r, g, b) = at(20, 20);
    assert!(r >= 195 && g >= 95 && b < 10, "{:?}", (r, g, b));
    // One picture pixel per pixel is a straight copy.
    let pic = Picture::from_rgba(2, 1, vec![10, 20, 30, 255, 40, 50, 60, 255]).unwrap();
    let one: HashMap<String, Arc<Picture>> = [("pimble-blob:y".to_string(), Arc::new(pic))].into();
    let small = Board::standalone();
    small.create(st).unwrap();
    let geom = PictureGeom { rect: Rect::new(3.0, 2.0, 2.0, 1.0), src: "pimble-blob:y".into(), px_w: 2, px_h: 1, crop: None };
    small.add_element(st, NewElement::new(Kind::Picture, Geom::Picture(geom))).unwrap();
    let px1 = render(&small.snapshot(), &one, &View::new(0.0, 0.0, 1.0), 8, 4, Some(Color::WHITE)).unwrap();
    let p = px1.pixel(4, 2).unwrap();
    assert_eq!((p.red(), p.green(), p.blue()), (40, 50, 60));
    let p = px1.pixel(2, 2).unwrap();
    assert_eq!((p.red(), p.green(), p.blue()), (255, 255, 255));
    // A missing picture draws its placeholder, not nothing.
    let placeholder = render(&snap, &NoPictures, &View::new(0.0, 0.0, 1.0), 40, 40, Some(Color::WHITE)).unwrap();
    let p = placeholder.pixel(20, 5).unwrap();
    assert_eq!((p.red(), p.green(), p.blue()), (0xe0, 0xe0, 0xe0));
}

#[test]
fn layers_delete_with_their_elements_and_merge_down() {
    let doc = node();
    let board = Board::of_node(&doc);
    let st = Stamp::new(None, T0);
    let (l1, _) = board.create(st).unwrap();
    assert_eq!(board.edit(st, |t| t.delete_layer(&l1)).result, Err(BoardError::LastLayer));
    let l2 = board.edit(st, |t| t.add_layer("Two", Placement::Top)).into_result().unwrap().0;
    let (a, _) = board.add_element(st, rect_el(0.0, 0.0, 1.0, 1.0).in_layer(&l1)).unwrap();
    let (b, _) = board.add_element(st, rect_el(0.0, 0.0, 1.0, 1.0).in_layer(&l2)).unwrap();
    let (c, _) = board.add_element(st, rect_el(0.0, 0.0, 1.0, 1.0).in_layer(&l2)).unwrap();
    board.edit(st, |t| t.rename_layer(&l2, "Issues")).into_result().unwrap();
    assert_eq!(board.snapshot().layer(&l2).unwrap().name, "Issues");
    assert_eq!(board.snapshot().layer(&l2).unwrap().count, 2);
    board.edit(st, |t| t.merge_layer_down(&l2)).into_result().unwrap();
    let snap = board.snapshot();
    assert_eq!(snap.layers.len(), 1);
    let ids: Vec<&str> = snap.elements.iter().map(|e| e.id()).collect();
    assert_eq!(ids, vec![a.as_str(), b.as_str(), c.as_str()]);
    // Bring forward and send back.
    board.edit(st, |t| t.move_element(&a, Placement::Top)).into_result().unwrap();
    board.edit(st, |t| t.move_element(&c, Placement::Below(b.clone()))).into_result().unwrap();
    let ids: Vec<String> = board.snapshot().elements.iter().map(|e| e.id().to_string()).collect();
    assert_eq!(ids, vec![c.clone(), b.clone(), a.clone()]);
    let l3 = board.edit(st, |t| t.add_layer("Three", Placement::Bottom)).into_result().unwrap().0;
    board.edit(st, |t| t.move_to_layer(&[b.clone()], &l3)).into_result().unwrap();
    assert_eq!(board.snapshot().elements[0].id(), b);
    board.edit(st, |t| t.delete_layer(&l3)).into_result().unwrap();
    assert!(board.snapshot().get(&b).is_none());
    board.delete_element(st, &a).unwrap();
    assert_eq!(board.snapshot().elements.len(), 1);
    assert_eq!(board.delete_element(st, &a), Err(BoardError::NoElement(a)));
}

#[test]
fn an_element_added_to_a_layer_deleted_concurrently_still_shows() {
    let mut a = node();
    let st = Stamp::new(None, T0);
    let board = Board::of_node(&a);
    board.create(st).unwrap();
    let l2 = board.edit(st, |t| t.add_layer("Two", Placement::Top)).into_result().unwrap().0;
    let mut b = peer(&a);
    Board::of_node(&a).edit(st, |t| t.delete_layer(&l2)).into_result().unwrap();
    let (id, _) = Board::of_node(&b).add_element(st, rect_el(0.0, 0.0, 5.0, 5.0).in_layer(&l2)).unwrap();
    sync(&mut a, &mut b);
    let snap = Board::of_node(&a).snapshot();
    let e = snap.get(&id).unwrap();
    assert_eq!(e.layer, snap.top_layer().unwrap().id, "an orphan is drawn in the top layer");
    assert!(e.visible);
}

#[test]
fn members_move_with_their_frame_and_groups_take_their_members_extent() {
    let doc = node();
    let board = Board::of_node(&doc);
    let st = Stamp::new(None, T0);
    board.create(st).unwrap();
    let (frame, _) = board.add_element(st, NewElement::new(Kind::Frame, Geom::Rect(Rect::new(0.0, 0.0, 200.0, 200.0))).with_text("Ideas")).unwrap();
    let (member, _) = board.add_element(st, rect_el(10.0, 10.0, 20.0, 20.0).with_parent(&frame)).unwrap();
    let before = board.snapshot().get(&member).unwrap().bounds;
    board.edit(st, |t| t.set_transform(&frame, &Transform::translate(100.0, 0.0))).into_result().unwrap();
    let after = board.snapshot().get(&member).unwrap().bounds;
    assert_eq!(after.x, before.x + 100.0);
    // A group is as big as what it holds.
    let (group, _) = board.add_element(st, NewElement::new(Kind::Group, Geom::None)).unwrap();
    let (g1, _) = board.add_element(st, rect_el(500.0, 500.0, 10.0, 10.0).with_parent(&group)).unwrap();
    board.add_element(st, rect_el(600.0, 520.0, 10.0, 10.0).with_parent(&group)).unwrap();
    let snap = board.snapshot();
    let gb = snap.get(&group).unwrap().bounds;
    assert!(gb.x <= 498.0 && gb.right() >= 612.0, "{gb:?}");
    // A cycle reads as no parent; a parent that names nothing too.
    board.edit(st, |t| t.set_parent(&group, Some(&g1))).into_result().unwrap();
    let snap = board.snapshot();
    assert!(snap.get(&group).unwrap().parent.is_none() || snap.get(&g1).unwrap().parent.is_none());
    board.delete_element(st, &frame).unwrap();
    assert_eq!(board.snapshot().get(&member).unwrap().parent, None);
}

#[test]
fn undo_takes_back_my_edits_and_never_a_collaborators() {
    let mut mine = node();
    let me = joe();
    let st = Stamp::new(Some(&me), T0);
    Board::of_node(&mine).create(st).unwrap();
    let mut theirs = peer(&mine);
    let board = Board::of_node(&mine).with_origin("joe-device");
    let mut undo = board.undo_manager(yrs::undo::Options::default()).unwrap();
    let (my_rect, _) = board.add_element(st, rect_el(0.0, 0.0, 10.0, 10.0)).unwrap();
    let (their_rect, _) = Board::of_node(&theirs).add_element(Stamp::new(Some(&sam()), T1), rect_el(50.0, 0.0, 10.0, 10.0)).unwrap();
    sync(&mut mine, &mut theirs);
    assert_eq!(board.snapshot().elements.len(), 2);
    let update = board.undo(&mut undo, false).expect("something to undo");
    theirs.apply_update(&update).unwrap();
    for doc in [&mine, &theirs] {
        let snap = Board::of_node(doc).snapshot();
        assert!(snap.get(&my_rect).is_none());
        assert!(snap.get(&their_rect).is_some());
    }
    let update = board.undo(&mut undo, true).expect("something to redo");
    theirs.apply_update(&update).unwrap();
    assert_eq!(Board::of_node(&theirs).snapshot().elements.len(), 2);
}

#[test]
fn boards_of_pictures_in_text_live_under_their_own_keys_and_are_searchable() {
    let doc = node();
    let st = Stamp::new(None, T0);
    let first = Board::in_text(&doc, "b1");
    let second = Board::in_text(&doc, "b2");
    first.create(st).unwrap();
    second.create(st).unwrap();
    first.add_element(st, NewElement::new(Kind::Text, Geom::Rect(Rect::new(0.0, 0.0, 0.0, 0.0))).with_text("look here")).unwrap();
    second.add_element(st, rect_el(0.0, 0.0, 10.0, 10.0)).unwrap();
    assert_eq!(first.snapshot().elements.len(), 1);
    assert_eq!(second.snapshot().elements.len(), 1);
    assert!(!Board::of_node(&doc).exists());
    let units = doc.board_units();
    assert_eq!(units.len(), 1);
    assert!(units[0].path.starts_with("f:boards/b1/"));
    assert_eq!(units[0].text, "look here");
    assert!(doc.data_json()["board:b1"]["elements"].is_object());
}

/// What a snapshot brought up to date from a watch reads is what a fresh
/// read reads, for local and remote edits alike.
#[test]
fn a_watched_snapshot_updates_to_what_a_full_read_gives() {
    let mut a = node();
    let me = sam();
    let st = Stamp::new(Some(&me), T0);
    let board = Board::of_node(&a);
    board.create(st).unwrap();
    let ids: Vec<String> = board
        .edit(st, |t| {
            let mut ids = Vec::new();
            for i in 0..20 {
                ids.push(t.add_element(rect_el(i as f32 * 30.0, 0.0, 20.0, 20.0).with_text(format!("n{i}")))?);
            }
            ids.push(t.add_element(NewElement::new(Kind::Step, Geom::Anchor(Point::new(0.0, 100.0))))?);
            Ok(ids)
        })
        .into_result()
        .unwrap()
        .0;
    let mut b = peer(&a);
    let watch = board.watch().unwrap();
    watch.take();
    let mut snap = board.snapshot();

    let same = |x: &BoardSnapshot, y: &BoardSnapshot| {
        let fx: Vec<(String, u64)> = x.elements.iter().map(|e| (e.raw.id.clone(), e.fingerprint)).collect();
        let fy: Vec<(String, u64)> = y.elements.iter().map(|e| (e.raw.id.clone(), e.fingerprint)).collect();
        assert_eq!(fx, fy);
        assert_eq!(x.layers, y.layers);
        assert_eq!(x.markup_visible, y.markup_visible);
    };

    // A local edit.
    board.edit(st, |t| t.set_text(&ids[3], "changed")).into_result().unwrap();
    let changes = watch.take();
    assert_eq!(changes.elements.len(), 1);
    assert!(changes.elements.contains(&ids[3]));
    snap = snap.update(&board, &changes);
    same(&snap, &board.snapshot());
    assert_eq!(snap.get(&ids[3]).unwrap().raw.text.as_deref(), Some("changed"));

    // Remote edits: a delete, an add, a move, a layer, the markup toggle.
    let bb = Board::of_node(&b);
    let jo = joe();
    let stb = Stamp::new(Some(&jo), T1);
    bb.edit(stb, |t| {
        t.delete_element(&ids[0])?;
        t.add_element(NewElement::new(Kind::Step, Geom::Anchor(Point::new(50.0, 100.0))))?;
        t.set_transform(&ids[5], &Transform::translate(5.0, 5.0))?;
        let l = t.add_layer("New", Placement::Top)?;
        t.move_to_layer(&[ids[7].clone()], &l)?;
        t.set_markup_visible(false)
    })
    .into_result()
    .unwrap();
    sync(&mut a, &mut b);
    let changes = watch.take();
    assert!(changes.board);
    assert!(changes.elements.contains(&ids[0]) && changes.elements.contains(&ids[5]) && changes.elements.contains(&ids[7]));
    snap = snap.update(&Board::of_node(&a), &changes);
    same(&snap, &Board::of_node(&a).snapshot());
    assert!(snap.get(&ids[0]).is_none());
    assert_eq!(snap.elements.iter().filter(|e| e.step_number.is_some()).count(), 2);
    // Nothing changed: the same snapshot back.
    let changes = watch.take();
    assert!(changes.is_empty());
    let n = snap.elements.len();
    assert_eq!(snap.update(&board, &changes).elements.len(), n);
}

/// A picture's board copied to another node (a paste, "Convert to Image
/// Node"): the same layers and elements, who made them and when, the
/// markup switch, and points; the copy is a board of its own afterwards.
#[test]
fn a_board_copied_into_a_new_one_is_the_same_board_and_then_its_own() {
    let host = node();
    let marked = Board::in_text(&host, "b1");
    marked
        .edit(Stamp::new(Some(&sam()), T0), |t| {
            t.create()?;
            let picture = t.add_layer("Picture", Placement::Bottom)?;
            let geom = PictureGeom { rect: Rect::new(0.0, 0.0, 40.0, 30.0), src: "pimble-blob:x/y".into(), px_w: 40, px_h: 30, crop: None };
            t.add_element(NewElement::new(Kind::Picture, Geom::Picture(geom)).in_layer(picture.clone()))?;
            t.set_layer_locked(&picture, true)?;
            t.add_element(NewElement::new(Kind::Pen, Geom::Stroke).with_points(vec![[1.0, 2.0, 0.5], [3.0, 4.0, 0.7]]))?;
            let id = t.add_element(rect_el(5.0, 5.0, 10.0, 10.0).with_text("note"))?;
            t.set_transform(&id, &Transform { tx: 3.0, ..Transform::IDENTITY })?;
            t.set_markup_visible(false)
        })
        .into_result()
        .unwrap();
    let raw = marked.read().unwrap();

    let target = node();
    let copy = Board::of_node(&target);
    let (_, update) = copy.edit(Stamp::new(Some(&joe()), T1), |t| t.copy_from(&raw)).into_result().unwrap();
    assert!(update.is_some());
    let mut copied = copy.read().unwrap();
    let mut original = raw.clone();
    for board in [&mut copied, &mut original] {
        board.layers.sort_by(|a, b| a.id.cmp(&b.id));
        board.elements.sort_by(|a, b| a.id.cmp(&b.id));
    }
    assert_eq!(copied, original, "the same board, authors and times kept");
    assert!(copy.edit(Stamp::new(None, T2), |t| t.copy_from(&raw)).result.is_err(), "only into a new board");

    // Its own afterwards: a mark on the copy is not on the original.
    copy.edit(Stamp::new(Some(&joe()), T2), |t| t.add_element(rect_el(0.0, 0.0, 1.0, 1.0))).into_result().unwrap();
    assert_eq!(copy.read().unwrap().elements.len(), raw.elements.len() + 1);
    assert_eq!(marked.read().unwrap().elements.len(), raw.elements.len());
}

/// Two people marking up the first pictures of one note while apart both keep
/// their boards: each board is its own key of the data root, so neither
/// writes a container the other also made (a `data.boards` map made on first
/// use lost one side's boards).
#[test]
fn first_boards_in_text_made_apart_both_survive_the_merge() {
    let st = Stamp::new(None, T0);
    let mut mine = node();
    let mut theirs = peer(&mine);
    let a = Board::in_text(&mine, "mine");
    a.create(st).unwrap();
    a.add_element(st, rect_el(0.0, 0.0, 10.0, 10.0)).unwrap();
    let b = Board::in_text(&theirs, "theirs");
    b.create(st).unwrap();
    b.add_element(st, rect_el(5.0, 5.0, 10.0, 10.0)).unwrap();
    sync(&mut mine, &mut theirs);
    for doc in [&mine, &theirs] {
        assert_eq!(Board::in_text(doc, "mine").snapshot().elements.len(), 1);
        assert_eq!(Board::in_text(doc, "theirs").snapshot().elements.len(), 1);
    }
}
