//! Locks (docs/IMAGE_MARKUP_CONTRACT.md section 5, docs/API_CONTRACT.md
//! decision 8 and open decision 5): `BoardTxn` refuses what a lock guards,
//! for every caller, with the sentence a person reads; a snapshot answers the
//! same rules before anything is written. And the one recipe a new canvas
//! starts from.

use pimble_board::locks::{ELEMENT_LOCKED, EVERY_LAYER_LOCKED, LAYER_LOCKED, LAYER_LOCKED_AGAINST_REMOVAL};
use pimble_board::recipe::{canvas_icon, CANVAS_ICON, PICTURE_CANVAS_ICON, PICTURE_LAYER_NAME};
use pimble_board::*;

const T: &str = "2026-11-02T10:00:00.000Z";

fn st() -> Stamp<'static> {
    Stamp::new(None, T)
}

fn rect_el(x: f32) -> NewElement {
    NewElement::new(Kind::Rect, Geom::Rect(Rect::new(x, 0.0, 10.0, 10.0)))
}

/// A board with "Layer 1" and two rectangles on it, the first locked.
fn board_with_a_locked_element() -> (Board, String, String, String) {
    let board = Board::standalone();
    let layer = board.create(st()).unwrap().0;
    let (locked, free) = board
        .edit(st(), |t| {
            let locked = t.add_element(rect_el(0.0))?;
            let free = t.add_element(rect_el(20.0))?;
            t.set_locked(&locked, true)?;
            Ok((locked, free))
        })
        .into_result()
        .unwrap()
        .0;
    (board, layer, locked, free)
}

fn refused<R: std::fmt::Debug>(board: &Board, f: impl FnOnce(&mut BoardTxn<'_, '_>) -> doc::Result<R>) -> BoardError {
    let edited = board.edit(st(), f);
    assert!(edited.update.is_none(), "a refused write writes nothing");
    edited.result.unwrap_err()
}

#[test]
fn a_locked_element_takes_no_edit_and_says_so() {
    let (board, _, locked, _) = board_with_a_locked_element();
    let other = board.edit(st(), |t| t.add_layer("Other", Placement::Top)).into_result().unwrap().0;
    let expected = BoardError::ElementLocked { id: locked.clone(), layer: None };
    let id = locked.as_str();
    let edits: Vec<Box<dyn FnOnce(&mut BoardTxn<'_, '_>) -> doc::Result<()>>> = vec![
        Box::new(|t| t.set_geom(id, &Geom::Rect(Rect::new(5.0, 5.0, 1.0, 1.0)))),
        Box::new(|t| t.set_style(id, &Style { width: Some(9.0), ..Style::default() })),
        Box::new(|t| t.clear_style(id, StyleKey::Fill)),
        Box::new(|t| t.set_transform(id, &Transform::translate(3.0, 0.0))),
        Box::new(|t| t.set_text(id, "hello")),
        Box::new(|t| t.set_parent(id, None)),
        Box::new(|t| t.move_element(id, Placement::Bottom)),
        Box::new(|t| t.delete_element(id)),
        Box::new(|t| t.move_to_layer(&[id.to_string()], &other)),
    ];
    for edit in edits {
        assert_eq!(refused(&board, edit), expected);
    }
    assert_eq!(expected.to_string(), ELEMENT_LOCKED);
    assert!(expected.is_lock());
}

#[test]
fn notes_fields_and_the_lock_itself_stay_open_on_a_locked_element() {
    let (board, _, locked, _) = board_with_a_locked_element();
    board
        .edit(st(), |t| {
            t.set_note(&locked, "the screenshot")?;
            t.set_field(&locked, "role", Some("evidence"))
        })
        .into_result()
        .unwrap();
    let snap = board.snapshot();
    let e = snap.get(&locked).unwrap();
    assert_eq!(e.raw.note.as_deref(), Some("the screenshot"));
    assert!(e.raw.locked, "still locked");
    // Unlocking is how it opens again, and then it moves.
    board
        .edit(st(), |t| {
            t.set_locked(&locked, false)?;
            t.set_transform(&locked, &Transform::translate(4.0, 0.0))
        })
        .into_result()
        .unwrap();
    assert!(!board.snapshot().get(&locked).unwrap().raw.locked);
}

