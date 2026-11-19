//! Symbols and scatters (docs/DRAWING_WAVE6_CONTRACT.md section 2): the
//! stored keys and the packing, making a symbol from a selection, editing
//! it (every instance changes), drawing (instances, culling, tint, clips),
//! hit testing, the region fill, SVG export, the raster cache, two people at
//! once, and the 25,000-placement forest the map session needed.

use std::time::Instant;

use pimble_board::scatter::{self, FillMethod, Jitter, Placement, RegionFill};
use pimble_board::*;

const T: &str = "2026-11-18T10:00:00.000Z";
const GREEN: Color = Color::rgb(30, 140, 40);
const BROWN: Color = Color::rgb(110, 70, 30);

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

fn pixels(snap: &BoardSnapshot, view: View, w: u32, h: u32) -> Vec<u8> {
    render(snap, &NoPictures, &view, w, h, Some(Color::WHITE)).unwrap().take()
}

fn at(px: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [px[i], px[i + 1], px[i + 2], px[i + 3]]
}

fn is_green(p: [u8; 4]) -> bool {
    p[1] > 100 && p[0] < 90 && p[2] < 90
}

const WHITE: [u8; 4] = [255, 255, 255, 255];

/// A "tree": a green circle 20 across over a brown trunk, drawn around
/// (100, 100). Answers (board, layer, crown, trunk).
fn tree() -> (Board, String, String, String) {
    let board = Board::standalone();
    let layer = board.create(st()).unwrap().0;
    let (crown, trunk) = ok(&board, |t| {
        let trunk = t.add_element(filled(Kind::Rect, Rect::new(98.0, 100.0, 4.0, 10.0), BROWN).in_layer(&layer))?;
        let crown = t.add_element(filled(Kind::Ellipse, Rect::new(90.0, 85.0, 20.0, 20.0), GREEN).in_layer(&layer))?;
        Ok((crown, trunk))
    });
    (board, layer, crown, trunk)
}

/// The tree made a symbol: answers (board, layer, symbol, the instance).
fn tree_symbol() -> (Board, String, String, String) {
    let (board, layer, crown, trunk) = tree();
    let made = ok(&board, |t| t.make_symbol(&[crown, trunk], "Tree", false));
    (board, layer, made.symbol, made.instance.unwrap())
}

fn doc_bytes(board: &Board) -> usize {
    use yrs::{ReadTxn, StateVector, Transact};
    board.doc().transact().encode_state_as_update_v1(&StateVector::default()).len()
}

#[test]
fn making_a_symbol_replaces_the_selection_with_one_instance_where_it_was() {
    let (board, layer, crown, trunk) = tree();
    let before = pixels(&board.snapshot(), View::new(50.0, 50.0, 1.0), 100, 100);
    let made = ok(&board, |t| t.make_symbol(&[crown.clone(), trunk.clone()], "Tree", false));
    let snap = board.snapshot();
    // The elements went into the symbol; one scatter stands in for them.
    assert!(snap.get(&crown).is_none() && snap.get(&trunk).is_none());
    assert_eq!(snap.elements.len(), 1);
    let instance = snap.get(made.instance.as_deref().unwrap()).unwrap();
    assert_eq!(instance.raw.kind, Kind::Scatter);
    assert_eq!(instance.layer, layer);
    let raw = instance.raw.scatter.as_ref().unwrap();
    assert_eq!(raw.symbol, made.symbol);
    assert_eq!(raw.placements.len(), 1);
    assert_eq!((raw.placements[0].x, raw.placements[0].y), (100.0, 97.5), "the middle of what they covered");
    // The symbol: its name, its two elements in symbol space.
    let def = snap.symbol(&made.symbol).unwrap();
    assert_eq!(def.name, "Tree");
    assert_eq!(def.elements.len(), 2);
    let b = def.bounds.unwrap();
    assert!((b.center().x).abs() < 0.01 && (b.center().y).abs() < 0.01, "centered on its origin: {b:?}");
    // Drawn as it was.
    let after = pixels(&snap, View::new(50.0, 50.0, 1.0), 100, 100);
    let differing = before.chunks_exact(4).zip(after.chunks_exact(4)).filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > 40)).count();
    assert!(differing < 30, "{differing} pixels differ");
    assert!(is_green(at(&after, 100, 50, 40)));
    // Stored: one key `symbol:<id>` holding its name and elements; the
    // scatter's `symbol` key and its packed placements.
    {
        use yrs::{Map, Transact};
        let txn = board.doc().transact();
        let map = board.board_map(&txn).unwrap();
        let Some(yrs::Out::YMap(sym)) = map.get(&txn, &scatter::symbol_key(&made.symbol)) else { panic!("the symbol's key") };
        assert_eq!(sym.get(&txn, "name").map(|v| v.to_string(&txn)), Some("Tree".to_string()));
        let Some(yrs::Out::YMap(els)) = sym.get(&txn, "elements") else { panic!("its elements") };
        assert_eq!(els.len(&txn), 2);
        let Some(yrs::Out::YMap(elements)) = map.get(&txn, "elements") else { panic!("elements") };
        let Some(yrs::Out::YMap(e)) = elements.get(&txn, made.instance.as_deref().unwrap()) else { panic!("the scatter") };
        assert_eq!(e.get(&txn, "symbol").map(|v| v.to_string(&txn)), Some(made.symbol.clone()));
        assert_eq!(e.get(&txn, "kind").map(|v| v.to_string(&txn)), Some("scatter".to_string()));
    }
    // Kept: the elements stay, nothing is added.
    let (board, _, crown, trunk) = tree();
    let made = ok(&board, |t| t.make_symbol(&[crown.clone(), trunk], "Kept", true));
    assert!(made.instance.is_none());
    let snap = board.snapshot();
    assert_eq!(snap.elements.len(), 2);
    assert!(snap.get(&crown).is_some());
    assert_eq!(snap.symbols.len(), 1);
    // One undo step's worth: refused without elements.
    let err = refused(&board, |t| t.make_symbol(&[], "", false));
    assert_eq!(err.to_string(), scatter::NO_SYMBOL_ELEMENTS);
}

