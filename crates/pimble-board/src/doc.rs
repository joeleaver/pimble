//! The board document in yrs (docs/IMAGE_MARKUP_CONTRACT.md section 3).
//!
//! ```text
//! board: Map {
//!   v: 1,
//!   page: Any { x, y, w, h, fill? }?,       // absent = infinite
//!   markup_visible: bool,
//!   edited_by: Any { id, name }?, edited_at: String?,   // board-level writes
//!   layers: Map<layer id, Map { name: Text, order: String, visible, locked, opacity,
//!                               kind: String?, blend: String?, parent: String?,
//!                               note: Text?, "field:<name>": String... }>,
//!   elements: Map<element id, Map {
//!     layer, order, parent?, kind,
//!     geom: Any {..} (per kind, written whole) | Array<Buffer> (pen, highlighter),
//!           a path's: Any { subpaths: [{ closed, anchors: [x, y, in_dx, in_dy,
//!                     out_dx, out_dy, mode, ...] }], rule } (crate::path),
//!     text: Text?, style: Map { stroke, fill, width, opacity, font_size, align,
//!                               shadow, arrow_heads, dash,
//!                               cap?, join?, miter?, dash_pattern?,   // stroke detail
//!                               radius?, sides?, points?, inner? },   // live shapes
//!     transform: Any { tx, ty, rot, sx, sy }?, locked,
//!     author: Any { id, name }?, created_at, edited_by?, edited_at?,
//!     note: Text?, "field:<name>": String...,   // what it means (below)
//!   }>,
//!   "paint:<op id>": Map { layer, order, kind, ... },   // paint operations,
//!                                   // one key each (crate::paint_ops)
//! }
//! ```
//!
//! Which values are one last-writer-wins value and which merge field by
//! field is the point of the layout: `geom` and `transform` are written whole
//! (two people dragging one arrow end with one of the two positions, never a
//! mix), the style map's keys and the element map's keys merge one by one,
//! text merges character by character, and a stroke's points are an array of
//! appended chunks, each chunk a run of little-endian `f32` triples (x, y,
//! pressure), so others see a stroke as it is drawn.
//!
//! **Meaning** (Joe, 2026-10-29): every element and every layer can say
//! what it is, for the people and the assistants reading the canvas: a free
//! `note`, co-edited text, and named fields, each its own key `field:<name>`
//! on the element's or layer's map (one last-writer-wins value per field, so
//! two people setting different fields keep both; never a container made on
//! first use, the rule node fields follow). Nothing reads them to draw; no
//! legend is made from them.
//!
//! A [`Board`] is a handle on where a board lives in a yrs document; it holds
//! no state of its own. Every write runs in one transaction and answers the
//! update it produced, an ordinary yrs update of the node's document, which
//! the caller sends through `applyEdit` like every other edit (CLAUDE.md:
//! "one way to write node content").

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use pimble_crdt::NodeDoc;
use yrs::types::text::TextRef;
use yrs::types::{Event, PathSegment};
use yrs::undo::UndoManager;
use yrs::{
    Any, Array, ArrayPrelim, ArrayRef, DeepObservable, Doc, GetString, Map, MapPrelim, MapRef, OffsetKind, Origin, Out, ReadTxn,
    StateVector, Text, TextPrelim, Transact, TransactionMut,
};

use crate::layer_tree::LayerTree;
use tiny_skia::Transform as Affine;
use crate::locks::Deleted;
use crate::model::*;
use crate::order;

/// The key of a board node's board in its `data` root.
pub const BOARD_KEY: &str = "board";
/// The prefix of the keys, in a node's `data` root, of the boards of
/// marked-up pictures in its text: `board:<board id>` (contract section 2).
/// Each board is a key of the data root itself, never an entry of a map made
/// on first use: two devices marking up their first pictures in one note
/// while apart would each make that map, and yrs would keep only one of them
/// (and its boards).
pub const TEXT_BOARD_PREFIX: &str = "board:";

/// The `data` key of the board of a marked-up picture in text.
pub fn text_board_key(board_id: &str) -> String {
    format!("{TEXT_BOARD_PREFIX}{board_id}")
}
/// The schema version written to `v`.
pub const VERSION: i64 = 1;
/// The first layer's name.
pub const FIRST_LAYER_NAME: &str = "Layer 1";

pub(crate) mod keys {
    pub const V: &str = "v";
    pub const PAGE: &str = "page";
    pub const MARKUP_VISIBLE: &str = "markup_visible";
    pub const LAYERS: &str = "layers";
    pub const ELEMENTS: &str = "elements";
    pub const NAME: &str = "name";
    pub const ORDER: &str = "order";
    pub const VISIBLE: &str = "visible";
    pub const LOCKED: &str = "locked";
    pub const OPACITY: &str = "opacity";
    pub const LAYER: &str = "layer";
    pub const PARENT: &str = "parent";
    pub const KIND: &str = "kind";
    pub const GEOM: &str = "geom";
    pub const TEXT: &str = "text";
    pub const STYLE: &str = "style";
    pub const TRANSFORM: &str = "transform";
    pub const AUTHOR: &str = "author";
    pub const CREATED_AT: &str = "created_at";
    pub const EDITED_BY: &str = "edited_by";
    pub const EDITED_AT: &str = "edited_at";
    pub const NOTE: &str = "note";
    pub const BLEND: &str = "blend";
}

/// The prefix of an element's or layer's named fields (see the module doc).
pub const FIELD_PREFIX: &str = "field:";
use keys::*;

/// Why a board write was refused. A refused single write changed nothing.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum BoardError {
    #[error("this node has no canvas")]
    NoBoard,
    #[error("the canvas has no element {0}")]
    NoElement(String),
    #[error("the canvas has no layer {0}")]
    NoLayer(String),
    #[error("a canvas keeps at least one layer")]
    LastLayer,
    #[error("element {0} is not a pen or highlighter stroke")]
    NotAStroke(String),
    /// An edit of an element that is locked, itself or by the layer it is
    /// drawn in (`layer` names that layer when it is the layer's lock).
    #[error("{}", crate::locks::ELEMENT_LOCKED)]
    ElementLocked { id: String, layer: Option<LockedLayer> },
    /// Something new was to go into a locked layer: an element added or
    /// moved there, or a layer merged down into it.
    #[error("{}", crate::locks::LAYER_LOCKED)]
    LayerLocked(LockedLayer),
    /// A locked layer was to be deleted or merged away.
    #[error("{}", crate::locks::LAYER_LOCKED_AGAINST_REMOVAL)]
    LayerLockedAgainstRemoval(LockedLayer),
    /// Nothing named a layer and every layer is locked.
    #[error("{}", crate::locks::EVERY_LAYER_LOCKED)]
    EveryLayerLocked,
    /// Elements were to go into a layer that holds none: a paint layer or a
    /// group (docs/DRAWING_CONTRACT.md section 1).
    #[error("{}", crate::locks::not_a_vector_layer(kind))]
    NotAVectorLayer { id: String, kind: LayerKind },
    /// Paint went to a layer that is not a paint layer
    /// (docs/DRAWING_CONTRACT.md section 3).
    #[error("{}", crate::locks::not_a_paint_layer(kind))]
    NotAPaintLayer { id: String, kind: LayerKind },
    /// No paint operation has this id.
    #[error("the canvas has no paint operation {0}")]
    NoPaintOp(String),
    /// Nothing named a layer and the board has no vector layer.
    #[error("{}", crate::locks::NO_VECTOR_LAYER)]
    NoVectorLayer,
    /// A merge onto or into a paint layer draws the merged layers as pixels
    /// first (docs/DRAWING_CONTRACT.md "Built in wave 5 (model)"):
    /// [`crate::raster::rasterize`], put each tile as a blob, then
    /// [`BoardTxn::apply_raster`] with their URLs.
    #[error("{0}")]
    NeedsPixels(String),
    /// Pictures the pixels would show have not been loaded yet (how many).
    #[error("{0} picture(s) this needs have not arrived yet; try again once they show.")]
    PicturesNotLoaded(usize),
    #[error("{0}")]
    Invalid(String),
}

/// The locked layer a refusal is about, by id and by the name a person sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedLayer {
    pub id: String,
    pub name: String,
}

impl BoardError {
    /// Whether this is a lock's refusal (contract section 5: a guard
    /// against accidents, not a permission), which a person is told about,
    /// rather than a mistake of the caller's.
    pub fn is_lock(&self) -> bool {
        matches!(
            self,
            BoardError::ElementLocked { .. } | BoardError::LayerLocked(_) | BoardError::LayerLockedAgainstRemoval(_) | BoardError::EveryLayerLocked
        )
    }
}

pub type Result<T> = std::result::Result<T, BoardError>;

/// What [`Board::edit`] answers: the closure's result, and the update the
/// transaction produced whether or not the closure succeeded (yrs has no
/// rollback, so a closure that fails after writing has still written, and the
/// caller must still send those bytes). `None` when nothing changed.
#[derive(Debug)]
pub struct Edited<R> {
    pub result: Result<R>,
    pub update: Option<Vec<u8>>,
}

impl<R> Edited<R> {
    /// The result with the update, for a caller that treats a failure as
    /// nothing written (true of every single write [`BoardTxn`] offers: each
    /// checks before it writes).
    pub fn into_result(self) -> Result<(R, Option<Vec<u8>>)> {
        let Edited { result, update } = self;
        result.map(|r| (r, update))
    }
}

/// Where a board lives in a yrs document, and the origin its writes carry.
#[derive(Clone)]
pub struct Board {
    doc: Doc,
    root: MapRef,
    path: Vec<Arc<str>>,
    origin: Option<Origin>,
}

impl Board {
    /// The board at `root[path[0]][path[1]]...` of `doc`. Intermediate maps
    /// are made by [`BoardTxn::create`].
    pub fn new(doc: Doc, root: MapRef, path: &[&str]) -> Self {
        assert!(!path.is_empty(), "a board lives under a key");
        Self { doc, root, path: path.iter().map(|k| Arc::from(*k)).collect(), origin: None }
    }

    /// A board node's board: `data.board` (contract section 1).
    pub fn of_node(node: &NodeDoc) -> Self {
        Self::new(node.ydoc().clone(), node.data_root(), &[BOARD_KEY])
    }

    /// The board of a marked-up picture in a node's text:
    /// `data["board:<board_id>"]` (contract section 2).
    pub fn in_text(node: &NodeDoc, board_id: &str) -> Self {
        Self::new(node.ydoc().clone(), node.data_root(), &[&text_board_key(board_id)])
    }

    /// A board in a document of its own (root map `data`, key `board`), for
    /// tests, benches and tools that render a board's bytes.
    pub fn standalone() -> Self {
        let doc = Doc::with_options(yrs::Options { offset_kind: OffsetKind::Utf16, ..Default::default() });
        let root = doc.get_or_insert_map(pimble_crdt::ROOT_DATA);
        Self::new(doc, root, &[BOARD_KEY])
    }

    /// Every write through this handle carries `origin` (yrs
    /// `transact_mut_with`), which is what a per-person `UndoManager` tracks
    /// (contract section 4, "Undo is per person").
    pub fn with_origin(mut self, origin: impl Into<Origin>) -> Self {
        self.origin = Some(origin.into());
        self
    }

    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    /// Whether the board has been created (here or by any peer).
    pub fn exists(&self) -> bool {
        let txn = self.doc.transact();
        self.board_map(&txn).is_some()
    }

    /// The board map, when it exists.
    pub fn board_map<T: ReadTxn>(&self, txn: &T) -> Option<MapRef> {
        let mut map = self.root.clone();
        for key in &self.path {
            match map.get(txn, key) {
                Some(Out::YMap(next)) => map = next,
                _ => return None,
            }
        }
        Some(map)
    }

    /// Run `f` in one write transaction (stamped with `stamp`) and answer
    /// its result with the update the transaction produced.
    pub fn edit<R>(&self, stamp: Stamp<'_>, f: impl FnOnce(&mut BoardTxn<'_, '_>) -> Result<R>) -> Edited<R> {
        let mut txn = match &self.origin {
            Some(origin) => self.doc.transact_mut_with(origin.clone()),
            None => self.doc.transact_mut(),
        };
        let board = self.board_map(&txn);
        let mut bt = BoardTxn { txn: &mut txn, location: self, board, stamp, tops: HashMap::new(), layer_cache: RefCell::new(None) };
        let result = f(&mut bt);
        txn.commit();
        let changed = *txn.before_state() != *txn.after_state() || !txn.delete_set().is_empty();
        let update = changed.then(|| txn.encode_update_v1());
        Edited { result, update }
    }

    /// Create the board: `v`, the markup shown, one layer "Layer 1" and no
    /// elements. Answers the first layer's id. On a board that exists it
    /// writes nothing and answers the bottom layer.
    pub fn create(&self, stamp: Stamp<'_>) -> Result<(String, Option<Vec<u8>>)> {
        self.edit(stamp, |t| t.create()).into_result()
    }

    /// Add one element (see [`BoardTxn::add_element`]).
    pub fn add_element(&self, stamp: Stamp<'_>, element: NewElement) -> Result<(String, Option<Vec<u8>>)> {
        self.edit(stamp, |t| t.add_element(element)).into_result()
    }

    /// Append points to a stroke being drawn (see [`BoardTxn::append_points`]).
    pub fn append_points(&self, stamp: Stamp<'_>, id: &str, points: &[[f32; 3]]) -> Result<Option<Vec<u8>>> {
        self.edit(stamp, |t| t.append_points(id, points)).into_result().map(|(_, u)| u)
    }

    pub fn delete_element(&self, stamp: Stamp<'_>, id: &str) -> Result<Option<Vec<u8>>> {
        self.edit(stamp, |t| t.delete_element(id)).into_result().map(|(_, u)| u)
    }

    /// Everything the renderer needs, read in one transaction.
    pub fn snapshot(&self) -> crate::BoardSnapshot {
        crate::BoardSnapshot::build(self.read())
    }

    /// The document as plain values (`None` when there is no board).
    pub fn read(&self) -> Option<RawBoard> {
        let txn = self.doc.transact();
        let board = self.board_map(&txn)?;
        Some(read_board(&txn, &board, true))
    }

    /// The board's own values and layers, and only the elements named in
    /// `ids` (each `None` when it is gone): what [`crate::BoardSnapshot::update`]
    /// re-reads after a change.
    pub fn read_some(&self, ids: &HashSet<String>) -> Option<(RawBoard, ChangedElements)> {
        let txn = self.doc.transact();
        let board = self.board_map(&txn)?;
        let raw = read_board(&txn, &board, false);
        let elements = match board.get(&txn, ELEMENTS) {
            Some(Out::YMap(map)) => map,
            _ => return Some((raw, ids.iter().map(|id| (id.clone(), None)).collect())),
        };
        let found = ids
            .iter()
            .map(|id| {
                let el = match elements.get(&txn, id) {
                    Some(Out::YMap(el)) => Some(read_element(&txn, id, &el)),
                    _ => None,
                };
                (id.clone(), el)
            })
            .collect();
        Some((raw, found))
    }

    /// The paint operations named in `ids` (each `None` when it is gone):
    /// what [`crate::BoardSnapshot::update`] re-reads after a stroke grows.
    pub fn read_paint_some(&self, ids: &HashSet<String>) -> Vec<(String, Option<crate::paint_ops::RawPaintOp>)> {
        let txn = self.doc.transact();
        let Some(board) = self.board_map(&txn) else {
            return ids.iter().map(|id| (id.clone(), None)).collect();
        };
        ids.iter()
            .map(|id| {
                let op = match board.get(&txn, &crate::paint_ops::paint_key(id)) {
                    Some(Out::YMap(map)) => Some(read_paint_op(&txn, id, &map)),
                    _ => None,
                };
                (id.clone(), op)
            })
            .collect()
    }

    /// Append points to a brush stroke being drawn (see
    /// [`BoardTxn::append_stroke`]).
    pub fn append_stroke(&self, stamp: Stamp<'_>, id: &str, points: &[crate::brush::BrushPoint]) -> Result<Option<Vec<u8>>> {
        self.edit(stamp, |t| t.append_stroke(id, points)).into_result().map(|(_, u)| u)
    }

    /// Watch the board for changes, local and remote, so a snapshot can be
    /// brought up to date by re-reading only what changed
    /// ([`crate::BoardSnapshot::update`]). `None` until the board exists.
    pub fn watch(&self) -> Option<BoardWatch> {
        let board = {
            let txn = self.doc.transact();
            self.board_map(&txn)?
        };
        let changes = Arc::new(Mutex::new(Changes::default()));
        let sink = changes.clone();
        let subscription = board.observe_deep(move |txn, events| {
            let mut c = sink.lock().unwrap();
            for event in events.iter() {
                let path = event.path();
                match path.front() {
                    // The board map itself: its values, or a whole subtree replaced.
                    None => {
                        if let Event::Map(e) = event {
                            for key in e.keys(txn).keys() {
                                match &**key {
                                    ELEMENTS | LAYERS => c.all = true,
                                    k => match k.strip_prefix(crate::paint_ops::PAINT_PREFIX) {
                                        Some(op) => {
                                            c.paint.insert(op.to_string());
                                        }
                                        None => c.board = true,
                                    },
                                }
                            }
                        } else {
                            c.all = true;
                        }
                    }
                    Some(PathSegment::Key(k)) if &**k == LAYERS => c.board = true,
                    Some(PathSegment::Key(k)) if k.starts_with(crate::paint_ops::PAINT_PREFIX) => {
                        c.paint.insert(k[crate::paint_ops::PAINT_PREFIX.len()..].to_string());
                    }
                    Some(PathSegment::Key(k)) if &**k == ELEMENTS => match path.get(1) {
                        Some(PathSegment::Key(id)) => {
                            c.elements.insert(id.to_string());
                        }
                        Some(_) => c.all = true,
                        None => {
                            if let Event::Map(e) = event {
                                c.elements.extend(e.keys(txn).keys().map(|k| k.to_string()));
                            }
                        }
                    },
                    Some(_) => c.board = true,
                }
            }
        });
        Some(BoardWatch { changes, _subscription: subscription })
    }

    /// A per-person undo manager over this board (contract section 4): it
    /// tracks only transactions carrying this handle's origin (set one with
    /// [`Board::with_origin`]), so undo takes back my edits and never a
    /// collaborator's. `None` until the board exists. `options` carries the
    /// clock (`yrs::undo::Options::default()` on native targets; the browser
    /// supplies its own clock).
    pub fn undo_manager(&self, mut options: yrs::undo::Options<()>) -> Option<UndoManager<()>> {
        let board = {
            let txn = self.doc.transact();
            self.board_map(&txn)?
        };
        if let Some(origin) = &self.origin {
            options.tracked_origins.insert(origin.clone());
        }
        let mut manager = UndoManager::with_options(options);
        manager.expand_scope(&self.doc, &board);
        Some(manager)
    }

    /// Undo (or with `redo`, redo) one step of `manager`, answering the
    /// update to send: what the undo changed, as a diff from the state
    /// before it.
    pub fn undo(&self, manager: &mut UndoManager<()>, redo: bool) -> Option<Vec<u8>> {
        let before = self.doc.transact().state_vector();
        let done = if redo { manager.redo_blocking() } else { manager.undo_blocking() };
        if !done {
            return None;
        }
        // A diff from the state before, not a state-vector comparison: an
        // undo that only deletes does not move the state vector, and the
        // diff still carries its deletions.
        Some(self.doc.transact().encode_diff_v1(&before))
    }

    /// Everything this board's document has that a peer at `sv` lacks.
    pub fn diff_since(&self, sv: &StateVector) -> Vec<u8> {
        self.doc.transact().encode_diff_v1(sv)
    }
}

/// Elements re-read by id: each `None` when it is gone.
pub type ChangedElements = Vec<(String, Option<RawElement>)>;