#[test]
fn a_locked_layer_guards_its_elements_and_takes_nothing_new() {
    let (board, layer, _, free) = board_with_a_locked_element();
    let top = board.edit(st(), |t| t.add_layer("Top", Placement::Top)).into_result().unwrap().0;
    board.edit(st(), |t| t.set_layer_locked(&layer, true)).into_result().unwrap();
    let named = LockedLayer { id: layer.clone(), name: "Layer 1".into() };

    // Its elements, even unlocked ones, take no edit; not even their own lock.
    let on_layer = BoardError::ElementLocked { id: free.clone(), layer: Some(named.clone()) };
    assert_eq!(refused(&board, |t| t.set_transform(&free, &Transform::translate(1.0, 0.0))), on_layer);
    assert_eq!(refused(&board, |t| t.set_locked(&free, true)), on_layer);
    // Nothing new goes in.
    assert_eq!(refused(&board, |t| t.add_element(rect_el(0.0).in_layer(layer.clone()))), BoardError::LayerLocked(named.clone()));
    assert_eq!(BoardError::LayerLocked(named.clone()).to_string(), LAYER_LOCKED);
    // It is neither deleted nor merged away.
    let removal = BoardError::LayerLockedAgainstRemoval(named.clone());
    assert_eq!(refused(&board, |t| t.delete_layer(&layer)), removal);
    assert_eq!(removal.to_string(), LAYER_LOCKED_AGAINST_REMOVAL);
    // Nothing is merged down into it either.
    assert_eq!(refused(&board, |t| t.merge_layer_down(&top)), BoardError::LayerLocked(named));
    // Its name, visibility, opacity, place and meaning stay open.
    board
        .edit(st(), |t| {
            t.rename_layer(&layer, "Base")?;
            t.set_layer_visible(&layer, false)?;
            t.set_layer_opacity(&layer, 0.5)?;
            t.move_layer(&layer, Placement::Top)?;
            t.set_layer_note(&layer, "the picture")
        })
        .into_result()
        .unwrap();
}

#[test]
fn new_elements_go_to_the_top_layer_that_is_not_locked() {
    let board = Board::standalone();
    let bottom = board.create(st()).unwrap().0;
    let top = board.edit(st(), |t| t.add_layer("Top", Placement::Top)).into_result().unwrap().0;
    board.edit(st(), |t| t.set_layer_locked(&top, true)).into_result().unwrap();
    let id = board.add_element(st(), rect_el(0.0)).unwrap().0;
    let snap = board.snapshot();
    assert_eq!(snap.get(&id).unwrap().layer, bottom);
    // The snapshot answers the same, and refuses a picked locked layer.
    assert_eq!(snap.layer_for_new(None).unwrap().id, bottom);
    assert!(matches!(snap.layer_for_new(Some(&top)), Err(BoardError::LayerLocked(l)) if l.id == top && l.name == "Top"));
    assert_eq!(snap.layer_for_new(Some(&bottom)).unwrap().id, bottom);

    board.edit(st(), |t| t.set_layer_locked(&bottom, true)).into_result().unwrap();
    assert_eq!(refused(&board, |t| t.add_element(rect_el(0.0))), BoardError::EveryLayerLocked);
    assert_eq!(BoardError::EveryLayerLocked.to_string(), EVERY_LAYER_LOCKED);
    assert!(matches!(board.snapshot().layer_for_new(None), Err(BoardError::EveryLayerLocked)));
}

#[test]
fn the_snapshot_judges_edits_as_the_transaction_does() {
    let (board, layer, locked, free) = board_with_a_locked_element();
    let snap = board.snapshot();
    assert!(!snap.editable(&locked) && snap.editable(&free));
    assert!(matches!(snap.judge_edit(&locked), Err(BoardError::ElementLocked { layer: None, .. })));
    assert!(!snap.in_locked_layer(snap.get(&free).unwrap()));
    board.edit(st(), |t| t.set_layer_locked(&layer, true)).into_result().unwrap();
    let snap = board.snapshot();
    assert!(!snap.editable(&free));
    assert!(snap.in_locked_layer(snap.get(&free).unwrap()));
    assert!(!snap.editable("nothing"));
}