#[test]
fn placements_pack_into_sixteen_bytes_each() {
    let (board, layer, symbol, _) = tree_symbol();
    let base = doc_bytes(&board);
    let placements: Vec<Placement> = (0..1000).map(|i| Placement { x: i as f32, y: 2.0 * i as f32, scale: 1.25, rotation: 10.0, tint: Color::rgba(0, 0, 0, 40) }).collect();
    let id = ok(&board, |t| t.add_scatter(Some(&layer), &symbol, &placements, None));
    let grown = doc_bytes(&board) - base;
    assert!(grown < 1000 * 16 + 1500, "{grown} bytes for 1,000 placements");
    let snap = board.snapshot();
    let raw = snap.get(&id).unwrap().raw.scatter.clone().unwrap();
    assert_eq!(raw.placements.len(), 1000);
    assert_eq!(raw.chunk_ends.len(), 1000usize.div_ceil(scatter::CHUNK_PLACEMENTS));
    assert_eq!(raw.placements[999], placements[999].stored());
    // Appending: one more chunk.
    ok(&board, |t| t.append_placements(&id, &placements[..10]));
    let raw = board.snapshot().get(&id).unwrap().raw.scatter.clone().unwrap();
    assert_eq!((raw.placements.len(), raw.chunk_ends.len()), (1010, 5));
}

#[test]
fn every_placement_draws_the_symbol_tinted_and_only_where_the_view_is() {
    let (board, layer, symbol, instance) = tree_symbol();
    ok(&board, |t| t.delete_element(&instance));
    let placements = vec![
        Placement::at(20.0, 20.0),
        Placement { scale: 2.0, ..Placement::at(70.0, 30.0) },
        Placement { tint: Color::rgba(255, 0, 0, 255), ..Placement::at(20.0, 75.0) },
        Placement { rotation: 180.0, ..Placement::at(70.0, 75.0) },
    ];
    let id = ok(&board, |t| t.add_scatter(Some(&layer), &symbol, &placements, None));
    let snap = board.snapshot();
    let px = pixels(&snap, View::new(0.0, 0.0, 1.0), 100, 100);
    // The crown sits 5 units above the origin, the trunk below it.
    assert!(is_green(at(&px, 100, 20, 15)), "{:?}", at(&px, 100, 20, 15));
    assert!(is_green(at(&px, 100, 70, 20)), "twice the size reaches further: {:?}", at(&px, 100, 78, 20));
    assert!(is_green(at(&px, 100, 78, 22)));
    let red = at(&px, 100, 20, 70);
    assert!(red[0] > 200 && red[1] < 60, "tinted all the way red: {red:?}");
    assert!(is_green(at(&px, 100, 70, 80)), "turned over, the crown is below: {:?}", at(&px, 100, 70, 80));
    assert_eq!(at(&px, 100, 45, 50), WHITE, "between them nothing");
    // A view far away draws nothing of it; a view of one placement only it.
    let far = pixels(&snap, View::new(1000.0, 1000.0, 1.0), 50, 50);
    assert!(far.chunks_exact(4).all(|p| p == WHITE));
    let one = pixels(&snap, View::new(10.0, 5.0, 1.0), 20, 20);
    assert!(is_green(at(&one, 20, 10, 10)));
    // The scatter's own tint moves everything toward it; the bounds are the
    // placements'.
    ok(&board, |t| t.set_style(&id, &Style { tint: Some(Color::rgba(0, 0, 255, 255)), ..Style::default() }));
    let snap = board.snapshot();
    let p = at(&pixels(&snap, View::new(0.0, 0.0, 1.0), 100, 100), 100, 20, 15);
    assert!(p[2] > 200 && p[1] < 60, "blue all over: {p:?}");
    let e = snap.get(&id).unwrap();
    assert!(e.bounds.x < 12.0 && e.bounds.right() > 88.0 && e.bounds.y < 12.0, "{:?}", e.bounds);
}

