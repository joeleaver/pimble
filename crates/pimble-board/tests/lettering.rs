//! Map lettering (docs/DRAWING_WAVE6_CONTRACT.md section 4): bundled fonts
//! (unknown names read as Noto Sans Regular), halos, letter spacing, and
//! text set along a path (glyphs follow it, moving it moves them, naming
//! nothing or a cycle reads as plain text), SVG export, hit testing, the
//! raster cache, and two people editing at once.

use pimble_board::*;
use pimble_crdt::NodeDoc;

const T: &str = "2026-11-18T10:00:00.000Z";
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

fn pixels(snap: &BoardSnapshot, w: u32, h: u32, bg: Color) -> Vec<u8> {
    render(snap, &NoPictures, &View::new(0.0, 0.0, 1.0), w, h, Some(bg)).unwrap().take()
}

fn count(px: &[u8], f: impl Fn([u8; 4]) -> bool) -> usize {
    px.chunks_exact(4).filter(|p| f([p[0], p[1], p[2], p[3]])).count()
}

fn text(at: Point, s: &str, style: Style) -> NewElement {
    NewElement::new(Kind::Text, Geom::Rect(Rect::new(at.x, at.y, 0.0, 0.0))).with_text(s).with_style(style)
}

fn one_text(s: &str, style: Style) -> (Board, String) {
    let board = Board::standalone();
    board.create(st()).unwrap();
    let id = ok(&board, |t| t.add_element(text(Point::new(10.0, 10.0), s, style)));
    (board, id)
}

fn width_of(style: Style) -> f32 {
    let (board, id) = one_text("Mill Creek", Style { font_size: Some(40.0), ..style });
    board.snapshot().get(&id).unwrap().layout.as_ref().unwrap().width
}

#[test]
fn fonts_are_chosen_by_key_and_an_unknown_one_reads_as_noto_sans() {
    let plain = width_of(Style::default());
    let serif = width_of(Style { font: Some(FontFamily::Serif), ..Style::default() });
    let bold = width_of(Style { bold: Some(true), ..Style::default() });
    let condensed = width_of(Style { font: Some(FontFamily::Condensed), ..Style::default() });
    assert_ne!(serif, plain);
    assert!(bold > plain && condensed < plain * 0.9, "{plain} {bold} {condensed}");

    // A name this version does not know (written by a newer app) reads as
    // today's Noto Sans Regular, pixel for pixel.
    let (board, id) = one_text("Mill Creek", Style { font_size: Some(40.0), ..Style::default() });
    let before = pixels(&board.snapshot(), 300, 80, Color::WHITE);
    ok(&board, |t| t.set_style(&id, &Style { font: None, ..Style::default() }));
    raw_style(&board, &id, "font", "fraktur");
    let snap = board.snapshot();
    assert_eq!(snap.get(&id).unwrap().raw.style.font, None);
    assert_eq!(snap.get(&id).unwrap().style.text.font, FontChoice::default());
    assert_eq!(pixels(&snap, 300, 80, Color::WHITE), before);
}

/// Write `value` under `key` in element `id`'s style map as another app
/// would, bypassing the typed setters.
fn raw_style(board: &Board, id: &str, key: &str, value: &str) {
    use yrs::{Map, Out, Transact};
    let doc = board.doc().clone();
    let data = doc.get_or_insert_map(pimble_crdt::ROOT_DATA);
    let mut txn = doc.transact_mut();
    let Some(Out::YMap(b)) = data.get(&txn, "board") else { panic!("no board") };
    let Some(Out::YMap(els)) = b.get(&txn, "elements") else { panic!("no elements") };
    let Some(Out::YMap(el)) = els.get(&txn, id) else { panic!("no element") };
    let Some(Out::YMap(style)) = el.get(&txn, "style") else { panic!("no style") };
    style.insert(&mut txn, key, value.to_string());
}