/// What changed on a board since [`BoardWatch::take`] was last called.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Changes {
    /// Something only a whole re-read can account for (a subtree replaced).
    pub all: bool,
    /// The board's own values or its layers.
    pub board: bool,
    /// Elements added, changed or removed.
    pub elements: HashSet<String>,
    /// Paint operations added, changed (a stroke's points appended) or
    /// removed, by op id.
    pub paint: HashSet<String>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        !self.all && !self.board && self.elements.is_empty() && self.paint.is_empty()
    }
}

/// A subscription to a board's changes (see [`Board::watch`]); dropping it
/// ends the subscription.
pub struct BoardWatch {
    changes: Arc<Mutex<Changes>>,
    _subscription: yrs::Subscription,
}

impl BoardWatch {
    /// What changed since the last call.
    pub fn take(&self) -> Changes {
        std::mem::take(&mut *self.changes.lock().unwrap())
    }
}

/// A write transaction on a board. Every write checks what it needs before
/// it writes, so a refused write changes nothing.
///
/// **Locks** (contract section 5) are judged here, for every caller: an
/// element that is locked, or drawn in a locked layer, takes no edit of its
/// geometry, text, style, place or membership, and is not deleted; a locked
/// layer takes no new elements and is neither deleted nor merged away. What
/// stays open on a locked thing: its lock (an element's own lock, while its
/// layer is not locked), what it means (notes and fields: a locked picture
/// can still be described), and a layer's name, visibility, opacity and
/// place among the layers.
pub struct BoardTxn<'a, 'doc> {
    txn: &'a mut TransactionMut<'doc>,
    location: &'a Board,
    board: Option<MapRef>,
    stamp: Stamp<'a>,
    /// The top `order` per layer, found once per transaction and kept up to
    /// date as elements are added, so adding many elements in one
    /// transaction is not quadratic.
    tops: HashMap<String, Option<String>>,
    /// The layers read as a tree, until a layer is written.
    layer_cache: RefCell<Option<Rc<Layers>>>,
}

/// A board's layers as stored and read as a tree, for the judgements of a
/// write.
struct Layers {
    raw: Vec<RawLayer>,
    tree: LayerTree,
}

impl Layers {
    /// The entry of layer `id`.
    fn entry(&self, id: &str) -> Option<usize> {
        self.tree.entries.iter().position(|e| self.raw[e.index].id == id)
    }

    fn raw_of(&self, entry: usize) -> &RawLayer {
        &self.raw[self.tree.entries[entry].index]
    }

    fn id(&self, entry: usize) -> &str {
        &self.raw_of(entry).id
    }

    fn locked_layer(&self, entry: usize) -> LockedLayer {
        let l = self.raw_of(entry);
        LockedLayer { id: l.id.clone(), name: l.name.clone() }
    }

    /// The lock that guards `entry` (its own, or a group's above it).
    fn locked(&self, entry: usize) -> Option<LockedLayer> {
        self.tree.entries[entry].locked_by.map(|by| self.locked_layer(by))
    }

    /// Whether new elements may go into `entry`: a vector layer, not locked.
    fn judge_new_into(&self, entry: usize) -> Result<()> {
        let e = &self.tree.entries[entry];
        if !e.kind.is_vector() {
            return Err(BoardError::NotAVectorLayer { id: self.id(entry).to_string(), kind: e.kind.clone() });
        }
        match self.locked(entry) {
            Some(locked) => Err(BoardError::LayerLocked(locked)),
            None => Ok(()),
        }
    }

    /// The ids and orders of the layers in group `parent` (top level when
    /// `None`), bottom to top.
    fn sibling_orders(&self, parent: Option<usize>) -> Vec<(String, String)> {
        let list = match parent {
            Some(p) => &self.tree.entries[p].children,
            None => &self.tree.roots,
        };
        list.iter().map(|at| (self.id(*at).to_string(), self.raw_of(*at).order.clone())).collect()
    }

    /// The group a layer placed at `placement` goes into: where the layer
    /// the placement names is, else `parent` (`Some(None)` top level, `None`
    /// where `moving` is now, top level for a new layer). Refused when
    /// `parent` names no group, or disagrees with the named layer's place.
    fn target_parent(&self, moving: Option<usize>, parent: Option<Option<&str>>, placement: &Placement) -> Result<Option<usize>> {
        let named = match parent {
            Some(Some(pid)) => {
                let p = self.entry(pid).ok_or_else(|| BoardError::NoLayer(pid.to_string()))?;
                if !self.tree.entries[p].kind.is_group() {
                    return Err(BoardError::Invalid("Layers go only into a group.".into()));
                }
                Some(Some(p))
            }
            Some(None) => Some(None),
            None => None,
        };
        match placement {
            Placement::Above(other) | Placement::Below(other) => {
                let o = self.entry(other).ok_or_else(|| BoardError::NoLayer(other.clone()))?;
                let there = self.tree.entries[o].parent;
                if named.is_some_and(|p| p != there) {
                    return Err(BoardError::Invalid("That layer is not in that group.".into()));
                }
                Ok(there)
            }
            Placement::Top | Placement::Bottom => Ok(match named {
                Some(p) => p,
                None => moving.and_then(|m| self.tree.entries[m].parent),
            }),
        }
    }
}

impl<'a, 'doc> BoardTxn<'a, 'doc> {
    // ── The board ────────────────────────────────────────────────────────

    /// See [`Board::create`].
    pub fn create(&mut self) -> Result<String> {
        if self.board.is_some() {
            return self.bottom_layer().ok_or(BoardError::LastLayer);
        }
        // Intermediate maps (`boards` for a picture in text).
        let mut map = self.location.root.clone();
        let path = self.location.path.clone();
        for key in &path {
            map = match map.get(self.txn, key) {
                Some(Out::YMap(next)) => next,
                _ => map.insert(self.txn, key.clone(), MapPrelim::default()),
            };
        }
        let board = map;
        board.insert(self.txn, V, Any::BigInt(VERSION));
        board.insert(self.txn, MARKUP_VISIBLE, true);
        let layers: MapRef = board.insert(self.txn, LAYERS, MapPrelim::default());
        board.insert(self.txn, ELEMENTS, MapPrelim::default());
        let id = new_id();
        insert_layer(self.txn, &layers, &id, FIRST_LAYER_NAME, &order::after(None));
        self.board = Some(board);
        self.stamp_board();
        Ok(id)
    }

    fn board(&self) -> Result<MapRef> {
        self.board.clone().ok_or(BoardError::NoBoard)
    }

    /// Make this board a copy of `raw` (another board read whole,
    /// [`Board::read`]): its layers and elements with their ids, orders,
    /// styles, text, authors and timestamps, its page and its markup switch.
    /// The marks of a picture in text copied to another node, or to a new
    /// canvas node ("Convert to Node"), keep saying who made them
    /// (contract section 6: the name is stamped, not looked up). Refused
    /// when this board exists already: a copy is a new board.
    pub fn copy_from(&mut self, raw: &RawBoard) -> Result<()> {
        if self.board.is_some() {
            return Err(BoardError::Invalid("a canvas is copied only into a new one".into()));
        }
        let mut map = self.location.root.clone();
        let path = self.location.path.clone();
        for key in &path {
            map = match map.get(self.txn, key) {
                Some(Out::YMap(next)) => next,
                _ => map.insert(self.txn, key.clone(), MapPrelim::default()),
            };
        }
        let board = map;
        board.insert(self.txn, V, Any::BigInt(VERSION));
        board.insert(self.txn, MARKUP_VISIBLE, raw.markup_visible);
        let layers: MapRef = board.insert(self.txn, LAYERS, MapPrelim::default());
        let elements: MapRef = board.insert(self.txn, ELEMENTS, MapPrelim::default());
        self.board = Some(board);
        if let Some(page) = &raw.page {
            self.set_page(Some(page))?;
        }
        if raw.layers.is_empty() {
            insert_layer(self.txn, &layers, &new_id(), FIRST_LAYER_NAME, &order::after(None));
        }
        for layer in &raw.layers {
            insert_layer(self.txn, &layers, &layer.id, &layer.name, &layer.order);
            if let Some(Out::YMap(map)) = layers.get(self.txn, &layer.id) {
                map.insert(self.txn, VISIBLE, layer.visible);
                map.insert(self.txn, LOCKED, layer.locked);
                map.insert(self.txn, OPACITY, layer.opacity as f64);
                if !layer.kind.is_vector() {
                    map.insert(self.txn, KIND, layer.kind.as_str().to_string());
                }
                if let Some(blend) = layer.blend {
                    map.insert(self.txn, BLEND, blend.as_str());
                }
                if let Some(parent) = &layer.parent {
                    map.insert(self.txn, PARENT, parent.clone());
                }
                copy_meaning(self.txn, &map, layer.note.as_deref(), &layer.fields);
            }
        }
        for e in &raw.elements {
            let map: MapRef = elements.insert(self.txn, e.id.clone(), MapPrelim::default());
            map.insert(self.txn, LAYER, e.layer.clone());
            map.insert(self.txn, ORDER, e.order.clone());
            if let Some(parent) = &e.parent {
                map.insert(self.txn, PARENT, parent.clone());
            }
            map.insert(self.txn, KIND, e.kind.as_str().to_string());
            match &e.geom {
                Geom::Stroke => {
                    let array: ArrayRef = map.insert(self.txn, GEOM, ArrayPrelim::default());
                    if !e.points.is_empty() {
                        array.push_back(self.txn, Any::Buffer(pack_points(&e.points).into()));
                    }
                }
                geom => {
                    map.insert(self.txn, GEOM, geom_any(geom));
                }
            }
            if let Some(text) = &e.text {
                map.insert(self.txn, TEXT, TextPrelim::new(text.clone()));
            }
            let style: MapRef = map.insert(self.txn, STYLE, MapPrelim::default());
            write_style(self.txn, &style, &e.style);
            if !e.transform.is_identity() {
                map.insert(self.txn, TRANSFORM, transform_any(&e.transform));
            }
            map.insert(self.txn, LOCKED, e.locked);
            if let Some(author) = &e.author {
                map.insert(self.txn, AUTHOR, author_any(author));
            }
            map.insert(self.txn, CREATED_AT, e.created_at.clone());
            if let Some(by) = &e.edited_by {
                map.insert(self.txn, EDITED_BY, author_any(by));
            }
            if let Some(at) = &e.edited_at {
                map.insert(self.txn, EDITED_AT, at.clone());
            }
            copy_meaning(self.txn, &map, e.note.as_deref(), &e.fields);
        }
        if let Some(board) = self.board.clone() {
            for op in &raw.paint {
                let map: MapRef = board.insert(self.txn, crate::paint_ops::paint_key(&op.id), MapPrelim::default());
                write_paint_op(self.txn, &map, op);
            }
        }
        if let (Some(by), Some(at)) = (&raw.edited_by, &raw.edited_at) {
            if let Some(board) = self.board.clone() {
                board.insert(self.txn, EDITED_BY, author_any(by));
                board.insert(self.txn, EDITED_AT, at.clone());
            }
        }
        Ok(())
    }


    fn sub(&self, key: &str) -> Result<MapRef> {
        let board = self.board()?;
        match board.get(self.txn, key) {
            Some(Out::YMap(map)) => Ok(map),
            _ => Err(BoardError::NoBoard),
        }
    }

    fn layers(&self) -> Result<MapRef> {
        self.sub(LAYERS)
    }

    fn elements(&self) -> Result<MapRef> {
        self.sub(ELEMENTS)
    }

    fn stamp_board(&mut self) {
        if let (Some(board), Some(author)) = (self.board.clone(), self.stamp.author) {
            board.insert(self.txn, EDITED_BY, author_any(author));
            board.insert(self.txn, EDITED_AT, self.stamp.now.to_string());
        }
    }

    /// The markup toggle (contract section 5): off hides every element that
    /// is not a picture, for everyone.
    pub fn set_markup_visible(&mut self, visible: bool) -> Result<()> {
        let board = self.board()?;
        board.insert(self.txn, MARKUP_VISIBLE, visible);
        self.stamp_board();
        Ok(())
    }

    /// Give the board a page, or with `None` make it infinite again.
    pub fn set_page(&mut self, page: Option<&Page>) -> Result<()> {
        let board = self.board()?;
        match page {
            Some(page) => {
                let mut m = rect_fields(&page.rect);
                if let Some(fill) = page.fill {
                    m.insert("fill".into(), Any::String(fill.to_hex().into()));
                }
                board.insert(self.txn, PAGE, Any::Map(Arc::new(m)));
            }
            None => {
                board.remove(self.txn, PAGE);
            }
        }
        self.stamp_board();
        Ok(())
    }

    // ── Layers ───────────────────────────────────────────────────────────

    fn layer_map(&self, id: &str) -> Result<MapRef> {
        match self.layers()?.get(self.txn, id) {
            Some(Out::YMap(map)) => Ok(map),
            _ => Err(BoardError::NoLayer(id.to_string())),
        }
    }

    /// The layers as stored, read as a tree once per transaction until a
    /// layer is written ([`BoardTxn::layers_changed`]).
    fn tree(&self) -> Result<Rc<Layers>> {
        if let Some(cached) = self.layer_cache.borrow().as_ref() {
            return Ok(cached.clone());
        }
        let raw = read_layers(self.txn, &self.layers()?);
        let tree = crate::layer_tree::resolve(&raw);
        let layers = Rc::new(Layers { raw, tree });
        *self.layer_cache.borrow_mut() = Some(layers.clone());
        Ok(layers)
    }

    /// Forget the tree read so far: a layer was written.
    fn layers_changed(&mut self) {
        *self.layer_cache.borrow_mut() = None;
    }

    fn bottom_layer(&self) -> Option<String> {
        let ls = self.tree().ok()?;
        ls.tree.entries.iter().position(|e| !e.kind.is_group()).map(|at| ls.id(at).to_string())
    }

    /// The layer `id` when it is locked, itself or by a group it is in: the
    /// lock that guards it, by id and name (the group's when it is the
    /// group's).
    fn locked_layer(&self, id: &str) -> Result<Option<LockedLayer>> {
        let ls = self.tree()?;
        let at = ls.entry(id).ok_or_else(|| BoardError::NoLayer(id.to_string()))?;
        Ok(ls.locked(at))
    }

    /// The layer something new goes into: `layer` when given (refused when
    /// it is locked, itself or by a group, or holds no elements), else the
    /// top vector layer that is not locked and not in a hidden group
    /// ([`crate::BoardSnapshot::layer_for_new`] is the same rule, read).
    fn layer_for_new(&self, layer: Option<&str>) -> Result<String> {
        let ls = self.tree()?;
        match layer {
            Some(id) => {
                let at = ls.entry(id).ok_or_else(|| BoardError::NoLayer(id.to_string()))?;
                ls.judge_new_into(at)?;
                Ok(id.to_string())
            }
            None => match ls.tree.default_target() {
                Some(at) => Ok(ls.id(at).to_string()),
                None if ls.tree.has_vector() => Err(BoardError::EveryLayerLocked),
                None => Err(BoardError::NoVectorLayer),
            },
        }
    }

    /// The layer an element is drawn in: its own, or the top vector layer
    /// when its own is gone (as [`crate::BoardSnapshot`] resolves it).
    fn drawn_in(&self, element: &MapRef) -> Result<String> {
        let own = string_of(element.get(self.txn, LAYER)).unwrap_or_default();
        let ls = self.tree()?;
        ls.tree.home_of(&ls.raw, &own).map(|at| ls.id(at).to_string()).ok_or(BoardError::LastLayer)
    }

    /// A new vector layer at `placement`; answers its id. Placed above or
    /// below a layer in a group, it goes into that group.
    pub fn add_layer(&mut self, name: &str, placement: Placement) -> Result<String> {
        self.add_layer_in(name, LayerKind::Vector, None, placement)
    }

    /// A new group at `placement` in `parent` (top level when `None`);
    /// answers its id. It passes through until a blend mode is set.
    pub fn add_group(&mut self, name: &str, parent: Option<&str>, placement: Placement) -> Result<String> {
        self.add_layer_in(name, LayerKind::Group, parent, placement)
    }

    /// A new layer of `kind` in group `parent` (top level when `None`) at
    /// `placement` among that group's layers; answers its id. Placed above
    /// or below a layer, it goes where that layer is (refused when that is
    /// not `parent`, when `parent` is given).
    pub fn add_layer_in(&mut self, name: &str, kind: LayerKind, parent: Option<&str>, placement: Placement) -> Result<String> {
        if let LayerKind::Other(k) = &kind {
            return Err(BoardError::Invalid(format!("a layer is vector, paint or group, not {k}")));
        }
        let ls = self.tree()?;
        let parent = ls.target_parent(None, parent.map(Some), &placement)?;
        let key = place(&ls.sibling_orders(parent), None, &placement)?;
        let id = new_id();
        let layers = self.layers()?;
        insert_layer(self.txn, &layers, &id, name, &key);
        if let Some(Out::YMap(map)) = layers.get(self.txn, &id) {
            map.insert(self.txn, KIND, kind.as_str().to_string());
            if let Some(p) = parent {
                map.insert(self.txn, PARENT, ls.id(p).to_string());
            }
        }
        self.layers_changed();
        self.stamp_board();
        Ok(id)
    }

    /// Rename in place: the name is text, edited as the smallest change.
    pub fn rename_layer(&mut self, id: &str, name: &str) -> Result<()> {
        let layer = self.layer_map(id)?;
        let text = match layer.get(self.txn, NAME) {
            Some(Out::YText(text)) => text,
            _ => layer.insert(self.txn, NAME, TextPrelim::new("")),
        };
        edit_text(self.txn, &text, name);
        self.layers_changed();
        self.stamp_board();
        Ok(())
    }

    pub fn set_layer_visible(&mut self, id: &str, visible: bool) -> Result<()> {
        self.layer_map(id)?.insert(self.txn, VISIBLE, visible);
        self.layers_changed();
        self.stamp_board();
        Ok(())
    }

    pub fn set_layer_locked(&mut self, id: &str, locked: bool) -> Result<()> {
        self.layer_map(id)?.insert(self.txn, LOCKED, locked);
        self.layers_changed();
        self.stamp_board();
        Ok(())
    }

    pub fn set_layer_opacity(&mut self, id: &str, opacity: f32) -> Result<()> {
        self.layer_map(id)?.insert(self.txn, OPACITY, opacity.clamp(0.0, 1.0) as f64);
        self.layers_changed();
        self.stamp_board();
        Ok(())
    }

    /// A layer's blend mode (docs/DRAWING_CONTRACT.md section 1). Pass
    /// through is a group's alone.
    pub fn set_layer_blend(&mut self, id: &str, blend: Blend) -> Result<()> {
        let ls = self.tree()?;
        let at = ls.entry(id).ok_or_else(|| BoardError::NoLayer(id.to_string()))?;
        if blend == Blend::PassThrough && !ls.tree.entries[at].kind.is_group() {
            return Err(BoardError::Invalid("Pass Through is for groups. Pick another blend mode for a layer.".into()));
        }
        self.layer_map(id)?.insert(self.txn, BLEND, blend.as_str());
        self.layers_changed();
        self.stamp_board();
        Ok(())
    }

    /// Move a layer among its siblings: one key of one map, so two people
    /// moving the same layer at once leave it in one place, never twice.
    /// Placed above or below a layer in another group, it moves into that
    /// group.
    pub fn move_layer(&mut self, id: &str, placement: Placement) -> Result<()> {
        self.relocate(id, None, placement)
    }

    /// Move a layer into group `parent` (top level when `None`) at
    /// `placement` among that group's layers. Refused when `parent` is not
    /// a group, or is the layer itself or inside it.
    pub fn move_layer_into(&mut self, id: &str, parent: Option<&str>, placement: Placement) -> Result<()> {
        self.relocate(id, Some(parent), placement)
    }