#[test]
fn editing_the_symbol_changes_every_instance() {
    let (board, layer, symbol, instance) = tree_symbol();
    ok(&board, |t| t.append_placements(&instance, &[Placement::at(30.0, 30.0), Placement::at(160.0, 60.0)]));
    let snap = board.snapshot();
    let print = snap.get(&instance).unwrap().fingerprint;
    let crown = snap.symbol(&symbol).unwrap().elements.iter().find(|e| e.raw.kind == Kind::Ellipse).unwrap().raw.id.clone();
    // The crown recolored, through the ordinary style edit.
    ok(&board, |t| t.set_style(&crown, &Style { fill: Some(Color::rgb(200, 120, 0)), ..Style::default() }));
    let snap = board.snapshot();
    assert_ne!(snap.get(&instance).unwrap().fingerprint, print, "the raster cache redraws the instances");
    let px = pixels(&snap, View::new(0.0, 0.0, 1.0), 200, 120);
    for (x, y) in [(30, 25), (160, 55), (100, 92)] {
        let p = at(&px, 200, x, y);
        assert!(p[0] > 170 && p[1] > 90 && p[1] < 150 && p[2] < 60, "orange at ({x}, {y}): {p:?}");
    }
    // The isolated view shows the symbol's elements alone, in symbol space,
    // in a layer of its own; something drawn there goes into the symbol.
    let options = ViewOptions { editing_symbol: Some(symbol.clone()), ..ViewOptions::default() };
    let editing = BoardSnapshot::build_with(board.read(), options.clone());
    assert_eq!(editing.elements.len(), 2);
    assert_eq!(editing.layers.len(), 1);
    let pseudo = editing.layers[0].id.clone();
    assert_eq!(pseudo, scatter::symbol_key(&symbol));
    assert_eq!(editing.layer_for_new(None).unwrap().id, pseudo);
    let added = ok(&board, |t| t.add_element(filled(Kind::Rect, Rect::new(-2.0, -20.0, 4.0, 4.0), BROWN).in_layer(&pseudo)));
    assert_eq!(BoardSnapshot::build_with(board.read(), options).elements.len(), 3);
    let snap = board.snapshot();
    assert!(snap.get(&added).is_none(), "not on the canvas itself");
    assert_eq!(snap.symbol(&symbol).unwrap().elements.len(), 3);
    // Deleted from the symbol, moved in it: the ordinary edits.
    ok(&board, |t| t.set_transform(&added, &Transform { tx: 5.0, ..Transform::IDENTITY }));
    ok(&board, |t| t.delete_element(&added));
    assert_eq!(board.snapshot().symbol(&symbol).unwrap().elements.len(), 2);
    // A scatter cannot go into a symbol.
    let err = refused(&board, |t| t.make_symbol(&[instance.clone()], "", false));
    assert_eq!(err.to_string(), scatter::SCATTER_IN_SYMBOL);
    // Deleting the symbol leaves its scatters drawing nothing; undo-able by
    // the ordinary undo (here: not drawn, still stored).
    ok(&board, |t| t.delete_symbol(&symbol));
    let snap = board.snapshot();
    let e = snap.get(&instance).unwrap();
    assert!(e.scatter.as_ref().unwrap().def.is_none());
    assert!(pixels(&snap, View::new(0.0, 0.0, 1.0), 200, 120).chunks_exact(4).all(|p| p == WHITE));
    let _ = layer;
}