#[test]
fn every_font_choice_round_trips_through_the_document() {
    for family in FontFamily::ALL {
        for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
            let style = Style { font: Some(family), bold: Some(bold), italic: Some(italic), tracking: Some(0.2), halo_color: Some(Color::WHITE), halo_width: Some(3.0), ..Style::default() };
            let (board, id) = one_text("Ashby", style.clone());
            let snap = board.snapshot();
            let e = snap.get(&id).unwrap();
            assert_eq!(e.raw.style.font, Some(family));
            assert_eq!(e.raw.style.bold, Some(bold));
            assert_eq!(e.raw.style.italic, Some(italic));
            assert_eq!(e.style.text.font, FontChoice { family, bold, italic });
            assert_eq!(e.style.text.tracking, 0.2);
            assert_eq!(e.style.halo, Some(Halo { color: Color::WHITE, width: 3.0 }));
        }
    }
}

#[test]
fn a_halo_strokes_the_glyphs_behind_their_fill() {
    // Dark text on a dark ground: with a white halo, white pixels ring the
    // glyphs, and the ink is still there on top.
    let ground = Color::rgb(30, 30, 30);
    let base = Style { font_size: Some(40.0), stroke: Some(INK), ..Style::default() };
    let (board, id) = one_text("Ashby", base.clone());
    let plain = pixels(&board.snapshot(), 200, 80, ground);
    let white = |p: [u8; 4]| p[0] > 240 && p[1] > 240 && p[2] > 240;
    let ink = |p: [u8; 4]| p[2] > 150 && p[0] < 80;
    assert_eq!(count(&plain, white), 0);
    let inked = count(&plain, ink);
    assert!(inked > 100);
    ok(&board, |t| t.set_style(&id, &Style { halo_color: Some(Color::WHITE), halo_width: Some(3.0), ..Style::default() }));
    let snap = board.snapshot();
    let haloed = pixels(&snap, 200, 80, ground);
    assert!(count(&haloed, white) > 200, "the halo shows");
    // The fill stays on top (antialiased edges aside).
    assert!(count(&haloed, ink) as f32 > inked as f32 * 0.8);
    // The bounds reach the halo.
    let e = snap.get(&id).unwrap();
    assert!(e.bounds.x <= 10.0 - 3.0 + 0.01, "{:?}", e.bounds);
    // A color alone gives a tenth of the font size; a width of 0 none.
    let (b2, id2) = one_text("A", Style { font_size: Some(40.0), halo_color: Some(Color::BLACK), ..Style::default() });
    assert_eq!(b2.snapshot().get(&id2).unwrap().style.halo, Some(Halo { color: Color::BLACK, width: 4.0 }));
    let (b3, id3) = one_text("A", Style { halo_width: Some(0.0), ..Style::default() });
    assert_eq!(b3.snapshot().get(&id3).unwrap().style.halo, None);
}

#[test]
fn a_halo_on_shape_and_sticky_text_too() {
    let board = Board::standalone();
    board.create(st()).unwrap();
    let halo = Style { halo_color: Some(Color::rgb(0, 200, 0)), halo_width: Some(2.0), ..Style::default() };
    let (rect, sticky) = ok(&board, |t| {
        let rect = t.add_element(NewElement::new(Kind::Rect, Geom::Rect(Rect::new(0.0, 0.0, 120.0, 60.0))).with_text("Mill").with_style(halo.clone()))?;
        let sticky = t.add_element(NewElement::new(Kind::Sticky, Geom::Rect(Rect::new(0.0, 80.0, 120.0, 60.0))).with_text("Mill").with_style(halo.clone()))?;
        Ok((rect, sticky))
    });
    let px = pixels(&board.snapshot(), 140, 160, Color::WHITE);
    let green = |p: [u8; 4]| p[1] > 150 && p[0] < 80 && p[2] < 80;
    let w = 140usize;
    let green_rows = |y0: usize, y1: usize| (y0..y1).flat_map(|y| (0..w).map(move |x| (x, y))).filter(|(x, y)| {
        let i = (y * w + x) * 4;
        green([px[i], px[i + 1], px[i + 2], px[i + 3]])
    }).count();
    assert!(green_rows(0, 60) > 30, "shape text haloed");
    assert!(green_rows(80, 140) > 30, "sticky text haloed");
    let _ = (rect, sticky);
}