    /// Put a layer on top of group `parent` (top level when `None`).
    pub fn set_layer_parent(&mut self, id: &str, parent: Option<&str>) -> Result<()> {
        self.relocate(id, Some(parent), Placement::Top)
    }

    /// Where a layer goes: `parent` `None` keeps its group (unless the
    /// placement names a layer elsewhere).
    fn relocate(&mut self, id: &str, parent: Option<Option<&str>>, placement: Placement) -> Result<()> {
        let ls = self.tree()?;
        let at = ls.entry(id).ok_or_else(|| BoardError::NoLayer(id.to_string()))?;
        if let Placement::Above(other) | Placement::Below(other) = &placement {
            if other == id {
                return Err(BoardError::Invalid("a layer is placed next to another layer, not itself".into()));
            }
        }
        let parent = ls.target_parent(Some(at), parent, &placement)?;
        if parent.is_some_and(|p| ls.tree.is_within(p, at)) {
            return Err(BoardError::Invalid("A group cannot go inside itself or a group it holds.".into()));
        }
        let key = place(&ls.sibling_orders(parent), Some(id), &placement)?;
        let map = self.layer_map(id)?;
        map.insert(self.txn, ORDER, key);
        match parent {
            Some(p) => {
                map.insert(self.txn, PARENT, ls.id(p).to_string());
            }
            None => {
                map.remove(self.txn, PARENT);
            }
        }
        self.layers_changed();
        self.stamp_board();
        Ok(())
    }

    /// "Group Layers": a new group named `name` at the place of the topmost
    /// of `ids`, holding them in their order. A layer whose group is also
    /// picked stays in that group. Answers the group's id.
    pub fn group_layers(&mut self, ids: &[String], name: &str) -> Result<String> {
        let ls = self.tree()?;
        let mut picked: Vec<usize> = Vec::with_capacity(ids.len());
        for id in ids {
            let at = ls.entry(id).ok_or_else(|| BoardError::NoLayer(id.clone()))?;
            if !picked.contains(&at) {
                picked.push(at);
            }
        }
        let outer: Vec<usize> = picked.iter().copied().filter(|at| !ls.tree.ancestors(*at).any(|a| picked.contains(&a))).collect();
        // Entries are in draw order: the largest is the topmost.
        let Some(&topmost) = outer.iter().max() else {
            return Err(BoardError::Invalid("pick the layers to group".into()));
        };
        let parent = ls.tree.entries[topmost].parent;
        let key = place(&ls.sibling_orders(parent), None, &Placement::Above(ls.id(topmost).to_string()))?;
        let gid = new_id();
        let layers = self.layers()?;
        insert_layer(self.txn, &layers, &gid, name, &key);
        if let Some(Out::YMap(map)) = layers.get(self.txn, &gid) {
            map.insert(self.txn, KIND, LayerKind::Group.as_str().to_string());
            if let Some(p) = parent {
                map.insert(self.txn, PARENT, ls.id(p).to_string());
            }
        }
        let mut members = outer;
        members.sort_unstable();
        let keys = order::n_between(None, None, members.len());
        for (at, key) in members.iter().zip(keys) {
            let map = self.layer_map(ls.id(*at))?;
            map.insert(self.txn, PARENT, gid.clone());
            map.insert(self.txn, ORDER, key);
        }
        self.layers_changed();
        self.stamp_board();
        Ok(gid)
    }

    /// "Ungroup": the group's layers move up to its place, in their order,
    /// and the group goes (its own visibility, lock and opacity with it;
    /// the layers keep theirs). Refused when the group is locked.
    pub fn ungroup(&mut self, id: &str) -> Result<()> {
        let ls = self.tree()?;
        let at = ls.entry(id).ok_or_else(|| BoardError::NoLayer(id.to_string()))?;
        if !ls.tree.entries[at].kind.is_group() {
            return Err(BoardError::Invalid("That layer is not a group.".into()));
        }
        if let Some(locked) = ls.locked(at) {
            return Err(BoardError::LayerLockedAgainstRemoval(locked));
        }
        let parent = ls.tree.entries[at].parent;
        let siblings = ls.sibling_orders(parent);
        let i = siblings.iter().position(|(s, _)| s == id).unwrap_or(0);
        let below = i.checked_sub(1).map(|b| siblings[b].1.clone());
        let above = siblings.get(i + 1).map(|(_, o)| o.clone());
        let children = ls.tree.entries[at].children.clone();
        let keys = order::n_between(below.as_deref(), above.as_deref(), children.len());
        for (child, key) in children.iter().zip(keys) {
            let map = self.layer_map(ls.id(*child))?;
            map.insert(self.txn, ORDER, key);
            match parent {
                Some(p) => {
                    map.insert(self.txn, PARENT, ls.id(p).to_string());
                }
                None => {
                    map.remove(self.txn, PARENT);
                }
            }
        }
        self.layers()?.remove(self.txn, id);
        self.layers_changed();
        self.stamp_board();
        Ok(())
    }

    /// Delete a layer with its elements; a group with every layer in it and
    /// their elements (one write, so one undo). A board keeps at least one
    /// layer that is not a group, and a locked layer stays: locked itself,
    /// by a group it is in, or (deleting a group) any layer inside it. The
    /// lock judged is the layers': deleting a layer is a deliberate act on
    /// the layer, and takes its elements with it whether or not they are
    /// locked one by one.
    pub fn delete_layer(&mut self, id: &str) -> Result<()> {
        let ls = self.tree()?;
        let at = ls.entry(id).ok_or_else(|| BoardError::NoLayer(id.to_string()))?;
        if let Some(locked) = ls.locked(at) {
            return Err(BoardError::LayerLockedAgainstRemoval(locked));
        }
        let mut doomed = ls.tree.descendants(at);
        if let Some(inner) = doomed.iter().find(|d| ls.raw_of(**d).locked) {
            return Err(BoardError::LayerLockedAgainstRemoval(ls.locked_layer(*inner)));
        }
        doomed.push(at);
        let left = ls.tree.entries.iter().enumerate().filter(|(i, e)| !e.kind.is_group() && !doomed.contains(i)).count();
        if left == 0 {
            return Err(BoardError::LastLayer);
        }
        let doomed_ids: HashSet<String> = doomed.iter().map(|d| ls.id(*d).to_string()).collect();
        let elements = self.elements()?;
        let gone: Vec<String> = elements
            .iter(self.txn)
            .filter_map(|(eid, out)| match out {
                Out::YMap(map) if string_of(map.get(self.txn, LAYER)).is_some_and(|l| doomed_ids.contains(&l)) => Some(eid.to_string()),
                _ => None,
            })
            .collect();
        for eid in gone {
            elements.remove(self.txn, &eid);
        }
        let board = self.board()?;
        for key in self.paint_keys_in(&doomed_ids) {
            board.remove(self.txn, &key);
        }
        let layers = self.layers()?;
        for d in &doomed_ids {
            layers.remove(self.txn, d);
            self.tops.remove(d);
        }
        self.layers_changed();
        self.stamp_board();
        Ok(())
    }

    /// "Merge Down": the layer's elements go on top of the layer below it in
    /// its group, in their order, and the layer goes. Refused when either
    /// layer is locked; elements locked one by one go along, still locked
    /// (as with [`BoardTxn::delete_layer`], the act is on the layer). Only
    /// vector layers merge for now: a group merges with
    /// [`BoardTxn::merge_group`], and paint layers wait for painting.
    pub fn merge_layer_down(&mut self, id: &str) -> Result<()> {
        let ls = self.tree()?;
        let at = ls.entry(id).ok_or_else(|| BoardError::NoLayer(id.to_string()))?;
        if ls.tree.entries[at].kind.is_group() {
            return Err(BoardError::Invalid("That is a group. Merge it with Merge Group.".into()));
        }
        let siblings = ls.tree.siblings(at);
        let pos = siblings.iter().position(|s| *s == at).unwrap_or(0);
        if pos == 0 {
            return Err(BoardError::Invalid("the bottom layer has nothing below it".into()));
        }
        let below = siblings[pos - 1];
        let (kind, below_kind) = (&ls.tree.entries[at].kind, &ls.tree.entries[below].kind);
        if below_kind.is_group() {
            return Err(BoardError::Invalid("The layer below is a group. Move this layer into it, or merge the group first.".into()));
        }
        match (kind.is_vector(), below_kind.is_vector()) {
            (true, true) => {}
            (false, true) => return Err(BoardError::Invalid(PAINT_ONTO_VECTOR.into())),
            (_, false) => return Err(BoardError::NeedsPixels("Merging onto a paint layer draws the layer as pixels first.".into())),
        }
        if let Some(locked) = ls.locked(at) {
            return Err(BoardError::LayerLockedAgainstRemoval(locked));
        }
        if let Some(locked) = ls.locked(below) {
            return Err(BoardError::LayerLocked(locked));
        }
        let below = ls.id(below).to_string();
        let ids = self.element_ids_in_layer(id)?;
        self.move_into_layer(&ids, &below, false)?;
        self.layers()?.remove(self.txn, id);
        self.tops.remove(id);
        self.layers_changed();
        Ok(())
    }

