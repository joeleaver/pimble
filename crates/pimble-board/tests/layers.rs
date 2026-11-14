//! Layer kinds, blend modes and layer groups (docs/DRAWING_CONTRACT.md
//! section 1): the writes `BoardTxn` offers on the tree, the locks and
//! visibility groups pass down, where new elements go, the renderer's
//! compositing, and the tree two people reach editing at once.

use pimble_board::locks::{not_a_vector_layer, NO_VECTOR_LAYER};
use pimble_board::*;
use pimble_crdt::NodeDoc;

const T: &str = "2026-11-14T10:00:00.000Z";

fn st() -> Stamp<'static> {
    Stamp::new(None, T)
}

fn rect_el(x: f32, w: f32, color: Color) -> NewElement {
    NewElement::new(Kind::Rect, Geom::Rect(Rect::new(x, 0.0, w, 10.0))).with_style(Style {
        fill: Some(color),
        stroke: Some(color),
        width: Some(0.0),
        ..Style::default()
    })
}

fn ok<R: std::fmt::Debug>(board: &Board, f: impl FnOnce(&mut BoardTxn<'_, '_>) -> doc::Result<R>) -> R {
    board.edit(st(), f).into_result().unwrap().0
}

fn refused<R: std::fmt::Debug>(board: &Board, f: impl FnOnce(&mut BoardTxn<'_, '_>) -> doc::Result<R>) -> BoardError {
    let edited = board.edit(st(), f);
    assert!(edited.update.is_none(), "a refused write writes nothing");
    edited.result.unwrap_err()
}

fn names(snap: &BoardSnapshot) -> Vec<String> {
    snap.layers.iter().map(|l| l.name.clone()).collect()
}

/// "Layer 1" at the bottom, then a group "G" holding "A" and "B" (B on top),
/// then "Top". Answers (board, layer 1, group, a, b, top).
fn grouped() -> (Board, String, String, String, String, String) {
    let board = Board::standalone();
    let l1 = board.create(st()).unwrap().0;
    let (g, a, b, top) = ok(&board, |t| {
        let g = t.add_group("G", None, Placement::Top)?;
        let a = t.add_layer_in("A", LayerKind::Vector, Some(&g), Placement::Top)?;
        let b = t.add_layer_in("B", LayerKind::Vector, Some(&g), Placement::Top)?;
        let top = t.add_layer("Top", Placement::Top)?;
        Ok((g, a, b, top))
    });
    (board, l1, g, a, b, top)
}

#[test]
fn a_group_reads_as_a_tree_in_drawing_order() {
    let (board, l1, g, a, b, _) = grouped();
    let snap = board.snapshot();
    assert_eq!(names(&snap), ["Layer 1", "A", "B", "G", "Top"]);
    let group = snap.layer(&g).unwrap();
    assert_eq!((group.kind.clone(), group.blend, group.depth, group.isolated), (LayerKind::Group, Blend::PassThrough, 0, false));
    let la = snap.layer(&a).unwrap();
    assert_eq!((la.parent.as_deref(), la.depth, la.kind.clone(), la.blend), (Some(g.as_str()), 1, LayerKind::Vector, Blend::Normal));
    assert_eq!(snap.children_of(Some(&g)).iter().map(|l| l.id.clone()).collect::<Vec<_>>(), [a.clone(), b.clone()]);
    assert_eq!(snap.children_of(None).len(), 3);
    assert_eq!(snap.layer(&l1).unwrap().parent, None);
    // Elements draw in the tree's order: A's under B's under Top's.
    let (ea, eb, et) = ok(&board, |t| {
        let et = t.add_element(rect_el(0.0, 5.0, Color::WHITE))?;
        let eb = t.add_element(rect_el(0.0, 5.0, Color::WHITE).in_layer(&b))?;
        let ea = t.add_element(rect_el(0.0, 5.0, Color::WHITE).in_layer(&a))?;
        Ok((ea, eb, et))
    });
    let snap = board.snapshot();
    let order: Vec<&str> = snap.elements.iter().map(|e| e.id()).collect();
    assert_eq!(order, [ea.as_str(), eb.as_str(), et.as_str()]);
    assert_eq!(snap.layer(&a).unwrap().count, 1);
    assert_eq!(snap.layer(&g).unwrap().count, 0);
}