#[test]
fn tracking_widens_measuring_wrapping_and_hit_testing() {
    let plain = width_of(Style::default());
    let wide = width_of(Style { tracking: Some(0.5), ..Style::default() });
    // "Mill Creek" has ten characters: nine gaps of half of 40.
    assert!((wide - plain - 9.0 * 20.0).abs() < 0.05, "{plain} {wide}");
    let (board, id) = one_text("Mill Creek", Style { font_size: Some(40.0), tracking: Some(0.5), ..Style::default() });
    let snap = board.snapshot();
    let e = snap.get(&id).unwrap();
    assert!((e.bounds.w - wide).abs() < 0.05);
    // The tracked-out end is hit; it was empty space untracked.
    let end = Point::new(10.0 + wide - 5.0, 30.0);
    assert!(e.hits(end, 0.5));
    let (b2, id2) = one_text("Mill Creek", Style { font_size: Some(40.0), ..Style::default() });
    assert!(!b2.snapshot().get(&id2).unwrap().hits(end, 0.5));
    // Out of range values are kept as stored and drawn clamped.
    let (b3, id3) = one_text("A", Style { tracking: Some(50.0), ..Style::default() });
    assert_eq!(b3.snapshot().get(&id3).unwrap().style.text.tracking, 2.0);
}

/// A canvas with a curved path from (20, 200) arching up to (380, 200) and
/// a text named "Braynard River" (centered) not yet on it.
fn river() -> (Board, String, String) {
    let board = Board::standalone();
    board.create(st()).unwrap();
    let (path, label) = ok(&board, |t| {
        let geom = PathGeom::from_anchors(
            vec![
                Anchor::new(Point::new(20.0, 200.0), Point::new(0.0, 0.0), Point::new(80.0, -120.0), AnchorMode::Corner),
                Anchor::new(Point::new(380.0, 200.0), Point::new(-80.0, -120.0), Point::new(0.0, 0.0), AnchorMode::Corner),
            ],
            false,
        );
        let path = t.add_element(NewElement::path(geom).with_style(Style { stroke: Some(Color::rgb(60, 120, 220)), width: Some(3.0), ..Style::default() }))?;
        let label = t.add_element(text(Point::new(500.0, 500.0), "Braynard River", Style { font_size: Some(24.0), align: Some(Align::Center), ..Style::default() }))?;
        Ok((path, label))
    });
    (board, path, label)
}