#[test]
fn a_scatter_is_hit_on_its_placements_and_not_between() {
    let (board, _, _, instance) = tree_symbol();
    ok(&board, |t| t.append_placements(&instance, &[Placement::at(200.0, 100.0)]));
    let snap = board.snapshot();
    assert_eq!(snap.hit_precise(Point::new(100.0, 92.0), 1.0).map(|e| e.raw.id.as_str()), Some(instance.as_str()));
    assert_eq!(snap.hit_precise(Point::new(200.0, 92.0), 1.0).map(|e| e.raw.id.as_str()), Some(instance.as_str()));
    assert!(snap.hit_precise(Point::new(150.0, 92.0), 1.0).is_none(), "between the trees");
    // A marquee around one tree takes the whole scatter's bounds into account.
    assert!(snap.get(&instance).unwrap().bounds.w > 100.0);
}

#[test]
fn a_region_fill_stays_inside_the_shape_and_is_the_same_for_the_same_seed() {
    let (board, layer, symbol, _) = tree_symbol();
    let ellipse = ok(&board, |t| t.add_element(filled(Kind::Ellipse, Rect::new(0.0, 0.0, 1000.0, 600.0), Color::rgb(200, 230, 200)).in_layer(&layer)));
    for method in [FillMethod::Poisson, FillMethod::Grid] {
        let fill = RegionFill { spacing: 25.0, method, seed: 42, jitter: Jitter { size_jitter: 0.2, rotation_jitter: 10.0, tint_jitter: 0.5, ..Jitter::default() }, ..RegionFill::default() };
        let id = ok(&board, |t| t.scatter_in_shape(&ellipse, &symbol, &fill, None));
        let snap = board.snapshot();
        let raw = snap.get(&id).unwrap().raw.scatter.clone().unwrap();
        let n = raw.placements.len();
        // An ellipse of about 471,000 square units at 25 apart.
        assert!((300..1000).contains(&n), "{method:?}: {n} placements");
        for p in &raw.placements {
            let (dx, dy) = ((p.x - 500.0) / 500.0, (p.y - 300.0) / 300.0);
            assert!(dx * dx + dy * dy <= 1.0 + 1e-3, "{method:?}: ({}, {}) outside", p.x, p.y);
        }
        if method == FillMethod::Poisson {
            // No two closer than the spacing (a few at a time, a grid check).
            for (i, a) in raw.placements.iter().enumerate().step_by(7) {
                for b in &raw.placements[i + 1..] {
                    assert!((a.x - b.x).hypot(a.y - b.y) >= 25.0 - 0.01);
                }
            }
        }
        // Just above the shape, in its layer.
        let order: Vec<&str> = snap.elements.iter().map(|e| e.raw.id.as_str()).collect();
        let (i, j) = (order.iter().position(|x| *x == ellipse).unwrap(), order.iter().position(|x| *x == id).unwrap());
        assert_eq!(j, i + 1, "{method:?}");
        // The same seed, the same placements.
        let again = ok(&board, |t| t.scatter_in_shape(&ellipse, &symbol, &fill, None));
        let snap = board.snapshot();
        assert_eq!(snap.get(&again).unwrap().raw.scatter.as_ref().unwrap().placements, raw.placements);
        ok(&board, |t| t.delete_elements(&[id.clone(), again.clone()]));
    }
    // An open line cannot be filled; nothing fits in a tiny shape.
    let line = ok(&board, |t| t.add_element(NewElement::new(Kind::Line, Geom::Segment { a: Point::new(0.0, 0.0), b: Point::new(50.0, 0.0) }).in_layer(&layer)));
    let err = refused(&board, |t| t.scatter_in_shape(&line, &symbol, &RegionFill::default(), None));
    assert_eq!(err.to_string(), scatter::NEEDS_CLOSED_SHAPE);
    let tiny = ok(&board, |t| t.add_element(filled(Kind::Rect, Rect::new(5000.0, 0.0, 0.0001, 0.0001), BROWN).in_layer(&layer)));
    assert!(refused(&board, |t| t.scatter_in_shape(&tiny, &symbol, &RegionFill::default(), None)).to_string().len() > 10);
}