#[test]
fn elements_go_only_into_vector_layers() {
    let (board, _, g, _, _, _) = grouped();
    let paint = ok(&board, |t| t.add_layer_in("Paint 1", LayerKind::Paint, None, Placement::Top));
    let into_group = refused(&board, |t| t.add_element(rect_el(0.0, 5.0, Color::WHITE).in_layer(&g)));
    assert_eq!(into_group, BoardError::NotAVectorLayer { id: g.clone(), kind: LayerKind::Group });
    assert_eq!(into_group.to_string(), not_a_vector_layer(&LayerKind::Group));
    let into_paint = refused(&board, |t| t.add_element(rect_el(0.0, 5.0, Color::WHITE).in_layer(&paint)));
    assert!(matches!(into_paint, BoardError::NotAVectorLayer { kind: LayerKind::Paint, .. }));
    // With nothing named, a paint layer on top is passed over.
    let e = ok(&board, |t| t.add_element(rect_el(0.0, 5.0, Color::WHITE)));
    let snap = board.snapshot();
    assert_eq!(snap.get(&e).unwrap().layer, snap.layers.iter().find(|l| l.name == "Top").unwrap().id);
    assert_eq!(snap.layer(&paint).unwrap().kind, LayerKind::Paint);
    let moved = refused(&board, |t| t.move_to_layer(&[e.clone()], &paint));
    assert!(matches!(moved, BoardError::NotAVectorLayer { .. }));
    // A canvas with no vector layer says so.
    let only_paint = Board::standalone();
    let first = only_paint.create(st()).unwrap().0;
    ok(&only_paint, |t| {
        t.add_layer_in("Paint", LayerKind::Paint, None, Placement::Top)?;
        t.delete_layer(&first)
    });
    let none = refused(&only_paint, |t| t.add_element(rect_el(0.0, 5.0, Color::WHITE)));
    assert_eq!(none, BoardError::NoVectorLayer);
    assert_eq!(none.to_string(), NO_VECTOR_LAYER);
}

#[test]
fn a_locked_group_locks_what_is_in_it_and_children_keep_their_own_flags() {
    let (board, _, g, a, b, _) = grouped();
    let e = ok(&board, |t| t.add_element(rect_el(0.0, 5.0, Color::WHITE).in_layer(&a)));
    ok(&board, |t| t.set_layer_locked(&g, true));
    let named = LockedLayer { id: g.clone(), name: "G".into() };
    assert_eq!(refused(&board, |t| t.set_text(&e, "x")), BoardError::ElementLocked { id: e.clone(), layer: Some(named.clone()) });
    assert_eq!(refused(&board, |t| t.add_element(rect_el(0.0, 5.0, Color::WHITE).in_layer(&b))), BoardError::LayerLocked(named.clone()));
    assert_eq!(refused(&board, |t| t.delete_layer(&a)), BoardError::LayerLockedAgainstRemoval(named.clone()));
    let snap = board.snapshot();
    let la = snap.layer(&a).unwrap();
    assert!(la.effective_locked && !la.locked && la.locked_by.as_deref() == Some(g.as_str()));
    assert!(!snap.editable(&e));
    assert!(snap.in_locked_layer(snap.get(&e).unwrap()));
    assert_eq!(snap.layer_lock(&a), Some(named));
    // Unlocked again, A's own lock applies; B is free.
    ok(&board, |t| {
        t.set_layer_locked(&a, true)?;
        t.set_layer_locked(&g, false)
    });
    let snap = board.snapshot();
    assert_eq!(snap.layer(&a).unwrap().locked_by.as_deref(), Some(a.as_str()));
    assert!(!snap.layer(&b).unwrap().effective_locked);
    assert_eq!(snap.layer_for_new(Some(&b)).unwrap().id, b);
}

#[test]
fn a_hidden_group_hides_what_is_in_it() {
    let (board, _, g, a, _, _) = grouped();
    let e = ok(&board, |t| t.add_element(rect_el(0.0, 5.0, Color::WHITE).in_layer(&a)));
    assert!(board.snapshot().get(&e).unwrap().visible);
    ok(&board, |t| t.set_layer_visible(&g, false));
    let snap = board.snapshot();
    assert!(!snap.get(&e).unwrap().visible);
    let la = snap.layer(&a).unwrap();
    assert!(la.visible && !la.effective_visible);
    assert!(snap.visible_extent().is_none());
}