    /// "Merge Group": every vector layer in the group, at any depth, becomes
    /// one vector layer in the group's place, with the group's id, name,
    /// visibility, opacity and blend (pass-through reads as normal), the
    /// elements in their drawing order. The layers' own opacity, blend and
    /// visibility are not carried into the elements. Refused when the group
    /// or any layer in it is locked, and (until painting) when it holds a
    /// paint layer.
    pub fn merge_group(&mut self, id: &str) -> Result<()> {
        let ls = self.tree()?;
        let at = ls.entry(id).ok_or_else(|| BoardError::NoLayer(id.to_string()))?;
        if !ls.tree.entries[at].kind.is_group() {
            return Err(BoardError::Invalid("That layer is not a group.".into()));
        }
        if let Some(locked) = ls.locked(at) {
            return Err(BoardError::LayerLockedAgainstRemoval(locked));
        }
        let inside = ls.tree.descendants(at);
        if let Some(inner) = inside.iter().find(|d| ls.raw_of(**d).locked) {
            return Err(BoardError::LayerLockedAgainstRemoval(ls.locked_layer(*inner)));
        }
        if inside.iter().any(|d| ls.tree.entries[*d].kind == LayerKind::Paint) {
            return Err(BoardError::NeedsPixels("That group holds a paint layer, so merging it draws it as pixels first.".into()));
        }
        let rank: HashMap<String, usize> = inside.iter().map(|d| (ls.id(*d).to_string(), *d)).collect();
        let elements = self.elements()?;
        let mut moving: Vec<(usize, String, String, MapRef)> = elements
            .iter(self.txn)
            .filter_map(|(eid, out)| match out {
                Out::YMap(map) => {
                    let r = *rank.get(&string_of(map.get(self.txn, LAYER))?)?;
                    Some((r, string_of(map.get(self.txn, ORDER)).unwrap_or_default(), eid.to_string(), map))
                }
                _ => None,
            })
            .collect();
        moving.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.cmp(&b.2)));
        let keys = order::n_between(None, None, moving.len());
        for ((_, _, _, map), key) in moving.into_iter().zip(keys.iter()) {
            map.insert(self.txn, LAYER, id.to_string());
            map.insert(self.txn, ORDER, key.clone());
        }
        let group = self.layer_map(id)?;
        group.insert(self.txn, KIND, LayerKind::Vector.as_str().to_string());
        if ls.tree.entries[at].blend == Blend::PassThrough {
            group.remove(self.txn, BLEND);
        }
        let layers = self.layers()?;
        for d in rank.keys() {
            layers.remove(self.txn, d);
            self.tops.remove(d);
        }
        self.tops.insert(id.to_string(), keys.last().cloned());
        self.layers_changed();
        self.stamp_board();
        Ok(())
    }

    /// "Move Selection to Layer": the elements go on top of `layer`, keeping
    /// their order among themselves. Refused, before anything is written,
    /// when `layer` is locked (itself or by a group), is not a vector layer,
    /// or any of the elements is locked.
    pub fn move_to_layer(&mut self, ids: &[String], layer: &str) -> Result<()> {
        let ls = self.tree()?;
        let at = ls.entry(layer).ok_or_else(|| BoardError::NoLayer(layer.to_string()))?;
        ls.judge_new_into(at)?;
        self.move_into_layer(ids, layer, true)
    }

    fn move_into_layer(&mut self, ids: &[String], layer: &str, judge_locks: bool) -> Result<()> {
        self.layer_map(layer)?;
        let elements = self.elements()?;
        let mut found = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(Out::YMap(map)) = elements.get(self.txn, id) else {
                return Err(BoardError::NoElement(id.clone()));
            };
            if judge_locks {
                self.judge_element(id, &map)?;
            }
            let key = string_of(map.get(self.txn, ORDER)).unwrap_or_default();
            let from = string_of(map.get(self.txn, LAYER)).unwrap_or_default();
            found.push((from, key, id.clone(), map));
        }
        found.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        let top = self.top_order(layer)?;
        let keys = order::n_between(top.as_deref(), None, found.len());
        for ((_, _, _, map), key) in found.into_iter().zip(keys.iter()) {
            map.insert(self.txn, LAYER, layer.to_string());
            map.insert(self.txn, ORDER, key.clone());
            self.stamp_element(&map);
        }
        if let Some(last) = keys.last() {
            self.tops.insert(layer.to_string(), Some(last.clone()));
        }
        Ok(())
    }

    fn element_ids_in_layer(&self, layer: &str) -> Result<Vec<String>> {
        let elements = self.elements()?;
        Ok(elements
            .iter(self.txn)
            .filter_map(|(eid, out)| match out {
                Out::YMap(map) if string_of(map.get(self.txn, LAYER)).as_deref() == Some(layer) => Some(eid.to_string()),
                _ => None,
            })
            .collect())
    }

    // ── Elements ─────────────────────────────────────────────────────────

    fn element(&self, id: &str) -> Result<MapRef> {
        match self.elements()?.get(self.txn, id) {
            Some(Out::YMap(map)) => Ok(map),
            _ => Err(BoardError::NoElement(id.to_string())),
        }
    }

    /// Element `id` for an edit the lock guards: refused when it or the
    /// layer it is drawn in is locked.
    fn editable(&self, id: &str) -> Result<MapRef> {
        let map = self.element(id)?;
        self.judge_element(id, &map)?;
        Ok(map)
    }

    fn judge_element(&self, id: &str, map: &MapRef) -> Result<()> {
        let layer = self.drawn_in(map)?;
        let element_locked = bool_of(map.get(self.txn, LOCKED)).unwrap_or(false);
        crate::locks::judge_element(id, element_locked, self.locked_layer(&layer)?)
    }

    fn stamp_element(&mut self, element: &MapRef) {
        if let Some(author) = self.stamp.author {
            element.insert(self.txn, EDITED_BY, author_any(author));
            element.insert(self.txn, EDITED_AT, self.stamp.now.to_string());
        }
    }

    /// Element ids of `layer` with their orders, bottom to top.
    fn element_orders(&self, layer: &str) -> Result<Vec<(String, String)>> {
        let elements = self.elements()?;
        let mut out: Vec<(String, String)> = elements
            .iter(self.txn)
            .filter_map(|(eid, out)| match out {
                Out::YMap(map) if string_of(map.get(self.txn, LAYER)).as_deref() == Some(layer) => {
                    Some((eid.to_string(), string_of(map.get(self.txn, ORDER)).unwrap_or_default()))
                }
                _ => None,
            })
            .collect();
        out.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        Ok(out)
    }

    fn top_order(&mut self, layer: &str) -> Result<Option<String>> {
        if let Some(top) = self.tops.get(layer) {
            return Ok(top.clone());
        }
        let top = self.element_orders(layer)?.into_iter().last().map(|(_, o)| o);
        self.tops.insert(layer.to_string(), top.clone());
        Ok(top)
    }

    /// Add an element; answers its new id. It goes into `element.layer` (the
    /// top layer that is not locked when `None`; a locked layer takes no new
    /// elements), at `element.placement` (on top when `None`), stamped
    /// `author` and `created_at`.
    pub fn add_element(&mut self, element: NewElement) -> Result<String> {
        let layer = self.layer_for_new(element.layer.as_deref())?;
        if element.kind.is_stroke() != matches!(element.geom, Geom::Stroke) {
            return Err(BoardError::Invalid(format!("a {} takes {}", element.kind, if element.kind.is_stroke() { "stroke geometry" } else { "geometry of its own" })));
        }
        judge_geom(&element.kind, &element.geom)?;
        let key = match &element.placement {
            None | Some(Placement::Top) => {
                let top = self.top_order(&layer)?;
                let key = order::after(top.as_deref());
                self.tops.insert(layer.clone(), Some(key.clone()));
                key
            }
            Some(placement) => {
                let orders = self.element_orders(&layer)?;
                let key = place(&orders, None, placement)?;
                self.tops.remove(&layer);
                key
            }
        };
        let id = new_id();
        let elements = self.elements()?;
        let map: MapRef = elements.insert(self.txn, id.clone(), MapPrelim::default());
        map.insert(self.txn, LAYER, layer);
        map.insert(self.txn, ORDER, key);
        if let Some(parent) = &element.parent {
            map.insert(self.txn, PARENT, parent.clone());
        }
        map.insert(self.txn, KIND, element.kind.as_str().to_string());
        match &element.geom {
            Geom::Stroke => {
                let array: ArrayRef = map.insert(self.txn, GEOM, ArrayPrelim::default());
                if !element.points.is_empty() {
                    array.push_back(self.txn, Any::Buffer(pack_points(&element.points).into()));
                }
            }
            geom => {
                map.insert(self.txn, GEOM, geom_any(geom));
            }
        }
        if let Some(text) = &element.text {
            map.insert(self.txn, TEXT, TextPrelim::new(text.clone()));
        }
        let style: MapRef = map.insert(self.txn, STYLE, MapPrelim::default());
        write_style(self.txn, &style, &element.style);
        if let Some(transform) = &element.transform {
            if !transform.is_identity() {
                map.insert(self.txn, TRANSFORM, transform_any(transform));
            }
        }
        map.insert(self.txn, LOCKED, false);
        if let Some(author) = self.stamp.author {
            map.insert(self.txn, AUTHOR, author_any(author));
        }
        map.insert(self.txn, CREATED_AT, self.stamp.now.to_string());
        Ok(id)
    }

    /// Append points (x, y, pressure) to a pen or highlighter stroke: one
    /// chunk, so a stroke being drawn is seen as it is drawn (contract: every
    /// ~100 ms). Appending is the drawing itself, not an edit of someone
    /// else's mark, so it does not stamp `edited_by`.
    pub fn append_points(&mut self, id: &str, points: &[[f32; 3]]) -> Result<()> {
        let map = self.editable(id)?;
        let array = match map.get(self.txn, GEOM) {
            Some(Out::YArray(array)) => array,
            _ => return Err(BoardError::NotAStroke(id.to_string())),
        };
        if !points.is_empty() {
            array.push_back(self.txn, Any::Buffer(pack_points(points).into()));
        }
        Ok(())
    }

    /// Replace a stroke's points whole (smoothing once on release, contract
    /// section 3): the old chunks are removed and one chunk written.
    pub fn replace_points(&mut self, id: &str, points: &[[f32; 3]]) -> Result<()> {
        let map = self.editable(id)?;
        let array = match map.get(self.txn, GEOM) {
            Some(Out::YArray(array)) => array,
            _ => return Err(BoardError::NotAStroke(id.to_string())),
        };
        let len = array.len(self.txn);
        if len > 0 {
            array.remove_range(self.txn, 0, len);
        }
        if !points.is_empty() {
            array.push_back(self.txn, Any::Buffer(pack_points(points).into()));
        }
        self.stamp_element(&map);
        Ok(())
    }

    /// Replace the geometry (one value, last writer wins). A stroke's
    /// geometry is its points: use [`BoardTxn::replace_points`], or move it
    /// with its transform.
    pub fn set_geom(&mut self, id: &str, geom: &Geom) -> Result<()> {
        let map = self.editable(id)?;
        if matches!(geom, Geom::Stroke) || matches!(map.get(self.txn, GEOM), Some(Out::YArray(_))) {
            return Err(BoardError::Invalid("a stroke's geometry is its points".into()));
        }
        let kind = Kind::parse(&string_of(map.get(self.txn, KIND)).unwrap_or_default());
        judge_geom(&kind, geom)?;
        map.insert(self.txn, GEOM, geom_any(geom));
        self.stamp_element(&map);
        Ok(())
    }

    /// Set the style keys that are `Some`; the others stay as they are.
    pub fn set_style(&mut self, id: &str, style: &Style) -> Result<()> {
        let map = self.editable(id)?;
        let style_map = match map.get(self.txn, STYLE) {
            Some(Out::YMap(m)) => m,
            _ => map.insert(self.txn, STYLE, MapPrelim::default()),
        };
        write_style(self.txn, &style_map, style);
        self.stamp_element(&map);
        Ok(())
    }

    /// Clear one style key, back to the kind's default. Clearing `fill` or
    /// `stroke` clears its gradient too (the solid key is the gradient's
    /// fallback, so "No fill" leaves no gradient behind).
    pub fn clear_style(&mut self, id: &str, key: StyleKey) -> Result<()> {
        let map = self.editable(id)?;
        if let Some(Out::YMap(style)) = map.get(self.txn, STYLE) {
            let mut changed = style.remove(self.txn, key.as_str()).is_some();
            let partner = match key {
                StyleKey::Fill => Some(StyleKey::FillGradient),
                StyleKey::Stroke => Some(StyleKey::StrokeGradient),
                _ => None,
            };
            if let Some(partner) = partner {
                changed |= style.remove(self.txn, partner.as_str()).is_some();
            }
            if changed {
                self.stamp_element(&map);
            }
        }
        Ok(())
    }

    /// What fills an element: nothing (the fill and its gradient cleared),
    /// a solid color (any gradient removed), or a gradient (its first stop's
    /// color written as the solid fallback an older app draws). What the
    /// color well writes; `set_style` alone would leave a gradient winning
    /// over a solid color picked after it.
    pub fn set_fill_paint(&mut self, id: &str, paint: &Paint) -> Result<()> {
        self.set_paint(id, paint, StyleKey::Fill, StyleKey::FillGradient)
    }

    /// What an element's stroke is painted with: a solid color or a
    /// gradient. A stroke is never "none" here (refused with a sentence): a
    /// stroke goes by giving it width 0.
    pub fn set_stroke_paint(&mut self, id: &str, paint: &Paint) -> Result<()> {
        if *paint == Paint::None {
            return Err(BoardError::Invalid("A stroke cannot be none; set its width to 0 instead.".into()));
        }
        self.set_paint(id, paint, StyleKey::Stroke, StyleKey::StrokeGradient)
    }

    fn set_paint(&mut self, id: &str, paint: &Paint, solid: StyleKey, gradient: StyleKey) -> Result<()> {
        let map = self.editable(id)?;
        if let Paint::Gradient(g) = paint {
            if !g.is_valid() {
                return Err(BoardError::Invalid("A gradient needs at least one stop and handles that are numbers (a radial one some radius).".into()));
            }
        }
        let style = match map.get(self.txn, STYLE) {
            Some(Out::YMap(m)) => m,
            _ => map.insert(self.txn, STYLE, MapPrelim::default()),
        };
        match paint {
            Paint::None => {
                style.remove(self.txn, solid.as_str());
                style.remove(self.txn, gradient.as_str());
            }
            Paint::Solid(c) => {
                style.insert(self.txn, solid.as_str(), c.to_hex());
                style.remove(self.txn, gradient.as_str());
            }
            Paint::Gradient(g) => {
                style.insert(self.txn, gradient.as_str(), gradient_any(g));
                style.insert(self.txn, solid.as_str(), g.fallback().to_hex());
            }
        }
        self.stamp_element(&map);
        Ok(())
    }

    /// The transform, written whole (identity removes it).
    pub fn set_transform(&mut self, id: &str, transform: &Transform) -> Result<()> {
        let map = self.editable(id)?;
        if transform.is_identity() {
            map.remove(self.txn, TRANSFORM);
        } else {
            map.insert(self.txn, TRANSFORM, transform_any(transform));
        }
        self.stamp_element(&map);
        Ok(())
    }

    /// The text, edited as the smallest change from what is there (the
    /// common start and end kept), so someone typing elsewhere in the same
    /// sticky note keeps their typing.
    pub fn set_text(&mut self, id: &str, text: &str) -> Result<()> {
        let map = self.editable(id)?;
        let text_ref = match map.get(self.txn, TEXT) {
            Some(Out::YText(t)) => t,
            _ => map.insert(self.txn, TEXT, TextPrelim::new("")),
        };
        edit_text(self.txn, &text_ref, text);
        self.stamp_element(&map);
        Ok(())
    }

    /// Lock or unlock an element. Its own lock never stands in the way of
    /// this (it is how a locked element is unlocked); a locked layer does.
    pub fn set_locked(&mut self, id: &str, locked: bool) -> Result<()> {
        let map = self.element(id)?;
        let layer = self.drawn_in(&map)?;
        crate::locks::judge_element(id, false, self.locked_layer(&layer)?)?;
        map.insert(self.txn, LOCKED, locked);
        self.stamp_element(&map);
        Ok(())
    }

    /// Membership in a frame or group (`None`: none).
    pub fn set_parent(&mut self, id: &str, parent: Option<&str>) -> Result<()> {
        let map = self.editable(id)?;
        if let Some(p) = parent {
            if p == id {
                return Err(BoardError::Invalid("an element cannot contain itself".into()));
            }
            self.element(p)?;
            map.insert(self.txn, PARENT, p.to_string());
        } else {
            map.remove(self.txn, PARENT);
        }
        self.stamp_element(&map);
        Ok(())
    }

    /// Bring forward or send back within the element's layer (writes
    /// `order`).
    pub fn move_element(&mut self, id: &str, placement: Placement) -> Result<()> {
        let map = self.editable(id)?;
        let layer = string_of(map.get(self.txn, LAYER)).unwrap_or_default();
        let orders = self.element_orders(&layer)?;
        let key = place(&orders, Some(id), &placement)?;
        map.insert(self.txn, ORDER, key);
        self.tops.remove(&layer);
        self.stamp_element(&map);
        Ok(())
    }

    // ── Paths (docs/DRAWING_CONTRACT.md section 2) ───────────────────────

    /// Turn an element into a `path` in this same edit (Direct select on a
    /// live shape: "turns it into a path in that same edit"): a rectangle
    /// (with its radius), ellipse, diamond, polygon, star, a line with no
    /// arrow heads, or a pen stroke ([`crate::path::to_path`]). It keeps its
    /// id, layer, order, parent, style, text, meaning and lock; its
    /// transform is adjusted so it stays exactly where it was drawn (a
    /// path's rotation pivots on its own extent). Answers the path written
    /// (in element space); a path is answered as it is, unchanged. Refused
    /// when locked or when the kind has no path ("Only shapes, lines and pen
    /// strokes turn into paths.").
    pub fn convert_to_path(&mut self, id: &str) -> Result<crate::path::PathGeom> {
        let map = self.editable(id)?;
        let raw = read_element(self.txn, id, &map);
        if raw.kind == Kind::Path {
            if let Geom::Path(p) = raw.geom {
                return Ok(p);
            }
        }
        let path = crate::path::to_path(&raw.kind, &raw.geom, &raw.style, &raw.points)
            .ok_or_else(|| BoardError::Invalid("Only shapes, lines without arrow heads and pen strokes turn into paths.".into()))?;
        self.write_as_path(&map, &raw, &path);
        Ok(path)
    }

    /// Finish a Pencil stroke: the pen element `id` streamed while drawing
    /// (others watched it grow, as a pen) becomes a `path` fitted to
    /// `points` (the whole stroke, as the drawer's device holds it) within
    /// `tolerance` board units ([`crate::path::fit_stroke`]), in one write.
    /// It keeps its id, layer, order and style. Refused when it is not a
    /// pen or highlighter stroke, or when the points fit to nothing.
    pub fn stroke_to_path(&mut self, id: &str, points: &[[f32; 3]], tolerance: f32) -> Result<crate::path::PathGeom> {
        let map = self.editable(id)?;
        if !matches!(map.get(self.txn, GEOM), Some(Out::YArray(_))) {
            return Err(BoardError::NotAStroke(id.to_string()));
        }
        let path = crate::path::fit_stroke(points, tolerance);
        if path.is_empty() {
            return Err(BoardError::Invalid("the stroke has no points".into()));
        }
        let mut raw = read_element(self.txn, id, &map);
        // The streamed points' extent is the pivot the transform was made for.
        raw.points = points.to_vec();
        self.write_as_path(&map, &raw, &path);
        Ok(path)
    }

    /// A new `path` element fitted to freehand `points` within `tolerance`
    /// (a Pencil stroke released before anything was streamed); answers its
    /// id. Goes where [`BoardTxn::add_element`] puts `element` (its kind and
    /// geometry are replaced by the fitted path).
    pub fn add_fitted_path(&mut self, points: &[[f32; 3]], tolerance: f32, mut element: NewElement) -> Result<String> {
        let path = crate::path::fit_stroke(points, tolerance);
        if path.is_empty() {
            return Err(BoardError::Invalid("the stroke has no points".into()));
        }
        element.kind = Kind::Path;
        element.geom = Geom::Path(path);
        element.points.clear();
        self.add_element(element)
    }

    /// Write `path` over an element read as `raw`: kind, geometry (a stroke's
    /// point array replaced by one value) and a transform that keeps it in
    /// place.
    fn write_as_path(&mut self, map: &MapRef, raw: &RawElement, path: &crate::path::PathGeom) {
        let style = raw.style.resolved(&raw.kind);
        let old_pivot = crate::snapshot::pivot_of(raw, &style);
        let new_pivot = path.bounds();
        map.insert(self.txn, KIND, Kind::Path.as_str().to_string());
        map.insert(self.txn, GEOM, geom_any(&Geom::Path(path.clone())));
        if let (Some(old), Some(new)) = (old_pivot, new_pivot) {
            let t = repivot(&raw.transform, old, new);
            if t != raw.transform {
                if t.is_identity() {
                    map.remove(self.txn, TRANSFORM);
                } else {
                    map.insert(self.txn, TRANSFORM, transform_any(&t));
                }
            }
        }
        self.stamp_element(map);
    }

    // ── Combining, flipping, SVG in (drawing wave 3) ─────────────────────

    /// The board as it stands in this transaction, resolved (world
    /// transforms, extents, z-order).
    fn snapshot_now(&self) -> Result<crate::BoardSnapshot> {
        let board = self.board()?;
        Ok(crate::BoardSnapshot::build(Some(read_board(self.txn, &board, true))))
    }

    /// Combine two or more elements into one `path` (docs/DRAWING_CONTRACT.md
    /// section 2, "Combining"), in this one transaction (one undo): their
    /// filled areas, each by its own fill rule and through its transform
    /// (and its group's), combined by `op` ([`crate::combine::combine`]).
    ///
    /// The result keeps the id, layer, order, group, style, text and meaning
    /// of the **top** operand (z-order), except for
    /// [`CombineOp::Subtract`], which is the bottom operand minus everything
    /// above it and keeps the **bottom** one's (Illustrator's "Minus
    /// Front"). Its geometry is written in its group's space with no
    /// transform of its own; the other operands are deleted. Answers the
    /// result's id.
    ///
    /// Refused, before anything is written: fewer than two elements; an
    /// element that has no outline to combine ("Only shapes, lines without
    /// arrow heads, pen strokes and paths combine: a text and a sticky note
    /// do not."); a locked one; a result with nothing left ("The shapes do
    /// not overlap, so intersecting them leaves nothing.").
    pub fn combine(&mut self, ids: &[String], op: crate::combine::CombineOp) -> Result<String> {
        use crate::combine::CombineOp;
        let mut seen = HashSet::new();
        let ids: Vec<&String> = ids.iter().filter(|id| seen.insert(id.as_str())).collect();
        if ids.len() < 2 {
            return Err(BoardError::Invalid("Combining takes two or more elements.".into()));
        }
        for id in &ids {
            self.editable(id)?;
        }
        let snap = self.snapshot_now()?;
        let mut found: Vec<(usize, crate::path::PathGeom)> = Vec::with_capacity(ids.len());
        let mut refused: Vec<String> = Vec::new();
        for id in &ids {
            let at = snap.elements.iter().position(|e| e.id() == id.as_str()).ok_or_else(|| BoardError::NoElement(id.to_string()))?;
            let e = &snap.elements[at];
            match crate::path::to_path(&e.raw.kind, &e.raw.geom, &e.raw.style, &e.raw.points) {
                Some(p) => {
                    let w = e.world;
                    found.push((at, p.map(|q| map_point(&w, q))));
                }
                None => {
                    let word = e.raw.kind.with_article();
                    if !refused.contains(&word) {
                        refused.push(word);
                    }
                }
            }
        }
        if !refused.is_empty() {
            let list = match refused.as_slice() {
                [one] => format!("{one} does not"),
                [rest @ .., last] => format!("{} and {last} do not", rest.join(", ")),
                [] => unreachable!(),
            };
            return Err(BoardError::Invalid(format!("Only shapes, lines without arrow heads, pen strokes and paths combine: {list}.")));
        }
        found.sort_by_key(|(at, _)| *at);
        let operands: Vec<crate::path::PathGeom> = found.iter().map(|(_, p)| p.clone()).collect();
        let result = crate::combine::combine(&operands, op).ok_or_else(|| {
            BoardError::Invalid(
                match op {
                    CombineOp::Intersect => "The shapes do not overlap, so intersecting them leaves nothing.",
                    CombineOp::Subtract => "Nothing is left: the shapes in front cover the one at the back.",
                    _ => "Nothing is left of the shapes.",
                }
                .into(),
            )
        })?;
        let keep_at = if op == CombineOp::Subtract { found[0].0 } else { found[found.len() - 1].0 };
        let keeper = &snap.elements[keep_at];
        let keep_id = keeper.id().to_string();
        // Into the keeper's group's space.
        let parent_world = keeper.parent.as_deref().and_then(|p| snap.get(p)).map(|p| p.world).unwrap_or_default();
        let inv = parent_world.invert().unwrap_or_default();
        let geom = result.map(|q| map_point(&inv, q));
        let map = self.element(&keep_id)?;
        map.insert(self.txn, KIND, Kind::Path.as_str().to_string());
        map.insert(self.txn, GEOM, geom_any(&Geom::Path(geom)));
        map.remove(self.txn, TRANSFORM);
        self.stamp_element(&map);
        let elements = self.elements()?;
        for (at, _) in &found {
            if *at != keep_at {
                elements.remove(self.txn, snap.elements[*at].id());
            }
        }
        Ok(keep_id)
    }

    /// Outline Stroke (docs/DRAWING_CONTRACT.md decision 9): each element's
    /// stroke becomes a filled path of the stroke's outline (its width,
    /// ends, corners, miter limit and dashes), in this one transaction (one
    /// undo). The outline is filled with the stroke's color (or its
    /// gradient) and has no stroke of its own; it is in the element's own
    /// space under a transform that keeps it exactly where the stroke was
    /// drawn.
    ///
    /// An element with nothing else to show (no fill, no text) **becomes**
    /// the outline, keeping its id, layer, order, group and meaning. One
    /// that has a fill or text keeps them and loses its stroke, and the
    /// outline is a new path right above it, in its group. Answers the
    /// outlines' ids, in the order asked.
    ///
    /// Refused, before anything is written: an element that has no outline
    /// ("Only shapes, lines without arrow heads, pen strokes and paths have
    /// a stroke to outline: a text does not."), one with no stroke ("... has
    /// no stroke to outline."), a locked one.
    pub fn outline_stroke(&mut self, ids: &[String]) -> Result<Vec<String>> {
        let mut seen = HashSet::new();
        let ids: Vec<&String> = ids.iter().filter(|id| seen.insert(id.as_str())).collect();
        if ids.is_empty() {
            return Err(BoardError::Invalid("Select a stroked shape, line or path to outline its stroke.".into()));
        }
        for id in &ids {
            self.editable(id)?;
        }
        let snap = self.snapshot_now()?;
        struct Plan {
            id: String,
            raw: RawElement,
            outline: crate::path::PathGeom,
            transform: Transform,
            keep: bool,
            style: Style,
        }
        let mut plans: Vec<Plan> = Vec::new();
        let mut refused: Vec<String> = Vec::new();
        for id in &ids {
            let e = snap.get(id).ok_or_else(|| BoardError::NoElement(id.to_string()))?;
            let Some(path) = crate::path::to_path(&e.raw.kind, &e.raw.geom, &e.raw.style, &e.raw.points) else {
                let word = e.raw.kind.with_article();
                if !refused.contains(&word) {
                    refused.push(word);
                }
                continue;
            };
            let s = &e.style;
            if s.width <= 0.0 {
                return Err(BoardError::Invalid(format!("{} has no stroke to outline.", capitalized(&e.raw.kind.with_article()))));
            }
            let outline = crate::path::outline_stroke(&path, &crate::paint::StrokeStyle::of(s).tiny())
                .ok_or_else(|| BoardError::Invalid(format!("{} has no stroke to outline.", capitalized(&e.raw.kind.with_article()))))?;
            let Some(new_pivot) = outline.bounds() else { continue };
            let old_pivot = crate::snapshot::pivot_of(&e.raw, s).unwrap_or(new_pivot);
            let transform = repivot(&e.raw.transform, old_pivot, new_pivot);
            let has_text = e.raw.text.as_ref().is_some_and(|t| !t.trim().is_empty());
            let keep = s.fill.is_some() || has_text;
            let style = Style {
                fill: Some(s.stroke),
                fill_gradient: s.stroke_gradient.clone(),
                width: Some(0.0),
                opacity: e.raw.style.opacity,
                ..Style::default()
            };
            plans.push(Plan { id: id.to_string(), raw: e.raw.clone(), outline, transform, keep, style });
        }
        if !refused.is_empty() {
            let list = match refused.as_slice() {
                [one] => format!("{one} does not"),
                [rest @ .., last] => format!("{} and {last} do not", rest.join(", ")),
                [] => unreachable!(),
            };
            return Err(BoardError::Invalid(format!("Only shapes, lines without arrow heads, pen strokes and paths have a stroke to outline: {list}.")));
        }
        let mut out = Vec::with_capacity(plans.len());
        for plan in plans {
            if plan.keep {
                // The fill (and text) stay; the stroke goes to a new path above.
                let map = self.element(&plan.id)?;
                let layer = string_of(map.get(self.txn, LAYER)).unwrap_or_default();
                let mut element = NewElement::new(Kind::Path, Geom::Path(plan.outline)).with_style(plan.style);
                element.layer = Some(layer);
                element.parent = plan.raw.parent.clone();
                element.placement = Some(Placement::Above(plan.id.clone()));
                element.transform = Some(plan.transform);
                let new = self.add_element(element)?;
                self.set_style(&plan.id, &Style { width: Some(0.0), ..Style::default() })?;
                out.push(new);
            } else {
                let map = self.element(&plan.id)?;
                map.insert(self.txn, KIND, Kind::Path.as_str().to_string());
                map.insert(self.txn, GEOM, geom_any(&Geom::Path(plan.outline)));
                if plan.transform.is_identity() {
                    map.remove(self.txn, TRANSFORM);
                } else {
                    map.insert(self.txn, TRANSFORM, transform_any(&plan.transform));
                }
                // The stroke's detail means nothing on a fill.
                let style_map: MapRef = map.insert(self.txn, STYLE, MapPrelim::default());
                write_style(self.txn, &style_map, &plan.style);
                self.stamp_element(&map);
                out.push(plan.id);
            }
        }
        Ok(out)
    }

    /// Flip elements horizontally or vertically about the center of their
    /// combined bounds, in one transaction. Judged first: a locked element
    /// refuses the whole flip.
    ///
    /// - Paths, shapes without text, lines, arrows, pen and highlighter
    ///   strokes and pictures are **mirrored** through their transform
    ///   (its scale negated on one axis and its rotation reflected), so
    ///   their geometry, gradients and handles are untouched and a second
    ///   flip gives back exactly what was there.
    /// - Text, sticky notes, cards, steps, callouts, frames, and shapes
    ///   that hold text are **moved** to their mirrored place, keeping
    ///   their rotation: glyphs are never mirrored. A frame's members ride
    ///   with it.
    /// - A group flips what is in it, each member as above (members of a
    ///   flipped frame ride with the frame).
    /// - A connector's free ends are mirrored; ends on elements follow
    ///   those elements.
    pub fn flip(&mut self, ids: &[String], axis: FlipAxis) -> Result<()> {
        let snap = self.snapshot_now()?;
        let index_of = |id: &str| snap.elements.iter().position(|e| e.id() == id);
        let mut center_box: Option<Rect> = None;
        let mut picked: Vec<usize> = Vec::new();
        let mut groups: Vec<usize> = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        for id in ids {
            let at = index_of(id).ok_or_else(|| BoardError::NoElement(id.clone()))?;
            let b = snap.elements[at].bounds;
            center_box = Some(center_box.map_or(b, |c| c.union(&b)));
            stack.push(at);
        }
        let Some(center_box) = center_box else { return Ok(()) };
        let mut seen = HashSet::new();
        while let Some(at) = stack.pop() {
            if !seen.insert(at) {
                continue;
            }
            let e = &snap.elements[at];
            if e.raw.kind == Kind::Group {
                groups.push(at);
                for (j, m) in snap.elements.iter().enumerate() {
                    if m.parent.as_deref() == Some(e.id()) {
                        stack.push(j);
                    }
                }
            } else {
                picked.push(at);
            }
        }
        // Members of a frame that is flipped ride with it.
        let frames: HashSet<&str> = picked.iter().map(|&i| &snap.elements[i]).filter(|e| e.raw.kind == Kind::Frame).map(|e| e.id()).collect();
        let rides = |i: usize| -> bool {
            let mut parent = snap.elements[i].parent.as_deref();
            let mut hops = 0;
            while let Some(p) = parent {
                if frames.contains(p) {
                    return true;
                }
                hops += 1;
                if hops > snap.elements.len() {
                    break;
                }
                parent = snap.get(p).and_then(|pe| pe.parent.as_deref());
            }
            false
        };
        let picked: Vec<usize> = picked.into_iter().filter(|&i| !rides(i)).collect();
        for &at in &picked {
            self.editable(snap.elements[at].id())?;
        }
        for &at in &groups {
            self.editable(snap.elements[at].id())?;
        }
        let c = center_box.center();
        let board_flip = match axis {
            FlipAxis::Horizontal => Affine::from_row(-1.0, 0.0, 0.0, 1.0, 2.0 * c.x, 0.0),
            FlipAxis::Vertical => Affine::from_row(1.0, 0.0, 0.0, -1.0, 0.0, 2.0 * c.y),
        };
        for &at in &picked {
            let e = &snap.elements[at];
            let map = self.element(e.id())?;
            if let Geom::Connector { .. } = &e.raw.geom {
                if let Some(g) = e.raw.geom.mapped(|p| map_point(&board_flip, p)) {
                    map.insert(self.txn, GEOM, geom_any(&g));
                    self.stamp_element(&map);
                }
                continue;
            }
            let parent = e.parent.as_deref().and_then(|p| snap.get(p)).map(|p| p.world).unwrap_or_default();
            let Some(inv) = parent.invert() else { continue };
            // The board's mirror, seen in the element's group's space.
            let l = inv.pre_concat(board_flip).pre_concat(parent);
            let t = e.raw.transform;
            let p = e.pivot_bounds.center();
            let moved_only = matches!(e.raw.kind, Kind::Text | Kind::Sticky | Kind::Card | Kind::Step | Kind::Callout | Kind::Frame | Kind::Spotlight)
                || (e.raw.kind.is_shape() && e.raw.text.as_deref().is_some_and(|s| !s.trim().is_empty()));
            let mut nt = t;
            if moved_only {
                // What it paints lands mirrored: its bounds' center goes to
                // its mirror, moved in its group's space.
                let bc = e.bounds.center();
                let (from, to) = (map_point(&inv, bc), map_point(&inv, map_point(&board_flip, bc)));
                nt.tx += to.x - from.x;
                nt.ty += to.y - from.y;
            } else {
                let there = map_point(&l, Point::new(p.x + t.tx, p.y + t.ty));
                nt.tx = there.x - p.x;
                nt.ty = there.y - p.y;
                // l's linear part is a reflection across a line at angle φ:
                // [cos 2φ, sin 2φ; sin 2φ, -cos 2φ] = R(2φ + 180°)·diag(-1, 1),
                // and diag(-1, 1)·R(θ)·S = R(-θ)·S(-sx, sy), so the flip is
                // R(2φ + 180° - θ)·S(-sx, sy).
                let two_phi = l.ky.atan2(l.sx).to_degrees();
                nt.rot = normalize_degrees(two_phi + 180.0 - t.rot);
                nt.sx = -t.sx;
            }
            if nt.is_identity() {
                map.remove(self.txn, TRANSFORM);
            } else {
                map.insert(self.txn, TRANSFORM, transform_any(&nt));
            }
            self.stamp_element(&map);
        }
        // A turned or scaled group pivots on its members' extent, which the
        // flip moved: keep it where it was, innermost groups first.
        let depth_of = |at: usize| {
            let mut d = 0;
            let mut e = &snap.elements[at];
            while let Some(p) = e.parent.as_deref().and_then(|p| snap.get(p)) {
                d += 1;
                e = p;
            }
            d
        };
        let mut groups: Vec<(usize, usize)> = groups.into_iter().map(|g| (depth_of(g), g)).collect();
        groups.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, at) in groups {
            let g = &snap.elements[at];
            let t = g.raw.transform;
            if t.rot == 0.0 && t.sx == 1.0 && t.sy == 1.0 {
                continue;
            }
            let now = self.snapshot_now()?;
            let Some(after) = now.get(g.id()) else { continue };
            let nt = repivot(&t, g.pivot_bounds, after.pivot_bounds);
            if nt != t {
                let map = self.element(g.id())?;
                map.insert(self.txn, TRANSFORM, transform_any(&nt));
            }
        }
        Ok(())
    }

    /// Put what [`crate::svg_in::parse`] read into the board, in this one
    /// transaction: every element in order (bottom to top), into `layer`
    /// (`None`: where [`BoardTxn::add_element`] puts new elements; a board
    /// with no vector layer refuses with [`BoardError::NoVectorLayer`], and
    /// the tools make "Vector 1" first, as for any new element), and the
    /// SVG's groups as board groups. Answers the new ids, as `import.items`.
    pub fn import_svg(&mut self, import: &crate::svg_in::SvgImport, layer: Option<&str>) -> Result<Vec<String>> {
        let mut ids: Vec<String> = Vec::with_capacity(import.items.len());
        for item in &import.items {
            let mut element = item.element.clone();
            element.layer = layer.map(str::to_string).or(element.layer);
            element.parent = None;
            ids.push(self.add_element(element)?);
        }
        for (item, id) in import.items.iter().zip(&ids) {
            if let Some(g) = item.group {
                let parent = ids.get(g).cloned().ok_or_else(|| BoardError::Invalid("an SVG group names no element".into()))?;
                self.set_parent(id, Some(&parent))?;
            }
        }
        Ok(ids)
    }

    // ── Meaning ──────────────────────────────────────────────────────────
    //
    // What an element or layer means is not what a lock guards
    // (docs/API_CONTRACT.md open decision 5, taken): a locked picture can
    // still be described, so none of these judge locks.

    /// An element's note, edited as the smallest change (someone typing in
    /// it keeps their typing).
    pub fn set_note(&mut self, id: &str, note: &str) -> Result<()> {
        let map = self.element(id)?;
        write_note(self.txn, &map, note);
        self.stamp_element(&map);
        Ok(())
    }

    /// An element's field `name` (matched without regard to case, keeping
    /// the name it was first given); `None` removes it.
    pub fn set_field(&mut self, id: &str, name: &str, value: Option<&str>) -> Result<()> {
        let map = self.element(id)?;
        write_field(self.txn, &map, name, value)?;
        self.stamp_element(&map);
        Ok(())
    }

    /// A layer's note.
    pub fn set_layer_note(&mut self, id: &str, note: &str) -> Result<()> {
        let map = self.layer_map(id)?;
        write_note(self.txn, &map, note);
        self.stamp_board();
        Ok(())
    }

    /// A layer's field `name`; `None` removes it.
    pub fn set_layer_field(&mut self, id: &str, name: &str, value: Option<&str>) -> Result<()> {
        let map = self.layer_map(id)?;
        write_field(self.txn, &map, name, value)?;
        self.stamp_board();
        Ok(())
    }

    /// Remove an element (its key goes; a concurrent edit of it is lost,
    /// which the contract accepts). Members of a deleted frame or group read
    /// as having no parent. Refused when it is locked.
    pub fn delete_element(&mut self, id: &str) -> Result<()> {
        self.editable(id)?;
        self.elements()?.remove(self.txn, id);
        Ok(())
    }

    /// Remove several elements, skipping the locked ones (contract open
    /// decision 5, taken: a delete of a selection that holds locked elements
    /// deletes the rest and says how many it kept). Refused, before anything
    /// is written, when an id names no element; an id given twice counts
    /// once.
    pub fn delete_elements(&mut self, ids: &[String]) -> Result<Deleted> {
        let mut seen = HashSet::new();
        let mut judged = Vec::with_capacity(ids.len());
        for id in ids {
            if seen.insert(id.as_str()) {
                let map = self.element(id)?;
                let free = match self.judge_element(id, &map) {
                    Ok(()) => true,
                    Err(e) if e.is_lock() => false,
                    Err(e) => return Err(e),
                };
                judged.push((id.clone(), free));
            }
        }
        let mut out = Deleted::default();
        let elements = self.elements()?;
        for (id, free) in judged {
            if free {
                elements.remove(self.txn, &id);
                out.deleted.push(id);
            } else {
                out.kept.push(id);
            }
        }
        Ok(out)
    }

    // ── Paint (docs/DRAWING_CONTRACT.md section 3) ───────────────────────

    /// The board-map keys of the paint operations in `layers`.
    fn paint_keys_in(&self, layers: &HashSet<String>) -> Vec<String> {
        let Some(board) = self.board.clone() else { return Vec::new() };
        board
            .iter(self.txn)
            .filter_map(|(key, out)| match out {
                Out::YMap(map) if key.starts_with(crate::paint_ops::PAINT_PREFIX) => {
                    string_of(map.get(self.txn, LAYER)).filter(|l| layers.contains(l)).map(|_| key.to_string())
                }
                _ => None,
            })
            .collect()
    }

    /// The map of paint operation `id`.
    fn paint_op_map(&self, id: &str) -> Result<MapRef> {
        match self.board()?.get(self.txn, &crate::paint_ops::paint_key(id)) {
            Some(Out::YMap(map)) => Ok(map),
            _ => Err(BoardError::NoPaintOp(id.to_string())),
        }
    }

    /// Whether paint may go into layer `id`: a paint layer, not locked
    /// (itself or by a group).
    fn judge_paint_into(&self, id: &str) -> Result<()> {
        let ls = self.tree()?;
        let at = ls.entry(id).ok_or_else(|| BoardError::NoLayer(id.to_string()))?;
        let kind = &ls.tree.entries[at].kind;
        if *kind != LayerKind::Paint {
            return Err(BoardError::NotAPaintLayer { id: id.to_string(), kind: kind.clone() });
        }
        match ls.locked(at) {
            Some(locked) => Err(BoardError::LayerLocked(locked)),
            None => Ok(()),
        }
    }

    /// The order key just above every operation of paint layer `layer`.
    fn next_paint_order(&mut self, layer: &str) -> Result<String> {
        let cache_key = format!("\u{0}paint:{layer}");
        let top = match self.tops.get(&cache_key) {
            Some(top) => top.clone(),
            None => {
                let board = self.board()?;
                board
                    .iter(self.txn)
                    .filter_map(|(key, out)| match out {
                        Out::YMap(map) if key.starts_with(crate::paint_ops::PAINT_PREFIX) && string_of(map.get(self.txn, LAYER)).as_deref() == Some(layer) => {
                            string_of(map.get(self.txn, ORDER))
                        }
                        _ => None,
                    })
                    .max()
            }
        };
        let key = order::after(top.as_deref());
        self.tops.insert(cache_key, Some(key.clone()));
        Ok(key)
    }

    fn insert_paint_op(&mut self, layer: &str, body: crate::paint_ops::PaintBody) -> Result<String> {
        self.insert_paint_op_at(layer, body, None)
    }

    /// [`BoardTxn::insert_paint_op`] at `order` (on top when `None`).
    fn insert_paint_op_at(&mut self, layer: &str, body: crate::paint_ops::PaintBody, order: Option<String>) -> Result<String> {
        self.judge_paint_into(layer)?;
        let order = match order {
            Some(o) => o,
            None => self.next_paint_order(layer)?,
        };
        let id = new_id();
        let op = crate::paint_ops::RawPaintOp {
            id: id.clone(),
            layer: layer.to_string(),
            order,
            body,
            author: self.stamp.author.cloned(),
            created_at: self.stamp.now.to_string(),
        };
        let board = self.board()?;
        let map: MapRef = board.insert(self.txn, crate::paint_ops::paint_key(&id), MapPrelim::default());
        write_paint_op(self.txn, &map, &op);
        Ok(id)
    }

    /// Start a brush stroke (or an eraser's, `blend` erase) on top of its
    /// paint layer, with its first points if it has any; answers the op id.
    /// More points come with [`BoardTxn::append_stroke`] while it is drawn,
    /// so others watch it drawn; [`BoardTxn::end_stroke`] when it is done.
    /// Refused when the layer is not a paint layer or is locked.
    pub fn begin_stroke(&mut self, stroke: crate::paint_ops::NewStroke) -> Result<String> {
        let crate::paint_ops::NewStroke { layer, preset, settings, color, blend, points } = stroke;
        let body = crate::paint_ops::PaintBody::Stroke(Arc::new(crate::paint_ops::StrokeOp { preset, settings: settings.sanitized(), color, blend, points }));
        self.insert_paint_op(&layer, body)
    }

    /// A whole stroke at once (the MCP's): [`BoardTxn::begin_stroke`] with
    /// every point. Refused when it has no points.
    pub fn add_stroke(&mut self, stroke: crate::paint_ops::NewStroke) -> Result<String> {
        if !stroke.points.iter().any(|p| p[0].is_finite() && p[1].is_finite()) {
            return Err(BoardError::Invalid("A stroke needs at least one point.".into()));
        }
        self.begin_stroke(stroke)
    }

    /// Append points (x, y, pressure, tilt) to a stroke being drawn: one
    /// chunk, as a pen's points are appended. Refused when its layer is
    /// locked.
    pub fn append_stroke(&mut self, id: &str, points: &[crate::brush::BrushPoint]) -> Result<()> {
        let map = self.paint_op_map(id)?;
        let layer = string_of(map.get(self.txn, LAYER)).unwrap_or_default();
        self.judge_paint_into(&layer)?;
        let array = match map.get(self.txn, paint_keys::POINTS) {
            Some(Out::YArray(array)) => array,
            _ => return Err(BoardError::Invalid(format!("paint operation {id} is not a stroke"))),
        };
        if !points.is_empty() {
            array.push_back(self.txn, Any::Buffer(crate::paint_ops::pack_brush_points(points).into()));
        }
        Ok(())
    }

    /// The stroke is done. A stroke that never got a point is removed (it
    /// draws nothing); answers whether the stroke stays.
    pub fn end_stroke(&mut self, id: &str) -> Result<bool> {
        let map = self.paint_op_map(id)?;
        let empty = match map.get(self.txn, paint_keys::POINTS) {
            Some(Out::YArray(array)) => array.iter(self.txn).all(|c| !matches!(c, Out::Any(Any::Buffer(b)) if b.len() >= 16)),
            _ => false,
        };
        if empty {
            self.board()?.remove(self.txn, &crate::paint_ops::paint_key(id));
        }
        Ok(!empty)
    }

    /// A picture (`src`, a `pimble-blob:` URL) laid over `rect` on top of
    /// paint layer `layer`: what Flatten, Merge Down and pasting into a paint
    /// layer make (wave 5). Answers the op id.
    pub fn add_pixels(&mut self, layer: &str, src: &str, rect: Rect) -> Result<String> {
        let rect = rect.normalized();
        if !(rect.w > 0.0 && rect.h > 0.0 && rect.is_finite()) {
            return Err(BoardError::Invalid("pixels need a rectangle with a size".into()));
        }
        self.insert_paint_op(layer, crate::paint_ops::PaintBody::Pixels(crate::paint_ops::PixelsOp { src: src.to_string(), rect, blend: Blend::Normal }))
    }

    /// Any operation laid on top of paint layer `layer` as it is (copying a
    /// layer's history to another layer or canvas: docs/DRAWING_CONTRACT.md
    /// "Large canvases"). A stroke's settings are sanitized as a new
    /// stroke's are; an operation of a later version (`PaintBody::Other`)
    /// and one that does not hold up (a stroke or retouch with no points,
    /// a fill or pixels with no area, a move with no selection) are
    /// refused. Judged as every paint operation is (a paint layer, not
    /// locked).
    pub fn add_paint_body(&mut self, layer: &str, body: crate::paint_ops::PaintBody) -> Result<String> {
        use crate::paint_ops::PaintBody;
        let sized = |r: &Rect| r.is_finite() && r.w > 0.0 && r.h > 0.0;
        let body = match body {
            PaintBody::Other(_) => return Err(BoardError::Invalid("an operation of a later version cannot be copied".into())),
            PaintBody::Stroke(s) if s.points.is_empty() => return Err(BoardError::Invalid("a stroke needs at least one point".into())),
            PaintBody::Stroke(s) => {
                let mut s = (*s).clone();
                s.settings = s.settings.sanitized();
                PaintBody::Stroke(Arc::new(s))
            }
            PaintBody::Retouch(r) if r.points.is_empty() => return Err(BoardError::Invalid("a smudge or blur needs at least one point".into())),
            PaintBody::Pixels(p) if !sized(&p.rect.normalized()) => return Err(BoardError::Invalid("pixels need a rectangle with a size".into())),
            PaintBody::Fill(f) if !sized(&f.clip.normalized()) => return Err(BoardError::Invalid("a fill needs an area".into())),
            PaintBody::Move(m) if !m.selection.is_valid() => return Err(BoardError::Invalid("a move needs a selection".into())),
            other => other,
        };
        self.insert_paint_op(layer, body)
    }

    /// [`BoardTxn::add_pixels`] laid on with `blend` (pass-through reads
    /// as normal).
    pub fn add_pixels_blended(&mut self, layer: &str, src: &str, rect: Rect, blend: Blend) -> Result<String> {
        let rect = rect.normalized();
        if !(rect.w > 0.0 && rect.h > 0.0 && rect.is_finite()) {
            return Err(BoardError::Invalid("pixels need a rectangle with a size".into()));
        }
        let blend = if blend == Blend::PassThrough { Blend::Normal } else { blend };
        self.insert_paint_op(layer, crate::paint_ops::PaintBody::Pixels(crate::paint_ops::PixelsOp { src: src.to_string(), rect, blend }))
    }

    /// A flood fill on top of its paint layer (docs/DRAWING_CONTRACT.md
    /// section 3): `fill.clip` is what the filler's device computed
    /// ([`crate::raster::fill_clip`]). Refused when the seed is outside the
    /// clip, the clip is larger than [`crate::raster::MAX_FILL_UNITS`] on
    /// a side, or the layer is not a paint layer or is locked.
    pub fn add_fill(&mut self, fill: crate::paint_ops::NewFill) -> Result<String> {
        let crate::paint_ops::NewFill { layer, seed, color, tolerance, clip, contiguous } = fill;
        let clip = clip.normalized();
        if !(clip.is_finite() && clip.w > 0.0 && clip.h > 0.0 && seed.x.is_finite() && seed.y.is_finite()) {
            return Err(BoardError::Invalid("A fill needs a seed point and an area with a size.".into()));
        }
        if !clip.contains(seed) {
            return Err(BoardError::Invalid("A fill starts inside the area it may fill.".into()));
        }
        let max = crate::raster::MAX_FILL_UNITS;
        if clip.w > max + 1.0 || clip.h > max + 1.0 {
            return Err(BoardError::Invalid(format!("A fill covers at most {max} units on a side.")));
        }
        let tolerance = if tolerance.is_finite() { tolerance.clamp(0.0, 1.0) } else { crate::paint_ops::DEFAULT_TOLERANCE };
        self.insert_paint_op(&layer, crate::paint_ops::PaintBody::Fill(crate::paint_ops::FillOp { seed, color, tolerance, clip, contiguous }))
    }

    /// Start a smudge or blur stroke on top of its paint layer, with its
    /// first points if it has any; answers the op id. It streams as a
    /// stroke does: [`BoardTxn::append_stroke`] and
    /// [`BoardTxn::end_stroke`] take it too.
    pub fn begin_retouch(&mut self, retouch: crate::paint_ops::NewRetouch) -> Result<String> {
        let crate::paint_ops::NewRetouch { layer, effect, preset, settings, strength, points } = retouch;
        let strength = if strength.is_finite() { strength.clamp(0.0, 1.0) } else { crate::paint_ops::DEFAULT_STRENGTH };
        let body = crate::paint_ops::PaintBody::Retouch(Arc::new(crate::paint_ops::RetouchOp { effect, preset, settings: settings.sanitized(), strength, points }));
        self.insert_paint_op(&layer, body)
    }

    /// A whole smudge or blur at once (the MCP's). Refused with no points.
    pub fn add_retouch(&mut self, retouch: crate::paint_ops::NewRetouch) -> Result<String> {
        if !retouch.points.iter().any(|p| p[0].is_finite() && p[1].is_finite()) {
            return Err(BoardError::Invalid("A smudge or blur needs at least one point.".into()));
        }
        self.begin_retouch(retouch)
    }

    /// Move a paint selection (docs/DRAWING_CONTRACT.md section 3, "Select"):
    /// the pixels of `layer` inside `selection` are lifted and put down
    /// through `transform`; `None` puts nothing down (Delete clears the
    /// selection). On top of the layer; answers the op id.
    pub fn add_move(&mut self, layer: &str, selection: crate::paint_ops::Selection, transform: Option<crate::paint_ops::PixelTransform>) -> Result<String> {
        if !selection.is_valid() {
            return Err(BoardError::Invalid("A selection needs an area: a rectangle with a size, or a lasso of three or more points.".into()));
        }
        if transform.is_some_and(|t| !t.is_finite() || t.inverse().is_none()) {
            return Err(BoardError::Invalid("That transform flattens the selection to nothing.".into()));
        }
        self.insert_paint_op(layer, crate::paint_ops::PaintBody::Move(crate::paint_ops::MoveOp { selection, transform }))
    }

    /// Change where a move puts its pixels down (the selection dragged on,
    /// scaled or turned after it was first moved): one value, last writer
    /// wins. `None` makes it a Delete.
    pub fn set_move_transform(&mut self, id: &str, transform: Option<crate::paint_ops::PixelTransform>) -> Result<()> {
        let map = self.paint_op_map(id)?;
        let layer = string_of(map.get(self.txn, LAYER)).unwrap_or_default();
        self.judge_paint_into(&layer)?;
        if string_of(map.get(self.txn, KIND)).as_deref() != Some("move") {
            return Err(BoardError::Invalid(format!("paint operation {id} is not a move")));
        }
        match transform {
            Some(t) if !t.is_finite() || t.inverse().is_none() => Err(BoardError::Invalid("That transform flattens the selection to nothing.".into())),
            Some(t) => {
                map.insert(self.txn, paint_keys::TRANSFORM, pixel_transform_any(&t));
                Ok(())
            }
            None => {
                map.remove(self.txn, paint_keys::TRANSFORM);
                Ok(())
            }
        }
    }

    /// Lay down what [`crate::raster::rasterize`] drew, `srcs` its tiles'
    /// `pimble-blob:` URLs in tile order (none when it drew nothing), in one
    /// transaction (one undo), one `pixels` operation per tile:
    ///
    /// - **Flatten**: the operations it drew are deleted and the tiles'
    ///   `pixels` operations take the lowest one's place; operations painted
    ///   since it was drawn stay, above them.
    /// - **Merge Down** onto a paint layer: the pixels go on top of the
    ///   layer below with the merged layer's blend, and the merged layer
    ///   goes with its elements or operations. Refused when the layer below
    ///   is no longer the one it was drawn for.
    /// - **Merge Group** holding a paint layer: the group becomes one paint
    ///   layer (its id, name, visibility, opacity and blend; pass-through
    ///   reads as normal) holding the pixels; what was in it goes.
    ///
    /// Locks are judged as for the merges and deleting operations; a
    /// refusal writes nothing. Answers the `pixels` operations' ids.
    pub fn apply_raster(&mut self, raster: &crate::raster::LayerRaster, srcs: &[String]) -> Result<Vec<String>> {
        use crate::raster::RasterJob;
        if srcs.len() != raster.tiles.len() {
            return Err(BoardError::Invalid(format!(
                "The pixels need one blob URL per tile: {} tiles, {} URLs.",
                raster.tiles.len(),
                srcs.len()
            )));
        }
        let pixels = |blend: Blend| -> Vec<crate::paint_ops::PaintBody> {
            raster
                .tiles
                .iter()
                .zip(srcs)
                .map(|(tile, src)| crate::paint_ops::PaintBody::Pixels(crate::paint_ops::PixelsOp { src: src.clone(), rect: tile.rect, blend }))
                .collect()
        };
        match &raster.job {
            RasterJob::Flatten(layer) => {
                self.judge_paint_into(layer)?;
                let board = self.board()?;
                let mut lowest: Option<String> = None;
                let mut doomed = Vec::new();
                for id in &raster.ops {
                    let Ok(map) = self.paint_op_map(id) else { continue };
                    if string_of(map.get(self.txn, LAYER)).as_deref() != Some(layer.as_str()) {
                        continue;
                    }
                    let o = string_of(map.get(self.txn, ORDER)).unwrap_or_default();
                    if lowest.as_ref().is_none_or(|l| o < *l) {
                        lowest = Some(o);
                    }
                    doomed.push(crate::paint_ops::paint_key(id));
                }
                for key in doomed {
                    board.remove(self.txn, &key);
                }
                // The tiles do not overlap, so they share the lowest one's
                // place in the order (ties break by id, the same on every
                // device).
                let mut out = Vec::with_capacity(srcs.len());
                for body in pixels(Blend::Normal) {
                    out.push(self.insert_paint_op_at(layer, body, lowest.clone())?);
                }
                Ok(out)
            }
            RasterJob::MergeDown(id) => {
                let ls = self.tree()?;
                let at = ls.entry(id).ok_or_else(|| BoardError::NoLayer(id.to_string()))?;
                if ls.tree.entries[at].kind.is_group() {
                    return Err(BoardError::Invalid("That is a group. Merge it with Merge Group.".into()));
                }
                let siblings = ls.tree.siblings(at);
                let pos = siblings.iter().position(|s| *s == at).unwrap_or(0);
                let below = pos.checked_sub(1).map(|p| siblings[p]).ok_or_else(|| BoardError::Invalid("the bottom layer has nothing below it".into()))?;
                if ls.tree.entries[below].kind != LayerKind::Paint {
                    return Err(BoardError::Invalid("The layer below is not a paint layer.".into()));
                }
                if raster.onto.as_deref() != Some(ls.id(below)) {
                    return Err(BoardError::Invalid("The layers changed while the layer was being drawn as pixels; merge it again.".into()));
                }
                if let Some(locked) = ls.locked(at) {
                    return Err(BoardError::LayerLockedAgainstRemoval(locked));
                }
                if let Some(locked) = ls.locked(below) {
                    return Err(BoardError::LayerLocked(locked));
                }
                let below = ls.id(below).to_string();
                let mut out = Vec::with_capacity(srcs.len());
                for body in pixels(raster.blend) {
                    out.push(self.insert_paint_op(&below, body)?);
                }
                self.remove_layers_with_contents(&[id.to_string()].into_iter().collect())?;
                self.layers_changed();
                self.stamp_board();
                Ok(out)
            }
            RasterJob::MergeGroup(id) => {
                let ls = self.tree()?;
                let at = ls.entry(id).ok_or_else(|| BoardError::NoLayer(id.to_string()))?;
                if !ls.tree.entries[at].kind.is_group() {
                    return Err(BoardError::Invalid("That layer is not a group.".into()));
                }
                if let Some(locked) = ls.locked(at) {
                    return Err(BoardError::LayerLockedAgainstRemoval(locked));
                }
                let inside = ls.tree.descendants(at);
                if let Some(inner) = inside.iter().find(|d| ls.raw_of(**d).locked) {
                    return Err(BoardError::LayerLockedAgainstRemoval(ls.locked_layer(*inner)));
                }
                let doomed: HashSet<String> = inside.iter().map(|d| ls.id(*d).to_string()).collect();
                let pass_through = ls.tree.entries[at].blend == Blend::PassThrough;
                self.remove_layers_with_contents(&doomed)?;
                let group = self.layer_map(id)?;
                group.insert(self.txn, KIND, LayerKind::Paint.as_str().to_string());
                if pass_through {
                    group.remove(self.txn, BLEND);
                }
                self.layers_changed();
                let mut out = Vec::with_capacity(srcs.len());
                for body in pixels(Blend::Normal) {
                    out.push(self.insert_paint_op(id, body)?);
                }
                self.stamp_board();
                Ok(out)
            }
        }
    }

    /// Remove the layers `ids` with their elements and paint operations
    /// (no lock judged: the caller has).
    fn remove_layers_with_contents(&mut self, ids: &HashSet<String>) -> Result<()> {
        let elements = self.elements()?;
        let gone: Vec<String> = elements
            .iter(self.txn)
            .filter_map(|(eid, out)| match out {
                Out::YMap(map) if string_of(map.get(self.txn, LAYER)).is_some_and(|l| ids.contains(&l)) => Some(eid.to_string()),
                _ => None,
            })
            .collect();
        for eid in gone {
            elements.remove(self.txn, &eid);
        }
        let board = self.board()?;
        for key in self.paint_keys_in(ids) {
            board.remove(self.txn, &key);
        }
        let layers = self.layers()?;
        for d in ids {
            layers.remove(self.txn, d);
            self.tops.remove(d);
        }
        Ok(())
    }

    /// Remove one paint operation (a tool's cancel). Refused when its layer
    /// is locked. Undo takes back a person's own last operation through the
    /// board's undo manager, as it does for elements.
    pub fn delete_paint_op(&mut self, id: &str) -> Result<()> {
        let map = self.paint_op_map(id)?;
        let layer = string_of(map.get(self.txn, LAYER)).unwrap_or_default();
        let ls = self.tree()?;
        if let Some(at) = ls.entry(&layer) {
            if let Some(locked) = ls.locked(at) {
                return Err(BoardError::LayerLocked(locked));
            }
        }
        self.board()?.remove(self.txn, &crate::paint_ops::paint_key(id));
        Ok(())
    }

    /// The paint layer a paint tool draws into (docs/DRAWING_CONTRACT.md
    /// section 1, "Tools pick their layer"): `picked` when it is a paint
    /// layer that is not locked and not in a hidden or locked group; else the
    /// nearest such paint layer above it that shows; else a new paint layer
    /// "Paint N" (the first free number) made just above `picked`, in its
    /// group (above the top-level layer holding it when that group is hidden
    /// or locked). With nothing picked: the topmost such paint layer that
    /// shows, else a new one on top. **The model makes the layer**, in the
    /// same transaction as whatever the caller does next (one undo with the
    /// first stroke); answers its id, and the UI picks it.
    pub fn paint_layer_for(&mut self, picked: Option<&str>) -> Result<String> {
        let ls = self.tree()?;
        let entries = &ls.tree.entries;
        let takes = |at: usize| {
            let e = &entries[at];
            e.kind == LayerKind::Paint && !e.effective_locked && e.parent.is_none_or(|p| entries[p].effective_visible)
        };
        let picked = picked.and_then(|id| ls.entry(id));
        if let Some(at) = picked {
            if takes(at) {
                return Ok(ls.id(at).to_string());
            }
            if let Some(found) = (at + 1..entries.len()).find(|j| takes(*j) && entries[*j].effective_visible) {
                return Ok(ls.id(found).to_string());
            }
        } else if let Some(found) = (0..entries.len()).rev().find(|j| takes(*j) && entries[*j].effective_visible) {
            return Ok(ls.id(found).to_string());
        }
        let name = free_layer_name(&ls.raw, "Paint");
        let (parent, placement) = match picked {
            Some(at) => {
                let group = entries[at].parent;
                let open = group.is_none_or(|g| entries[g].effective_visible && !entries[g].effective_locked);
                if open {
                    (group.map(|g| ls.id(g).to_string()), Placement::Above(ls.id(at).to_string()))
                } else {
                    // Above the top-level layer that holds the pick.
                    let top = ls.tree.ancestors(at).last().unwrap_or(at);
                    (None, Placement::Above(ls.id(top).to_string()))
                }
            }
            None => (None, Placement::Top),
        };
        self.add_layer_in(&name, LayerKind::Paint, parent.as_deref(), placement)
    }

    /// The paint layer the highlighter preset draws into on a picture
    /// (docs/DRAWING_CONTRACT.md section 1): the paint layer named
    /// "Highlights" just above the picture's layer when there is one, else a
    /// new one made there, blended multiply. With no picture (a canvas whose
    /// picture is under everything) it is the bottom layer of the canvas.
    /// Refused when that "Highlights" layer is locked.
    pub fn highlights_layer(&mut self, picture: Option<&str>) -> Result<String> {
        let ls = self.tree()?;
        let (parent, placement) = match picture {
            Some(id) => {
                let map = self.element(id)?;
                let layer = self.drawn_in(&map)?;
                let at = ls.entry(&layer).ok_or_else(|| BoardError::NoLayer(layer.clone()))?;
                let siblings = ls.tree.siblings(at);
                let pos = siblings.iter().position(|s| *s == at).unwrap_or(0);
                if let Some(&above) = siblings.get(pos + 1) {
                    if is_highlights(&ls, above) {
                        return reuse_highlights(&ls, above);
                    }
                }
                (ls.tree.entries[at].parent.map(|p| ls.id(p).to_string()), Placement::Above(layer))
            }
            None => {
                if let Some(&bottom) = ls.tree.roots.first() {
                    if is_highlights(&ls, bottom) {
                        return reuse_highlights(&ls, bottom);
                    }
                }
                (None, Placement::Bottom)
            }
        };
        let id = self.add_layer_in(HIGHLIGHTS_LAYER_NAME, LayerKind::Paint, parent.as_deref(), placement)?;
        if picture.is_some() {
            // Over a picture the highlighter filters it, as ink on paper.
            self.set_layer_blend(&id, crate::brush::HIGHLIGHTER.layer_blend)?;
        } else {
            // On bare canvas there is nothing to filter, and multiply over a
            // dark theme's background reads as a dark smear (Joe,
            // 2026-11-17): a translucent normal layer reads as a band on both.
            self.set_layer_opacity(&id, BARE_HIGHLIGHTS_OPACITY)?;
        }
        Ok(id)
    }
}

