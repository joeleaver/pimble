//! The raster cache (contract spike 1): whatever path a frame took (dirty
//! rectangles re-rendered, a pan that shifted the cache, a settled zoom), it
//! shows what a full render of the same view shows.

use pimble_board::*;

const T0: &str = "2026-10-09T10:00:00.000Z";
const BG: Color = Color::rgb(250, 250, 250);

fn board_with_things() -> Board {
    let board = Board::standalone();
    let st = Stamp::new(None, T0);
    board.create(st).unwrap();
    board
        .edit(st, |t| {
            for i in 0..40 {
                let x = (i % 8) as f32 * 120.0 - 400.0;
                let y = (i / 8) as f32 * 120.0 - 300.0;
                t.add_element(
                    NewElement::new(Kind::Rect, Geom::Rect(Rect::new(x, y, 80.0, 60.0)))
                        .with_style(Style { fill: Some(Color::rgb((i * 6) as u8, 120, 200)), ..Style::default() }),
                )?;
            }
            t.add_element(NewElement::new(Kind::Arrow, Geom::Segment { a: Point::new(-300.0, -200.0), b: Point::new(300.0, 200.0) }))?;
            t.add_element(NewElement::new(Kind::Sticky, Geom::Rect(Rect::new(100.0, -250.0, 180.0, 120.0))).with_text("a sticky note that wraps"))?;
            t.add_element(NewElement::new(Kind::Step, Geom::Anchor(Point::new(0.0, 0.0))))?;
            Ok(())
        })
        .into_result()
        .unwrap();
    board
}

fn full(view: &BoardView, snap: &BoardSnapshot) -> Vec<u8> {
    let (w, h) = view.size();
    render(snap, &NoPictures, &view.view(), w, h, Some(BG)).unwrap().take()
}

/// Pixels that differ by more than a rounding step.
fn differing(a: &[u8], b: &[u8]) -> usize {
    a.chunks_exact(4).zip(b.chunks_exact(4)).filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > 2)).count()
}

#[test]
fn frames_are_opaque_at_the_requested_size_and_only_when_something_changed() {
    let board = board_with_things();
    let mut view = BoardView::new(320, 200, BG);
    view.set_snapshot(board.snapshot());
    let frame = view.frame(&NoPictures).unwrap().to_vec();
    assert_eq!(frame.len(), 320 * 200 * 4);
    assert!(frame.chunks_exact(4).all(|p| p[3] == 255));
    assert!(view.frame(&NoPictures).is_none(), "nothing changed");
    assert_eq!(differing(&frame, &full(&view, &board.snapshot())), 0);
}

#[test]
fn an_edit_re_renders_only_its_rectangle_and_matches_a_full_render() {
    let board = board_with_things();
    let st = Stamp::new(None, T0);
    let mut view = BoardView::new(640, 480, BG);
    view.set_snapshot(board.snapshot());
    view.frame(&NoPictures);
    // Move one rectangle, delete another, add one.
    let snap = board.snapshot();
    let a = snap.elements[3].id().to_string();
    let b = snap.elements[9].id().to_string();
    board
        .edit(st, |t| {
            t.set_transform(&a, &Transform::translate(37.0, 11.0))?;
            t.delete_element(&b)?;
            t.add_element(NewElement::new(Kind::Ellipse, Geom::Rect(Rect::new(-50.0, -50.0, 90.0, 70.0))))?;
            Ok(())
        })
        .into_result()
        .unwrap();
    let snap = board.snapshot();
    view.set_snapshot(board.snapshot());
    let frame = view.frame(&NoPictures).unwrap().to_vec();
    let d = differing(&frame, &full(&view, &snap));
    assert!(d < 40, "{d} pixels differ from a full render");
}

#[test]
fn panning_shifts_the_cache_and_a_zoom_settles() {
    let board = board_with_things();
    let snap = board.snapshot();
    let mut view = BoardView::new(400, 300, BG);
    view.set_snapshot(board.snapshot());
    view.frame(&NoPictures);
    // Within the margin, then well past it.
    view.pan_by(30.0, -20.0);
    let f = view.frame(&NoPictures).unwrap().to_vec();
    assert!(differing(&f, &full(&view, &snap)) < 40);
    view.pan_by(-380.0, 250.0);
    let f = view.frame(&NoPictures).unwrap().to_vec();
    let d = differing(&f, &full(&view, &snap));
    assert!(d < 60, "{d}");
    // A zoom shows the scaled cache until it settles.
    view.zoom_at(200.0, 150.0, 1.6);
    assert!(view.needs_settle());
    let scaled = view.frame(&NoPictures).unwrap().to_vec();
    assert_eq!(scaled.len(), 400 * 300 * 4);
    assert!(view.settle(&NoPictures));
    assert!(!view.needs_settle());
    let f = view.frame(&NoPictures).unwrap().to_vec();
    // The cache is drawn from a corner a whole number of pixels away from
    // the frame's; at a fractional zoom the float translation moves an
    // anti-aliased edge pixel or two by a rounding step.
    let d = differing(&f, &full(&view, &snap));
    assert!(d < 10, "{d}");
}

#[test]
fn a_stroke_is_stamped_segment_by_segment_and_then_rendered_from_the_document() {
    let board = Board::standalone();
    let st = Stamp::new(None, T0);
    board.create(st).unwrap();
    let mut view = BoardView::new(200, 200, BG);
    view.set_snapshot(board.snapshot());
    let blank = view.frame(&NoPictures).unwrap().to_vec();
    let style = Style { stroke: Some(Color::rgb(0, 0, 0)), width: Some(6.0), ..Style::default() };
    view.begin_stroke(Kind::Pen, &style, [-50.0, 0.0, 1.0]);
    let mut points = vec![[-50.0f32, 0.0, 1.0]];
    for i in 1..=10 {
        let p = [-50.0 + i as f32 * 10.0, (i as f32).sin() * 20.0, 1.0];
        view.extend_stroke(&[p]);
        points.push(p);
        let f = view.frame(&NoPictures).unwrap();
        assert_ne!(f, &blank[..]);
    }
    let ended = view.end_stroke();
    assert_eq!(ended, points);
    // The app writes the stroke; the document's rendering replaces the stamps.
    board.add_element(st, NewElement::stroke(Kind::Pen, ended).with_style(style.clone())).unwrap();
    let snap = board.snapshot();
    view.set_snapshot(board.snapshot());
    let f = view.frame(&NoPictures).unwrap().to_vec();
    assert_eq!(differing(&f, &full(&view, &snap)), 0);
    // A cancelled stroke leaves nothing behind.
    view.begin_stroke(Kind::Pen, &style, [0.0, 60.0, 1.0]);
    view.extend_stroke(&[[40.0, 60.0, 1.0]]);
    view.frame(&NoPictures);
    view.cancel_stroke();
    let f = view.frame(&NoPictures).unwrap().to_vec();
    assert_eq!(differing(&f, &full(&view, &snap)), 0);
}

#[test]
fn hiding_markup_re_renders_everything() {
    let board = board_with_things();
    let mut view = BoardView::new(300, 300, BG);
    view.set_snapshot(board.snapshot());
    view.frame(&NoPictures);
    board.edit(Stamp::new(None, T0), |t| t.set_markup_visible(false)).into_result().unwrap();
    view.set_snapshot(board.snapshot());
    let f = view.frame(&NoPictures).unwrap();
    assert!(f.chunks_exact(4).all(|p| p[..3] == [250, 250, 250]), "nothing but pictures shows, and there are none");
}
