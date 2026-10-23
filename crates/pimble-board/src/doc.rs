//! The board document in yrs (docs/IMAGE_MARKUP_CONTRACT.md section 3).
//!
//! ```text
//! board: Map {
//!   v: 1,
//!   page: Any { x, y, w, h, fill? }?,       // absent = infinite
//!   markup_visible: bool,
//!   edited_by: Any { id, name }?, edited_at: String?,   // board-level writes
//!   layers: Map<layer id, Map { name: Text, order: String, visible, locked, opacity }>,
//!   elements: Map<element id, Map {
//!     layer, order, parent?, kind,
//!     geom: Any {..} (per kind, written whole) | Array<Buffer> (pen, highlighter),
//!     text: Text?, style: Map { stroke, fill, width, opacity, font_size, align,
//!                               shadow, arrow_heads, dash },
//!     transform: Any { tx, ty, rot, sx, sy }?, locked,
//!     author: Any { id, name }?, created_at, edited_by?, edited_at?,
//!   }>,
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
//! A [`Board`] is a handle on where a board lives in a yrs document; it holds
//! no state of its own. Every write runs in one transaction and answers the
//! update it produced, an ordinary yrs update of the node's document, which
//! the caller sends through `applyEdit` like every other edit (CLAUDE.md:
//! "one way to write node content").

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use pimble_crdt::NodeDoc;
use yrs::types::text::TextRef;
use yrs::types::{Event, PathSegment};
use yrs::undo::UndoManager;
use yrs::{
    Any, Array, ArrayPrelim, ArrayRef, DeepObservable, Doc, GetString, Map, MapPrelim, MapRef, OffsetKind, Origin, Out, ReadTxn,
    StateVector, Text, TextPrelim, Transact, TransactionMut,
};

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
}
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
    #[error("{0}")]
    Invalid(String),
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
        let mut bt = BoardTxn { txn: &mut txn, location: self, board, stamp, tops: HashMap::new() };
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
                                    _ => c.board = true,
                                }
                            }
                        } else {
                            c.all = true;
                        }
                    }
                    Some(PathSegment::Key(k)) if &**k == LAYERS => c.board = true,
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
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        !self.all && !self.board && self.elements.is_empty()
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
pub struct BoardTxn<'a, 'doc> {
    txn: &'a mut TransactionMut<'doc>,
    location: &'a Board,
    board: Option<MapRef>,
    stamp: Stamp<'a>,
    /// The top `order` per layer, found once per transaction and kept up to
    /// date as elements are added, so adding many elements in one
    /// transaction is not quadratic.
    tops: HashMap<String, Option<String>>,
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
    /// image node ("Make Canvas Node"), keep saying who made them
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

    /// Layer ids with their orders, sorted bottom to top.
    fn layer_orders(&self) -> Result<Vec<(String, String)>> {
        let layers = self.layers()?;
        let mut out: Vec<(String, String)> = layers
            .iter(self.txn)
            .filter_map(|(id, out)| match out {
                Out::YMap(map) => Some((id.to_string(), string_of(map.get(self.txn, ORDER)).unwrap_or_default())),
                _ => None,
            })
            .collect();
        out.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        Ok(out)
    }

    fn bottom_layer(&self) -> Option<String> {
        self.layer_orders().ok()?.into_iter().next().map(|(id, _)| id)
    }

    fn top_layer(&self) -> Result<String> {
        self.layer_orders()?.into_iter().last().map(|(id, _)| id).ok_or(BoardError::LastLayer)
    }

    /// A new layer at `placement`; answers its id.
    pub fn add_layer(&mut self, name: &str, placement: Placement) -> Result<String> {
        let orders = self.layer_orders()?;
        let key = place(&orders, None, &placement)?;
        let id = new_id();
        let layers = self.layers()?;
        insert_layer(self.txn, &layers, &id, name, &key);
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
        self.stamp_board();
        Ok(())
    }

    pub fn set_layer_visible(&mut self, id: &str, visible: bool) -> Result<()> {
        self.layer_map(id)?.insert(self.txn, VISIBLE, visible);
        self.stamp_board();
        Ok(())
    }

    pub fn set_layer_locked(&mut self, id: &str, locked: bool) -> Result<()> {
        self.layer_map(id)?.insert(self.txn, LOCKED, locked);
        self.stamp_board();
        Ok(())
    }

    pub fn set_layer_opacity(&mut self, id: &str, opacity: f32) -> Result<()> {
        self.layer_map(id)?.insert(self.txn, OPACITY, opacity.clamp(0.0, 1.0) as f64);
        self.stamp_board();
        Ok(())
    }

    /// Move a layer in the order: one key of one map, so two people moving
    /// the same layer at once leave it in one place, never twice.
    pub fn move_layer(&mut self, id: &str, placement: Placement) -> Result<()> {
        let layer = self.layer_map(id)?;
        let orders = self.layer_orders()?;
        let key = place(&orders, Some(id), &placement)?;
        layer.insert(self.txn, ORDER, key);
        self.stamp_board();
        Ok(())
    }

    /// Delete a layer with its elements. A board keeps at least one layer.
    pub fn delete_layer(&mut self, id: &str) -> Result<()> {
        self.layer_map(id)?;
        if self.layer_orders()?.len() <= 1 {
            return Err(BoardError::LastLayer);
        }
        let elements = self.elements()?;
        let doomed: Vec<String> = elements
            .iter(self.txn)
            .filter_map(|(eid, out)| match out {
                Out::YMap(map) if string_of(map.get(self.txn, LAYER)).as_deref() == Some(id) => Some(eid.to_string()),
                _ => None,
            })
            .collect();
        for eid in doomed {
            elements.remove(self.txn, &eid);
        }
        self.layers()?.remove(self.txn, id);
        self.tops.remove(id);
        self.stamp_board();
        Ok(())
    }