/// The refusal of Merge Down from a paint layer onto a vector layer.
pub const PAINT_ONTO_VECTOR: &str = "A paint layer cannot merge onto a vector layer: pixels do not become shapes.";

/// The name of the paint layer the highlighter makes on a picture.
pub const HIGHLIGHTS_LAYER_NAME: &str = "Highlights";

/// The opacity of a Highlights layer made on bare canvas (normal blend).
pub const BARE_HIGHLIGHTS_OPACITY: f32 = 0.4;

fn is_highlights(ls: &Layers, at: usize) -> bool {
    ls.tree.entries[at].kind == LayerKind::Paint && ls.raw_of(at).name == HIGHLIGHTS_LAYER_NAME
}

fn reuse_highlights(ls: &Layers, at: usize) -> Result<String> {
    match ls.locked(at) {
        Some(locked) => Err(BoardError::LayerLocked(locked)),
        None => Ok(ls.id(at).to_string()),
    }
}

/// "`stem` N" with the smallest N no layer is named.
pub fn free_layer_name(layers: &[RawLayer], stem: &str) -> String {
    (1..).map(|n| format!("{stem} {n}")).find(|name| !layers.iter().any(|l| &l.name == name)).expect("a free number")
}

// ── Reading ─────────────────────────────────────────────────────────────