#[test]
fn a_scatter_respects_its_layers_clip_and_blend() {
    let (board, layer, symbol, instance) = tree_symbol();
    ok(&board, |t| t.append_placements(&instance, &[Placement::at(30.0, 30.0)]));
    let clip = ok(&board, |t| {
        let clips = t.add_layer("Clips", pimble_board::Placement::Bottom)?;
        let r = t.add_element(filled(Kind::Rect, Rect::new(0.0, 0.0, 60.0, 60.0), Color::BLACK).in_layer(&clips))?;
        t.set_layer_visible(&clips, false)?;
        t.set_layer_clip(&layer, Some(&r))?;
        Ok(r)
    });
    let px = pixels(&board.snapshot(), View::new(0.0, 0.0, 1.0), 150, 150);
    assert!(is_green(at(&px, 150, 30, 25)), "inside the clip");
    assert_eq!(at(&px, 150, 100, 92), WHITE, "outside it");
    ok(&board, |t| t.set_layer_blend(&layer, Blend::Multiply));
    let px = pixels(&board.snapshot(), View::new(0.0, 0.0, 1.0), 150, 150);
    assert!(is_green(at(&px, 150, 30, 25)));
    let _ = (clip, symbol);
}

#[test]
fn svg_export_writes_the_symbol_once_and_a_use_per_placement() {
    let (board, layer, symbol, instance) = tree_symbol();
    ok(&board, |t| t.append_placements(&instance, &[Placement::at(30.0, 30.0), Placement { tint: Color::rgba(255, 0, 0, 255), ..Placement::at(160.0, 60.0) }]));
    let snap = board.snapshot();
    let svg = render_svg(&snap, Rect::new(0.0, 0.0, 200.0, 200.0), None, &|_| None);
    assert_eq!(svg.matches("<symbol ").count(), 2, "plain and tinted red: {svg}");
    assert_eq!(svg.matches("<use ").count(), 3);
    assert!(svg.contains(&format!("id=\"pimble-symbol-{symbol}\"")));
    assert!(svg.contains("#ff0000") || svg.contains("#fc0404") || svg.contains("fill=\"#f"), "the tinted symbol's colors moved toward red");
    // Only what the rectangle shows.
    let svg = render_svg(&snap, Rect::new(0.0, 0.0, 60.0, 60.0), None, &|_| None);
    assert_eq!(svg.matches("<use ").count(), 1);
    let _ = layer;
}

#[test]
fn the_raster_cache_matches_a_full_render_after_placements_are_appended() {
    let (board, _, _, instance) = tree_symbol();
    let bg = Color::rgb(250, 250, 250);
    let mut view = BoardView::new(200, 200, bg);
    view.set_camera(Point::new(100.0, 100.0), 1.0);
    view.set_snapshot(board.snapshot());
    view.frame(&NoPictures);
    let w = board.watch().unwrap();
    w.take();
    ok(&board, |t| t.append_placements(&instance, &[Placement::at(40.0, 40.0), Placement::at(160.0, 160.0)]));
    view.update(&board, &w.take());
    let frame = view.frame(&NoPictures).expect("redrawn").to_vec();
    let full = render(view.snapshot(), &NoPictures, &view.view(), 200, 200, Some(bg)).unwrap().take();
    let differing = frame.chunks_exact(4).zip(full.chunks_exact(4)).filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > 2)).count();
    assert_eq!(differing, 0);
    assert!(is_green(at(&frame, 200, 40, 35)));
    assert!(is_green(at(&frame, 200, 160, 155)));
}

#[test]
fn two_people_stamping_into_one_scatter_and_editing_its_symbol_merge() {
    use yrs::{ReadTxn, StateVector, Transact, Update, updates::decoder::Decode};
    let (a, _, symbol, instance) = tree_symbol();
    let b = Board::standalone();
    let sync = |from: &Board, to: &Board| {
        let sv = to.doc().transact().state_vector();
        let diff = from.doc().transact().encode_diff_v1(&sv);
        to.doc().transact_mut().apply_update(Update::decode_v1(&diff).unwrap()).unwrap();
    };
    sync(&a, &b);
    let crown = b.snapshot().symbol(&symbol).unwrap().elements.iter().find(|e| e.raw.kind == Kind::Ellipse).unwrap().raw.id.clone();
    ok(&a, |t| t.append_placements(&instance, &[Placement::at(10.0, 10.0)]));
    ok(&b, |t| t.append_placements(&instance, &[Placement::at(300.0, 10.0)]));
    ok(&b, |t| t.set_style(&crown, &Style { fill: Some(Color::rgb(0, 90, 0)), ..Style::default() }));
    sync(&a, &b);
    sync(&b, &a);
    let (sa, sb) = (a.snapshot(), b.snapshot());
    let (ea, eb) = (sa.get(&instance).unwrap(), sb.get(&instance).unwrap());
    assert_eq!(ea.raw.scatter.as_ref().unwrap().placements.len(), 3);
    assert_eq!(ea.raw, eb.raw);
    assert_eq!(ea.fingerprint, eb.fingerprint, "the same picture on both");
    let _ = StateVector::default();
}