#[test]
fn text_on_a_path_follows_it() {
    let (board, path, label) = river();
    let before = board.snapshot();
    assert!(before.get(&label).unwrap().along.is_none());
    assert!(before.get(&label).unwrap().bounds.x >= 500.0);
    ok(&board, |t| t.put_on_path(&label, &path));
    let snap = board.snapshot();
    let e = snap.get(&label).unwrap();
    let placed = e.along.as_ref().expect("on the path");
    assert_eq!(placed.target, path);
    // The box it had is ignored: the glyphs are over the arch.
    assert!(e.bounds.x > 20.0 && e.bounds.right() < 380.0, "{:?}", e.bounds);
    assert!(e.bounds.bottom() < 205.0 && e.bounds.y > 60.0, "{:?}", e.bounds);
    // Glyphs turn with the curve: the first rises, the middle is level, the
    // last falls (left to right), and they sit above the line.
    let angle = |q: &[Point; 4]| (q[1].y - q[0].y).atan2(q[1].x - q[0].x).to_degrees();
    let quads = &placed.quads;
    let n = quads.len();
    assert_eq!(n, "Braynard River".chars().count());
    assert!(angle(&quads[0]) < -10.0, "{}", angle(&quads[0]));
    assert!(angle(&quads[n / 2]).abs() < 8.0, "{}", angle(&quads[n / 2]));
    assert!(angle(&quads[n - 1]) > 10.0, "{}", angle(&quads[n - 1]));
    // Centered (the default offset for centered text is the middle).
    let mid = (quads[0][0].x + quads[n - 1][1].x) / 2.0;
    assert!((mid - 200.0).abs() < 6.0, "{mid}");
    // Hit on its glyphs, not on the empty middle of its bounds.
    let g = quads[n / 2];
    let c = Point::new((g[0].x + g[2].x) / 2.0, (g[0].y + g[2].y) / 2.0);
    assert!(e.hits(c, 0.5));
    assert!(!e.hits(Point::new(200.0, 195.0 + 50.0), 0.5));

    // Moving the path moves the text.
    ok(&board, |t| t.set_transform(&path, &Transform::translate(0.0, 100.0)));
    let moved = board.snapshot();
    let m = moved.get(&label).unwrap();
    assert!((m.bounds.y - e.bounds.y - 100.0).abs() < 0.01, "{:?} {:?}", m.bounds, e.bounds);
    assert_ne!(m.fingerprint, e.fingerprint);

    // Reshaping it too: a straight line makes level glyphs.
    let straight = PathGeom::polyline(&[Point::new(20.0, 200.0), Point::new(380.0, 200.0)], false);
    ok(&board, |t| t.set_geom(&path, &Geom::Path(straight)));
    let level = board.snapshot();
    let q = &level.get(&label).unwrap().along.as_ref().unwrap().quads;
    assert!(q.iter().all(|q| angle(q).abs() < 0.01));
    // The baseline sits a quarter em above the line (y 300 after the move).
    let bottom_of_box = q[0][3].y; // descender line
    let (_, descender, _) = (0.0, 24.0 * 0.293, 0.0);
    assert!((bottom_of_box - descender - (300.0 - 6.0)).abs() < 0.5, "{bottom_of_box}");
}

#[test]
fn offset_side_and_reading_direction() {
    let board = Board::standalone();
    board.create(st()).unwrap();
    let (line, label) = ok(&board, |t| {
        let line = t.add_element(NewElement::new(Kind::Line, Geom::Segment { a: Point::new(0.0, 100.0), b: Point::new(400.0, 100.0) }))?;
        let label = t.add_element(text(Point::default(), "Road", Style { font_size: Some(20.0), ..Style::default() }))?;
        t.put_on_path(&label, &line)?;
        Ok((line, label))
    });
    let quads = |b: &Board| b.snapshot().get(&label).unwrap().along.as_ref().unwrap().quads.clone();
    // Left aligned, no offset: it starts at the line's start, above it.
    let q = quads(&board);
    assert!(q[0][0].x.abs() < 0.01);
    // The baseline a quarter em above the line: the box's foot is its
    // descender below that.
    assert!((q[0][3].y - (95.0 + 20.0 * 0.293)).abs() < 0.05, "{:?}", q[0]);
    // An offset of a half: it starts at the middle.
    ok(&board, |t| t.set_style(&label, &Style { along_offset: Some(0.5), ..Style::default() }));
    assert!((quads(&board)[0][0].x - 200.0).abs() < 0.01);
    // Below: the glyphs' tops under the line.
    ok(&board, |t| t.set_style(&label, &Style { along_side: Some(AlongSide::Below), ..Style::default() }));
    let q = quads(&board);
    assert!(q[0][0].y > 100.0 - 24.0 * 0.4 && q.iter().all(|q| q[2].y > 105.0), "{:?}", q[0]);
    // On: centered across it.
    ok(&board, |t| t.set_style(&label, &Style { along_side: Some(AlongSide::On), ..Style::default() }));
    let q = quads(&board);
    assert!(q[0][0].y < 100.0 && q[0][3].y > 100.0);
    // A line drawn right to left is read left to right: never upside down.
    ok(&board, |t| t.set_geom(&line, &Geom::Segment { a: Point::new(400.0, 100.0), b: Point::new(0.0, 100.0) }));
    ok(&board, |t| t.release_from_path(&label));
    ok(&board, |t| t.put_on_path(&label, &line));
    let q = quads(&board);
    assert!(q[1][0].x > q[0][0].x, "reads left to right");
    assert!(q[0][0].y < q[0][3].y, "upright");
}