/// A layer as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct RawLayer {
    pub id: String,
    pub name: String,
    pub order: String,
    pub visible: bool,
    pub locked: bool,
    pub opacity: f32,
    /// What it is for (see the module doc, "Meaning").
    pub note: Option<String>,
    /// Its named fields, by name.
    pub fields: Vec<(String, String)>,
    /// What it holds (docs/DRAWING_CONTRACT.md section 1); absent reads as
    /// vector.
    pub kind: LayerKind,
    /// Its blend mode as stored; `None` when absent or not one this version
    /// knows (the kind's default, [`LayerKind::default_blend`]).
    pub blend: Option<Blend>,
    /// The group it is in, as stored (`order` is then among that group's
    /// layers). [`crate::layer_tree::resolve`] reads what it means.
    pub parent: Option<String>,
}

/// An element as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct RawElement {
    pub id: String,
    pub layer: String,
    pub order: String,
    pub parent: Option<String>,
    pub kind: Kind,
    pub geom: Geom,
    pub text: Option<String>,
    pub style: Style,
    pub transform: Transform,
    pub locked: bool,
    pub author: Option<Author>,
    pub created_at: String,
    pub edited_by: Option<Author>,
    pub edited_at: Option<String>,
    /// A stroke's points, chunks concatenated.
    pub points: Vec<[f32; 3]>,
    /// What it means (see the module doc, "Meaning").
    pub note: Option<String>,
    /// Its named fields, by name.
    pub fields: Vec<(String, String)>,
}

/// The board as stored, before anything is derived.
#[derive(Debug, Clone, PartialEq)]
pub struct RawBoard {
    pub version: i64,
    pub page: Option<Page>,
    pub markup_visible: bool,
    pub edited_by: Option<Author>,
    pub edited_at: Option<String>,
    pub layers: Vec<RawLayer>,
    pub elements: Vec<RawElement>,
    /// Paint operations, in no particular order (docs/DRAWING_CONTRACT.md
    /// section 3); read with the elements.
    pub paint: Vec<crate::paint_ops::RawPaintOp>,
}