/// Open decision 5, taken: a delete of several skips the locked ones and
/// says how many it kept.
#[test]
fn deleting_several_keeps_the_locked_ones_and_counts_them() {
    let (board, _, locked, free) = board_with_a_locked_element();
    let (done, update) = board
        .edit(st(), |t| t.delete_elements(&[locked.clone(), free.clone(), free.clone()]))
        .into_result()
        .unwrap();
    assert!(update.is_some());
    assert_eq!(done, Deleted { deleted: vec![free.clone()], kept: vec![locked.clone()] });
    assert_eq!(done.kept_sentence().as_deref(), Some("1 locked element was kept. Unlock it to delete it."));
    let snap = board.snapshot();
    assert!(snap.get(&locked).is_some() && snap.get(&free).is_none());

    // Every one locked: nothing written, everything kept.
    let edited = board.edit(st(), |t| t.delete_elements(std::slice::from_ref(&locked)));
    assert!(edited.update.is_none());
    assert_eq!(edited.result.unwrap().kept, vec![locked.clone()]);

    // An id that names nothing refuses the whole delete before it writes.
    let (board, _, _, free) = board_with_a_locked_element();
    assert_eq!(refused(&board, |t| t.delete_elements(&[free.clone(), "gone".into()])), BoardError::NoElement("gone".into()));
    assert!(board.snapshot().get(&free).is_some());

    let none = Deleted { deleted: vec!["a".into()], kept: vec![] };
    assert_eq!(none.kept_sentence(), None);
    let two = Deleted { deleted: vec![], kept: vec!["a".into(), "b".into()] };
    assert_eq!(two.kept_sentence().as_deref(), Some("2 locked elements were kept. Unlock them to delete them."));
}

/// Deleting or merging an unlocked layer is an act on the layer: its
/// elements locked one by one go with it.
#[test]
fn a_layer_act_takes_its_locked_elements_along() {
    let (board, layer, locked, _) = board_with_a_locked_element();
    let top = board.edit(st(), |t| t.add_layer("Top", Placement::Top)).into_result().unwrap().0;
    let mover = board.edit(st(), |t| t.add_element(rect_el(50.0).in_layer(top.clone()))).into_result().unwrap().0;
    board.edit(st(), |t| t.set_locked(&mover, true)).into_result().unwrap();
    board.edit(st(), |t| t.merge_layer_down(&top)).into_result().unwrap();
    let snap = board.snapshot();
    let moved = snap.get(&mover).unwrap();
    assert_eq!(moved.layer, layer);
    assert!(moved.raw.locked, "still locked where it went");
    // And a layer deleted takes its locked elements with it (a canvas keeps
    // one layer, so another goes in first).
    board.edit(st(), |t| t.add_layer("Spare", Placement::Top)).into_result().unwrap();
    board.edit(st(), |t| t.delete_layer(&layer)).into_result().unwrap();
    let snap = board.snapshot();
    assert!(snap.get(&locked).is_none() && snap.get(&mover).is_none());
}

#[test]
fn a_canvas_starts_one_way() {
    let board = Board::standalone();
    let made = board.edit(st(), |t| t.start_canvas(None)).into_result().unwrap().0;
    let snap = board.snapshot();
    assert_eq!(snap.layers.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["Layer 1"]);
    assert_eq!(made, NewCanvas { draw_layer: snap.layers[0].id.clone(), picture: None });

    let board = Board::standalone();
    let picture = StartingPicture { src: "pimble-blob:s/h", px_w: 640, px_h: 480 };
    let made = board.edit(st(), |t| t.start_canvas(Some(picture))).into_result().unwrap().0;
    let snap = board.snapshot();
    assert_eq!(snap.layers.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), [PICTURE_LAYER_NAME, "Layer 1"]);
    let (picture_layer, element) = made.picture.unwrap();
    assert!(snap.layer(&picture_layer).unwrap().locked, "the picture is locked in a layer of its own");
    assert!(!snap.layer(&made.draw_layer).unwrap().locked);
    let e = snap.get(&element).unwrap();
    assert_eq!(e.raw.geom, Geom::Picture(PictureGeom { rect: Rect::new(0.0, 0.0, 640.0, 480.0), src: "pimble-blob:s/h".into(), px_w: 640, px_h: 480, crop: None }));
    // New marks go above it, into the layer that takes them.
    let mark = board.add_element(st(), rect_el(0.0)).unwrap().0;
    assert_eq!(board.snapshot().get(&mark).unwrap().layer, made.draw_layer);

    assert_eq!((canvas_icon(false), canvas_icon(true)), (CANVAS_ICON, PICTURE_CANVAS_ICON));
}