#[test]
fn new_elements_skip_layers_in_locked_or_hidden_groups() {
    let board = Board::standalone();
    let l1 = board.create(st()).unwrap().0;
    let (g, inner) = ok(&board, |t| {
        let g = t.add_group("G", None, Placement::Top)?;
        let inner = t.add_layer_in("Inner", LayerKind::Vector, Some(&g), Placement::Top)?;
        Ok((g, inner))
    });
    assert_eq!(board.snapshot().layer_for_new(None).unwrap().id, inner);
    ok(&board, |t| t.set_layer_locked(&g, true));
    assert_eq!(board.snapshot().layer_for_new(None).unwrap().id, l1);
    let e = ok(&board, |t| t.add_element(rect_el(0.0, 5.0, Color::WHITE)));
    assert_eq!(board.snapshot().get(&e).unwrap().layer, l1);
    ok(&board, |t| {
        t.set_layer_locked(&g, false)?;
        t.set_layer_visible(&g, false)
    });
    assert_eq!(board.snapshot().layer_for_new(None).unwrap().id, l1, "a hidden group is passed over");
    ok(&board, |t| {
        t.set_layer_visible(&g, true)?;
        t.set_layer_locked(&g, true)?;
        t.set_layer_locked(&l1, true)
    });
    assert_eq!(board.snapshot().layer_for_new(None), Err(BoardError::EveryLayerLocked));
    assert_eq!(refused(&board, |t| t.add_element(rect_el(0.0, 5.0, Color::WHITE))), BoardError::EveryLayerLocked);
}

#[test]
fn layers_move_into_and_out_of_groups_and_a_group_never_into_itself() {
    let (board, l1, g, a, b, top) = grouped();
    // Above a layer in a group: into that group.
    ok(&board, |t| t.move_layer(&top, Placement::Above(a.clone())));
    let snap = board.snapshot();
    assert_eq!(snap.layer(&top).unwrap().parent.as_deref(), Some(g.as_str()));
    assert_eq!(names(&snap), ["Layer 1", "A", "Top", "B", "G"]);
    // Out to the top level.
    ok(&board, |t| t.set_layer_parent(&top, None));
    assert_eq!(names(&board.snapshot()), ["Layer 1", "A", "B", "G", "Top"]);
    // Into a group at its bottom.
    ok(&board, |t| t.move_layer_into(&l1, Some(&g), Placement::Bottom));
    assert_eq!(names(&board.snapshot()), ["Layer 1", "A", "B", "G", "Top"]);
    assert_eq!(board.snapshot().layer(&l1).unwrap().depth, 1);
    // A group inside itself or a group it holds is refused.
    let h = ok(&board, |t| t.add_group("H", Some(&g), Placement::Top));
    assert!(matches!(refused(&board, |t| t.move_layer_into(&g, Some(&h), Placement::Top)), BoardError::Invalid(_)));
    assert!(matches!(refused(&board, |t| t.move_layer_into(&g, Some(&g), Placement::Top)), BoardError::Invalid(_)));
    // Only a group takes layers.
    assert!(matches!(refused(&board, |t| t.move_layer_into(&a, Some(&b), Placement::Top)), BoardError::Invalid(_)));
}

#[test]
fn group_layers_puts_the_picked_layers_in_a_new_group_at_the_topmost_ones_place() {
    let board = Board::standalone();
    let l1 = board.create(st()).unwrap().0;
    let (l2, l3, l4) = ok(&board, |t| Ok((t.add_layer("L2", Placement::Top)?, t.add_layer("L3", Placement::Top)?, t.add_layer("L4", Placement::Top)?)));
    let g = ok(&board, |t| t.group_layers(&[l3.clone(), l1.clone()], "Group 1"));
    let snap = board.snapshot();
    assert_eq!(names(&snap), ["L2", "Layer 1", "L3", "Group 1", "L4"]);
    assert_eq!(snap.layer(&l1).unwrap().parent.as_deref(), Some(g.as_str()));
    assert_eq!(snap.layer(&l2).unwrap().parent, None);
    assert_eq!(snap.layer(&l4).unwrap().parent, None);
    assert_eq!(snap.layer(&g).unwrap().kind, LayerKind::Group);
    // Picking a group and a layer in it keeps the layer in its group.
    let outer = ok(&board, |t| t.group_layers(&[g.clone(), l3.clone()], "Outer"));
    let snap = board.snapshot();
    assert_eq!(snap.layer(&g).unwrap().parent.as_deref(), Some(outer.as_str()));
    assert_eq!(snap.layer(&l3).unwrap().parent.as_deref(), Some(g.as_str()));
    assert_eq!(snap.layer(&l3).unwrap().depth, 2);
}