/// The board's values; its elements too when `with_elements`.
pub(crate) fn read_board<T: ReadTxn>(txn: &T, board: &MapRef, with_elements: bool) -> RawBoard {
    let version = match board.get(txn, V) {
        Some(Out::Any(any)) => number(&any).map(|n| n as i64).unwrap_or(VERSION),
        _ => VERSION,
    };
    let page = match board.get(txn, PAGE) {
        Some(Out::Any(Any::Map(m))) => rect_of(&m).map(|rect| Page {
            rect,
            fill: m.get("fill").and_then(any_str).and_then(Color::parse),
        }),
        _ => None,
    };
    let markup_visible = bool_of(board.get(txn, MARKUP_VISIBLE)).unwrap_or(true);
    let layers = match board.get(txn, LAYERS) {
        Some(Out::YMap(map)) => read_layers(txn, &map),
        _ => Vec::new(),
    };
    let mut elements = Vec::new();
    let mut paint = Vec::new();
    if with_elements {
        if let Some(Out::YMap(map)) = board.get(txn, ELEMENTS) {
            elements.reserve(map.len(txn) as usize);
            for (id, out) in map.iter(txn) {
                let Out::YMap(el) = out else { continue };
                elements.push(read_element(txn, id, &el));
            }
        }
        for (key, out) in board.iter(txn) {
            let (Some(id), Out::YMap(op)) = (key.strip_prefix(crate::paint_ops::PAINT_PREFIX), out) else { continue };
            paint.push(read_paint_op(txn, id, &op));
        }
    }
    RawBoard {
        version,
        page,
        markup_visible,
        edited_by: author_of(board.get(txn, EDITED_BY)),
        edited_at: string_of(board.get(txn, EDITED_AT)),
        layers,
        elements,
        paint,
    }
}

/// The layers of a board's `layers` map, in no particular order.
pub(crate) fn read_layers<T: ReadTxn>(txn: &T, map: &MapRef) -> Vec<RawLayer> {
    let mut layers = Vec::new();
    for (id, out) in map.iter(txn) {
        let Out::YMap(layer) = out else { continue };
        layers.push(RawLayer {
            id: id.to_string(),
            name: match layer.get(txn, NAME) {
                Some(Out::YText(t)) => t.get_string(txn),
                other => string_of(other).unwrap_or_default(),
            },
            order: string_of(layer.get(txn, ORDER)).unwrap_or_default(),
            visible: bool_of(layer.get(txn, VISIBLE)).unwrap_or(true),
            locked: bool_of(layer.get(txn, LOCKED)).unwrap_or(false),
            opacity: number_of(layer.get(txn, OPACITY)).unwrap_or(1.0).clamp(0.0, 1.0),
            note: read_note(txn, &layer),
            fields: read_fields(txn, &layer),
            kind: LayerKind::parse(&string_of(layer.get(txn, KIND)).unwrap_or_default()),
            blend: string_of(layer.get(txn, BLEND)).as_deref().and_then(Blend::parse),
            parent: string_of(layer.get(txn, PARENT)).filter(|p| !p.is_empty()),
        });
    }
    layers
}

fn read_element<T: ReadTxn>(txn: &T, id: &str, el: &MapRef) -> RawElement {
    let kind = Kind::parse(&string_of(el.get(txn, KIND)).unwrap_or_default());
    let mut points = Vec::new();
    let geom = match el.get(txn, GEOM) {
        Some(Out::YArray(array)) => {
            for chunk in array.iter(txn) {
                if let Out::Any(Any::Buffer(bytes)) = chunk {
                    unpack_points(&bytes, &mut points);
                }
            }
            Geom::Stroke
        }
        Some(Out::Any(Any::Map(m))) => geom_of(&kind, &m),
        _ => {
            if kind.is_stroke() {
                Geom::Stroke
            } else if kind == Kind::Path {
                Geom::Path(Default::default())
            } else {
                Geom::None
            }
        }
    };
    let style = match el.get(txn, STYLE) {
        Some(Out::YMap(m)) => read_style(txn, &m),
        _ => Style::default(),
    };
    let transform = match el.get(txn, TRANSFORM) {
        Some(Out::Any(Any::Map(m))) => Transform {
            tx: field(&m, "tx").unwrap_or(0.0),
            ty: field(&m, "ty").unwrap_or(0.0),
            rot: field(&m, "rot").unwrap_or(0.0),
            sx: field(&m, "sx").unwrap_or(1.0),
            sy: field(&m, "sy").unwrap_or(1.0),
        },
        _ => Transform::IDENTITY,
    };
    RawElement {
        id: id.to_string(),
        layer: string_of(el.get(txn, LAYER)).unwrap_or_default(),
        order: string_of(el.get(txn, ORDER)).unwrap_or_default(),
        parent: string_of(el.get(txn, PARENT)),
        kind,
        geom,
        text: match el.get(txn, TEXT) {
            Some(Out::YText(t)) => Some(t.get_string(txn)),
            other => string_of(other),
        },
        style,
        transform,
        locked: bool_of(el.get(txn, LOCKED)).unwrap_or(false),
        author: author_of(el.get(txn, AUTHOR)),
        created_at: string_of(el.get(txn, CREATED_AT)).unwrap_or_default(),
        edited_by: author_of(el.get(txn, EDITED_BY)),
        edited_at: string_of(el.get(txn, EDITED_AT)),
        points,
        note: read_note(txn, el),
        fields: read_fields(txn, el),
    }
}

/// A paint operation as stored (see [`crate::paint_ops`]).
pub(crate) fn read_paint_op<T: ReadTxn>(txn: &T, id: &str, map: &MapRef) -> crate::paint_ops::RawPaintOp {
    use crate::paint_ops::{FillOp, MoveOp, PaintBody, PixelTransform, PixelsOp, Retouch, RetouchOp, Selection, StrokeOp};
    let kind = string_of(map.get(txn, KIND)).unwrap_or_default();
    let body = match kind.as_str() {
        "stroke" => {
            let (preset, settings, named) = read_brush(txn, map);
            let color = string_of(map.get(txn, paint_keys::COLOR))
                .as_deref()
                .and_then(Color::parse)
                .or(named.map(|p| p.color))
                .unwrap_or(Color::BLACK);
            let blend = crate::brush::StrokeBlend::parse(&string_of(map.get(txn, BLEND)).unwrap_or_default());
            let points = read_brush_points(txn, map);
            PaintBody::Stroke(Arc::new(StrokeOp { preset, settings, color, blend, points }))
        }
        "pixels" => {
            let rect = match map.get(txn, paint_keys::RECT) {
                Some(Out::Any(Any::Map(m))) => rect_of(&m),
                _ => None,
            };
            let blend = string_of(map.get(txn, BLEND)).as_deref().and_then(Blend::parse).filter(|b| *b != Blend::PassThrough).unwrap_or(Blend::Normal);
            match (string_of(map.get(txn, paint_keys::SRC)), rect) {
                (Some(src), Some(rect)) => PaintBody::Pixels(PixelsOp { src, rect, blend }),
                _ => PaintBody::Other(kind.clone()),
            }
        }
        "fill" => {
            let seed = match map.get(txn, paint_keys::SEED) {
                Some(Out::Any(Any::Map(m))) => field(&m, "x").zip(field(&m, "y")).map(|(x, y)| Point::new(x, y)),
                _ => None,
            };
            let clip = match map.get(txn, paint_keys::CLIP) {
                Some(Out::Any(Any::Map(m))) => rect_of(&m),
                _ => None,
            };
            let color = string_of(map.get(txn, paint_keys::COLOR)).as_deref().and_then(Color::parse);
            match (seed, clip, color) {
                (Some(seed), Some(clip), Some(color)) => PaintBody::Fill(FillOp {
                    seed,
                    color,
                    tolerance: number_of(map.get(txn, paint_keys::TOLERANCE)).unwrap_or(0.0).clamp(0.0, 1.0),
                    clip,
                    contiguous: bool_of(map.get(txn, paint_keys::CONTIGUOUS)).unwrap_or(true),
                }),
                _ => PaintBody::Other(kind.clone()),
            }
        }
        "smudge" | "blur" => {
            let effect = Retouch::parse(&kind).unwrap_or(Retouch::Smudge);
            let (preset, settings, _) = read_brush(txn, map);
            let strength = number_of(map.get(txn, paint_keys::STRENGTH)).unwrap_or(crate::paint_ops::DEFAULT_STRENGTH).clamp(0.0, 1.0);
            let points = read_brush_points(txn, map);
            PaintBody::Retouch(Arc::new(RetouchOp { effect, preset, settings, strength, points }))
        }
        "move" => {
            let selection = match map.get(txn, paint_keys::SELECTION) {
                Some(Out::Any(Any::Map(m))) => match m.get("kind").and_then(any_str) {
                    Some("rect") => rect_of(&m).map(Selection::Rect),
                    Some("lasso") => match m.get("points") {
                        Some(Any::Array(items)) => {
                            let n: Vec<f32> = items.iter().filter_map(number).map(|v| v as f32).collect();
                            Some(Selection::Lasso(n.chunks_exact(2).map(|c| Point::new(c[0], c[1])).collect()))
                        }
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            };
            let transform = match map.get(txn, paint_keys::TRANSFORM) {
                Some(Out::Any(Any::Array(items))) => {
                    let n: Vec<f32> = items.iter().filter_map(number).map(|v| v as f32).collect();
                    // A malformed transform puts nothing down rather than
                    // something somewhere unknown.
                    (n.len() == 6).then(|| PixelTransform::from_array([n[0], n[1], n[2], n[3], n[4], n[5]])).filter(|t| t.is_finite())
                }
                _ => None,
            };
            match selection {
                Some(selection) => PaintBody::Move(MoveOp { selection, transform }),
                None => PaintBody::Other(kind.clone()),
            }
        }
        _ => PaintBody::Other(kind.clone()),
    };
    crate::paint_ops::RawPaintOp {
        id: id.to_string(),
        layer: string_of(map.get(txn, LAYER)).unwrap_or_default(),
        order: string_of(map.get(txn, ORDER)).unwrap_or_default(),
        body,
        author: author_of(map.get(txn, AUTHOR)),
        created_at: string_of(map.get(txn, CREATED_AT)).unwrap_or_default(),
    }
}

/// A stroke's (or smudge's, blur's) preset name and brush settings, read
/// over the named preset's, and that preset when this version has it.
fn read_brush<T: ReadTxn>(txn: &T, map: &MapRef) -> (String, crate::brush::BrushSettings, Option<&'static crate::brush::Preset>) {
    let preset = string_of(map.get(txn, paint_keys::PRESET)).unwrap_or_default();
    let named = crate::brush::preset(&preset);
    let base = named.map(|p| p.settings).unwrap_or_default();
    let brush = match map.get(txn, paint_keys::BRUSH) {
        Some(Out::Any(Any::Map(m))) => Some(m),
        _ => None,
    };
    let settings = crate::paint_ops::settings_from(base, |k| {
        let v = brush.as_ref()?.get(k)?;
        match v {
            Any::Array(items) => Some(items.iter().filter_map(number).map(|n| n as f32).collect()),
            other => number(other).map(|n| vec![n as f32]),
        }
    });
    (preset, settings, named)
}

fn read_brush_points<T: ReadTxn>(txn: &T, map: &MapRef) -> Vec<crate::brush::BrushPoint> {
    let mut points = Vec::new();
    if let Some(Out::YArray(array)) = map.get(txn, paint_keys::POINTS) {
        for chunk in array.iter(txn) {
            if let Out::Any(Any::Buffer(bytes)) = chunk {
                crate::paint_ops::unpack_brush_points(&bytes, &mut points);
            }
        }
    }
    points
}

fn write_brush(txn: &mut TransactionMut, map: &MapRef, preset: &str, settings: &crate::brush::BrushSettings, points: &[crate::brush::BrushPoint]) {
    use crate::paint_ops::{pack_brush_points, settings_fields, SettingValue};
    map.insert(txn, paint_keys::PRESET, preset.to_string());
    let mut brush = HashMap::new();
    for (k, v) in settings_fields(settings) {
        let any = match v {
            SettingValue::N(n) => Any::Number(n as f64),
            SettingValue::Curve(c) => Any::Array(vec![Any::Number(c.at0 as f64), Any::Number(c.at1 as f64), Any::Number(c.gamma as f64)].into()),
        };
        brush.insert(k.to_string(), any);
    }
    map.insert(txn, paint_keys::BRUSH, Any::Map(Arc::new(brush)));
    let array: ArrayRef = map.insert(txn, paint_keys::POINTS, ArrayPrelim::default());
    if !points.is_empty() {
        array.push_back(txn, Any::Buffer(pack_brush_points(points).into()));
    }
}

/// A selection as stored.
fn selection_any(s: &crate::paint_ops::Selection) -> Any {
    use crate::paint_ops::Selection;
    let mut m = HashMap::new();
    match s {
        Selection::Rect(r) => {
            m = rect_fields(&r.normalized());
            m.insert("kind".to_string(), Any::String("rect".into()));
        }
        Selection::Lasso(points) => {
            m.insert("kind".to_string(), Any::String("lasso".into()));
            let n: Vec<Any> = points.iter().flat_map(|p| [Any::Number(p.x as f64), Any::Number(p.y as f64)]).collect();
            m.insert("points".to_string(), Any::Array(n.into()));
        }
    }
    Any::Map(Arc::new(m))
}

fn pixel_transform_any(t: &crate::paint_ops::PixelTransform) -> Any {
    Any::Array(t.to_array().iter().map(|v| Any::Number(*v as f64)).collect::<Vec<_>>().into())
}

/// Write a paint operation's own keys into its (new) map.
fn write_paint_op(txn: &mut TransactionMut, map: &MapRef, op: &crate::paint_ops::RawPaintOp) {
    use crate::paint_ops::PaintBody;
    map.insert(txn, LAYER, op.layer.clone());
    map.insert(txn, ORDER, op.order.clone());
    map.insert(txn, KIND, op.kind().to_string());
    match &op.body {
        PaintBody::Stroke(s) => {
            map.insert(txn, paint_keys::COLOR, s.color.to_hex());
            map.insert(txn, BLEND, s.blend.as_str());
            write_brush(txn, map, &s.preset, &s.settings, &s.points);
        }
        PaintBody::Pixels(p) => {
            map.insert(txn, paint_keys::SRC, p.src.clone());
            map.insert(txn, paint_keys::RECT, Any::Map(Arc::new(rect_fields(&p.rect))));
            if p.blend != Blend::Normal {
                map.insert(txn, BLEND, p.blend.as_str());
            }
        }
        PaintBody::Fill(f) => {
            let mut seed = HashMap::new();
            point_fields(&mut seed, "x", "y", f.seed);
            map.insert(txn, paint_keys::SEED, Any::Map(Arc::new(seed)));
            map.insert(txn, paint_keys::COLOR, f.color.to_hex());
            map.insert(txn, paint_keys::TOLERANCE, Any::Number(f.tolerance as f64));
            map.insert(txn, paint_keys::CLIP, Any::Map(Arc::new(rect_fields(&f.clip.normalized()))));
            if !f.contiguous {
                map.insert(txn, paint_keys::CONTIGUOUS, false);
            }
        }
        PaintBody::Retouch(r) => {
            map.insert(txn, paint_keys::STRENGTH, Any::Number(r.strength as f64));
            write_brush(txn, map, &r.preset, &r.settings, &r.points);
        }
        PaintBody::Move(m) => {
            map.insert(txn, paint_keys::SELECTION, selection_any(&m.selection));
            if let Some(t) = &m.transform {
                map.insert(txn, paint_keys::TRANSFORM, pixel_transform_any(t));
            }
        }
        PaintBody::Other(_) => {}
    }
    if let Some(author) = &op.author {
        map.insert(txn, AUTHOR, author_any(author));
    }
    map.insert(txn, CREATED_AT, op.created_at.clone());
}

/// The keys of a paint operation's map besides the element keys it shares
/// (`layer`, `order`, `kind`, `blend`, `author`, `created_at`).
pub(crate) mod paint_keys {
    pub const PRESET: &str = "preset";
    pub const COLOR: &str = "color";
    pub const BRUSH: &str = "brush";
    pub const POINTS: &str = "points";
    pub const SRC: &str = "src";
    pub const RECT: &str = "rect";
    pub const SEED: &str = "seed";
    pub const TOLERANCE: &str = "tolerance";
    pub const CLIP: &str = "clip";
    pub const CONTIGUOUS: &str = "contiguous";
    pub const STRENGTH: &str = "strength";
    pub const SELECTION: &str = "selection";
    pub const TRANSFORM: &str = "transform";
}

fn read_note<T: ReadTxn>(txn: &T, map: &MapRef) -> Option<String> {
    match map.get(txn, NOTE) {
        Some(Out::YText(t)) => Some(t.get_string(txn)),
        other => string_of(other),
    }
    .filter(|n| !n.trim().is_empty())
}

fn read_fields<T: ReadTxn>(txn: &T, map: &MapRef) -> Vec<(String, String)> {
    let mut fields: Vec<(String, String)> = map
        .iter(txn)
        .filter_map(|(key, value)| {
            let name = key.strip_prefix(FIELD_PREFIX)?;
            let value = match value {
                Out::Any(Any::String(s)) => s.to_string(),
                Out::Any(Any::Number(n)) => n.to_string(),
                Out::Any(Any::BigInt(n)) => n.to_string(),
                Out::Any(Any::Bool(b)) => b.to_string(),
                _ => return None,
            };
            Some((name.to_string(), value))
        })
        .collect();
    fields.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()).then(a.0.cmp(&b.0)));
    fields
}

fn write_note(txn: &mut TransactionMut, map: &MapRef, note: &str) {
    let text = match map.get(txn, NOTE) {
        Some(Out::YText(t)) => t,
        _ => map.insert(txn, NOTE, TextPrelim::new("")),
    };
    edit_text(txn, &text, note);
}

fn write_field(txn: &mut TransactionMut, map: &MapRef, name: &str, value: Option<&str>) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(BoardError::Invalid("a field needs a name".into()));
    }
    // The name it already has, in whatever case it was first given.
    let existing = map
        .keys(txn)
        .filter_map(|k| k.strip_prefix(FIELD_PREFIX).map(str::to_string))
        .find(|n| n.eq_ignore_ascii_case(name));
    let key = format!("{FIELD_PREFIX}{}", existing.as_deref().unwrap_or(name));
    match value {
        Some(v) => {
            map.insert(txn, key.as_str(), v.to_string());
        }
        None => {
            map.remove(txn, &key);
        }
    }
    Ok(())
}

fn copy_meaning(txn: &mut TransactionMut, map: &MapRef, note: Option<&str>, fields: &[(String, String)]) {
    if let Some(note) = note {
        map.insert(txn, NOTE, TextPrelim::new(note.to_string()));
    }
    for (name, value) in fields {
        map.insert(txn, format!("{FIELD_PREFIX}{name}"), value.clone());
    }
}

fn read_style<T: ReadTxn>(txn: &T, m: &MapRef) -> Style {
    let s = |k: &str| string_of(m.get(txn, k));
    Style {
        stroke: s("stroke").and_then(|v| Color::parse(&v)),
        fill: s("fill").and_then(|v| Color::parse(&v)),
        width: number_of(m.get(txn, "width")),
        opacity: number_of(m.get(txn, "opacity")),
        font_size: number_of(m.get(txn, "font_size")),
        align: s("align").and_then(|v| Align::parse(&v)),
        shadow: bool_of(m.get(txn, "shadow")),
        arrow_heads: s("arrow_heads").and_then(|v| ArrowHeads::parse(&v)),
        dash: s("dash").and_then(|v| Dash::parse(&v)),
        cap: s("cap").and_then(|v| Cap::parse(&v)),
        join: s("join").and_then(|v| Join::parse(&v)),
        miter: number_of(m.get(txn, "miter")),
        dash_pattern: s("dash_pattern").and_then(|v| parse_dash_pattern(&v)),
        radius: number_of(m.get(txn, "radius")),
        sides: number_of(m.get(txn, "sides")).filter(|v| *v >= 0.0).map(|v| v.round() as u32),
        points: number_of(m.get(txn, "points")).filter(|v| *v >= 0.0).map(|v| v.round() as u32),
        inner: number_of(m.get(txn, "inner")),
        fill_gradient: gradient_of(m.get(txn, "fill_gradient")),
        stroke_gradient: gradient_of(m.get(txn, "stroke_gradient")),
    }
}