    /// "Merge Down": the layer's elements go on top of the layer below, in
    /// their order, and the layer goes.
    pub fn merge_layer_down(&mut self, id: &str) -> Result<()> {
        let orders = self.layer_orders()?;
        let at = orders.iter().position(|(l, _)| l == id).ok_or_else(|| BoardError::NoLayer(id.to_string()))?;
        if at == 0 {
            return Err(BoardError::Invalid("the bottom layer has nothing below it".into()));
        }
        let below = orders[at - 1].0.clone();
        let ids = self.element_ids_in_layer(id)?;
        self.move_to_layer(&ids, &below)?;
        self.layers()?.remove(self.txn, id);
        self.tops.remove(id);
        Ok(())
    }

    /// "Move Selection to Layer": the elements go on top of `layer`, keeping
    /// their order among themselves.
    pub fn move_to_layer(&mut self, ids: &[String], layer: &str) -> Result<()> {
        self.layer_map(layer)?;
        let elements = self.elements()?;
        let mut found = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(Out::YMap(map)) = elements.get(self.txn, id) else {
                return Err(BoardError::NoElement(id.clone()));
            };
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
    /// top layer when `None`), at `element.placement` (on top when `None`),
    /// stamped `author` and `created_at`.
    pub fn add_element(&mut self, element: NewElement) -> Result<String> {
        let layer = match &element.layer {
            Some(layer) => {
                self.layer_map(layer)?;
                layer.clone()
            }
            None => self.top_layer()?,
        };
        if element.kind.is_stroke() != matches!(element.geom, Geom::Stroke) {
            return Err(BoardError::Invalid(format!("a {} takes {}", element.kind, if element.kind.is_stroke() { "stroke geometry" } else { "geometry of its own" })));
        }
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
        let map = self.element(id)?;
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
        let map = self.element(id)?;
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
        let map = self.element(id)?;
        if matches!(geom, Geom::Stroke) || matches!(map.get(self.txn, GEOM), Some(Out::YArray(_))) {
            return Err(BoardError::Invalid("a stroke's geometry is its points".into()));
        }
        map.insert(self.txn, GEOM, geom_any(geom));
        self.stamp_element(&map);
        Ok(())
    }

    /// Set the style keys that are `Some`; the others stay as they are.
    pub fn set_style(&mut self, id: &str, style: &Style) -> Result<()> {
        let map = self.element(id)?;
        let style_map = match map.get(self.txn, STYLE) {
            Some(Out::YMap(m)) => m,
            _ => map.insert(self.txn, STYLE, MapPrelim::default()),
        };
        write_style(self.txn, &style_map, style);
        self.stamp_element(&map);
        Ok(())
    }

    /// Clear one style key, back to the kind's default.
    pub fn clear_style(&mut self, id: &str, key: StyleKey) -> Result<()> {
        let map = self.element(id)?;
        if let Some(Out::YMap(style)) = map.get(self.txn, STYLE) {
            if style.remove(self.txn, key.as_str()).is_some() {
                self.stamp_element(&map);
            }
        }
        Ok(())
    }

    /// The transform, written whole (identity removes it).
    pub fn set_transform(&mut self, id: &str, transform: &Transform) -> Result<()> {
        let map = self.element(id)?;
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
        let map = self.element(id)?;
        let text_ref = match map.get(self.txn, TEXT) {
            Some(Out::YText(t)) => t,
            _ => map.insert(self.txn, TEXT, TextPrelim::new("")),
        };
        edit_text(self.txn, &text_ref, text);
        self.stamp_element(&map);
        Ok(())
    }

    pub fn set_locked(&mut self, id: &str, locked: bool) -> Result<()> {
        let map = self.element(id)?;
        map.insert(self.txn, LOCKED, locked);
        self.stamp_element(&map);
        Ok(())
    }

    /// Membership in a frame or group (`None`: none).
    pub fn set_parent(&mut self, id: &str, parent: Option<&str>) -> Result<()> {
        let map = self.element(id)?;
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
        let map = self.element(id)?;
        let layer = string_of(map.get(self.txn, LAYER)).unwrap_or_default();
        let orders = self.element_orders(&layer)?;
        let key = place(&orders, Some(id), &placement)?;
        map.insert(self.txn, ORDER, key);
        self.tops.remove(&layer);
        self.stamp_element(&map);
        Ok(())
    }

    /// Remove an element (its key goes; a concurrent edit of it is lost,
    /// which the contract accepts). Members of a deleted frame or group read
    /// as having no parent.
    pub fn delete_element(&mut self, id: &str) -> Result<()> {
        let elements = self.elements()?;
        if elements.remove(self.txn, id).is_none() {
            return Err(BoardError::NoElement(id.to_string()));
        }
        Ok(())
    }
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
    let mut layers = Vec::new();
    if let Some(Out::YMap(map)) = board.get(txn, LAYERS) {
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
            });
        }
    }
    let mut elements = Vec::new();
    if with_elements {
        if let Some(Out::YMap(map)) = board.get(txn, ELEMENTS) {
            elements.reserve(map.len(txn) as usize);
            for (id, out) in map.iter(txn) {
                let Out::YMap(el) = out else { continue };
                elements.push(read_element(txn, id, &el));
            }
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
    }
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
    }
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
    };
    Any::Map(Arc::new(m))
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