#[test]
fn ungroup_moves_the_layers_up_to_the_groups_place_in_order() {
    let (board, _, g, a, b, top) = grouped();
    ok(&board, |t| t.ungroup(&g));
    let snap = board.snapshot();
    assert_eq!(names(&snap), ["Layer 1", "A", "B", "Top"]);
    assert!(snap.layer(&g).is_none());
    assert!(snap.layer(&a).unwrap().parent.is_none() && snap.layer(&b).unwrap().parent.is_none());
    assert!(matches!(refused(&board, |t| t.ungroup(&top)), BoardError::Invalid(_)));
    // A locked group stays a group.
    let (board, _, g, ..) = grouped();
    ok(&board, |t| t.set_layer_locked(&g, true));
    assert!(matches!(refused(&board, |t| t.ungroup(&g)), BoardError::LayerLockedAgainstRemoval(_)));
}

#[test]
fn deleting_a_group_deletes_what_is_in_it_unless_something_is_locked() {
    let (board, l1, g, a, b, top) = grouped();
    let (ea, keep) = ok(&board, |t| {
        let ea = t.add_element(rect_el(0.0, 5.0, Color::WHITE).in_layer(&a))?;
        let keep = t.add_element(rect_el(0.0, 5.0, Color::WHITE).in_layer(&l1))?;
        Ok((ea, keep))
    });
    ok(&board, |t| t.set_layer_locked(&b, true));
    assert_eq!(refused(&board, |t| t.delete_layer(&g)), BoardError::LayerLockedAgainstRemoval(LockedLayer { id: b.clone(), name: "B".into() }));
    ok(&board, |t| t.set_layer_locked(&b, false));
    ok(&board, |t| t.delete_layer(&g));
    let snap = board.snapshot();
    assert_eq!(names(&snap), ["Layer 1", "Top"]);
    assert!(snap.get(&ea).is_none());
    assert!(snap.get(&keep).is_some());
    // A board keeps one layer that is not a group.
    let lonely = ok(&board, |t| {
        t.delete_layer(&top)?;
        let g = t.add_group("Empty", None, Placement::Top)?;
        Ok(g)
    });
    assert_eq!(refused(&board, |t| t.delete_layer(&l1)), BoardError::LastLayer);
    ok(&board, |t| t.delete_layer(&lonely));
}

#[test]
fn merge_group_makes_one_vector_layer_and_waits_for_painting_with_paint() {
    let (board, _, g, a, b, _) = grouped();
    let (ea, eb) = ok(&board, |t| {
        let eb = t.add_element(rect_el(0.0, 5.0, Color::WHITE).in_layer(&b))?;
        let ea = t.add_element(rect_el(0.0, 5.0, Color::WHITE).in_layer(&a))?;
        Ok((ea, eb))
    });
    ok(&board, |t| t.set_layer_opacity(&g, 0.5));
    ok(&board, |t| t.merge_group(&g));
    let snap = board.snapshot();
    assert_eq!(names(&snap), ["Layer 1", "G", "Top"]);
    let merged = snap.layer(&g).unwrap();
    assert_eq!((merged.kind.clone(), merged.blend, merged.opacity, merged.count), (LayerKind::Vector, Blend::Normal, 0.5, 2));
    let order: Vec<&str> = snap.elements.iter().map(|e| e.id()).collect();
    assert_eq!(order, [ea.as_str(), eb.as_str()], "A's elements stay under B's");
    // A paint layer inside: refused for now.
    let (board, _, g, ..) = grouped();
    ok(&board, |t| t.add_layer_in("Paint", LayerKind::Paint, Some(&g), Placement::Top));
    assert!(matches!(refused(&board, |t| t.merge_group(&g)), BoardError::Invalid(s) if s.contains("paint")));
}