/// A gradient as stored: `{ type: "linear" | "radial", x1, y1, x2, y2,
/// stops: [{ offset, color }] }`, one value written whole. `None` when it
/// is not one (a kind this version does not know, no stops).
fn gradient_of(out: Option<Out>) -> Option<Gradient> {
    let Some(Out::Any(Any::Map(m))) = out else { return None };
    let kind = GradientKind::parse(m.get("type").and_then(any_str)?)?;
    let stops = match m.get("stops") {
        Some(Any::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Any::Map(s) => Some(GradientStop::new(field(s, "offset")?, Color::parse(s.get("color").and_then(any_str)?)?)),
                _ => None,
            })
            .collect::<Vec<_>>(),
        _ => return None,
    };
    if stops.is_empty() {
        return None;
    }
    Some(Gradient {
        kind,
        from: Point::new(field(&m, "x1").unwrap_or(0.0), field(&m, "y1").unwrap_or(0.0)),
        to: Point::new(field(&m, "x2").unwrap_or(1.0), field(&m, "y2").unwrap_or(0.0)),
        stops,
    })
}

fn gradient_any(g: &Gradient) -> Any {
    let mut m: HashMap<String, Any> = HashMap::new();
    m.insert("type".into(), Any::from(g.kind.as_str()));
    for (k, v) in [("x1", g.from.x), ("y1", g.from.y), ("x2", g.to.x), ("y2", g.to.y)] {
        m.insert(k.into(), Any::Number(v as f64));
    }
    let stops: Vec<Any> = g
        .stops
        .iter()
        .map(|s| {
            let mut sm: HashMap<String, Any> = HashMap::new();
            sm.insert("offset".into(), Any::Number(s.offset as f64));
            sm.insert("color".into(), Any::from(s.color.to_hex()));
            Any::from(sm)
        })
        .collect();
    m.insert("stops".into(), Any::from(stops));
    Any::from(m)
}

fn write_style(txn: &mut TransactionMut, m: &MapRef, style: &Style) {
    if let Some(c) = style.stroke {
        m.insert(txn, "stroke", c.to_hex());
    }
    if let Some(c) = style.fill {
        m.insert(txn, "fill", c.to_hex());
    }
    if let Some(v) = style.width {
        m.insert(txn, "width", v as f64);
    }
    if let Some(v) = style.opacity {
        m.insert(txn, "opacity", v as f64);
    }
    if let Some(v) = style.font_size {
        m.insert(txn, "font_size", v as f64);
    }
    if let Some(v) = style.align {
        m.insert(txn, "align", v.as_str());
    }
    if let Some(v) = style.shadow {
        m.insert(txn, "shadow", v);
    }
    if let Some(v) = style.arrow_heads {
        m.insert(txn, "arrow_heads", v.as_str());
    }
    if let Some(v) = style.dash {
        m.insert(txn, "dash", v.as_str());
    }
    if let Some(v) = style.cap {
        m.insert(txn, "cap", v.as_str());
    }
    if let Some(v) = style.join {
        m.insert(txn, "join", v.as_str());
    }
    if let Some(v) = style.miter {
        m.insert(txn, "miter", v as f64);
    }
    if let Some(v) = &style.dash_pattern {
        m.insert(txn, "dash_pattern", dash_pattern_text(v));
    }
    if let Some(v) = style.radius {
        m.insert(txn, "radius", v as f64);
    }
    if let Some(v) = style.sides {
        m.insert(txn, "sides", v as f64);
    }
    if let Some(v) = style.points {
        m.insert(txn, "points", v as f64);
    }
    if let Some(v) = style.inner {
        m.insert(txn, "inner", v as f64);
    }
    if let Some(g) = &style.fill_gradient {
        m.insert(txn, "fill_gradient", gradient_any(g));
        // The solid fallback an older app draws, unless a fill is given.
        if style.fill.is_none() {
            m.insert(txn, "fill", g.fallback().to_hex());
        }
    }
    if let Some(g) = &style.stroke_gradient {
        m.insert(txn, "stroke_gradient", gradient_any(g));
        if style.stroke.is_none() {
            m.insert(txn, "stroke", g.fallback().to_hex());
        }
    }
}

// ── Encoding helpers ─────────────────────────────────────────────────────

/// A random id for a layer or element: 64 random bits as hex. Ids only need
/// to be unique within one board.
pub fn new_id() -> String {
    let uuid = uuid::Uuid::new_v4();
    let bytes = uuid.as_bytes();
    // Skip the version and variant nibbles' bytes so every hex digit is random.
    let pick = [bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[7], bytes[9]];
    pick.iter().map(|b| format!("{b:02x}")).collect()
}

fn insert_layer(txn: &mut TransactionMut, layers: &MapRef, id: &str, name: &str, order_key: &str) {
    let layer: MapRef = layers.insert(txn, id.to_string(), MapPrelim::default());
    layer.insert(txn, NAME, TextPrelim::new(name.to_string()));
    layer.insert(txn, ORDER, order_key.to_string());
    layer.insert(txn, VISIBLE, true);
    layer.insert(txn, LOCKED, false);
    layer.insert(txn, OPACITY, 1.0f64);
}

/// Whether `geom` is the shape of geometry `kind` takes: a path takes a
/// path's, and only a path does; polygons and stars take a box.
fn judge_geom(kind: &Kind, geom: &Geom) -> Result<()> {
    let ok = match (kind, geom) {
        (Kind::Path, g) => matches!(g, Geom::Path(_)),
        (_, Geom::Path(_)) => false,
        (Kind::Polygon | Kind::Star, g) => matches!(g, Geom::Rect(_)),
        _ => true,
    };
    if ok {
        Ok(())
    } else {
        Err(BoardError::Invalid(format!("a {kind} does not take that geometry")))
    }
}

/// Which way [`BoardTxn::flip`] mirrors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlipAxis {
    /// Left becomes right (about a vertical line).
    Horizontal,
    /// Top becomes bottom (about a horizontal line).
    Vertical,
}

fn map_point(m: &Affine, p: Point) -> Point {
    let mut q = [tiny_skia::Point::from_xy(p.x, p.y)];
    m.map_points(&mut q);
    Point::new(q[0].x, q[0].y)
}

/// Degrees in (-180, 180].
fn normalize_degrees(d: f32) -> f32 {
    let mut d = d % 360.0;
    if d <= -180.0 {
        d += 360.0;
    } else if d > 180.0 {
        d -= 360.0;
    }
    if d.abs() < 1e-4 {
        0.0
    } else {
        d
    }
}

/// `s` with its first letter in capitals (a sentence's start).
fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// `t` adjusted so that an element whose pivot (the extent its rotation and
/// scale turn about) goes from `old` to `new` maps every point where it did
/// (Direct select writes it with a path's new geometry, so editing a point
/// of a turned or scaled path leaves the rest where it was).
pub fn repivot(t: &Transform, old: Rect, new: Rect) -> Transform {
    if t.is_identity() {
        return *t;
    }
    let (c, c2) = (old.center(), new.center());
    // t' = t + (c - c') + RS(c' - c), RS the rotation after the scale.
    let (dx, dy) = (c2.x - c.x, c2.y - c.y);
    let (sx, sy) = (dx * t.sx, dy * t.sy);
    let (sin, cos) = t.rot.to_radians().sin_cos();
    let (rx, ry) = (sx * cos - sy * sin, sx * sin + sy * cos);
    Transform { tx: t.tx - dx + rx, ty: t.ty - dy + ry, ..*t }
}

/// The order key for `placement` among `siblings` (sorted bottom to top),
/// leaving out `moving` (the thing being placed, when it is already there).
fn place(siblings: &[(String, String)], moving: Option<&str>, placement: &Placement) -> Result<String> {
    let rest: Vec<&(String, String)> = siblings.iter().filter(|(id, _)| Some(id.as_str()) != moving).collect();
    let key_at = |i: usize| rest.get(i).map(|(_, o)| o.as_str());
    Ok(match placement {
        Placement::Top => order::after(rest.last().map(|(_, o)| o.as_str())),
        Placement::Bottom => order::between(None, key_at(0)),
        Placement::Above(other) => {
            let i = rest.iter().position(|(id, _)| id == other).ok_or_else(|| BoardError::NoElement(other.clone()))?;
            order::between(key_at(i), key_at(i + 1))
        }
        Placement::Below(other) => {
            let i = rest.iter().position(|(id, _)| id == other).ok_or_else(|| BoardError::NoElement(other.clone()))?;
            order::between(if i == 0 { None } else { key_at(i - 1) }, key_at(i))
        }
    })
}

/// Edit `text` into `new` by the smallest middle replacement, in the
/// document's offset unit.
fn edit_text(txn: &mut TransactionMut, text: &TextRef, new: &str) {
    let old = text.get_string(txn);
    if old == new {
        return;
    }
    let utf16 = matches!(txn.doc().offset_kind(), OffsetKind::Utf16);
    let len = |s: &str| if utf16 { s.encode_utf16().count() as u32 } else { s.len() as u32 };
    let prefix: usize = old.chars().zip(new.chars()).take_while(|(a, b)| a == b).map(|(c, _)| c.len_utf8()).sum();
    let old_rest = &old[prefix..];
    let new_rest = &new[prefix..];
    let suffix: usize =
        old_rest.chars().rev().zip(new_rest.chars().rev()).take_while(|(a, b)| a == b).map(|(c, _)| c.len_utf8()).sum();
    let removed = &old_rest[..old_rest.len() - suffix];
    let inserted = &new_rest[..new_rest.len() - suffix];
    let at = len(&old[..prefix]);
    if !removed.is_empty() {
        text.remove_range(txn, at, len(removed));
    }
    if !inserted.is_empty() {
        text.insert(txn, at, inserted);
    }
}

pub(crate) fn pack_points(points: &[[f32; 3]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(points.len() * 12);
    for p in points {
        for v in p {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

pub(crate) fn unpack_points(bytes: &[u8], out: &mut Vec<[f32; 3]>) {
    for chunk in bytes.chunks_exact(12) {
        let f = |i: usize| f32::from_le_bytes([chunk[i], chunk[i + 1], chunk[i + 2], chunk[i + 3]]);
        out.push([f(0), f(4), f(8)]);
    }
}

fn author_any(author: &Author) -> Any {
    let mut m = HashMap::new();
    m.insert("id".to_string(), Any::String(author.id.as_str().into()));
    m.insert("name".to_string(), Any::String(author.name.as_str().into()));
    Any::Map(Arc::new(m))
}

fn author_of(out: Option<Out>) -> Option<Author> {
    match out {
        Some(Out::Any(Any::Map(m))) => {
            Some(Author { id: m.get("id").and_then(any_str)?.to_string(), name: m.get("name").and_then(any_str).unwrap_or("").to_string() })
        }
        _ => None,
    }
}

fn transform_any(t: &Transform) -> Any {
    let mut m = HashMap::new();
    for (k, v) in [("tx", t.tx), ("ty", t.ty), ("rot", t.rot), ("sx", t.sx), ("sy", t.sy)] {
        m.insert(k.to_string(), Any::Number(v as f64));
    }
    Any::Map(Arc::new(m))
}

fn rect_fields(r: &Rect) -> HashMap<String, Any> {
    let mut m = HashMap::new();
    for (k, v) in [("x", r.x), ("y", r.y), ("w", r.w), ("h", r.h)] {
        m.insert(k.to_string(), Any::Number(v as f64));
    }
    m
}

fn point_fields(m: &mut HashMap<String, Any>, kx: &str, ky: &str, p: Point) {
    m.insert(kx.to_string(), Any::Number(p.x as f64));
    m.insert(ky.to_string(), Any::Number(p.y as f64));
}

fn end_any(end: &End) -> Any {
    let mut m = HashMap::new();
    match end {
        End::Element { id, side, last } => {
            m.insert("element".to_string(), Any::String(id.as_str().into()));
            if let Some(side) = side {
                m.insert("side".to_string(), Any::String(side.as_str().into()));
            }
            point_fields(&mut m, "x", "y", *last);
        }
        End::Point(p) => point_fields(&mut m, "x", "y", *p),
    }
    Any::Map(Arc::new(m))
}

fn end_of(any: Option<&Any>) -> End {
    let Some(Any::Map(m)) = any else { return End::Point(Point::default()) };
    let p = Point::new(field(m, "x").unwrap_or(0.0), field(m, "y").unwrap_or(0.0));
    match m.get("element").and_then(any_str) {
        Some(id) => End::Element { id: id.to_string(), side: m.get("side").and_then(any_str).and_then(Side::parse), last: p },
        None => End::Point(p),
    }
}

fn geom_any(geom: &Geom) -> Any {
    let m = match geom {
        Geom::None | Geom::Stroke => HashMap::new(),
        Geom::Rect(r) => rect_fields(r),
        Geom::Picture(p) => {
            let mut m = rect_fields(&p.rect);
            m.insert("src".into(), Any::String(p.src.as_str().into()));
            m.insert("px_w".into(), Any::BigInt(p.px_w as i64));
            m.insert("px_h".into(), Any::BigInt(p.px_h as i64));
            if let Some(crop) = &p.crop {
                m.insert("crop".into(), Any::Map(Arc::new(rect_fields(crop))));
            }
            m
        }
        Geom::Segment { a, b } => {
            let mut m = HashMap::new();
            point_fields(&mut m, "x1", "y1", *a);
            point_fields(&mut m, "x2", "y2", *b);
            m
        }
        Geom::Anchor(p) => {
            let mut m = HashMap::new();
            point_fields(&mut m, "x", "y", *p);
            m
        }
        Geom::Callout { rect, tip } => {
            let mut m = rect_fields(rect);
            point_fields(&mut m, "tip_x", "tip_y", *tip);
            m
        }
        Geom::Connector { from, to } => {
            let mut m = HashMap::new();
            m.insert("from".into(), end_any(from));
            m.insert("to".into(), end_any(to));
            m
        }
        Geom::Card { rect, link } => {
            let mut m = rect_fields(rect);
            m.insert("link".into(), Any::String(link.as_str().into()));
            m
        }
        Geom::Path(p) => path_fields(p),
    };
    Any::Map(Arc::new(m))
}

/// A path's geometry value (see [`crate::path`]): `mode` as a number.
fn path_fields(p: &crate::path::PathGeom) -> HashMap<String, Any> {
    let subpaths: Vec<Any> = p
        .subpaths
        .iter()
        .map(|sp| {
            let mut nums = Vec::with_capacity(sp.anchors.len() * 7);
            for a in &sp.anchors {
                for v in [a.p.x, a.p.y, a.h_in.x, a.h_in.y, a.h_out.x, a.h_out.y] {
                    nums.push(Any::Number(v as f64));
                }
                nums.push(Any::Number(a.mode.code() as f64));
            }
            let mut m = HashMap::new();
            m.insert("closed".to_string(), Any::Bool(sp.closed));
            m.insert("anchors".to_string(), Any::Array(nums.into()));
            Any::Map(Arc::new(m))
        })
        .collect();
    let mut m = HashMap::new();
    m.insert("subpaths".to_string(), Any::Array(subpaths.into()));
    m.insert("rule".to_string(), Any::String(p.rule.as_str().into()));
    m
}

/// A path's geometry read back; what is not a finite anchor is skipped.
fn path_of(m: &HashMap<String, Any>) -> crate::path::PathGeom {
    use crate::path::{Anchor, AnchorMode, PathGeom, PathRule, Subpath};
    let rule = m.get("rule").and_then(any_str).and_then(PathRule::parse).unwrap_or_default();
    let mut subpaths = Vec::new();
    if let Some(Any::Array(list)) = m.get("subpaths") {
        for sp in list.iter() {
            let Any::Map(sm) = sp else { continue };
            let closed = matches!(sm.get("closed"), Some(Any::Bool(true)));
            let mut anchors = Vec::new();
            if let Some(Any::Array(nums)) = sm.get("anchors") {
                for chunk in nums.chunks_exact(7) {
                    let v: Vec<f32> = chunk[..6].iter().map(|a| number(a).unwrap_or(f64::NAN) as f32).collect();
                    if v.iter().any(|x| !x.is_finite()) {
                        continue;
                    }
                    let mode = match &chunk[6] {
                        Any::String(s) => AnchorMode::parse(s).unwrap_or_default(),
                        other => AnchorMode::from_code(number(other).unwrap_or(0.0)),
                    };
                    anchors.push(Anchor::new(Point::new(v[0], v[1]), Point::new(v[2], v[3]), Point::new(v[4], v[5]), mode));
                }
            }
            if !anchors.is_empty() {
                subpaths.push(Subpath::new(anchors, closed));
            }
        }
    }
    PathGeom::new(subpaths, rule)
}

fn geom_of(kind: &Kind, m: &HashMap<String, Any>) -> Geom {
    let rect = || rect_of(m).unwrap_or_default();
    let pt = |kx: &str, ky: &str| Point::new(field(m, kx).unwrap_or(0.0), field(m, ky).unwrap_or(0.0));
    match kind {
        Kind::Picture => Geom::Picture(PictureGeom {
            rect: rect(),
            src: m.get("src").and_then(any_str).unwrap_or("").to_string(),
            px_w: field(m, "px_w").unwrap_or(0.0).max(0.0) as u32,
            px_h: field(m, "px_h").unwrap_or(0.0).max(0.0) as u32,
            crop: match m.get("crop") {
                Some(Any::Map(c)) => rect_of(c),
                _ => None,
            },
        }),
        Kind::Line | Kind::Arrow => Geom::Segment { a: pt("x1", "y1"), b: pt("x2", "y2") },
        Kind::Step => Geom::Anchor(pt("x", "y")),
        Kind::Callout => Geom::Callout { rect: rect(), tip: pt("tip_x", "tip_y") },
        Kind::Connector => Geom::Connector { from: end_of(m.get("from")), to: end_of(m.get("to")) },
        Kind::Card => Geom::Card { rect: rect(), link: m.get("link").and_then(any_str).unwrap_or("").to_string() },
        Kind::Group => Geom::None,
        Kind::Pen | Kind::Highlighter => Geom::Stroke,
        Kind::Path => Geom::Path(path_of(m)),
        _ => match rect_of(m) {
            Some(r) => Geom::Rect(r),
            None => Geom::None,
        },
    }
}

fn rect_of(m: &HashMap<String, Any>) -> Option<Rect> {
    Some(Rect::new(field(m, "x")?, field(m, "y")?, field(m, "w").unwrap_or(0.0), field(m, "h").unwrap_or(0.0)))
}

fn field(m: &HashMap<String, Any>, key: &str) -> Option<f32> {
    m.get(key).and_then(number).map(|v| v as f32).filter(|v| v.is_finite())
}

fn number(any: &Any) -> Option<f64> {
    match any {
        Any::Number(n) => Some(*n),
        Any::BigInt(i) => Some(*i as f64),
        _ => None,
    }
}

fn any_str(any: &Any) -> Option<&str> {
    match any {
        Any::String(s) => Some(s),
        _ => None,
    }
}

fn string_of(out: Option<Out>) -> Option<String> {
    match out {
        Some(Out::Any(Any::String(s))) => Some(s.to_string()),
        _ => None,
    }
}

fn bool_of(out: Option<Out>) -> Option<bool> {
    match out {
        Some(Out::Any(Any::Bool(b))) => Some(b),
        _ => None,
    }
}

fn number_of(out: Option<Out>) -> Option<f32> {
    match out {
        Some(Out::Any(any)) => number(&any).map(|v| v as f32).filter(|v| v.is_finite()),
        _ => None,
    }
}