#[test]
fn twenty_five_thousand_placements_are_one_small_element() {
    let (board, layer, symbol, _) = tree_symbol();
    let base = doc_bytes(&board);
    let frame = ok(&board, |t| t.add_element(filled(Kind::Polygon, Rect::new(0.0, 0.0, 11_800.0, 11_000.0), Color::rgb(230, 240, 220)).in_layer(&layer)));
    let fill = RegionFill { spacing: 62.0, method: FillMethod::Grid, position_jitter: 0.9, seed: 7, jitter: Jitter { size_jitter: 0.3, rotation_jitter: 15.0, tint_jitter: 0.6, ..Jitter::default() } };
    let started = Instant::now();
    let id = ok(&board, |t| t.scatter_in_shape(&frame, &symbol, &fill, None));
    let made = started.elapsed();
    let snap = board.snapshot();
    let n = snap.get(&id).unwrap().raw.scatter.as_ref().unwrap().placements.len();
    assert!((22_000..30_000).contains(&n), "{n}");
    let bytes = doc_bytes(&board) - base;
    assert!(bytes < 600_000, "{bytes} bytes for {n} placements");
    // Rendered: fitted to the whole map, then a view of a part, then again
    // (sprites cached).
    let extent = snap.get(&id).unwrap().bounds;
    let fitted = View::fit(&extent, 1920, 1080, 0.0);
    let t0 = Instant::now();
    render(&snap, &NoPictures, &fitted, 1920, 1080, Some(Color::WHITE)).unwrap();
    let cold = t0.elapsed();
    let t1 = Instant::now();
    let fitted_px = render(&snap, &NoPictures, &fitted, 1920, 1080, Some(Color::WHITE)).unwrap().take();
    let warm = t1.elapsed();
    let part = View::new(4000.0, 4000.0, 1.0);
    let t2 = Instant::now();
    let part_px = render(&snap, &NoPictures, &part, 1920, 1080, Some(Color::WHITE)).unwrap().take();
    let zoomed = t2.elapsed();
    let t3 = Instant::now();
    let rebuilt = BoardSnapshot::build(board.read());
    let snapshot_time = t3.elapsed();
    assert_eq!(rebuilt.elements.len(), snap.elements.len());
    println!(
        "25k scatter: {n} placements, {bytes} bytes ({:.1} per placement); filled in {made:?}; snapshot {snapshot_time:?}; \
         render fitted 1920x1080 cold {cold:?}, warm {warm:?}; 1:1 part {zoomed:?}; sprites {} KB",
        bytes as f64 / n as f64,
        scatter::sprite_bytes() / 1024
    );
    // Fitted, the smallest trees are dots of the tree's mean color over the
    // pixels they cover (`scatter::SPLAT_PX`): green, lighter than a crown.
    let greenish = |p: &[u8]| p[1] as i32 > p[0] as i32 + 20 && p[1] as i32 > p[2] as i32 + 20;
    assert!(fitted_px.chunks_exact(4).filter(|p| greenish(p)).count() > 100_000, "the forest shows fitted");
    assert!(part_px.chunks_exact(4).filter(|p| is_green([p[0], p[1], p[2], p[3]])).count() > 50_000, "and close up");
    assert!(scatter::sprite_bytes() <= scatter::SPRITE_BUDGET);
}

#[test]
fn far_off_a_placement_is_a_dot_of_its_mean_color() {
    let (board, _, _, instance) = tree_symbol();
    let snap = board.snapshot();
    // 0.08 pixels a unit: the 20 x 25 tree is 1.6 x 2 pixels, under SPLAT_PX.
    let view = View::new(100.0 - 50.0 / 0.08, 97.5 - 50.0 / 0.08, 0.08);
    let px = pixels(&snap, view, 100, 100);
    let lit: Vec<[u8; 4]> = px.chunks_exact(4).map(|p| [p[0], p[1], p[2], p[3]]).filter(|p| *p != WHITE).collect();
    assert!((1..=9).contains(&lit.len()), "a few pixels: {lit:?}");
    assert!(lit.iter().all(|p| p[1] > p[0] && p[1] > p[2]), "greenish: {lit:?}");
    let _ = instance;
}