#[test]
fn a_pen_stroke_or_a_line_in_a_group_carries_text_where_it_is_drawn() {
    let board = Board::standalone();
    board.create(st()).unwrap();
    let (pen, label) = ok(&board, |t| {
        let pts: Vec<[f32; 3]> = (0..=20).map(|i| [i as f32 * 20.0, 100.0 + (i as f32 * 0.5).sin() * 20.0, 0.5]).collect();
        let pen = t.add_element(NewElement::stroke(Kind::Pen, pts))?;
        let label = t.add_element(text(Point::default(), "Trail", Style::default()))?;
        t.put_on_path(&label, &pen)?;
        Ok((pen, label))
    });
    let a = board.snapshot().get(&label).unwrap().bounds;
    assert!(a.y > 40.0 && a.y < 130.0, "{a:?}");
    // In a group that moves, the stroke's world moves the text.
    let group = ok(&board, |t| {
        let g = t.add_element(NewElement::new(Kind::Group, Geom::None))?;
        t.set_parent(&pen, Some(&g))?;
        t.set_transform(&g, &Transform::translate(50.0, 0.0))?;
        Ok(g)
    });
    let b = board.snapshot().get(&label).unwrap().bounds;
    assert!((b.x - a.x - 50.0).abs() < 0.01, "{a:?} {b:?}");
    let _ = group;
}

#[test]
fn naming_nothing_or_what_is_not_a_line_reads_as_plain_text() {
    let (board, path, label) = river();
    let plain = board.snapshot().get(&label).unwrap().bounds;
    // Nothing by that id.
    ok(&board, |t| t.set_style(&label, &Style { along: Some("nope".into()), ..Style::default() }));
    let s = board.snapshot();
    assert!(s.get(&label).unwrap().along.is_none());
    assert_eq!(s.get(&label).unwrap().bounds, plain);
    // Itself (the only cycle a text can make: a text is never a line).
    ok(&board, |t| t.set_style(&label, &Style { along: Some(label.clone()), ..Style::default() }));
    assert!(board.snapshot().get(&label).unwrap().along.is_none());
    // Another text, or a shape.
    let (other, rect) = ok(&board, |t| {
        let other = t.add_element(text(Point::new(0.0, 0.0), "x", Style::default()))?;
        let rect = t.add_element(NewElement::new(Kind::Rect, Geom::Rect(Rect::new(0.0, 0.0, 10.0, 10.0))))?;
        Ok((other, rect))
    });
    for target in [&other, &rect] {
        ok(&board, |t| t.set_style(&label, &Style { along: Some(target.to_string()), ..Style::default() }));
        assert!(board.snapshot().get(&label).unwrap().along.is_none());
    }
    // Two texts each naming the other read as plain text both.
    ok(&board, |t| t.set_style(&other, &Style { along: Some(label.clone()), ..Style::default() }));
    ok(&board, |t| t.set_style(&label, &Style { along: Some(other.clone()), ..Style::default() }));
    let s = board.snapshot();
    assert!(s.get(&label).unwrap().along.is_none() && s.get(&other).unwrap().along.is_none());
    // The path deleted: plain again.
    ok(&board, |t| t.put_on_path(&label, &path));
    assert!(board.snapshot().get(&label).unwrap().along.is_some());
    ok(&board, |t| t.delete_element(&path));
    assert_eq!(board.snapshot().get(&label).unwrap().bounds, plain);
    // The command refuses with a sentence.
    let e = refused(&board, |t| t.put_on_path(&label, &rect));
    assert_eq!(e.to_string(), along::NOT_A_LINE);
    let e = refused(&board, |t| t.put_on_path(&rect, &other));
    assert_eq!(e.to_string(), along::NEEDS_TEXT_AND_PATH);
}