#[test]
fn merge_down_stays_among_siblings_and_leaves_paint_for_later() {
    let (board, l1, g, a, b, top) = grouped();
    let eb = ok(&board, |t| t.add_element(rect_el(0.0, 5.0, Color::WHITE).in_layer(&b)));
    ok(&board, |t| t.merge_layer_down(&b));
    let snap = board.snapshot();
    assert_eq!(snap.get(&eb).unwrap().layer, a);
    assert_eq!(names(&snap), ["Layer 1", "A", "G", "Top"]);
    // The bottom of a group has nothing below it in the group.
    assert!(matches!(refused(&board, |t| t.merge_layer_down(&a)), BoardError::Invalid(_)));
    // Onto a group, or a group itself: refused.
    assert!(matches!(refused(&board, |t| t.merge_layer_down(&top)), BoardError::Invalid(_)));
    assert!(matches!(refused(&board, |t| t.merge_layer_down(&g)), BoardError::Invalid(_)));
    let paint = ok(&board, |t| t.add_layer_in("Paint", LayerKind::Paint, None, Placement::Above(l1.clone())));
    let onto_paint = ok(&board, |t| t.add_layer("V", Placement::Above(paint.clone())));
    assert!(matches!(refused(&board, |t| t.merge_layer_down(&onto_paint)), BoardError::Invalid(s) if s.contains("paint")));
    assert!(matches!(refused(&board, |t| t.merge_layer_down(&paint)), BoardError::Invalid(_)));
}

#[test]
fn blend_modes_are_stored_and_pass_through_is_for_groups() {
    let (board, _, g, a, ..) = grouped();
    ok(&board, |t| t.set_layer_blend(&a, Blend::Multiply));
    assert_eq!(board.snapshot().layer(&a).unwrap().blend, Blend::Multiply);
    assert!(board.snapshot().layer(&a).unwrap().isolated);
    assert!(matches!(refused(&board, |t| t.set_layer_blend(&a, Blend::PassThrough)), BoardError::Invalid(_)));
    ok(&board, |t| t.set_layer_blend(&g, Blend::Screen));
    assert!(board.snapshot().layer(&g).unwrap().isolated);
    for b in Blend::ALL {
        assert_eq!(Blend::parse(b.as_str()), Some(b));
    }
}

fn pixel(px: &tiny_skia::Pixmap, x: u32, y: u32) -> (u8, u8, u8) {
    let p = px.pixel(x, y).unwrap();
    (p.red(), p.green(), p.blue())
}

fn near(a: (u8, u8, u8), b: (u8, u8, u8)) -> bool {
    a.0.abs_diff(b.0) <= 2 && a.1.abs_diff(b.1) <= 2 && a.2.abs_diff(b.2) <= 2
}

#[test]
fn a_multiply_layer_multiplies_what_is_below_it() {
    let board = Board::standalone();
    let bottom = board.create(st()).unwrap().0;
    let top = ok(&board, |t| {
        t.add_element(rect_el(0.0, 20.0, Color::rgb(200, 100, 50)).in_layer(&bottom))?;
        let top = t.add_layer("Multiply", Placement::Top)?;
        t.add_element(rect_el(0.0, 10.0, Color::rgb(128, 128, 128)).in_layer(&top))?;
        t.set_layer_blend(&top, Blend::Multiply)?;
        Ok(top)
    });
    let snap = board.snapshot();
    assert!(snap.layer(&top).unwrap().isolated);
    let px = render(&snap, &NoPictures, &View::new(0.0, 0.0, 1.0), 20, 10, Some(Color::WHITE)).unwrap();
    let product = pixel(&px, 5, 5);
    assert!(near(product, (100, 50, 25)), "{product:?}");
    assert_eq!(pixel(&px, 15, 5), (200, 100, 50), "outside the top layer's rectangle");
    // The SVG says the same.
    let svg = render_svg(&snap, Rect::new(0.0, 0.0, 20.0, 10.0), None, &|_| None);
    assert!(svg.contains("mix-blend-mode:multiply"), "{svg}");
}