#[test]
fn release_from_path_puts_it_back_in_its_box() {
    let (board, path, label) = river();
    let plain = board.snapshot().get(&label).unwrap().bounds;
    ok(&board, |t| {
        t.put_on_path(&label, &path)?;
        t.set_style(&label, &Style { along_offset: Some(0.3), along_side: Some(AlongSide::Below), ..Style::default() })
    });
    ok(&board, |t| t.release_from_path(&label));
    let s = board.snapshot();
    let e = s.get(&label).unwrap();
    assert!(e.along.is_none());
    assert_eq!((e.raw.style.along.as_deref(), e.raw.style.along_offset, e.raw.style.along_side), (None, None, None));
    assert_eq!(e.bounds, plain);
}

#[test]
fn the_svg_export_draws_halos_and_curved_text_as_glyph_outlines() {
    let (board, path, label) = river();
    ok(&board, |t| {
        t.put_on_path(&label, &path)?;
        t.set_style(&label, &Style { halo_color: Some(Color::WHITE), halo_width: Some(2.0), font: Some(FontFamily::Serif), ..Style::default() })
    });
    let svg = render_svg(&board.snapshot(), Rect::new(0.0, 0.0, 400.0, 220.0), None, &|_| None);
    assert!(!svg.contains("<text") && !svg.contains("Braynard"), "no text nodes");
    assert!(svg.contains("stroke=\"#ffffff\""), "the halo stroke");
    assert!(svg.contains("stroke-width=\"4\""), "twice the halo width");
    assert!(svg.contains("stroke-linejoin=\"round\"") || !svg.contains("stroke-linejoin=\"miter\""));
    // The halo comes before the fill.
    let halo_at = svg.find("stroke=\"#ffffff\"").unwrap();
    let fill_at = svg.rfind("fill=\"#e53935\"").unwrap();
    assert!(halo_at < fill_at);
}

#[test]
fn the_raster_cache_redraws_text_when_its_path_moves() {
    let (board, path, label) = river();
    ok(&board, |t| t.put_on_path(&label, &path));
    let bg = Color::rgb(250, 250, 250);
    let mut view = BoardView::new(400, 400, bg);
    view.set_camera(Point::new(200.0, 200.0), 1.0);
    view.set_snapshot(board.snapshot());
    view.frame(&NoPictures);
    let w = board.watch().unwrap();
    w.take();
    ok(&board, |t| t.set_transform(&path, &Transform::translate(0.0, 120.0)));
    view.update(&board, &w.take());
    let frame = view.frame(&NoPictures).expect("redrawn").to_vec();
    let full = render(view.snapshot(), &NoPictures, &view.view(), 400, 400, Some(bg)).unwrap().take();
    let differing = frame.chunks_exact(4).zip(full.chunks_exact(4)).filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > 2)).count();
    assert_eq!(differing, 0);
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
fn two_people_lettering_at_once_keep_both_changes() {
    let mut a = node();
    let ba = Board::of_node(&a);
    ba.create(st()).unwrap();
    let id = ba.edit(st(), |t| t.add_element(text(Point::default(), "Ashby", Style::default()))).into_result().unwrap().0;
    let mut b = node();
    sync(&mut a, &mut b);
    let bb = Board::of_node(&b);
    // One makes it serif, the other bold and haloed, while apart.
    Board::of_node(&a).edit(st(), |t| t.set_style(&id, &Style { font: Some(FontFamily::Serif), ..Style::default() })).into_result().unwrap();
    bb.edit(st(), |t| t.set_style(&id, &Style { bold: Some(true), halo_width: Some(2.0), ..Style::default() })).into_result().unwrap();
    sync(&mut a, &mut b);
    for doc in [&a, &b] {
        let snap = Board::of_node(doc).snapshot();
        let e = snap.get(&id).unwrap();
        assert_eq!(e.style.text.font, FontChoice { family: FontFamily::Serif, bold: true, italic: false });
        assert_eq!(e.style.halo.map(|h| h.width), Some(2.0));
    }
}