#[test]
fn a_group_at_half_opacity_is_blended_whole() {
    let board = Board::standalone();
    let first = board.create(st()).unwrap().0;
    let g = ok(&board, |t| {
        let g = t.add_group("G", None, Placement::Top)?;
        t.move_layer_into(&first, Some(&g), Placement::Top)?;
        let second = t.add_layer_in("Second", LayerKind::Vector, Some(&g), Placement::Top)?;
        // Two opaque black rectangles overlapping on x 5..10.
        t.add_element(rect_el(0.0, 10.0, Color::rgb(0, 0, 0)).in_layer(&first))?;
        t.add_element(rect_el(5.0, 10.0, Color::rgb(0, 0, 0)).in_layer(&second))?;
        Ok(g)
    });
    // Passing through at full opacity: the fast path, no buffers.
    assert!(!board.snapshot().layer(&g).unwrap().isolated);
    ok(&board, |t| t.set_layer_opacity(&g, 0.5));
    let snap = board.snapshot();
    let px = render(&snap, &NoPictures, &View::new(0.0, 0.0, 1.0), 15, 10, Some(Color::WHITE)).unwrap();
    let alone = pixel(&px, 2, 5);
    let overlap = pixel(&px, 7, 5);
    assert!(near(alone, (127, 127, 127)) || near(alone, (128, 128, 128)), "{alone:?}");
    assert_eq!(alone, overlap, "the group is blended whole: the overlap is no darker");
    let svg = render_svg(&snap, Rect::new(0.0, 0.0, 15.0, 10.0), None, &|_| None);
    assert!(svg.contains("<g opacity=\"0.5\">"), "{svg}");
    // A layer at half opacity on its own is blended whole too.
    let board = Board::standalone();
    let l = board.create(st()).unwrap().0;
    ok(&board, |t| {
        t.add_element(rect_el(0.0, 10.0, Color::rgb(0, 0, 0)))?;
        t.add_element(rect_el(5.0, 10.0, Color::rgb(0, 0, 0)))?;
        t.set_layer_opacity(&l, 0.5)
    });
    let px = render(&board.snapshot(), &NoPictures, &View::new(0.0, 0.0, 1.0), 15, 10, Some(Color::WHITE)).unwrap();
    assert_eq!(pixel(&px, 2, 5), pixel(&px, 7, 5));
}

#[test]
fn changing_a_layers_blend_changes_its_elements_fingerprints() {
    let board = Board::standalone();
    let l = board.create(st()).unwrap().0;
    let e = ok(&board, |t| t.add_element(rect_el(0.0, 10.0, Color::WHITE)));
    let before = board.snapshot().get(&e).unwrap().fingerprint;
    ok(&board, |t| t.set_layer_blend(&l, Blend::Screen));
    assert_ne!(board.snapshot().get(&e).unwrap().fingerprint, before);
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
fn concurrent_nesting_and_deleting_read_the_same_tree_everywhere() {
    let mut one = node();
    Board::of_node(&one).create(st()).unwrap();
    let (p, q) = ok(&Board::of_node(&one), |t| Ok((t.add_group("P", None, Placement::Top)?, t.add_group("Q", None, Placement::Top)?)));
    let mut two = NodeDoc::load(&one.save()).unwrap();
    // P into Q here, Q into P there: a cycle once merged.
    ok(&Board::of_node(&one), |t| t.move_layer_into(&p, Some(&q), Placement::Top));
    ok(&Board::of_node(&two), |t| t.move_layer_into(&q, Some(&p), Placement::Top));
    sync(&mut one, &mut two);
    let (s1, s2) = (Board::of_node(&one).snapshot(), Board::of_node(&two).snapshot());
    assert_eq!(s1.layers, s2.layers);
    let smaller = if p < q { &p } else { &q };
    assert_eq!(s1.layer(smaller).unwrap().parent, None, "the smaller id reads as top level");
    assert_eq!(s1.layer(smaller).unwrap().depth, 0);
    // A group deleted while someone moves a layer into it: top level.
    let mut one = node();
    let first = Board::of_node(&one).create(st()).unwrap().0;
    let g = ok(&Board::of_node(&one), |t| t.add_group("G", None, Placement::Top));
    let extra = ok(&Board::of_node(&one), |t| t.add_layer("Extra", Placement::Top));
    let mut two = NodeDoc::load(&one.save()).unwrap();
    ok(&Board::of_node(&one), |t| t.delete_layer(&g));
    ok(&Board::of_node(&two), |t| t.move_layer_into(&extra, Some(&g), Placement::Top));
    sync(&mut one, &mut two);
    let (s1, s2) = (Board::of_node(&one).snapshot(), Board::of_node(&two).snapshot());
    assert_eq!(s1.layers, s2.layers);
    let l = s1.layer(&extra).unwrap();
    assert_eq!((l.parent.clone(), l.depth), (None, 0));
    assert!(s1.layer(&first).is_some());
}

#[test]
fn a_copy_keeps_kinds_blends_and_groups() {
    let (board, _, g, a, ..) = grouped();
    ok(&board, |t| {
        t.set_layer_blend(&a, Blend::Overlay)?;
        t.add_layer_in("Paint", LayerKind::Paint, Some(&g), Placement::Top)
    });
    let raw = board.read().unwrap();
    let copy = Board::standalone();
    ok(&copy, |t| t.copy_from(&raw));
    assert_eq!(copy.snapshot().layers, board.snapshot().layers);
}
